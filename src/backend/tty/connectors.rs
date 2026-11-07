use std::collections::HashSet;

use anyhow::{Context, anyhow};
use smithay::{
    backend::{
        allocator::gbm::{GbmAllocator, GbmBufferFlags},
        drm::{
            compositor::DrmCompositor,
            exporter::gbm::{GbmFramebufferExporter, NodeFilter},
        },
        renderer::ImportDma,
    },
    output::{Mode, Output, PhysicalProperties, Scale},
    reexports::drm::control::crtc,
    utils::Transform,
};

use crate::outputs::{OutputSetup, config::to_smithay_transform};

use super::*;

impl Tty {
    /// Synchronizes outputs with currently connected monitors.
    ///
    /// Missing or replaced monitors are removed and new monitors are lit.
    ///
    /// A forced probe asks the kernel to redetect every connector.
    ///
    /// This can flicker active displays and is needed only at startup or
    /// after a VT switch.
    pub(super) fn scan_connectors(&mut self, state: &mut Orbit, force_probe: bool) {
        if self.gpu_removed {
            return;
        }

        let monitors = match monitors::discover(&self.drm, force_probe) {
            Ok(monitors) => monitors,
            Err(err) => {
                tracing::warn!(%err, "failed to scan connectors");
                return;
            }
        };

        let gone: Vec<crtc::Handle> = self
            .surfaces
            .iter()
            .filter(|(_, surface)| {
                !monitors.iter().any(|monitor| {
                    monitor.connector == surface.connector
                        && monitor.description() == surface.description
                })
            })
            .map(|(&crtc, _)| crtc)
            .collect();
        for crtc in gone {
            if let Some(surface) = self.surfaces.remove(&crtc) {
                tracing::info!("monitor {} disconnected", surface.output.name());
                state.remove_output(&surface.output);
                // Dropping the compositor turns the CRTC off.
            }
        }

        // Forget skipped monitors that were unplugged or swapped.
        self.skipped.retain(|connector, skipped| {
            monitors
                .iter()
                .any(|m| m.connector == *connector && m.description() == skipped.description())
        });

        for monitor in &monitors {
            if self.skipped.contains_key(&monitor.connector)
                || self
                    .surfaces
                    .values()
                    .any(|surface| surface.connector == monitor.connector)
            {
                continue;
            }
            tracing::info!("found monitor {monitor}");
            match self.connect(state, monitor) {
                Ok(true) => {}
                Ok(false) => {
                    self.skipped.insert(monitor.connector, monitor.clone());
                }
                Err(err) => {
                    tracing::warn!("could not light up {}: {err:#}", monitor.name);
                    self.skipped.insert(monitor.connector, monitor.clone());
                }
            }
        }
    }

    /// Lights up a monitor using configured or selected display settings.
    ///
    /// Lower modes are tried when the first mode cannot be driven. Returns
    /// false when configuration disables the monitor.
    pub(super) fn connect(&mut self, state: &mut Orbit, monitor: &Monitor) -> anyhow::Result<bool> {
        let description = monitor.description();
        let config = state.output_settings(&monitor.name, &description);
        if config.off {
            tracing::info!("{} is turned off in the config", monitor.name);
            return Ok(false);
        }

        let in_use: HashSet<crtc::Handle> = self.surfaces.keys().copied().collect();
        let crtc = monitors::assign_crtc(&monitor.possible_crtcs, &in_use)
            .context("no free CRTC can drive it")?;
        let first =
            monitors::pick_mode(&monitor.modes, config.mode.as_ref()).context("it has no modes")?;
        let output = Output::new(
            monitor.name.clone(),
            PhysicalProperties {
                size: monitor.size_mm.into(),
                subpixel: monitor.subpixel,
                make: monitor.make.clone(),
                model: monitor.model.clone(),
                serial_number: monitor.serial.clone().unwrap_or_else(|| "Unknown".into()),
            },
        );
        for &mode in &monitor.modes {
            output.add_mode(Mode::from(mode));
        }
        if let Some(preferred) = monitors::pick_mode(&monitor.modes, None) {
            output.set_preferred(Mode::from(preferred));
        }

        let mut last_err = None;
        for drm_mode in monitors::fallback_modes(&monitor.modes, first) {
            let mode = Mode::from(drm_mode);
            let scale = config.scale.unwrap_or_else(|| {
                monitors::auto_scale(
                    (mode.size.w, mode.size.h),
                    monitor.size_mm,
                    monitor.is_internal(),
                )
            });
            output.change_current_state(
                Some(mode),
                Some(
                    config
                        .transform
                        .map_or(Transform::Normal, to_smithay_transform),
                ),
                Some(Scale::Fractional(scale)),
                None,
            );
            let compositor = match self.create_compositor(&output, crtc, drm_mode, monitor) {
                Ok(compositor) => compositor,
                Err(err) => {
                    tracing::warn!(
                        "{} can't take {}x{} @ {:.3} Hz: {err:#}",
                        monitor.name,
                        mode.size.w,
                        mode.size.h,
                        mode.refresh as f64 / 1000.0,
                    );
                    last_err = Some(err);
                    continue;
                }
            };
            tracing::info!(
                "using {} at {}x{} @ {:.3} Hz, scale {}",
                monitor.name,
                mode.size.w,
                mode.size.h,
                mode.refresh as f64 / 1000.0,
                scale
            );
            self.surfaces.insert(
                crtc,
                OutputSurface {
                    output: output.clone(),
                    connector: monitor.connector,
                    description,
                    monitor: monitor.clone(),
                    compositor,
                    failures: FailedFrames::default(),
                },
            );
            state.add_output(
                &output,
                OutputSetup {
                    configured_position: config.position,
                    internal: monitor.is_internal(),
                },
            );
            return Ok(true);
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no mode worked")))
    }

    /// Creates a DRM compositor for a CRTC and display mode.
    ///
    /// The initial test commit detects unsupported hardware modes early.
    fn create_compositor(
        &mut self,
        output: &Output,
        crtc: crtc::Handle,
        mode: smithay::reexports::drm::control::Mode,
        monitor: &Monitor,
    ) -> anyhow::Result<GbmDrmCompositor> {
        let surface = self.drm.create_surface(crtc, mode, &[monitor.connector])?;
        let allocator = GbmAllocator::new(
            self.gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let exporter = GbmFramebufferExporter::new(self.gbm.clone(), NodeFilter::None);
        Ok(DrmCompositor::new(
            output,
            surface,
            None,
            allocator,
            exporter,
            SWAPCHAIN_FORMATS,
            self.renderer.dmabuf_formats(),
            self.drm.cursor_size(),
            Some(self.gbm.clone()),
        )?)
    }
}

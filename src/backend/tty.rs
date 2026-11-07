mod connectors;
mod render;
mod session;

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType, compositor::DrmCompositor,
            exporter::gbm::GbmFramebufferExporter,
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{ImportDma, gles::GlesRenderer},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{self, UdevBackend, UdevEvent},
    },
    desktop::utils::OutputPresentationFeedback,
    output::{Mode, Output},
    reexports::{
        calloop::{EventLoop, RegistrationToken},
        drm::control::{connector, crtc},
        input::{self, Libinput},
        rustix::fs::{Dev, OFlags},
    },
    utils::{DeviceFd, Transform},
    wayland::dmabuf::DmabufGlobal,
};

use super::{
    Backend, FailedFrames, dmabuf,
    monitors::{self, Monitor},
    refresh_interval,
};

use crate::{render::OrbitRenderElement, state::Orbit};

const CLEAR_COLOR: [f32; 4] = [0.08, 0.09, 0.12, 1.0];
/// Formats the swapchain may use, in order of preference.
const SWAPCHAIN_FORMATS: [Fourcc; 2] = [Fourcc::Argb8888, Fourcc::Xrgb8888];
/// Failed frames in a row on an output before its buffers are reset, and
/// again before the renderer is rebuilt.
const FAILURES_BEFORE_RESET: u32 = 3;
/// The renderer is rebuilt at most this often, so a GPU that keeps
/// failing can't keep Orbit busy rebuilding it.
const RENDERER_REBUILD_INTERVAL: Duration = Duration::from_secs(5);
/// How long a replaced dmabuf global stays, disabled, so clients binding
/// it meanwhile don't fail.
const DMABUF_GLOBAL_REMOVAL_DELAY: Duration = Duration::from_secs(5);

/// Each queued frame carries the presentation feedback of what it shows,
/// answered when its vblank arrives.
type GbmDrmCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

/// Native DRM and libseat backend state.
pub struct Tty {
    /// Active libseat session.
    session: LibSeatSession,
    /// Libinput context for input devices.
    libinput: Libinput,
    /// DRM device used for display output.
    drm: DrmDevice,
    /// GBM device used for buffer allocation.
    gbm: GbmDevice<DrmDeviceFd>,
    /// GPU device number used to select its udev events.
    device_id: Option<Dev>,
    renderer: GlesRenderer,
    /// Advertises renderer formats and is replaced with the renderer.
    dmabuf_global: DmabufGlobal,
    /// Where clients should allocate their buffers.
    render_node: Option<DrmNode>,
    /// When the renderer was last rebuilt after failures.
    renderer_rebuilt: Option<Instant>,
    /// The DRM device's vblank source, removed if the GPU goes away.
    drm_token: Option<RegistrationToken>,
    /// Records that the GPU was unplugged and drawing has stopped.
    gpu_removed: bool,
    /// The monitors being driven, by the CRTC driving them.
    surfaces: HashMap<crtc::Handle, OutputSurface>,
    /// Connected monitors left dark, on purpose (`off`) or because they
    /// couldn't be lit up, with their description. They are tried again
    /// only when that changes or after a VT switch, rather than on every
    /// udev event.
    skipped: HashMap<connector::Handle, Monitor>,
    /// Pointer devices, to apply `[input]` again when it changes.
    devices: Vec<input::Device>,
    /// Render elements reused for each frame and empty between frames.
    elements: Vec<OrbitRenderElement<GlesRenderer>>,
}

/// A monitor being driven and its DRM state.
struct OutputSurface {
    output: Output,
    connector: connector::Handle,
    /// Monitor description used to detect a replacement on the same connector.
    description: String,
    /// The monitor, for its DRM modes and to remember it when it's turned off.
    monitor: Monitor,
    /// The compositor driving the monitor, for its CRTC and framebuffer.
    compositor: GbmDrmCompositor,
    /// The number of failed frames in a row, before the compositor is reset.
    failures: FailedFrames,
}

impl Tty {
    /// Returns mutable access to the GLES renderer.
    pub fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }

    /// Returns connected monitors that are currently dark.
    ///
    /// This includes monitors disabled by configuration and monitors that
    /// could not be lit.
    pub fn dark_heads(&self) -> Vec<crate::outputs::config::HeadInfo> {
        let mut heads: Vec<_> = self
            .skipped
            .values()
            .map(|m| {
                let preferred = monitors::pick_mode(&m.modes, None);
                crate::outputs::config::HeadInfo {
                    name: m.name.clone(),
                    description: m.description(),
                    make: m.make.clone(),
                    model: m.model.clone(),
                    serial: m.serial.clone().unwrap_or_else(|| "Unknown".into()),
                    size_mm: m.size_mm,
                    modes: m
                        .modes
                        .iter()
                        .map(|&d| {
                            let mode = Mode::from(d);
                            crate::outputs::config::HeadMode {
                                size: (mode.size.w, mode.size.h),
                                refresh: mode.refresh,
                                preferred: Some(d) == preferred,
                            }
                        })
                        .collect(),
                    enabled: false,
                    current: None,
                    position: (0, 0).into(),
                    transform: Transform::Normal,
                    scale: 1.0,
                }
            })
            .collect();
        heads.sort_by(|a, b| a.name.cmp(&b.name));
        heads
    }

    /// Switches a lit monitor to one of its supported modes.
    pub fn set_mode(&mut self, output: &Output, mode: Mode) -> Result<(), String> {
        let Some(surface) = self.surfaces.values_mut().find(|s| s.output == *output) else {
            return Err(format!("{} isn't lit", output.name()));
        };
        let Some(drm_mode) = surface
            .monitor
            .modes
            .iter()
            .copied()
            .find(|&d| Mode::from(d) == mode)
        else {
            return Err(format!("{} has no such mode", output.name()));
        };
        surface
            .compositor
            .use_mode(drm_mode)
            .map_err(|err| format!("{} can't take that mode: {err}", output.name()))?;
        output.change_current_state(Some(mode), None, None, None);
        Ok(())
    }

    /// Turns a connected monitor off or enables it with its saved settings.
    pub fn set_enabled(&mut self, state: &mut Orbit, name: &str, on: bool) -> Result<(), String> {
        if on {
            let Some((&connector, monitor)) = self.skipped.iter().find(|(_, m)| m.name == name)
            else {
                return Ok(());
            };
            let monitor = monitor.clone();
            self.skipped.remove(&connector);
            match self.connect(state, &monitor) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    self.skipped.insert(connector, monitor);
                    Ok(())
                }
                Err(err) => {
                    self.skipped.insert(connector, monitor);
                    Err(format!("{err:#}"))
                }
            }
        } else {
            let Some((&crtc, _)) = self.surfaces.iter().find(|(_, s)| s.output.name() == name)
            else {
                return Ok(());
            };
            if let Some(surface) = self.surfaces.remove(&crtc) {
                tracing::info!("turning {name} off");
                state.remove_output(&surface.output);
                self.skipped.insert(surface.connector, surface.monitor);
            }
            Ok(())
        }
    }

    /// Applies the input configuration to every pointer device again.
    pub fn configure_devices(&mut self, config: &orbit_config::InputConfig) {
        for device in &mut self.devices {
            crate::input::config::configure_device(device, config);
        }
    }
}

/// Starts the native session.
///
/// The seat is acquired, monitors are enabled, and input and hotplug events
/// are registered.
pub fn init(event_loop: &mut EventLoop<'static, Orbit>, state: &mut Orbit) -> anyhow::Result<()> {
    let (session, notifier) = LibSeatSession::new().context(
        "could not take over the seat. Run Orbit from a TTY, \
         or use --nested inside another desktop",
    )?;
    let seat = session.seat();

    let gpu = primary_gpu(&seat)?;
    tracing::info!(gpu = %gpu.display(), %seat, "starting native session");

    let mut session_for_open = session.clone();
    let fd = session_for_open
        .open(
            &gpu,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .with_context(|| format!("could not open {}", gpu.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let device_id = match fd.dev_id() {
        Ok(id) => Some(id),
        Err(err) => {
            tracing::warn!(%err, "failed to identify the GPU; monitor hotplug won't work");
            None
        }
    };

    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let renderer = create_renderer(&gbm)?;

    // Hardware-accelerated clients share buffers with us through dmabuf.
    let render_node = render_node(&fd);
    let dmabuf_global = dmabuf::create_global(state, renderer.dmabuf_formats(), render_node);

    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| anyhow!("libinput could not use seat {seat}"))?;

    let mut tty = Tty {
        session: session.clone(),
        libinput: libinput.clone(),
        drm,
        gbm,
        device_id,
        renderer,
        dmabuf_global,
        render_node,
        renderer_rebuilt: None,
        drm_token: None,
        gpu_removed: false,
        surfaces: HashMap::new(),
        skipped: HashMap::new(),
        devices: Vec::new(),
        elements: Vec::new(),
    };
    tty.scan_connectors(state, true);
    if tty.surfaces.is_empty() {
        // No monitor is not fatal because one may be plugged in later.
        tracing::warn!("no monitor could be lit up");
    }

    let handle = event_loop.handle();
    handle
        .insert_source(
            LibinputInputBackend::new(libinput),
            |mut event, _, state| {
                match &mut event {
                    InputEvent::DeviceAdded { device } => {
                        tracing::info!(name = %device.name(), "input device added");
                        crate::input::config::configure_device(device, &state.config.input);
                        if let Some(tty) = state.backend.as_mut().and_then(Backend::tty) {
                            tty.devices.push(device.clone());
                        }
                    }
                    InputEvent::DeviceRemoved { device } => {
                        if let Some(tty) = state.backend.as_mut().and_then(Backend::tty) {
                            tty.devices.retain(|d| d != device);
                        }
                    }
                    _ => {}
                }
                state.process_input_event(event);
            },
        )
        // Keep the insert error because the returned source is not Send.
        .map_err(|err| err.error)?;

    handle
        .insert_source(notifier, |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                if let Some(tty) = state.backend.as_mut().and_then(Backend::tty) {
                    tty.libinput.suspend();
                    tty.drm.pause();
                }
            }
            SessionEvent::ActivateSession => state.on_session_resumed(),
        })
        .map_err(|err| err.error)?;

    let drm_token = handle
        .insert_source(drm_notifier, |event, metadata, state| match event {
            DrmEvent::VBlank(crtc) => state.on_vblank(crtc, metadata.as_ref()),
            DrmEvent::Error(err) => tracing::warn!(%err, "DRM error"),
        })
        .map_err(|err| err.error)?;
    tty.drm_token = Some(drm_token);

    handle
        .insert_source(UdevBackend::new(&seat)?, |event, _, state| match event {
            UdevEvent::Changed { device_id } => state.on_gpu_changed(device_id),
            UdevEvent::Added { path, .. } => {
                tracing::info!(path = %path.display(), "GPU added; only the primary GPU is used");
            }
            UdevEvent::Removed { device_id } => state.on_gpu_removed(device_id),
        })
        .map_err(|err| err.error)?;

    state.session = Some(session);
    state.enable_software_cursor()?;
    state.backend = Some(Backend::Tty(Box::new(tty)));

    Ok(())
}

/// Returns the seat boot GPU or the first available GPU.
pub fn primary_gpu(seat: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = udev::primary_gpu(seat)? {
        return Ok(path);
    }
    udev::all_gpus(seat)?
        .into_iter()
        .next()
        .with_context(|| format!("no GPU found on seat {seat}"))
}

/// Creates a GLES renderer on the GBM device GPU.
///
/// The context requests high priority. Drivers or permissions can reduce it
/// to the normal priority.
fn create_renderer(gbm: &GbmDevice<DrmDeviceFd>) -> anyhow::Result<GlesRenderer> {
    // Safety the GBM device outlives the display and renderer.
    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = match EGLContext::new_with_priority(&egl, ContextPriority::High) {
        Ok(context) => context,
        Err(err) => {
            tracing::debug!(%err, "no high-priority EGL context, using the default");
            EGLContext::new(&egl)?
        }
    };
    // Safety the context is used only from this thread.
    Ok(unsafe { GlesRenderer::new(context)? })
}

/// Returns the GPU render node used for client buffer allocation.
///
/// Devices without a separate render node use the primary node.
fn render_node(fd: &DrmDeviceFd) -> Option<DrmNode> {
    let node = match DrmNode::from_file(fd) {
        Ok(node) => node,
        Err(err) => {
            tracing::debug!(%err, "failed to identify the DRM node");
            return None;
        }
    };
    match node.node_with_type(NodeType::Render) {
        Some(Ok(render)) => Some(render),
        Some(Err(err)) => {
            tracing::debug!(%node, %err, "failed to open the render node, using the primary node");
            Some(node)
        }
        None => Some(node),
    }
}

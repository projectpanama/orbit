use std::time::Instant;

use orbit_core::frame::FrameOutcome;
use smithay::{
    backend::{
        drm::{DrmEventMetadata, DrmEventTime, compositor::FrameFlags},
        renderer::ImportDma,
        session::Session,
    },
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        drm::control::crtc,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
    },
    utils::{Monotonic, Time},
    wayland::presentation::Refresh,
};

use super::*;

impl Tty {
    /// Renders and queues a frame for one native output.
    pub fn render(&mut self, state: &mut Orbit, output: &Output) -> FrameOutcome {
        if !self.session.is_active() || self.gpu_removed {
            return FrameOutcome::Skipped;
        }
        let Some((&crtc, surface)) = self
            .surfaces
            .iter_mut()
            .find(|(_, surface)| surface.output == *output)
        else {
            return FrameOutcome::Skipped;
        };
        let span = tracy_client::span!("render");
        // Label traces with the monitor name when profiling is active.
        if tracy_client::Client::is_running() {
            span.emit_text(&output.name());
        }

        state.render_elements(&mut self.renderer, output, &mut self.elements);
        let damage_span = tracy_client::span!("render_frame");
        let rendered = surface
            .compositor
            .render_frame(
                &mut self.renderer,
                &self.elements,
                CLEAR_COLOR,
                FrameFlags::DEFAULT,
            )
            .map(|result| (!result.is_empty, result.states));
        drop(damage_span);
        self.elements.clear();

        let (has_damage, states) = match rendered {
            Ok(rendered) => rendered,
            Err(err) => {
                state.post_render(output, None);
                self.frame_failed(state, crtc, &err);
                return FrameOutcome::Failed;
            }
        };
        state.post_render(output, Some(&states));
        if !has_damage {
            surface.failures.succeeded(&output.name());
            return FrameOutcome::NoDamage;
        }

        let feedback = state.presentation_feedback(output, &states);
        match surface.compositor.queue_frame(Some(feedback)) {
            Ok(()) => {
                surface.failures.succeeded(&output.name());
                tracy_client::frame_mark();
                FrameOutcome::Submitted
            }
            Err(err) => {
                self.frame_failed(state, crtc, &err);
                FrameOutcome::Failed
            }
        }
    }

    /// Handles a failed frame on a CRTC.
    ///
    /// Repeated failures reset output buffers and eventually rebuild the renderer.
    fn frame_failed(&mut self, state: &mut Orbit, crtc: crtc::Handle, err: &dyn std::fmt::Debug) {
        let Some(surface) = self.surfaces.get_mut(&crtc) else {
            return;
        };
        let failures = surface.failures.failed(&surface.output.name(), err);
        if failures == FAILURES_BEFORE_RESET {
            tracing::info!(output = %surface.output.name(), "frames keep failing; resetting its buffers");
            surface.compositor.reset_buffers();
        } else if failures >= 2 * FAILURES_BEFORE_RESET && failures % FAILURES_BEFORE_RESET == 0 {
            self.rebuild_renderer(state);
        }
    }

    /// Replaces the renderer on the same GPU after repeated failures.
    ///
    /// Rebuilds are rate limited. Client buffers are imported again as they
    /// are next drawn.
    fn rebuild_renderer(&mut self, state: &mut Orbit) {
        if self
            .renderer_rebuilt
            .is_some_and(|at| at.elapsed() < RENDERER_REBUILD_INTERVAL)
        {
            return;
        }
        self.renderer_rebuilt = Some(Instant::now());
        tracing::warn!("drawing keeps failing; rebuilding the renderer");
        let renderer = match create_renderer(&self.gbm) {
            Ok(renderer) => renderer,
            Err(err) => {
                tracing::error!("could not rebuild the renderer: {err:#}");
                return;
            }
        };
        self.renderer = renderer;

        // Replace the dmabuf global after clients have time to notice.
        let new_global =
            dmabuf::create_global(state, self.renderer.dmabuf_formats(), self.render_node);
        let old_global = std::mem::replace(&mut self.dmabuf_global, new_global);
        state
            .dmabuf_state
            .disable_global::<Orbit>(&state.display_handle, &old_global);
        let timer = Timer::from_duration(DMABUF_GLOBAL_REMOVAL_DELAY);
        let mut old_global = Some(old_global);
        if let Err(err) = state.loop_handle.insert_source(timer, move |_, _, orbit| {
            if let Some(global) = old_global.take() {
                orbit
                    .dmabuf_state
                    .destroy_global::<Orbit>(&orbit.display_handle, global);
            }
            TimeoutAction::Drop
        }) {
            tracing::warn!(%err, "failed to schedule removing the old dmabuf global");
        }

        for surface in self.surfaces.values_mut() {
            surface.compositor.reset_buffers();
        }
    }
}

impl Orbit {
    /// Completes a submitted frame when the DRM vblank arrives.
    pub(super) fn on_vblank(&mut self, crtc: crtc::Handle, metadata: Option<&DrmEventMetadata>) {
        let Some(tty) = self.backend.as_mut().and_then(Backend::tty) else {
            return;
        };
        let Some(surface) = tty.surfaces.get_mut(&crtc) else {
            return;
        };
        let feedback = match surface.compositor.frame_submitted() {
            Ok(feedback) => feedback.flatten(),
            Err(err) => {
                tracing::warn!(?err, "failed to mark frame as submitted");
                None
            }
        };
        let output = surface.output.clone();
        let (time, flags) = self.vblank_time(metadata);
        self.record_presented(&output, time.into());
        if let Some(mut feedback) = feedback {
            let sequence = metadata.map_or(0, |metadata| metadata.sequence);
            feedback.presented(
                time,
                Refresh::fixed(refresh_interval(&output)),
                u64::from(sequence),
                flags,
            );
        }
        self.frame_presented(&output);
    }

    /// Returns the vblank time and presentation flags.
    ///
    /// A valid kernel timestamp is preferred. The current clock is used when
    /// no usable timestamp is available.
    fn vblank_time(
        &self,
        metadata: Option<&DrmEventMetadata>,
    ) -> (Time<Monotonic>, wp_presentation_feedback::Kind) {
        use wp_presentation_feedback::Kind;
        match metadata.map(|metadata| metadata.time) {
            Some(DrmEventTime::Monotonic(time)) if !time.is_zero() => (
                time.into(),
                Kind::Vsync | Kind::HwClock | Kind::HwCompletion,
            ),
            _ => (self.clock.now(), Kind::Vsync),
        }
    }
}

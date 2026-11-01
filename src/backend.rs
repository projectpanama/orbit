mod dmabuf;

#[cfg(test)]
pub mod headless;
pub mod winit;

use std::{fmt::Debug, time::Duration};

use orbit_core::frame::{self, FrameId, FrameOutcome};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{ImportDma, Renderer, gles::GlesRenderer},
    },
    output::Output,
    reexports::calloop::timer::{TimeoutAction, Timer},
};

use crate::{backend::winit::Winit, state::Orbit};

#[derive(Debug, Default)]
pub struct FailedFrames(u32);

impl FailedFrames {
    /// Notes a failed frame and returns how many failed in a row.
    pub fn failed(&mut self, output: &str, err: &dyn Debug) -> u32 {
        self.0 += 1;
        if self.0 == 1 {
            tracing::warn!(output, ?err, "failed to draw a frame; retrying");
        } else {
            tracing::debug!(output, failures = self.0, ?err, "failed to draw a frame");
        }
        self.0
    }

    /// Notes a frame that worked.
    pub fn succeeded(&mut self, output: &str) {
        if self.0 > 0 {
            tracing::info!(output, failures = self.0, "drawing works again");
            self.0 = 0;
        }
    }
}

/// TODO: Implement TTY backend
pub enum Backend {
    Winit(Box<Winit>),
    #[cfg(test)]
    Headless(Box<headless::Headless>),
}

impl Backend {
    pub fn winit(&mut self) -> Option<&mut Winit> {
        match self {
            Backend::Winit(winit) => Some(winit),
            _ => None,
        }
    }

    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            Backend::Winit(winit) => Some(winit.renderer()),
            _ => None,
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        let Some(renderer) = self.renderer() else {
            return false;
        };
        match renderer.import_dmabuf(dmabuf, None) {
            Ok(_texture) => true,
            Err(err) => {
                tracing::debug!(?err, "failed to import dmabuf");
                false
            }
        }
    }

    pub fn cleanup(&mut self) {
        if let Some(renderer) = self.renderer()
            && let Err(err) = renderer.cleanup_texture_cache()
        {
            tracing::debug!(?err, "failed to clean up textures");
        }
    }

    fn render(&mut self, orbit: &mut Orbit, output: &Output) -> FrameOutcome {
        match self {
            Backend::Winit(winit) => winit.render(orbit, output),
            #[cfg(test)]
            Backend::Headless(headless) => headless.render(orbit, output),
        }
    }
}

impl Orbit {
    pub fn with_backend<T>(&mut self, f: impl FnOnce(&mut Backend, &mut Orbit) -> T) -> Option<T> {
        let mut backend = self.backend.take()?;
        let result = f(&mut backend, self);
        self.backend = Some(backend);
        Some(result)
    }

    pub fn queue_redraw(&mut self, output: &Output) {
        if self
            .outputs
            .get_mut(output)
            .is_some_and(|state| state.frame.request())
        {
            self.schedule_queued_renders();
        }
    }

    pub fn queue_redraw_all(&mut self) {
        let mut any = false;
        for state in self.outputs.values_mut() {
            any |= state.frame.request();
        }
        if any {
            self.schedule_queued_renders();
        }
    }

    pub fn next_frame_time(&self, output: &Output) -> Duration {
        let now = Duration::from(self.clock.now());
        let last = self
            .outputs
            .get(output)
            .and_then(|state| state.last_presented);
        frame::next_presentation(last, refresh_interval(output), now)
    }

    pub fn frame_presented(&mut self, output: &Output) {
        if self
            .outputs
            .get_mut(output)
            .is_some_and(|state| state.frame.presented())
        {
            self.schedule_queued_renders();
        }
    }

    fn schedule_queued_renders(&mut self) {
        if std::mem::replace(&mut self.frames.renders_scheduled, true) {
            return;
        }
        self.loop_handle.insert_idle(|state| state.render_queued());
    }

    fn render_queued(&mut self) {
        self.frames.renders_scheduled = false;

        // Reuses one buffer rather than allocating every frame.
        let mut queued = std::mem::take(&mut self.frames.queued_outputs);
        queued.extend(
            self.outputs
                .iter()
                .filter(|(_, state)| state.frame.is_queued())
                .map(|(output, _)| output.clone()),
        );
        for output in queued.drain(..) {
            self.render_output(&output);
        }
        self.frames.queued_outputs = queued;
    }

    fn render_output(&mut self, output: &Output) {
        self.frames.frame_time = self.next_frame_time(output);
        self.tick_animations(output);

        let outcome = self
            .with_backend(|backend, orbit| backend.render(orbit, output))
            .unwrap_or(FrameOutcome::Skipped);

        let Some(state) = self.outputs.get_mut(output) else {
            return;
        };

        if let Some(frame) = state.frame.rendered(outcome)
            && matches!(outcome, FrameOutcome::NoDamage | FrameOutcome::Failed)
        {
            self.estimate_vblank(output, frame);
        }
        // An animation keeps asking for the next frame, which the scheduler
        // holds until this one was shown: each output animates at its own
        // refresh rate. Not while the session is away, which would spin.
        if outcome != FrameOutcome::Skipped && self.is_animating(output) {
            self.queue_redraw(output);
        }
    }

    fn estimate_vblank(&mut self, output: &Output, frame: FrameId) {
        let interval = refresh_interval(output);
        let delay = self.outputs.get(output).map_or(interval, |state| {
            state.frame.estimated_vblank_delay(interval)
        });
        let timer = Timer::from_duration(delay);
        let target = output.clone();
        let inserted = self.loop_handle.insert_source(timer, move |_, _, state| {
            if state
                .outputs
                .get_mut(&target)
                .is_some_and(|output_state| output_state.frame.estimated_presented(frame))
            {
                state.schedule_queued_renders();
            }
            TimeoutAction::Drop
        });
        if let Err(err) = inserted {
            // Without the timer the frame would stay in flight forever.
            tracing::warn!(%err, "failed to schedule the next frame");
            self.frame_presented(output);
        }
    }
}

fn refresh_interval(output: &Output) -> Duration {
    let refresh_mhz = output
        .current_mode()
        .map_or(60_000, |mode| mode.refresh)
        .max(1);
    Duration::from_micros(1_000_000_000 / u64::from(refresh_mhz.unsigned_abs()))
}

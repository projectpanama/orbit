pub mod dmabuf;
#[cfg(test)]
pub mod headless;
pub mod monitors;
pub mod tty;
pub mod winit;

use std::{fmt::Debug, time::Duration};

use orbit_core::frame::{self, FrameId, FrameOutcome};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{ImportDma, Renderer, gles::GlesRenderer},
    },
    desktop::{Window, find_popup_root_surface},
    input::pointer::CursorImageStatus,
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle},
    wayland::compositor::get_parent,
};

use crate::{backend::tty::Tty, backend::winit::Winit, state::Orbit};

/// How far (logical pixels) a cursor image can reach from the pointer in
/// any direction. This is generous for a large theme at a fractional scale.
const CURSOR_REACH: i32 = 128;

/// Counts an output's failed frames in a row, so a failing GPU or display
/// logs a warning once per incident rather than at the refresh rate.
#[derive(Debug, Default)]
pub struct FailedFrames(u32);

impl FailedFrames {
    /// Records a failed frame and returns the consecutive failure count.
    ///
    /// The first failure is logged at warning level and subsequent failures
    /// are logged at debug level.
    pub fn failed(&mut self, output: &str, err: &dyn Debug) -> u32 {
        self.0 += 1;
        if self.0 == 1 {
            tracing::warn!(output, ?err, "failed to draw a frame; retrying");
        } else {
            tracing::debug!(output, failures = self.0, ?err, "failed to draw a frame");
        }
        self.0
    }

    /// Records a successful frame and clears the failure count.
    pub fn succeeded(&mut self, output: &str) {
        if self.0 > 0 {
            tracing::info!(output, failures = self.0, "drawing works again");
            self.0 = 0;
        }
    }
}

/// Rendering backend selected for the compositor.
pub enum Backend {
    /// OpenGL rendering in a nested winit window.
    Winit(Box<Winit>),
    /// OpenGL rendering on a TTY.
    Tty(Box<Tty>),
    /// Test-only headless rendering backend.
    #[cfg(test)]
    Headless(Box<headless::Headless>),
}

impl Backend {
    /// Returns the nested winit backend, if selected.
    pub fn winit(&mut self) -> Option<&mut Winit> {
        match self {
            Backend::Winit(winit) => Some(winit),
            _ => None,
        }
    }
    /// Returns the TTY backend, if selected.
    pub fn tty(&mut self) -> Option<&mut Tty> {
        match self {
            Backend::Tty(tty) => Some(tty),
            _ => None,
        }
    }

    /// Returns the active GLES renderer, if this backend provides one.
    pub fn renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            Backend::Winit(winit) => Some(winit.renderer()),
            Backend::Tty(tty) => Some(tty.renderer()),
            #[cfg(test)]
            Backend::Headless(_) => None,
        }
    }

    /// Tests whether the renderer can import `dmabuf`.
    ///
    /// Returns `false` when no renderer is available or import fails.
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

    /// Frees the textures of buffers that are gone. The renderer does this
    /// itself after each frame, but nothing may be drawn for a while after
    /// a client quits (or while another VT has the display).
    pub fn cleanup(&mut self) {
        if let Some(renderer) = self.renderer()
            && let Err(err) = renderer.cleanup_texture_cache()
        {
            tracing::debug!(?err, "failed to clean up textures");
        }
    }

    /// Renders one output using the selected backend.
    fn render(&mut self, orbit: &mut Orbit, output: &Output) -> FrameOutcome {
        match self {
            Backend::Winit(winit) => winit.render(orbit, output),
            Backend::Tty(tty) => tty.render(orbit, output),
            #[cfg(test)]
            Backend::Headless(headless) => headless.render(orbit, output),
        }
    }
}

impl Orbit {
    /// Runs `f` with the backend and the rest of the state borrowed
    /// separately. The backend is put back afterwards on every path.
    ///
    /// Returns `None` if there is no backend, which includes being called
    /// from inside another `with_backend`.
    pub fn with_backend<T>(&mut self, f: impl FnOnce(&mut Backend, &mut Orbit) -> T) -> Option<T> {
        let mut backend = self.backend.take()?;
        let result = f(&mut backend, self);
        self.backend = Some(backend);
        Some(result)
    }

    /// Asks for `output` to be redrawn. Cheap and idempotent (any number
    /// of requests before the next frame results in one render).
    pub fn queue_redraw(&mut self, output: &Output) {
        if self
            .outputs
            .get_mut(output)
            .is_some_and(|state| state.frame.request())
        {
            self.schedule_queued_renders();
        }
    }

    /// Asks for the outputs that overlap any of `areas` to be redrawn.
    pub fn queue_redraw_areas(&mut self, areas: &[Rectangle<i32, Logical>]) {
        let mut any = false;
        for (output, state) in &mut self.outputs {
            if let Some(geo) = self.space.output_geometry(output)
                && areas.iter().any(|area| area.overlaps(geo))
            {
                any |= state.frame.request();
            }
        }
        if any {
            self.schedule_queued_renders();
        }
    }

    /// Asks for the outputs `window` is drawn on (with its popups and
    /// title bar) to be redrawn. Nothing for a hidden or minimized window.
    ///
    /// Call it before and after changing a window, so the outputs it
    /// leaves are redrawn as well as those it reaches.
    pub fn queue_redraw_window(&mut self, window: &Window) {
        let Some(bbox) = self.space.element_bbox(window) else {
            return;
        };
        let frame = self.frame_geometry(window).unwrap_or(bbox);
        self.queue_redraw_areas(&[bbox, frame]);
    }

    /// Asks for the outputs the cursor is drawn on to be redrawn.
    ///
    /// Reads the pointer's location, so it must not be called while
    /// Smithay holds the pointer (from `SeatHandler` callbacks or grabs).
    pub fn queue_redraw_pointer(&mut self) {
        let pointer = self.pointer.current_location().to_i32_round();
        let reach = Point::from((CURSOR_REACH, CURSOR_REACH));
        self.queue_redraw_areas(&[Rectangle::new(pointer - reach, (reach + reach).to_size())]);
    }

    /// Queues the smallest known redraw region for a committed surface.
    ///
    /// Popups, layer surfaces, lock surfaces, and cursor surfaces are mapped to
    /// their owning output or window while unknown surfaces trigger all outputs.
    pub fn queue_redraw_surface(&mut self, surface: &WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        // A popup is shown with what it belongs to.
        let mut popup = false;
        if let Some(kind) = self.popups.find_popup(&root)
            && let Ok(parent) = find_popup_root_surface(&kind)
        {
            popup = true;
            root = parent;
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
        }

        if let Some(window) = self.window_for_surface(&root) {
            self.queue_redraw_window(&window);
            // A popup's size isn't remembered from before its commit, as
            // the window's is, also where the window was last drawn.
            if popup {
                for output in self.space.outputs_for_element(&window) {
                    self.queue_redraw(&output);
                }
            }
        } else if let Some(output) = self
            .output_for_layer(&root)
            .or_else(|| self.output_of_lock_surface(&root))
        {
            self.queue_redraw(&output);
        } else if matches!(&self.input.cursor_status, CursorImageStatus::Surface(cursor) if *cursor == root)
        {
            self.queue_redraw_pointer();
        } else {
            self.queue_redraw_all();
        }
    }

    /// Asks for every output to be redrawn. Outputs where nothing changed
    /// find no damage and submit nothing.
    pub fn queue_redraw_all(&mut self) {
        let mut any = false;
        for state in self.outputs.values_mut() {
            any |= state.frame.request();
        }
        if any {
            self.schedule_queued_renders();
        }
    }

    /// Records when the frame in flight on `output` reached the screen, so
    /// later frames can predict their own presentation time.
    pub fn record_presented(&mut self, output: &Output, time: Duration) {
        if let Some(state) = self.outputs.get_mut(output) {
            state.last_presented = Some(time);
        }
    }

    /// Estimates the next presentation time for `output`. Animations are
    /// sampled at this time, so their steps stay even at any refresh rate.
    pub fn next_frame_time(&self, output: &Output) -> Duration {
        let now = Duration::from(self.clock.now());
        let last = self
            .outputs
            .get(output)
            .and_then(|state| state.last_presented);
        frame::next_presentation(last, refresh_interval(output), now)
    }

    /// Marks the queued frame as presented and schedules more queued work.
    pub fn frame_presented(&mut self, output: &Output) {
        if self
            .outputs
            .get_mut(output)
            .is_some_and(|state| state.frame.presented())
        {
            self.schedule_queued_renders();
        }
    }

    /// Marks the queued frame as lost (e.g. after the session was paused)
    /// and allows it to be retried.
    pub fn frame_lost(&mut self, output: &Output) {
        if self
            .outputs
            .get_mut(output)
            .is_some_and(|state| state.frame.reset())
        {
            self.schedule_queued_renders();
        }
    }

    /// Frees the textures of surfaces destroyed meanwhile once the event
    /// loop has handled everything pending, so a client quitting with
    /// many surfaces costs one cleanup.
    pub fn schedule_cleanup(&mut self) {
        if std::mem::replace(&mut self.frames.cleanup_scheduled, true) {
            return;
        }
        self.loop_handle.insert_idle(|state| {
            state.frames.cleanup_scheduled = false;
            state.with_backend(|backend, _| backend.cleanup());
        });
    }

    /// Schedules one deferred pass for queued output renders.
    fn schedule_queued_renders(&mut self) {
        if std::mem::replace(&mut self.frames.renders_scheduled, true) {
            return;
        }
        self.loop_handle.insert_idle(|state| state.render_queued());
    }

    /// Renders all outputs currently queued by their frame schedulers.
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

    /// Renders one output and updates its frame scheduler.
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

    /// Estimates presentation using a timer when no backend event will arrive.
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

/// Returns the output refresh interval, defaulting to 60 Hz.
fn refresh_interval(output: &Output) -> Duration {
    let refresh_mhz = output
        .current_mode()
        .map_or(60_000, |mode| mode.refresh)
        .max(1);
    Duration::from_micros(1_000_000_000 / u64::from(refresh_mhz.unsigned_abs()))
}

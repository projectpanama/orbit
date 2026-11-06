use std::{collections::HashMap, time::Duration};

use orbit_core::{
    animation::{Animation, Spring, Timing, gesture::VelocityTracker},
    ids::{OutputId, WindowId},
    overview,
};
use smithay::{
    backend::renderer::element::solid::SolidColorBuffer,
    desktop::Window,
    output::Output,
    utils::{Logical, Point, Rectangle},
};

use crate::{
    geometry::{ToCore, ToSmithay},
    state::Orbit,
    workspaces::window_id,
};

/// Space reserved for the shell workspace strip at the top.
pub const TOP_BAND: i32 = 160;
/// Space reserved around the other sides of the overview layout.
pub const MARGIN: i32 = 48;
/// Finger travel in logical pixels from desktop to overview.
const SWIPE_DISTANCE: f64 = 300.0;
/// Release velocity in progress per second that decides a flick.
const FLICK_VELOCITY: f64 = 1.0;

/// Current motion mode for an overview transition.
#[derive(Debug, Clone, Copy)]
enum Motion {
    /// The overview is at rest.
    Settled(f64),
    /// The overview is following a timed animation.
    Eased(Animation),
    /// The overview is following a spring.
    Spring(Spring),
    /// A touch gesture is controlling the progress.
    Held(f64),
}

/// Overview state for one output while it is open or closing.
#[derive(Debug)]
pub struct OverviewState {
    /// Current transition mode.
    motion: Motion,
    /// Each window's frame (title bar included) at progress 1, in
    /// output-local logical coordinates, in layout order. Set through
    /// [`set_layout`](Self::set_layout).
    layout: Vec<(WindowId, Rectangle<i32, Logical>)>,
    /// Each window's place in `layout`, so drawing a window finds its
    /// frame without a search.
    positions: HashMap<WindowId, usize>,
    /// A hash of what `layout` was computed from, so it is only
    /// recomputed when that changes.
    inputs: Option<u64>,
    /// Buffer used to dim the output behind the overview.
    pub dim: SolidColorBuffer,
}

impl OverviewState {
    /// Creates a closed overview with an empty layout.
    pub fn new() -> Self {
        Self {
            motion: Motion::Settled(0.0),
            layout: Vec::new(),
            positions: HashMap::new(),
            inputs: None,
            dim: SolidColorBuffer::default(),
        }
    }

    /// Returns the current progress at the given time.
    ///
    /// Zero is the desktop state and one is the overview state.
    pub fn progress(&self, now: Duration) -> f64 {
        let v = match self.motion {
            Motion::Settled(v) | Motion::Held(v) => v,
            Motion::Eased(a) => a.value(now),
            Motion::Spring(s) => s.value(now),
        };
        v.clamp(0.0, 1.0)
    }

    /// Returns the transition target.
    ///
    /// One means open and zero means closed. A held gesture has no target.
    pub fn target(&self) -> Option<f64> {
        match self.motion {
            Motion::Settled(v) => Some(v),
            Motion::Eased(a) => Some(a.to()),
            Motion::Spring(s) => Some(s.to()),
            Motion::Held(_) => None,
        }
    }

    /// Starts an eased transition towards `to`.
    ///
    /// The duration is scaled to the remaining distance.
    pub fn animate_to(&mut self, to: f64, now: Duration, timing: Timing) {
        let from = self.progress(now);
        if (from - to).abs() < f64::EPSILON || timing.is_off() {
            self.motion = Motion::Settled(to);
            return;
        }
        let timing = Timing {
            duration: timing.duration.mul_f64((to - from).abs()),
            ..timing
        };
        self.motion = Motion::Eased(Animation::new(from, to, now, timing));
    }

    /// Holds the current progress for a touch gesture.
    pub fn hold(&mut self, now: Duration) {
        self.motion = Motion::Held(self.progress(now));
    }

    /// Changes the held progress by `by` and keeps it within its bounds.
    pub fn drag(&mut self, by: f64) {
        if let Motion::Held(v) = &mut self.motion {
            *v = (*v + by).clamp(0.0, 1.0);
        }
    }

    /// Releases the gesture and starts a spring towards an end state.
    ///
    /// A fast flick selects its direction. Otherwise the nearer end state is
    /// selected.
    pub fn release(&mut self, velocity: f64, now: Duration, timing: Timing) {
        let at = self.progress(now);
        let to = if velocity > FLICK_VELOCITY {
            1.0
        } else if velocity < -FLICK_VELOCITY {
            0.0
        } else if at >= 0.5 {
            1.0
        } else {
            0.0
        };
        if timing.is_off() {
            self.motion = Motion::Settled(to);
        } else {
            self.motion = Motion::Spring(Spring::new(at, to, velocity, now, timing.duration));
        }
    }

    /// Reports whether a timed transition is still active.
    pub fn is_moving(&self, now: Duration) -> bool {
        match self.motion {
            Motion::Settled(_) | Motion::Held(_) => false,
            Motion::Eased(a) => !a.is_done(now),
            Motion::Spring(s) => !s.is_done(now),
        }
    }

    /// Settles a finished animation exactly at its end.
    pub fn tick(&mut self, now: Duration) {
        let done = match self.motion {
            Motion::Eased(a) if a.is_done(now) => Some(a.to()),
            Motion::Spring(s) if s.is_done(now) => Some(s.to()),
            _ => None,
        };
        if let Some(to) = done {
            self.motion = Motion::Settled(to);
        }
    }

    /// Reports whether the overview is fully closed and at rest.
    pub fn is_closed(&self) -> bool {
        matches!(self.motion, Motion::Settled(v) if v == 0.0)
    }

    /// Returns the current overview layout in layout order.
    pub fn layout(&self) -> &[(WindowId, Rectangle<i32, Logical>)] {
        &self.layout
    }

    /// Replaces the layout and rebuilds the window index.
    pub fn set_layout(&mut self, layout: Vec<(WindowId, Rectangle<i32, Logical>)>) {
        if layout == self.layout {
            return;
        }
        self.positions.clear();
        self.positions
            .extend(layout.iter().enumerate().map(|(i, &(id, _))| (id, i)));
        self.layout = layout;
    }

    /// Returns the overview frame assigned to `window`.
    pub fn frame_of(&self, window: WindowId) -> Option<Rectangle<i32, Logical>> {
        let &index = self.positions.get(&window)?;
        self.layout.get(index).map(|&(_, rect)| rect)
    }

    /// Returns the window whose frame contains an output-local point.
    pub fn window_at(&self, local: Point<f64, Logical>) -> Option<WindowId> {
        self.layout
            .iter()
            .rev()
            .find(|(_, r)| r.to_f64().contains(local))
            .map(|(id, _)| *id)
    }
}

/// State for the overview swipe currently in progress.
#[derive(Debug)]
pub struct OverviewSwipe {
    /// Output controlled by the gesture.
    pub output: OutputId,
    /// Recent gesture movement used to calculate release velocity.
    tracker: VelocityTracker,
}

impl Orbit {
    /// Returns the overview state for `output` when it exists.
    pub fn overview(&self, output: &Output) -> Option<&OverviewState> {
        self.outputs.get(output).and_then(|s| s.overview.as_ref())
    }

    /// Returns overview progress for the frame being drawn.
    ///
    /// Zero is desktop and one is overview.
    pub fn overview_progress(&self, output: &Output) -> f64 {
        self.overview(output)
            .map_or(0.0, |o| o.progress(self.frames.frame_time))
    }

    /// Reports whether the output is showing or entering overview mode.
    ///
    /// Windows do not receive pointer input while this returns true.
    pub fn in_overview(&self, output: &Output) -> bool {
        self.overview(output).is_some_and(|o| {
            o.target() != Some(0.0) || o.progress(self.next_frame_time(output)) > 0.0
        })
    }

    /// Recomputes the overview layout when its inputs have changed.
    ///
    /// This runs before each frame while the overview is open.
    pub fn relayout_overview(&mut self, output: &Output) {
        let _span = tracy_client::span!("relayout_overview");
        let Some(geo) = self.space.output_geometry(output) else {
            return;
        };
        if self.overview(output).is_none() {
            return;
        }
        let area = Rectangle::new(
            (MARGIN, TOP_BAND).into(),
            (
                (geo.size.w - 2 * MARGIN).max(1),
                (geo.size.h - TOP_BAND - MARGIN).max(1),
            )
                .into(),
        );
        let inputs = {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            (area.size.w, area.size.h).hash(&mut hasher);
            for (id, r) in self.overview_windows(output, geo) {
                (id, r.loc.x, r.loc.y, r.size.w, r.size.h).hash(&mut hasher);
            }
            hasher.finish()
        };
        if self
            .overview(output)
            .is_some_and(|o| o.inputs == Some(inputs))
        {
            return;
        }
        let windows: Vec<_> = self
            .overview_windows(output, geo)
            .map(|(id, frame)| (id, frame.to_core()))
            .collect();
        let laid = overview::layout(&windows, area.to_core())
            .into_iter()
            .map(|(id, frame)| (id, frame.to_smithay()))
            .collect();
        if let Some(state) = self
            .outputs
            .get_mut(output)
            .and_then(|s| s.overview.as_mut())
        {
            state.set_layout(laid);
            state.inputs = Some(inputs);
        }
    }

    /// Opens the overview on `output`.
    pub fn open_overview(&mut self, output: &Output) {
        if self.session_lock.is_active() {
            return;
        }
        let now = self.next_frame_time(output);
        let timing = self.config.animations.overview;
        let Some(state) = self.outputs.get_mut(output) else {
            return;
        };
        let overview = state.overview.get_or_insert_with(OverviewState::new);
        if overview.target() != Some(1.0) {
            overview.animate_to(1.0, now, timing);
        }
        self.relayout_overview(output);
        self.queue_redraw(output);
    }

    /// Closes the overview on `output`.
    pub fn close_overview(&mut self, output: &Output) {
        let now = self.next_frame_time(output);
        let timing = self.config.animations.overview;
        if let Some(overview) = self
            .outputs
            .get_mut(output)
            .and_then(|s| s.overview.as_mut())
            && overview.target() != Some(0.0)
        {
            overview.animate_to(0.0, now, timing);
            self.queue_redraw(output);
        }
    }

    /// Toggles overview mode on the active output.
    pub fn toggle_overview(&mut self) {
        let Some(output) = self.active_output() else {
            return;
        };
        if self
            .overview(&output)
            .is_some_and(|o| o.target() != Some(0.0))
        {
            self.close_overview(&output);
        } else {
            self.open_overview(&output);
        }
    }

    /// Closes every overview immediately.
    ///
    /// This is used when the session locks.
    pub fn drop_overviews(&mut self) {
        for state in self.outputs.values_mut() {
            state.overview = None;
        }
    }

    /// Returns the active output whose overview Escape should close.
    pub fn overview_for_escape(&self) -> Option<Output> {
        let output = self.active_output()?;
        self.in_overview(&output).then_some(output)
    }

    /// Handles a global pointer press during overview mode.
    ///
    /// A window under the point is activated. Any other point only closes the
    /// overview. The return value reports whether overview handled the press.
    pub fn overview_click(&mut self, pos: Point<f64, Logical>) -> bool {
        let Some(output) = self.space.output_under(pos).next().cloned() else {
            return false;
        };
        if !self.in_overview(&output) {
            return false;
        }
        let Some(geo) = self.space.output_geometry(&output) else {
            return false;
        };
        let local = pos - geo.loc.to_f64();
        let picked = self
            .overview(&output)
            .and_then(|o| o.window_at(local))
            .and_then(|id| self.windows.get(&id).cloned());
        if let Some(window) = picked {
            self.activate_window(&window);
        }
        self.close_overview(&output);
        true
    }

    /// Returns the target frame and progress for drawing `window` in overview.
    pub fn overview_target(
        &self,
        output: &Output,
        window: &Window,
    ) -> Option<(Rectangle<i32, Logical>, f64)> {
        let overview = self.overview(output)?;
        let id = window_id(window)?;
        let p = overview.progress(self.frames.frame_time);
        if p <= 0.0 {
            return None;
        }
        let target = overview.frame_of(id)?;
        Some((target, p))
    }

    /// Updates overview animation and layout state before a frame.
    ///
    /// Finished animations are settled and closed overviews are discarded.
    pub fn tick_overview(&mut self, output: &Output) {
        let now = self.frames.frame_time;
        let Some(state) = self.outputs.get_mut(output) else {
            return;
        };
        let Some(overview) = state.overview.as_mut() else {
            return;
        };
        overview.tick(now);
        if overview.is_closed() {
            state.overview = None;
            return;
        }
        self.relayout_overview(output);
    }

    /// Reports whether the overview on `output` is still animating.
    pub fn overview_animating(&self, output: &Output) -> bool {
        self.overview(output)
            .is_some_and(|o| o.is_moving(self.frames.frame_time))
    }

    /// Starts an overview swipe on the active output.
    ///
    /// Returns false when gestures are disabled or another gesture is active.
    pub fn overview_swipe_begin(&mut self) -> bool {
        if !self.config.overview_swipe.enabled
            || self.input.overview_swipe.is_some()
            || self.input.workspace_swipe.is_some()
        {
            return false;
        }
        let Some(output) = self.active_output() else {
            return false;
        };
        let Some(id) = self.output_id(&output) else {
            return false;
        };
        let now = self.next_frame_time(&output);
        let Some(state) = self.outputs.get_mut(&output) else {
            return false;
        };
        state
            .overview
            .get_or_insert_with(OverviewState::new)
            .hold(now);
        self.input.overview_swipe = Some(OverviewSwipe {
            output: id,
            tracker: VelocityTracker::default(),
        });
        self.relayout_overview(&output);
        self.queue_redraw(&output);
        true
    }

    /// Updates the overview swipe using vertical finger movement.
    ///
    /// Moving up opens the overview.
    pub fn overview_swipe_update(&mut self, dy: f64) {
        let Some(swipe) = &self.input.overview_swipe else {
            return;
        };
        let Some(output) = self.output_by_id(swipe.output) else {
            self.input.overview_swipe = None;
            return;
        };
        let by = -dy / SWIPE_DISTANCE;
        if let Some(o) = self
            .outputs
            .get_mut(&output)
            .and_then(|s| s.overview.as_mut())
        {
            o.drag(by);
        }
        let now = Duration::from(self.clock.now());
        if let Some(swipe) = &mut self.input.overview_swipe {
            swipe.tracker.push(now, by);
        }
        self.queue_redraw(&output);
    }

    /// Ends the overview swipe and settles or springs to an end state.
    ///
    /// A cancelled gesture closes the overview.
    pub fn overview_swipe_end(&mut self, cancelled: bool) {
        let Some(swipe) = self.input.overview_swipe.take() else {
            return;
        };
        let Some(output) = self.output_by_id(swipe.output) else {
            return;
        };
        let velocity = swipe.tracker.velocity(Duration::from(self.clock.now()));
        let now = self.next_frame_time(&output);
        let timing = self.config.animations.overview;
        if let Some(o) = self
            .outputs
            .get_mut(&output)
            .and_then(|s| s.overview.as_mut())
        {
            if cancelled {
                o.animate_to(0.0, now, timing);
            } else {
                o.release(velocity, now, timing);
            }
        }
        self.queue_redraw(&output);
    }

    /// Reports whether an overview swipe is active.
    pub fn overview_swiping(&self) -> bool {
        self.input.overview_swipe.is_some()
    }

    /// The windows the overview of `output` lays out, and their frames
    /// now (output-local).
    fn overview_windows(
        &self,
        output: &Output,
        geo: Rectangle<i32, Logical>,
    ) -> impl Iterator<Item = (WindowId, Rectangle<i32, Logical>)> + '_ {
        let windows = self
            .output_id(output)
            .and_then(|id| self.workspaces.active(id))
            .map_or(&[][..], |active| self.workspaces.windows_of(active));
        windows
            .iter()
            .filter_map(|w| self.windows.get(w).map(|window| (*w, window)))
            .filter(|(_, window)| self.space.element_location(window).is_some())
            .filter_map(move |(w, window)| {
                let mut frame = self.frame_geometry(window)?;
                frame.loc -= geo.loc;
                Some((w, frame))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_eases_holds_and_springs() {
        let timing = Timing {
            duration: Duration::from_millis(300),
            easing: orbit_core::animation::Easing::EaseOutCubic,
        };
        let mut o = OverviewState::new();
        assert_eq!(o.progress(Duration::ZERO), 0.0);
        o.animate_to(1.0, Duration::ZERO, timing);
        let mid = o.progress(Duration::from_millis(100));
        assert!(mid > 0.0 && mid < 1.0);
        o.tick(Duration::from_millis(300));
        assert_eq!(o.progress(Duration::from_millis(300)), 1.0);
        assert_eq!(o.target(), Some(1.0));

        o.hold(Duration::from_millis(400));
        o.drag(-0.8);
        assert!((o.progress(Duration::ZERO) - 0.2).abs() < 1e-9);
        assert_eq!(o.target(), None);
        // A slow release near the start closes.
        o.release(0.0, Duration::from_secs(1), timing);
        assert_eq!(o.target(), Some(0.0));
        o.tick(Duration::from_secs(10));
        assert!(o.is_closed());

        // A flick up opens even from near the bottom.
        let mut o = OverviewState::new();
        o.hold(Duration::ZERO);
        o.drag(0.1);
        o.release(3.0, Duration::ZERO, timing);
        assert_eq!(o.target(), Some(1.0));
    }
}

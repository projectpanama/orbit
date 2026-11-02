use std::time::Duration;

use orbit_core::{
    animation::{gesture::VelocityTracker, workspace::WorkspaceSlide},
    ids::{OutputId, WorkspaceId},
};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::{state::Orbit, workspaces::WorkspaceTarget};

/// Finger travel, in libinput units, that moves one workspace at sensitivity 1.
const SWIPE_DISTANCE: f64 = 400.0;
/// Idle time after which a scroll workspace_swipe ends when no new event arrives.
const SCROLL_IDLE_END: Duration = Duration::from_millis(150);

/// Input source that started a workspace workspace_swipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeSource {
    /// Direct touchpad gesture input.
    Touchpad,
    /// Modifier-assisted horizontal scrolling.
    Scroll,
}

/// State for a workspace workspace_swipe currently in progress.
#[derive(Debug)]
pub struct WorkspaceSwipe {
    /// Output whose workspace strip is being dragged.
    pub output: OutputId,
    /// Input source that started the workspace_swipe.
    pub source: SwipeSource,
    /// Recent movement samples used to project release velocity.
    tracker: VelocityTracker,
    /// Monotonic timestamp of the most recent movement.
    last_event: Duration,
    /// Sequence number used to invalidate older idle timers.
    events: u64,
}

impl Orbit {
    /// Starts a workspace_swipe on the output under the pointer.
    ///
    /// Returns `false` when swipes are disabled, another workspace_swipe is active, no
    /// output is active, or the pointer is currently grabbed.
    pub fn swipe_begin(&mut self, source: SwipeSource) -> bool {
        if !self.config.workspace_swipe.enabled || self.input.workspace_swipe.is_some() {
            return false;
        }
        if self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed())
        {
            return false;
        }
        let Some(output) = self.active_output() else {
            return false;
        };
        let Some(id) = self.output_id(&output) else {
            return false;
        };
        let Some(active) = self.workspaces.active(id) else {
            return false;
        };

        let workspaces: Vec<WorkspaceId> = self.workspaces.workspace_ids(id).collect();
        let now = self.next_frame_time(&output);
        let Some(state) = self.outputs.get_mut(&output) else {
            return false;
        };

        let Some(slide) =
            WorkspaceSlide::grab(workspaces, active, state.workspace_slide.as_ref(), now)
        else {
            return false;
        };

        state.workspace_slide = Some(slide);

        self.input.workspace_swipe = Some(WorkspaceSwipe {
            output: id,
            source,
            tracker: VelocityTracker::default(),
            last_event: Duration::from(self.clock.now()),
            events: 0,
        });

        // Where the workspace_swipe lands is where keyboard actions go next.
        self.set_focused_output(&output);
        tracing::debug!(output = %output.name(), ?source, "workspace workspace_swipe started");
        self.queue_redraw(&output);
        true
    }

    /// Applies horizontal finger movement to the active workspace slide.
    ///
    /// Positive `dx` means movement to the right while negative movement
    /// reveals the workspace on the right.
    pub fn swipe_update(&mut self, dx: f64) {
        let Some(workspace_swipe) = &self.input.workspace_swipe else {
            return;
        };
        let Some(output) = self.output_by_id(workspace_swipe.output) else {
            // Its output went away.
            self.input.workspace_swipe = None;
            return;
        };
        let slots = -dx * self.config.workspace_swipe.sensitivity / SWIPE_DISTANCE;
        let Some(slide) = self
            .outputs
            .get_mut(&output)
            .and_then(|state| state.workspace_slide.as_mut())
        else {
            self.input.workspace_swipe = None;
            return;
        };
        slide.drag(slots);
        let now = Duration::from(self.clock.now());
        if let Some(workspace_swipe) = &mut self.input.workspace_swipe {
            workspace_swipe.tracker.push(now, slots);
            workspace_swipe.last_event = now;
        }
        self.queue_redraw(&output);
    }

    /// Ends the workspace_swipe and settles on a workspace.
    ///
    /// A cancelled workspace_swipe returns toward its starting workspace.
    pub fn swipe_end(&mut self, cancelled: bool) {
        let now = Duration::from(self.clock.now());
        self.finish_swipe(cancelled, now);
    }

    /// Handles modifier-assisted horizontal scrolling as workspace swiping.
    ///
    /// `Some(0.0)` indicates scroll completion and `None` indicates no
    /// horizontal component. Returns `true` when the event is consumed.
    pub fn swipe_scroll(
        &mut self,
        amount: Option<f64>,
        vertical: Option<f64>,
        mod_held: bool,
    ) -> bool {
        match self
            .input
            .workspace_swipe
            .as_ref()
            .map(|workspace_swipe| workspace_swipe.source)
        {
            Some(SwipeSource::Touchpad) => return false,
            Some(SwipeSource::Scroll) => {
                match amount {
                    Some(0.0) => self.swipe_end(false),
                    Some(amount) => {
                        self.swipe_update(-amount);
                        self.arm_scroll_idle_end();
                    }
                    None => {}
                }
                return true;
            }
            None => {}
        }

        let config = self.config.workspace_swipe;
        if !config.mod_scroll || !mod_held {
            return false;
        }
        let Some(amount) = amount.filter(|amount| *amount != 0.0) else {
            return false;
        };

        // Mostly vertical scrolling is still the client's.
        if vertical.is_some_and(|vertical| vertical.abs() > amount.abs()) {
            return false;
        }
        if !self.swipe_begin(SwipeSource::Scroll) {
            return false;
        }
        self.swipe_update(-amount);
        self.arm_scroll_idle_end();
        true
    }

    /// Ends a modifier-scroll workspace_swipe when its modifier is released.
    pub fn swipe_mod_released(&mut self) {
        if self
            .input
            .workspace_swipe
            .as_ref()
            .is_some_and(|workspace_swipe| workspace_swipe.source == SwipeSource::Scroll)
        {
            self.swipe_end(false);
        }
    }

    /// Arms a timer that ends the scroll workspace_swipe after an idle interval.
    ///
    /// The event sequence ensures an older timer cannot end a newer workspace_swipe.
    fn arm_scroll_idle_end(&mut self) {
        let Some(workspace_swipe) = &mut self.input.workspace_swipe else {
            return;
        };

        workspace_swipe.events += 1;
        let events = workspace_swipe.events;
        let timer = Timer::from_duration(SCROLL_IDLE_END);

        let inserted = self.loop_handle.insert_source(timer, move |_, _, state| {
            if let Some(workspace_swipe) = &state.input.workspace_swipe
                && workspace_swipe.source == SwipeSource::Scroll
                && workspace_swipe.events == events
            {
                // Its speed is what it was when the scrolling stopped.
                let last_event = workspace_swipe.last_event;
                state.finish_swipe(false, last_event);
            }
            TimeoutAction::Drop
        });

        if let Err(err) = inserted {
            // Without the timer it would only end on Mod release.
            tracing::warn!(%err, "failed to schedule the end of a scroll workspace_swipe");
        }
    }

    /// Completes a workspace_swipe using velocity measured at `velocity_at`.
    fn finish_swipe(&mut self, cancelled: bool, velocity_at: Duration) {
        let Some(workspace_swipe) = self.input.workspace_swipe.take() else {
            return;
        };

        let Some(output) = self.output_by_id(workspace_swipe.output) else {
            return;
        };

        let timing = self.config.animations.workspace_switch;
        let now = self.next_frame_time(&output);
        let velocity = workspace_swipe.tracker.velocity(velocity_at);

        let Some(slide) = self
            .outputs
            .get_mut(&output)
            .and_then(|state| state.workspace_slide.as_mut())
        else {
            return;
        };

        let target = if cancelled {
            slide.cancel(now, timing);
            None
        } else {
            slide.release(velocity, now, timing)
        };

        tracing::debug!(output = %output.name(), cancelled, velocity, "workspace workspace_swipe ended");

        // The slide is already heading there, therefore, this makes it the
        // logical state, which `animate_switch` leaves the slide alone for.
        let active = self.workspaces.active(workspace_swipe.output);
        if let Some(index) = target
            .filter(|&target| Some(target) != active)
            .and_then(|target| self.workspaces.index_of(workspace_swipe.output, target))
        {
            self.switch_workspace(&output, WorkspaceTarget::Index(index));
        }

        self.queue_redraw(&output);
    }
}

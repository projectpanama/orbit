use std::time::Duration;

/// The longest delay between retries after consecutive failed frame attempts.
///
/// The scheduler increases the delay after each failure until it reaches this
/// limit. This prevents a persistent rendering error from causing retries on
/// every display refresh.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Identifies a frame that has completed a render attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameId(u64);

/// Describes the current stage of frame scheduling.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    /// No redraw is waiting to be rendered and no frame is awaiting presentation.
    #[default]
    Idle,
    /// A redraw request is ready for the next render attempt.
    Queued,
    /// A render attempt has completed and its frame is awaiting presentation.
    ///
    /// The scheduler stays in this state until the backend reports
    /// presentation or the caller estimates that presentation has occurred.
    InFlight {
        frame: FrameId,
        /// Indicates that another redraw was requested before presentation.
        redraw_needed: bool,
    },
}

/// Describes what happened when the queued frame was processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The frame was submitted and the backend should report its presentation.
    Submitted,
    /// No visible changes were rendered, so no presentation event is expected.
    ///
    /// The caller should estimate when the frame would have been presented.
    NoDamage,
    /// Rendering or submission failed and the frame should be tried again.
    ///
    /// The retry should wait for the estimated vblank delay. Consecutive
    /// failures increase that delay up to [`MAX_RETRY_DELAY`].
    Failed,
    /// The frame was not rendered, for example because the session is inactive.
    Skipped,
}

/// Coordinates redraw requests with rendering and presentation.
#[derive(Debug, Default)]
pub struct FrameScheduler {
    state: FrameState,
    next_frame: u64,
    failures: u32,
}

impl FrameScheduler {
    /// Requests that the next frame be rendered.
    ///
    /// Returns `true` when the scheduler was idle and this call queued work.
    ///
    /// Returns `false` when work is already queued or a frame is in flight.
    ///
    /// A request made during presentation is still remembered for the next frame.
    pub fn request(&mut self) -> bool {
        match &mut self.state {
            FrameState::Idle => {
                self.state = FrameState::Queued;
                true
            }
            FrameState::Queued => false,
            FrameState::InFlight { redraw_needed, .. } => {
                *redraw_needed = true;
                false
            }
        }
    }

    /// Reports whether a render attempt is ready to start.
    pub fn is_queued(&self) -> bool {
        self.state == FrameState::Queued
    }

    /// Reports whether a rendered frame is waiting for presentation.
    pub fn is_in_flight(&self) -> bool {
        matches!(self.state, FrameState::InFlight { .. })
    }

    /// Records the result of the current render attempt.
    ///
    /// A submitted, damage-free, or failed attempt returns a new [`FrameId`]
    /// and waits for presentation. A skipped attempt returns `None` and
    /// returns to the idle state.
    pub fn rendered(&mut self, outcome: FrameOutcome) -> Option<FrameId> {
        debug_assert!(self.is_queued(), "rendered without being queued");
        match outcome {
            FrameOutcome::Failed => self.failures = self.failures.saturating_add(1),
            FrameOutcome::Submitted | FrameOutcome::NoDamage => self.failures = 0,
            // Nothing was tried, so nothing is learned.
            FrameOutcome::Skipped => {}
        }
        match outcome {
            FrameOutcome::Submitted | FrameOutcome::NoDamage | FrameOutcome::Failed => {
                let frame = FrameId(self.next_frame);
                self.next_frame = self.next_frame.wrapping_add(1);
                self.state = FrameState::InFlight {
                    frame,
                    redraw_needed: outcome == FrameOutcome::Failed,
                };
                Some(frame)
            }
            FrameOutcome::Skipped => {
                self.state = FrameState::Idle;
                None
            }
        }
    }

    /// Confirms that the in-flight frame was presented.
    ///
    /// Returns `true` when another redraw was requested while the frame was
    /// in flight. That redraw is queued for the next render attempt.
    ///
    /// Returns `false` when no additional redraw is needed or no frame is in flight.
    pub fn presented(&mut self) -> bool {
        match self.state {
            FrameState::InFlight { redraw_needed, .. } => self.finish(redraw_needed),
            FrameState::Idle | FrameState::Queued => false,
        }
    }

    /// Confirms presentation for a specific in-flight frame.
    ///
    /// Notifications for stale or unknown frame identifiers are ignored.
    ///
    /// Returns `true` when a redraw was requested during the frame and is now
    /// queued for rendering.
    pub fn estimated_presented(&mut self, frame: FrameId) -> bool {
        match self.state {
            FrameState::InFlight {
                frame: in_flight,
                redraw_needed,
            } if in_flight == frame => self.finish(redraw_needed),
            _ => false,
        }
    }

    /// Returns the delay to use before retrying a failed frame.
    ///
    /// The first failure uses `interval`. Each later consecutive failure
    /// doubles the delay. The result is never shorter than `interval` and is
    /// capped at [`MAX_RETRY_DELAY`].
    ///
    /// A submitted or damage-free frame clears the failure count.
    pub fn estimated_vblank_delay(&self, interval: Duration) -> Duration {
        let Some(doublings) = self.failures.checked_sub(1) else {
            return interval;
        };
        let backoff = 1u32.checked_shl(doublings).unwrap_or(u32::MAX);
        interval
            .checked_mul(backoff)
            .unwrap_or(Duration::MAX)
            .min(MAX_RETRY_DELAY)
            .max(interval)
    }

    /// Discards the current state and requests a fresh redraw.
    ///
    /// Any frame awaiting presentation is forgotten. The failure count is
    /// cleared and the scheduler is left with a queued redraw.
    pub fn reset(&mut self) -> bool {
        self.state = FrameState::Idle;
        self.failures = 0;
        self.request()
    }

    fn finish(&mut self, redraw_needed: bool) -> bool {
        if redraw_needed {
            self.state = FrameState::Queued;
        } else {
            self.state = FrameState::Idle;
        }
        redraw_needed
    }
}

/// Returns the next presentation time at or after `now`, aligned to `interval`.
/// The schedule is anchored at the last presentation, or starts at `now`
/// when no presentation history exists.
pub fn next_presentation(last: Option<Duration>, interval: Duration, now: Duration) -> Duration {
    let Some(last) = last else {
        return now;
    };
    if interval.is_zero() || last >= now {
        return last.max(now);
    }
    let behind = (now - last).as_nanos();
    let interval_ns = interval.as_nanos();
    let cycles = behind.div_ceil(interval_ns);
    let ahead = cycles.saturating_mul(interval_ns);
    last.saturating_add(Duration::from_nanos(
        u64::try_from(ahead).unwrap_or(u64::MAX),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ_60: Duration = Duration::from_nanos(16_666_667);
    const HZ_144: Duration = Duration::from_nanos(6_944_444);

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn next_presentation_without_history_is_now() {
        assert_eq!(next_presentation(None, HZ_60, ms(5)), ms(5));
    }

    #[test]
    fn next_presentation_is_the_coming_vblank() {
        // 5 ms after a vblank, the next is one cycle after it.
        assert_eq!(
            next_presentation(Some(ms(100)), HZ_60, ms(105)),
            ms(100) + HZ_60
        );
        assert_eq!(
            next_presentation(Some(ms(100)), HZ_144, ms(105)),
            ms(100) + HZ_144
        );
        // Exactly on a vblank.
        assert_eq!(next_presentation(Some(ms(100)), HZ_60, ms(100)), ms(100));
    }

    #[test]
    fn next_presentation_skips_missed_vblanks() {
        // Idle for a while: stays on the vblank grid.
        let next = next_presentation(Some(ms(100)), HZ_144, ms(150));
        assert_eq!(next, ms(100) + HZ_144 * 8);
        assert!(next >= ms(150) && next < ms(150) + HZ_144);
    }

    #[test]
    fn next_presentation_never_goes_back() {
        // A timestamp from the future (e.g. clock skew) is used as is.
        assert_eq!(next_presentation(Some(ms(200)), HZ_60, ms(150)), ms(200));
        assert_eq!(
            next_presentation(Some(ms(100)), Duration::ZERO, ms(150)),
            ms(150)
        );
    }

    fn in_flight(scheduler: &mut FrameScheduler, outcome: FrameOutcome) -> FrameId {
        assert!(scheduler.request());
        scheduler.rendered(outcome).unwrap()
    }

    #[test]
    fn failed_frame_is_retried_after_its_estimated_vblank() {
        let mut scheduler = FrameScheduler::default();
        let frame = in_flight(&mut scheduler, FrameOutcome::Failed);
        assert!(scheduler.estimated_presented(frame));
        assert!(scheduler.is_queued());
        // Succeeding again ends the retries.
        let frame = scheduler.rendered(FrameOutcome::NoDamage).unwrap();
        assert!(!scheduler.estimated_presented(frame));
        assert!(!scheduler.is_queued());
    }

    /// Fails `count` frames in a row, each retried at its estimated vblank.
    fn fail(scheduler: &mut FrameScheduler, count: u32) {
        for _ in 0..count {
            if !scheduler.is_queued() {
                assert!(scheduler.request());
            }
            let frame = scheduler.rendered(FrameOutcome::Failed).unwrap();
            assert!(scheduler.estimated_presented(frame));
        }
    }

    #[test]
    fn healthy_frames_are_estimated_one_refresh_later() {
        let mut scheduler = FrameScheduler::default();
        assert_eq!(scheduler.estimated_vblank_delay(HZ_144), HZ_144);
        in_flight(&mut scheduler, FrameOutcome::NoDamage);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_144), HZ_144);
    }

    #[test]
    fn failures_in_a_row_double_the_retry_delay() {
        let mut scheduler = FrameScheduler::default();
        fail(&mut scheduler, 1);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_60), HZ_60);
        fail(&mut scheduler, 1);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_60), HZ_60 * 2);
        fail(&mut scheduler, 1);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_60), HZ_60 * 4);
    }

    #[test]
    fn retry_delay_is_capped() {
        let mut scheduler = FrameScheduler::default();
        fail(&mut scheduler, 10);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_60), MAX_RETRY_DELAY);
        // Thousands of failures can't overflow the shift or the product.
        fail(&mut scheduler, 5000);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_144), MAX_RETRY_DELAY);
        // A refresh slower than the cap still waits a whole refresh.
        let slow = Duration::from_secs(2);
        assert_eq!(scheduler.estimated_vblank_delay(slow), slow);
    }

    #[test]
    fn a_working_frame_ends_the_backoff() {
        for outcome in [FrameOutcome::Submitted, FrameOutcome::NoDamage] {
            let mut scheduler = FrameScheduler::default();
            fail(&mut scheduler, 4);
            scheduler.rendered(outcome).unwrap();
            assert_eq!(scheduler.estimated_vblank_delay(HZ_60), HZ_60);
        }
    }

    #[test]
    fn skipped_frames_keep_the_backoff() {
        let mut scheduler = FrameScheduler::default();
        fail(&mut scheduler, 3);
        assert_eq!(scheduler.rendered(FrameOutcome::Skipped), None);
        assert_eq!(scheduler.estimated_vblank_delay(HZ_60), HZ_60 * 4);
    }

    #[test]
    fn reset_ends_the_backoff() {
        let mut scheduler = FrameScheduler::default();
        fail(&mut scheduler, 4);
        assert!(scheduler.rendered(FrameOutcome::Failed).is_some());
        assert!(scheduler.reset());
        assert_eq!(scheduler.estimated_vblank_delay(HZ_60), HZ_60);
    }

    #[test]
    fn idle_request_schedules_once() {
        let mut scheduler = FrameScheduler::default();
        assert!(scheduler.request());
        assert!(scheduler.is_queued());
        assert!(!scheduler.request());
    }

    #[test]
    fn idle_after_presented_without_changes() {
        let mut scheduler = FrameScheduler::default();
        in_flight(&mut scheduler, FrameOutcome::Submitted);
        assert!(!scheduler.presented());
        assert!(!scheduler.is_queued());
        assert!(scheduler.request());
    }

    #[test]
    fn requests_while_in_flight_wait_for_presented() {
        let mut scheduler = FrameScheduler::default();
        in_flight(&mut scheduler, FrameOutcome::Submitted);
        assert!(!scheduler.request());
        assert!(!scheduler.request());
        assert!(!scheduler.is_queued());
        assert!(scheduler.presented());
        assert!(scheduler.is_queued());
    }

    #[test]
    fn skipped_returns_to_idle() {
        let mut scheduler = FrameScheduler::default();
        assert!(scheduler.request());
        assert_eq!(scheduler.rendered(FrameOutcome::Skipped), None);
        assert!(scheduler.request());
    }

    #[test]
    fn no_damage_waits_for_its_estimated_vblank() {
        let mut scheduler = FrameScheduler::default();
        let frame = in_flight(&mut scheduler, FrameOutcome::NoDamage);
        assert!(!scheduler.request());
        assert!(scheduler.estimated_presented(frame));
        assert!(scheduler.is_queued());
    }

    #[test]
    fn stale_estimated_vblank_is_ignored() {
        let mut scheduler = FrameScheduler::default();
        let old = in_flight(&mut scheduler, FrameOutcome::NoDamage);
        // Something else completed that frame first.
        assert!(!scheduler.presented());
        let new = in_flight(&mut scheduler, FrameOutcome::NoDamage);
        assert_ne!(old, new);
        assert!(!scheduler.estimated_presented(old));
        // `new` is still in flight.
        assert!(!scheduler.request());
        assert!(scheduler.estimated_presented(new));
    }

    #[test]
    fn presented_when_not_in_flight_does_nothing() {
        let mut scheduler = FrameScheduler::default();
        assert!(!scheduler.presented());
        assert!(scheduler.request());
        assert!(!scheduler.presented());
        assert!(scheduler.is_queued());
    }

    #[test]
    fn reset_forgets_the_frame_in_flight() {
        let mut scheduler = FrameScheduler::default();
        let frame = in_flight(&mut scheduler, FrameOutcome::Submitted);
        assert!(scheduler.reset());
        assert!(scheduler.is_queued());
        assert!(!scheduler.estimated_presented(frame));
    }
}

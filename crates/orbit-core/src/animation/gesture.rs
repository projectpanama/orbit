use std::{collections::VecDeque, time::Duration};

/// The maximum age of movement samples used to estimate release velocity.
const VELOCITY_WINDOW: Duration = Duration::from_millis(100);

/// Accumulates gesture movement and estimates its recent velocity.
#[derive(Debug, Default)]
pub struct VelocityTracker {
    /// Timestamped movement samples retained within [`VELOCITY_WINDOW`].
    samples: VecDeque<(Duration, f64)>,
}

impl VelocityTracker {
    /// Records movement at `time` and discards samples that are too old.
    pub fn push(&mut self, time: Duration, delta: f64) {
        self.samples.push_back((time, delta));
        self.prune(time);
    }

    /// Returns the average movement per second over the recent sample window.
    ///
    /// Returns `0.0` when there are fewer than two samples spanning a
    /// positive amount of time. Pausing before releasing naturally causes
    /// older movement to fall outside the window.
    pub fn velocity(&self, now: Duration) -> f64 {
        let mut recent = self
            .samples
            .iter()
            .filter(|(time, _)| now.saturating_sub(*time) <= VELOCITY_WINDOW);
        let Some(&(first, _)) = recent.next() else {
            return 0.0;
        };
        // The first sample's movement happened before its time,
        // only what followed it spans the measured interval.
        let (last, distance) =
            recent.fold((first, 0.0), |(_, sum), &(time, delta)| (time, sum + delta));
        let span = last.saturating_sub(first).as_secs_f64();
        if span > 0.0 { distance / span } else { 0.0 }
    }

    fn prune(&mut self, now: Duration) {
        while self
            .samples
            .front()
            .is_some_and(|(time, _)| now.saturating_sub(*time) > VELOCITY_WINDOW)
        {
            self.samples.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn steady_drag() {
        let mut tracker = VelocityTracker::default();
        for step in 0..=10 {
            tracker.push(ms(step * 10), 0.05);
        }
        // 0.05 per 10 ms.
        assert!((tracker.velocity(ms(100)) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn needs_two_samples() {
        let mut tracker = VelocityTracker::default();
        assert_eq!(tracker.velocity(ms(0)), 0.0);
        tracker.push(ms(5), 1.0);
        assert_eq!(tracker.velocity(ms(5)), 0.0);
    }

    #[test]
    fn pausing_forgets_the_flick() {
        let mut tracker = VelocityTracker::default();
        for step in 0..5 {
            tracker.push(ms(step * 8), -0.1);
        }
        assert!(tracker.velocity(ms(32)) < 0.0);
        assert_eq!(tracker.velocity(ms(400)), 0.0);
        // Pushing later drops the stale samples for good.
        tracker.push(ms(400), 0.01);
        assert_eq!(tracker.samples.len(), 1);
    }
}

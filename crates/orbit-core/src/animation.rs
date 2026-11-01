pub mod workspace;

use std::time::Duration;

/// Distance from the target and speed below which a spring is considered settled.
const SPRING_EPSILON: f64 = 0.0005;

/// Time constant used to compute the spring's angular frequency from a settling time.
const SETTLE_OMEGA_TIME: f64 = 9.23;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Easing {
    /// Interpolates at a constant rate.
    Linear,
    /// Decelerates according to a quadratic curve.
    EaseOutQuad,
    /// Decelerates according to a cubic curve.
    #[default]
    EaseOutCubic,
    /// Decelerates quickly and approaches the target exponentially.
    EaseOutExpo,
}

impl Easing {
    /// Parses an easing name used by Orbit configuration.
    ///
    /// Returns `None` when `name` is not a supported easing name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "linear" => Self::Linear,
            "ease-out-quad" => Self::EaseOutQuad,
            "ease-out-cubic" => Self::EaseOutCubic,
            "ease-out-expo" => Self::EaseOutExpo,
            _ => return None,
        })
    }

    /// Applies the easing curve to a normalized time value.
    ///
    /// The input `t` is expected to be in the range `[0.0, 1.0]`, and the output is
    /// also in that range. Values outside the range are clamped to the nearest endpoint.
    fn apply(self, t: f64) -> f64 {
        let t = if t.is_nan() { 1.0 } else { t.clamp(0.0, 1.0) };
        match self {
            Self::Linear => t,
            Self::EaseOutQuad => 1.0 - (1.0 - t).powi(2),
            Self::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Self::EaseOutExpo if t >= 1.0 => 1.0,
            Self::EaseOutExpo => 1.0 - 2f64.powf(-10.0 * t),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timing {
    /// Total time over which the animation runs.
    pub duration: Duration,
    /// Curve used to interpolate between the endpoints.
    pub easing: Easing,
}

impl Timing {
    /// Timing configuration that disables an animation.
    pub const OFF: Self = Self {
        duration: Duration::ZERO,
        easing: Easing::Linear,
    };

    /// Returns whether the timing configuration disables an animation.
    pub fn is_off(self) -> bool {
        self.duration.is_zero()
    }
}

/// A value animated between two endpoints over a fixed duration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animation {
    from: f64,
    to: f64,
    start: Duration,
    timing: Timing,
}

impl Animation {
    /// Creates an animation beginning at `start`.
    pub fn new(from: f64, to: f64, start: Duration, timing: Timing) -> Self {
        Self {
            from,
            to,
            start,
            timing,
        }
    }

    /// Returns the animation's target value.
    pub fn to(&self) -> f64 {
        self.to
    }

    /// Evaluates the animated value at `now`.
    ///
    /// Values after the animation ends are clamped to the target.
    pub fn value(&self, now: Duration) -> f64 {
        if self.is_done(now) {
            return self.to;
        }
        let elapsed = now.saturating_sub(self.start).as_secs_f64();
        let t = elapsed / self.timing.duration.as_secs_f64();
        self.from + (self.to - self.from) * self.timing.easing.apply(t)
    }

    /// Reports whether the animation has reached its end time.
    pub fn is_done(&self, now: Duration) -> bool {
        now >= self.start.saturating_add(self.timing.duration)
    }
}

/// A critically damped spring moving toward a target value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    to: f64,
    displacement: f64,
    velocity: f64,
    start: Duration,
    omega: f64,
    deadline: Duration,
}

impl Spring {
    /// Creates a spring with an initial position, velocity, and settling time.
    ///
    /// Non-finite initial velocities are treated as zero. A zero settling time
    /// produces an immediately settled spring.
    pub fn new(from: f64, to: f64, velocity: f64, start: Duration, settle: Duration) -> Self {
        let omega = if settle.is_zero() {
            f64::INFINITY
        } else {
            SETTLE_OMEGA_TIME / settle.as_secs_f64()
        };
        Self {
            to,
            displacement: from - to,
            velocity: if velocity.is_finite() { velocity } else { 0.0 },
            start,
            omega,
            deadline: start.saturating_add(settle.saturating_mul(3)),
        }
    }

    /// Returns the spring's target value.
    pub fn to(&self) -> f64 {
        self.to
    }

    /// Evaluates the spring's position at `now`.
    pub fn value(&self, now: Duration) -> f64 {
        let Some((t, decay)) = self.decay(now) else {
            return self.to;
        };
        let c2 = self.velocity + self.omega * self.displacement;
        self.to + (self.displacement + c2 * t) * decay
    }

    /// Evaluates the spring's velocity at `now`.
    pub fn velocity(&self, now: Duration) -> f64 {
        let Some((t, decay)) = self.decay(now) else {
            return 0.0;
        };
        let c2 = self.velocity + self.omega * self.displacement;
        (c2 - self.omega * (self.displacement + c2 * t)) * decay
    }

    /// Reports whether the spring has settled within its tolerance.
    pub fn is_done(&self, now: Duration) -> bool {
        self.decay(now).is_none()
            || ((self.value(now) - self.to).abs() < SPRING_EPSILON
                && self.velocity(now).abs() < SPRING_EPSILON * self.omega)
    }

    fn decay(&self, now: Duration) -> Option<(f64, f64)> {
        if !self.omega.is_finite() || now >= self.deadline {
            return None;
        }
        let t = now.saturating_sub(self.start).as_secs_f64();
        Some((t, (-self.omega * t).exp()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Easing; 4] = [
        Easing::Linear,
        Easing::EaseOutQuad,
        Easing::EaseOutCubic,
        Easing::EaseOutExpo,
    ];

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn easing_endpoints_and_monotonic() {
        for easing in ALL {
            assert_eq!(easing.apply(0.0), 0.0, "{easing:?}");
            assert_eq!(easing.apply(1.0), 1.0, "{easing:?}");
            assert_eq!(easing.apply(-3.0), 0.0, "{easing:?}");
            assert_eq!(easing.apply(7.0), 1.0, "{easing:?}");
            let mut last = 0.0;
            for step in 0..=1000 {
                let value = easing.apply(f64::from(step) / 1000.0);
                assert!(value >= last, "{easing:?} goes backwards at {step}");
                last = value;
            }
        }
    }

    #[test]
    fn easing_names() {
        assert_eq!(Easing::from_name("linear"), Some(Easing::Linear));
        assert_eq!(
            Easing::from_name("ease-out-expo"),
            Some(Easing::EaseOutExpo)
        );
        assert_eq!(Easing::from_name("bouncy"), None);
    }

    #[test]
    fn animation_runs_from_start_to_end() {
        let timing = Timing {
            duration: ms(200),
            easing: Easing::Linear,
        };
        let animation = Animation::new(10.0, 20.0, ms(1000), timing);
        assert_eq!(animation.value(ms(0)), 10.0);
        assert_eq!(animation.value(ms(1000)), 10.0);
        assert!((animation.value(ms(1100)) - 15.0).abs() < 1e-9);
        assert!(!animation.is_done(ms(1199)));
        assert!(animation.is_done(ms(1200)));
        assert_eq!(animation.value(ms(1200)), 20.0);
        assert_eq!(animation.value(ms(5000)), 20.0);
    }

    #[test]
    fn spring_settles_on_its_target() {
        let spring = Spring::new(0.0, 1.0, 0.0, ms(100), ms(250));
        assert_eq!(spring.value(ms(100)), 0.0);
        assert_eq!(spring.velocity(ms(100)), 0.0);
        assert!(!spring.is_done(ms(150)));
        // From rest it approaches without overshooting.
        let mut last = 0.0;
        for step in 0..=400 {
            let value = spring.value(ms(100 + step));
            assert!(value >= last && value <= 1.0, "at {step} ms: {value}");
            last = value;
        }
        assert!(spring.is_done(ms(100 + 300)));
        assert_eq!(spring.value(ms(100 + 800)), 1.0);
    }

    #[test]
    fn spring_keeps_the_initial_velocity() {
        let spring = Spring::new(0.3, 1.0, 4.0, ms(0), ms(250));
        assert!((spring.velocity(ms(0)) - 4.0).abs() < 1e-9);
        // Just after the start it moves at about that speed.
        let early = (spring.value(Duration::from_micros(100)) - 0.3) / 0.0001;
        assert!((early - 4.0).abs() < 0.1, "{early}");
        // Thrown backwards, it turns around and still ends on target.
        let back = Spring::new(0.5, 1.0, -3.0, ms(0), ms(250));
        assert!(back.value(ms(5)) < 0.5);
        assert!(back.is_done(ms(750)));
        assert_eq!(back.value(ms(750)), 1.0);
    }

    #[test]
    fn spring_is_the_same_at_any_refresh_rate() {
        let spring = Spring::new(0.2, 1.0, 2.0, ms(0), ms(250));
        let at = |hz: u64, frame: u64| Duration::from_nanos(frame * 1_000_000_000 / hz);
        for n in 0..=6 {
            assert_eq!(spring.value(at(60, 5 * n)), spring.value(at(144, 12 * n)));
        }
    }

    #[test]
    fn zero_settle_springs_jump() {
        let spring = Spring::new(0.0, 2.0, 5.0, ms(10), Duration::ZERO);
        assert!(spring.is_done(ms(10)));
        assert_eq!(spring.value(ms(10)), 2.0);
    }

    #[test]
    fn off_finishes_immediately() {
        let animation = Animation::new(0.0, 1.0, ms(500), Timing::OFF);
        assert!(animation.is_done(ms(500)));
        assert_eq!(animation.value(ms(500)), 1.0);
    }
}

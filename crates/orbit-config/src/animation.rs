use std::time::Duration;

use orbit_core::animation::{Easing, Timing};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawTiming {
    #[serde(default)]
    off: bool,
    duration_ms: Option<i64>,
    curve: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RawAnimations {
    #[serde(default)]
    off: bool,
    slowdown: Option<f64>,
    overview: Option<RawTiming>,
    workspace_switch: Option<RawTiming>,
    window_open: Option<RawTiming>,
}

/// Minimum accepted animation slowdown multiplier.
const MIN_SLOWDOWN: f64 = 0.1;
/// Maximum accepted animation slowdown multiplier.
const MAX_SLOWDOWN: f64 = 10.0;
/// Maximum accepted individual animation duration.
const MAX_ANIMATION_MS: i64 = 10_000;

/// Timing configuration for compositor animations.
///
/// The global configuration can disable every animation, and each individual
/// animation can also be disabled independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationConfig {
    /// Timing for entering and leaving the overview.
    pub overview: Timing,
    /// Timing for switching between workspaces.
    pub workspace_switch: Timing,
    /// Timing for opening windows.
    pub window_open: Timing,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            overview: Timing {
                duration: Duration::from_millis(300),
                easing: Easing::EaseOutCubic,
            },
            workspace_switch: Timing {
                duration: Duration::from_millis(250),
                easing: Easing::EaseOutCubic,
            },
            window_open: Timing {
                duration: Duration::from_millis(150),
                easing: Easing::EaseOutCubic,
            },
        }
    }
}

impl AnimationConfig {
    /// Converts raw settings into validated animation timings.
    ///
    /// Invalid values are logged and replaced with defaults. The slowdown
    /// multiplier is applied to each resulting duration.
    pub(crate) fn from_raw(raw: RawAnimations) -> Self {
        let defaults = Self::default();
        if raw.off {
            return Self {
                workspace_switch: Timing::OFF,
                window_open: Timing::OFF,
                overview: Timing::OFF,
            };
        }
        let slowdown = match raw.slowdown {
            None => 1.0,
            Some(slowdown) if (MIN_SLOWDOWN..=MAX_SLOWDOWN).contains(&slowdown) => slowdown,
            Some(slowdown) => {
                tracing::warn!(
                    "ignoring animation slowdown {slowdown}: not {MIN_SLOWDOWN}..={MAX_SLOWDOWN}"
                );
                1.0
            }
        };
        let timing = |name: &str, raw: Option<RawTiming>, default: Timing| {
            let mut timing = default;
            if let Some(raw) = raw {
                if raw.off {
                    return Timing::OFF;
                }
                match raw.duration_ms {
                    None => {}
                    Some(ms) if (0..=MAX_ANIMATION_MS).contains(&ms) => {
                        timing.duration = Duration::from_millis(ms.unsigned_abs());
                    }
                    Some(ms) => tracing::warn!(
                        "ignoring {name} duration {ms} ms: not 0..={MAX_ANIMATION_MS}"
                    ),
                }
                match raw
                    .curve
                    .as_deref()
                    .map(|curve| (curve, Easing::from_name(curve)))
                {
                    None => {}
                    Some((_, Some(easing))) => timing.easing = easing,
                    Some((curve, None)) => {
                        tracing::warn!("ignoring {name} curve: unknown curve `{curve}`");
                    }
                }
            }
            timing.duration = timing.duration.mul_f64(slowdown);
            timing
        };
        Self {
            overview: timing("overview", raw.overview, defaults.overview),
            workspace_switch: timing(
                "workspace-switch",
                raw.workspace_switch,
                defaults.workspace_switch,
            ),
            window_open: timing("window-open", raw.window_open, defaults.window_open),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn parses_animations() {
        assert_eq!(
            Config::parse("").unwrap().animations,
            AnimationConfig::default()
        );

        let config = Config::parse(
            r#"
            [animations]
            slowdown = 2
            [animations.workspace-switch]
            duration-ms = 300
            curve = "ease-out-expo"
            [animations.window-open]
            off = true
            "#,
        )
        .unwrap();
        assert_eq!(
            config.animations.workspace_switch,
            Timing {
                duration: Duration::from_millis(600),
                easing: Easing::EaseOutExpo,
            }
        );
        assert!(config.animations.window_open.is_off());

        let off = Config::parse("[animations]\noff = true").unwrap();
        assert!(off.animations.workspace_switch.is_off());
        assert!(off.animations.window_open.is_off());
    }

    #[test]
    fn bad_animation_values_fall_back_to_defaults() {
        let config = Config::parse(
            r#"
            [animations]
            slowdown = 50
            [animations.workspace-switch]
            duration-ms = -5
            curve = "bouncy"
            "#,
        )
        .unwrap();
        assert_eq!(config.animations, AnimationConfig::default());
        assert!(Config::parse("[animations]\nspeed = 2").is_err());
    }
}

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawMouse {
    acceleration_speed: Option<f64>,
    acceleration_profile: Option<String>,
    natural_scroll: Option<bool>,
    left_handed: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawTouchpad {
    acceleration_speed: Option<f64>,
    acceleration_profile: Option<String>,
    natural_scroll: Option<bool>,
    tap_to_click: Option<bool>,
    disable_while_typing: Option<bool>,
    two_finger_scroll: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawKeyboard {
    layout: Option<String>,
    variant: Option<String>,
    options: Option<String>,
    model: Option<String>,
    repeat_rate: Option<i64>,
    repeat_delay: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawInput {
    #[serde(default)]
    mouse: RawMouse,
    #[serde(default)]
    touchpad: RawTouchpad,
    #[serde(default)]
    keyboard: RawKeyboard,
}

/// Pointer acceleration profile supported by libinput.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccelerationProfile {
    /// Adapts acceleration to movement speed.
    #[default]
    Adaptive,
    /// Applies a constant pointer response.
    Flat,
}

/// Libinput settings shared by mice and touchpads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerConfig {
    /// Pointer acceleration speed in the range `-1.0..=1.0`.
    pub acceleration_speed: f64,
    /// Pointer acceleration profile.
    pub acceleration_profile: AccelerationProfile,
    /// Whether scrolling follows natural finger movement.
    pub natural_scroll: bool,
    /// Whether the device uses left-handed button mapping.
    pub left_handed: bool,
    /// Whether tapping acts as a click.
    pub tap_to_click: bool,
    /// Whether the touchpad is disabled while typing.
    pub disable_while_typing: bool,
    /// Whether two-finger scrolling is enabled instead of edge scrolling.
    pub two_finger_scroll: bool,
}

impl PointerConfig {
    /// Default settings for a mouse.
    pub const MOUSE: Self = Self {
        acceleration_speed: 0.0,
        acceleration_profile: AccelerationProfile::Adaptive,
        natural_scroll: false,
        left_handed: false,
        tap_to_click: false,
        disable_while_typing: false,
        two_finger_scroll: true,
    };

    /// Default settings for a touchpad.
    pub const TOUCHPAD: Self = Self {
        acceleration_speed: 0.0,
        acceleration_profile: AccelerationProfile::Adaptive,
        natural_scroll: true,
        left_handed: false,
        tap_to_click: true,
        disable_while_typing: true,
        two_finger_scroll: true,
    };
}

/// Keyboard layout, XKB options, and key-repeat settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyboardConfig {
    /// XKB layout names where an empty string uses the XKB default.
    pub layout: String,
    /// XKB layout variant.
    pub variant: String,
    /// XKB options.
    pub options: String,
    /// XKB keyboard model.
    pub model: String,
    /// Key repetitions per second.
    pub repeat_rate: i32,
    /// Delay before repetition starts (milliseconds).
    pub repeat_delay: i32,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            layout: String::new(),
            variant: String::new(),
            options: String::new(),
            model: String::new(),
            repeat_rate: 25,
            repeat_delay: 600,
        }
    }
}

/// Complete mouse, touchpad, and keyboard configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct InputConfig {
    /// Mouse configuration.
    pub mouse: PointerConfig,
    /// Touchpad configuration.
    pub touchpad: PointerConfig,
    /// Keyboard configuration.
    pub keyboard: KeyboardConfig,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            mouse: PointerConfig::MOUSE,
            touchpad: PointerConfig::TOUCHPAD,
            keyboard: KeyboardConfig::default(),
        }
    }
}

impl InputConfig {
    /// Converts raw configuration into validated settings with defaults.
    pub(crate) fn from_raw(raw: RawInput) -> Self {
        let raw_mouse = raw.mouse;
        let mouse = PointerConfig {
            acceleration_speed: validate_acceleration_speed("mouse", raw_mouse.acceleration_speed),
            acceleration_profile: parse_acceleration_profile(
                "mouse",
                raw_mouse.acceleration_profile.as_deref(),
            ),
            natural_scroll: raw_mouse
                .natural_scroll
                .unwrap_or(PointerConfig::MOUSE.natural_scroll),
            left_handed: raw_mouse
                .left_handed
                .unwrap_or(PointerConfig::MOUSE.left_handed),
            ..PointerConfig::MOUSE
        };

        let raw_touchpad = raw.touchpad;
        let touchpad_defaults = PointerConfig::TOUCHPAD;
        let touchpad = PointerConfig {
            acceleration_speed: validate_acceleration_speed(
                "touchpad",
                raw_touchpad.acceleration_speed,
            ),
            acceleration_profile: parse_acceleration_profile(
                "touchpad",
                raw_touchpad.acceleration_profile.as_deref(),
            ),
            natural_scroll: raw_touchpad
                .natural_scroll
                .unwrap_or(touchpad_defaults.natural_scroll),
            tap_to_click: raw_touchpad
                .tap_to_click
                .unwrap_or(touchpad_defaults.tap_to_click),
            disable_while_typing: raw_touchpad
                .disable_while_typing
                .unwrap_or(touchpad_defaults.disable_while_typing),
            two_finger_scroll: raw_touchpad
                .two_finger_scroll
                .unwrap_or(touchpad_defaults.two_finger_scroll),
            ..touchpad_defaults
        };

        // Helper to validate a numeric value is in range, warning and defaulting when it is not.
        let ranged = |name: &str,
                      v: Option<i64>,
                      range: std::ops::RangeInclusive<i64>,
                      default: i32| match v {
            None => default,
            Some(v) if range.contains(&v) => v as i32,
            Some(v) => {
                tracing::warn!(
                    "ignoring input.keyboard.{name} {v}: not in {}..={}",
                    range.start(),
                    range.end()
                );
                default
            }
        };

        let raw_keyboard = raw.keyboard;
        let keyboard_defaults = KeyboardConfig::default();
        let keyboard = KeyboardConfig {
            layout: raw_keyboard.layout.unwrap_or_default(),
            variant: raw_keyboard.variant.unwrap_or_default(),
            options: raw_keyboard.options.unwrap_or_default(),
            model: raw_keyboard.model.unwrap_or_default(),
            repeat_rate: ranged(
                "repeat-rate",
                raw_keyboard.repeat_rate,
                1..=200,
                keyboard_defaults.repeat_rate,
            ),
            repeat_delay: ranged(
                "repeat-delay",
                raw_keyboard.repeat_delay,
                50..=5000,
                keyboard_defaults.repeat_delay,
            ),
        };

        Self {
            mouse,
            touchpad,
            keyboard,
        }
    }
}

/// Validates an acceleration speed, warning and defaulting when it is out of range.
fn validate_acceleration_speed(section: &str, value: Option<f64>) -> f64 {
    match value {
        None => 0.0,
        Some(v) if (-1.0..=1.0).contains(&v) => v,
        Some(v) => {
            tracing::warn!("ignoring input.{section}.acceleration-speed {v}: not in -1..=1");
            0.0
        }
    }
}

/// Parses an acceleration profile, warning and defaulting on unknown values.
fn parse_acceleration_profile(section: &str, value: Option<&str>) -> AccelerationProfile {
    match value {
        None | Some("adaptive") => AccelerationProfile::Adaptive,
        Some("flat") => AccelerationProfile::Flat,
        Some(other) => {
            tracing::warn!(
                "ignoring input.{section}.acceleration-profile `{other}`: adaptive | flat"
            );
            AccelerationProfile::Adaptive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn parses_input() {
        let config = Config::parse(
            r#"
[input.keyboard]
layout = "us,de"
options = "caps:escape"
repeat-rate = 40
repeat-delay = 300
[input.mouse]
acceleration-speed = -0.5
acceleration-profile = "flat"
left-handed = true
[input.touchpad]
natural-scroll = false
tap-to-click = false
two-finger-scroll = false
"#,
        )
        .expect("valid");
        let i = &config.input;
        assert_eq!(i.keyboard.layout, "us,de");
        assert_eq!(i.keyboard.options, "caps:escape");
        assert_eq!((i.keyboard.repeat_rate, i.keyboard.repeat_delay), (40, 300));
        assert_eq!(i.mouse.acceleration_speed, -0.5);
        assert_eq!(i.mouse.acceleration_profile, AccelerationProfile::Flat);
        assert!(i.mouse.left_handed && !i.mouse.natural_scroll);
        assert!(
            !i.touchpad.natural_scroll && !i.touchpad.tap_to_click && !i.touchpad.two_finger_scroll
        );
        assert!(i.touchpad.disable_while_typing);
        assert_eq!(Config::default().input, InputConfig::default());
    }

    #[test]
    fn bad_input_values_fall_back() {
        let config = Config::parse(
            "[input.keyboard]\nrepeat-rate = 0\n[input.mouse]\nacceleration-speed = 3\nacceleration-profile = \"wild\"\n",
        )
        .expect("still valid");
        assert_eq!(config.input.keyboard.repeat_rate, 25);
        assert_eq!(config.input.mouse.acceleration_speed, 0.0);
        assert_eq!(
            config.input.mouse.acceleration_profile,
            AccelerationProfile::Adaptive
        );
        assert!(Config::parse("[input.mouse]\ntap-to-click = true\n").is_err());
    }
}

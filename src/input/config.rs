use orbit_config::{AccelerationProfile, InputConfig, KeyboardConfig, PointerConfig};
use smithay::{
    input::keyboard::XkbConfig,
    reexports::input::{AccelProfile, Device, DeviceCapability, ScrollMethod},
};

use crate::state::Orbit;

impl Orbit {
    /// Applies the configured keyboard layout and repeat settings.
    ///
    /// Invalid XKB layouts are logged and leave the previous layout active.
    pub fn apply_keyboard_config(&mut self) {
        let config = self.config.input.keyboard.clone();
        let keyboard = self.keyboard.clone();
        if let Err(err) = keyboard.set_xkb_config(self, xkb_config(&config)) {
            tracing::warn!(?err, layout = %config.layout, "invalid keyboard layout, keeping the previous one");
        }
        keyboard.change_repeat_info(config.repeat_rate, config.repeat_delay);
    }
}

/// Converts Orbit's keyboard settings into Smithay's XKB configuration.
///
/// Empty XKB fields are passed through as empty values so XKB can apply its
/// defaults. Empty options are represented as `None`.
pub fn xkb_config(config: &KeyboardConfig) -> XkbConfig<'_> {
    XkbConfig {
        rules: "",
        model: &config.model,
        layout: &config.layout,
        variant: &config.variant,
        options: (!config.options.is_empty()).then(|| config.options.clone()),
    }
}

/// Applies the configured mouse or touchpad settings to a libinput device.
///
/// Settings unavailable on the device are skipped. Individual libinput setter
/// errors are intentionally discarded because input configuration is best
/// effort and should not prevent the compositor from starting.
pub fn configure_device(device: &mut Device, config: &InputConfig) {
    if !device.has_capability(DeviceCapability::Pointer) {
        return;
    }

    let touchpad = device.config_tap_finger_count() > 0;

    let pointer: &PointerConfig = if touchpad {
        &config.touchpad
    } else {
        &config.mouse
    };

    // Apply pointer acceleration and profile settings if available.
    if device.config_accel_is_available() {
        let _ = device.config_accel_set_speed(pointer.acceleration_speed);
        let profile = match pointer.acceleration_profile {
            AccelerationProfile::Adaptive => AccelProfile::Adaptive,
            AccelerationProfile::Flat => AccelProfile::Flat,
        };
        if device.config_accel_profiles().contains(&profile) {
            let _ = device.config_accel_set_profile(profile);
        }
    }

    // Apply natural scroll setting if available.
    if device.config_scroll_has_natural_scroll() {
        let _ = device.config_scroll_set_natural_scroll_enabled(pointer.natural_scroll);
    }

    // Apply left-handed setting if available.
    if device.config_left_handed_is_available() {
        let _ = device.config_left_handed_set(pointer.left_handed);
    }

    // Apply tap-to-click, disable-while-typing, and scroll method settings if available.
    if touchpad {
        let _ = device.config_tap_set_enabled(pointer.tap_to_click);
        if device.config_dwt_is_available() {
            let _ = device.config_dwt_set_enabled(pointer.disable_while_typing);
        }
        let method = if pointer.two_finger_scroll {
            ScrollMethod::TwoFinger
        } else {
            ScrollMethod::Edge
        };
        if device.config_scroll_methods().contains(&method) {
            let _ = device.config_scroll_set_method(method);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xkb_names_pass_through() {
        let config = KeyboardConfig {
            layout: "us,de".into(),
            options: "caps:escape".into(),
            ..KeyboardConfig::default()
        };
        let xkb = xkb_config(&config);
        assert_eq!(xkb.layout, "us,de");
        assert_eq!(xkb.options.as_deref(), Some("caps:escape"));
        assert_eq!(xkb_config(&KeyboardConfig::default()).options, None);
    }
}

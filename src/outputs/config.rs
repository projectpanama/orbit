use orbit_config::{MAX_SCALE, MIN_SCALE, ModeSpec, OutputConfig};
use smithay::{
    output::{Mode, Output, Scale},
    utils::{Logical, Point, Transform},
};

use crate::{backend::Backend, geometry::ToCore, state::Orbit};

/// A display mode exposed by a connected output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadMode {
    /// Resolution in physical pixels.
    pub size: (i32, i32),
    /// Refresh rate in millihertz.
    pub refresh: i32,
    /// Whether this is the output's preferred mode.
    pub preferred: bool,
}

/// Current state and capabilities of a connected monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadInfo {
    /// Connector or output name.
    pub name: String,
    /// Human-readable output description.
    pub description: String,
    /// Monitor manufacturer.
    pub make: String,
    /// Monitor model.
    pub model: String,
    /// Monitor serial number.
    pub serial: String,
    /// Physical dimensions in millimetres; zero when unknown.
    pub size_mm: (i32, i32),
    /// Modes supported by the output.
    pub modes: Vec<HeadMode>,
    /// Whether the output is currently enabled.
    pub enabled: bool,
    /// Current mode index in `modes`, when enabled.
    pub current: Option<usize>,
    /// Top-left position in the global logical layout.
    pub position: Point<i32, Logical>,
    /// Current rotation and flip.
    pub transform: Transform,
    /// Current fractional scale.
    pub scale: f64,
}

/// Requested changes for one output.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadChange {
    /// Name of the output to change.
    pub name: String,
    /// Whether the output should be enabled.
    pub enabled: bool,
    /// Requested `(width, height)` and refresh rate in millihertz.
    ///
    /// A refresh rate of zero accepts any refresh rate for the requested size.
    pub mode: Option<((i32, i32), i32)>,
    /// New top-left position in the global logical layout.
    pub position: Option<Point<i32, Logical>>,
    /// New rotation and flip.
    pub transform: Option<Transform>,
    /// New fractional scale.
    pub scale: Option<f64>,
}

impl HeadChange {
    /// Creates an enable or disable request for an output name.
    pub fn new(name: &str, enabled: bool) -> Self {
        Self {
            name: name.to_string(),
            enabled,
            mode: None,
            position: None,
            transform: None,
            scale: None,
        }
    }
}

impl Orbit {
    /// Reports whether Orbit is running inside a nested window backend.
    fn is_nested(&self) -> bool {
        matches!(self.backend, Some(Backend::Winit(_)))
    }

    /// Returns all connected outputs, enabled ones first, sorted by name.
    ///
    /// Disabled outputs are included when the active backend can report them.
    pub fn heads(&self) -> Vec<HeadInfo> {
        let nested = self.is_nested();

        let mut outputs: Vec<HeadInfo> = self
            .outputs
            .keys()
            .map(|output| head_of(output, nested))
            .collect();
        outputs.sort_by(|a, b| a.name.cmp(&b.name));

        let mut off = match &self.backend {
            // TODO: add Tty support once implemented and sort this list.
            #[cfg(test)]
            Some(Backend::Headless(h)) => h
                .disabled
                .iter()
                .map(|o| HeadInfo {
                    enabled: false,
                    current: None,
                    ..head_of(o, false)
                })
                .collect(),
            _ => Vec::new(),
        };

        outputs.extend(off);
        outputs
    }

    /// Returns configured settings for an output.
    ///
    /// Runtime overrides take precedence over the static `[outputs]`
    /// configuration. Unknown outputs receive default settings.
    pub fn output_settings(&self, name: &str, description: &str) -> OutputConfig {
        if let Some(o) = self.output_management.overrides.get(name) {
            return o.clone();
        }
        self.config
            .output(name, description)
            .cloned()
            .unwrap_or_default()
    }

    /// Validates and optionally applies output changes.
    ///
    /// When `test_only` is `true`, validation runs without changing state.
    /// Outputs not mentioned in `changes` remain unchanged and at least one
    /// monitor must remain enabled.
    pub fn apply_heads(&mut self, changes: &[HeadChange], test_only: bool) -> Result<(), String> {
        let outputs = self.heads();
        let nested = self.is_nested();

        for change in changes {
            let output = outputs
                .iter()
                .find(|info| info.name == change.name)
                .ok_or_else(|| format!("no monitor {}", change.name))?;
            if !change.enabled {
                if nested {
                    return Err("the nested window can't be turned off".into());
                }
                continue;
            }
            if let Some(scale) = change.scale
                && !(MIN_SCALE..=MAX_SCALE).contains(&scale)
            {
                return Err(format!(
                    "scale {scale} is outside {MIN_SCALE}..={MAX_SCALE}"
                ));
            }
            if let Some(((width, height), refresh)) = change.mode {
                let known = output
                    .modes
                    .iter()
                    .any(|m| m.size == (width, height) && (refresh == 0 || m.refresh == refresh));
                if !known {
                    return Err(format!(
                        "{} has no {width}x{height} mode at {refresh} mHz",
                        change.name
                    ));
                }
                if nested
                    && output
                        .current
                        .and_then(|i| output.modes.get(i))
                        .map(|m| m.size)
                        != Some((width, height))
                {
                    return Err("the nested window's size is set by the host".into());
                }
            }
            if nested
                && change
                    .transform
                    .is_some_and(|transform| transform != Transform::Normal)
            {
                return Err("the nested window can't be rotated".into());
            }
        }

        let lit_after = outputs
            .iter()
            .filter(|output| {
                changes
                    .iter()
                    .find(|change| change.name == output.name)
                    .map_or(output.enabled, |change| change.enabled)
            })
            .count();

        if lit_after == 0 {
            return Err("that would turn every monitor off".into());
        }
        if test_only {
            return Ok(());
        }

        for change in changes {
            let Some(output) = outputs.iter().find(|info| info.name == change.name) else {
                continue;
            };
            let mut settings = self.output_settings(&output.name, &output.description);
            settings.off = !change.enabled;
            if let Some(((w, h), refresh)) = change.mode {
                settings.mode = Some(ModeSpec {
                    width: w as u16,
                    height: h as u16,
                    refresh_mhz: (refresh > 0).then_some(refresh),
                });
            }
            if let Some(p) = change.position {
                settings.position = Some(p.to_core());
            }
            if let Some(t) = change.transform {
                settings.transform = Some(to_config_transform(t));
            }
            if let Some(s) = change.scale {
                settings.scale = Some((s * 120.0).round() / 120.0);
            }
            self.output_management
                .overrides
                .insert(output.name.clone(), settings.clone());
            if output.enabled {
                if change.enabled {
                    self.update_lit_output(&output.name, &settings)?;
                } else {
                    self.set_output_enabled(&output.name, false)?;
                }
            } else if change.enabled {
                self.set_output_enabled(&output.name, true)?;
            }
        }

        self.relayout_outputs();
        self.output_management.dirty = true;
        Ok(())
    }

    /// Applies mode, transform, scale, and position settings to a lit output.
    ///
    /// This does not perform a full output layout pass.
    pub fn update_lit_output(&mut self, name: &str, settings: &OutputConfig) -> Result<(), String> {
        let Some(output) = self.outputs.keys().find(|o| o.name() == name).cloned() else {
            return Err(format!("{name} isn't lit"));
        };

        if let Some(spec) = settings.mode {
            let current = output.current_mode();
            let wanted = output.modes().into_iter().filter(|m| {
                m.size.w == i32::from(spec.width) && m.size.h == i32::from(spec.height)
            });
            let wanted = match spec.refresh_mhz {
                Some(r) => wanted.min_by_key(|m| (m.refresh - r).abs()),
                None => wanted.max_by_key(|m| m.refresh),
            };
            if let Some(mode) = wanted
                && Some(mode) != current
            {
                self.set_output_mode(&output, mode)?;
            }
        }

        let nested = self.is_nested();
        let transform = if nested {
            None
        } else {
            Some(
                settings
                    .transform
                    .map_or(Transform::Normal, to_smithay_transform),
            )
        };

        let scale = settings.scale.map(Scale::Fractional);
        if transform.is_some() || scale.is_some() {
            output.change_current_state(None, transform, scale, None);
        }
        if let Some(state) = self.outputs.get_mut(&output) {
            state.configured_position = settings.position;
        }

        Ok(())
    }

    /// Changes an output's mode through the active backend.
    fn set_output_mode(&mut self, output: &Output, mode: Mode) -> Result<(), String> {
        let _output = output.clone();
        let result = self.with_backend(|backend, _orbit| match backend {
            // TODO: add Tty support once implemented. Likely to be the same function signature.
            // Backend::Tty(tty) => tty.set_mode(&output, mode),
            Backend::Winit(_) => Err("the nested window's size is set by the host".to_string()),
            #[cfg(test)]
            Backend::Headless(_) => {
                output.change_current_state(Some(mode), None, None, None);
                Ok(())
            }
        });
        result.unwrap_or_else(|| Err("no backend".into()))
    }

    /// Enables or disables an output through the active backend.
    fn set_output_enabled(&mut self, name: &str, on: bool) -> Result<(), String> {
        let _name = name.to_string();
        let result = self.with_backend(|backend, orbit| match backend {
            // TODO: add Tty support once implemented. Not sure it will be exactly this function signature, but it will be similar.
            // Backend::Tty(tty) => tty.set_enabled(orbit, &name, on),
            Backend::Winit(_) => Err("the nested window can't be turned off".to_string()),
            #[cfg(test)]
            Backend::Headless(h) => {
                if on {
                    if let Some(i) = h.disabled.iter().position(|o| o.name() == name) {
                        let output = h.disabled.remove(i);
                        h.outputs.push(output.clone());
                        orbit.add_output(&output, crate::outputs::OutputSetup::default());
                    }
                } else if let Some(output) = h.outputs.iter().find(|o| o.name() == name).cloned() {
                    h.outputs.retain(|o| *o != output);
                    h.disabled.push(output.clone());
                    orbit.remove_output(&output);
                }
                Ok(())
            }
        });
        result.unwrap_or_else(|| Err("no backend".into()))
    }

    /// Discards runtime overrides and reapplies static `[outputs]` settings.
    ///
    /// Errors are logged per output so one invalid or unavailable monitor does
    /// not prevent other outputs from being updated.
    pub fn reapply_output_config(&mut self) {
        self.output_management.overrides.clear();
        for output in self.heads() {
            let settings = self.output_settings(&output.name, &output.description);
            let result = match (output.enabled, settings.off) {
                (true, true) => self.set_output_enabled(&output.name, false),
                (false, false) => self.set_output_enabled(&output.name, true),
                (true, false) => self.update_lit_output(&output.name, &settings),
                (false, true) => Ok(()),
            };
            if let Err(err) = result {
                tracing::warn!("couldn't apply the config to {}: {err}", output.name);
            }
        }
        self.relayout_outputs();
        self.output_management.dirty = true;
    }
}

/// Describes a currently connected output and its available modes.
pub fn head_of(output: &Output, nested: bool) -> HeadInfo {
    let current = output.current_mode();
    let preferred = output.preferred_mode();
    let properties = output.physical_properties();

    let mut modes: Vec<HeadMode> = Vec::new();

    for mode in output.modes() {
        let output_mode = HeadMode {
            size: (mode.size.w, mode.size.h),
            refresh: mode.refresh,
            preferred: Some(mode) == preferred,
        };
        if !modes.contains(&output_mode) {
            modes.push(output_mode);
        }
    }

    let current = current.and_then(|mode| {
        modes.iter().position(|output_mode| {
            output_mode.size == (mode.size.w, mode.size.h) && output_mode.refresh == mode.refresh
        })
    });

    HeadInfo {
        name: output.name(),
        description: output.description(),
        make: properties.make,
        model: properties.model,
        serial: properties.serial_number,
        size_mm: (properties.size.w, properties.size.h),
        modes,
        enabled: true,
        current,
        position: output.current_location(),
        transform: if nested {
            Transform::Normal
        } else {
            output.current_transform()
        },
        scale: output.current_scale().fractional_scale(),
    }
}

/// Converts an Orbit configuration transform to Smithay's representation.
pub fn to_smithay_transform(transform: orbit_config::Transform) -> Transform {
    use orbit_config::Transform as T;
    match transform {
        T::Normal => Transform::Normal,
        T::Rotate90 => Transform::_90,
        T::Rotate180 => Transform::_180,
        T::Rotate270 => Transform::_270,
        T::Flipped => Transform::Flipped,
        T::Flipped90 => Transform::Flipped90,
        T::Flipped180 => Transform::Flipped180,
        T::Flipped270 => Transform::Flipped270,
    }
}

/// Converts Smithay's transform representation to Orbit configuration form.
fn to_config_transform(transform: Transform) -> orbit_config::Transform {
    use orbit_config::Transform as T;
    match transform {
        Transform::Normal => T::Normal,
        Transform::_90 => T::Rotate90,
        Transform::_180 => T::Rotate180,
        Transform::_270 => T::Rotate270,
        Transform::Flipped => T::Flipped,
        Transform::Flipped90 => T::Flipped90,
        Transform::Flipped180 => T::Flipped180,
        Transform::Flipped270 => T::Flipped270,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_convert_both_ways() {
        use orbit_config::Transform as T;
        for t in [
            T::Normal,
            T::Rotate90,
            T::Rotate180,
            T::Rotate270,
            T::Flipped,
            T::Flipped90,
            T::Flipped180,
            T::Flipped270,
        ] {
            assert_eq!(to_config_transform(to_smithay_transform(t)), t);
        }
        assert_eq!(to_smithay_transform(T::Rotate90), Transform::_90);
    }
}

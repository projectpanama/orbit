use orbit_core::layout::geometry::Point;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawScale {
    Value(f64),
    Name(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPosition {
    x: i32,
    y: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawOutput {
    #[serde(default)]
    off: bool,
    mode: Option<String>,
    scale: Option<RawScale>,
    position: Option<RawPosition>,
    transform: Option<String>,
}

/// Minimum supported fractional output scale.
pub const MIN_SCALE: f64 = 0.25;

/// Maximum supported fractional output scale.
pub const MAX_SCALE: f64 = 8.0;

/// A display mode requested in configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeSpec {
    /// Horizontal resolution in physical pixels.
    pub width: u16,
    /// Vertical resolution in physical pixels.
    pub height: u16,
    /// Refresh rate in millihertz. `None` selects the highest available rate.
    pub refresh_mhz: Option<i32>,
}

impl ModeSpec {
    /// Parses `WIDTHxHEIGHT` or `WIDTHxHEIGHT@HZ`.
    ///
    /// The refresh rate may be fractional and must be finite, positive, and
    /// below 1000 Hz.
    fn parse(spec: &str) -> Result<Self, String> {
        let (size, refresh) = match spec.split_once('@') {
            Some((size, refresh)) => (size, Some(refresh)),
            None => (spec, None),
        };
        let (width, height) = size
            .split_once('x')
            .ok_or_else(|| format!("mode `{spec}` is not WIDTHxHEIGHT[@HZ]"))?;
        let number = |text: &str| {
            text.trim()
                .parse::<u16>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("invalid size in mode `{spec}`"))
        };
        let refresh_mhz = match refresh {
            None => None,
            Some(hz) => match hz.trim().parse::<f64>() {
                Ok(hz) if hz.is_finite() && hz > 0.0 && hz < 1000.0 => {
                    Some((hz * 1000.0).round() as i32)
                }
                _ => return Err(format!("invalid refresh rate in mode `{spec}`")),
            },
        };
        Ok(Self {
            width: number(width)?,
            height: number(height)?,
            refresh_mhz,
        })
    }
}

/// A monitor's rotation and optional horizontal flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    /// No rotation or flip.
    Normal,
    /// Rotate 90 degrees.
    Rotate90,
    /// Rotate 180 degrees.
    Rotate180,
    /// Rotate 270 degrees.
    Rotate270,
    /// Flip without rotation.
    Flipped,
    /// Flip, then rotate 90 degrees.
    Flipped90,
    /// Flip, then rotate 180 degrees.
    Flipped180,
    /// Flip, then rotate 270 degrees.
    Flipped270,
}

impl Transform {
    /// Parses a transform name such as `normal`, `90`, or `flipped-90`.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "normal" | "0" => Self::Normal,
            "90" => Self::Rotate90,
            "180" => Self::Rotate180,
            "270" => Self::Rotate270,
            "flipped" => Self::Flipped,
            "flipped-90" => Self::Flipped90,
            "flipped-180" => Self::Flipped180,
            "flipped-270" => Self::Flipped270,
            _ => return None,
        })
    }
}

/// Validated settings for one monitor output.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputConfig {
    /// Disables the output while retaining its configuration.
    pub off: bool,
    /// Requested display mode or the backend's default when absent.
    pub mode: Option<ModeSpec>,
    /// Fractional scale. `None` selects a scale automatically.
    pub scale: Option<f64>,
    /// Top-left corner in the global layout, in logical pixels.
    pub position: Option<Point>,
    /// Rotation and flip. `None` leaves the output untransformed.
    pub transform: Option<Transform>,
}

impl OutputConfig {
    /// Converts raw output settings into validated configuration.
    ///
    /// Explicit scales are rounded to the 1/120 increments advertised to clients.
    pub(crate) fn from_raw(raw: RawOutput) -> Result<Self, String> {
        let mode = raw.mode.as_deref().map(ModeSpec::parse).transpose()?;
        let scale = match raw.scale {
            None => None,
            Some(RawScale::Name(name)) if name.eq_ignore_ascii_case("auto") => None,
            Some(RawScale::Name(name)) => return Err(format!("unknown scale `{name}`")),
            Some(RawScale::Value(scale)) if (MIN_SCALE..=MAX_SCALE).contains(&scale) => {
                // Fractional scales reach clients in 120ths.
                Some((scale * 120.0).round() / 120.0)
            }
            Some(RawScale::Value(scale)) => {
                return Err(format!(
                    "scale {scale} is outside {MIN_SCALE}..={MAX_SCALE}"
                ));
            }
        };
        let transform = raw
            .transform
            .as_deref()
            .map(|t| Transform::from_name(t).ok_or_else(|| format!("unknown transform `{t}`")))
            .transpose()?;
        Ok(Self {
            off: raw.off,
            mode,
            scale,
            position: raw.position.map(|p| Point::new(p.x, p.y)),
            transform,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn parses_outputs() {
        let config = Config::parse(
            r#"
            [outputs."HDMI-A-1"]
            mode = "2560x1440@143.912"
            scale = 1.5
            position = { x = 1920, y = -100 }

            [outputs."eDP-1"]
            off = true
            mode = "1920x1080"
            scale = 2

            [outputs."DP-1"]
            scale = "auto"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.output("HDMI-A-1", "whatever"),
            Some(&OutputConfig {
                off: false,
                mode: Some(ModeSpec {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: Some(143_912),
                }),
                scale: Some(1.5),
                position: Some((1920, -100).into()),
                transform: None,
            })
        );
        let edp = config.output("eDP-1", "").unwrap();
        assert!(edp.off);
        assert_eq!(edp.scale, Some(2.0));
        assert_eq!(edp.mode.unwrap().refresh_mhz, None);
        assert_eq!(config.output("DP-1", "").unwrap().scale, None);
    }

    #[test]
    fn connector_match_wins_over_description() {
        let config = Config::parse(
            r#"
            [outputs."DEL DELL U2415 ABC"]
            scale = 1.25
            [outputs."DP-2"]
            scale = 1.5
            "#,
        )
        .unwrap();
        assert_eq!(
            config.output("DP-1", "DEL DELL U2415 ABC").unwrap().scale,
            Some(1.25)
        );
        assert_eq!(
            config.output("DP-2", "DEL DELL U2415 ABC").unwrap().scale,
            Some(1.5)
        );
        assert_eq!(config.output("DP-3", "Other"), None);
    }

    #[test]
    fn bad_output_sections_are_skipped() {
        let config = Config::parse(
            r#"
            [outputs."A"]
            mode = "big"
            [outputs."B"]
            scale = 100
            [outputs."C"]
            mode = "1920x1080@0"
            [outputs."D"]
            scale = "huge"
            [outputs."E"]
            scale = 1.0
            "#,
        )
        .unwrap();
        for name in ["A", "B", "C", "D"] {
            assert_eq!(config.output(name, ""), None, "{name}");
        }
        assert!(config.output("E", "").is_some());
        // Unknown keys are a typo worth failing loudly on, like elsewhere.
        assert!(
            Config::parse(
                "[outputs.X]
scael = 2"
            )
            .is_err()
        );
    }

    #[test]
    fn parses_output_transform() {
        let config = Config::parse("[outputs.A]\ntransform = \"flipped-90\"\n").expect("valid");
        assert_eq!(
            config.output("A", "").and_then(|o| o.transform),
            Some(Transform::Flipped90)
        );
        // A bad monitor section is skipped, not the file.
        let bad = Config::parse("[outputs.A]\ntransform = \"sideways\"\n").expect("valid");
        assert!(bad.output("A", "").is_none());
    }
}

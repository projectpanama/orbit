use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawButtons {
    #[serde(default)]
    left: Vec<String>,
    #[serde(default)]
    right: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct RawDecorations {
    height: Option<i64>,
    border: Option<bool>,
    corner_radius: Option<f64>,
    title: Option<bool>,
    font: Option<String>,
    font_size: Option<f64>,
    font_weight: Option<f64>,
    buttons: Option<RawButtons>,
    button_size: Option<i64>,
    button_gap: Option<i64>,
    button_margin: Option<i64>,
    button_radius: Option<f64>,
    button_glyphs: Option<bool>,
    #[serde(default)]
    colors: HashMap<String, String>,
}

const MAX_TITLEBAR_HEIGHT: i64 = 128;

/// Straight (not premultiplied) RGBA color with each component in `0.0..=1.0`.
pub type Rgba = [f32; 4];

/// A title bar button manipulating the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    /// Closes the window.
    Close,
    /// Minimizes the window.
    Minimize,
    /// Maximizes or restores the window.
    Maximize,
}

impl Button {
    /// Converts a configuration button name to a button.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "close" => Self::Close,
            "minimize" => Self::Minimize,
            "maximize" => Self::Maximize,
            _ => return None,
        })
    }
}

/// Colors used to draw a server-side title bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecorationColors {
    /// Background color of the active title bar.
    pub active: Rgba,
    /// Background color of an inactive title bar.
    pub inactive: Rgba,
    /// Border color.
    pub border: Rgba,
    /// Title text color for the active window.
    pub title: Rgba,
    /// Title text color for an inactive window.
    pub title_inactive: Rgba,
    /// Close button color.
    pub close: Rgba,
    /// Minimize button color.
    pub minimize: Rgba,
    /// Maximize button color.
    pub maximize: Rgba,
    /// Button glyph color for the active window.
    pub glyph: Rgba,
    /// Button glyph color for an inactive window.
    pub glyph_inactive: Rgba,
    /// Overlay color when a non-close button is hovered.
    pub button_hover: Rgba,
    /// Close button color while hovered.
    pub close_hover: Rgba,
    /// Close button glyph color while hovered.
    pub close_hover_glyph: Rgba,
}

impl Default for DecorationColors {
    fn default() -> Self {
        let c = |hex: &str| parse_color(hex).expect("invalid default colour");
        Self {
            active: c("#33384a"),
            inactive: c("#24262e"),
            border: c("#00000040"),
            title: c("#ffffffff"),
            title_inactive: c("#ffffff99"),
            close: c("#e3544f"),
            minimize: c("#f2bf40"),
            maximize: c("#5cbf66"),
            glyph: c("#00000099"),
            glyph_inactive: c("#00000066"),
            button_hover: c("#ffffff26"),
            close_hover: c("#e3544f"),
            close_hover_glyph: c("#ffffff"),
        }
    }
}

impl DecorationColors {
    /// Returns the mutable color slot named by a configuration key.
    fn slot(&mut self, name: &str) -> Option<&mut Rgba> {
        Some(match name {
            "active" => &mut self.active,
            "inactive" => &mut self.inactive,
            "border" => &mut self.border,
            "title" => &mut self.title,
            "title-inactive" => &mut self.title_inactive,
            "close" => &mut self.close,
            "minimize" => &mut self.minimize,
            "maximize" => &mut self.maximize,
            "glyph" => &mut self.glyph,
            "glyph-inactive" => &mut self.glyph_inactive,
            "button-hover" => &mut self.button_hover,
            "close-hover" => &mut self.close_hover,
            "close-hover-glyph" => &mut self.close_hover_glyph,
            _ => return None,
        })
    }
}

/// Configuration for title bars on windows using server-side decorations.
///
/// Lengths are logical pixels. The default configuration enables titles and
/// borders, places minimize/maximize/close buttons on the right, and uses a
/// 28-pixel title bar.
#[derive(Debug, Clone, PartialEq)]
pub struct DecorationConfig {
    /// Title bar height.
    pub height: i32,
    /// Radius of the bar's top corners.
    pub corner_radius: f64,
    /// Buttons at the left edge, left to right.
    pub left: Vec<Button>,
    /// Buttons at the right edge, left to right.
    pub right: Vec<Button>,
    /// Button diameter.
    pub button_size: i32,
    /// Horizontal gap between adjacent buttons.
    pub button_gap: i32,
    /// From the bar's edge to the first button.
    pub button_margin: i32,
    /// Button corner radius.
    pub button_radius: f64,
    /// Whether button glyphs are drawn.
    pub button_glyphs: bool,
    /// Whether the window title is drawn.
    pub title: bool,
    /// A fontconfig pattern or a font file.
    pub font: String,
    /// Title font size in logical pixels.
    pub font_size: f64,
    /// CSS-style font weight, applied to variable fonts when supported.
    pub font_weight: Option<f64>,
    /// Whether a border is drawn around the title bar.
    pub border: bool,
    /// Colors used by the title bar and its controls.
    pub colors: DecorationColors,
}

impl Default for DecorationConfig {
    fn default() -> Self {
        Self {
            height: 28,
            corner_radius: 0.0,
            left: Vec::new(),
            right: vec![Button::Minimize, Button::Maximize, Button::Close],
            button_size: 14,
            button_gap: 8,
            button_margin: 8,
            button_radius: 7.0,
            button_glyphs: false,
            title: true,
            font: "sans-serif".into(),
            font_size: 13.0,
            font_weight: None,
            border: true,
            colors: DecorationColors::default(),
        }
    }
}

impl DecorationConfig {
    /// Builds a validated configuration from parsed decoration settings.
    ///
    /// Each invalid value is reported and replaced by its corresponding
    /// default. Unknown fields are rejected by serde before this method runs.
    pub(crate) fn from_raw(raw: RawDecorations) -> Self {
        let mut config = Self::default();
        let int = |name: &str,
                   value: Option<i64>,
                   range: std::ops::RangeInclusive<i64>,
                   default: i32| match value {
            None => default,
            Some(v) if range.contains(&v) => v as i32,
            Some(v) => {
                tracing::warn!("ignoring decorations {name} {v}: not {range:?}");
                default
            }
        };
        let float = |name: &str,
                     value: Option<f64>,
                     range: std::ops::RangeInclusive<f64>,
                     default: f64| match value {
            None => default,
            Some(v) if range.contains(&v) => v,
            Some(v) => {
                tracing::warn!("ignoring decorations {name} {v}: not {range:?}");
                default
            }
        };
        config.height = int("height", raw.height, 1..=MAX_TITLEBAR_HEIGHT, config.height);
        config.button_size = int(
            "button-size",
            raw.button_size,
            0..=MAX_TITLEBAR_HEIGHT,
            config.button_size,
        );
        config.button_gap = int("button-gap", raw.button_gap, 0..=256, config.button_gap);
        config.button_margin = int(
            "button-margin",
            raw.button_margin,
            0..=256,
            config.button_margin,
        );
        config.corner_radius = float(
            "corner-radius",
            raw.corner_radius,
            0.0..=64.0,
            config.corner_radius,
        );
        config.button_radius = float(
            "button-radius",
            raw.button_radius,
            0.0..=64.0,
            config.button_radius,
        );
        config.font_size = float("font-size", raw.font_size, 1.0..=128.0, config.font_size);
        config.font_weight = raw.font_weight.filter(|w| {
            let ok = (1.0..=1000.0).contains(w);
            if !ok {
                tracing::warn!("ignoring decorations font-weight {w}: not 1..=1000");
            }
            ok
        });
        if let Some(font) = raw.font.filter(|f| !f.trim().is_empty()) {
            config.font = font;
        }
        config.button_glyphs = raw.button_glyphs.unwrap_or(config.button_glyphs);
        config.title = raw.title.unwrap_or(config.title);
        config.border = raw.border.unwrap_or(config.border);
        if let Some(buttons) = raw.buttons {
            let mut seen = Vec::new();
            let mut side = |names: Vec<String>| -> Vec<Button> {
                names
                    .into_iter()
                    .filter_map(|name| match Button::from_name(&name) {
                        Some(b) if !seen.contains(&b) => {
                            seen.push(b);
                            Some(b)
                        }
                        Some(_) => {
                            tracing::warn!("ignoring decorations button `{name}`: listed twice");
                            None
                        }
                        None => {
                            tracing::warn!("ignoring decorations button `{name}`: not close, minimize or maximize");
                            None
                        }
                    })
                    .collect()
            };
            config.left = side(buttons.left);
            config.right = side(buttons.right);
        }
        for (name, value) in raw.colors {
            match (config.colors.slot(&name), parse_color(&value)) {
                (Some(slot), Some(color)) => *slot = color,
                (None, _) => tracing::warn!("ignoring decorations colour `{name}`: unknown"),
                (Some(_), None) => tracing::warn!(
                    "ignoring decorations colour {name} = `{value}`: not #rrggbb or #rrggbbaa"
                ),
            }
        }
        config
    }
}

/// Parses a `#rrggbb` or `#rrggbbaa` color into normalized RGBA components.
fn parse_color(text: &str) -> Option<Rgba> {
    let hex = text.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let a = if hex.len() == 8 { byte(6)? } else { 255 };
    Some([byte(0)?, byte(2)?, byte(4)?, a].map(|c| f32::from(c) / 255.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn decoration_defaults() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.decorations, DecorationConfig::default());
        assert_eq!(config.decorations.height, 28);
        assert_eq!(
            config.decorations.right,
            vec![Button::Minimize, Button::Maximize, Button::Close]
        );
        assert_eq!(
            config.decorations.colors.close,
            [
                0xe3 as f32 / 255.0,
                0x54 as f32 / 255.0,
                0x4f as f32 / 255.0,
                1.0
            ]
        );
    }

    #[test]
    fn parses_decorations() {
        let config = Config::parse(
            r##"
            [decorations]
            height = 36
            corner-radius = 12
            buttons = { left = ["close", "minimize", "maximize"] }
            button-size = 20
            button-glyphs = true
            font = "Inter Variable"
            font-weight = 600
            [decorations.colors]
            active = "#ffffff"
            border = "#00000014"
            "##,
        )
        .unwrap();
        let d = &config.decorations;
        assert_eq!(d.height, 36);
        assert_eq!(d.corner_radius, 12.0);
        assert_eq!(
            d.left,
            vec![Button::Close, Button::Minimize, Button::Maximize]
        );
        assert!(d.right.is_empty());
        assert_eq!(d.button_size, 20);
        assert!(d.button_glyphs);
        assert_eq!(d.font, "Inter Variable");
        assert_eq!(d.font_weight, Some(600.0));
        assert_eq!(d.colors.active, [1.0; 4]);
        assert_eq!(d.colors.border[3], 0x14 as f32 / 255.0);
        // Untouched values keep their defaults.
        assert_eq!(d.button_gap, DecorationConfig::default().button_gap);
        assert_eq!(d.colors.inactive, DecorationColors::default().inactive);
    }

    #[test]
    fn bad_decoration_values_fall_back_to_defaults() {
        let config = Config::parse(
            r##"
            [decorations]
            height = 0
            button-size = -3
            corner-radius = 1000
            buttons = { left = ["close", "close", "shade"], right = ["maximize"] }
            font = " "
            [decorations.colors]
            active = "blue"
            title = "#12345"
            sparkle = "#ffffff"
            "##,
        )
        .unwrap();
        let d = &config.decorations;
        let defaults = DecorationConfig::default();
        assert_eq!(d.height, defaults.height);
        assert_eq!(d.button_size, defaults.button_size);
        assert_eq!(d.corner_radius, defaults.corner_radius);
        assert_eq!(d.left, vec![Button::Close]);
        assert_eq!(d.right, vec![Button::Maximize]);
        assert_eq!(d.font, defaults.font);
        assert_eq!(d.colors, defaults.colors);
        // Unknown keys are an error, as elsewhere.
        assert!(Config::parse("[decorations]\nshadow = true").is_err());
    }
}

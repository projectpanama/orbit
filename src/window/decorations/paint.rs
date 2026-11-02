use ab_glyph::{Font, FontArc, ScaleFont, point};
use orbit_config::{Button, Rgba};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Size, Transform},
};
use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke};

use super::{hit::button_rects, *};

/// Button glyph stroke width in logical pixels.
const GLYPH_STROKE: f32 = 1.5;
/// Minimum space between the title and the button groups, in logical pixels.
const TITLE_PADDING: i32 = 12;

/// Inputs that determine a rendered title bar.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BarKey {
    /// Frame width in logical pixels.
    pub(super) width: i32,
    /// Bit representation of the output scale.
    pub(super) scale: u64,
    /// Whether the window is active.
    pub(super) active: bool,
    /// Button currently under the pointer, if any.
    pub(super) hovered: Option<Button>,
    /// Window title to render.
    pub(super) title: String,
}

/// Converts a logical bar size to a physical pixel size.
///
/// Each dimension is rounded after scaling and clamped to at least one pixel.
pub(super) fn bar_pixels(size: Size<i32, Logical>, scale: f64) -> (u32, u32) {
    let px = |v: i32| (f64::from(v) * scale).round().max(1.0) as u32;
    (px(size.w), px(size.h))
}

/// Renders a title bar into a Smithay memory buffer.
///
/// Returns `None` when the backing pixmap cannot be allocated.
pub(super) fn draw_bar(
    style: &DecorationStyle,
    key: &BarKey,
    scale: f64,
) -> Option<MemoryRenderBuffer> {
    let pixmap = draw_bar_pixmap(style, key, scale)?;
    Some(MemoryRenderBuffer::from_slice(
        pixmap.data(),
        // tiny-skia stores premultiplied RGBA bytes: DRM's ABGR8888.
        Fourcc::Abgr8888,
        (pixmap.width() as i32, pixmap.height() as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// Shortens `text` in the middle with an ellipsis until it fits `max_width`.
///
/// Characters are retained from both ends as evenly as possible. If even an
/// ellipsis cannot fit, the returned string is empty.
pub fn truncate_middle(text: &str, max_width: f32, width: impl Fn(&[char]) -> f32) -> String {
    let chars: Vec<char> = text.chars().collect();
    if width(&chars) <= max_width {
        return text.to_string();
    }
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let head = keep.div_ceil(2);
        let tail = keep / 2;
        let candidate: Vec<char> = chars[..head]
            .iter()
            .chain(std::iter::once(&'…'))
            .chain(&chars[chars.len() - tail..])
            .copied()
            .collect();
        if width(&candidate) <= max_width {
            return candidate.into_iter().collect();
        }
    }
    String::new()
}

/// Creates an anti-aliased `tiny-skia` paint from normalized RGBA.
fn paint(color: Rgba) -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    let [r, g, b, a] = color.map(|c| c.clamp(0.0, 1.0));
    paint.set_color_rgba8(
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    );
    paint
}

/// Builds a rectangle with independently rounded top and bottom corners.
///
/// Radii are clamped to half the rectangle's smallest dimension.
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, top: f32, bottom: f32) -> Option<tiny_skia::Path> {
    // Control point distance for a quarter circle.
    const K: f32 = 0.552_284_8;
    let top = top.min(w / 2.0).min(h / 2.0).max(0.0);
    let bottom = bottom.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + top, y);
    pb.line_to(x + w - top, y);
    pb.cubic_to(
        x + w - top * (1.0 - K),
        y,
        x + w,
        y + top * (1.0 - K),
        x + w,
        y + top,
    );
    pb.line_to(x + w, y + h - bottom);
    pb.cubic_to(
        x + w,
        y + h - bottom * (1.0 - K),
        x + w - bottom * (1.0 - K),
        y + h,
        x + w - bottom,
        y + h,
    );
    pb.line_to(x + bottom, y + h);
    pb.cubic_to(
        x + bottom * (1.0 - K),
        y + h,
        x,
        y + h - bottom * (1.0 - K),
        x,
        y + h - bottom,
    );
    pb.line_to(x, y + top);
    pb.cubic_to(x, y + top * (1.0 - K), x + top * (1.0 - K), y, x + top, y);
    pb.close();
    pb.finish()
}

/// Renders a title bar into a premultiplied `tiny-skia` pixmap.
fn draw_bar_pixmap(style: &DecorationStyle, key: &BarKey, scale: f64) -> Option<Pixmap> {
    let config = &style.config;
    let colors = &config.colors;
    let (pw, ph) = bar_pixels((key.width, config.height).into(), scale);
    let mut pixmap = Pixmap::new(pw, ph)?;
    let s = scale as f32;
    let identity = tiny_skia::Transform::identity();

    let background = if key.active {
        colors.active
    } else {
        colors.inactive
    };

    let radius = config.corner_radius as f32 * s;
    if let Some(path) = rounded_rect(0.0, 0.0, pw as f32, ph as f32, radius, 0.0) {
        pixmap.fill_path(&path, &paint(background), FillRule::Winding, identity, None);
    }

    if config.border {
        let line = s.round().max(1.0);
        if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, ph as f32 - line, pw as f32, line) {
            pixmap.fill_rect(rect, &paint(colors.border), identity, None);
        }
    }

    for (button, rect) in button_rects(config, key.width) {
        let (x, y) = (rect.loc.x as f32 * s, rect.loc.y as f32 * s);
        let size = rect.size.w as f32 * s;
        let r = config.button_radius as f32 * s;
        let Some(shape) = rounded_rect(x, y, size, size, r, r) else {
            continue;
        };
        let hovered = key.hovered == Some(button);
        let fill = match button {
            Button::Close if hovered => colors.close_hover,
            Button::Close => colors.close,
            Button::Minimize => colors.minimize,
            Button::Maximize => colors.maximize,
        };
        pixmap.fill_path(&shape, &paint(fill), FillRule::Winding, identity, None);
        if hovered && button != Button::Close {
            pixmap.fill_path(
                &shape,
                &paint(colors.button_hover),
                FillRule::Winding,
                identity,
                None,
            );
        }
        if config.button_glyphs {
            let color = match (button, hovered, key.active) {
                (Button::Close, true, _) => colors.close_hover_glyph,
                (_, _, true) => colors.glyph,
                _ => colors.glyph_inactive,
            };
            draw_glyph(&mut pixmap, button, x, y, size, s, color);
        }
    }

    if let Some(font) = &style.font
        && !key.title.is_empty()
    {
        // The title is centred on the bar, kept clear of the buttons on both sides.
        let extent = |buttons: &[Button]| {
            if buttons.is_empty() {
                0
            } else {
                config.button_margin
                    + buttons.len() as i32 * (config.button_size + config.button_gap)
                    - config.button_gap
            }
        };
        let side = extent(&config.left).max(extent(&config.right)) + TITLE_PADDING;
        let available = (key.width - 2 * side) as f32 * s;
        let color = if key.active {
            colors.title
        } else {
            colors.title_inactive
        };
        draw_title(
            &mut pixmap,
            font,
            &key.title,
            config.font_size as f32 * s,
            available,
            color,
        );
    }

    Some(pixmap)
}

/// Draws the close, minimize, or maximize glyph inside a button.
///
/// `x`, `y`, and `size` are physical-pixel coordinates. `s` converts the
/// logical glyph stroke width to physical pixels.
fn draw_glyph(pixmap: &mut Pixmap, button: Button, x: f32, y: f32, size: f32, s: f32, color: Rgba) {
    let (lo, hi, mid) = (size * 0.3, size * 0.7, size * 0.5);
    let mut pb = PathBuilder::new();
    match button {
        Button::Close => {
            pb.move_to(x + lo, y + lo);
            pb.line_to(x + hi, y + hi);
            pb.move_to(x + hi, y + lo);
            pb.line_to(x + lo, y + hi);
        }
        Button::Minimize => {
            pb.move_to(x + lo, y + mid);
            pb.line_to(x + hi, y + mid);
        }
        Button::Maximize => {
            pb.move_to(x + lo, y + mid);
            pb.line_to(x + hi, y + mid);
            pb.move_to(x + mid, y + lo);
            pb.line_to(x + mid, y + hi);
        }
    }
    let Some(path) = pb.finish() else {
        return;
    };
    let stroke = Stroke {
        width: GLYPH_STROKE * s,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(
        &path,
        &paint(color),
        &stroke,
        tiny_skia::Transform::identity(),
        None,
    );
}

/// Returns horizontal advances for `text`, including pairwise kerning.
fn advances<F: Font>(font: &impl ScaleFont<F>, text: &[char]) -> Vec<f32> {
    let mut prev = None;
    text.iter()
        .map(|&c| {
            let id = font.glyph_id(c);
            let kern = prev.map_or(0.0, |p| font.kern(p, id));
            prev = Some(id);
            kern + font.h_advance(id)
        })
        .collect()
}

/// Draws a centered, optionally truncated title into `pixmap`.
///
/// The title is vertically centered using the font's ascent and descent.
/// `max_width` and `px` are physical-pixel units.
fn draw_title(
    pixmap: &mut Pixmap,
    font: &FontArc,
    title: &str,
    px: f32,
    max_width: f32,
    color: Rgba,
) {
    if max_width <= 0.0 {
        return;
    }

    let Some(px_scale) = font.pt_to_px_scale(px) else {
        return;
    };

    let scaled = font.as_scaled(px_scale);
    let measure = |chars: &[char]| advances(&scaled, chars).iter().sum::<f32>();
    let text: Vec<char> = truncate_middle(title, max_width, measure).chars().collect();
    let width = measure(&text);
    let (pw, ph) = (pixmap.width() as f32, pixmap.height() as f32);

    let mut x = ((pw - width) / 2.0).round();
    let baseline = ((ph - (scaled.ascent() - scaled.descent())) / 2.0 + scaled.ascent()).round();
    let [r, g, b, a] = color.map(|c| c.clamp(0.0, 1.0));
    let (stride, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let pixels = pixmap.pixels_mut();

    for (&c, advance) in text.iter().zip(advances(&scaled, &text)) {
        let id = scaled.glyph_id(c);
        // Advances include kerning before the glyph.
        let kern = advance - scaled.h_advance(id);
        x += kern;
        let glyph = id.with_scale_and_position(px_scale, point(x, baseline));
        x += scaled.h_advance(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if !(0..stride).contains(&px) || !(0..height).contains(&py) {
                return;
            }
            let pixel = &mut pixels[(py * stride + px) as usize];
            let src_a = a * coverage.clamp(0.0, 1.0);
            let keep = 1.0 - src_a;
            let blend =
                |src: f32, dst: u8| (src * src_a * 255.0 + f32::from(dst) * keep).round() as u8;
            let out_a = blend(1.0, pixel.alpha());
            if let Some(out) = PremultipliedColorU8::from_rgba(
                blend(r, pixel.red()).min(out_a),
                blend(g, pixel.green()).min(out_a),
                blend(b, pixel.blue()).min(out_a),
                out_a,
            ) {
                *pixel = out;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_shorten_in_the_middle() {
        let width = |chars: &[char]| chars.len() as f32;
        assert_eq!(truncate_middle("short", 10.0, width), "short");
        assert_eq!(truncate_middle("abcdefghij", 5.0, width), "ab…ij");
        assert_eq!(truncate_middle("abcdefghij", 4.0, width), "ab…j");
        assert_eq!(truncate_middle("abc", 0.0, width), "");
    }

    #[test]
    fn bars_draw_at_physical_size() {
        let style = DecorationStyle::default();
        let key = BarKey {
            width: 300,
            scale: 1.5f64.to_bits(),
            active: true,
            hovered: Some(Button::Close),
            title: "Title".into(),
        };
        assert!(draw_bar(&style, &key, 1.5).is_some());
        assert_eq!(bar_pixels((300, 28).into(), 1.5), (450, 42));
    }

    #[test]
    fn bars_show_corners_hover_and_focus() {
        let style = DecorationStyle {
            config: DecorationConfig {
                corner_radius: 8.0,
                left: vec![Button::Close],
                right: vec![],
                ..DecorationConfig::default()
            },
            font: None,
        };
        let key = BarKey {
            width: 100,
            scale: 1f64.to_bits(),
            active: true,
            hovered: None,
            title: String::new(),
        };
        let pixel = |key: &BarKey, x: u32, y: u32| {
            let p = draw_bar_pixmap(&style, key, 1.0)
                .unwrap()
                .pixel(x, y)
                .unwrap();
            [p.red(), p.green(), p.blue(), p.alpha()]
        };
        let to_u8 = |c: Rgba| c.map(|v| (v * 255.0).round() as u8);
        let colors = style.config.colors;
        // The top corners are cut, the middle is the active colour.
        assert_eq!(pixel(&key, 0, 0)[3], 0);
        assert_eq!(pixel(&key, 50, 10), to_u8(colors.active));
        // The close button (8 + 7 = its centre) turns to its hover colour.
        let centre = (8 + 7, 14);
        assert_eq!(pixel(&key, centre.0, centre.1), to_u8(colors.close));
        let hovered = BarKey {
            hovered: Some(Button::Close),
            ..key.clone()
        };
        assert_eq!(
            pixel(&hovered, centre.0, centre.1),
            to_u8(colors.close_hover)
        );
        let inactive = BarKey {
            active: false,
            ..key.clone()
        };
        assert_eq!(pixel(&inactive, 50, 10), to_u8(colors.inactive));
    }
}

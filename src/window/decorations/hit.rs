use orbit_config::{Button, DecorationConfig};
use smithay::utils::{Logical, Point, Rectangle};

use crate::window::grabs::Edges;

/// Width of the invisible resize band around a decorated frame, in logical pixels.
const RESIZE_BORDER: i32 = 6;

/// The decoration region currently under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationHit {
    Titlebar,
    Close,
    Maximize,
    Minimize,
    Edge(Edges),
}

impl DecorationHit {
    /// Returns the window-control button represented by this hit.
    ///
    /// Returns `None` for the title bar and resize edges.
    pub fn button(self) -> Option<Button> {
        match self {
            Self::Close => Some(Button::Close),
            Self::Minimize => Some(Button::Minimize),
            Self::Maximize => Some(Button::Maximize),
            Self::Titlebar | Self::Edge(_) => None,
        }
    }

    /// Converts a configured button into its corresponding hit result.
    fn from_button(button: Button) -> Self {
        match button {
            Button::Close => Self::Close,
            Button::Minimize => Self::Minimize,
            Button::Maximize => Self::Maximize,
        }
    }
}

/// Returns configured button rectangles relative to the frame's top-left corner.
///
/// Buttons are returned in left-side order followed by right-side order.
pub(super) fn button_rects(
    config: &DecorationConfig,
    frame_width: i32,
) -> Vec<(Button, Rectangle<i32, Logical>)> {
    let size = config.button_size;
    let y = (config.height - size) / 2;
    let step = size + config.button_gap;
    let rect = |x: i32| Rectangle::new((x, y).into(), (size, size).into());
    let left = config
        .left
        .iter()
        .enumerate()
        .map(|(i, &b)| (b, rect(config.button_margin + step * i as i32)));
    let n = config.right.len() as i32;
    let right = config.right.iter().enumerate().map(move |(i, &b)| {
        let from_right = n - i as i32;
        (
            b,
            rect(frame_width - config.button_margin - step * from_right + config.button_gap),
        )
    });
    left.chain(right).collect()
}

/// Hit-tests `pos` against a decorated window frame.
pub fn hit_test(
    config: &DecorationConfig,
    frame: Rectangle<i32, Logical>,
    pos: Point<f64, Logical>,
) -> Option<DecorationHit> {
    let mut grown = frame;
    grown.loc -= Point::from((RESIZE_BORDER, RESIZE_BORDER));
    grown.size += (RESIZE_BORDER * 2, RESIZE_BORDER * 2).into();

    if !grown.to_f64().contains(pos) {
        return None;
    }

    if !frame.to_f64().contains(pos) {
        let f = frame.to_f64();
        let mut edges = Edges::empty();
        edges.set(Edges::TOP, pos.y < f.loc.y);
        edges.set(Edges::BOTTOM, pos.y >= f.loc.y + f.size.h);
        edges.set(Edges::LEFT, pos.x < f.loc.x);
        edges.set(Edges::RIGHT, pos.x >= f.loc.x + f.size.w);
        return Some(DecorationHit::Edge(edges));
    }

    let local = pos - frame.loc.to_f64();
    if local.y >= f64::from(config.height) {
        return None;
    }

    let hit = button_rects(config, frame.size.w)
        .into_iter()
        .find(|(_, rect)| rect.to_f64().contains(local))
        .map_or(DecorationHit::Titlebar, |(button, _)| {
            DecorationHit::from_button(button)
        });
    Some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new((100, 100).into(), (400, 300).into())
    }

    #[test]
    fn hits_buttons_bar_and_edges() {
        let config = DecorationConfig::default();
        let f = frame();
        let at = |x: f64, y: f64| hit_test(&config, f, (x, y).into());
        let step = (config.button_gap + config.button_size) as f64;
        // Close is the rightmost button.
        let close_x = 100.0 + 400.0 - config.button_margin as f64 - config.button_size as f64 + 2.0;
        let button_y = 100.0 + ((config.height - config.button_size) / 2) as f64 + 2.0;
        assert_eq!(at(close_x, button_y), Some(DecorationHit::Close));
        assert_eq!(at(close_x - step, button_y), Some(DecorationHit::Maximize));
        assert_eq!(
            at(close_x - 2.0 * step, button_y),
            Some(DecorationHit::Minimize)
        );
        assert_eq!(at(150.0, 110.0), Some(DecorationHit::Titlebar));
        // Inside the content area: the client's, not ours.
        assert_eq!(at(150.0, 200.0), None);
        assert_eq!(
            at(97.0, 97.0),
            Some(DecorationHit::Edge(Edges::TOP | Edges::LEFT))
        );
        assert_eq!(at(50.0, 50.0), None);
    }

    #[test]
    fn buttons_follow_the_configured_side_and_height() {
        let config = DecorationConfig {
            height: 36,
            left: vec![Button::Close, Button::Minimize, Button::Maximize],
            right: Vec::new(),
            button_size: 20,
            button_gap: 4,
            button_margin: 10,
            ..DecorationConfig::default()
        };
        let f = frame();
        let at = |x: f64, y: f64| hit_test(&config, f, (x, y).into());
        // Buttons are 20 tall, centred in 36: y 108..128.
        let y = 100.0 + 8.0 + 1.0;
        assert_eq!(at(100.0 + 10.0 + 1.0, y), Some(DecorationHit::Close));
        assert_eq!(at(100.0 + 34.0 + 1.0, y), Some(DecorationHit::Minimize));
        assert_eq!(at(100.0 + 58.0 + 1.0, y), Some(DecorationHit::Maximize));
        // The gap between buttons, and the right side, are title bar.
        assert_eq!(at(100.0 + 31.0, y), Some(DecorationHit::Titlebar));
        assert_eq!(at(490.0, y), Some(DecorationHit::Titlebar));
        // 35 is still bar, 36 is the client's.
        assert_eq!(at(300.0, 135.0), Some(DecorationHit::Titlebar));
        assert_eq!(at(300.0, 136.0), None);
    }
}

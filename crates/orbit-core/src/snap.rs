use crate::{
    layout::geometry::{Point, Rect},
    layout::{Layout, Side},
};

/// Maximum edge distance for magnetic snapping, in logical pixels.
const MAGNET_DISTANCE: i32 = 16;
/// Maximum pointer distance from an output edge to activate a drop zone.
const ZONE_DISTANCE: f64 = 8.0;

/// Shortest eligible edge movement found so far.
#[derive(Default)]
struct Pull(Option<i32>);

impl Pull {
    /// Records the shortest eligible movement for either edge pair.
    fn towards(&mut self, pairs: [(i32, i32); 2]) {
        for (edge, target) in pairs {
            let delta = target - edge;
            if delta.abs() <= MAGNET_DISTANCE && self.0.is_none_or(|best| delta.abs() < best.abs())
            {
                self.0 = Some(delta);
            }
        }
    }
}

/// Returns the offset that snaps `frame` to nearby output or window edges.
///
/// Horizontal and vertical axes are considered independently. Windows attract
/// only when they overlap, within the magnet distance, on the other axis.
/// Returns zero for an axis with no eligible target.
pub fn magnetize(frame: Rect, output: Rect, others: impl IntoIterator<Item = Rect>) -> Point {
    let (left, right) = (frame.loc.x, frame.right());
    let (top, bottom) = (frame.loc.y, frame.bottom());

    // The shortest pull within reach on each axis; the first one found wins a tie.
    let mut x = Pull::default();
    let mut y = Pull::default();
    x.towards([(left, output.loc.x), (right, output.right())]);
    y.towards([(top, output.loc.y), (bottom, output.bottom())]);

    for other in others {
        let (o_left, o_right) = (other.loc.x, other.right());
        let (o_top, o_bottom) = (other.loc.y, other.bottom());
        // Only windows that are side by side (overlapping on the other axis,
        // give or take the magnet distance) attract each other.
        let overlaps_vertically =
            top < o_bottom + MAGNET_DISTANCE && o_top < bottom + MAGNET_DISTANCE;
        let overlaps_horizontally =
            left < o_right + MAGNET_DISTANCE && o_left < right + MAGNET_DISTANCE;
        if overlaps_vertically {
            for edge in [left, right] {
                x.towards([(edge, o_left), (edge, o_right)]);
            }
        }
        if overlaps_horizontally {
            for edge in [top, bottom] {
                y.towards([(edge, o_top), (edge, o_bottom)]);
            }
        }
    }
    Point::new(x.0.unwrap_or(0), y.0.unwrap_or(0))
}

/// Returns the layout activated by dropping at `pointer` on `output`.
///
/// The top edge maximizes; the left and right edges tile to the matching
/// half. Points outside the output or away from these zones return `None`.
pub fn drop_zone(pointer: Point<f64>, output: Rect) -> Option<Layout> {
    let output = output.to_f64();
    if !output.contains(pointer) {
        return None;
    }
    if pointer.y - output.loc.y <= ZONE_DISTANCE {
        Some(Layout::Maximized)
    } else if pointer.x - output.loc.x <= ZONE_DISTANCE {
        Some(Layout::Tiled(Side::Left))
    } else if output.loc.x + output.size.w - pointer.x <= ZONE_DISTANCE + 1.0 {
        Some(Layout::Tiled(Side::Right))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y), (w, h))
    }

    fn output() -> Rect {
        rect(0, 0, 1920, 1080)
    }

    #[test]
    fn sticks_to_output_edges() {
        assert_eq!(
            magnetize(rect(10, 500, 400, 300), output(), []),
            (-10, 0).into()
        );
        assert_eq!(
            magnetize(rect(1510, 790, 400, 300), output(), []),
            (10, -10).into()
        );
        assert_eq!(
            magnetize(rect(100, 100, 400, 300), output(), []),
            (0, 0).into()
        );
    }

    #[test]
    fn sticks_to_neighbours_side_by_side_only() {
        let other = rect(600, 200, 400, 300);
        // Our right edge (590) is 10px from their left edge, our top 10px below theirs.
        assert_eq!(
            magnetize(rect(190, 210, 400, 300), output(), [other]),
            (10, -10).into()
        );
        // Same x, but far below. No horizontal pull.
        assert_eq!(
            magnetize(rect(190, 700, 400, 300), output(), [other]),
            (0, 0).into()
        );
    }

    #[test]
    fn drop_zones() {
        assert_eq!(
            drop_zone((900.0, 2.0).into(), output()),
            Some(Layout::Maximized)
        );
        assert_eq!(
            drop_zone((3.0, 500.0).into(), output()),
            Some(Layout::Tiled(Side::Left))
        );
        assert_eq!(
            drop_zone((1915.0, 500.0).into(), output()),
            Some(Layout::Tiled(Side::Right))
        );
        assert_eq!(drop_zone((900.0, 500.0).into(), output()), None);
    }
}

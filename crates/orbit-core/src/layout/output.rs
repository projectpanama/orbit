use super::geometry::{Point, Rect, Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
}

/// Requested geometry for an output slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Output dimensions.
    pub size: Size,
    /// Optional configured position.
    pub configured: Option<Point>,
}

/// Resulting position and placement status for an output slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arranged {
    /// Position assigned to the output.
    pub position: Point,
    /// Whether its configured position overlapped an earlier configured slot.
    pub overlapped: bool,
}

/// Arranges output slots, honoring valid configured positions first.
///
/// Slots without positions, and slots whose configured positions overlap an
/// earlier configured slot, are placed left-to-right after the placed slots.
pub fn arrange(slots: &[Slot]) -> Vec<Arranged> {
    let mut placed: Vec<Option<Rect>> = vec![None; slots.len()];
    let mut overlapped = vec![false; slots.len()];

    for (index, slot) in slots.iter().enumerate() {
        let Some(position) = slot.configured else {
            continue;
        };
        let rect = Rect::new(position, slot.size);
        if placed.iter().flatten().any(|other| other.overlaps(rect)) {
            overlapped[index] = true;
            continue;
        }
        placed[index] = Some(rect);
    }

    for (index, slot) in slots.iter().enumerate() {
        if placed[index].is_some() {
            continue;
        }
        let x = placed
            .iter()
            .flatten()
            .map(|rect| rect.right())
            .max()
            .unwrap_or(0);
        placed[index] = Some(Rect::new((x, 0), slot.size));
    }

    placed
        .into_iter()
        .zip(overlapped)
        .map(|(rect, overlapped)| Arranged {
            position: rect.map(|rect| rect.loc).unwrap_or_default(),
            overlapped,
        })
        .collect()
}

/// Constrains pointer movement to the union of the available outputs.
///
/// Movement into an output is accepted unchanged. Movement into a gap or
/// beyond an edge is clamped to the output containing the old pointer, or to
/// the first output when the old pointer is already outside all outputs.
pub fn constrain_pointer<I: Iterator<Item = Rect>>(
    old: Point<f64>,
    new: Point<f64>,
    outputs: impl Fn() -> I,
) -> Point<f64> {
    if outputs().any(|rect| rect.to_f64().contains(new)) {
        return new;
    }

    let Some(current) = outputs()
        .find(|rect| rect.to_f64().contains(old))
        .or_else(|| outputs().next())
    else {
        return new;
    };

    let min = current.loc.to_f64();
    let size = current.size.to_f64();
    let max = Point::new(min.x + size.w - 1.0, min.y + size.h - 1.0);
    Point::new(new.x.clamp(min.x, max.x), new.y.clamp(min.y, max.y))
}

/// Finds the nearest output in the requested horizontal direction.
///
/// The returned value is an index into `outputs`. Returns `None` when `from`
/// is invalid or no output lies in that direction.
pub fn neighbor(outputs: &[Rect], from: usize, direction: Direction) -> Option<usize> {
    let center = |rect: &Rect| {
        (
            i64::from(rect.loc.x) * 2 + i64::from(rect.size.w),
            i64::from(rect.loc.y) * 2 + i64::from(rect.size.h),
        )
    };
    let (x, y) = center(outputs.get(from)?);
    outputs
        .iter()
        .enumerate()
        .filter(|&(index, _)| index != from)
        .filter_map(|(index, rect)| {
            let (cx, cy) = center(rect);
            let dx = match direction {
                Direction::Left => x - cx,
                Direction::Right => cx - x,
            };
            (dx > 0).then_some((index, (dx, (cy - y).abs())))
        })
        .min_by_key(|&(_, distance)| distance)
        .map(|(index, _)| index)
}

/// Fits a frame inside `to`, preserving its relative position from `from`.
///
/// When no source output is provided, the frame's existing position is used.
/// The returned position is clamped so the entire frame remains inside `to`.
pub fn fit_rect(frame: Rect, from: Option<Rect>, to: Rect) -> Point {
    let loc = match from {
        Some(from) => {
            let scale = |offset: i32, from_len: i32, to_len: i32| {
                (f64::from(offset) * f64::from(to_len) / f64::from(from_len.max(1))).round() as i32
            };
            let offset = frame.loc - from.loc;
            to.loc
                + Point::new(
                    scale(offset.x, from.size.w, to.size.w),
                    scale(offset.y, from.size.h, to.size.h),
                )
        }
        None => frame.loc,
    };
    let clamp = |value: i32, start: i32, len: i32, frame_len: i32| {
        value.clamp(start, start + (len - frame_len).max(0))
    };
    Point::new(
        clamp(loc.x, to.loc.x, to.size.w, frame.size.w),
        clamp(loc.y, to.loc.y, to.size.h, frame.size.h),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y), (w, h))
    }

    fn positions(slots: &[Slot]) -> Vec<Point> {
        arrange(slots).iter().map(|a| a.position).collect()
    }

    fn slot(w: i32, h: i32, configured: Option<(i32, i32)>) -> Slot {
        Slot {
            size: (w, h).into(),
            configured: configured.map(Point::from),
        }
    }

    #[test]
    fn auto_outputs_line_up_left_to_right() {
        let positions = positions(&[slot(1920, 1080, None), slot(2560, 1440, None)]);
        assert_eq!(positions, vec![(0, 0).into(), (1920, 0).into()]);
    }

    #[test]
    fn configured_positions_win_and_auto_goes_after() {
        let positions = positions(&[
            slot(1920, 1080, None),
            slot(2560, 1440, Some((-2560, -200))),
            slot(1280, 1024, Some((0, 0))),
        ]);
        assert_eq!(
            positions,
            vec![(1280, 0).into(), (-2560, -200).into(), (0, 0).into()]
        );
    }

    #[test]
    fn overlapping_configured_position_falls_back_to_auto() {
        let positions = positions(&[
            slot(1920, 1080, Some((0, 0))),
            slot(1920, 1080, Some((100, 100))),
        ]);
        assert_eq!(positions, vec![(0, 0).into(), (1920, 0).into()]);
        let overlapped = arrange(&[
            slot(1920, 1080, Some((0, 0))),
            slot(1920, 1080, Some((100, 100))),
        ])
        .iter()
        .map(|a| a.overlapped)
        .collect::<Vec<_>>();
        assert_eq!(overlapped, [false, true]);
    }

    #[test]
    fn touching_outputs_do_not_overlap() {
        let positions = positions(&[
            slot(1920, 1080, Some((0, 0))),
            slot(1920, 1080, Some((1920, 0))),
        ]);
        assert_eq!(positions, vec![(0, 0).into(), (1920, 0).into()]);
    }

    #[test]
    fn pointer_crosses_shared_edges() {
        let outputs = [rect(0, 0, 1920, 1080), rect(1920, 0, 1280, 1024)];
        let new = constrain_pointer((1915.0, 500.0).into(), (1925.0, 500.0).into(), || {
            outputs.iter().copied()
        });
        assert_eq!(new, (1925.0, 500.0).into());
    }

    #[test]
    fn pointer_stops_at_gaps_and_outer_edges() {
        let outputs = [rect(0, 0, 1920, 1080), rect(1920, 0, 1280, 1024)];
        // Below the shorter output there is nothing.
        let new = constrain_pointer((2000.0, 1020.0).into(), (2000.0, 1050.0).into(), || {
            outputs.iter().copied()
        });
        assert_eq!(new, (2000.0, 1023.0).into());
        let new = constrain_pointer((10.0, 10.0).into(), (-50.0, -5.0).into(), || {
            outputs.iter().copied()
        });
        assert_eq!(new, (0.0, 0.0).into());
    }

    #[test]
    fn stranded_pointer_is_pulled_onto_an_output() {
        let outputs = [rect(0, 0, 1920, 1080)];
        let new = constrain_pointer((3000.0, 500.0).into(), (3001.0, 500.0).into(), || {
            outputs.iter().copied()
        });
        assert_eq!(new, (1919.0, 500.0).into());
    }

    #[test]
    fn finds_neighbours() {
        let outputs = [
            rect(0, 0, 1920, 1080),
            rect(1920, 0, 2560, 1440),
            rect(-1280, 200, 1280, 1024),
            rect(4480, 0, 1920, 1080),
        ];
        assert_eq!(neighbor(&outputs, 0, Direction::Right), Some(1));
        assert_eq!(neighbor(&outputs, 0, Direction::Left), Some(2));
        assert_eq!(neighbor(&outputs, 1, Direction::Right), Some(3));
        assert_eq!(neighbor(&outputs, 2, Direction::Left), None);
        assert_eq!(neighbor(&outputs, 3, Direction::Right), None);
        assert_eq!(neighbor(&outputs, 9, Direction::Right), None);
    }

    #[test]
    fn fit_keeps_the_relative_position() {
        let from = rect(1920, 0, 2560, 1440);
        let to = rect(0, 0, 1280, 720);
        let loc = fit_rect(rect(1920 + 640, 360, 400, 300), Some(from), to);
        assert_eq!(loc, (320, 180).into());
    }

    #[test]
    fn fit_keeps_the_frame_on_the_output() {
        let to = rect(0, 0, 1920, 1080);
        assert_eq!(
            fit_rect(rect(1800, 1000, 400, 300), None, to),
            (1520, 780).into()
        );
        assert_eq!(
            fit_rect(rect(-500, -500, 3000, 2000), None, to),
            (0, 0).into()
        );
    }
}

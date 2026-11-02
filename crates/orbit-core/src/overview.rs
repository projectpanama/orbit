use crate::{
    ids::WindowId,
    layout::geometry::{Point, Rect, Size},
};

/// Maximum fraction of a grid cell occupied by a window.
const CELL_FILL: f64 = 0.9;

/// Places windows in a centered grid, preserving aspect ratios without
/// enlarging them. Windows are ordered by position, and the column count that
/// maximizes displayed area is selected. An incomplete final row is centered.
///
/// Returns no rectangles for empty windows or a non-positive area.
pub fn layout(windows: &[(WindowId, Rect)], area: Rect) -> Vec<(WindowId, Rect)> {
    let n = windows.len();
    if n == 0 || area.size.w <= 0 || area.size.h <= 0 {
        return Vec::new();
    }

    let mut ordered: Vec<(WindowId, Rect)> = windows.to_vec();
    ordered.sort_by_key(|(id, r)| (r.loc.y, r.loc.x, *id));

    let cols = best_columns(&ordered, area, candidate_columns(n));
    let rows = n.div_ceil(cols);
    let cell = cell_size(area, cols, rows);
    let mut out = Vec::with_capacity(n);

    for (i, (id, r)) in ordered.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        // The last row is centred when it isn't full.
        let in_row = if row == rows - 1 {
            n - row * cols
        } else {
            cols
        };
        let row_offset = (cols - in_row) as f64 * cell.0 / 2.0;
        let s = scale_into(r.size, cell);
        let (w, h) = (f64::from(r.size.w) * s, f64::from(r.size.h) * s);
        let cx = f64::from(area.loc.x) + row_offset + cell.0 * (col as f64 + 0.5);
        let cy = f64::from(area.loc.y) + cell.1 * (row as f64 + 0.5);
        out.push((
            *id,
            Rect::new(
                ((cx - w / 2.0).round() as i32, (cy - h / 2.0).round() as i32),
                ((w.round() as i32).max(1), (h.round() as i32).max(1)),
            ),
        ));
    }
    out
}

/// Computes the center offset and uniform scale from `real` toward `target`.
///
/// `p = 0` is the original geometry and `p = 1` is the target; other values
/// are extrapolated.
pub fn interpolate(real: Rect, target: Rect, p: f64) -> (Point<f64>, f64) {
    let (from, to) = (real.to_f64().center(), target.to_f64().center());
    let offset = Point::new((to.x - from.x) * p, (to.y - from.y) * p);
    let full = if real.size.w > 0 {
        f64::from(target.size.w) / f64::from(real.size.w)
    } else {
        1.0
    };
    (offset, 1.0 + (full - 1.0) * p)
}

/// Selects the column count that maximizes total displayed area, preferring
/// fewer columns on ties.
fn best_columns(
    windows: &[(WindowId, Rect)],
    area: Rect,
    candidates: impl Iterator<Item = usize>,
) -> usize {
    let n = windows.len();
    let total = |cols: usize| -> f64 {
        let cell = cell_size(area, cols, n.div_ceil(cols));
        windows
            .iter()
            .map(|(_, r)| {
                let s = scale_into(r.size, cell);
                f64::from(r.size.w) * s * f64::from(r.size.h) * s
            })
            .sum()
    };
    candidates
        .map(|cols| (cols, total(cols)))
        .max_by(|(a, ta), (b, tb)| ta.total_cmp(tb).then(b.cmp(a)))
        .map_or(1, |(cols, _)| cols)
}

/// Returns one grid cell's floating-point width and height.
fn cell_size(area: Rect, cols: usize, rows: usize) -> (f64, f64) {
    (
        f64::from(area.size.w) / cols as f64,
        f64::from(area.size.h) / rows as f64,
    )
}

/// Produces distinct column counts for the possible row counts, most first.
fn candidate_columns(n: usize) -> impl Iterator<Item = usize> {
    let mut last = None;
    (1..=n).filter_map(move |rows| {
        let cols = n.div_ceil(rows);
        (last != Some(cols)).then(|| {
            last = Some(cols);
            cols
        })
    })
}

/// Returns the largest uniform scale that fits `size` into a cell without
/// enlarging it. Non-positive dimensions return `1.0`.
fn scale_into(size: Size, cell: (f64, f64)) -> f64 {
    if size.w <= 0 || size.h <= 0 {
        return 1.0;
    }
    (cell.0 * CELL_FILL / f64::from(size.w))
        .min(cell.1 * CELL_FILL / f64::from(size.h))
        .min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wid(n: u64) -> WindowId {
        WindowId::from(std::num::NonZeroU64::new(n).expect("non-zero"))
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y), (w, h))
    }

    #[test]
    fn layout_fits_without_overlap() {
        let area = rect(48, 160, 1824, 872);
        for n in 1..=9 {
            let windows: Vec<_> = (0..n)
                .map(|i| (wid(i as u64 + 1), rect(i * 50, i * 30, 1200, 800)))
                .collect();
            let laid = layout(&windows, area);
            assert_eq!(laid.len(), n as usize);
            for (i, (_, a)) in laid.iter().enumerate() {
                assert!(
                    area.contains_rect(*a),
                    "{n} windows: {a:?} outside {area:?}"
                );
                for (_, b) in &laid[i + 1..] {
                    assert!(!a.overlaps(*b), "{n} windows overlap: {a:?} {b:?}");
                }
                // Aspect kept (within rounding).
                let ratio = f64::from(a.size.w) / f64::from(a.size.h);
                assert!((ratio - 1.5).abs() < 0.02, "{ratio}");
            }
        }
        assert!(layout(&[], area).is_empty());
    }

    #[test]
    fn trying_fewer_column_counts_picks_the_same_count() {
        for area in [rect(48, 160, 1824, 872), rect(0, 0, 800, 2000)] {
            for n in 1..=80u64 {
                // Mixed sizes, some smaller than their cell would be.
                let windows: Vec<_> = (0..n)
                    .map(|i| {
                        let w = 200 + (i * 373 % 1500) as i32;
                        let h = 150 + (i * 211 % 900) as i32;
                        (wid(i + 1), rect(0, 0, w, h))
                    })
                    .collect();
                let n = n as usize;
                assert_eq!(
                    best_columns(&windows, area, candidate_columns(n)),
                    best_columns(&windows, area, 1..=n),
                    "{n} windows in {area:?}"
                );
            }
        }
        assert_eq!(
            candidate_columns(10).collect::<Vec<_>>(),
            [10, 5, 4, 3, 2, 1]
        );
    }

    #[test]
    fn small_windows_are_not_enlarged() {
        let laid = layout(&[(wid(1), rect(0, 0, 300, 200))], rect(0, 0, 2000, 1000));
        assert_eq!(laid[0].1.size, (300, 200).into());
    }

    #[test]
    fn interpolation_moves_centre_and_scales() {
        let real = rect(0, 0, 100, 100);
        let target = rect(200, 200, 50, 50);
        let (offset, scale) = interpolate(real, target, 0.0);
        assert_eq!((offset.x, offset.y, scale), (0.0, 0.0, 1.0));
        let (offset, scale) = interpolate(real, target, 1.0);
        assert_eq!((offset.x, offset.y, scale), (175.0, 175.0, 0.5));
    }
}

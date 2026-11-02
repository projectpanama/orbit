pub mod geometry;
pub mod output;

use geometry::Rect;

/// Side of an output to which a window is tiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Window placement, independent of temporary fullscreen state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Floating,
    Maximized,
    Tiled(Side),
}

impl Layout {
    /// Returns the frame area assigned inside `region`.
    ///
    /// Floating windows return `None` while tiled layouts split the width,
    /// with an odd extra pixel assigned to the right half.
    pub fn area(self, region: Rect) -> Option<Rect> {
        let half = region.size.w / 2;
        match self {
            Self::Floating => None,
            Self::Maximized => Some(region),
            Self::Tiled(Side::Left) => Some(Rect::new(region.loc, (half, region.size.h))),
            Self::Tiled(Side::Right) => Some(Rect::new(
                (region.loc.x + half, region.loc.y),
                (region.size.w - half, region.size.h),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_cover_the_region_without_overlap() {
        // An odd width: the right half takes the extra pixel.
        let region = Rect::new((10, 30), (1921, 1050));
        let left = Layout::Tiled(Side::Left).area(region);
        let right = Layout::Tiled(Side::Right).area(region);
        assert_eq!(left, Some(Rect::new((10, 30), (960, 1050))));
        assert_eq!(right, Some(Rect::new((970, 30), (961, 1050))));
        assert_eq!(Layout::Maximized.area(region), Some(region));
        assert_eq!(Layout::Floating.area(region), None);
    }
}

use std::ops::{Add, Sub};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Point<T = i32> {
    pub x: T,
    pub y: T,
}

impl<T> Point<T> {
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl<T> From<(T, T)> for Point<T> {
    fn from((x, y): (T, T)) -> Self {
        Self { x, y }
    }
}

impl<T: Add<Output = T>> Add for Point<T> {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl<T: Sub<Output = T>> Sub for Point<T> {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl Point {
    pub fn to_f64(self) -> Point<f64> {
        Point::new(f64::from(self.x), f64::from(self.y))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size<T = i32> {
    pub w: T,
    pub h: T,
}

impl<T> Size<T> {
    pub const fn new(w: T, h: T) -> Self {
        Self { w, h }
    }
}

impl<T> From<(T, T)> for Size<T> {
    fn from((w, h): (T, T)) -> Self {
        Self { w, h }
    }
}

impl Size {
    pub fn to_f64(self) -> Size<f64> {
        Size::new(f64::from(self.w), f64::from(self.h))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect<T = i32> {
    pub loc: Point<T>,
    pub size: Size<T>,
}

impl<T> Rect<T> {
    pub fn new(loc: impl Into<Point<T>>, size: impl Into<Size<T>>) -> Self {
        Self {
            loc: loc.into(),
            size: size.into(),
        }
    }
}

impl Rect {
    /// Returns the x coordinate of the right edge of the rectangle.
    pub fn right(self) -> i32 {
        self.loc.x + self.size.w
    }

    /// Returns the y coordinate of the bottom edge of the rectangle.
    pub fn bottom(self) -> i32 {
        self.loc.y + self.size.h
    }

    /// Returns true if this rectangle overlaps with another rectangle.
    pub fn overlaps(self, other: Self) -> bool {
        self.loc.x < other.right()
            && other.loc.x < self.right()
            && self.loc.y < other.bottom()
            && other.loc.y < self.bottom()
    }

    /// Returns true if this rectangle completely contains another rectangle.
    pub fn contains_rect(self, other: Self) -> bool {
        self.loc.x <= other.loc.x
            && self.loc.y <= other.loc.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    pub fn to_f64(self) -> Rect<f64> {
        Rect {
            loc: self.loc.to_f64(),
            size: self.size.to_f64(),
        }
    }
}

impl Rect<f64> {
    /// Returns true if this rectangle contains a point.
    pub fn contains(self, point: Point<f64>) -> bool {
        point.x >= self.loc.x
            && point.y >= self.loc.y
            && point.x < self.loc.x + self.size.w
            && point.y < self.loc.y + self.size.h
    }

    /// Returns the center point of the rectangle.
    pub fn center(self) -> Point<f64> {
        Point::new(
            self.loc.x + self.size.w / 2.0,
            self.loc.y + self.size.h / 2.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y), (w, h))
    }

    #[test]
    fn touching_rects_do_not_overlap() {
        let a = rect(0, 0, 100, 100);
        assert!(!a.overlaps(rect(100, 0, 50, 50)));
        assert!(!a.overlaps(rect(0, 100, 50, 50)));
        assert!(a.overlaps(rect(99, 99, 50, 50)));
        assert!(a.overlaps(rect(10, 10, 5, 5)));
    }

    #[test]
    fn containment_includes_the_edges_of_a_rect_but_not_of_a_point() {
        let a = rect(0, 0, 100, 100);
        assert!(a.contains_rect(a));
        assert!(a.contains_rect(rect(50, 50, 50, 50)));
        assert!(!a.contains_rect(rect(50, 50, 51, 50)));
        let a = a.to_f64();
        assert!(a.contains(Point::new(0.0, 0.0)));
        assert!(a.contains(Point::new(99.9, 99.9)));
        assert!(!a.contains(Point::new(100.0, 50.0)));
    }

    #[test]
    fn points_add_and_subtract() {
        let a = Point::new(3, 4);
        assert_eq!(a + Point::new(1, -1), Point::new(4, 3));
        assert_eq!(a - a, Point::default());
        assert_eq!(rect(-5, 2, 10, 3).right(), 5);
        assert_eq!(rect(-5, 2, 10, 3).bottom(), 5);
    }

    #[test]
    fn constructors_and_tuple_conversions_preserve_values() {
        assert_eq!(Point::new(3, -4), Point::from((3, -4)));
        assert_eq!(Size::new(800, 600), Size::from((800, 600)));
        assert_eq!(
            Rect::new(Point::new(-10, 20), Size::new(800, 600)),
            Rect::new((-10, 20), (800, 600))
        );
    }

    #[test]
    fn integer_geometry_converts_to_floating_point_geometry() {
        let point = Point::new(-3, 7);
        let size = Size::new(640, 480);
        let rectangle = Rect::new(point, size);

        assert_eq!(point.to_f64(), Point::new(-3.0, 7.0));
        assert_eq!(size.to_f64(), Size::new(640.0, 480.0));
        assert_eq!(
            rectangle.to_f64(),
            Rect::new(Point::new(-3.0, 7.0), Size::new(640.0, 480.0))
        );
    }

    #[test]
    fn rectangle_center_is_halfway_between_opposite_edges() {
        let rectangle = Rect::new((10.0, -5.0), (101.0, 51.0));

        assert_eq!(rectangle.center(), Point::new(60.5, 20.5));
    }

    #[test]
    fn point_containment_handles_fractional_and_translated_boundaries() {
        let rectangle = Rect::new((-10.5, 20.25), (4.0, 3.5));

        assert!(rectangle.contains(Point::new(-10.5, 20.25)));
        assert!(rectangle.contains(Point::new(-6.500_001, 23.749_999)));
        assert!(!rectangle.contains(Point::new(-6.5, 21.0)));
        assert!(!rectangle.contains(Point::new(-8.0, 23.75)));
        assert!(!rectangle.contains(Point::new(-10.500_001, 21.0)));
    }

    #[test]
    fn zero_sized_rectangles_do_not_overlap_or_contain_points() {
        let rectangle = rect(10, 20, 0, 0);

        assert!(!rectangle.overlaps(rectangle));
        assert!(!rectangle.to_f64().contains(Point::new(10.0, 20.0)));
    }

    #[test]
    fn overlap_is_symmetric() {
        let a = rect(-20, 5, 30, 40);
        let b = rect(-5, 25, 50, 10);

        assert_eq!(a.overlaps(b), b.overlaps(a));
    }
}

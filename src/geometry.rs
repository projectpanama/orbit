use orbit_core::layout::geometry as core;
use smithay::utils::{Logical, Point, Rectangle, Size};

pub trait ToCore {
    type Core;
    fn to_core(self) -> Self::Core;
}

pub trait ToSmithay {
    type Smithay;
    fn to_smithay(self) -> Self::Smithay;
}

pub trait RectExt {
    fn center(self) -> Point<f64, Logical>;
}

impl RectExt for Rectangle<i32, Logical> {
    fn center(self) -> Point<f64, Logical> {
        self.to_core().to_f64().center().to_smithay()
    }
}

impl ToCore for Point<i32, Logical> {
    type Core = core::Point;
    fn to_core(self) -> core::Point {
        core::Point::new(self.x, self.y)
    }
}

impl ToCore for Point<f64, Logical> {
    type Core = core::Point<f64>;
    fn to_core(self) -> core::Point<f64> {
        core::Point::new(self.x, self.y)
    }
}

impl ToCore for Size<i32, Logical> {
    type Core = core::Size;
    fn to_core(self) -> core::Size {
        core::Size::new(self.w, self.h)
    }
}

impl ToCore for Rectangle<i32, Logical> {
    type Core = core::Rect;
    fn to_core(self) -> core::Rect {
        core::Rect {
            loc: self.loc.to_core(),
            size: self.size.to_core(),
        }
    }
}

impl ToSmithay for core::Point {
    type Smithay = Point<i32, Logical>;
    fn to_smithay(self) -> Point<i32, Logical> {
        Point::from((self.x, self.y))
    }
}

impl ToSmithay for core::Point<f64> {
    type Smithay = Point<f64, Logical>;
    fn to_smithay(self) -> Point<f64, Logical> {
        Point::from((self.x, self.y))
    }
}

impl ToSmithay for core::Size {
    type Smithay = Size<i32, Logical>;
    fn to_smithay(self) -> Size<i32, Logical> {
        Size::from((self.w, self.h))
    }
}

impl ToSmithay for core::Rect {
    type Smithay = Rectangle<i32, Logical>;
    fn to_smithay(self) -> Rectangle<i32, Logical> {
        Rectangle::new(self.loc.to_smithay(), self.size.to_smithay())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let rect = Rectangle::<i32, Logical>::new((-5, 7).into(), (100, 30).into());
        assert_eq!(rect.to_core().to_smithay(), rect);
        let point = Point::<f64, Logical>::from((1.5, -2.25));
        assert_eq!(point.to_core().to_smithay(), point);
    }

    #[test]
    fn center_is_between_pixels_for_odd_sizes() {
        let rect = Rectangle::<i32, Logical>::new((10, -4).into(), (5, 8).into());
        assert_eq!(rect.center(), Point::from((12.5, 0.0)));
    }
}

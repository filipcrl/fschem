use std::ops::{Add, Mul, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn length(self) -> f64 {
        self.x.hypot(self.y)
    }
}
impl Add for Point {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}
impl Sub for Point {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}
impl Mul<f64> for Point {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self::new(self.x * rhs, self.y * rhs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Point,
    pub max: Point,
}
impl Bounds {
    pub fn new(a: Point, b: Point) -> Self {
        Self {
            min: Point::new(a.x.min(b.x), a.y.min(b.y)),
            max: Point::new(a.x.max(b.x), a.y.max(b.y)),
        }
    }
    pub fn from_points(points: impl IntoIterator<Item = Point>) -> Option<Self> {
        let mut points = points.into_iter();
        let first = points.next()?;
        let mut bounds = Self::new(first, first);
        for point in points {
            bounds.include(Self::new(point, point));
        }
        Some(bounds)
    }
    pub fn include(&mut self, other: Self) {
        self.min.x = self.min.x.min(other.min.x);
        self.min.y = self.min.y.min(other.min.y);
        self.max.x = self.max.x.max(other.max.x);
        self.max.y = self.max.y.max(other.max.y);
    }
    pub fn center(self) -> Point {
        (self.min + self.max) * 0.5
    }
    pub fn size(self) -> Point {
        self.max - self.min
    }
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
    }
    pub fn intersects(self, other: Self) -> bool {
        self.max.x >= other.min.x
            && self.min.x <= other.max.x
            && self.max.y >= other.min.y
            && self.min.y <= other.max.y
    }
    pub fn expanded(self, amount: f64) -> Self {
        let pad = Point::new(amount, amount);
        Self {
            min: self.min - pad,
            max: self.max + pad,
        }
    }
}

/// Orthonormal transform in Xschem's Y-down coordinates. Mirror local X first,
/// then rotate clockwise in quarter turns, then translate.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    pub origin: Point,
    pub x_axis: Point,
    pub y_axis: Point,
}
impl Default for Transform {
    fn default() -> Self {
        Self::new(Point::default(), 0, false)
    }
}
impl Transform {
    pub fn new(origin: Point, rotation: u8, flip: bool) -> Self {
        let (x_axis, y_axis) = match rotation % 4 {
            0 => (Point::new(1., 0.), Point::new(0., 1.)),
            1 => (Point::new(0., 1.), Point::new(-1., 0.)),
            2 => (Point::new(-1., 0.), Point::new(0., -1.)),
            _ => (Point::new(0., -1.), Point::new(1., 0.)),
        };
        Self {
            origin,
            x_axis: x_axis * if flip { -1. } else { 1. },
            y_axis,
        }
    }
    pub fn vector(self, point: Point) -> Point {
        self.x_axis * point.x + self.y_axis * point.y
    }
    pub fn point(self, point: Point) -> Point {
        self.origin + self.vector(point)
    }
    pub fn then(self, local: Self) -> Self {
        Self {
            origin: self.point(local.origin),
            x_axis: self.vector(local.x_axis),
            y_axis: self.vector(local.y_axis),
        }
    }
    pub fn bounds(self, bounds: Bounds) -> Bounds {
        Bounds::from_points(
            [
                bounds.min,
                bounds.max,
                Point::new(bounds.min.x, bounds.max.y),
                Point::new(bounds.max.x, bounds.min.y),
            ]
            .map(|p| self.point(p)),
        )
        .expect("four corners")
    }
}

/// Route new wires using only horizontal, vertical and 45-degree segments.
pub fn wire_route(start: Point, end: Point) -> Vec<Point> {
    let delta = end - start;
    let diagonal = delta.x.abs().min(delta.y.abs());
    let bend = start + Point::new(delta.x.signum() * diagonal, delta.y.signum() * diagonal);
    if bend == start || bend == end {
        vec![start, end]
    } else {
        vec![start, bend, end]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    pub center: Point,
    pub zoom: f64,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            center: Point::default(),
            zoom: 2.,
        }
    }
}
impl Viewport {
    pub fn fit(&mut self, bounds: Bounds, size: Point) {
        self.center = bounds.center();
        let extent = bounds.size();
        self.zoom = ((size.x - 60.).max(1.) / extent.x.max(20.))
            .min((size.y - 60.).max(1.) / extent.y.max(20.))
            .clamp(1e-12, 1e6);
    }
    pub fn screen(self, world: Point, size: Point) -> Point {
        (world - self.center) * self.zoom + size * 0.5
    }
    pub fn world(self, screen: Point, size: Point) -> Point {
        self.center + (screen - size * 0.5) * (1. / self.zoom)
    }
    pub fn pan(&mut self, delta: Point) {
        self.center = self.center - delta * (1. / self.zoom);
    }
    pub fn zoom_at(&mut self, factor: f64, anchor: Point, size: Point) {
        if !factor.is_finite() || factor <= 0. {
            return;
        }
        let world = self.world(anchor, size);
        self.zoom = (self.zoom * factor).clamp(1e-12, 1e6);
        self.center = world - (anchor - size * 0.5) * (1. / self.zoom);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mirrored_pin_rotates_before_translation() {
        let t = Transform::new(Point::new(100., 200.), 1, true);
        assert_eq!(t.point(Point::new(10., 20.)), Point::new(80., 190.));
        for rot in 0..4 {
            for flip in [false, true] {
                let t = Transform::new(Point::new(100., 200.), rot, flip);
                assert_eq!(t.vector(Point::new(3., 4.)).length(), 5.);
                let composed = t.then(Transform::new(Point::new(10., 20.), 3, true));
                let p = Point::new(4., 9.);
                assert_eq!(
                    composed.point(p),
                    t.point(Transform::new(Point::new(10., 20.), 3, true).point(p))
                );
            }
        }
    }
    #[test]
    fn zoom_keeps_pointer_position_fixed() {
        let mut view = Viewport {
            center: Point::new(1000., -200.),
            zoom: 1.7,
        };
        let size = Point::new(900., 650.);
        let anchor = Point::new(200., 50.);
        let world = view.world(anchor, size);
        view.zoom_at(2., anchor, size);
        assert!((view.world(anchor, size) - world).length() < 1e-9);
        view.pan(Point::new(30., -10.));
        assert!((view.screen(world, size) - (anchor + Point::new(30., -10.))).length() < 1e-9);
    }
}

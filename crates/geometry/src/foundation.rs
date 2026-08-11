//! Owned, dependency-free numeric types shared by the native kernel layers.

use crate::{Point2, Vector2};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LengthUnit {
    Millimetre,
    Centimetre,
    Metre,
    Inch,
}

impl LengthUnit {
    const fn millimetres_per_unit(self) -> f64 {
        match self {
            Self::Millimetre => 1.0,
            Self::Centimetre => 10.0,
            Self::Metre => 1_000.0,
            Self::Inch => 25.4,
        }
    }
}

/// A finite canonical model length stored in millimetres.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Length(f64);

impl Length {
    pub fn new(value: f64, unit: LengthUnit) -> Option<Self> {
        let millimetres = value * unit.millimetres_per_unit();
        millimetres.is_finite().then_some(Self(millimetres))
    }

    pub const fn millimetres(self) -> f64 {
        self.0
    }

    pub fn in_unit(self, unit: LengthUnit) -> f64 {
        self.0 / unit.millimetres_per_unit()
    }
}

/// A finite angle normalized to `[-pi, pi)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Angle(f64);

impl Angle {
    pub fn radians(value: f64) -> Option<Self> {
        value.is_finite().then(|| {
            let mut normalized = value.rem_euclid(std::f64::consts::TAU);
            if normalized >= std::f64::consts::PI {
                normalized -= std::f64::consts::TAU;
            }
            Self(normalized)
        })
    }

    pub fn degrees(value: f64) -> Option<Self> {
        Self::radians(value.to_radians())
    }

    pub const fn as_radians(self) -> f64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Point3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub const fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl std::ops::Sub for Point3 {
    type Output = Vector3;

    fn sub(self, rhs: Self) -> Self::Output {
        Vector3::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Add<Vector3> for Point3 {
    type Output = Self;

    fn add(self, rhs: Vector3) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub<Vector3> for Point3 {
    type Output = Self;

    fn sub(self, rhs: Vector3) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vector3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub const fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    pub fn dot(self, other: Self) -> f64 {
        self.x
            .mul_add(other.x, self.y.mul_add(other.y, self.z * other.z))
    }

    pub fn cross(self, other: Self) -> Self {
        Self::new(
            self.y.mul_add(other.z, -(self.z * other.y)),
            self.z.mul_add(other.x, -(self.x * other.z)),
            self.x.mul_add(other.y, -(self.y * other.x)),
        )
    }

    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }
}

impl std::ops::Add for Vector3 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub for Vector3 {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Mul<f64> for Vector3 {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl std::ops::Div<f64> for Vector3 {
    type Output = Self;
    fn div(self, rhs: f64) -> Self::Output {
        Self::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Direction2(Vector2);

impl Direction2 {
    pub fn new(vector: Vector2) -> Option<Self> {
        let length = vector.x.hypot(vector.y);
        (length.is_finite() && length > 0.0)
            .then(|| Self(Vector2::new(vector.x / length, vector.y / length)))
    }

    pub const fn vector(self) -> Vector2 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Direction3(Vector3);

impl Direction3 {
    pub fn new(vector: Vector3) -> Option<Self> {
        let scale = vector.x.abs().max(vector.y.abs()).max(vector.z.abs());
        if !scale.is_finite() || scale == 0.0 {
            return None;
        }
        let scaled = Vector3::new(vector.x / scale, vector.y / scale, vector.z / scale);
        let length = scaled.length();
        (length.is_finite() && length > 0.0).then(|| {
            Self(Vector3::new(
                scaled.x / length,
                scaled.y / length,
                scaled.z / length,
            ))
        })
    }

    pub const fn vector(self) -> Vector3 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds2 {
    pub min: Point2,
    pub max: Point2,
}

impl Bounds2 {
    pub fn new(min: Point2, max: Point2) -> Option<Self> {
        (min.is_finite() && max.is_finite() && min.x <= max.x && min.y <= max.y)
            .then_some(Self { min, max })
    }

    pub fn contains(self, point: Point2) -> bool {
        point.is_finite()
            && (self.min.x..=self.max.x).contains(&point.x)
            && (self.min.y..=self.max.y).contains(&point.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds3 {
    pub min: Point3,
    pub max: Point3,
}

impl Bounds3 {
    pub fn new(min: Point3, max: Point3) -> Option<Self> {
        (min.is_finite() && max.is_finite() && min.x <= max.x && min.y <= max.y && min.z <= max.z)
            .then_some(Self { min, max })
    }

    pub fn contains(self, point: Point3) -> bool {
        point.is_finite()
            && (self.min.x..=self.max.x).contains(&point.x)
            && (self.min.y..=self.max.y).contains(&point.y)
            && (self.min.z..=self.max.z).contains(&point.z)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane3 {
    pub origin: Point3,
    pub normal: Direction3,
}

impl Plane3 {
    pub fn new(origin: Point3, normal: Vector3) -> Option<Self> {
        origin.is_finite().then_some(Self {
            origin,
            normal: Direction3::new(normal)?,
        })
    }

    pub fn signed_distance(self, point: Point3) -> f64 {
        (point - self.origin).dot(self.normal.vector())
    }
}

/// Finite positive-uniform 2D similarity transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Similarity2 {
    translation: Vector2,
    cosine: f64,
    sine: f64,
    scale: f64,
}

impl Similarity2 {
    pub fn new(translation: Vector2, rotation: Angle, scale: f64) -> Option<Self> {
        (translation.is_finite() && scale.is_finite() && scale > 0.0).then(|| Self {
            translation,
            cosine: rotation.as_radians().cos(),
            sine: rotation.as_radians().sin(),
            scale,
        })
    }

    pub fn transform_point(self, point: Point2) -> Option<Point2> {
        let result = Point2::new(
            self.scale * self.cosine.mul_add(point.x, -(self.sine * point.y)) + self.translation.x,
            self.scale * self.sine.mul_add(point.x, self.cosine * point.y) + self.translation.y,
        );
        result.is_finite().then_some(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrecisionPolicy {
    pub modeling_resolution: Length,
    pub minimum_feature_size: Length,
    pub approximation_budget: Length,
}

impl PrecisionPolicy {
    pub fn new(
        modeling_resolution: Length,
        minimum_feature_size: Length,
        approximation_budget: Length,
    ) -> Option<Self> {
        (modeling_resolution.millimetres() > 0.0
            && minimum_feature_size.millimetres() >= modeling_resolution.millimetres()
            && approximation_budget.millimetres() >= modeling_resolution.millimetres())
        .then_some(Self {
            modeling_resolution,
            minimum_feature_size,
            approximation_budget,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_directions_bounds_plane_similarity_and_precision_are_validated() {
        let inch = Length::new(1.0, LengthUnit::Inch).unwrap();
        assert_eq!(inch.millimetres(), 25.4);
        assert_eq!(inch.in_unit(LengthUnit::Inch), 1.0);
        assert!(Length::new(f64::NAN, LengthUnit::Millimetre).is_none());

        let direction = Direction3::new(Vector3::new(3.0, 0.0, 4.0)).unwrap();
        assert!((direction.vector().length() - 1.0).abs() < 1.0e-15);
        assert!(Direction3::new(Vector3::default()).is_none());

        let bounds = Bounds3::new(Point3::default(), Point3::new(2.0, 3.0, 4.0)).unwrap();
        assert!(bounds.contains(Point3::new(1.0, 2.0, 3.0)));
        assert!(!bounds.contains(Point3::new(5.0, 2.0, 3.0)));

        let plane = Plane3::new(Point3::new(0.0, 0.0, 2.0), Vector3::new(0.0, 0.0, 8.0)).unwrap();
        assert_eq!(plane.signed_distance(Point3::new(0.0, 0.0, 5.0)), 3.0);

        let transform =
            Similarity2::new(Vector2::new(2.0, 3.0), Angle::degrees(90.0).unwrap(), 2.0).unwrap();
        let result = transform.transform_point(Point2::new(1.0, 0.0)).unwrap();
        assert!((result.x - 2.0).abs() < 1.0e-14);
        assert!((result.y - 5.0).abs() < 1.0e-14);

        let resolution = Length::new(1.0e-6, LengthUnit::Millimetre).unwrap();
        let minimum = Length::new(1.0e-4, LengthUnit::Millimetre).unwrap();
        let budget = Length::new(1.0e-3, LengthUnit::Millimetre).unwrap();
        assert!(PrecisionPolicy::new(resolution, minimum, budget).is_some());
        assert!(PrecisionPolicy::new(minimum, resolution, budget).is_none());
    }
}

//! Exact analytic geometry used by sketch authoring and profile extraction.
//!
//! `SketchCurve2` is the persistent, point-ID based representation.  Geometry
//! algorithms operate on [`EvaluatedCurve2`], which contains the resolved
//! coordinates for one sketch revision.  Keeping those forms separate makes
//! it impossible for display tessellation to become authoritative geometry.

use std::f64::consts::{PI, TAU};

use artificer_protocol::{
    ArcDirection as ProtocolArcDirection, PlanarCurve2, Point2 as ProtocolPoint2, PrecisionPolicy,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SketchPointId;

/// A point in a sketch plane's local `(u, v)` parameter space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SketchPoint2 {
    pub u: f64,
    pub v: f64,
}

impl SketchPoint2 {
    #[must_use]
    pub const fn new(u: f64, v: f64) -> Self {
        Self { u, v }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.u.is_finite() && self.v.is_finite()
    }

    #[must_use]
    pub fn distance_squared(self, other: Self) -> f64 {
        let delta = self - other;
        delta.length_squared()
    }

    #[must_use]
    pub fn distance(self, other: Self) -> f64 {
        self.distance_squared(other).sqrt()
    }

    #[must_use]
    pub fn total_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.u
            .total_cmp(&other.u)
            .then_with(|| self.v.total_cmp(&other.v))
    }
}

impl From<SketchPoint2> for ProtocolPoint2 {
    fn from(point: SketchPoint2) -> Self {
        Self::new(point.u, point.v)
    }
}

impl From<ProtocolPoint2> for SketchPoint2 {
    fn from(point: ProtocolPoint2) -> Self {
        Self::new(point.x, point.y)
    }
}

impl std::ops::Add<SketchVector2> for SketchPoint2 {
    type Output = Self;

    fn add(self, rhs: SketchVector2) -> Self::Output {
        Self::new(self.u + rhs.u, self.v + rhs.v)
    }
}

impl std::ops::Sub for SketchPoint2 {
    type Output = SketchVector2;

    fn sub(self, rhs: Self) -> Self::Output {
        SketchVector2::new(self.u - rhs.u, self.v - rhs.v)
    }
}

/// A vector in sketch-plane coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SketchVector2 {
    pub u: f64,
    pub v: f64,
}

impl SketchVector2 {
    #[must_use]
    pub const fn new(u: f64, v: f64) -> Self {
        Self { u, v }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.u.is_finite() && self.v.is_finite()
    }

    #[must_use]
    pub fn dot(self, other: Self) -> f64 {
        self.u.mul_add(other.u, self.v * other.v)
    }

    #[must_use]
    pub fn cross(self, other: Self) -> f64 {
        self.u.mul_add(other.v, -(self.v * other.u))
    }

    #[must_use]
    pub fn length_squared(self) -> f64 {
        self.dot(self)
    }

    #[must_use]
    pub fn length(self) -> f64 {
        self.length_squared().sqrt()
    }

    #[must_use]
    pub fn normalized(self) -> Option<Self> {
        let length = self.length();
        (length.is_finite() && length > 0.0).then(|| self / length)
    }

    #[must_use]
    pub const fn left_normal(self) -> Self {
        Self::new(-self.v, self.u)
    }
}

impl std::ops::Add for SketchVector2 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.u + rhs.u, self.v + rhs.v)
    }
}

impl std::ops::Sub for SketchVector2 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.u - rhs.u, self.v - rhs.v)
    }
}

impl std::ops::Mul<f64> for SketchVector2 {
    type Output = Self;

    fn mul(self, rhs: f64) -> Self::Output {
        Self::new(self.u * rhs, self.v * rhs)
    }
}

impl std::ops::Div<f64> for SketchVector2 {
    type Output = Self;

    fn div(self, rhs: f64) -> Self::Output {
        Self::new(self.u / rhs, self.v / rhs)
    }
}

impl std::ops::Neg for SketchVector2 {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self::new(-self.u, -self.v)
    }
}

/// Conservative Cartesian bounds for one analytic curve.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Aabb2 {
    pub min: SketchPoint2,
    pub max: SketchPoint2,
}

impl Aabb2 {
    #[must_use]
    pub fn from_points(first: SketchPoint2, second: SketchPoint2) -> Self {
        Self {
            min: SketchPoint2::new(first.u.min(second.u), first.v.min(second.v)),
            max: SketchPoint2::new(first.u.max(second.u), first.v.max(second.v)),
        }
    }

    pub fn include(&mut self, point: SketchPoint2) {
        self.min.u = self.min.u.min(point.u);
        self.min.v = self.min.v.min(point.v);
        self.max.u = self.max.u.max(point.u);
        self.max.v = self.max.v.max(point.v);
    }

    #[must_use]
    pub fn expanded(self, amount: f64) -> Self {
        Self {
            min: SketchPoint2::new(self.min.u - amount, self.min.v - amount),
            max: SketchPoint2::new(self.max.u + amount, self.max.v + amount),
        }
    }

    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.min.u <= other.max.u
            && other.min.u <= self.max.u
            && self.min.v <= other.max.v
            && other.min.v <= self.max.v
    }

    #[must_use]
    pub fn contains(self, point: SketchPoint2) -> bool {
        point.u >= self.min.u
            && point.u <= self.max.u
            && point.v >= self.min.v
            && point.v <= self.max.v
    }
}

/// Persisted exact curve intent. Endpoints and centres refer to authored point
/// outputs rather than copying coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SketchCurve2 {
    Line {
        start: SketchPointId,
        end: SketchPointId,
    },
    CircularArc {
        center: SketchPointId,
        start: SketchPointId,
        end: SketchPointId,
        direction: CurveDirection,
    },
    Circle {
        center: SketchPointId,
        radius: f64,
        direction: CurveDirection,
    },
}

impl SketchCurve2 {
    /// The points this curve is defined by, in deterministic order.
    #[must_use]
    pub fn referenced_points(self) -> Vec<SketchPointId> {
        match self {
            Self::Line { start, end } => vec![start, end],
            Self::CircularArc {
                center, start, end, ..
            } => vec![center, start, end],
            Self::Circle { center, .. } => vec![center],
        }
    }
}

/// Direction of travel around a circular carrier in sketch-plane coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveDirection {
    CounterClockwise,
    Clockwise,
}

impl From<CurveDirection> for ProtocolArcDirection {
    fn from(direction: CurveDirection) -> Self {
        match direction {
            CurveDirection::CounterClockwise => Self::CounterClockwise,
            CurveDirection::Clockwise => Self::Clockwise,
        }
    }
}

impl From<ProtocolArcDirection> for CurveDirection {
    fn from(direction: ProtocolArcDirection) -> Self {
        match direction {
            ProtocolArcDirection::CounterClockwise => Self::CounterClockwise,
            ProtocolArcDirection::Clockwise => Self::Clockwise,
        }
    }
}

/// Resolved analytic curve for one evaluated sketch revision.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvaluatedCurve2 {
    Line {
        start: SketchPoint2,
        end: SketchPoint2,
    },
    CircularArc {
        center: SketchPoint2,
        start: SketchPoint2,
        end: SketchPoint2,
        direction: CurveDirection,
    },
    Circle {
        center: SketchPoint2,
        radius: f64,
        direction: CurveDirection,
    },
}

/// A rigid 2D transform. Reflections are deliberately excluded so arc
/// direction cannot be changed accidentally.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidTransform2 {
    pub cos_angle: f64,
    pub sin_angle: f64,
    pub translation: SketchVector2,
}

impl RigidTransform2 {
    #[must_use]
    pub fn new(angle_radians: f64, translation: SketchVector2) -> Self {
        Self {
            cos_angle: angle_radians.cos(),
            sin_angle: angle_radians.sin(),
            translation,
        }
    }

    #[must_use]
    pub fn apply_point(self, point: SketchPoint2) -> SketchPoint2 {
        SketchPoint2::new(
            self.cos_angle.mul_add(point.u, -(self.sin_angle * point.v)) + self.translation.u,
            self.sin_angle.mul_add(point.u, self.cos_angle * point.v) + self.translation.v,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Error)]
pub enum CurveGeometryError {
    #[error("curve coordinates must be finite")]
    NonFinite,
    #[error("curve is degenerate")]
    Degenerate,
    #[error("arc endpoints disagree on radius")]
    RadiusMismatch,
    #[error("parameter must be finite and lie in the curve domain")]
    ParameterOutsideDomain,
}

impl EvaluatedCurve2 {
    pub fn validate(self, precision: &PrecisionPolicy) -> Result<(), CurveGeometryError> {
        let finite = match self {
            Self::Line { start, end } => start.is_finite() && end.is_finite(),
            Self::CircularArc {
                center, start, end, ..
            } => center.is_finite() && start.is_finite() && end.is_finite(),
            Self::Circle { center, radius, .. } => center.is_finite() && radius.is_finite(),
        };
        if !finite {
            return Err(CurveGeometryError::NonFinite);
        }

        match self {
            Self::Line { start, end } => {
                if start.distance(end) < precision.min_feature_size {
                    Err(CurveGeometryError::Degenerate)
                } else {
                    Ok(())
                }
            }
            Self::CircularArc {
                center, start, end, ..
            } => {
                let first = center.distance(start);
                let second = center.distance(end);
                if first < precision.min_feature_size || start == end {
                    return Err(CurveGeometryError::Degenerate);
                }
                let allowed = precision.linear_agreement.max(
                    precision
                        .modeling_resolution
                        .max(f64::EPSILON * first.max(second) * 16.0),
                );
                if (first - second).abs() > allowed {
                    Err(CurveGeometryError::RadiusMismatch)
                } else {
                    Ok(())
                }
            }
            Self::Circle { radius, .. } => {
                if radius < precision.min_feature_size {
                    Err(CurveGeometryError::Degenerate)
                } else {
                    Ok(())
                }
            }
        }
    }

    #[must_use]
    pub const fn is_periodic(self) -> bool {
        matches!(self, Self::Circle { .. })
    }

    #[must_use]
    pub const fn parameter_domain(self) -> std::ops::RangeInclusive<f64> {
        0.0..=1.0
    }

    pub fn evaluate(self, parameter: f64) -> Result<SketchPoint2, CurveGeometryError> {
        if !parameter.is_finite()
            || !(0.0..=1.0).contains(&parameter)
            || (self.is_periodic() && parameter == 1.0)
        {
            return Err(CurveGeometryError::ParameterOutsideDomain);
        }
        // Non-periodic authored endpoints are authoritative topology. Avoid
        // reconstructing them through interpolation or trigonometry: even a
        // one-ulp drift can turn an exact shared point into an open profile at
        // the sketch/compiler boundary.
        if parameter == 0.0
            && let Self::Line { start, .. } | Self::CircularArc { start, .. } = self
        {
            return Ok(start);
        }
        if parameter == 1.0
            && let Self::Line { end, .. } | Self::CircularArc { end, .. } = self
        {
            return Ok(end);
        }
        Ok(match self {
            Self::Line { start, end } => start + (end - start) * parameter,
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let start_angle = angle_of(start - center);
                let sweep = directed_sweep(start_angle, angle_of(end - center), direction);
                point_on_circle(
                    center,
                    center.distance(start),
                    start_angle + sweep * parameter,
                )
            }
            Self::Circle {
                center,
                radius,
                direction,
            } => {
                let sign = direction_sign(direction);
                point_on_circle(center, radius, sign * TAU * parameter)
            }
        })
    }

    pub fn tangent(self, parameter: f64) -> Result<SketchVector2, CurveGeometryError> {
        let point = self.evaluate(parameter)?;
        Ok(match self {
            Self::Line { start, end } => end - start,
            Self::CircularArc {
                center, direction, ..
            }
            | Self::Circle {
                center, direction, ..
            } => (point - center).left_normal() * direction_sign(direction),
        })
    }

    #[must_use]
    pub fn endpoints(self) -> Option<(SketchPoint2, SketchPoint2)> {
        match self {
            Self::Line { start, end } | Self::CircularArc { start, end, .. } => Some((start, end)),
            Self::Circle { .. } => None,
        }
    }

    #[must_use]
    pub fn radius(self) -> Option<f64> {
        match self {
            Self::Line { .. } => None,
            Self::CircularArc { center, start, .. } => Some(center.distance(start)),
            Self::Circle { radius, .. } => Some(radius),
        }
    }

    #[must_use]
    pub fn center(self) -> Option<SketchPoint2> {
        match self {
            Self::Line { .. } => None,
            Self::CircularArc { center, .. } | Self::Circle { center, .. } => Some(center),
        }
    }

    #[must_use]
    pub fn reverse(self) -> Self {
        match self {
            Self::Line { start, end } => Self::Line {
                start: end,
                end: start,
            },
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => Self::CircularArc {
                center,
                start: end,
                end: start,
                direction: opposite_direction(direction),
            },
            Self::Circle {
                center,
                radius,
                direction,
            } => Self::Circle {
                center,
                radius,
                direction: opposite_direction(direction),
            },
        }
    }

    #[must_use]
    pub fn transformed(self, transform: RigidTransform2) -> Self {
        match self {
            Self::Line { start, end } => Self::Line {
                start: transform.apply_point(start),
                end: transform.apply_point(end),
            },
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => Self::CircularArc {
                center: transform.apply_point(center),
                start: transform.apply_point(start),
                end: transform.apply_point(end),
                direction,
            },
            Self::Circle {
                center,
                radius,
                direction,
            } => Self::Circle {
                center: transform.apply_point(center),
                radius,
                direction,
            },
        }
    }

    #[must_use]
    pub fn arc_length(self) -> f64 {
        match self {
            Self::Line { start, end } => start.distance(end),
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                center.distance(start)
                    * directed_sweep(angle_of(start - center), angle_of(end - center), direction)
                        .abs()
            }
            Self::Circle { radius, .. } => TAU * radius,
        }
    }

    /// Signed contribution to `1/2 integral(x dy - y dx)`.
    #[must_use]
    pub fn signed_area_contribution(self) -> f64 {
        match self {
            Self::Line { start, end } => 0.5 * (start.u * end.v - start.v * end.u),
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => arc_area(center, start, end, direction),
            Self::Circle {
                radius, direction, ..
            } => direction_sign(direction) * PI * radius * radius,
        }
    }

    #[must_use]
    pub fn bounds(self) -> Aabb2 {
        match self {
            Self::Line { start, end } => Aabb2::from_points(start, end),
            Self::Circle { center, radius, .. } => Aabb2 {
                min: SketchPoint2::new(center.u - radius, center.v - radius),
                max: SketchPoint2::new(center.u + radius, center.v + radius),
            },
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let mut bounds = Aabb2::from_points(start, end);
                let start_angle = angle_of(start - center);
                let end_angle = angle_of(end - center);
                let radius = center.distance(start);
                for candidate in [0.0, PI / 2.0, PI, 3.0 * PI / 2.0] {
                    if angle_on_directed_arc(candidate, start_angle, end_angle, direction, 0.0) {
                        bounds.include(point_on_circle(center, radius, candidate));
                    }
                }
                bounds
            }
        }
    }

    #[must_use]
    pub fn closest_parameter(self, point: SketchPoint2) -> f64 {
        match self {
            Self::Line { start, end } => {
                let delta = end - start;
                ((point - start).dot(delta) / delta.length_squared()).clamp(0.0, 1.0)
            }
            Self::Circle {
                center, direction, ..
            } => parameter_for_circle_angle(angle_of(point - center), direction),
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let start_angle = angle_of(start - center);
                let end_angle = angle_of(end - center);
                let point_angle = angle_of(point - center);
                let sweep = directed_sweep(start_angle, end_angle, direction);
                let from_start = directed_sweep_allow_zero(start_angle, point_angle, direction);
                if from_start <= sweep.abs() {
                    (from_start / sweep.abs()).clamp(0.0, 1.0)
                } else if point.distance(start) <= point.distance(end) {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }

    pub fn subcurve(
        self,
        start_parameter: f64,
        end_parameter: f64,
    ) -> Result<Self, CurveGeometryError> {
        if !start_parameter.is_finite()
            || !end_parameter.is_finite()
            || start_parameter < 0.0
            || end_parameter > 1.0
            || start_parameter >= end_parameter
        {
            return Err(CurveGeometryError::ParameterOutsideDomain);
        }
        let start = self.evaluate(start_parameter)?;
        let end_parameter_for_eval = if self.is_periodic() && end_parameter == 1.0 {
            0.0
        } else {
            end_parameter
        };
        let end = self.evaluate(end_parameter_for_eval)?;
        Ok(match self {
            Self::Line { .. } => Self::Line { start, end },
            Self::CircularArc {
                center, direction, ..
            }
            | Self::Circle {
                center, direction, ..
            } => Self::CircularArc {
                center,
                start,
                end,
                direction,
            },
        })
    }

    #[must_use]
    pub fn to_planar_curve(self) -> PlanarCurve2 {
        match self {
            Self::Line { start, end } => PlanarCurve2::Line {
                start: start.into(),
                end: end.into(),
            },
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => PlanarCurve2::CircularArc {
                center: center.into(),
                start: start.into(),
                end: end.into(),
                direction: direction.into(),
            },
            Self::Circle {
                center,
                radius,
                direction,
            } => PlanarCurve2::Circle {
                center: center.into(),
                radius,
                direction: direction.into(),
            },
        }
    }
}

#[must_use]
pub(crate) fn opposite_direction(direction: CurveDirection) -> CurveDirection {
    match direction {
        CurveDirection::CounterClockwise => CurveDirection::Clockwise,
        CurveDirection::Clockwise => CurveDirection::CounterClockwise,
    }
}

#[must_use]
pub(crate) fn direction_sign(direction: CurveDirection) -> f64 {
    match direction {
        CurveDirection::CounterClockwise => 1.0,
        CurveDirection::Clockwise => -1.0,
    }
}

#[must_use]
pub(crate) fn normalize_angle(angle: f64) -> f64 {
    angle.rem_euclid(TAU)
}

#[must_use]
pub(crate) fn angle_of(vector: SketchVector2) -> f64 {
    normalize_angle(vector.v.atan2(vector.u))
}

#[must_use]
pub(crate) fn directed_sweep(start: f64, end: f64, direction: CurveDirection) -> f64 {
    let magnitude = match direction {
        CurveDirection::CounterClockwise => (end - start).rem_euclid(TAU),
        CurveDirection::Clockwise => (start - end).rem_euclid(TAU),
    };
    direction_sign(direction) * if magnitude == 0.0 { TAU } else { magnitude }
}

#[must_use]
pub(crate) fn directed_sweep_allow_zero(start: f64, end: f64, direction: CurveDirection) -> f64 {
    match direction {
        CurveDirection::CounterClockwise => (end - start).rem_euclid(TAU),
        CurveDirection::Clockwise => (start - end).rem_euclid(TAU),
    }
}

#[must_use]
pub(crate) fn angle_on_directed_arc(
    candidate: f64,
    start: f64,
    end: f64,
    direction: CurveDirection,
    angular_tolerance: f64,
) -> bool {
    let total = directed_sweep(start, end, direction).abs();
    let partial = directed_sweep_allow_zero(start, candidate, direction);
    partial <= total + angular_tolerance
}

#[must_use]
pub(crate) fn parameter_for_circle_angle(angle: f64, direction: CurveDirection) -> f64 {
    match direction {
        CurveDirection::CounterClockwise => normalize_angle(angle) / TAU,
        CurveDirection::Clockwise => normalize_angle(-angle) / TAU,
    }
}

#[must_use]
pub(crate) fn parameter_for_arc_angle(
    angle: f64,
    start: f64,
    end: f64,
    direction: CurveDirection,
) -> f64 {
    directed_sweep_allow_zero(start, angle, direction) / directed_sweep(start, end, direction).abs()
}

#[must_use]
fn point_on_circle(center: SketchPoint2, radius: f64, angle: f64) -> SketchPoint2 {
    SketchPoint2::new(
        radius.mul_add(angle.cos(), center.u),
        radius.mul_add(angle.sin(), center.v),
    )
}

#[must_use]
fn arc_area(
    center: SketchPoint2,
    start: SketchPoint2,
    end: SketchPoint2,
    direction: CurveDirection,
) -> f64 {
    let start_angle = angle_of(start - center);
    let end_angle = angle_of(end - center);
    let delta = directed_sweep(start_angle, end_angle, direction);
    let radius = center.distance(start);
    0.5 * (radius
        * (center.u * (end_angle.sin() - start_angle.sin())
            - center.v * (end_angle.cos() - start_angle.cos()))
        + radius * radius * delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(first: f64, second: f64) {
        assert!((first - second).abs() < 1.0e-10, "{first} != {second}");
    }

    #[test]
    fn directed_arc_evaluation_and_reversal_are_consistent() {
        let arc = EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(0.0, 0.0),
            start: SketchPoint2::new(1.0, 0.0),
            end: SketchPoint2::new(0.0, 1.0),
            direction: CurveDirection::CounterClockwise,
        };
        let middle = arc.evaluate(0.5).unwrap();
        near(middle.u, 2.0_f64.sqrt() / 2.0);
        near(middle.v, 2.0_f64.sqrt() / 2.0);
        assert_eq!(
            arc.evaluate(0.25).unwrap(),
            arc.reverse().evaluate(0.75).unwrap()
        );
    }

    #[test]
    fn nonperiodic_evaluation_and_subcurves_preserve_authored_endpoint_bits() {
        let arc = EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(0.0, 0.0),
            start: SketchPoint2::new(2.0, 0.0),
            end: SketchPoint2::new(-2.0, 0.0),
            direction: CurveDirection::CounterClockwise,
        };
        assert_eq!(arc.evaluate(0.0).unwrap(), SketchPoint2::new(2.0, 0.0));
        assert_eq!(arc.evaluate(1.0).unwrap(), SketchPoint2::new(-2.0, 0.0));
        assert_eq!(arc.subcurve(0.0, 1.0).unwrap(), arc);

        let first_half = arc.subcurve(0.0, 0.5).unwrap();
        let second_half = arc.subcurve(0.5, 1.0).unwrap();
        let (first_start, first_end) = first_half.endpoints().unwrap();
        let (second_start, second_end) = second_half.endpoints().unwrap();
        assert_eq!(first_start, SketchPoint2::new(2.0, 0.0));
        assert_eq!(first_end, second_start);
        assert_eq!(second_end, SketchPoint2::new(-2.0, 0.0));

        let line = EvaluatedCurve2::Line {
            start: SketchPoint2::new(1.0e16, -3.0),
            end: SketchPoint2::new(1.0e16 + 4.0, 7.0),
        };
        assert_eq!(line.evaluate(0.0).unwrap(), line.endpoints().unwrap().0);
        assert_eq!(line.evaluate(1.0).unwrap(), line.endpoints().unwrap().1);
        assert_eq!(line.subcurve(0.0, 1.0).unwrap(), line);
    }

    #[test]
    fn analytic_area_preserves_circles_and_arcs() {
        let circle = EvaluatedCurve2::Circle {
            center: SketchPoint2::new(20.0, -4.0),
            radius: 3.0,
            direction: CurveDirection::CounterClockwise,
        };
        near(circle.signed_area_contribution(), 9.0 * PI);

        let upper = EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(0.0, 0.0),
            start: SketchPoint2::new(1.0, 0.0),
            end: SketchPoint2::new(-1.0, 0.0),
            direction: CurveDirection::CounterClockwise,
        };
        let closing = EvaluatedCurve2::Line {
            start: SketchPoint2::new(-1.0, 0.0),
            end: SketchPoint2::new(1.0, 0.0),
        };
        near(
            upper.signed_area_contribution() + closing.signed_area_contribution(),
            PI / 2.0,
        );
    }

    #[test]
    fn bounds_include_arc_extrema_without_sampling() {
        let arc = EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(2.0, 3.0),
            start: SketchPoint2::new(2.0, 5.0),
            end: SketchPoint2::new(2.0, 1.0),
            direction: CurveDirection::CounterClockwise,
        };
        let bounds = arc.bounds();
        assert_eq!(bounds.min, SketchPoint2::new(0.0, 1.0));
        assert_eq!(bounds.max, SketchPoint2::new(2.0, 5.0));
    }

    #[test]
    fn complete_circle_subcurve_is_an_exact_arc() {
        let circle = EvaluatedCurve2::Circle {
            center: SketchPoint2::new(0.0, 0.0),
            radius: 2.0,
            direction: CurveDirection::CounterClockwise,
        };
        let quarter = circle.subcurve(0.0, 0.25).unwrap();
        near(quarter.arc_length(), PI);
        assert!(matches!(
            quarter.to_planar_curve(),
            PlanarCurve2::CircularArc { .. }
        ));
    }
}

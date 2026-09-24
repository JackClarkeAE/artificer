//! One agreement model (ADR 0056 G1; ADR 0002): the answer to "do these two
//! positions agree", written once.
//!
//! The review of 2026-09-23 counted nine formulas for that question in the
//! construction modules, four ways of computing the "scale" they multiply,
//! and two floors for the angular agreement. A body one rung accepts can be
//! refused by the next for no geometric reason. This module is the one
//! place the formula lives; every rung that reads it agrees with every
//! other, and a change to the policy's meaning is a change here.
//!
//! The policy's linear agreement is dimensioned (millimetres, ADR 0033) and
//! is read as *relative* to the coordinates being compared: two points a
//! metre from the origin are represented to a coarser absolute precision
//! than two points a millimetre from it, and an agreement that ignored that
//! would refuse geometry it accepts nearer the origin. The scale is the
//! largest coordinate magnitude in play, floored at one so that a body
//! smaller than a unit is not held to a tighter bound than its own
//! representation carries.
//!
//! The weld multipliers are the named forms of the `·8`, `·32` and `·128`
//! literals: how many agreements apart two ends may lie and still be one
//! point, at each of the three stages that weld.

use artificer_protocol::PrecisionPolicy;

use crate::topology::{Point3, Topology};

/// The floor under every linear agreement: a policy that asks for less than
/// this is asking for more than a double can express across a body.
const LINEAR_FLOOR: f64 = 1.0e-12;

/// The floor under the angular agreement, in radians.
const ANGULAR_FLOOR: f64 = 1.0e-12;

/// Two ends of pieces of one carrier, cut by the same arithmetic at a seam,
/// agree to the last few bits: a handful of agreements. Part of the G1
/// vocabulary the construction rungs (Tracks B, F, I) read; not every
/// multiplier is consumed by the robustness track that introduces them.
#[allow(dead_code)]
pub(crate) const SEAM_WELD: f64 = 8.0;

/// Two faces reporting one section curve by different routes agree to more
/// bits than that: the weld the section chaining and the sew use.
pub(crate) const SECTION_WELD: f64 = 32.0;

/// Face pieces sewn after independent 2D Booleans on either side of an edge
/// agree only to rounding accumulated across both: the widest weld.
#[allow(dead_code)]
pub(crate) const SEW_WELD: f64 = 128.0;

/// The agreement bounds a precision policy asks for, floored where a double
/// cannot answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Agreement {
    linear: f64,
    angular: f64,
    feature: f64,
}

impl From<PrecisionPolicy> for Agreement {
    fn from(precision: PrecisionPolicy) -> Self {
        Self {
            linear: precision.linear_agreement.max(LINEAR_FLOOR),
            angular: precision.angular_agreement_radians.max(ANGULAR_FLOOR),
            feature: precision.min_feature_size.max(LINEAR_FLOOR),
        }
    }
}

impl Agreement {
    /// The distance within which two points of a body of coordinate `scale`
    /// are the same point.
    #[must_use]
    pub(crate) fn point(self, scale: f64) -> f64 {
        self.linear * coordinate_scale_of(scale)
    }

    /// The angle within which two directions are the same direction. Part of
    /// the G1 vocabulary the construction rungs read.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn angle(self) -> f64 {
        self.angular
    }

    /// The smallest feature the policy retains: structure below it is a
    /// sliver, not geometry.
    #[must_use]
    pub(crate) const fn feature(self) -> f64 {
        self.feature
    }

    /// The distance two ends may lie apart and still weld into one point at
    /// a stage that welds `multiplier` agreements wide.
    #[must_use]
    pub(crate) fn weld(self, scale: f64, multiplier: f64) -> f64 {
        self.point(scale) * multiplier
    }
}

/// A coordinate magnitude as a scale: floored at one, and one when it is
/// not a finite number at all.
fn coordinate_scale_of(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.max(1.0)
    } else {
        1.0
    }
}

/// The scale of a set of points: the largest coordinate magnitude among
/// them, floored at one.
#[must_use]
pub(crate) fn scale_of_points(points: impl IntoIterator<Item = Point3>) -> f64 {
    coordinate_scale_of(
        points
            .into_iter()
            .map(|point| point.x.abs().max(point.y.abs()).max(point.z.abs()))
            .fold(0.0_f64, f64::max),
    )
}

impl Topology {
    /// The one "scale" of a body: the largest coordinate magnitude of any of
    /// its vertices, floored at one.
    #[must_use]
    pub(crate) fn coordinate_scale(&self) -> f64 {
        scale_of_points(self.vertices.iter().map(|vertex| vertex.value.point))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_point_agreement_grows_with_the_scale_and_never_below_one() {
        let agreement = Agreement::from(PrecisionPolicy::default());
        assert_eq!(agreement.point(0.001), 1.0e-9);
        assert_eq!(agreement.point(1.0), 1.0e-9);
        assert_eq!(agreement.point(1.0e5), 1.0e-4);
        assert_eq!(agreement.point(f64::NAN), 1.0e-9);
        assert_eq!(agreement.weld(10.0, SECTION_WELD), 32.0e-8);
    }

    #[test]
    fn the_floors_hold_against_a_policy_that_asks_for_nothing() {
        let agreement = Agreement::from(PrecisionPolicy {
            linear_agreement: 0.0,
            angular_agreement_radians: 0.0,
            min_feature_size: 0.0,
            ..PrecisionPolicy::default()
        });
        assert_eq!(agreement.point(1.0), LINEAR_FLOOR);
        assert_eq!(agreement.angle(), ANGULAR_FLOOR);
        assert_eq!(agreement.feature(), LINEAR_FLOOR);
    }

    #[test]
    fn a_body_scale_is_its_largest_coordinate() {
        let mut topology = Topology::default();
        assert_eq!(topology.coordinate_scale(), 1.0);
        for point in [Point3::new(0.5, -0.25, 0.0), Point3::new(-12.0, 3.0, 7.0)] {
            topology.vertices.push(crate::topology::Record {
                id: crate::topology::EntityId::from_raw(1),
                value: crate::topology::Vertex { point },
            });
        }
        assert_eq!(topology.coordinate_scale(), 12.0);
    }
}

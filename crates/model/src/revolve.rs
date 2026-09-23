//! A revolve of sketch regions about an axis (ADR 0055).
//!
//! A revolve names its profile the way an extrusion does — the sketch and
//! the signatures of the regions it takes — and names its axis rather than
//! storing one: a line drawn in the sketch, one of the sketch's own axes, or
//! one of the document's origin axes. Replay compiles the regions and finds
//! the axis in the sketch and the document as they now stand, so editing the
//! sketch, moving its plane or changing a variable one of its dimensions
//! follows reshapes the revolve the next time it is rebuilt.
//!
//! A revolve turns a full turn, or through an angle (ADR 0055 R3). An angle
//! typed over document variables stays with them, the way an extrusion's
//! distance does (ADR 0052): the recipe keeps the expression, and replay
//! evaluates it.

use std::collections::BTreeMap;

use std::collections::BTreeSet;
use std::f64::consts::TAU;

use artificer_protocol::{
    KernelCommand, PlanarAxis2, PlanarFrame3, Point2, PrecisionPolicy, RevolveAngle,
    SolidOperation, Vector3,
};
use artificer_sketch::{EvaluatedCurve2, RegionSignature, SketchEntityId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::loft::section_plane;
use crate::parameterized::ParameterizedKernelError;
use crate::parameters::{EvaluatedParameters, ParameterExpression, ParameterValue, QuantityKind};
use crate::sketch_region::{
    MAX_SELECTED_SKETCH_REGIONS, SketchRegionResolveError, compile_sketch_regions,
};
use crate::{
    DatumAxisRecipe, FeatureId, ModelDocument, ParameterId, ReplayAction, ResolvedDatumAxis,
    ResolvedDatumPlane, SketchId,
};

/// Schema written for newly created revolve recipes.
pub const CURRENT_SKETCH_REVOLVE_RECIPE_VERSION: u32 = 1;

const fn current_sketch_revolve_recipe_version() -> u32 {
    CURRENT_SKETCH_REVOLVE_RECIPE_VERSION
}

/// One of a sketch's own axes, through its origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SketchAxisDirection {
    /// The sketch's horizontal axis.
    U,
    /// The sketch's vertical axis.
    V,
}

/// One of the document's origin axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginAxis {
    X,
    Y,
    Z,
}

impl OriginAxis {
    #[must_use]
    pub const fn direction(self) -> Vector3 {
        match self {
            Self::X => Vector3::new(1.0, 0.0, 0.0),
            Self::Y => Vector3::new(0.0, 1.0, 0.0),
            Self::Z => Vector3::new(0.0, 0.0, 1.0),
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::X => "X axis",
            Self::Y => "Y axis",
            Self::Z => "Z axis",
        }
    }
}

/// The line a revolve turns about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RevolveAxis {
    /// A straight line drawn in the revolve's own sketch — usually a
    /// centreline, but any line will do.
    SketchLine { entity: SketchEntityId },
    /// The sketch's own horizontal or vertical axis through its origin.
    SketchAxis { axis: SketchAxisDirection },
    /// One of the document's origin axes. It has to lie in the sketch's
    /// plane: the X axis serves a sketch on the XY or XZ plane, but not one
    /// on the YZ plane or on a plane lifted off the origin.
    OriginAxis { axis: OriginAxis },
    /// A construction axis, named by its feature. It has to lie in the
    /// sketch's plane too, and the revolve turns right-handed about it the
    /// way it runs.
    DatumAxis { axis: FeatureId },
}

/// Which way a revolve that stops short of a full turn goes from its sketch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevolveDirection {
    /// Right-handed about the axis as it runs: thumb along the axis, the
    /// turn goes the way the fingers curl.
    #[default]
    Forward,
    /// The other way round.
    Reversed,
    /// Half the angle each way, so the sketch sits in the middle.
    Symmetric,
}

/// How far a revolve turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RevolveExtent {
    #[default]
    FullTurn,
    /// Through `radians`, strictly between nothing and a full turn.
    Angle {
        radians: f64,
        #[serde(default)]
        direction: RevolveDirection,
    },
}

impl RevolveExtent {
    /// The kernel's angle for this extent: where the turn starts, measured
    /// from the sketch, and how far it goes.
    #[must_use]
    pub fn kernel_angle(self) -> RevolveAngle {
        match self {
            Self::FullTurn => RevolveAngle::FullTurn,
            Self::Angle { radians, direction } => RevolveAngle::partial(
                match direction {
                    RevolveDirection::Forward => 0.0,
                    RevolveDirection::Reversed => -radians,
                    RevolveDirection::Symmetric => -radians / 2.0,
                },
                radians,
            ),
        }
    }

    /// The angle turned through, in radians: a full turn's is `2π`.
    #[must_use]
    pub const fn radians(self) -> f64 {
        match self {
            Self::FullTurn => TAU,
            Self::Angle { radians, .. } => radians,
        }
    }
}

/// A revolve of regions of one sketch about an axis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchRevolve {
    #[serde(default = "current_sketch_revolve_recipe_version")]
    pub version: u32,
    pub sketch: SketchId,
    pub regions: Vec<RegionSignature>,
    pub axis: RevolveAxis,
    #[serde(default)]
    pub extent: RevolveExtent,
    pub operation: SolidOperation,
    /// The angle follows this expression over document variables: `sweep`,
    /// `sweep / 2`. Replay evaluates it and uses its value as the extent's
    /// angle, which holds what it last came to. Only an angle can follow
    /// one; a full turn has nothing to follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle_expression: Option<ParameterExpression>,
}

impl SketchRevolve {
    pub fn new(
        sketch: SketchId,
        mut regions: Vec<RegionSignature>,
        axis: RevolveAxis,
        extent: RevolveExtent,
        operation: SolidOperation,
    ) -> Result<Self, SketchRevolveError> {
        regions.sort();
        regions.dedup();
        let recipe = Self {
            version: CURRENT_SKETCH_REVOLVE_RECIPE_VERSION,
            sketch,
            regions,
            axis,
            extent,
            operation,
            angle_expression: None,
        };
        recipe.validate()?;
        Ok(recipe)
    }

    /// Makes the angle follow an expression over document variables, or
    /// stop following one. The extent's angle should hold what the
    /// expression evaluates to now.
    pub fn with_angle_expression(
        mut self,
        expression: Option<ParameterExpression>,
    ) -> Result<Self, SketchRevolveError> {
        self.angle_expression = expression;
        self.validate()?;
        Ok(self)
    }

    /// The variables the recipe reads, which its feature lists as parameter
    /// inputs so that changing one rebuilds it.
    #[must_use]
    pub fn parameter_references(&self) -> BTreeSet<ParameterId> {
        self.angle_expression
            .as_ref()
            .map(ParameterExpression::referenced_parameters)
            .unwrap_or_default()
    }

    /// The recipe with its angle taken from the evaluated variables. A recipe
    /// that follows no expression is returned as it is. An expression that
    /// comes to a whole turn makes a full turn.
    pub fn resolve_parameters(
        &self,
        parameters: &EvaluatedParameters,
    ) -> Result<Self, ParameterizedKernelError> {
        let Some(expression) = &self.angle_expression else {
            return Ok(self.clone());
        };
        let value = expression
            .evaluate_with(parameters)
            .map_err(|error| ParameterizedKernelError::AngleExpression(error.to_string()))?;
        let ParameterValue::Quantity { value } = value else {
            return Err(ParameterizedKernelError::AngleNotAnAngle);
        };
        if value.unit.quantity_kind() != QuantityKind::Angle {
            return Err(ParameterizedKernelError::AngleNotAnAngle);
        }
        // Canonical angles are radians, the recipe's unit.
        let radians = value.magnitude;
        let RevolveExtent::Angle { direction, .. } = self.extent else {
            return Err(ParameterizedKernelError::InvalidAngleValue);
        };
        let mut resolved = self.clone();
        resolved.extent = if (radians - TAU).abs() <= FULL_TURN_AGREEMENT {
            RevolveExtent::FullTurn
        } else if is_partial_turn(radians) {
            RevolveExtent::Angle { radians, direction }
        } else {
            return Err(ParameterizedKernelError::InvalidAngleValue);
        };
        Ok(resolved)
    }

    /// Structural checks that need no geometry.
    pub fn validate(&self) -> Result<(), SketchRevolveError> {
        if self.version != CURRENT_SKETCH_REVOLVE_RECIPE_VERSION {
            return Err(SketchRevolveError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.sketch.get() == 0 {
            return Err(SketchRevolveError::InvalidSketch);
        }
        if self.regions.is_empty() {
            return Err(SketchRevolveError::NoRegions);
        }
        if self.regions.len() > MAX_SELECTED_SKETCH_REGIONS {
            return Err(SketchRevolveError::TooManyRegions {
                limit: MAX_SELECTED_SKETCH_REGIONS,
            });
        }
        if self.regions.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(SketchRevolveError::NonCanonicalRegions);
        }
        if let RevolveExtent::Angle { radians, .. } = self.extent
            && !is_partial_turn(radians)
        {
            return Err(SketchRevolveError::InvalidAngle);
        }
        if let Some(expression) = &self.angle_expression {
            if self.extent == RevolveExtent::FullTurn {
                return Err(SketchRevolveError::ExpressionOnAFullTurn);
            }
            if expression.validate_bounds().is_err()
                || expression.referenced_parameters().is_empty()
            {
                return Err(SketchRevolveError::InvalidAngleExpression);
            }
        }
        Ok(())
    }

    /// Compiles the regions and finds the axis, in the sketch's plane as it
    /// now stands, and hands the kernel one revolve.
    ///
    /// `planes` are construction-plane frames a rebuild has already resolved
    /// and the document's cache does not hold yet; a sketch on such a plane
    /// is placed there.
    pub fn resolve_with_planes(
        &self,
        document: &ModelDocument,
        precision: PrecisionPolicy,
        planes: &BTreeMap<FeatureId, ResolvedDatumPlane>,
    ) -> Result<ReplayAction, SketchRegionResolveError> {
        self.resolve_with_datums(document, precision, planes, &BTreeMap::new())
    }

    /// As [`Self::resolve_with_planes`], turning about a construction axis
    /// where `axes` holds it — a rebuild passes the axes it has resolved so
    /// far — and otherwise where its recipe last put it.
    pub fn resolve_with_datums(
        &self,
        document: &ModelDocument,
        precision: PrecisionPolicy,
        planes: &BTreeMap<FeatureId, ResolvedDatumPlane>,
        axes: &BTreeMap<FeatureId, ResolvedDatumAxis>,
    ) -> Result<ReplayAction, SketchRegionResolveError> {
        self.validate()
            .map_err(SketchRegionResolveError::InvalidRevolve)?;
        let (profile, drawn_frame) =
            compile_sketch_regions(document, self.sketch, &self.regions, precision)?;
        let frame = section_plane(document, self.sketch, planes)
            .or_else(|| document.sketch_frame(self.sketch))
            .unwrap_or(drawn_frame);
        let axis = match self.axis {
            RevolveAxis::DatumAxis { axis } => {
                let line = axes
                    .get(&axis)
                    .copied()
                    .or_else(|| document.datum_axis(axis).map(DatumAxisRecipe::cached))
                    .ok_or(SketchRevolveError::MissingDatumAxis { axis })
                    .map_err(SketchRegionResolveError::InvalidRevolve)?;
                line_in_frame(line.origin, line.direction, frame, precision)
                    .ok_or(SketchRevolveError::DatumAxisOffSketchPlane { axis })
                    .map_err(SketchRegionResolveError::InvalidRevolve)?
            }
            other => resolve_axis(document, self.sketch, other, frame, precision)
                .map_err(SketchRegionResolveError::InvalidRevolve)?,
        };
        Ok(ReplayAction::Kernel(KernelCommand::RevolvePlanarProfile {
            frame,
            profile,
            axis,
            angle: self.extent.kernel_angle(),
            operation: self.operation,
        }))
    }
}

/// How close to a whole turn an evaluated angle may come and still be one.
const FULL_TURN_AGREEMENT: f64 = 1.0e-9;

/// Whether `radians` is an angle a partial revolve can turn through.
fn is_partial_turn(radians: f64) -> bool {
    radians.is_finite() && radians > 0.0 && radians < TAU
}

/// Finds a revolve axis in the sketch's own coordinates, where the kernel
/// takes it.
pub fn resolve_axis(
    document: &ModelDocument,
    sketch: SketchId,
    axis: RevolveAxis,
    frame: PlanarFrame3,
    precision: PrecisionPolicy,
) -> Result<PlanarAxis2, SketchRevolveError> {
    match axis {
        RevolveAxis::SketchLine { entity } => {
            let authoring = document
                .sketch(sketch)
                .and_then(|record| document.sketch_payload(sketch, record.geometry_revision))
                .and_then(|payload| payload.authoring())
                .ok_or(SketchRevolveError::MissingAxisLine { entity })?;
            match authoring.evaluated_curve(entity) {
                Ok(EvaluatedCurve2::Line { start, end }) => Ok(PlanarAxis2::new(
                    Point2::new(start.u, start.v),
                    Point2::new(end.u, end.v),
                )),
                Ok(_) => Err(SketchRevolveError::AxisNotALine { entity }),
                Err(_) => Err(SketchRevolveError::MissingAxisLine { entity }),
            }
        }
        RevolveAxis::SketchAxis { axis } => Ok(match axis {
            SketchAxisDirection::U => {
                PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(1.0, 0.0))
            }
            SketchAxisDirection::V => {
                PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0))
            }
        }),
        RevolveAxis::OriginAxis { axis } => origin_axis_in_frame(axis, frame, precision),
        RevolveAxis::DatumAxis { axis } => {
            let line = document
                .datum_axis(axis)
                .map(DatumAxisRecipe::cached)
                .ok_or(SketchRevolveError::MissingDatumAxis { axis })?;
            line_in_frame(line.origin, line.direction, frame, precision)
                .ok_or(SketchRevolveError::DatumAxisOffSketchPlane { axis })
        }
    }
}

/// A document origin axis in a sketch plane's coordinates, if it lies in
/// that plane.
pub fn origin_axis_in_frame(
    axis: OriginAxis,
    frame: PlanarFrame3,
    precision: PrecisionPolicy,
) -> Result<PlanarAxis2, SketchRevolveError> {
    line_in_frame(
        artificer_protocol::Point3::new(0.0, 0.0, 0.0),
        axis.direction(),
        frame,
        precision,
    )
    .ok_or(SketchRevolveError::AxisOffSketchPlane { axis })
}

/// A line in space in a sketch plane's coordinates, if it lies in that
/// plane: through `origin`, running along `direction`.
#[must_use]
pub fn line_in_frame(
    origin: artificer_protocol::Point3,
    direction: Vector3,
    frame: PlanarFrame3,
    precision: PrecisionPolicy,
) -> Option<PlanarAxis2> {
    let dot = |a: Vector3, b: Vector3| a.x * b.x + a.y * b.y + a.z * b.z;
    let normal = Vector3::new(
        frame.u.y * frame.v.z - frame.u.z * frame.v.y,
        frame.u.z * frame.v.x - frame.u.x * frame.v.z,
        frame.u.x * frame.v.y - frame.u.y * frame.v.x,
    );
    let length = dot(normal, normal).sqrt();
    let reach = dot(direction, direction).sqrt();
    if !(length.is_finite() && length > 0.0 && reach.is_finite() && reach > 0.0) {
        return None;
    }
    // The line's point, seen from the frame's origin.
    let relative = Vector3::new(
        origin.x - frame.origin.x,
        origin.y - frame.origin.y,
        origin.z - frame.origin.z,
    );
    let off_plane = (dot(relative, normal) / length).abs();
    let tilt = (dot(direction, normal) / (length * reach)).abs();
    let scale = relative
        .x
        .abs()
        .max(relative.y.abs())
        .max(relative.z.abs())
        .max(1.0);
    if off_plane > precision.linear_agreement * scale || tilt > 1.0e-9 {
        return None;
    }
    let start = Point2::new(dot(relative, frame.u), dot(relative, frame.v));
    Some(PlanarAxis2::new(
        start,
        Point2::new(
            start.x + dot(direction, frame.u) / reach,
            start.y + dot(direction, frame.v) / reach,
        ),
    ))
}

/// Why a revolve recipe cannot be kept or replayed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SketchRevolveError {
    #[error(
        "unsupported revolve recipe version {found}; this build supports {CURRENT_SKETCH_REVOLVE_RECIPE_VERSION}"
    )]
    UnsupportedVersion { found: u32 },
    #[error("a revolve requires a non-zero source sketch")]
    InvalidSketch,
    #[error("a revolve takes at least one region")]
    NoRegions,
    #[error("a revolve takes at most {limit} regions")]
    TooManyRegions { limit: usize },
    #[error("a revolve's regions must be sorted and unique")]
    NonCanonicalRegions,
    #[error("an add or cut revolve names the body it changes as its input")]
    MissingTargetBody,
    #[error("the revolve's axis line {entity} is no longer in its sketch")]
    MissingAxisLine { entity: SketchEntityId },
    #[error("the revolve's axis {entity} is not a straight line")]
    AxisNotALine { entity: SketchEntityId },
    #[error("the {} does not lie in the sketch's plane", axis.label())]
    AxisOffSketchPlane { axis: OriginAxis },
    #[error("construction axis {axis} is not in the history")]
    MissingDatumAxis { axis: FeatureId },
    #[error("construction axis {axis} does not lie in the sketch's plane")]
    DatumAxisOffSketchPlane { axis: FeatureId },
    #[error("a revolve's angle must be more than nothing and less than a full turn")]
    InvalidAngle,
    #[error("a full-turn revolve has no angle to follow a variable")]
    ExpressionOnAFullTurn,
    #[error("a revolve's angle expression must name at least one variable")]
    InvalidAngleExpression,
}

#[cfg(test)]
mod tests {
    use artificer_protocol::Point3;

    use super::*;

    fn frame(origin: [f64; 3], u: [f64; 3], v: [f64; 3]) -> PlanarFrame3 {
        PlanarFrame3::new(
            Point3::new(origin[0], origin[1], origin[2]),
            Vector3::new(u[0], u[1], u[2]),
            Vector3::new(v[0], v[1], v[2]),
        )
    }

    #[test]
    fn an_origin_axis_is_found_in_a_plane_that_holds_it() {
        let precision = PrecisionPolicy::default();
        let xz = frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        assert_eq!(
            origin_axis_in_frame(OriginAxis::Z, xz, precision),
            Ok(PlanarAxis2::new(
                Point2::new(0.0, 0.0),
                Point2::new(0.0, 1.0)
            ))
        );
        assert_eq!(
            origin_axis_in_frame(OriginAxis::X, xz, precision),
            Ok(PlanarAxis2::new(
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 0.0)
            ))
        );
        assert_eq!(
            origin_axis_in_frame(OriginAxis::Y, xz, precision),
            Err(SketchRevolveError::AxisOffSketchPlane {
                axis: OriginAxis::Y
            }),
            "the Y axis stands out of the XZ plane"
        );
        // A plane shifted along its own surface still holds the axis, which
        // it sees off its own origin.
        let shifted = frame([3.0, 0.0, -2.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        assert_eq!(
            origin_axis_in_frame(OriginAxis::Z, shifted, precision),
            Ok(PlanarAxis2::new(
                Point2::new(-3.0, 2.0),
                Point2::new(-3.0, 3.0)
            ))
        );
        // A plane lifted off the origin does not.
        let lifted = frame([0.0, 5.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        assert!(origin_axis_in_frame(OriginAxis::Z, lifted, precision).is_err());
    }

    #[test]
    fn a_recipe_needs_regions_and_keeps_them_in_order() {
        let sketch = SketchId::from_allocated(1);
        let axis = RevolveAxis::SketchAxis {
            axis: SketchAxisDirection::V,
        };
        assert_eq!(
            SketchRevolve::new(
                sketch,
                Vec::new(),
                axis,
                RevolveExtent::FullTurn,
                SolidOperation::New
            ),
            Err(SketchRevolveError::NoRegions)
        );
        let json = serde_json::json!({
            "sketch": 1,
            "regions": [],
            "axis": { "kind": "origin_axis", "axis": "z" },
            "operation": "new",
        });
        let recipe: SketchRevolve = serde_json::from_value(json).expect("decodes");
        assert_eq!(recipe.extent, RevolveExtent::FullTurn);
        assert_eq!(recipe.version, CURRENT_SKETCH_REVOLVE_RECIPE_VERSION);
    }
}

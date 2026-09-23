//! A revolve of sketch regions about an axis (ADR 0055).
//!
//! A revolve names its profile the way an extrusion does — the sketch and
//! the signatures of the regions it takes — and names its axis rather than
//! storing one: a line drawn in the sketch, one of the sketch's own axes, or
//! one of the document's origin axes. Replay compiles the regions and finds
//! the axis in the sketch and the document as they now stand, so editing the
//! sketch, moving its plane or changing a variable one of its dimensions
//! follows reshapes the revolve the next time it is rebuilt.

use std::collections::BTreeMap;

use artificer_protocol::{
    KernelCommand, PlanarAxis2, PlanarFrame3, Point2, PrecisionPolicy, RevolveAngle,
    SolidOperation, Vector3,
};
use artificer_sketch::{EvaluatedCurve2, RegionSignature, SketchEntityId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::loft::section_plane;
use crate::sketch_region::{
    MAX_SELECTED_SKETCH_REGIONS, SketchRegionResolveError, compile_sketch_regions,
};
use crate::{FeatureId, ModelDocument, ReplayAction, ResolvedDatumPlane, SketchId};

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
}

/// How far a revolve turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RevolveExtent {
    #[default]
    FullTurn,
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
        };
        recipe.validate()?;
        Ok(recipe)
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
        self.validate()
            .map_err(SketchRegionResolveError::InvalidRevolve)?;
        let (profile, drawn_frame) =
            compile_sketch_regions(document, self.sketch, &self.regions, precision)?;
        let frame = section_plane(document, self.sketch, planes)
            .or_else(|| document.sketch_frame(self.sketch))
            .unwrap_or(drawn_frame);
        let axis = resolve_axis(document, self.sketch, self.axis, frame, precision)
            .map_err(SketchRegionResolveError::InvalidRevolve)?;
        let angle = match self.extent {
            RevolveExtent::FullTurn => RevolveAngle::FullTurn,
        };
        Ok(ReplayAction::Kernel(KernelCommand::RevolvePlanarProfile {
            frame,
            profile,
            axis,
            angle,
            operation: self.operation,
        }))
    }
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
    }
}

/// A document origin axis in a sketch plane's coordinates, if it lies in
/// that plane.
pub fn origin_axis_in_frame(
    axis: OriginAxis,
    frame: PlanarFrame3,
    precision: PrecisionPolicy,
) -> Result<PlanarAxis2, SketchRevolveError> {
    let dot = |a: Vector3, b: Vector3| a.x * b.x + a.y * b.y + a.z * b.z;
    let normal = Vector3::new(
        frame.u.y * frame.v.z - frame.u.z * frame.v.y,
        frame.u.z * frame.v.x - frame.u.x * frame.v.z,
        frame.u.x * frame.v.y - frame.u.y * frame.v.x,
    );
    let length = dot(normal, normal).sqrt();
    if !(length.is_finite() && length > 0.0) {
        return Err(SketchRevolveError::AxisOffSketchPlane { axis });
    }
    // The world origin, seen from the frame's origin.
    let relative = Vector3::new(-frame.origin.x, -frame.origin.y, -frame.origin.z);
    let direction = axis.direction();
    let off_plane = (dot(relative, normal) / length).abs();
    let tilt = (dot(direction, normal) / length).abs();
    if off_plane > precision.linear_agreement || tilt > 1.0e-9 {
        return Err(SketchRevolveError::AxisOffSketchPlane { axis });
    }
    let start = Point2::new(dot(relative, frame.u), dot(relative, frame.v));
    Ok(PlanarAxis2::new(
        start,
        Point2::new(
            start.x + dot(direction, frame.u),
            start.y + dot(direction, frame.v),
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

//! A sweep of sketch regions along a path drawn in another sketch (ADR 0055).
//!
//! A sweep names its profile the way a loft names a section — a sketch and
//! the signatures of the regions it takes — and names its path as curves of
//! a second sketch, by entity id. Nothing geometric is stored. Replay
//! compiles the profile from its sketch as it now stands, puts the path's
//! curves end to end with [`artificer_sketch::SketchDefinition::ordered_chain`],
//! places both on their sketches' planes as the planes now stand, and hands
//! the kernel one [`KernelCommand::SweepPlanarProfile`]. Editing either
//! sketch, or moving its plane, reshapes the sweep the next time it is
//! rebuilt.

use std::collections::BTreeMap;
use std::f64::consts::TAU;

use artificer_protocol::{
    KernelCommand, PlanarFrame3, Point3, PrecisionPolicy, SolidOperation, SweepOrientation,
    SweepPath3, SweepSegment3, Vector3,
};
use artificer_sketch::{ChainError, CurveDirection, EvaluatedCurve2, SketchEntityId, SketchPoint2};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::loft::{SketchLoftSection, section_plane};
use crate::sketch_region::{
    MAX_SELECTED_SKETCH_REGIONS, SketchRegionResolveError, compile_sketch_regions,
};
use crate::{FeatureId, ModelDocument, ReplayAction, ResolvedDatumPlane, SketchId};

/// Schema written for newly created sweep recipes.
pub const CURRENT_SKETCH_SWEEP_RECIPE_VERSION: u32 = 1;

/// The most curves a sweep's path may name.
pub const MAX_SWEEP_PATH_CURVES: usize = artificer_protocol::MAX_SWEEP_PATH_SEGMENTS;

const fn current_sketch_sweep_recipe_version() -> u32 {
    CURRENT_SKETCH_SWEEP_RECIPE_VERSION
}

/// The path a sweep follows: curves of one sketch, put end to end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepPath {
    pub sketch: SketchId,
    /// The curves, put end to end from the free end of the one named first.
    pub entities: Vec<SketchEntityId>,
    /// Whether the path runs the other way, from the far end of the chain.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reversed: bool,
}

/// A sweep of a profile along a path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchSweep {
    #[serde(default = "current_sketch_sweep_recipe_version")]
    pub version: u32,
    pub profile: SketchLoftSection,
    pub path: SweepPath,
    #[serde(default)]
    pub orientation: SweepOrientation,
    pub operation: SolidOperation,
}

impl SketchSweep {
    pub fn new(
        profile: SketchLoftSection,
        path: SweepPath,
        orientation: SweepOrientation,
        operation: SolidOperation,
    ) -> Result<Self, SketchSweepError> {
        let recipe = Self {
            version: CURRENT_SKETCH_SWEEP_RECIPE_VERSION,
            profile,
            path,
            orientation,
            operation,
        };
        recipe.validate()?;
        Ok(recipe)
    }

    /// Structural checks that need no geometry.
    pub fn validate(&self) -> Result<(), SketchSweepError> {
        if self.version != CURRENT_SKETCH_SWEEP_RECIPE_VERSION {
            return Err(SketchSweepError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.profile.sketch.get() == 0 || self.path.sketch.get() == 0 {
            return Err(SketchSweepError::InvalidSketch);
        }
        if self.profile.regions.is_empty() {
            return Err(SketchSweepError::NoRegions);
        }
        if self.profile.regions.len() > MAX_SELECTED_SKETCH_REGIONS {
            return Err(SketchSweepError::TooManyRegions {
                limit: MAX_SELECTED_SKETCH_REGIONS,
            });
        }
        if self
            .profile
            .regions
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(SketchSweepError::NonCanonicalRegions);
        }
        // A path in the profile's own sketch lies in the profile's plane,
        // along which nothing can be swept.
        if self.path.sketch == self.profile.sketch {
            return Err(SketchSweepError::PathInProfileSketch);
        }
        if self.path.entities.is_empty() {
            return Err(SketchSweepError::EmptyPath);
        }
        if self.path.entities.len() > MAX_SWEEP_PATH_CURVES {
            return Err(SketchSweepError::TooManyPathCurves {
                limit: MAX_SWEEP_PATH_CURVES,
            });
        }
        Ok(())
    }

    /// The sketches the sweep reads: its profile's and its path's.
    #[must_use]
    pub fn sketches(&self) -> [SketchId; 2] {
        [self.profile.sketch, self.path.sketch]
    }

    /// Compiles the profile and places the path, in their sketches' planes
    /// as they now stand, and hands the kernel one sweep.
    pub fn resolve_with_planes(
        &self,
        document: &ModelDocument,
        precision: PrecisionPolicy,
        planes: &BTreeMap<FeatureId, ResolvedDatumPlane>,
    ) -> Result<ReplayAction, SketchRegionResolveError> {
        self.validate()
            .map_err(SketchRegionResolveError::InvalidSweep)?;
        let (profile, drawn_frame) = compile_sketch_regions(
            document,
            self.profile.sketch,
            &self.profile.regions,
            precision,
        )?;
        let frame = section_plane(document, self.profile.sketch, planes)
            .or_else(|| document.sketch_frame(self.profile.sketch))
            .unwrap_or(drawn_frame);
        let path = self
            .path_in_space(document, planes)
            .map_err(SketchRegionResolveError::InvalidSweep)?;
        Ok(ReplayAction::Kernel(KernelCommand::SweepPlanarProfile {
            frame,
            profile,
            path,
            orientation: self.orientation,
            operation: self.operation,
        }))
    }

    /// The path's curves, end to end, in model space.
    pub fn path_in_space(
        &self,
        document: &ModelDocument,
        planes: &BTreeMap<FeatureId, ResolvedDatumPlane>,
    ) -> Result<SweepPath3, SketchSweepError> {
        let sketch = self.path.sketch;
        let payload = document
            .sketch(sketch)
            .and_then(|record| document.sketch_payload(sketch, record.geometry_revision))
            .ok_or(SketchSweepError::MissingPathSketch(sketch))?;
        let authoring = payload
            .authoring()
            .ok_or(SketchSweepError::MissingPathSketch(sketch))?;
        let frame = section_plane(document, sketch, planes)
            .or_else(|| document.sketch_frame(sketch))
            .unwrap_or(payload.frame);
        let chain = authoring
            .ordered_chain(&self.path.entities)
            .map_err(SketchSweepError::Path)?;
        let place = |curve: &artificer_sketch::ChainCurve, reversed: bool| {
            place_curve(&curve.curve, reversed, frame, curve.entity)
        };
        let segments = if self.path.reversed {
            chain
                .iter()
                .rev()
                .map(|curve| place(curve, !curve.reversed))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            chain
                .iter()
                .map(|curve| place(curve, curve.reversed))
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(SweepPath3 { segments })
    }
}

/// One sketch curve as a path segment in space, run the way the chain runs.
fn place_curve(
    curve: &EvaluatedCurve2,
    reversed: bool,
    frame: PlanarFrame3,
    entity: SketchEntityId,
) -> Result<SweepSegment3, SketchSweepError> {
    let place = |point: SketchPoint2| {
        Point3::new(
            frame.origin.x + frame.u.x * point.u + frame.v.x * point.v,
            frame.origin.y + frame.u.y * point.u + frame.v.y * point.v,
            frame.origin.z + frame.u.z * point.u + frame.v.z * point.v,
        )
    };
    match curve {
        EvaluatedCurve2::Line { start, end } => {
            let (start, end) = if reversed { (end, start) } else { (start, end) };
            Ok(SweepSegment3::Line {
                start: place(*start),
                end: place(*end),
            })
        }
        EvaluatedCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            // Run the way the chain does: reversed, the arc starts at its
            // end and turns the other way.
            let (start, end, counter_clockwise) = match (reversed, direction) {
                (false, CurveDirection::CounterClockwise) => (start, end, true),
                (false, CurveDirection::Clockwise) => (start, end, false),
                (true, CurveDirection::CounterClockwise) => (end, start, false),
                (true, CurveDirection::Clockwise) => (end, start, true),
            };
            let angle = |point: &SketchPoint2| (point.v - center.v).atan2(point.u - center.u);
            let turn = if counter_clockwise {
                (angle(end) - angle(start)).rem_euclid(TAU)
            } else {
                (angle(start) - angle(end)).rem_euclid(TAU)
            };
            let normal = Vector3::new(
                frame.u.y * frame.v.z - frame.u.z * frame.v.y,
                frame.u.z * frame.v.x - frame.u.x * frame.v.z,
                frame.u.x * frame.v.y - frame.u.y * frame.v.x,
            );
            let sign = if counter_clockwise { 1.0 } else { -1.0 };
            Ok(SweepSegment3::Arc {
                center: place(*center),
                start: place(*start),
                normal: Vector3::new(normal.x * sign, normal.y * sign, normal.z * sign),
                sweep: if turn == 0.0 { TAU } else { turn },
            })
        }
        EvaluatedCurve2::Bspline {
            control_points,
            degree,
            knots,
            weights,
        } => {
            if weights.is_some() {
                return Err(SketchSweepError::RationalPathSpline { entity });
            }
            let mut points = control_points
                .iter()
                .map(|point| place(*point))
                .collect::<Vec<_>>();
            let mut knots = knots.clone();
            if reversed {
                // The same curve run backwards: its points reversed, and its
                // knots mirrored about the middle of their span.
                points.reverse();
                let (first, last) = (knots[0], knots[knots.len() - 1]);
                knots = knots.iter().rev().map(|knot| first + last - knot).collect();
            }
            Ok(SweepSegment3::Spline {
                degree: u32::try_from(*degree).unwrap_or(u32::MAX),
                knots,
                points,
            })
        }
        EvaluatedCurve2::Circle { .. } => {
            Err(SketchSweepError::Path(ChainError::ClosedCurve { entity }))
        }
    }
}

/// Why a sweep recipe cannot be kept or replayed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SketchSweepError {
    #[error(
        "unsupported sweep recipe version {found}; this build supports {CURRENT_SKETCH_SWEEP_RECIPE_VERSION}"
    )]
    UnsupportedVersion { found: u32 },
    #[error("a sweep requires non-zero profile and path sketches")]
    InvalidSketch,
    #[error("a sweep takes at least one profile region")]
    NoRegions,
    #[error("a sweep takes at most {limit} profile regions")]
    TooManyRegions { limit: usize },
    #[error("a sweep's profile regions must be sorted and unique")]
    NonCanonicalRegions,
    #[error("a sweep's path must be drawn in a sketch other than its profile's")]
    PathInProfileSketch,
    #[error("a sweep's path names no curves")]
    EmptyPath,
    #[error("a sweep's path names at most {limit} curves")]
    TooManyPathCurves { limit: usize },
    #[error("the sweep's path sketch {0} is no longer in the document")]
    MissingPathSketch(SketchId),
    #[error("the sweep's path is not one open, smooth chain: {0}")]
    Path(ChainError),
    #[error("path curve {entity} is a rational spline, which a sweep cannot follow yet")]
    RationalPathSpline { entity: SketchEntityId },
    #[error("an add or cut sweep names the body it changes as its input")]
    MissingTargetBody,
}

#[cfg(test)]
mod tests {
    use artificer_sketch::RegionSignature;

    use super::*;

    fn xz() -> PlanarFrame3 {
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        )
    }

    fn entity(id: u64) -> SketchEntityId {
        serde_json::from_value(serde_json::json!(id)).expect("an entity id")
    }

    #[test]
    fn a_recipe_needs_a_profile_and_a_path_in_another_sketch() {
        let profile = |sketch: u64, regions: Vec<RegionSignature>| SketchLoftSection {
            sketch: SketchId::from_allocated(sketch),
            regions,
        };
        let path = |sketch: u64, entities: Vec<SketchEntityId>| SweepPath {
            sketch: SketchId::from_allocated(sketch),
            entities,
            reversed: false,
        };
        let new = |profile, path| {
            SketchSweep::new(
                profile,
                path,
                SweepOrientation::RotationMinimising,
                SolidOperation::New,
            )
        };
        assert_eq!(
            new(profile(1, Vec::new()), path(2, vec![entity(1)])),
            Err(SketchSweepError::NoRegions)
        );
        let json = serde_json::json!({
            "profile": { "sketch": 1, "regions": [] },
            "path": { "sketch": 1, "entities": [3] },
            "operation": "new",
        });
        let decoded: SketchSweep = serde_json::from_value(json).expect("decodes");
        assert_eq!(decoded.orientation, SweepOrientation::RotationMinimising);
        assert_eq!(decoded.version, CURRENT_SKETCH_SWEEP_RECIPE_VERSION);
        let mut same_sketch = decoded;
        same_sketch.profile.regions = vec![RegionSignature {
            outer: Vec::new(),
            holes: Vec::new(),
        }];
        assert_eq!(
            same_sketch.validate(),
            Err(SketchSweepError::PathInProfileSketch)
        );
        same_sketch.path.sketch = SketchId::from_allocated(2);
        same_sketch.path.entities.clear();
        assert_eq!(same_sketch.validate(), Err(SketchSweepError::EmptyPath));
    }

    /// An arc run backwards starts at its end and turns the other way; a
    /// spline run backwards keeps its shape.
    #[test]
    fn a_reversed_curve_is_placed_running_the_other_way() {
        let arc = EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(0.0, 0.0),
            start: SketchPoint2::new(2.0, 0.0),
            end: SketchPoint2::new(0.0, 2.0),
            direction: CurveDirection::CounterClockwise,
        };
        let forward = place_curve(&arc, false, xz(), entity(1)).expect("placed");
        let backward = place_curve(&arc, true, xz(), entity(1)).expect("placed");
        let SweepSegment3::Arc {
            start,
            normal,
            sweep,
            ..
        } = forward
        else {
            panic!("an arc");
        };
        assert_eq!(start, Point3::new(2.0, 0.0, 0.0));
        // Anticlockwise in the XZ sketch is about the sketch normal, -Y.
        assert_eq!(normal, Vector3::new(0.0, -1.0, 0.0));
        assert!((sweep - std::f64::consts::FRAC_PI_2).abs() < 1.0e-12);
        let SweepSegment3::Arc {
            start,
            normal,
            sweep,
            ..
        } = backward
        else {
            panic!("an arc");
        };
        assert_eq!(start, Point3::new(0.0, 0.0, 2.0));
        assert_eq!(normal, Vector3::new(0.0, 1.0, 0.0));
        assert!((sweep - std::f64::consts::FRAC_PI_2).abs() < 1.0e-12);

        let spline = EvaluatedCurve2::Bspline {
            control_points: vec![
                SketchPoint2::new(0.0, 0.0),
                SketchPoint2::new(1.0, 2.0),
                SketchPoint2::new(3.0, 2.0),
                SketchPoint2::new(4.0, 0.0),
            ],
            degree: 3,
            knots: vec![0.0, 0.0, 0.0, 0.0, 2.0, 2.0, 2.0, 2.0],
            weights: None,
        };
        let SweepSegment3::Spline { points, knots, .. } =
            place_curve(&spline, true, xz(), entity(2)).expect("placed")
        else {
            panic!("a spline");
        };
        assert_eq!(points[0], Point3::new(4.0, 0.0, 0.0));
        assert_eq!(points[3], Point3::new(0.0, 0.0, 0.0));
        assert_eq!(knots, vec![0.0, 0.0, 0.0, 0.0, 2.0, 2.0, 2.0, 2.0]);
    }
}

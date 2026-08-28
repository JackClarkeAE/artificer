//! Late-bound sketch-region feature recipes.
//!
//! A modeling feature owns the regions it consumes. The source sketch keeps
//! only authoring intent and an optional derived profile cache; it never owns a
//! global "selected profile". Rebuild resolves these signatures against the
//! current analytic arrangement and compiles a fresh exact kernel profile.

use artificer_protocol::{
    ArcDirection, EntityId, EntityKind, EntityRef, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarProfile2, Point2 as ProtocolPoint2, PrecisionPolicy,
    SnapshotId, Vector3,
};
use artificer_sketch::{
    ArrangementLimits, ProfileCompileError, RegionSignature, SketchValidationError,
    build_arrangement, compile_selected_profile,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::persistent::{
    CURRENT_PERSISTENT_REF_VERSION, MAX_PERSISTENT_LINEAGE_DEPTH, PersistentRef, TargetedKernel,
};
use crate::{ModelDocument, ReplayAction, SketchId};

/// Schema written for newly-created sketch-region replay recipes.
pub const CURRENT_SKETCH_REGION_RECIPE_VERSION: u32 = 1;

/// Defensive ceiling matching the exact planar-profile region limit.
pub const MAX_SELECTED_SKETCH_REGIONS: usize = 32;

const fn current_sketch_region_recipe_version() -> u32 {
    CURRENT_SKETCH_REGION_RECIPE_VERSION
}

/// Whether a signed feature distance means "build on the other side of the
/// sketch plane".
///
/// The recipe's distance carries direction in its sign, but `KernelCommand`
/// depths are positive by protocol invariant, so the direction has to be
/// re-expressed as a reversed frame. The convention is the one the workbench
/// panel states: positive adds, negative cuts. A Cut therefore travels into the
/// material by default and reverses when asked for a positive distance, while
/// New body and Add travel along the frame normal and reverse when negative.
#[must_use]
pub fn extrusion_frame_is_reversed(
    operation: Option<FaceExtrusionOperation>,
    distance: f64,
) -> bool {
    match operation {
        Some(FaceExtrusionOperation::Cut) => distance > 0.0,
        _ => distance < 0.0,
    }
}

/// Reverses the frame normal while reflecting profile coordinates to match, so
/// the physical sketch wires stay exactly where they were drawn.
///
/// Negating `v` alone would mirror the profile about the frame's u axis; the
/// matching reflection of every curve's v coordinate (and of each arc's sense)
/// puts every point back on the plane where the user drew it, leaving only the
/// normal flipped. This is what keeps the protocol's positive-depth invariant
/// independent of which way a feature grows.
#[must_use]
pub fn reversed_extrusion_direction(
    mut frame: PlanarFrame3,
    profile: PlanarProfile2,
) -> (PlanarFrame3, PlanarProfile2) {
    frame.v = Vector3::new(-frame.v.x, -frame.v.y, -frame.v.z);
    (frame, reflected_profile_across_u(profile))
}

/// Reflects every profile coordinate about the frame's u axis.
///
/// This is the profile half of [`reversed_extrusion_direction`], exposed on its
/// own because it is an involution: applying it to a reversed command's profile
/// recovers the profile as the sketch actually holds it, which is what a region
/// signature has to be matched against.
#[must_use]
pub fn reflected_profile_across_u(mut profile: PlanarProfile2) -> PlanarProfile2 {
    for curve in profile
        .regions
        .iter_mut()
        .flat_map(|region| std::iter::once(&mut region.outer).chain(&mut region.holes))
        .flat_map(|profile_loop| &mut profile_loop.curves)
    {
        let reflected = match curve {
            PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
                start: reflect_profile_point(*start),
                end: reflect_profile_point(*end),
            },
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => PlanarCurve2::CircularArc {
                center: reflect_profile_point(*center),
                start: reflect_profile_point(*start),
                end: reflect_profile_point(*end),
                direction: reverse_arc_direction(*direction),
            },
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            } => PlanarCurve2::Circle {
                center: reflect_profile_point(*center),
                radius: *radius,
                direction: reverse_arc_direction(*direction),
            },
            PlanarCurve2::Bspline {
                control_points,
                degree,
                knots,
                weights,
            } => PlanarCurve2::Bspline {
                control_points: control_points.iter().map(|p| reflect_profile_point(*p)).collect(),
                degree: *degree,
                knots: knots.clone(),
                weights: weights.clone(),
            },
        };
        *curve = reflected;
    }
    profile
}

const fn reflect_profile_point(point: ProtocolPoint2) -> ProtocolPoint2 {
    ProtocolPoint2::new(point.x, -point.y)
}

const fn reverse_arc_direction(direction: ArcDirection) -> ArcDirection {
    match direction {
        ArcDirection::CounterClockwise => ArcDirection::Clockwise,
        ArcDirection::Clockwise => ArcDirection::CounterClockwise,
    }
}

/// Where a compiled sketch profile is applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SketchRegionExtrusionTarget {
    /// Create an independent solid from the sketch frame.
    NewBody,
    /// Add to or cut from one persistent planar face.
    PlanarFace {
        face: PersistentRef,
        operation: FaceExtrusionOperation,
    },
}

/// Serializable, exact, late-bound profile-feature intent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchRegionExtrusion {
    #[serde(default = "current_sketch_region_recipe_version")]
    pub version: u32,
    pub sketch: SketchId,
    pub regions: Vec<RegionSignature>,
    pub target: SketchRegionExtrusionTarget,
    pub distance: f64,
}

impl SketchRegionExtrusion {
    /// Creates a standalone extrusion recipe with a canonical region set.
    pub fn new_body(
        sketch: SketchId,
        regions: Vec<RegionSignature>,
        distance: f64,
    ) -> Result<Self, SketchRegionRecipeError> {
        Self::new(
            sketch,
            regions,
            SketchRegionExtrusionTarget::NewBody,
            distance,
        )
    }

    /// Creates a face add/cut recipe with a canonical region set.
    pub fn on_face(
        sketch: SketchId,
        regions: Vec<RegionSignature>,
        face: PersistentRef,
        operation: FaceExtrusionOperation,
        distance: f64,
    ) -> Result<Self, SketchRegionRecipeError> {
        Self::new(
            sketch,
            regions,
            SketchRegionExtrusionTarget::PlanarFace { face, operation },
            distance,
        )
    }

    fn new(
        sketch: SketchId,
        mut regions: Vec<RegionSignature>,
        target: SketchRegionExtrusionTarget,
        distance: f64,
    ) -> Result<Self, SketchRegionRecipeError> {
        regions.sort();
        regions.dedup();
        let recipe = Self {
            version: CURRENT_SKETCH_REGION_RECIPE_VERSION,
            sketch,
            regions,
            target,
            distance,
        };
        recipe.validate()?;
        Ok(recipe)
    }

    /// Validates only persisted recipe structure. Geometry is deliberately
    /// resolved later from the current sketch revision.
    pub fn validate(&self) -> Result<(), SketchRegionRecipeError> {
        if self.version != CURRENT_SKETCH_REGION_RECIPE_VERSION {
            return Err(SketchRegionRecipeError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.sketch.get() == 0 {
            return Err(SketchRegionRecipeError::InvalidSketch);
        }
        if self.regions.is_empty() {
            return Err(SketchRegionRecipeError::EmptySelection);
        }
        if self.regions.len() > MAX_SELECTED_SKETCH_REGIONS {
            return Err(SketchRegionRecipeError::TooManyRegions {
                actual: self.regions.len(),
                limit: MAX_SELECTED_SKETCH_REGIONS,
            });
        }
        if self.regions.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(SketchRegionRecipeError::NonCanonicalSelection);
        }
        // The sign is direction, not magnitude: it says which side of the
        // sketch plane the material goes. Only zero and non-finite are
        // structurally invalid. See [`extrusion_frame_is_reversed`].
        if !self.distance.is_finite() || self.distance == 0.0 {
            return Err(SketchRegionRecipeError::InvalidDistance);
        }
        if let SketchRegionExtrusionTarget::PlanarFace { face, .. } = &self.target {
            validate_face_reference(face, 0)?;
        }
        Ok(())
    }

    /// Resolves the current sketch revision and produces an ordinary replay
    /// action for the existing kernel command paths.
    pub fn resolve(
        &self,
        document: &ModelDocument,
        precision: PrecisionPolicy,
    ) -> Result<ReplayAction, SketchRegionResolveError> {
        self.validate()
            .map_err(SketchRegionResolveError::InvalidRecipe)?;
        let sketch = document
            .sketch(self.sketch)
            .ok_or(SketchRegionResolveError::UnknownSketch(self.sketch))?;
        let payload = document
            .sketch_payload(self.sketch, sketch.geometry_revision)
            .ok_or(SketchRegionResolveError::MissingSketchPayload {
                sketch: self.sketch,
                geometry_revision: sketch.geometry_revision,
            })?;
        let authoring =
            payload
                .authoring()
                .ok_or(SketchRegionResolveError::MissingAuthoringDefinition {
                    sketch: self.sketch,
                    geometry_revision: sketch.geometry_revision,
                })?;
        authoring.validate(precision).map_err(|error| {
            SketchRegionResolveError::InvalidAuthoringDefinition {
                sketch: self.sketch,
                error,
            }
        })?;
        let inputs = authoring.arrangement_inputs().map_err(|error| {
            SketchRegionResolveError::InvalidAuthoringDefinition {
                sketch: self.sketch,
                error,
            }
        })?;
        let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());

        // Do not let `cell()` silently choose the first entry if a corrupt or
        // future arrangement implementation ever emits duplicate signatures.
        for signature in &self.regions {
            match arrangement
                .cells
                .iter()
                .filter(|cell| &cell.signature == signature)
                .count()
            {
                0 => {
                    return Err(SketchRegionResolveError::MissingRegion {
                        sketch: self.sketch,
                        signature: signature.clone(),
                    });
                }
                1 => {}
                count => {
                    return Err(SketchRegionResolveError::AmbiguousRegion {
                        sketch: self.sketch,
                        signature: signature.clone(),
                        candidates: count,
                    });
                }
            }
        }

        let compiled = compile_selected_profile(&arrangement, &self.regions, &precision)
            .map_err(SketchRegionResolveError::Profile)?;
        // Replay must reconstruct the same solid the feature first built, so
        // the sign is re-expressed here exactly as it was when the command was
        // issued: a reversed frame plus a positive depth.
        let operation = match &self.target {
            SketchRegionExtrusionTarget::NewBody => None,
            SketchRegionExtrusionTarget::PlanarFace { operation, .. } => Some(*operation),
        };
        let (frame, profile) = if extrusion_frame_is_reversed(operation, self.distance) {
            reversed_extrusion_direction(payload.frame, compiled.profile)
        } else {
            (payload.frame, compiled.profile)
        };
        let distance = self.distance.abs();
        let command = match operation {
            None => KernelCommand::ExtrudePlanarProfile {
                frame,
                profile,
                distance,
            },
            Some(operation) => KernelCommand::ExtrudeFacePlanarProfile {
                // Serialization placeholder only. `TargetedKernel::rebind`
                // overwrites this value before execution.
                target_face: EntityRef {
                    snapshot: SnapshotId::ZERO,
                    entity: EntityId(0),
                    kind: EntityKind::Face,
                },
                frame,
                profile,
                distance,
                operation,
            },
        };
        match &self.target {
            SketchRegionExtrusionTarget::NewBody => Ok(ReplayAction::Kernel(command)),
            SketchRegionExtrusionTarget::PlanarFace { face, .. } => {
                TargetedKernel::new(command, face.clone())
                    .map(ReplayAction::TargetedKernel)
                    .map_err(|_| {
                        SketchRegionResolveError::InvalidRecipe(
                            SketchRegionRecipeError::InvalidFaceTarget,
                        )
                    })
            }
        }
    }
}

/// Structural recipe rejection detected without evaluating geometry.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SketchRegionRecipeError {
    #[error(
        "unsupported sketch-region recipe version {found}; this build supports {CURRENT_SKETCH_REGION_RECIPE_VERSION}"
    )]
    UnsupportedVersion { found: u32 },
    #[error("a sketch-region feature requires a non-zero source sketch")]
    InvalidSketch,
    #[error("a sketch-region feature must select at least one bounded region")]
    EmptySelection,
    #[error("selected sketch regions exceed the limit of {limit}: {actual}")]
    TooManyRegions { actual: usize, limit: usize },
    #[error("selected sketch regions must be sorted and unique")]
    NonCanonicalSelection,
    #[error("sketch-region extrusion distance must be finite and non-zero")]
    InvalidDistance,
    #[error("a face sketch-region feature requires a valid persistent face target")]
    InvalidFaceTarget,
    #[error("persistent face lineage exceeds the depth limit of {limit}")]
    FaceLineageTooDeep { limit: usize },
}

/// Failure while resolving current sketch geometry during rebuild.
#[derive(Clone, Debug, PartialEq, Error)]
pub enum SketchRegionResolveError {
    #[error("invalid sketch-region replay recipe: {0}")]
    InvalidRecipe(SketchRegionRecipeError),
    #[error("sketch-region replay references unknown sketch {0}")]
    UnknownSketch(SketchId),
    #[error("sketch {sketch} revision {geometry_revision} has no exact payload")]
    MissingSketchPayload {
        sketch: SketchId,
        geometry_revision: u64,
    },
    #[error("sketch {sketch} revision {geometry_revision} has no editable authoring graph")]
    MissingAuthoringDefinition {
        sketch: SketchId,
        geometry_revision: u64,
    },
    #[error("sketch {sketch} authoring graph is invalid: {error}")]
    InvalidAuthoringDefinition {
        sketch: SketchId,
        error: SketchValidationError,
    },
    #[error("a selected region no longer resolves in sketch {sketch}")]
    MissingRegion {
        sketch: SketchId,
        signature: RegionSignature,
    },
    #[error(
        "a selected region resolves to {candidates} candidates in sketch {sketch}; repair is required"
    )]
    AmbiguousRegion {
        sketch: SketchId,
        signature: RegionSignature,
        candidates: usize,
    },
    #[error("the selected sketch regions could not compile: {0}")]
    Profile(ProfileCompileError),
}

fn validate_face_reference(
    reference: &PersistentRef,
    depth: usize,
) -> Result<(), SketchRegionRecipeError> {
    if depth >= MAX_PERSISTENT_LINEAGE_DEPTH {
        return Err(SketchRegionRecipeError::FaceLineageTooDeep {
            limit: MAX_PERSISTENT_LINEAGE_DEPTH,
        });
    }
    if reference.version != CURRENT_PERSISTENT_REF_VERSION
        || reference.producer.get() == 0
        || reference.kind != EntityKind::Face
    {
        return Err(SketchRegionRecipeError::InvalidFaceTarget);
    }
    if let Some(lineage) = &reference.lineage {
        validate_face_reference(lineage, depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        KernelCommand, PlanarFrame3, Point3, PrecisionPolicy, SemanticDigest, SnapshotId, Vector3,
    };
    use artificer_sketch::{
        ConfirmationSource, PointInput, SignedLength, SketchDefinition, SketchPoint2, SketchRecipe,
        SketchValue,
    };

    use super::*;
    use crate::{
        FeatureDraft, FeatureInput, FeatureKind, OutputDraft, RebuildState, SketchPayload,
        SketchSupportRecipe, SnapshotAssociation,
    };

    fn frame() -> PlanarFrame3 {
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        )
    }

    fn rectangle(width: f64, height: f64) -> SketchDefinition {
        let mut definition = SketchDefinition::new();
        let transaction = definition
            .stage(
                SketchRecipe::TwoPointRectangle {
                    first_corner: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
                    width: SketchValue::Literal(SignedLength::new(width).unwrap()),
                    height: SketchValue::Literal(SignedLength::new(height).unwrap()),
                },
                "Rectangle",
            )
            .unwrap();
        definition
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
        definition
    }

    fn selected_profile(
        definition: &SketchDefinition,
    ) -> (Vec<RegionSignature>, artificer_protocol::PlanarProfile2) {
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(
            &definition.arrangement_inputs().unwrap(),
            &precision,
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 1);
        let regions = vec![arrangement.cells[0].signature.clone()];
        let profile = compile_selected_profile(&arrangement, &regions, &precision)
            .unwrap()
            .profile;
        (regions, profile)
    }

    fn document_with_rectangle() -> (ModelDocument, SketchId, RegionSignature) {
        let definition = rectangle(2.0, 3.0);
        let (regions, profile) = selected_profile(&definition);
        let payload = SketchPayload::from_authoring(
            frame(),
            definition,
            Some(profile),
            SketchSupportRecipe::Origin,
        )
        .unwrap();
        let marker = SnapshotAssociation::new(
            SnapshotId::ZERO,
            SnapshotId::ZERO,
            SemanticDigest::new([0; 32]),
        );
        let mut document = ModelDocument::default();
        let appended = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker)
                    .with_sketch_payload(payload)
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 1".into(),
                        geometry_revision: 1,
                    })
                    .with_commit(marker),
            )
            .unwrap();
        (document, appended.created_sketches[0], regions[0].clone())
    }

    #[test]
    fn a_negative_distance_resolves_to_a_reversed_frame_and_a_positive_depth() {
        // The protocol keeps extrusion depths positive, so a feature that
        // grows the other way has to say so with its frame. If replay dropped
        // the sign here, a rebuild would silently move the body to the far
        // side of the sketch plane.
        let (document, sketch, signature) = document_with_rectangle();
        let precision = PrecisionPolicy::default();
        let upward = SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], 5.0).unwrap();
        let downward = SketchRegionExtrusion::new_body(sketch, vec![signature], -5.0).unwrap();

        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile {
            frame: up_frame,
            distance: up_distance,
            ..
        }) = upward.resolve(&document, precision).unwrap()
        else {
            panic!("a standalone region recipe resolves to a profile extrusion")
        };
        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile {
            frame: down_frame,
            distance: down_distance,
            ..
        }) = downward.resolve(&document, precision).unwrap()
        else {
            panic!("a standalone region recipe resolves to a profile extrusion")
        };

        assert_eq!(up_distance, 5.0);
        assert_eq!(
            down_distance, 5.0,
            "depth stays positive; the frame carries direction"
        );
        assert_eq!(down_frame.origin, up_frame.origin);
        assert_eq!(down_frame.u, up_frame.u);
        assert_eq!(
            down_frame.v,
            Vector3::new(-up_frame.v.x, -up_frame.v.y, -up_frame.v.z)
        );
    }

    #[test]
    fn a_zero_distance_is_the_only_invalid_magnitude() {
        let (_, sketch, signature) = document_with_rectangle();
        assert!(SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], -5.0).is_ok());
        assert_eq!(
            SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], 0.0).unwrap_err(),
            SketchRegionRecipeError::InvalidDistance
        );
        assert_eq!(
            SketchRegionExtrusion::new_body(sketch, vec![signature], f64::NAN).unwrap_err(),
            SketchRegionRecipeError::InvalidDistance
        );
    }

    #[test]
    fn the_direction_rule_matches_the_panel_it_is_written_from() {
        use artificer_protocol::FaceExtrusionOperation;
        // "positive adds, negative cuts": Add and New body travel along the
        // frame normal, a Cut travels into the material.
        assert!(!extrusion_frame_is_reversed(None, 5.0));
        assert!(extrusion_frame_is_reversed(None, -5.0));
        assert!(!extrusion_frame_is_reversed(
            Some(FaceExtrusionOperation::Add),
            5.0
        ));
        assert!(extrusion_frame_is_reversed(
            Some(FaceExtrusionOperation::Add),
            -5.0
        ));
        assert!(extrusion_frame_is_reversed(
            Some(FaceExtrusionOperation::Cut),
            5.0
        ));
        assert!(!extrusion_frame_is_reversed(
            Some(FaceExtrusionOperation::Cut),
            -5.0
        ));
    }

    #[test]
    fn reflecting_a_profile_twice_returns_it_unchanged() {
        // The workbench relies on this to recover the profile as drawn from a
        // command whose direction was already folded into its frame.
        let (document, sketch, signature) = document_with_rectangle();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], 5.0).unwrap();
        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile { profile, .. }) = recipe
            .resolve(&document, PrecisionPolicy::default())
            .unwrap()
        else {
            panic!("a standalone region recipe resolves to a profile extrusion")
        };
        assert_eq!(
            reflected_profile_across_u(reflected_profile_across_u(profile.clone())),
            profile
        );
    }

    #[test]
    fn recipe_round_trips_without_embedding_selection_in_sketch_payload() {
        let (mut document, sketch, signature) = document_with_rectangle();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], 5.0).unwrap();
        let appended = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Extrude,
                    "Extrude",
                    ReplayAction::SketchRegionExtrusion(recipe.clone()),
                )
                .with_input(FeatureInput::Sketch(sketch))
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".into(),
                }),
            )
            .unwrap();

        let json = serde_json::to_string(&document).unwrap();
        let restored = serde_json::from_str::<ModelDocument>(&json).unwrap();
        assert_eq!(
            restored.feature(appended.feature).unwrap().action,
            ReplayAction::SketchRegionExtrusion(recipe)
        );
        assert!(
            restored
                .sketch_payload(sketch, 1)
                .unwrap()
                .authoring()
                .is_some()
        );
    }

    #[test]
    fn upstream_authoring_edit_recompiles_profile_and_dirties_consumer() {
        let (mut document, sketch, signature) = document_with_rectangle();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], 5.0).unwrap();
        let extrusion = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Extrude,
                    "Extrude",
                    ReplayAction::SketchRegionExtrusion(recipe.clone()),
                )
                .with_input(FeatureInput::Sketch(sketch))
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".into(),
                }),
            )
            .unwrap()
            .feature;
        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile {
            profile: before, ..
        }) = recipe
            .resolve(&document, PrecisionPolicy::default())
            .unwrap()
        else {
            panic!("standalone region recipe should resolve to profile extrusion")
        };

        let edited = rectangle(4.0, 3.0);
        let (_, edited_profile) = selected_profile(&edited);
        document
            .replace_sketch_payload(
                sketch,
                SketchPayload::from_authoring(
                    frame(),
                    edited,
                    Some(edited_profile),
                    SketchSupportRecipe::Origin,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            document.feature(extrusion).unwrap().state.rebuild,
            RebuildState::Dirty
        );
        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile { profile: after, .. }) =
            recipe
                .resolve(&document, PrecisionPolicy::default())
                .unwrap()
        else {
            panic!("edited region recipe should still resolve")
        };
        assert_ne!(
            before, after,
            "the persisted profile cache must not be replay authority"
        );
    }

    #[test]
    fn unresolved_signature_fails_instead_of_retargeting_by_nearest_geometry() {
        let (mut document, sketch, signature) = document_with_rectangle();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], 5.0).unwrap();
        let circle = {
            use artificer_sketch::{Angle, Length};
            let mut definition = SketchDefinition::new();
            let transaction = definition
                .stage(
                    SketchRecipe::CentrePointCircle {
                        center: PointInput::Position(SketchPoint2::new(1.0, 1.0)),
                        radius: SketchValue::Literal(Length::new(1.0).unwrap()),
                        radial_angle: SketchValue::Literal(Angle::radians(0.0).unwrap()),
                    },
                    "Circle",
                )
                .unwrap();
            definition
                .commit(transaction, ConfirmationSource::GreenTick)
                .unwrap();
            definition
        };
        let (_, circle_profile) = selected_profile(&circle);
        document
            .replace_sketch_payload(
                sketch,
                SketchPayload::from_authoring(
                    frame(),
                    circle,
                    Some(circle_profile),
                    SketchSupportRecipe::Origin,
                )
                .unwrap(),
            )
            .unwrap();

        assert!(matches!(
            recipe.resolve(&document, PrecisionPolicy::default()),
            Err(SketchRegionResolveError::MissingRegion {
                sketch: missing_sketch,
                signature: missing_signature,
            }) if missing_sketch == sketch && missing_signature == signature
        ));
    }
}

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
    ArrangementLimits, ProfileCompileError, RegionSignature, SketchPoint2, SketchValidationError,
    build_arrangement, compile_selected_profile,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::persistent::{
    CURRENT_PERSISTENT_REF_VERSION, MAX_PERSISTENT_LINEAGE_DEPTH, PersistentRef, TargetedKernel,
};
use crate::{FeatureId, ModelDocument, ReplayAction, SketchId};

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
                control_points: control_points
                    .iter()
                    .map(|p| reflect_profile_point(*p))
                    .collect(),
                degree: *degree,
                knots: knots.clone(),
                weights: weights.clone(),
            },
        };
        *curve = reflected;
    }
    profile
}

/// The frame moved `offset` along its own normal, `u × v`: the direction a
/// positive depth sweeps. A two-sided extrusion moves its frame back by the
/// second side's length so one sweep covers both sides.
#[must_use]
pub fn frame_moved_along_normal(mut frame: PlanarFrame3, offset: f64) -> PlanarFrame3 {
    let (u, v) = (frame.u, frame.v);
    let normal = Vector3::new(
        u.y * v.z - u.z * v.y,
        u.z * v.x - u.x * v.z,
        u.x * v.y - u.y * v.x,
    );
    let length = (normal.x * normal.x + normal.y * normal.y + normal.z * normal.z).sqrt();
    if !length.is_finite() || length <= f64::EPSILON || !offset.is_finite() {
        return frame;
    }
    let scale = offset / length;
    frame.origin = artificer_protocol::Point3::new(
        frame.origin.x + normal.x * scale,
        frame.origin.y + normal.y * scale,
        frame.origin.z + normal.z * scale,
    );
    frame
}

/// A plane's height above a frame, measured along the frame's normal: how
/// far a side has to sweep to end at a face lying in that plane. `None`
/// when the plane is not parallel to the frame (within `angle_tolerance`
/// on the cosine) or anything is not finite. The sign says which side of
/// the frame the plane lies on.
#[must_use]
pub fn plane_height_above_frame(
    frame: PlanarFrame3,
    plane_origin: artificer_protocol::Point3,
    plane_normal: Vector3,
    angle_tolerance: f64,
) -> Option<f64> {
    let (u, v) = (frame.u, frame.v);
    let normal = Vector3::new(
        u.y * v.z - u.z * v.y,
        u.z * v.x - u.x * v.z,
        u.x * v.y - u.y * v.x,
    );
    let length = (normal.x * normal.x + normal.y * normal.y + normal.z * normal.z).sqrt();
    let plane_length = (plane_normal.x * plane_normal.x
        + plane_normal.y * plane_normal.y
        + plane_normal.z * plane_normal.z)
        .sqrt();
    if !length.is_finite()
        || length <= f64::EPSILON
        || !plane_length.is_finite()
        || plane_length <= f64::EPSILON
    {
        return None;
    }
    let cosine =
        (normal.x * plane_normal.x + normal.y * plane_normal.y + normal.z * plane_normal.z)
            / (length * plane_length);
    if !cosine.is_finite() || cosine.abs() < 1.0 - angle_tolerance {
        return None;
    }
    let offset = Vector3::new(
        plane_origin.x - frame.origin.x,
        plane_origin.y - frame.origin.y,
        plane_origin.z - frame.origin.z,
    );
    let height = (offset.x * normal.x + offset.y * normal.y + offset.z * normal.z) / length;
    height.is_finite().then_some(height)
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
    /// Draft angle in degrees: the walls lean outward by this much per unit
    /// of height when positive, inward when negative. Zero is a straight
    /// extrusion. A drafted new body replays as a loft to the profile's
    /// offset section, which is exact for straight edges and tangent arcs.
    /// Face features do not draft.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub draft_degrees: f64,
    /// The other side of the sketch plane, as a positive length: the feature
    /// then spans from `second_distance` behind the plane to `distance` in
    /// front of it. `None` is the one-sided extrusion every recipe was
    /// before 0.98.1. A face feature has no second side: it grows from its
    /// face, and the kernel holds the profile to that face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_distance: Option<f64>,
    /// The first side ends at this face instead of at `distance`, which then
    /// holds the length last measured to it; replay measures it again
    /// against the face as it then stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to_face: Option<PersistentRef>,
    /// The second side's face, as `up_to_face` is the first side's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_up_to_face: Option<PersistentRef>,
    /// The first side ends at this construction plane (ADR 0048). A side ends
    /// at a face or at a plane, never both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to_plane: Option<FeatureId>,
    /// The second side's plane, as `up_to_plane` is the first side's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_up_to_plane: Option<FeatureId>,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// The largest draft the recipe accepts, in degrees. Beyond it the offset
/// section outruns any plausible profile; the kernel would refuse the
/// collapse anyway, but a typo should fail at the recipe.
pub const MAX_DRAFT_DEGREES: f64 = 75.0;

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
            draft_degrees: 0.0,
            second_distance: None,
            up_to_face: None,
            second_up_to_face: None,
            up_to_plane: None,
            second_up_to_plane: None,
        };
        recipe.validate()?;
        Ok(recipe)
    }

    /// Gives the extrusion a second side, `second_distance` behind the
    /// sketch plane. Only a new body has one; a face feature is refused.
    pub fn with_second_side(
        mut self,
        second_distance: f64,
    ) -> Result<Self, SketchRegionRecipeError> {
        self.second_distance = Some(second_distance);
        self.validate()?;
        Ok(self)
    }

    /// Ends a side at a face rather than at a distance. The distances stay
    /// as the lengths last measured to those faces, so the recipe reads
    /// sensibly on its own; replay measures them again.
    pub fn with_up_to_faces(
        mut self,
        first: Option<PersistentRef>,
        second: Option<PersistentRef>,
    ) -> Result<Self, SketchRegionRecipeError> {
        self.up_to_face = first;
        self.second_up_to_face = second;
        self.validate()?;
        Ok(self)
    }

    /// Ends a side at a construction plane rather than at a distance.
    pub fn with_up_to_planes(
        mut self,
        first: Option<FeatureId>,
        second: Option<FeatureId>,
    ) -> Result<Self, SketchRegionRecipeError> {
        self.up_to_plane = first;
        self.second_up_to_plane = second;
        self.validate()?;
        Ok(self)
    }

    /// Whether a side ends at a face or a plane, which replay measures
    /// before resolving.
    #[must_use]
    pub const fn ends_at_a_face(&self) -> bool {
        self.up_to_face.is_some()
            || self.second_up_to_face.is_some()
            || self.up_to_plane.is_some()
            || self.second_up_to_plane.is_some()
    }

    /// The construction planes this recipe ends at, which it depends on.
    pub fn end_planes(&self) -> impl Iterator<Item = FeatureId> {
        [self.up_to_plane, self.second_up_to_plane]
            .into_iter()
            .flatten()
    }

    /// The recipe with the lengths a replay measured to its faces. A side
    /// that ends at a distance keeps its own, and the first side keeps its
    /// direction: the measurement is a length, the sign is which way.
    #[must_use]
    pub fn with_measured_distances(mut self, first: Option<f64>, second: Option<f64>) -> Self {
        if let Some(first) = first {
            self.distance = first.abs().copysign(self.distance);
        }
        if let (Some(second), Some(slot)) = (second, self.second_distance.as_mut()) {
            *slot = second.abs();
        }
        self
    }

    /// How far the kernel sweeps in all: both sides together.
    #[must_use]
    pub fn total_distance(&self) -> f64 {
        self.distance.abs() + self.second_distance.unwrap_or(0.0)
    }

    /// Sets the draft angle of a new-body extrusion. Face features cannot
    /// draft, and a draft outside `±MAX_DRAFT_DEGREES` is refused.
    pub fn with_draft(mut self, draft_degrees: f64) -> Result<Self, SketchRegionRecipeError> {
        self.draft_degrees = draft_degrees;
        self.validate()?;
        Ok(self)
    }

    /// Whether replay lofts this extrusion to an offset section.
    #[must_use]
    pub fn is_drafted(&self) -> bool {
        self.draft_degrees != 0.0
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
        if !self.draft_degrees.is_finite() || self.draft_degrees.abs() > MAX_DRAFT_DEGREES {
            return Err(SketchRegionRecipeError::InvalidDraft);
        }
        if let Some(second) = self.second_distance {
            if !second.is_finite() || second <= 0.0 {
                return Err(SketchRegionRecipeError::InvalidSecondDistance);
            }
            if matches!(self.target, SketchRegionExtrusionTarget::PlanarFace { .. }) {
                return Err(SketchRegionRecipeError::TwoSidedFaceFeature);
            }
            if self.draft_degrees != 0.0 {
                return Err(SketchRegionRecipeError::InvalidDraft);
            }
        } else if self.second_up_to_face.is_some() || self.second_up_to_plane.is_some() {
            return Err(SketchRegionRecipeError::SecondFaceWithoutSecondSide);
        }
        if (self.up_to_face.is_some() && self.up_to_plane.is_some())
            || (self.second_up_to_face.is_some() && self.second_up_to_plane.is_some())
        {
            return Err(SketchRegionRecipeError::FaceAndPlaneOnOneSide);
        }
        if self.end_planes().any(|plane| plane.get() == 0) {
            return Err(SketchRegionRecipeError::InvalidPlaneTarget);
        }
        for face in [&self.up_to_face, &self.second_up_to_face]
            .into_iter()
            .flatten()
        {
            validate_face_reference(face, 0)?;
        }
        if let SketchRegionExtrusionTarget::PlanarFace { face, .. } = &self.target {
            if self.draft_degrees != 0.0 {
                return Err(SketchRegionRecipeError::InvalidDraft);
            }
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
        self.resolve_in_frame(document, precision, None)
    }

    /// Resolves as [`Self::resolve`] does, with the sketch placed in `frame`
    /// when one is given. A rebuild passes the frame it has just resolved for
    /// a construction plane, which the document's cache does not hold yet;
    /// without one, the sketch sits where the document last placed it.
    pub fn resolve_in_frame(
        &self,
        document: &ModelDocument,
        precision: PrecisionPolicy,
        frame: Option<PlanarFrame3>,
    ) -> Result<ReplayAction, SketchRegionResolveError> {
        self.validate()
            .map_err(SketchRegionResolveError::InvalidRecipe)?;
        let (profile, drawn_frame) =
            compile_sketch_regions(document, self.sketch, &self.regions, precision)?;
        // Replay must reconstruct the same solid the feature first built, so
        // the sign is re-expressed here exactly as it was when the command was
        // issued: a reversed frame plus a positive depth.
        let operation = match &self.target {
            SketchRegionExtrusionTarget::NewBody => None,
            SketchRegionExtrusionTarget::PlanarFace { operation, .. } => Some(*operation),
        };
        let placed = frame
            .or_else(|| document.sketch_frame(self.sketch))
            .unwrap_or(drawn_frame);
        let (frame, profile) = if extrusion_frame_is_reversed(operation, self.distance) {
            reversed_extrusion_direction(placed, profile)
        } else {
            (placed, profile)
        };
        let distance = self.distance.abs();
        // A second side starts the sweep behind the plane: the frame moves
        // back along its own normal and the depth covers both sides.
        let (frame, distance) = match self.second_distance {
            Some(second) => (frame_moved_along_normal(frame, -second), distance + second),
            None => (frame, distance),
        };
        let command = match operation {
            None if self.is_drafted() => KernelCommand::LoftPlanarProfileOffset {
                frame,
                profile,
                distance,
                offset: distance * self.draft_degrees.to_radians().tan(),
            },
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
    #[error(
        "a draft angle must be finite, within ±{MAX_DRAFT_DEGREES} degrees, and zero for a face feature"
    )]
    InvalidDraft,
    #[error("a face sketch-region feature requires a valid persistent face target")]
    InvalidFaceTarget,
    #[error("a second side must be a finite, positive length")]
    InvalidSecondDistance,
    #[error("a feature on a face grows from that face and has no second side")]
    TwoSidedFaceFeature,
    #[error("a second-side face needs a second side")]
    SecondFaceWithoutSecondSide,
    #[error("a side ends at a face or at a plane, not both")]
    FaceAndPlaneOnOneSide,
    #[error("a side's end plane must name a construction-plane feature")]
    InvalidPlaneTarget,
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
    #[error("invalid loft recipe: {0}")]
    InvalidLoft(crate::loft::SketchLoftError),
}

/// Compiles the named regions of a sketch's current authoring graph into an
/// exact kernel profile, and returns it with the frame the sketch was drawn
/// in.
///
/// Every signature must name exactly one bounded cell: a region that has gone
/// is refused, and so is one that now names two, rather than letting the
/// arrangement's first match stand in for it. Extrusions and lofts both read
/// their regions through this, so they fail the same way on the same sketch.
pub(crate) fn compile_sketch_regions(
    document: &ModelDocument,
    sketch_id: SketchId,
    regions: &[RegionSignature],
    precision: PrecisionPolicy,
) -> Result<(PlanarProfile2, PlanarFrame3), SketchRegionResolveError> {
    let sketch = document
        .sketch(sketch_id)
        .ok_or(SketchRegionResolveError::UnknownSketch(sketch_id))?;
    let payload = document
        .sketch_payload(sketch_id, sketch.geometry_revision)
        .ok_or(SketchRegionResolveError::MissingSketchPayload {
            sketch: sketch_id,
            geometry_revision: sketch.geometry_revision,
        })?;
    let authoring =
        payload
            .authoring()
            .ok_or(SketchRegionResolveError::MissingAuthoringDefinition {
                sketch: sketch_id,
                geometry_revision: sketch.geometry_revision,
            })?;
    authoring.validate(precision).map_err(|error| {
        SketchRegionResolveError::InvalidAuthoringDefinition {
            sketch: sketch_id,
            error,
        }
    })?;
    let inputs = authoring.arrangement_inputs().map_err(|error| {
        SketchRegionResolveError::InvalidAuthoringDefinition {
            sketch: sketch_id,
            error,
        }
    })?;
    let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());

    // Do not let `cell()` silently choose the first entry if a corrupt or
    // future arrangement implementation ever emits duplicate signatures.
    for signature in regions {
        match arrangement
            .cells
            .iter()
            .filter(|cell| &cell.signature == signature)
            .count()
        {
            0 => {
                return Err(SketchRegionResolveError::MissingRegion {
                    sketch: sketch_id,
                    signature: signature.clone(),
                });
            }
            1 => {}
            count => {
                return Err(SketchRegionResolveError::AmbiguousRegion {
                    sketch: sketch_id,
                    signature: signature.clone(),
                    candidates: count,
                });
            }
        }
    }

    let compiled = compile_selected_profile(&arrangement, regions, &precision)
        .map_err(SketchRegionResolveError::Profile)?;
    Ok((compiled.profile, payload.frame))
}

/// The region of a sketch, as the sketch now stands, that contains `point`
/// (in the sketch's own coordinates): the smallest bounded cell around it.
///
/// This is how a pick in the model view becomes a region a feature can name.
/// `None` when the sketch has no editable graph or the point is in no cell.
#[must_use]
pub fn sketch_region_at(
    document: &ModelDocument,
    sketch_id: SketchId,
    point: [f64; 2],
    precision: PrecisionPolicy,
) -> Option<RegionSignature> {
    let sketch = document.sketch(sketch_id)?;
    let payload = document.sketch_payload(sketch_id, sketch.geometry_revision)?;
    let inputs = payload.authoring()?.arrangement_inputs().ok()?;
    let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());
    arrangement
        .cell_at_point(SketchPoint2::new(point[0], point[1]), &precision)
        .map(|cell| cell.signature.clone())
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

    /// A two-sided new body sweeps once, from behind the plane to in front
    /// of it: the frame moves back by the second side and the depth is the
    /// two sides together. A reversed first side moves the reversed frame
    /// back along its own normal, so the solid still spans the same slab.
    #[test]
    fn a_second_side_moves_the_frame_back_and_sweeps_both_sides() {
        let (document, sketch, signature) = document_with_rectangle();
        let precision = PrecisionPolicy::default();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], 5.0)
            .unwrap()
            .with_second_side(2.0)
            .unwrap();
        assert_eq!(recipe.total_distance(), 7.0);
        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile {
            frame, distance, ..
        }) = recipe.resolve(&document, precision).unwrap()
        else {
            panic!("a two-sided new body is still one planar extrusion");
        };
        assert!((distance - 7.0).abs() < 1.0e-12);
        assert!((frame.origin.z + 2.0).abs() < 1.0e-12, "{frame:?}");

        let reversed = SketchRegionExtrusion::new_body(sketch, vec![signature], -5.0)
            .unwrap()
            .with_second_side(2.0)
            .unwrap();
        let ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile {
            frame, distance, ..
        }) = reversed.resolve(&document, precision).unwrap()
        else {
            panic!("a reversed two-sided new body is still one planar extrusion");
        };
        assert!((distance - 7.0).abs() < 1.0e-12);
        // The reversed frame sweeps -Z, so "back" is +Z.
        assert!((frame.origin.z - 2.0).abs() < 1.0e-12, "{frame:?}");
    }

    #[test]
    fn a_second_side_is_refused_where_it_cannot_be_built() {
        let (_, sketch, signature) = document_with_rectangle();
        let new_body =
            SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], 5.0).unwrap();
        assert_eq!(
            new_body.clone().with_second_side(0.0).unwrap_err(),
            SketchRegionRecipeError::InvalidSecondDistance
        );
        assert_eq!(
            new_body
                .clone()
                .with_draft(10.0)
                .unwrap()
                .with_second_side(1.0)
                .unwrap_err(),
            SketchRegionRecipeError::InvalidDraft
        );
        let face = PersistentRef::new(
            crate::FeatureId::from_allocated(7),
            artificer_protocol::OperationRole::new("base.entity", None),
            EntityKind::Face,
        );
        let on_face = SketchRegionExtrusion::on_face(
            sketch,
            vec![signature],
            face.clone(),
            FaceExtrusionOperation::Cut,
            5.0,
        )
        .unwrap();
        assert_eq!(
            on_face.with_second_side(1.0).unwrap_err(),
            SketchRegionRecipeError::TwoSidedFaceFeature
        );
        assert_eq!(
            new_body.with_up_to_faces(None, Some(face)).unwrap_err(),
            SketchRegionRecipeError::SecondFaceWithoutSecondSide
        );
    }

    #[test]
    fn measured_distances_keep_the_direction_and_a_frame_moves_along_its_normal() {
        let (_, sketch, signature) = document_with_rectangle();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], -5.0)
            .unwrap()
            .with_second_side(1.0)
            .unwrap()
            .with_measured_distances(Some(8.0), Some(3.0));
        assert_eq!(recipe.distance, -8.0);
        assert_eq!(recipe.second_distance, Some(3.0));

        let moved = frame_moved_along_normal(frame(), -2.5);
        assert!((moved.origin.z + 2.5).abs() < 1.0e-12);
        let height = plane_height_above_frame(
            frame(),
            Point3::new(4.0, 4.0, 6.5),
            Vector3::new(0.0, 0.0, -1.0),
            1.0e-9,
        );
        assert_eq!(height, Some(6.5));
        assert_eq!(
            plane_height_above_frame(
                frame(),
                Point3::new(0.0, 0.0, 6.5),
                Vector3::new(1.0, 0.0, 0.0),
                1.0e-9,
            ),
            None,
            "a face that is not parallel cannot end a sweep"
        );
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

    /// A document with a construction plane 10 above XY and a rectangle
    /// sketched on it, as the workbench builds one (ADR 0048).
    fn document_with_plane_sketch() -> (ModelDocument, crate::FeatureId, SketchId, RegionSignature)
    {
        use crate::datum::{DatumPlaneBase, DatumPlaneRecipe, OriginPlane, ResolvedDatumPlane};
        let marker = SnapshotAssociation::new(
            SnapshotId::ZERO,
            SnapshotId::ZERO,
            SemanticDigest::new([0; 32]),
        );
        let mut recipe = DatumPlaneRecipe::new(
            DatumPlaneBase::Origin {
                plane: OriginPlane::Xy,
            },
            ResolvedDatumPlane {
                frame: crate::datum::offset_along_normal(OriginPlane::Xy.frame(), 10.0),
                half_extent: [25.0, 25.0],
            },
        );
        recipe.offset = 10.0;
        let mut document = ModelDocument::default();
        let plane = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::DatumPlane,
                    "Plane 1",
                    ReplayAction::DatumPlane(recipe.clone()),
                )
                .with_commit(marker),
            )
            .unwrap()
            .feature;
        let definition = rectangle(2.0, 3.0);
        let (regions, profile) = selected_profile(&definition);
        let payload = SketchPayload::from_authoring(
            recipe.frame,
            definition,
            Some(profile),
            SketchSupportRecipe::DatumPlane { plane },
        )
        .unwrap();
        let sketch = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker)
                    .with_input(FeatureInput::Feature(plane))
                    .with_sketch_payload(payload)
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 1".into(),
                        geometry_revision: 1,
                    })
                    .with_commit(marker),
            )
            .unwrap()
            .created_sketches[0];
        (document, plane, sketch, regions[0].clone())
    }

    fn extruded_origin(action: ReplayAction) -> Point3 {
        match action {
            ReplayAction::Kernel(KernelCommand::ExtrudePlanarProfile { frame, .. }) => frame.origin,
            other => panic!("a new-body region recipe resolves to a profile extrusion: {other:?}"),
        }
    }

    #[test]
    fn a_sketch_on_a_plane_is_replayed_where_the_plane_now_is() {
        let (document, plane, sketch, signature) = document_with_plane_sketch();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], 5.0).unwrap();
        let action = ReplayAction::SketchRegionExtrusion(recipe);
        // Without a live frame the sketch sits on the plane's cached frame.
        let cached = action
            .resolve_sketch_regions(&document, PrecisionPolicy::default())
            .unwrap();
        assert!((extruded_origin(cached).z - 10.0).abs() < 1.0e-12);
        // A rebuild that has just resolved the plane higher up passes that
        // frame, and the extrusion follows it before any cache is refreshed.
        let moved = crate::datum::ResolvedDatumPlane {
            frame: crate::datum::offset_along_normal(crate::datum::OriginPlane::Xy.frame(), 30.0),
            half_extent: [25.0, 25.0],
        };
        let live = action
            .resolve_sketch_regions_with_planes(
                &document,
                PrecisionPolicy::default(),
                &std::collections::BTreeMap::from([(plane, moved)]),
            )
            .unwrap();
        assert!((extruded_origin(live).z - 30.0).abs() < 1.0e-12);
    }

    #[test]
    fn refreshing_plane_frames_moves_the_plane_and_its_sketches_together() {
        let (mut document, plane, sketch, _) = document_with_plane_sketch();
        let moved = crate::datum::ResolvedDatumPlane {
            frame: crate::datum::offset_along_normal(crate::datum::OriginPlane::Xy.frame(), -4.0),
            half_extent: [30.0, 30.0],
        };
        let resolved = std::collections::BTreeMap::from([(plane, moved)]);
        assert!(document.refresh_datum_plane_frames(&resolved));
        assert_eq!(document.datum_plane(plane).unwrap().frame, moved.frame);
        assert_eq!(document.sketch_frame(sketch), Some(moved.frame));
        let record = document.sketch(sketch).unwrap();
        assert_eq!(
            document
                .sketch_payload(sketch, record.geometry_revision)
                .unwrap()
                .frame,
            moved.frame
        );
        // A second refresh with the same frames changes nothing.
        assert!(!document.refresh_datum_plane_frames(&resolved));
    }

    #[test]
    fn a_plane_something_is_built_on_cannot_be_deleted() {
        let (mut document, plane, sketch, _) = document_with_plane_sketch();
        let sketch_feature = document.sketch(sketch).unwrap().created_by;
        assert_eq!(
            document.remove_datum_plane(plane).unwrap_err(),
            crate::DocumentError::FeatureInUse {
                feature: plane,
                dependent: sketch_feature,
            }
        );
        assert!(document.feature(plane).is_some());
    }

    #[test]
    fn an_unused_plane_is_deleted_and_the_deletion_undoes() {
        use crate::datum::{DatumPlaneBase, DatumPlaneRecipe, OriginPlane, ResolvedDatumPlane};
        let mut document = ModelDocument::default();
        let plane = document
            .append_feature(FeatureDraft::new(
                FeatureKind::DatumPlane,
                "Plane 1",
                ReplayAction::DatumPlane(DatumPlaneRecipe::new(
                    DatumPlaneBase::Origin {
                        plane: OriginPlane::Yz,
                    },
                    ResolvedDatumPlane {
                        frame: OriginPlane::Yz.frame(),
                        half_extent: [25.0, 25.0],
                    },
                )),
            ))
            .unwrap()
            .feature;
        assert!(document.set_datum_plane_visible(plane, false).unwrap());
        assert!(!document.datum_plane(plane).unwrap().visible);
        assert_eq!(
            document.feature(plane).unwrap().state.rebuild,
            RebuildState::Dirty,
            "a plane appended without a commit is dirty; hiding it does not change that"
        );
        document.remove_datum_plane(plane).unwrap();
        assert!(document.feature(plane).is_none());
        assert!(document.undo());
        assert!(document.feature(plane).is_some());
    }

    #[test]
    fn a_side_that_ends_at_a_plane_names_the_plane_as_an_input() {
        let (mut document, plane, sketch, signature) = document_with_plane_sketch();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], 5.0)
            .unwrap()
            .with_up_to_planes(Some(plane), None)
            .unwrap();
        let missing_input = document.append_feature(
            FeatureDraft::new(
                FeatureKind::Extrude,
                "Extrude",
                ReplayAction::SketchRegionExtrusion(recipe.clone()),
            )
            .with_input(FeatureInput::Sketch(sketch))
            .with_output(OutputDraft::CreateBody {
                label: "Body 1".into(),
            }),
        );
        assert_eq!(
            missing_input.unwrap_err(),
            crate::DocumentError::EndPlaneMustBeInput(plane)
        );
        let extrusion = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Extrude,
                    "Extrude",
                    ReplayAction::SketchRegionExtrusion(recipe.clone()),
                )
                .with_input(FeatureInput::Sketch(sketch))
                .with_input(FeatureInput::Feature(plane))
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".into(),
                }),
            )
            .unwrap()
            .feature;
        assert!(
            document
                .feature(extrusion)
                .unwrap()
                .dependencies
                .contains(&plane)
        );
        // Going back to a distance drops the plane input and its dependency.
        let mut plain = recipe;
        plain.up_to_plane = None;
        document
            .replace_feature_action_and_inputs(
                extrusion,
                ReplayAction::SketchRegionExtrusion(plain),
                vec![FeatureInput::Sketch(sketch)],
            )
            .unwrap();
        let node = document.feature(extrusion).unwrap();
        assert!(!node.inputs.contains(&FeatureInput::Feature(plane)));
        assert!(!node.dependencies.contains(&plane));
        assert_eq!(
            SketchRegionExtrusion::new_body(sketch, vec![], 1.0).unwrap_err(),
            SketchRegionRecipeError::EmptySelection
        );
    }

    #[test]
    fn a_plane_document_round_trips_through_the_native_envelope() {
        let (document, plane, sketch, _) = document_with_plane_sketch();
        let native = document.to_native();
        assert_eq!(native.version(), crate::CURRENT_DOCUMENT_VERSION);
        let json = serde_json::to_string(&native).unwrap();
        let restored = ModelDocument::from_native(serde_json::from_str(&json).unwrap()).unwrap();
        assert_eq!(restored.datum_plane(plane), document.datum_plane(plane));
        assert_eq!(restored.sketch_frame(sketch), document.sketch_frame(sketch));
    }

    /// A plane sketch document with a second sketch on the XY origin plane,
    /// for a loft from the origin up to the plane.
    fn document_with_two_sections() -> (ModelDocument, crate::FeatureId, SketchId, SketchId) {
        let (mut document, plane, upper, _) = document_with_plane_sketch();
        let definition = rectangle(4.0, 4.0);
        let (_, profile) = selected_profile(&definition);
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
        let lower = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker)
                    .with_sketch_payload(payload)
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 2".into(),
                        geometry_revision: 1,
                    })
                    .with_commit(marker),
            )
            .unwrap()
            .created_sketches[0];
        (document, plane, lower, upper)
    }

    fn loft_between(
        document: &ModelDocument,
        lower: SketchId,
        upper: SketchId,
        operation: artificer_protocol::LoftOperation,
    ) -> crate::SketchLoft {
        let precision = PrecisionPolicy::default();
        let section = |sketch| {
            crate::SketchLoftSection::new(
                sketch,
                vec![sketch_region_at(document, sketch, [1.0, 1.0], precision).unwrap()],
            )
        };
        crate::SketchLoft::new(vec![section(lower), section(upper)], operation).unwrap()
    }

    fn section_heights(action: ReplayAction) -> Vec<f64> {
        match action {
            ReplayAction::Kernel(KernelCommand::LoftPlanarSections { sections, .. }) => sections
                .iter()
                .map(|section| section.frame.origin.z)
                .collect(),
            other => panic!("a loft recipe resolves to a loft between sections: {other:?}"),
        }
    }

    /// Each section is compiled from its own sketch and placed on that
    /// sketch's plane; a plane the rebuild has just moved carries its section
    /// with it before any cache is refreshed.
    #[test]
    fn a_loft_places_each_section_on_its_own_plane() {
        let (document, plane, lower, upper) = document_with_two_sections();
        let recipe = loft_between(
            &document,
            lower,
            upper,
            artificer_protocol::LoftOperation::New,
        );
        let action = ReplayAction::SketchLoft(recipe);
        let cached = action
            .resolve_sketch_regions(&document, PrecisionPolicy::default())
            .unwrap();
        assert_eq!(section_heights(cached), vec![0.0, 10.0]);
        let moved = crate::datum::ResolvedDatumPlane {
            frame: crate::datum::offset_along_normal(crate::datum::OriginPlane::Xy.frame(), 25.0),
            half_extent: [25.0, 25.0],
        };
        let live = action
            .resolve_sketch_regions_with_planes(
                &document,
                PrecisionPolicy::default(),
                &std::collections::BTreeMap::from([(plane, moved)]),
            )
            .unwrap();
        assert_eq!(section_heights(live), vec![0.0, 25.0]);
    }

    #[test]
    fn a_loft_needs_two_sections_from_two_sketches() {
        let (document, _, lower, upper) = document_with_two_sections();
        let recipe = loft_between(
            &document,
            lower,
            upper,
            artificer_protocol::LoftOperation::New,
        );
        let one = crate::SketchLoft::new(
            recipe.sections[..1].to_vec(),
            artificer_protocol::LoftOperation::New,
        );
        assert_eq!(one.unwrap_err(), crate::SketchLoftError::TooFewSections);
        let repeated = crate::SketchLoft::new(
            vec![recipe.sections[0].clone(), recipe.sections[0].clone()],
            artificer_protocol::LoftOperation::New,
        );
        assert_eq!(
            repeated.unwrap_err(),
            crate::SketchLoftError::RepeatedSketch(lower)
        );
    }

    /// A loft reads its sketches on every replay and changes the body an add
    /// or a cut names, so the history holds both as inputs; and only a loft
    /// feature carries a loft recipe.
    #[test]
    fn a_loft_feature_names_its_sketches_and_its_body() {
        use artificer_protocol::LoftOperation;
        let (mut document, _, lower, upper) = document_with_two_sections();
        let marker = SnapshotAssociation::new(
            SnapshotId::ZERO,
            SnapshotId::ZERO,
            SemanticDigest::new([0; 32]),
        );
        let new_body = loft_between(&document, lower, upper, LoftOperation::New);
        let draft = |kind, recipe: crate::SketchLoft| {
            FeatureDraft::new(kind, "Loft 1", ReplayAction::SketchLoft(recipe)).with_commit(marker)
        };
        assert_eq!(
            document
                .append_feature(
                    draft(FeatureKind::Loft, new_body.clone())
                        .with_input(FeatureInput::Sketch(lower))
                )
                .unwrap_err(),
            crate::DocumentError::SketchRegionSourceMustBeInput(upper)
        );
        assert_eq!(
            document
                .append_feature(
                    draft(FeatureKind::Extrude, new_body.clone())
                        .with_input(FeatureInput::Sketch(lower))
                        .with_input(FeatureInput::Sketch(upper))
                )
                .unwrap_err(),
            crate::DocumentError::InvalidLoftFeature
        );
        let cut = loft_between(&document, lower, upper, LoftOperation::Cut);
        assert_eq!(
            document
                .append_feature(
                    draft(FeatureKind::Loft, cut)
                        .with_input(FeatureInput::Sketch(lower))
                        .with_input(FeatureInput::Sketch(upper))
                )
                .unwrap_err(),
            crate::DocumentError::SketchLoft(crate::SketchLoftError::MissingTargetBody)
        );
        let appended = document
            .append_feature(
                draft(FeatureKind::Loft, new_body)
                    .with_input(FeatureInput::Sketch(lower))
                    .with_input(FeatureInput::Sketch(upper))
                    .with_output(OutputDraft::CreateBody {
                        label: "Body 1".into(),
                    }),
            )
            .unwrap();
        assert_eq!(appended.created_bodies.len(), 1);
        // A loft document is written in the schema that knows lofts, and
        // reads back to the same recipe.
        let native = document.to_native();
        assert_eq!(native.version(), crate::SKETCH_LOFT_DOCUMENT_VERSION);
        let json = serde_json::to_string(&native).unwrap();
        let restored = ModelDocument::from_native(serde_json::from_str(&json).unwrap()).unwrap();
        assert_eq!(
            restored.feature(appended.feature).map(|node| &node.action),
            document.feature(appended.feature).map(|node| &node.action)
        );
    }

    #[test]
    fn a_pick_inside_a_region_names_it_and_a_pick_outside_names_nothing() {
        let (document, sketch, signature) = document_with_rectangle();
        let precision = PrecisionPolicy::default();
        assert_eq!(
            sketch_region_at(&document, sketch, [1.0, 1.5], precision),
            Some(signature)
        );
        assert_eq!(
            sketch_region_at(&document, sketch, [5.0, 1.5], precision),
            None
        );
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;
    use crate::{
        FeatureDraft, FeatureKind, ModelDocument, OutputDraft, SketchPayload, SketchSupportRecipe,
        SnapshotAssociation,
    };
    use artificer_protocol::{
        PlanarFrame3, Point3, PrecisionPolicy, SemanticDigest, SnapshotId, Vector3,
    };
    use artificer_sketch::{
        ArrangementLimits, ConfirmationSource, PointInput, SignedLength, SketchDefinition,
        SketchPoint2, SketchRecipe, SketchValue, build_arrangement, compile_selected_profile,
    };

    fn document_with_square() -> (ModelDocument, SketchId, RegionSignature) {
        let mut definition = SketchDefinition::new();
        let transaction = definition
            .stage(
                SketchRecipe::TwoPointRectangle {
                    first_corner: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
                    width: SketchValue::Literal(SignedLength::new(4.0).unwrap()),
                    height: SketchValue::Literal(SignedLength::new(4.0).unwrap()),
                },
                "Square",
            )
            .unwrap();
        definition
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(
            &definition.arrangement_inputs().unwrap(),
            &precision,
            ArrangementLimits::default(),
        );
        let regions = vec![arrangement.cells[0].signature.clone()];
        let profile = compile_selected_profile(&arrangement, &regions, &precision)
            .unwrap()
            .profile;
        let payload = SketchPayload::from_authoring(
            PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            definition,
            Some(profile),
            SketchSupportRecipe::Origin,
        )
        .unwrap();
        let mut document = ModelDocument::default();
        let appended = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Sketch, "Sketch", ReplayAction::Marker)
                    .with_sketch_payload(payload)
                    .with_output(OutputDraft::CreateSketch {
                        label: "Sketch 1".into(),
                        geometry_revision: 1,
                    })
                    .with_commit(SnapshotAssociation::new(
                        SnapshotId::ZERO,
                        SnapshotId::ZERO,
                        SemanticDigest::new([0; 32]),
                    )),
            )
            .unwrap();
        (document, appended.created_sketches[0], regions[0].clone())
    }

    #[test]
    fn a_drafted_new_body_replays_as_a_loft_to_the_offset_section() {
        let (document, sketch, signature) = document_with_square();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature], 5.0)
            .unwrap()
            .with_draft(10.0)
            .unwrap();
        assert!(recipe.is_drafted());
        let ReplayAction::Kernel(KernelCommand::LoftPlanarProfileOffset {
            distance, offset, ..
        }) = recipe
            .resolve(&document, PrecisionPolicy::default())
            .unwrap()
        else {
            panic!("a drafted recipe lofts")
        };
        assert_eq!(distance, 5.0);
        assert!((offset - 5.0 * 10.0_f64.to_radians().tan()).abs() < 1.0e-12);

        // The persisted form omits a zero draft and reads old recipes as
        // straight extrusions.
        let straight =
            SketchRegionExtrusion::new_body(sketch, recipe.regions.clone(), 5.0).unwrap();
        let json = serde_json::to_string(&straight).unwrap();
        assert!(!json.contains("draft_degrees"));
        let parsed: SketchRegionExtrusion = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, straight);
        let json = serde_json::to_string(&recipe).unwrap();
        assert!(json.contains("draft_degrees"));
        assert_eq!(
            serde_json::from_str::<SketchRegionExtrusion>(&json).unwrap(),
            recipe
        );
    }

    #[test]
    fn drafts_are_bounded_and_refused_on_face_features() {
        let (_, sketch, signature) = document_with_square();
        let recipe = SketchRegionExtrusion::new_body(sketch, vec![signature.clone()], 5.0).unwrap();
        assert_eq!(
            recipe.clone().with_draft(80.0).unwrap_err(),
            SketchRegionRecipeError::InvalidDraft
        );
        assert_eq!(
            recipe.with_draft(f64::NAN).unwrap_err(),
            SketchRegionRecipeError::InvalidDraft
        );
        let face = PersistentRef::new(
            crate::FeatureId::from_allocated(1),
            artificer_protocol::OperationRole::new("base.entity", None),
            EntityKind::Face,
        );
        let on_face = SketchRegionExtrusion::on_face(
            sketch,
            vec![signature],
            face,
            FaceExtrusionOperation::Add,
            5.0,
        )
        .unwrap();
        assert_eq!(
            on_face.with_draft(5.0).unwrap_err(),
            SketchRegionRecipeError::InvalidDraft
        );
    }
}

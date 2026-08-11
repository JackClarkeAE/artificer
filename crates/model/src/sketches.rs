//! Portable, snapshot-independent sketch geometry retained by the document.

use artificer_protocol::{
    EntityKind, MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS,
    PlanarCurve2, PlanarFrame3, PlanarProfile2, PrecisionPolicy,
};
use artificer_sketch::SketchDefinition;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::BodyId;
use crate::persistent::{
    CURRENT_PERSISTENT_REF_VERSION, MAX_PERSISTENT_LINEAGE_DEPTH, PersistentRef,
};

/// Version of the authoring-side precision/validation contract persisted with
/// editable sketches. This is independent of the native document version.
pub const CURRENT_SKETCH_PRECISION_POLICY_VERSION: u32 = 1;

const fn current_sketch_precision_policy_version() -> u32 {
    CURRENT_SKETCH_PRECISION_POLICY_VERSION
}

/// Exact geometry and placement needed to rehydrate one committed sketch.
///
/// The geometry revision is carried by the owning [`crate::FeatureOutput`].
/// Keeping the payload on that revision-producing feature preserves historical
/// revisions instead of mutating one global sketch blob.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchPayload {
    pub frame: PlanarFrame3,
    /// Checked exact profile cache used by legacy readers and fast replay.
    /// Editable authoring intent remains authoritative when present.
    pub profile: PlanarProfile2,
    pub support: SketchSupportRecipe,
    #[serde(default)]
    pub authoring: Option<SketchDefinition>,
    #[serde(default = "current_sketch_precision_policy_version")]
    pub precision_policy_version: u32,
}

impl SketchPayload {
    /// Constructs and validates an exact portable sketch payload.
    pub fn new(
        frame: PlanarFrame3,
        profile: PlanarProfile2,
        support: SketchSupportRecipe,
    ) -> Result<Self, SketchPayloadError> {
        let authoring = SketchDefinition::from_legacy_profile(&profile, PrecisionPolicy::default())
            .map_err(|_| SketchPayloadError::InvalidAuthoringDefinition)?;
        let payload = Self {
            frame,
            profile,
            support,
            authoring: Some(authoring),
            precision_policy_version: CURRENT_SKETCH_PRECISION_POLICY_VERSION,
        };
        payload.validate()?;
        Ok(payload)
    }

    /// Constructs an editable sketch. `profile` is a checked derived cache and
    /// may be absent for an open or construction-only sketch.
    pub fn from_authoring(
        frame: PlanarFrame3,
        authoring: SketchDefinition,
        profile: Option<PlanarProfile2>,
        support: SketchSupportRecipe,
    ) -> Result<Self, SketchPayloadError> {
        let payload = Self {
            frame,
            profile: profile.unwrap_or_default(),
            support,
            authoring: Some(authoring),
            precision_policy_version: CURRENT_SKETCH_PRECISION_POLICY_VERSION,
        };
        payload.validate()?;
        Ok(payload)
    }

    #[must_use]
    pub const fn authoring(&self) -> Option<&SketchDefinition> {
        self.authoring.as_ref()
    }

    /// Validates defensive bounds and snapshot-independent support identity.
    ///
    /// Curve closure, winding, intersections, and tolerances remain kernel
    /// certification responsibilities during replay.
    pub fn validate(&self) -> Result<(), SketchPayloadError> {
        validate_frame(self.frame)?;
        if self.precision_policy_version != CURRENT_SKETCH_PRECISION_POLICY_VERSION {
            return Err(SketchPayloadError::UnsupportedPrecisionPolicyVersion {
                found: self.precision_policy_version,
            });
        }
        if let Some(authoring) = &self.authoring {
            authoring
                .validate(PrecisionPolicy::default())
                .map_err(|_| SketchPayloadError::InvalidAuthoringDefinition)?;
            if !self.profile.regions.is_empty() {
                validate_profile(&self.profile)?;
            }
        } else {
            validate_profile(&self.profile)?;
        }
        self.support.validate()
    }
}

/// Stable support recipe for a portable sketch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SketchSupportRecipe {
    /// A document origin plane. The exact plane placement is in the payload frame.
    Origin,
    /// A planar face on a stable body branch.
    PlanarFace {
        body: BodyId,
        /// Document-level face identity; never a snapshot-local entity handle.
        face: PersistentRef,
    },
}

impl SketchSupportRecipe {
    fn validate(&self) -> Result<(), SketchPayloadError> {
        match self {
            Self::Origin => Ok(()),
            Self::PlanarFace { body, face } => {
                if body.get() == 0 {
                    return Err(SketchPayloadError::InvalidSupportBody);
                }
                if face.kind != EntityKind::Face {
                    return Err(SketchPayloadError::PlanarFaceTargetRequired);
                }
                validate_persistent_ref(face, 0)
            }
        }
    }

    /// Body branch carrying this support, if it is face-hosted.
    #[must_use]
    pub const fn body(&self) -> Option<BodyId> {
        match self {
            Self::Origin => None,
            Self::PlanarFace { body, .. } => Some(*body),
        }
    }

    /// Persistent face recipe for face-hosted sketches.
    #[must_use]
    pub const fn face(&self) -> Option<&PersistentRef> {
        match self {
            Self::Origin => None,
            Self::PlanarFace { face, .. } => Some(face),
        }
    }
}

/// Portable-sketch validation failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SketchPayloadError {
    #[error("the sketch frame must contain only finite coordinates")]
    NonFiniteFrame,
    #[error("the sketch frame axes must be non-zero and non-parallel")]
    DegenerateFrame,
    #[error("a portable sketch profile must contain at least one material region")]
    EmptyProfile,
    #[error("the editable sketch authoring graph is invalid")]
    InvalidAuthoringDefinition,
    #[error("unsupported editable-sketch precision policy version {found}")]
    UnsupportedPrecisionPolicyVersion { found: u32 },
    #[error("a portable sketch profile loop must contain at least one exact curve")]
    EmptyLoop,
    #[error("a portable sketch profile contains a non-finite curve")]
    NonFiniteCurve,
    #[error("a portable sketch circle radius must be positive")]
    InvalidCircleRadius,
    #[error("the portable sketch profile exceeds the region limit of {MAX_PLANAR_PROFILE_REGIONS}")]
    TooManyRegions,
    #[error("the portable sketch profile exceeds the loop limit of {MAX_PLANAR_PROFILE_LOOPS}")]
    TooManyLoops,
    #[error("the portable sketch profile exceeds the curve limit of {MAX_PLANAR_PROFILE_CURVES}")]
    TooManyCurves,
    #[error("a planar-face sketch support must carry a non-zero body ID")]
    InvalidSupportBody,
    #[error("a planar-face sketch support must target a face")]
    PlanarFaceTargetRequired,
    #[error(
        "unsupported persistent-reference version {found}; this build supports {CURRENT_PERSISTENT_REF_VERSION}"
    )]
    UnsupportedPersistentReferenceVersion { found: u32 },
    #[error("a persistent-reference producer must be non-zero")]
    InvalidPersistentProducer,
    #[error(
        "persistent-reference lineage exceeds the depth limit of {MAX_PERSISTENT_LINEAGE_DEPTH}"
    )]
    PersistentLineageTooDeep,
}

fn validate_frame(frame: PlanarFrame3) -> Result<(), SketchPayloadError> {
    if !frame.is_finite() {
        return Err(SketchPayloadError::NonFiniteFrame);
    }
    let u2 = frame.u.x * frame.u.x + frame.u.y * frame.u.y + frame.u.z * frame.u.z;
    let v2 = frame.v.x * frame.v.x + frame.v.y * frame.v.y + frame.v.z * frame.v.z;
    if u2 == 0.0 || v2 == 0.0 {
        return Err(SketchPayloadError::DegenerateFrame);
    }
    let cross_x = frame.u.y * frame.v.z - frame.u.z * frame.v.y;
    let cross_y = frame.u.z * frame.v.x - frame.u.x * frame.v.z;
    let cross_z = frame.u.x * frame.v.y - frame.u.y * frame.v.x;
    let cross2 = cross_x * cross_x + cross_y * cross_y + cross_z * cross_z;
    if !u2.is_finite()
        || !v2.is_finite()
        || !cross2.is_finite()
        || cross2 <= f64::EPSILON * f64::EPSILON * u2 * v2
    {
        return Err(SketchPayloadError::DegenerateFrame);
    }
    Ok(())
}

fn validate_profile(profile: &PlanarProfile2) -> Result<(), SketchPayloadError> {
    if profile.regions.is_empty() {
        return Err(SketchPayloadError::EmptyProfile);
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return Err(SketchPayloadError::TooManyRegions);
    }

    let mut loops = 0usize;
    let mut curves = 0usize;
    for region in &profile.regions {
        loops = loops
            .checked_add(1 + region.holes.len())
            .ok_or(SketchPayloadError::TooManyLoops)?;
        if loops > MAX_PLANAR_PROFILE_LOOPS {
            return Err(SketchPayloadError::TooManyLoops);
        }
        for profile_loop in std::iter::once(&region.outer).chain(&region.holes) {
            if profile_loop.curves.is_empty() {
                return Err(SketchPayloadError::EmptyLoop);
            }
            curves = curves
                .checked_add(profile_loop.curves.len())
                .ok_or(SketchPayloadError::TooManyCurves)?;
            if curves > MAX_PLANAR_PROFILE_CURVES {
                return Err(SketchPayloadError::TooManyCurves);
            }
            for curve in &profile_loop.curves {
                if !curve.is_finite() {
                    return Err(SketchPayloadError::NonFiniteCurve);
                }
                if let PlanarCurve2::Circle { radius, .. } = curve
                    && *radius <= 0.0
                {
                    return Err(SketchPayloadError::InvalidCircleRadius);
                }
            }
        }
    }
    Ok(())
}

fn validate_persistent_ref(
    reference: &PersistentRef,
    depth: usize,
) -> Result<(), SketchPayloadError> {
    if depth >= MAX_PERSISTENT_LINEAGE_DEPTH {
        return Err(SketchPayloadError::PersistentLineageTooDeep);
    }
    if reference.version != CURRENT_PERSISTENT_REF_VERSION {
        return Err(SketchPayloadError::UnsupportedPersistentReferenceVersion {
            found: reference.version,
        });
    }
    if reference.producer.get() == 0 {
        return Err(SketchPayloadError::InvalidPersistentProducer);
    }
    if let Some(lineage) = &reference.lineage {
        validate_persistent_ref(lineage, depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        ArcDirection, EntityKind, OperationRole, PlanarLoop2, PlanarRegion2, Point2, Point3,
        Vector3,
    };

    use super::*;
    use crate::FeatureId;

    fn frame() -> PlanarFrame3 {
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        )
    }

    #[test]
    fn analytic_circle_payload_is_exact_and_serde_ready() {
        let payload = SketchPayload::new(
            frame(),
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(2.0, 3.0),
                            radius: 4.0,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: Vec::new(),
                }],
            },
            SketchSupportRecipe::Origin,
        )
        .expect("analytic circle should validate");
        let json = serde_json::to_string(&payload).expect("payload should serialize");
        assert_eq!(
            serde_json::from_str::<SketchPayload>(&json).expect("payload should deserialize"),
            payload
        );
    }

    #[test]
    fn empty_editable_sketch_payload_round_trips_without_inventing_a_profile() {
        let payload = SketchPayload::from_authoring(
            frame(),
            SketchDefinition::new(),
            None,
            SketchSupportRecipe::Origin,
        )
        .expect("an open empty editable sketch is valid document intent");
        assert!(payload.profile.regions.is_empty());
        assert_eq!(payload.authoring(), Some(&SketchDefinition::new()));

        let json = serde_json::to_string(&payload).expect("editable payload should serialize");
        let decoded = serde_json::from_str::<SketchPayload>(&json)
            .expect("editable payload should deserialize");
        assert_eq!(decoded, payload);
        decoded
            .validate()
            .expect("round-tripped graph should validate");
    }

    #[test]
    fn degenerate_frames_and_unbounded_profiles_are_rejected() {
        let profile = PlanarProfile2::from_polygon(&[
            Point2::new(0.0, 0.0),
            Point2::new(1.0, 0.0),
            Point2::new(1.0, 1.0),
        ]);
        assert_eq!(
            SketchPayload::new(
                PlanarFrame3::new(
                    Point3::new(0.0, 0.0, 0.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(2.0, 0.0, 0.0),
                ),
                profile,
                SketchSupportRecipe::Origin,
            ),
            Err(SketchPayloadError::DegenerateFrame)
        );

        let excessive = PlanarProfile2 {
            regions: (0..=MAX_PLANAR_PROFILE_REGIONS)
                .map(|_| {
                    PlanarRegion2::from_polygon(&[
                        Point2::new(0.0, 0.0),
                        Point2::new(1.0, 0.0),
                        Point2::new(1.0, 1.0),
                    ])
                })
                .collect(),
        };
        assert_eq!(
            SketchPayload::new(frame(), excessive, SketchSupportRecipe::Origin),
            Err(SketchPayloadError::TooManyRegions)
        );
    }

    #[test]
    fn planar_support_rejects_non_face_persistent_targets() {
        let error = SketchPayload::new(
            frame(),
            PlanarProfile2::from_polygon(&[
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 0.0),
                Point2::new(1.0, 1.0),
            ]),
            SketchSupportRecipe::PlanarFace {
                body: BodyId::from_allocated(1),
                face: PersistentRef::new(
                    FeatureId::from_allocated(1),
                    OperationRole::new("edge", Some(0)),
                    EntityKind::Edge,
                ),
            },
        )
        .expect_err("an edge cannot host a planar-face sketch");
        assert_eq!(error, SketchPayloadError::PlanarFaceTargetRequired);
    }
}

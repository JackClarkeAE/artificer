//! The first native Artificer geometry-kernel slice.
//!
//! This crate owns its topology, validation, transactions, identity, and
//! diagnostic display geometry. It intentionally has no foreign geometry,
//! UI, renderer, or backend-abstraction dependency.

mod analytic_extrusion;
pub mod api;
pub mod brep;
mod corner_blend;
mod cuboid;
mod edge_finish;
mod exact_face_feature;
mod extrusion;
mod face_feature;
mod faceted_boolean;
// The certified loop offset is the geometric core of rim-loop blends
// (ADR 0023 frontier, milestone B). It is complete and unit-tested; the band,
// sphere-corner, and ledge assembly that consumes it is still to come.
mod analytic_boolean;
#[allow(dead_code)]
mod loft;
mod loop_offset;
mod planar_profile;
mod prism_boolean;
mod prism_edge_finish;
mod profile_boolean;
mod push_pull;
mod revolve;
mod rim_loop_blend;
mod section_revolve;
mod sew;
mod surface_intersection;
mod topology;
mod transform;
mod validator;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use artificer_compute::{ComputePool, perf_span};
use artificer_protocol::{
    Aabb3, ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION,
    Diagnostic as ProtocolDiagnostic, DiagnosticCode as ProtocolDiagnosticCode,
    DiagnosticMeasurement, DiagnosticSeverity, EntityId as ProtocolEntityId, EntityKind, EntityRef,
    ExecuteRequest, FaceExtrusionOperation, HistoryRecord, HistoryRelation, KernelCommand,
    KernelError, KernelErrorCode, KernelStage, NumericInterval, OperationReport, OperationRole,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
    Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy, QuantityKind,
    SemanticDigest, SnapshotId, TopologyCounts as ProtocolTopologyCounts, ValidationProfile,
    ValidationReport as ProtocolValidationReport, Vector3 as ProtocolVector3,
};
use sha2::{Digest, Sha256};

use crate::analytic_extrusion::{build_analytic_extrusion, validate_analytic_profile_extrusion};
use crate::cuboid::build_cuboid;
use crate::exact_face_feature::validate_exact_face_feature;
use crate::extrusion::{ExtrusionInputError, build_extrusion, validate_extrusion_input};
use crate::face_feature::{
    FaceFeatureArguments, FaceFeatureInputError, build_face_feature, validate_face_feature_input,
};
use crate::planar_profile::{
    PlanarProfileInputError, profile_contains_analytic_curves, validate_linear_profile_extrusion,
};
use crate::push_pull::{
    FacePushPullArguments, FacePushPullInputError, build_face_push_pull,
    validate_face_push_pull_input,
};
use crate::topology::{
    Curve2, Curve3, Orientation, Point3, Surface, Topology, TopologyCounts, Vector3,
};
use crate::transform::{Similarity, TransformInputError, transform_topology};

pub use crate::topology::FaceRole;

/// Immutable, validated model state.
#[derive(Clone, Debug)]
pub struct Snapshot {
    id: SnapshotId,
    semantic_digest: SemanticDigest,
    precision: Option<PrecisionPolicy>,
    topology: Topology,
    measures: SnapshotMeasures,
}

impl Snapshot {
    #[must_use]
    pub const fn id(&self) -> SnapshotId {
        self.id
    }

    #[must_use]
    pub const fn semantic_digest(&self) -> SemanticDigest {
        self.semantic_digest
    }

    #[must_use]
    pub fn counts(&self) -> ProtocolTopologyCounts {
        protocol_counts(TopologyCounts::from(&self.topology))
    }

    #[must_use]
    pub const fn measures(&self) -> SnapshotMeasures {
        self.measures
    }

    #[must_use]
    pub const fn precision_policy(&self) -> Option<PrecisionPolicy> {
        self.precision
    }
}

/// Analytic measures computed from the committed B-rep.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapshotMeasures {
    pub bounds: Option<Aabb3>,
    pub surface_area: f64,
    pub volume: f64,
    pub centroid: Option<ProtocolPoint3>,
}

/// Result of a successful transaction.
#[derive(Clone, Debug)]
pub struct ExecutionOutcome {
    pub snapshot: Snapshot,
    pub report: OperationReport,
}

/// Two complementary committed results of a split: target minus tool and the
/// material common to both operands.
#[derive(Clone, Debug)]
pub struct BooleanSplitOutcome {
    pub remainder: ExecutionOutcome,
    pub overlap: ExecutionOutcome,
}

/// Cooperative cancellation handle. Failed/cancelled work never publishes a snapshot.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Deterministic, source-mapped triangles used for diagnostics and the first UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugTriangle {
    pub vertices: [ProtocolPoint3; 3],
    /// Exact unit outward normals of the carrier surface at each vertex, not
    /// of the chord triangle. A curved face therefore shades as the surface it
    /// approximates rather than as its facets, and a vertex shared by two
    /// triangles of one carrier carries bit-identical normals in both.
    pub normals: [ProtocolVector3; 3],
    pub source_face: EntityRef,
    pub role: FaceRole,
}

/// Deterministic, source-mapped B-rep edge used for diagnostics and the first UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugEdge {
    pub endpoints: [ProtocolPoint3; 2],
    pub source_edge: EntityRef,
    /// True when this edge only separates low-dihedral approximation facets.
    /// The topology retains it for validation, while CAD presentation and
    /// picking may treat it as a smooth internal subdivision.
    pub is_smooth: bool,
    /// True when two different exact carriers meet here with one shared
    /// normal — the transition rail of a fillet. The rail is real topology
    /// and stays selectable, but it is not a crease: presentation draws it
    /// only where it happens to be the body's outline, or when the user
    /// hovers or selects it.
    pub is_tangent: bool,
    /// The faces this edge separates, in topology order. Presentation uses
    /// them to tell an outline edge — one incident face turned away from the
    /// camera — from an interior crease, which is most of why a drafting
    /// viewport reads as crisp rather than as a wireframe.
    pub incident_faces: [Option<EntityRef>; 2],
}

/// Presentation-only description of one curved face's carrier surface.
///
/// The viewport needs the analytic surface to draw the silhouette where a
/// smooth face rolls away from the camera — a curve that is in no B-rep edge
/// because no topology changes there. This descriptor mirrors the surface
/// parameters for that purpose alone: it is display metadata, carries no
/// authority, and nothing evaluated from it may re-enter modelling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayCarrier {
    pub source_face: EntityRef,
    pub surface: DisplaySurface,
    /// The face's parameter rectangle `[[u_min, u_max], [v_min, v_max]]` —
    /// the same domain its display tessellation spans, so a silhouette drawn
    /// inside it is bounded exactly as the shaded fill is.
    pub domain: [[f64; 2]; 2],
}

/// The curved half of the surface vocabulary, in display-facing form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DisplaySurface {
    Cylinder {
        origin: ProtocolPoint3,
        axis: ProtocolVector3,
        radial_u: ProtocolVector3,
        radial_v: ProtocolVector3,
        radius: f64,
        angular_sign: f64,
    },
    Cone {
        origin: ProtocolPoint3,
        axis: ProtocolVector3,
        radial_u: ProtocolVector3,
        radial_v: ProtocolVector3,
        base_radius: f64,
        slope: f64,
        angular_sign: f64,
    },
    Sphere {
        origin: ProtocolPoint3,
        axis: ProtocolVector3,
        radial_u: ProtocolVector3,
        radial_v: ProtocolVector3,
        radius: f64,
        angular_sign: f64,
    },
    Torus {
        origin: ProtocolPoint3,
        axis: ProtocolVector3,
        radial_u: ProtocolVector3,
        radial_v: ProtocolVector3,
        major_radius: f64,
        minor_radius: f64,
        angular_sign: f64,
    },
}

impl DisplaySurface {
    /// The carrier point at one parameter pair, in the surface's own exact
    /// parameterisation.
    #[must_use]
    pub fn evaluate(self, u: f64, v: f64) -> ProtocolPoint3 {
        let (origin, axis, radial_u, radial_v, angular_sign) = self.frame();
        let angle = angular_sign * u;
        let (sin, cos) = angle.sin_cos();
        let radial = ProtocolVector3::new(
            radial_u.x.mul_add(cos, radial_v.x * sin),
            radial_u.y.mul_add(cos, radial_v.y * sin),
            radial_u.z.mul_add(cos, radial_v.z * sin),
        );
        let (ring, lift) = match self {
            Self::Cylinder { radius, .. } => (radius, v),
            Self::Cone {
                base_radius, slope, ..
            } => (slope.mul_add(v, base_radius), v),
            Self::Sphere { radius, .. } => {
                let (sin_v, cos_v) = v.sin_cos();
                (radius * cos_v, radius * sin_v)
            }
            Self::Torus {
                major_radius,
                minor_radius,
                ..
            } => {
                let (sin_v, cos_v) = v.sin_cos();
                (
                    minor_radius.mul_add(cos_v, major_radius),
                    minor_radius * sin_v,
                )
            }
        };
        ProtocolPoint3::new(
            radial.x.mul_add(ring, axis.x.mul_add(lift, origin.x)),
            radial.y.mul_add(ring, axis.y.mul_add(lift, origin.y)),
            radial.z.mul_add(ring, axis.z.mul_add(lift, origin.z)),
        )
    }

    /// `(origin, axis, radial_u, radial_v, angular_sign)`, shared by every arm.
    #[must_use]
    pub const fn frame(
        self,
    ) -> (
        ProtocolPoint3,
        ProtocolVector3,
        ProtocolVector3,
        ProtocolVector3,
        f64,
    ) {
        match self {
            Self::Cylinder {
                origin,
                axis,
                radial_u,
                radial_v,
                angular_sign,
                ..
            }
            | Self::Cone {
                origin,
                axis,
                radial_u,
                radial_v,
                angular_sign,
                ..
            }
            | Self::Sphere {
                origin,
                axis,
                radial_u,
                radial_v,
                angular_sign,
                ..
            }
            | Self::Torus {
                origin,
                axis,
                radial_u,
                radial_v,
                angular_sign,
                ..
            } => (origin, axis, radial_u, radial_v, angular_sign),
        }
    }
}

/// Deterministic, source-mapped B-rep vertex used for selection and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebugVertex {
    pub point: ProtocolPoint3,
    pub source_vertex: EntityRef,
    /// True when every incident edge is an approximation-only subdivision.
    /// Such vertices remain in the B-rep but are not useful CAD point targets.
    pub is_smooth: bool,
}

/// Read-only diagnostic scene derived from a committed snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugScene {
    pub snapshot: SnapshotId,
    pub semantic_digest: SemanticDigest,
    pub triangles: Vec<DebugTriangle>,
    pub edges: Vec<DebugEdge>,
    pub vertices: Vec<DebugVertex>,
    /// One entry per curved face, for the per-frame silhouette pass.
    pub carriers: Vec<DisplayCarrier>,
}

/// One exact face-boundary curve expressed in that face's own planar frame.
///
/// Every edge of a planar face lies in the face plane, so the frame projection
/// is an isometry and this carries the edge's analytic form rather than a
/// chord approximation. Snapping and inference can therefore report a hole's
/// true centre and quadrants instead of fitting sampled points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FaceBoundaryCurve2 {
    Segment {
        endpoints: [ProtocolPoint2; 2],
    },
    /// `center + radius * (u cos t + v sin t)` over `[start, end]`, with `u`
    /// and `v` orthonormal in the face frame. A full turn marks a whole circle.
    Arc {
        center: ProtocolPoint2,
        u: [f64; 2],
        v: [f64; 2],
        radius: f64,
        start: f64,
        end: f64,
    },
}

impl FaceBoundaryCurve2 {
    #[must_use]
    pub fn evaluate(self, parameter: f64) -> ProtocolPoint2 {
        match self {
            Self::Segment { endpoints } => ProtocolPoint2::new(
                (endpoints[1].x - endpoints[0].x).mul_add(parameter, endpoints[0].x),
                (endpoints[1].y - endpoints[0].y).mul_add(parameter, endpoints[0].y),
            ),
            Self::Arc {
                center,
                u,
                v,
                radius,
                ..
            } => {
                let (sine, cosine) = parameter.sin_cos();
                ProtocolPoint2::new(
                    radius.mul_add(v[0].mul_add(sine, u[0] * cosine), center.x),
                    radius.mul_add(v[1].mul_add(sine, u[1] * cosine), center.y),
                )
            }
        }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        match self {
            Self::Segment { endpoints } => endpoints
                .into_iter()
                .all(|point| point.x.is_finite() && point.y.is_finite()),
            Self::Arc {
                center,
                u,
                v,
                radius,
                start,
                end,
            } => {
                center.x.is_finite()
                    && center.y.is_finite()
                    && u.into_iter().all(f64::is_finite)
                    && v.into_iter().all(f64::is_finite)
                    && radius.is_finite()
                    && start.is_finite()
                    && end.is_finite()
            }
        }
    }
}

/// Exact read-only placement and boundary for a planar B-rep face.
///
/// Sketches may use this local two-dimensional frame without treating debug
/// tessellation as modeling truth. The face reference and support digest bind
/// the result to the immutable snapshot from which it was queried.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanarFaceSupport {
    pub face: EntityRef,
    pub frame: PlanarFrame3,
    pub boundary: Vec<ProtocolPoint2>,
    /// Ordered void boundaries owned by this face. Their winding is opposite
    /// to `boundary`, so sketch/profile consumers can preserve face domains
    /// without consulting diagnostic tessellation.
    pub inner_boundaries: Vec<Vec<ProtocolPoint2>>,
    /// The outer loop's analytic curves, in `boundary` coedge order.
    ///
    /// This is the same loop as `boundary` without the chord budget. Callers
    /// that need exact reference geometry — snapping, inference, measurement
    /// readouts — use this; callers that need a polygon keep `boundary`.
    pub boundary_curves: Vec<FaceBoundaryCurve2>,
    /// Analytic curves for each entry of `inner_boundaries`, in the same order.
    pub inner_boundary_curves: Vec<Vec<FaceBoundaryCurve2>>,
    /// Whether the selected face's owning solid can enter the current
    /// reconstructive LINEAR Add/Cut path. Analytic-owner circles use a
    /// separate local rewrite and may remain supported when this is false.
    pub linear_profile_extrusion_supported: bool,
    pub support_digest: SemanticDigest,
}

/// Native kernel facade. It is stateless; all state is carried by immutable snapshots.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeKernel;

impl NativeKernel {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Returns the stable initial snapshot. Its precision policy is bound by
    /// the first successful operation.
    #[must_use]
    pub fn empty() -> Snapshot {
        Snapshot {
            id: SnapshotId::ZERO,
            semantic_digest: digest_bytes(b"artificer.native.empty.v0"),
            precision: None,
            topology: Topology::default(),
            measures: SnapshotMeasures::default(),
        }
    }

    /// Executes one command transactionally against `input`.
    pub fn execute(
        input: &Snapshot,
        request: &ExecuteRequest,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionOutcome, KernelError> {
        if request.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(error(
                KernelErrorCode::Unsupported,
                KernelStage::Protocol,
                input.id,
                format!(
                    "protocol version {} is unsupported; expected {}",
                    request.protocol_version, CURRENT_PROTOCOL_VERSION
                ),
                vec![simple_diagnostic(
                    "PROTOCOL_VERSION_UNSUPPORTED",
                    KernelStage::Protocol,
                    "The request protocol version is not supported by this kernel build.",
                )],
            ));
        }
        check_cancelled(input.id, cancellation, KernelStage::Preflight)?;
        if request.expected_snapshot != input.id {
            let mut result = error(
                KernelErrorCode::StaleSnapshot,
                KernelStage::Preflight,
                input.id,
                "the command targets a stale snapshot",
                vec![simple_diagnostic(
                    "STALE_SNAPSHOT",
                    KernelStage::Preflight,
                    "Expected snapshot does not match the supplied immutable input.",
                )],
            );
            result.details.insert(
                "expected_snapshot".to_owned(),
                request.expected_snapshot.to_string(),
            );
            result
                .details
                .insert("actual_snapshot".to_owned(), input.id.to_string());
            return Err(result);
        }
        validate_precision(input.id, request.precision)?;
        if input
            .precision
            .is_some_and(|precision| precision != request.precision)
        {
            return Err(error(
                KernelErrorCode::PrecisionPolicyMismatch,
                KernelStage::Preflight,
                input.id,
                "precision policy cannot change within a snapshot lineage",
                vec![simple_diagnostic(
                    "PRECISION_POLICY_MISMATCH",
                    KernelStage::Preflight,
                    "The request precision policy differs from the input snapshot policy.",
                )],
            ));
        }

        // Warnings a construction rung wants the caller to see. Most rungs
        // publish none: a certified result needs no caveat. The faceted
        // fallback does, because its answer is an approximation and nothing
        // else in the report distinguishes it from an exact one.
        let mut warnings = Vec::new();
        let (topology, history_mode) = match &request.command {
            KernelCommand::MakeCuboid {
                origin,
                size_x,
                size_y,
                size_z,
            } => {
                validate_cuboid_input(
                    input.id,
                    *origin,
                    [*size_x, *size_y, *size_z],
                    request.precision,
                )?;
                (
                    build_cuboid(
                        internal_point(*origin),
                        Vector3::new(*size_x, *size_y, *size_z),
                    ),
                    HistoryMode::Generated,
                )
            }
            KernelCommand::MakeRevolvedAnnulus {
                frame,
                inner_radius,
                outer_radius,
                height,
            } => {
                validate_extrusion_source(input)?;
                if *inner_radius < 0.0 || *outer_radius <= *inner_radius || *height <= 0.0 {
                    return Err(simple_invalid_input(
                        input.id,
                        "REVOLVE_ANNULUS_DIMENSIONS_INVALID",
                        "Revolve radii and height must define a positive radial section.",
                    ));
                }
                let center = ProtocolPoint2::new(0.0, 0.0);
                let profile = PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: PlanarLoop2 {
                            curves: vec![PlanarCurve2::Circle {
                                center,
                                radius: *outer_radius,
                                direction: ArcDirection::CounterClockwise,
                            }],
                        },
                        holes: (*inner_radius > request.precision.min_feature_size)
                            .then(|| PlanarLoop2 {
                                curves: vec![PlanarCurve2::Circle {
                                    center,
                                    radius: *inner_radius,
                                    direction: ArcDirection::Clockwise,
                                }],
                            })
                            .into_iter()
                            .collect(),
                    }],
                };
                let extrusion = validate_analytic_profile_extrusion(
                    *frame,
                    &profile,
                    *height,
                    request.precision,
                )
                .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                (build_analytic_extrusion(&extrusion), HistoryMode::Generated)
            }
            KernelCommand::RevolvePlanarProfile {
                frame,
                profile,
                axis,
                angle,
            } => {
                validate_extrusion_source(input)?;
                let revolved =
                    revolve::validate_revolve(*frame, profile, *axis, *angle, request.precision)
                        .map_err(|reason| revolve_input_error(input.id, reason))?;
                (revolve::build_revolve(&revolved), HistoryMode::Generated)
            }
            KernelCommand::TransformSnapshot { transform } => {
                validate_transform_source(input)?;
                let similarity = validate_transform_input(input.id, *transform)?;
                let candidate = transform_topology(&input.topology, similarity);
                validate_transform_candidate(
                    input.id,
                    &input.topology,
                    &candidate,
                    similarity,
                    request.precision,
                )?;
                (candidate, HistoryMode::OneToOne)
            }
            KernelCommand::ExtrudePolygon {
                frame,
                vertices,
                distance,
            } => {
                validate_extrusion_source(input)?;
                let extrusion =
                    validate_extrusion_input(*frame, vertices, *distance, request.precision)
                        .map_err(|reason| extrusion_input_error(input.id, reason))?;
                let profile_vertices = extrusion.vertex_count();
                (
                    build_extrusion(&extrusion),
                    HistoryMode::Extrusion { profile_vertices },
                )
            }
            KernelCommand::ExtrudePlanarProfile {
                frame,
                profile,
                distance,
            } => {
                validate_extrusion_source(input)?;
                if profile_contains_analytic_curves(profile) {
                    let extrusion = validate_analytic_profile_extrusion(
                        *frame,
                        profile,
                        *distance,
                        request.precision,
                    )
                    .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                    (build_analytic_extrusion(&extrusion), HistoryMode::Generated)
                } else {
                    let extrusion = validate_linear_profile_extrusion(
                        input.id,
                        *frame,
                        profile,
                        *distance,
                        request.precision,
                    )
                    .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                    (extrusion.topology, HistoryMode::Generated)
                }
            }
            KernelCommand::LoftPlanarProfileOffset {
                frame,
                profile,
                distance,
                offset,
            } => {
                validate_extrusion_source(input)?;
                let minimum = request
                    .precision
                    .modeling_resolution
                    .max(request.precision.min_feature_size);
                if offset.abs() <= minimum {
                    // No draft is a straight extrusion; build it as one so the
                    // walls are the cylinders and planes an extrusion makes.
                    let extrusion = validate_analytic_profile_extrusion(
                        *frame,
                        profile,
                        *distance,
                        request.precision,
                    )
                    .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                    (build_analytic_extrusion(&extrusion), HistoryMode::Generated)
                } else {
                    let loft = loft::validate_offset_loft(
                        *frame,
                        profile,
                        *distance,
                        *offset,
                        request.precision,
                    )
                    .map_err(|reason| loft_input_error(input.id, reason))?;
                    (loft::build_offset_loft(&loft), HistoryMode::Generated)
                }
            }
            KernelCommand::ExtrudeFaceProfile {
                target_face,
                frame,
                vertices,
                distance,
                operation,
            } => {
                let feature = validate_face_feature_input(FaceFeatureArguments {
                    snapshot: input.id,
                    topology: &input.topology,
                    target_face: *target_face,
                    frame: *frame,
                    vertices,
                    distance: *distance,
                    operation: *operation,
                    precision: request.precision,
                })
                .map_err(|reason| face_feature_input_error(input.id, reason))?;
                let exit_face = feature.exit_face_index.map(|index| {
                    entity_ref(
                        input.id,
                        input.topology.faces[index].id.get(),
                        EntityKind::Face,
                    )
                });
                (
                    build_face_feature(&feature),
                    HistoryMode::FaceFeature {
                        operation: *operation,
                        target_face: *target_face,
                        exit_face,
                    },
                )
            }
            KernelCommand::ExtrudeFacePlanarProfile {
                target_face,
                frame,
                profile,
                distance,
                operation,
            } => {
                let feature = validate_exact_face_feature(
                    input.id,
                    &input.topology,
                    *target_face,
                    *frame,
                    profile,
                    *distance,
                    *operation,
                    request.precision,
                );
                let (topology, exit_face, regularized) = match feature {
                    Ok(feature) => {
                        let exit_face = feature.exit_face_index.map(|index| {
                            entity_ref(
                                input.id,
                                input.topology.faces[index].id.get(),
                                EntityKind::Face,
                            )
                        });
                        (feature.topology, exit_face, false)
                    }
                    Err(PlanarProfileInputError::FaceFeature(
                        FaceFeatureInputError::SweepCollision,
                    )) if *operation == FaceExtrusionOperation::Cut => {
                        let target_index = input
                            .topology
                            .faces
                            .iter()
                            .position(|face| face.id.get() == target_face.entity.0)
                            .ok_or_else(|| {
                                planar_profile_input_error(
                                    input.id,
                                    PlanarProfileInputError::FaceFeature(
                                        FaceFeatureInputError::TargetMissing,
                                    ),
                                )
                            })?;
                        let plane = input.topology.faces[target_index]
                            .value
                            .surface
                            .as_plane()
                            .ok_or_else(|| {
                                planar_profile_input_error(
                                    input.id,
                                    PlanarProfileInputError::FaceFeature(
                                        FaceFeatureInputError::TargetNotPlanar,
                                    ),
                                )
                            })?;
                        let normal_length = plane.normal.length();
                        if !normal_length.is_finite() || normal_length <= f64::EPSILON {
                            return Err(planar_profile_input_error(
                                input.id,
                                PlanarProfileInputError::FaceFeature(
                                    FaceFeatureInputError::TargetDegenerate,
                                ),
                            ));
                        }
                        // Crossing curved voids use the faceted Boolean tier.
                        // The ordinary display tessellation may contain
                        // thousands of triangles for a single circle and is
                        // an unsuitable Boolean operand (two crossed bores
                        // previously caused explosive BSP fragmentation).
                        // Bound this construction mesh independently; the
                        // immutable analytic predecessor remains untouched.
                        let mut boolean_input = input.clone();
                        let mut boolean_precision = request.precision;
                        boolean_precision.max_subdivisions =
                            boolean_precision.max_subdivisions.min(4);
                        boolean_input.precision = Some(boolean_precision);
                        let scene = NativeKernel::authoritative_scene(&boolean_input);
                        let topology = faceted_boolean::subtract_crossing_profile(
                            &scene,
                            *frame,
                            profile,
                            plane.normal / normal_length * -1.0,
                            *distance,
                            // NOTE: this is deliberately the request's budget,
                            // not the clamped `boolean_precision` above, even
                            // though the comment on that clamp reads as though
                            // both meshes should share it. Handing the cutter
                            // the clamped budget halves the fragmentation
                            // (2959 -> 1551 faces) but drops one bore's panel
                            // fan below the eight-normal threshold in
                            // `presentation_prismatic_feature_roles`, so its
                            // seams stop being recognised as one logical
                            // cylinder and are drawn as creases instead. Change
                            // it together with the coplanar merge that removes
                            // the fan altogether, not before.
                            request.precision,
                        )
                        .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                        certify_faceted_candidate(input.id, &topology, request.precision)?;
                        // The result is a tessellation, not a certified solid.
                        // Say so: every other report this kernel publishes means
                        // "exact", so a caller with no way to tell the
                        // difference will quote this body's volume as though it
                        // were.
                        warnings.push(faceted_cut_warning());
                        (topology, None, true)
                    }
                    Err(PlanarProfileInputError::FaceFeature(
                        FaceFeatureInputError::ProfileOutsideFace,
                    )) if input.topology.solids.len() == 1 => {
                        // The profile crosses the face boundary. That is not
                        // an error of intent — half a circle over the edge is
                        // an everyday boss or notch — so the operation is
                        // reformulated exactly: the profile becomes a prism
                        // tool and the whole solid the boolean target. A cut
                        // becomes the difference the crossing-pocket engine
                        // already certifies; an add becomes the stacked boss
                        // glued at the face plane.
                        let normal = {
                            let u = frame.u;
                            let v = frame.v;
                            ProtocolVector3::new(
                                u.y * v.z - u.z * v.y,
                                u.z * v.x - u.x * v.z,
                                u.x * v.y - u.y * v.x,
                            )
                        };
                        let length =
                            (normal.x * normal.x + normal.y * normal.y + normal.z * normal.z)
                                .sqrt();
                        if !length.is_finite() || length <= f64::EPSILON {
                            return Err(planar_profile_input_error(
                                input.id,
                                PlanarProfileInputError::FaceFeature(
                                    FaceFeatureInputError::TargetDegenerate,
                                ),
                            ));
                        }
                        let unit = ProtocolVector3::new(
                            normal.x / length,
                            normal.y / length,
                            normal.z / length,
                        );
                        // A cut's tool occupies the space below the face; an
                        // add's tool the space above. Both extrude along the
                        // frame normal, so the cut simply starts deeper.
                        let tool_origin = match operation {
                            FaceExtrusionOperation::Cut => ProtocolPoint3::new(
                                frame.origin.x - unit.x * *distance,
                                frame.origin.y - unit.y * *distance,
                                frame.origin.z - unit.z * *distance,
                            ),
                            FaceExtrusionOperation::Add => frame.origin,
                        };
                        let tool_frame = PlanarFrame3::new(tool_origin, frame.u, frame.v);
                        let tool = validate_analytic_profile_extrusion(
                            tool_frame,
                            profile,
                            *distance,
                            request.precision,
                        )
                        .map_err(|_| {
                            planar_profile_input_error(
                                input.id,
                                PlanarProfileInputError::FaceFeature(
                                    FaceFeatureInputError::ProfileOutsideFace,
                                ),
                            )
                        })?;
                        let tool = build_analytic_extrusion(&tool);
                        let boolean_operation = match operation {
                            FaceExtrusionOperation::Add => BooleanOperation::Union,
                            FaceExtrusionOperation::Cut => BooleanOperation::Difference,
                        };
                        let topology = match prism_boolean::build_prism_boolean(
                            &input.topology,
                            &tool,
                            boolean_operation,
                            request.precision,
                        ) {
                            Ok(topology) => topology,
                            Err(_) if *operation == FaceExtrusionOperation::Cut => {
                                let mut boolean_input = input.clone();
                                let mut boolean_precision = request.precision;
                                boolean_precision.max_subdivisions =
                                    boolean_precision.max_subdivisions.min(4);
                                boolean_input.precision = Some(boolean_precision);
                                let scene = NativeKernel::authoritative_scene(&boolean_input);
                                let topology = faceted_boolean::subtract_crossing_profile(
                                    &scene,
                                    *frame,
                                    profile,
                                    Vector3::new(-unit.x, -unit.y, -unit.z),
                                    *distance,
                                    request.precision,
                                )
                                .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                                certify_faceted_candidate(input.id, &topology, request.precision)?;
                                warnings.push(faceted_cut_warning());
                                topology
                            }
                            Err(_) => {
                                return Err(planar_profile_input_error(
                                    input.id,
                                    PlanarProfileInputError::FaceFeature(
                                        FaceFeatureInputError::ProfileOutsideFace,
                                    ),
                                ));
                            }
                        };
                        (topology, None, true)
                    }
                    Err(reason) => {
                        return Err(planar_profile_input_error(input.id, reason));
                    }
                };
                (
                    topology,
                    if regularized {
                        HistoryMode::RegularizedFaceFeature
                    } else {
                        HistoryMode::FaceFeature {
                            operation: *operation,
                            target_face: *target_face,
                            exit_face,
                        }
                    },
                )
            }
            KernelCommand::PushPullFace {
                target_face,
                distance,
            } => {
                let push_pull = validate_face_push_pull_input(FacePushPullArguments {
                    snapshot: input.id,
                    topology: &input.topology,
                    target_face: *target_face,
                    distance: *distance,
                    precision: request.precision,
                })
                .map_err(|reason| face_push_pull_input_error(input.id, reason))?;
                (
                    build_face_push_pull(&input.topology, &push_pull),
                    HistoryMode::FacePushPull {
                        target_face: *target_face,
                    },
                )
            }
            KernelCommand::DrillHole {
                target_face,
                frame,
                center,
                diameter,
                depth,
            } => {
                if *diameter <= request.precision.min_feature_size * 2.0 {
                    return Err(simple_invalid_input(
                        input.id,
                        "HOLE_DIAMETER_INVALID",
                        "Hole diameter must exceed twice the minimum feature size.",
                    ));
                }
                let profile = circle_profile(*center, *diameter * 0.5);
                let feature = validate_exact_face_feature(
                    input.id,
                    &input.topology,
                    *target_face,
                    *frame,
                    &profile,
                    *depth,
                    FaceExtrusionOperation::Cut,
                    request.precision,
                )
                .map_err(|reason| planar_profile_input_error(input.id, reason))?;
                (feature.topology, HistoryMode::RegularizedFaceFeature)
            }
            KernelCommand::AddRib {
                target_face,
                frame,
                start,
                end,
                thickness,
                height,
            } => {
                let du = end.x - start.x;
                let dv = end.y - start.y;
                let length = du.hypot(dv);
                if length <= request.precision.min_feature_size
                    || *thickness <= request.precision.min_feature_size
                {
                    return Err(simple_invalid_input(
                        input.id,
                        "RIB_SECTION_INVALID",
                        "Rib centre line and thickness must define a positive section.",
                    ));
                }
                let offset = (
                    -dv / length * *thickness * 0.5,
                    du / length * *thickness * 0.5,
                );
                let vertices = vec![
                    ProtocolPoint2::new(start.x + offset.0, start.y + offset.1),
                    ProtocolPoint2::new(end.x + offset.0, end.y + offset.1),
                    ProtocolPoint2::new(end.x - offset.0, end.y - offset.1),
                    ProtocolPoint2::new(start.x - offset.0, start.y - offset.1),
                ];
                let feature = validate_face_feature_input(FaceFeatureArguments {
                    snapshot: input.id,
                    topology: &input.topology,
                    target_face: *target_face,
                    frame: *frame,
                    vertices: &vertices,
                    distance: *height,
                    operation: FaceExtrusionOperation::Add,
                    precision: request.precision,
                })
                .map_err(|reason| face_feature_input_error(input.id, reason))?;
                (
                    build_face_feature(&feature),
                    HistoryMode::RegularizedFaceFeature,
                )
            }
            KernelCommand::MirrorSnapshot {
                plane_origin,
                plane_normal,
            } => {
                validate_transform_source(input)?;
                let normal = Vector3::new(plane_normal.x, plane_normal.y, plane_normal.z);
                if normal.length() <= request.precision.angular_agreement_radians
                    || input
                        .topology
                        .faces
                        .iter()
                        .any(|face| !matches!(face.value.surface, Surface::Plane(_)))
                {
                    return Err(simple_invalid_input(
                        input.id,
                        "MIRROR_DOMAIN_UNSUPPORTED",
                        "Mirror currently requires a non-zero plane normal and an all-planar body.",
                    ));
                }
                let topology = faceted_boolean::mirror_scene(
                    &Self::authoritative_scene(input),
                    internal_point(*plane_origin),
                    normal,
                    request.precision,
                )
                .ok_or_else(|| {
                    simple_invalid_input(input.id, "MIRROR_FAILED", "Mirror produced no solid.")
                })?;
                (topology, HistoryMode::RegularizedFaceFeature)
            }
            KernelCommand::LinearPatternSnapshot {
                direction,
                spacing,
                count,
            } => {
                validate_transform_source(input)?;
                let direction = Vector3::new(direction.x, direction.y, direction.z);
                if *count < 2
                    || *count > 128
                    || *spacing <= request.precision.min_feature_size
                    || direction.length() <= request.precision.angular_agreement_radians
                    || input
                        .topology
                        .faces
                        .iter()
                        .any(|face| !matches!(face.value.surface, Surface::Plane(_)))
                {
                    return Err(simple_invalid_input(
                        input.id,
                        "LINEAR_PATTERN_DOMAIN_UNSUPPORTED",
                        "Pattern requires 2..=128 separated copies of an all-planar body.",
                    ));
                }
                let topology = faceted_boolean::linear_pattern_scene(
                    &Self::authoritative_scene(input),
                    direction,
                    *spacing,
                    *count,
                    request.precision,
                )
                .ok_or_else(|| {
                    simple_invalid_input(
                        input.id,
                        "LINEAR_PATTERN_FAILED",
                        "Pattern produced no solid.",
                    )
                })?;
                (topology, HistoryMode::RegularizedFaceFeature)
            }
            KernelCommand::FinishEdge {
                target_edge,
                kind,
                distance,
            } => {
                let analytic = edge_finish::build_edge_finish(
                    input.id,
                    &input.topology,
                    *target_edge,
                    *kind,
                    *distance,
                    request.precision,
                );
                let topology = match analytic {
                    Ok(topology) => topology,
                    Err(edge_finish::EdgeFinishError::DomainUnsupported) => {
                        regularized_edge_finish(
                            input,
                            &[*target_edge],
                            *kind,
                            *distance,
                            request.precision,
                            &mut warnings,
                        )?
                    }
                    Err(reason) => return Err(edge_finish_error(input.id, reason, false)),
                };
                (topology, HistoryMode::RegularizedFaceFeature)
            }
            KernelCommand::FinishEdges {
                target_edges,
                kind,
                distance,
            } => {
                let analytic = edge_finish::build_edge_finishes(
                    input.id,
                    &input.topology,
                    target_edges,
                    *kind,
                    *distance,
                    request.precision,
                );
                let topology = match analytic {
                    Ok(topology) => topology,
                    Err(edge_finish::EdgeFinishError::DomainUnsupported) => {
                        regularized_edge_finish(
                            input,
                            target_edges,
                            *kind,
                            *distance,
                            request.precision,
                            &mut warnings,
                        )?
                    }
                    Err(reason) => return Err(edge_finish_error(input.id, reason, true)),
                };
                (topology, HistoryMode::RegularizedFaceFeature)
            }
        };

        check_cancelled(input.id, cancellation, KernelStage::Construction)?;
        let internal_validation =
            validator::validate(&topology, request.precision.linear_agreement);
        let validation =
            protocol_validation(input.id, ValidationProfile::Solid, &internal_validation);
        if !validation.valid {
            return Err(error(
                KernelErrorCode::ValidationFailed,
                KernelStage::Validation,
                input.id,
                "candidate topology failed validation and was not committed",
                validation.diagnostics,
            ));
        }
        check_cancelled(input.id, cancellation, KernelStage::Commit)?;

        let semantic_digest = semantic_digest(&topology, request.precision);
        let output_snapshot = snapshot_id(semantic_digest);
        let measures = public_measures(internal_validation.measures);
        let snapshot = Snapshot {
            id: output_snapshot,
            semantic_digest,
            precision: Some(request.precision),
            topology,
            measures,
        };
        let mut report = OperationReport {
            input_snapshot: input.id,
            output_snapshot,
            semantic_digest,
            topology: snapshot.counts(),
            bounds: measures.bounds,
            history: match history_mode {
                HistoryMode::Generated => generated_history(&snapshot),
                HistoryMode::OneToOne => transformed_history(input, &snapshot),
                HistoryMode::Extrusion { profile_vertices } => {
                    extrusion_history(&snapshot, profile_vertices)
                }
                HistoryMode::FaceFeature {
                    operation,
                    target_face,
                    exit_face,
                } => face_feature_history(input, &snapshot, target_face, exit_face, operation)?,
                HistoryMode::RegularizedFaceFeature => {
                    regularized_face_feature_history(input, &snapshot)
                }
                HistoryMode::FacePushPull { target_face } => {
                    face_push_pull_history(input, &snapshot, target_face)?
                }
            },
            validation,
            warnings,
        };
        report.sort_deterministically();
        Ok(ExecutionOutcome { snapshot, report })
    }

    /// Executes a regularized Boolean between two immutable planar B-reps.
    /// This first production domain deliberately rejects analytic curved
    /// surfaces instead of polygonizing them silently.
    pub fn execute_boolean(
        target: &Snapshot,
        tool: &Snapshot,
        request: &BooleanRequest,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionOutcome, KernelError> {
        if request.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(error(
                KernelErrorCode::Unsupported,
                KernelStage::Protocol,
                target.id,
                "the Boolean request uses an unsupported protocol version",
                vec![simple_diagnostic(
                    "PROTOCOL_VERSION_UNSUPPORTED",
                    KernelStage::Protocol,
                    "The Boolean request protocol version is not supported by this kernel build.",
                )],
            ));
        }
        check_cancelled(target.id, cancellation, KernelStage::Preflight)?;
        if request.expected_target_snapshot != target.id
            || request.expected_tool_snapshot != tool.id
        {
            return Err(error(
                KernelErrorCode::StaleSnapshot,
                KernelStage::Preflight,
                target.id,
                "a Boolean operand is stale",
                vec![simple_diagnostic(
                    "BOOLEAN_STALE_OPERAND",
                    KernelStage::Preflight,
                    "Both target and tool snapshots must match the explicitly staged operands.",
                )],
            ));
        }
        validate_precision(target.id, request.precision)?;
        if target.precision != Some(request.precision) || tool.precision != Some(request.precision)
        {
            return Err(error(
                KernelErrorCode::PrecisionPolicyMismatch,
                KernelStage::Preflight,
                target.id,
                "Boolean operands must share one precision policy",
                vec![simple_diagnostic(
                    "BOOLEAN_PRECISION_POLICY_MISMATCH",
                    KernelStage::Preflight,
                    "Target, tool, and request must use the same persisted precision policy.",
                )],
            ));
        }
        if target.topology.solids.is_empty() || tool.topology.solids.is_empty() {
            return Err(error(
                KernelErrorCode::InvalidInput,
                KernelStage::Preflight,
                target.id,
                "Boolean operands must contain solid material",
                vec![simple_diagnostic(
                    "BOOLEAN_EMPTY_OPERAND",
                    KernelStage::Preflight,
                    "An empty snapshot cannot be used as a Boolean target or tool.",
                )],
            ));
        }
        // The exact prism path first: when both operands are prisms along one
        // direction with compatible slabs, the Boolean reduces to a certified
        // 2D profile Boolean and rebuilds through the analytic extrusion
        // path — curved walls included, no tessellation anywhere.
        let analytic = perf_span!(
            "kernel.boolean.prism",
            target.topology.faces.len() + tool.topology.faces.len(),
            {
                prism_boolean::build_prism_boolean(
                    &target.topology,
                    &tool.topology,
                    request.operation,
                    request.precision,
                )
            }
        );
        if matches!(analytic, Err(prism_boolean::PrismBooleanError::EmptyResult)) {
            return Err(error(
                KernelErrorCode::Unsupported,
                KernelStage::Construction,
                target.id,
                "the Boolean has no regularized solid result",
                vec![simple_diagnostic(
                    "BOOLEAN_EMPTY_OR_UNRESOLVED_RESULT",
                    KernelStage::Construction,
                    "The selected operation produced no publishable closed component.",
                )],
            ));
        }

        // Beyond the prism reductions, the general analytic engine runs the
        // full imprint/classify/regularize/sew pipeline for operands whose
        // faces it can carry. Everything is exact; nothing tessellates.
        let topology = match analytic {
            Ok(topology) => topology,
            Err(_) => {
                if analytic_boolean::operands_in_engine_vocabulary(&target.topology, &tool.topology)
                {
                    match perf_span!(
                        "kernel.boolean.analytic",
                        target.topology.faces.len() + tool.topology.faces.len(),
                        {
                            analytic_boolean::build_analytic_boolean(
                                &target.topology,
                                &tool.topology,
                                request.operation,
                                request.precision,
                            )
                        }
                    ) {
                        Ok(topology) => topology,
                        Err(analytic_boolean::AnalyticBooleanError::EmptyResult) => {
                            return Err(error(
                                KernelErrorCode::Unsupported,
                                KernelStage::Construction,
                                target.id,
                                "the Boolean has no regularized solid result",
                                vec![simple_diagnostic(
                                    "BOOLEAN_EMPTY_OR_UNRESOLVED_RESULT",
                                    KernelStage::Construction,
                                    "The selected operation produced no publishable closed component.",
                                )],
                            ));
                        }
                        Err(analytic_boolean::AnalyticBooleanError::DomainUnsupported) => {
                            // An out-of-matrix carrier pair is a vocabulary
                            // limit and says so; anything else the engine
                            // refuses is a contact outside the transverse
                            // domain.
                            let carriers = |snapshot: &Snapshot| {
                                snapshot
                                    .topology
                                    .faces
                                    .iter()
                                    .map(|face| face.value.surface)
                                    .collect::<Vec<_>>()
                            };
                            let (code, message) =
                                match surface_intersection::first_unsupported_pair(
                                    &carriers(target),
                                    &carriers(tool),
                                    request.precision,
                                ) {
                                    Some((first, second)) => (
                                        "BOOLEAN_SURFACE_PAIR_UNSUPPORTED",
                                        format!(
                                            "The {} and {} carriers meet in a curve outside this kernel's line and circle vocabulary.",
                                            surface_intersection::surface_name(first),
                                            surface_intersection::surface_name(second)
                                        ),
                                    ),
                                    None => (
                                        "BOOLEAN_CONTACT_UNSUPPORTED",
                                        "The operands meet tangentially, share coincident geometry, or otherwise leave the transverse-contact domain; regularized reconstruction refuses rather than guesses."
                                            .to_owned(),
                                    ),
                                };
                            return Err(error(
                                KernelErrorCode::Unsupported,
                                KernelStage::Construction,
                                target.id,
                                "the Boolean operands leave the regularized analytic domain",
                                vec![simple_diagnostic(code, KernelStage::Construction, &message)],
                            ));
                        }
                    }
                } else {
                    // Faces the engine cannot carry: name the first carrier
                    // pair outside the intersection matrix, or admit the
                    // configuration is expressible but not yet reconstructed.
                    let carriers = |snapshot: &Snapshot| {
                        snapshot
                            .topology
                            .faces
                            .iter()
                            .map(|face| face.value.surface)
                            .collect::<Vec<_>>()
                    };
                    let (code, message) = match surface_intersection::first_unsupported_pair(
                        &carriers(target),
                        &carriers(tool),
                        request.precision,
                    ) {
                        Some((first, second)) => (
                            "BOOLEAN_SURFACE_PAIR_UNSUPPORTED",
                            format!(
                                "The {} and {} carriers meet in a curve outside this kernel's line and circle vocabulary.",
                                surface_intersection::surface_name(first),
                                surface_intersection::surface_name(second)
                            ),
                        ),
                        None => (
                            "BOOLEAN_ANALYTIC_RECONSTRUCTION_PENDING",
                            "Every carrier pair intersects exactly, but reconstruction over this face class is not implemented yet."
                                .to_owned(),
                        ),
                    };
                    return Err(error(
                        KernelErrorCode::Unsupported,
                        KernelStage::Preflight,
                        target.id,
                        "this Boolean configuration is outside the analytic domain",
                        vec![simple_diagnostic(code, KernelStage::Preflight, &message)],
                    ));
                }
            }
        };
        check_cancelled(target.id, cancellation, KernelStage::Construction)?;
        let internal_validation =
            validator::validate(&topology, request.precision.linear_agreement);
        let validation =
            protocol_validation(target.id, ValidationProfile::Solid, &internal_validation);
        if !validation.valid {
            return Err(error(
                KernelErrorCode::ValidationFailed,
                KernelStage::Validation,
                target.id,
                "Boolean candidate failed solid validation",
                validation.diagnostics,
            ));
        }
        check_cancelled(target.id, cancellation, KernelStage::Commit)?;
        let semantic_digest = semantic_digest(&topology, request.precision);
        let output_snapshot = snapshot_id(semantic_digest);
        let measures = public_measures(internal_validation.measures);
        let snapshot = Snapshot {
            id: output_snapshot,
            semantic_digest,
            precision: Some(request.precision),
            topology,
            measures,
        };
        let mut history = regularized_face_feature_history(target, &snapshot);
        history.extend(boolean_tool_deleted_history(tool));
        let mut report = OperationReport {
            input_snapshot: target.id,
            output_snapshot,
            semantic_digest,
            topology: snapshot.counts(),
            bounds: measures.bounds,
            history,
            validation,
            warnings: Vec::new(),
        };
        report.sort_deterministically();
        Ok(ExecutionOutcome { snapshot, report })
    }

    /// Splits the target by the tool and returns the two independently valid
    /// successor snapshots without mutating either input.
    pub fn split_boolean(
        target: &Snapshot,
        tool: &Snapshot,
        request: &BooleanRequest,
        cancellation: &CancellationToken,
    ) -> Result<BooleanSplitOutcome, KernelError> {
        let mut remainder_request = request.clone();
        remainder_request.operation = BooleanOperation::Difference;
        let remainder = Self::execute_boolean(target, tool, &remainder_request, cancellation)?;
        let mut overlap_request = request.clone();
        overlap_request.operation = BooleanOperation::Intersection;
        let overlap = Self::execute_boolean(target, tool, &overlap_request, cancellation)?;
        Ok(BooleanSplitOutcome { remainder, overlap })
    }

    #[must_use]
    pub fn validate(snapshot: &Snapshot, profile: ValidationProfile) -> ProtocolValidationReport {
        Self::validate_with_pool(ComputePool::global(), snapshot, profile)
    }

    #[must_use]
    pub fn validate_with_pool(
        compute: &ComputePool,
        snapshot: &Snapshot,
        profile: ValidationProfile,
    ) -> ProtocolValidationReport {
        let tolerance = snapshot.precision.unwrap_or_default().linear_agreement;
        let report = validator::validate_with_pool(compute, &snapshot.topology, tolerance);
        protocol_validation(snapshot.id, profile, &report)
    }

    /// Executes mutually independent snapshot commands concurrently. Results
    /// retain their input positions; each transaction still validates and
    /// commits atomically inside its own immutable branch.
    pub fn execute_batch(
        compute: &ComputePool,
        batch: &[(Snapshot, ExecuteRequest)],
        cancellation: &CancellationToken,
    ) -> Vec<Result<ExecutionOutcome, KernelError>> {
        compute.map(
            "kernel.execute.independent",
            batch,
            |_, (snapshot, request)| Self::execute(snapshot, request, cancellation),
        )
    }

    /// Returns an exact face-owned sketch support without exposing private
    /// topology storage or depending on the debug-scene tessellation.
    pub fn planar_face_support(
        snapshot: &Snapshot,
        face: EntityRef,
    ) -> Result<PlanarFaceSupport, KernelError> {
        if face.snapshot != snapshot.id || face.kind != EntityKind::Face {
            return Err(error(
                KernelErrorCode::InvalidInput,
                KernelStage::Preflight,
                snapshot.id,
                "the requested sketch support is not a face in this snapshot",
                vec![simple_diagnostic(
                    "PLANAR_SUPPORT_REFERENCE_INVALID",
                    KernelStage::Preflight,
                    "A planar support query requires a face reference owned by the supplied snapshot.",
                )],
            ));
        }
        let record = snapshot
            .topology
            .faces
            .iter()
            .find(|record| record.id.get() == face.entity.0)
            .ok_or_else(|| {
                error(
                    KernelErrorCode::InvalidInput,
                    KernelStage::Preflight,
                    snapshot.id,
                    "the requested face does not exist in this snapshot",
                    vec![simple_diagnostic(
                        "PLANAR_SUPPORT_FACE_MISSING",
                        KernelStage::Preflight,
                        "The face identifier could not be resolved in the supplied immutable snapshot.",
                    )],
                )
            })?;
        let precision = snapshot.precision.unwrap_or_default();
        let polygon = sampled_loop_polygon(
            &snapshot.topology,
            record.value.outer_loop,
            ChordBudget::Authoritative,
            precision,
        )
        .filter(|polygon| polygon.len() >= 3)
        .ok_or_else(|| {
            error(
                KernelErrorCode::Unsupported,
                KernelStage::Preflight,
                snapshot.id,
                "the requested face has no usable planar boundary",
                vec![simple_diagnostic(
                    "PLANAR_SUPPORT_BOUNDARY_UNAVAILABLE",
                    KernelStage::Preflight,
                    "The face outer loop cannot be represented as a planar sketch boundary.",
                )],
            )
        })?;
        let plane = record.value.surface.as_plane().ok_or_else(|| {
            error(
                KernelErrorCode::Unsupported,
                KernelStage::Preflight,
                snapshot.id,
                "the requested face is not planar",
                vec![simple_diagnostic(
                    "PLANAR_SUPPORT_SURFACE_UNSUPPORTED",
                    KernelStage::Preflight,
                    "A planar support query cannot use a cylindrical face.",
                )],
            )
        })?;
        let u_length = plane.u.length();
        let v_length = plane.v.length();
        if !u_length.is_finite()
            || !v_length.is_finite()
            || u_length <= f64::EPSILON
            || v_length <= f64::EPSILON
        {
            return Err(error(
                KernelErrorCode::NumericallyIndeterminate,
                KernelStage::Preflight,
                snapshot.id,
                "the face frame cannot be normalized safely",
                vec![simple_diagnostic(
                    "PLANAR_SUPPORT_FRAME_INDETERMINATE",
                    KernelStage::Preflight,
                    "The face surface axes are degenerate or non-finite.",
                )],
            ));
        }
        let u = plane.u / u_length;
        let v = plane.v / v_length;
        let count = polygon.len() as f64;
        let center = Point3::new(
            polygon.iter().map(|point| point.x).sum::<f64>() / count,
            polygon.iter().map(|point| point.y).sum::<f64>() / count,
            polygon.iter().map(|point| point.z).sum::<f64>() / count,
        );
        let boundary = polygon
            .iter()
            .map(|point| {
                let relative = *point - center;
                ProtocolPoint2::new(relative.dot(u), relative.dot(v))
            })
            .collect();
        let inner_boundaries = record
            .value
            .inner_loops
            .iter()
            .map(|loop_key| {
                sampled_loop_polygon(
                    &snapshot.topology,
                    *loop_key,
                    ChordBudget::Authoritative,
                    precision,
                )
                .map(|inner| {
                    inner
                        .iter()
                        .map(|point| {
                            let relative = *point - center;
                            ProtocolPoint2::new(relative.dot(u), relative.dot(v))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                error(
                    KernelErrorCode::Unsupported,
                    KernelStage::Preflight,
                    snapshot.id,
                    "the requested face has an unusable inner boundary",
                    vec![simple_diagnostic(
                        "PLANAR_SUPPORT_INNER_BOUNDARY_UNAVAILABLE",
                        KernelStage::Preflight,
                        "A face-owned inner loop could not be represented as a planar sketch boundary.",
                    )],
                )
            })?;
        let boundary_curves =
            face_frame_loop_curves(&snapshot.topology, record.value.outer_loop, center, u, v)
                .unwrap_or_default();
        let inner_boundary_curves = record
            .value
            .inner_loops
            .iter()
            .map(|loop_key| {
                face_frame_loop_curves(&snapshot.topology, *loop_key, center, u, v)
                    .unwrap_or_default()
            })
            .collect();
        Ok(PlanarFaceSupport {
            face,
            frame: PlanarFrame3::new(
                protocol_point(center),
                protocol_vector(u),
                protocol_vector(v),
            ),
            boundary,
            inner_boundaries,
            boundary_curves,
            inner_boundary_curves,
            linear_profile_extrusion_supported: linear_face_feature_owner_supported(
                &snapshot.topology,
                face,
            ),
            support_digest: snapshot.semantic_digest,
        })
    }

    /// Returns the complete outer rim loop of the cap face containing `edge`.
    ///
    /// Interactive selection expands through this so a whole rim enters an
    /// edge-set finish as one unit. A seed that is not on a cap outer loop
    /// falls back to its analytic carrier group, so callers can use this as
    /// the single loop-selection entry point.
    pub fn rim_loop_group(
        snapshot: &Snapshot,
        edge: EntityRef,
    ) -> Result<Vec<EntityRef>, KernelError> {
        resolve_measure_entity(snapshot, edge, EntityKind::Edge, "edge")?;
        rim_loop_blend::rim_loop_group(&snapshot.topology, edge)
            .map_or_else(|| Self::carrier_edge_group(snapshot, edge), Ok)
    }

    /// Returns every edge sharing the seed edge's analytic circular carrier.
    ///
    /// Full circles are represented as two exact semicircle edges with seam
    /// vertices; interactively they are one logical rim. The group is decided
    /// on the authoritative carriers (equal centre, radius, and plane normal
    /// within presentation agreement), never on sampled display chords, so a
    /// rim selects, highlights, and measures as a single closed edge. A line
    /// edge is its own group.
    pub fn carrier_edge_group(
        snapshot: &Snapshot,
        edge: EntityRef,
    ) -> Result<Vec<EntityRef>, KernelError> {
        let record = resolve_measure_entity(snapshot, edge, EntityKind::Edge, "edge")?;
        let Curve3::Circle {
            center: seed_center,
            u: seed_u,
            v: seed_v,
            radius: seed_radius,
        } = snapshot.topology.edges[record].value.curve
        else {
            return Ok(vec![edge]);
        };
        let seed_normal = seed_u.cross(seed_v);
        let scale = 1.0
            + seed_center
                .x
                .abs()
                .max(seed_center.y.abs())
                .max(seed_center.z.abs().max(seed_radius.abs()));
        let agreement = 1.0e-9 * scale;
        let group = snapshot
            .topology
            .edges
            .iter()
            .filter(|candidate| {
                let Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                } = candidate.value.curve
                else {
                    return false;
                };
                let normal = u.cross(v);
                let normals_parallel = normal.cross(seed_normal).length()
                    <= agreement * normal.length().max(seed_normal.length());
                (center - seed_center).length() <= agreement
                    && (radius - seed_radius).abs() <= agreement
                    && normals_parallel
            })
            .map(|candidate| entity_ref(snapshot.id, candidate.id.get(), EntityKind::Edge))
            .collect();
        Ok(group)
    }

    /// Returns the exact model-space length of one authoritative B-rep edge.
    pub fn edge_length(snapshot: &Snapshot, edge: EntityRef) -> Result<f64, KernelError> {
        let record = resolve_measure_entity(snapshot, edge, EntityKind::Edge, "edge")?;
        Ok(snapshot.topology.edges[record].value.length())
    }

    /// Returns the exact model-space area of one authoritative B-rep face.
    pub fn face_area(snapshot: &Snapshot, face: EntityRef) -> Result<f64, KernelError> {
        let record = resolve_measure_entity(snapshot, face, EntityKind::Face, "face")?;
        let face = &snapshot.topology.faces[record].value;
        let parameter_area = validator::face_parameter_area_and_moment(&snapshot.topology, face)
            .map(|(area, _)| area.abs())
            .ok_or_else(|| {
                error(
                    KernelErrorCode::InvalidInput,
                    KernelStage::Preflight,
                    snapshot.id,
                    "the requested face area could not be evaluated",
                    Vec::new(),
                )
            })?;
        let jacobian = match face.surface {
            Surface::Plane(plane) => plane.u.cross(plane.v).length(),
            Surface::Cylinder(cylinder) => cylinder.radius * cylinder.axis.length(),
            Surface::Torus(torus) => {
                return torus_face_area(&snapshot.topology, face, torus).ok_or_else(|| {
                    error(
                        KernelErrorCode::InvalidInput,
                        KernelStage::Preflight,
                        snapshot.id,
                        "the requested torus face area could not be evaluated",
                        Vec::new(),
                    )
                });
            }
            Surface::Sphere(sphere) => {
                return sphere_face_area(&snapshot.topology, face, sphere).ok_or_else(|| {
                    error(
                        KernelErrorCode::InvalidInput,
                        KernelStage::Preflight,
                        snapshot.id,
                        "the requested sphere face area could not be evaluated",
                        Vec::new(),
                    )
                });
            }
            Surface::Cone(cone) => {
                return cone_face_area(&snapshot.topology, face, cone).ok_or_else(|| {
                    error(
                        KernelErrorCode::InvalidInput,
                        KernelStage::Preflight,
                        snapshot.id,
                        "the requested cone face area could not be evaluated",
                        Vec::new(),
                    )
                });
            }
        };
        Ok(parameter_area * jacobian)
    }

    #[must_use]
    pub fn debug_scene(snapshot: &Snapshot) -> DebugScene {
        Self::debug_scene_with_pool(ComputePool::global(), snapshot)
    }

    /// Source-mapped display tessellation. Arc sampling spends the
    /// presentation chord budget; never feed this scene back into modeling.
    #[must_use]
    pub fn debug_scene_with_pool(compute: &ComputePool, snapshot: &Snapshot) -> DebugScene {
        Self::scene_with_budget(compute, snapshot, ChordBudget::Display)
    }

    /// Display tessellation additionally coarsened by `scale` for bodies
    /// that currently project small on screen. Sampling never drops below
    /// eight chords per full turn, and the result remains presentation-only.
    #[must_use]
    pub fn display_scene_scaled(snapshot: &Snapshot, scale: f64) -> DebugScene {
        Self::scene_with_budget(
            ComputePool::global(),
            snapshot,
            ChordBudget::DisplayScaled(scale),
        )
    }

    /// Faceted scene sampled at the kernel approximation budget. This is the
    /// scene consumed by regularized faceted reconstruction and by faceted
    /// interchange export, where sampling density is part of the contract.
    #[must_use]
    pub fn authoritative_scene(snapshot: &Snapshot) -> DebugScene {
        Self::scene_with_budget(ComputePool::global(), snapshot, ChordBudget::Authoritative)
    }

    fn scene_with_budget(
        compute: &ComputePool,
        snapshot: &Snapshot,
        budget: ChordBudget,
    ) -> DebugScene {
        perf_span!("kernel.tessellate", snapshot.topology.faces.len(), {
            Self::scene_with_budget_inner(compute, snapshot, budget)
        })
    }

    fn scene_with_budget_inner(
        compute: &ComputePool,
        snapshot: &Snapshot,
        budget: ChordBudget,
    ) -> DebugScene {
        let precision = snapshot.precision.unwrap_or_default();
        let fallback = if matches!(budget, ChordBudget::Authoritative) {
            TessellationFallback::Refuse
        } else {
            TessellationFallback::Display
        };
        let triangles = compute.flat_map(
            "kernel.tessellation.faces",
            &snapshot.topology.faces,
            |index, face| {
                let mut triangles = Vec::new();
                let source_face = entity_ref(snapshot.id, face.id.get(), EntityKind::Face);
                match face.value.surface {
                    Surface::Plane(plane) => {
                        let Some(boundaries) = face
                            .value
                            .loops()
                            .map(|loop_key| {
                                sampled_loop_polygon(
                                    &snapshot.topology,
                                    loop_key,
                                    budget,
                                    precision,
                                )
                            })
                            .collect::<Option<Vec<_>>>()
                        else {
                            return triangles;
                        };
                        if boundaries.first().is_none_or(|polygon| polygon.len() < 3) {
                            return triangles;
                        }
                        for vertices in triangulate_face_boundaries(&boundaries, plane, fallback) {
                            triangles.push(shaded_triangle(
                                face.value.surface,
                                vertices,
                                source_face,
                                snapshot.topology.faces[index].value.role,
                            ));
                        }
                    }
                    Surface::Cylinder(cylinder) => {
                        for vertices in tessellate_cylinder_face(
                            &snapshot.topology,
                            &face.value,
                            cylinder,
                            budget,
                            precision,
                        ) {
                            triangles.push(shaded_triangle(
                                face.value.surface,
                                vertices,
                                source_face,
                                snapshot.topology.faces[index].value.role,
                            ));
                        }
                    }
                    Surface::Torus(torus) => {
                        for vertices in tessellate_torus_face(
                            &snapshot.topology,
                            &face.value,
                            torus,
                            budget,
                            precision,
                        ) {
                            triangles.push(shaded_triangle(
                                face.value.surface,
                                vertices,
                                source_face,
                                snapshot.topology.faces[index].value.role,
                            ));
                        }
                    }
                    Surface::Sphere(sphere) => {
                        for vertices in tessellate_sphere_face(
                            &snapshot.topology,
                            &face.value,
                            sphere,
                            budget,
                            precision,
                        ) {
                            triangles.push(shaded_triangle(
                                face.value.surface,
                                vertices,
                                source_face,
                                snapshot.topology.faces[index].value.role,
                            ));
                        }
                    }
                    Surface::Cone(cone) => {
                        for vertices in tessellate_cone_face(
                            &snapshot.topology,
                            &face.value,
                            cone,
                            budget,
                            precision,
                        ) {
                            triangles.push(shaded_triangle(
                                face.value.surface,
                                vertices,
                                source_face,
                                snapshot.topology.faces[index].value.role,
                            ));
                        }
                    }
                }
                triangles
            },
        );

        let presentation_flags = presentation_edge_flags(&snapshot.topology);
        let presentation_smooth_edges = &presentation_flags.smooth;
        let edge_incident_faces = edge_incident_faces(snapshot);
        let edges = compute.flat_map(
            "kernel.tessellation.edges",
            &snapshot.topology.edges,
            |index, edge| {
                let is_smooth = presentation_smooth_edges[index];
                let is_tangent = presentation_flags.tangent[index];
                let incident_faces = edge_incident_faces[index];
                sampled_edge_segments(edge.value, budget, precision)
                    .into_iter()
                    .map(|endpoints| DebugEdge {
                        endpoints: endpoints.map(protocol_point),
                        source_edge: entity_ref(snapshot.id, edge.id.get(), EntityKind::Edge),
                        is_smooth,
                        is_tangent,
                        incident_faces,
                    })
                    .collect()
            },
        );
        // A vertex where only tangent rails and one crease meet is not a
        // corner the eye can see; classify vertices against the crease graph.
        let crease_hidden = presentation_smooth_edges
            .iter()
            .zip(&presentation_flags.tangent)
            .map(|(smooth, tangent)| *smooth || *tangent)
            .collect::<Vec<_>>();

        let vertices = snapshot
            .topology
            .vertices
            .iter()
            .enumerate()
            .map(|(vertex_index, vertex)| DebugVertex {
                point: protocol_point(vertex.value.point),
                source_vertex: entity_ref(snapshot.id, vertex.id.get(), EntityKind::Vertex),
                is_smooth: presentation_vertex_is_smooth(
                    &snapshot.topology,
                    vertex_index,
                    &crease_hidden,
                ),
            })
            .collect();

        DebugScene {
            snapshot: snapshot.id,
            semantic_digest: snapshot.semantic_digest,
            triangles,
            edges,
            vertices,
            carriers: display_carriers(snapshot),
        }
    }
}

/// The faces on either side of every edge, in topology order.
///
/// One pass over the coedges rather than a face scan per edge: the display
/// scene is rebuilt on every commit and dense Boolean results have thousands
/// of edges.
fn edge_incident_faces(snapshot: &Snapshot) -> Vec<[Option<EntityRef>; 2]> {
    let mut incident = vec![[None; 2]; snapshot.topology.edges.len()];
    for face in &snapshot.topology.faces {
        let face_ref = entity_ref(snapshot.id, face.id.get(), EntityKind::Face);
        for loop_key in face.value.loops() {
            let Some(loop_record) = snapshot.topology.loop_record(loop_key) else {
                continue;
            };
            for coedge_key in &loop_record.value.coedges {
                let Some(coedge) = snapshot.topology.coedge(*coedge_key) else {
                    continue;
                };
                let Some(slot) = incident.get_mut(coedge.value.edge.0) else {
                    continue;
                };
                if slot[0].is_none() {
                    slot[0] = Some(face_ref);
                } else if slot[1].is_none() && slot[0] != Some(face_ref) {
                    slot[1] = Some(face_ref);
                }
            }
        }
    }
    incident
}

/// Display-only carrier descriptors for the curved faces of a snapshot.
///
/// Planes are omitted: a planar face's outline is its own boundary edges, so
/// it has no silhouette the B-rep does not already carry.
fn display_carriers(snapshot: &Snapshot) -> Vec<DisplayCarrier> {
    snapshot
        .topology
        .faces
        .iter()
        .filter_map(|face| {
            let (u_min, u_max, v_min, v_max) =
                face_parameter_bounds(&snapshot.topology, &face.value)?;
            let surface = match face.value.surface {
                Surface::Plane(_) => return None,
                Surface::Cylinder(cylinder) => DisplaySurface::Cylinder {
                    origin: protocol_point(cylinder.origin),
                    axis: protocol_vector(cylinder.axis),
                    radial_u: protocol_vector(cylinder.radial_u),
                    radial_v: protocol_vector(cylinder.radial_v),
                    radius: cylinder.radius,
                    angular_sign: cylinder.angular_sign,
                },
                Surface::Cone(cone) => DisplaySurface::Cone {
                    origin: protocol_point(cone.origin),
                    axis: protocol_vector(cone.axis),
                    radial_u: protocol_vector(cone.radial_u),
                    radial_v: protocol_vector(cone.radial_v),
                    base_radius: cone.base_radius,
                    slope: cone.slope,
                    angular_sign: cone.angular_sign,
                },
                Surface::Sphere(sphere) => DisplaySurface::Sphere {
                    origin: protocol_point(sphere.origin),
                    axis: protocol_vector(sphere.axis),
                    radial_u: protocol_vector(sphere.radial_u),
                    radial_v: protocol_vector(sphere.radial_v),
                    radius: sphere.radius,
                    angular_sign: sphere.angular_sign,
                },
                Surface::Torus(torus) => DisplaySurface::Torus {
                    origin: protocol_point(torus.origin),
                    axis: protocol_vector(torus.axis),
                    radial_u: protocol_vector(torus.radial_u),
                    radial_v: protocol_vector(torus.radial_v),
                    major_radius: torus.major_radius,
                    minor_radius: torus.minor_radius,
                    angular_sign: torus.angular_sign,
                },
            };
            Some(DisplayCarrier {
                source_face: entity_ref(snapshot.id, face.id.get(), EntityKind::Face),
                surface,
                domain: [[u_min, u_max], [v_min, v_max]],
            })
        })
        .collect()
}

fn presentation_vertex_is_smooth(
    topology: &Topology,
    vertex_index: usize,
    smooth_edges: &[bool],
) -> bool {
    let visible = topology
        .edges
        .iter()
        .enumerate()
        .filter(|(edge_index, edge)| {
            !smooth_edges[*edge_index]
                && edge
                    .value
                    .vertices
                    .contains(&topology::VertexKey(vertex_index))
        })
        .map(|(edge_index, _)| edge_index)
        .collect::<Vec<_>>();
    match visible.as_slice() {
        [] => true,
        [first, second] => {
            let Some(first) = edge_direction_from_vertex(topology, *first, vertex_index) else {
                return false;
            };
            let Some(second) = edge_direction_from_vertex(topology, *second, vertex_index) else {
                return false;
            };
            -first.dot(second) >= 35.0_f64.to_radians().cos()
        }
        _ => false,
    }
}

#[cfg(test)]
fn presentation_smooth_edge_flags(topology: &Topology) -> Vec<bool> {
    presentation_edge_flags(topology).smooth
}

fn presentation_edge_flags(topology: &Topology) -> PresentationEdgeFlags {
    let prismatic_feature_roles = presentation_prismatic_feature_roles(topology);
    let incident_faces = edge_incident_face_indices(topology);
    let classifications = (0..topology.edges.len())
        .map(|edge_index| presentation_edge_classification(topology, &incident_faces, edge_index))
        .collect::<Vec<_>>();
    let logical_carrier_subdivision = classifications
        .iter()
        .map(|classification| {
            classification
                .same_feature_side_role
                .is_some_and(|role| prismatic_feature_roles.contains(&role))
        })
        .collect::<Vec<_>>();
    let mut smooth = classifications
        .iter()
        .zip(&logical_carrier_subdivision)
        .map(|(classification, logical)| classification.smooth || *logical)
        .collect::<Vec<_>>();
    let mut incident = vec![Vec::<usize>::new(); topology.vertices.len()];
    for (edge_index, edge) in topology.edges.iter().enumerate() {
        for vertex in edge.value.vertices {
            incident[vertex.0].push(edge_index);
        }
    }

    // A regularized corner can split a real transition rail into several
    // source/strip and strip/strip fragments. Starting at a dangling visible
    // rail, promote only its best continuation through the otherwise hidden
    // same-surface graph. This closes presentation gaps without restoring the
    // full transverse facet grid; exact role ownership has already decided
    // which candidates are safe to hide.
    for _ in 0..topology.edges.len() {
        let mut promoted = None::<usize>;
        'vertices: for (vertex_index, edges) in incident.iter().enumerate() {
            let visible = edges
                .iter()
                .copied()
                .filter(|edge_index| !smooth[*edge_index])
                .collect::<Vec<_>>();
            if visible.len() != 1 {
                continue;
            }
            let Some(incoming) = edge_direction_from_vertex(topology, visible[0], vertex_index)
            else {
                continue;
            };
            let mut best = None::<(f64, usize)>;
            for candidate in edges.iter().copied().filter(|edge_index| {
                smooth[*edge_index]
                    && !classifications[*edge_index].coplanar_subdivision
                    && classifications[*edge_index]
                        .same_feature_side_role
                        .is_none()
                    && !logical_carrier_subdivision[*edge_index]
            }) {
                let Some(outgoing) = edge_direction_from_vertex(topology, candidate, vertex_index)
                else {
                    continue;
                };
                let continuation = -incoming.dot(outgoing);
                if continuation < 0.5 {
                    continue;
                }
                if best.is_none_or(|(score, edge_index)| {
                    continuation > score + 1.0e-12
                        || ((continuation - score).abs() <= 1.0e-12 && candidate < edge_index)
                }) {
                    best = Some((continuation, candidate));
                }
            }
            // Never turn a coplanar triangulation member back into a model
            // edge merely because a real rail ends at the same regularized
            // vertex. Curved approximation strips, however, may need a broad
            // continuation through a trihedral blend and remain eligible.
            if let Some((continuation, candidate)) = best {
                debug_assert!(continuation.is_finite());
                promoted = Some(candidate);
                break 'vertices;
            }
        }
        let Some(edge_index) = promoted else {
            break;
        };
        smooth[edge_index] = false;
    }
    let tangent = classifications
        .iter()
        .zip(&smooth)
        .map(|(classification, smooth)| classification.tangent && !smooth)
        .collect();
    PresentationEdgeFlags { smooth, tangent }
}

/// Identifies faceted patches that are fragments of one extruded curve
/// carrier. Boolean splitting may turn a cylinder into hundreds of planar
/// B-rep patches, but their normals still share one extrusion axis. Keeping
/// that ownership here makes the collection one selectable/display surface
/// without erasing genuine intersections between different cylinders.
fn presentation_prismatic_feature_roles(topology: &Topology) -> BTreeSet<u32> {
    // Every planar face of a role, with its area, so that the microscopic
    // fragments a regularized Boolean welds at a seam cannot veto the
    // carrier: their normals are whatever a few-micron triangle happens to
    // have, and they carry no material worth a crease.
    let mut faces = BTreeMap::<u32, Vec<(Vector3, f64)>>::new();
    for face in &topology.faces {
        let (FaceRole::FeatureSide(role), Surface::Plane(plane)) =
            (face.value.role, face.value.surface)
        else {
            continue;
        };
        let length = plane.normal.length();
        if !length.is_finite() || length <= f64::EPSILON {
            continue;
        }
        let normal = plane.normal / length;
        let area = planar_face_area(topology, &face.value);
        faces.entry(role).or_default().push((normal, area));
    }
    let mut normals = BTreeMap::<u32, Vec<Vector3>>::new();
    for (role, faces) in faces {
        let largest = faces.iter().map(|(_, area)| *area).fold(0.0_f64, f64::max);
        let role_normals = normals.entry(role).or_default();
        for (normal, area) in faces {
            if area < largest * 1.0e-4 {
                continue;
            }
            if !role_normals
                .iter()
                .any(|candidate| candidate.dot(normal).abs() >= 1.0 - 1.0e-8)
            {
                role_normals.push(normal);
            }
        }
    }

    normals
        .into_iter()
        .filter_map(|(role, normals)| {
            // Two or more distinct directions uniquely identify an extruded
            // curved carrier sharing one axis, while excluding flat single-plane
            // chamfers and side walls.
            if normals.len() < 2 {
                return None;
            }
            let first = normals[0];
            let axis = normals.iter().skip(1).find_map(|normal| {
                let cross = first.cross(*normal);
                let length = cross.length();
                (length.is_finite() && length > 1.0e-6).then_some(cross / length)
            })?;
            // The panels of a faceted bore are coaxial by construction, but
            // the regularized assembly welds their corners at up to a
            // thousandth of a millimetre, so a panel a millimetre wide can
            // tilt by a milliradian and still be the same logical carrier.
            normals
                .iter()
                .all(|normal| normal.dot(axis).abs() <= 1.0e-3)
                .then_some(role)
        })
        .collect()
}

/// The area of a planar face's outer loop, from its vertices in loop order.
/// Inner loops are ignored: this only ranks fragments against each other.
fn planar_face_area(topology: &Topology, face: &topology::Face) -> f64 {
    let Some(loop_record) = topology.loops.get(face.outer_loop.0) else {
        return 0.0;
    };
    let points = loop_record
        .value
        .coedges
        .iter()
        .filter_map(|coedge_key| {
            let coedge = topology.coedges.get(coedge_key.0)?.value;
            let edge = topology.edges.get(coedge.edge.0)?.value;
            let vertex = match coedge.orientation {
                topology::Orientation::Forward => edge.vertices[0],
                topology::Orientation::Reverse => edge.vertices[1],
            };
            topology
                .vertices
                .get(vertex.0)
                .map(|record| record.value.point)
        })
        .collect::<Vec<_>>();
    if points.len() < 3 {
        return 0.0;
    }
    let mut twice_area = Vector3::default();
    for (index, start) in points.iter().enumerate() {
        let end = points[(index + 1) % points.len()];
        twice_area = twice_area
            + Vector3::new(
                (start.y - end.y) * (start.z + end.z),
                (start.z - end.z) * (start.x + end.x),
                (start.x - end.x) * (start.y + end.y),
            );
    }
    0.5 * twice_area.length()
}

fn edge_direction_from_vertex(
    topology: &Topology,
    edge_index: usize,
    vertex_index: usize,
) -> Option<Vector3> {
    let edge = topology.edges.get(edge_index)?.value;
    let other = if edge.vertices[0].0 == vertex_index {
        edge.vertices[1]
    } else if edge.vertices[1].0 == vertex_index {
        edge.vertices[0]
    } else {
        return None;
    };
    let origin = topology.vertices.get(vertex_index)?.value.point;
    let direction = topology.vertices.get(other.0)?.value.point - origin;
    let length = direction.length();
    (length > f64::EPSILON).then(|| direction / length)
}

#[derive(Clone, Copy)]
struct PresentationEdgeClassification {
    smooth: bool,
    /// The two carriers differ but share their normal along this edge: the
    /// transition rail of an exact fillet. The edge is real topology and
    /// stays selectable (a successor finish can target it), but it is not a
    /// crease and must not draw as one.
    tangent: bool,
    coplanar_subdivision: bool,
    same_feature_side_role: Option<u32>,
}

/// Per-edge presentation flags for one topology.
struct PresentationEdgeFlags {
    smooth: Vec<bool>,
    tangent: Vec<bool>,
}

#[cfg(test)]
fn presentation_edge_is_smooth(topology: &Topology, edge_index: usize) -> bool {
    presentation_edge_classification(topology, &edge_incident_face_indices(topology), edge_index)
        .smooth
}

/// The faces incident to every edge, as indices, built in one pass.
///
/// The classification below used to answer this per edge by scanning every
/// face, every loop, and every coedge — quadratic in a body's size, and paid on
/// every display-scene build. Two crossing round cuts take a box from 6 faces
/// to nearly three thousand, where the difference is seconds rather than
/// milliseconds.
fn edge_incident_face_indices(topology: &Topology) -> Vec<Vec<usize>> {
    let mut incident = vec![Vec::new(); topology.edges.len()];
    for (face_index, face) in topology.faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            let Some(loop_record) = topology.loops.get(loop_key.0) else {
                continue;
            };
            for coedge_key in &loop_record.value.coedges {
                let Some(coedge) = topology.coedges.get(coedge_key.0) else {
                    continue;
                };
                let Some(slot) = incident.get_mut(coedge.value.edge.0) else {
                    continue;
                };
                // One face may use an edge through several coedges (a seam
                // closing on itself); the classification counts faces, not uses.
                if slot.last() != Some(&face_index) && !slot.contains(&face_index) {
                    slot.push(face_index);
                }
            }
        }
    }
    incident
}

fn presentation_edge_classification(
    topology: &Topology,
    incident_faces: &[Vec<usize>],
    edge_index: usize,
) -> PresentationEdgeClassification {
    let hard = || PresentationEdgeClassification {
        smooth: false,
        tangent: false,
        coplanar_subdivision: false,
        same_feature_side_role: None,
    };
    let Some([first, second]) = incident_faces.get(edge_index).map(Vec::as_slice) else {
        return hard();
    };
    let edge = topology.edges[edge_index].value;
    let endpoints = edge.endpoints();
    // Normals are compared at a point on the curve itself. The chord midpoint
    // of an arc sits inside the arc, where a torus or sphere carrier's normal
    // is a different direction, and that difference is exactly the size of
    // the tangency test below.
    let midpoint = edge
        .curve
        .evaluate((edge.parameter_range.start + edge.parameter_range.end) * 0.5);
    let first_surface = topology.faces[*first].value.surface;
    let second_surface = topology.faces[*second].value.surface;
    let normal = |surface: Surface| -> Option<Vector3> {
        let normal = match surface {
            Surface::Plane(plane) => plane.normal,
            Surface::Sphere(sphere) => {
                let relative = midpoint - sphere.origin;
                let length = relative.length();
                if length <= f64::EPSILON {
                    return None;
                }
                relative / length
            }
            Surface::Cone(cone) => {
                let relative = midpoint - cone.origin;
                let axial = cone.axis * relative.dot(cone.axis);
                let planar = relative - axial;
                let planar_length = planar.length();
                if planar_length <= f64::EPSILON {
                    return None;
                }
                let radial = planar / planar_length;
                let normal = radial - cone.axis * cone.slope;
                let length = normal.length();
                if length <= f64::EPSILON {
                    return None;
                }
                normal / length
            }
            Surface::Torus(torus) => {
                // Outward normal at the point nearest `midpoint`: radial from
                // the ring-centre circle toward the point.
                let relative = midpoint - torus.origin;
                let axial = torus.axis * relative.dot(torus.axis);
                let planar = relative - axial;
                let planar_length = planar.length();
                if planar_length <= f64::EPSILON {
                    return None;
                }
                let ring = planar * (torus.major_radius / planar_length);
                let from_ring = relative - ring;
                let length = from_ring.length();
                if length <= f64::EPSILON {
                    return None;
                }
                from_ring / length
            }
            Surface::Cylinder(cylinder) => {
                let relative = midpoint - cylinder.origin;
                let axis_denominator = cylinder.axis.dot(cylinder.axis);
                if axis_denominator <= f64::EPSILON {
                    return None;
                }
                relative - cylinder.axis * (relative.dot(cylinder.axis) / axis_denominator)
            }
        };
        let length = normal.length();
        (length > f64::EPSILON).then(|| normal / length)
    };
    let Some(first_normal) = normal(first_surface) else {
        return hard();
    };
    let Some(second_normal) = normal(second_surface) else {
        return hard();
    };
    // Boolean subdivision is free to preserve different semantic roles on
    // adjacent fragments of one physical plane.  Those roles are useful for
    // history attribution, but they must not manufacture a selectable seam
    // (or expose every ear-clipping diagonal) in the model presentation.
    let coincident_planes = match (first_surface, second_surface) {
        (Surface::Plane(first), Surface::Plane(second)) => {
            let first_length = first.normal.length();
            let second_length = second.normal.length();
            if first_length <= f64::EPSILON || second_length <= f64::EPSILON {
                false
            } else {
                let first_unit = first.normal / first_length;
                let second_unit = second.normal / second_length;
                let scale = (endpoints[1] - endpoints[0])
                    .length()
                    .max((midpoint - Point3::default()).length())
                    .max((first.origin - Point3::default()).length())
                    .max((second.origin - Point3::default()).length())
                    .max(1.0);
                first_unit.dot(second_unit).abs() >= 1.0 - 1.0e-9
                    && (second.origin - first.origin).dot(first_unit).abs() <= scale * 1.0e-8
            }
        }
        _ => false,
    };
    // Regularized multi-edge corner patches can fan more coarsely than the
    // primary cylindrical strip even while remaining one continuous rounded
    // presentation surface. An unchanged feature-side role is authoritative
    // for that subdivision; different strips are joined only across a small
    // dihedral. Harder, differently-owned patch intersections remain real
    // visible/selectable rails and can host a successor finish.
    let normal_dot = first_normal.dot(second_normal).abs();
    let low_dihedral = normal_dot >= 15.0_f64.to_radians().cos();
    let approximation_strip = |face_index: usize| {
        matches!(topology.faces[face_index].value.surface, Surface::Plane(_))
            && matches!(
                topology.faces[face_index].value.role,
                FaceRole::FeatureSide(_)
            )
    };
    let same_strip = topology.faces[*first].value.role == topology.faces[*second].value.role;
    let same_feature_side_role = match (
        topology.faces[*first].value.role,
        topology.faces[*second].value.role,
    ) {
        (FaceRole::FeatureSide(first), FaceRole::FeatureSide(second)) if first == second => {
            Some(first)
        }
        _ => None,
    };
    // A shared approximation-strip role may fan quite coarsely at a rounded
    // transition, but it is not permission to erase a real rail.
    //
    // The allowance was 60 degrees, which is exactly the dihedral between two
    // 45-degree chamfer slants meeting at a cube corner — so the mitre between
    // them landed on the threshold and the comparison decided by rounding.
    // Where three such chamfers met, some of the three mitres tested just
    // under and drew while the rest tested just over and vanished. A fan
    // approximating a curve is dense, not coarse: the blend cutter panels a
    // quarter turn twelve ways, so its steps are 7.5 degrees and even a
    // four-panel fan turns by 22.5. Thirty degrees clears every such fan by a
    // wide margin while leaving a chamfer mitre the visible, selectable rail
    // it is.
    let same_rounded_strip = same_strip && normal_dot >= 30.0_f64.to_radians().cos();
    let coincident_cylinder_strip = match (first_surface, second_surface) {
        (Surface::Cylinder(first), Surface::Cylinder(second)) => {
            let first_axis_length = first.axis.length();
            let second_axis_length = second.axis.length();
            if first_axis_length <= f64::EPSILON || second_axis_length <= f64::EPSILON {
                false
            } else {
                let first_axis = first.axis / first_axis_length;
                let second_axis = second.axis / second_axis_length;
                let scale = first
                    .radius
                    .abs()
                    .max(second.radius.abs())
                    .max((first.origin - Point3::default()).length())
                    .max((second.origin - Point3::default()).length())
                    .max(1.0);
                let tolerance = scale * 1.0e-5;
                first_axis.dot(second_axis).abs() >= 1.0 - 1.0e-6
                    && (first.radius - second.radius).abs() <= tolerance
                    && (second.origin - first.origin).cross(first_axis).length() <= tolerance
            }
        }
        _ => false,
    };
    // Rim blends split every torus, cone, and sphere carrier into half-faces
    // (ADR 0016). Those parameterization seams are no more real than a
    // cylinder's and must not draw as model edges across a smooth band. The
    // comparisons stay strict — a false negative merely draws a line, while a
    // false positive would hide a genuine rail.
    let unit = |axis: Vector3| {
        let length = axis.length();
        (length > f64::EPSILON).then(|| axis / length)
    };
    let carrier_scale = |origin: Point3, radius: f64| {
        radius
            .abs()
            .max((origin - Point3::default()).length())
            .max(1.0)
    };
    let coincident_revolved_strip = match (first_surface, second_surface) {
        (Surface::Torus(first), Surface::Torus(second)) => {
            match (unit(first.axis), unit(second.axis)) {
                (Some(first_axis), Some(second_axis)) => {
                    let tolerance =
                        carrier_scale(first.origin, first.major_radius + first.minor_radius)
                            * 1.0e-5;
                    first_axis.dot(second_axis).abs() >= 1.0 - 1.0e-6
                        && (second.origin - first.origin).length() <= tolerance
                        && (first.major_radius - second.major_radius).abs() <= tolerance
                        && (first.minor_radius - second.minor_radius).abs() <= tolerance
                }
                _ => false,
            }
        }
        (Surface::Cone(first), Surface::Cone(second)) => {
            match (unit(first.axis), unit(second.axis)) {
                (Some(first_axis), Some(second_axis)) => {
                    let alignment = first_axis.dot(second_axis);
                    let tolerance = carrier_scale(first.origin, first.base_radius) * 1.0e-5;
                    // An anti-parallel axis flips the axial parameter, so the
                    // same point set carries the negated slope.
                    alignment.abs() >= 1.0 - 1.0e-6
                        && (second.origin - first.origin).length() <= tolerance
                        && (first.base_radius - second.base_radius).abs() <= tolerance
                        && (first.slope - alignment.signum() * second.slope).abs() <= tolerance
                }
                _ => false,
            }
        }
        (Surface::Sphere(first), Surface::Sphere(second)) => {
            let tolerance = carrier_scale(first.origin, first.radius) * 1.0e-5;
            (second.origin - first.origin).length() <= tolerance
                && (first.radius - second.radius).abs() <= tolerance
        }
        _ => false,
    };
    let smooth = coincident_planes
        || (coincident_cylinder_strip || coincident_revolved_strip) && low_dihedral
        || approximation_strip(*first)
            && approximation_strip(*second)
            && (same_rounded_strip || low_dihedral);
    // Two different exact carriers whose normals agree along the edge meet
    // tangentially: a fillet's plane/cylinder or cylinder/torus rail. The
    // tolerance is tight because both normals come from closed forms; a
    // faceted fan never gets this close, and a chamfer never does.
    let same_carrier = coincident_planes || coincident_cylinder_strip || coincident_revolved_strip;
    let tangent = !smooth && !same_carrier && normal_dot >= 1.0 - 1.0e-9;
    PresentationEdgeClassification {
        smooth,
        tangent,
        coplanar_subdivision: coincident_planes,
        same_feature_side_role,
    }
}

fn faceted_cut_warning() -> ProtocolDiagnostic {
    approximation_warning(
        "FACE_FEATURE_FACETED_APPROXIMATION",
        "This cut crosses geometry that the exact rewrite cannot split - curved walls, or an \
         interior void with material resuming beyond it - so the body was rebuilt from a \
         tessellation. Its faces, edges, and measures approximate the true solid rather than \
         certifying it: two round bores that cross meet in ellipses, which are outside this \
         kernel's line-and-circle curve vocabulary.",
    )
}

/// A faceted candidate that fails the closed-solid validator is not this
/// cut's answer. Naming the tier that failed, and why, beats the bare
/// validation failure the generic gate would otherwise report.
fn certify_faceted_candidate(
    snapshot: SnapshotId,
    topology: &Topology,
    precision: PrecisionPolicy,
) -> Result<(), KernelError> {
    let validation = validator::validate(topology, precision.linear_agreement);
    if validation.diagnostics.is_empty() {
        return Ok(());
    }
    let mut diagnostics = vec![simple_diagnostic(
        "FACE_FEATURE_FACETED_UNRESOLVED",
        KernelStage::Construction,
        "The crossing cut was rebuilt from a tessellation, but the rebuilt shell did not close: \
         fragments where the cutter meets existing geometry could not be welded within the \
         approximation budget.",
    )];
    diagnostics.extend(
        validation
            .diagnostics
            .iter()
            .map(|diagnostic| validator_diagnostic(snapshot, diagnostic)),
    );
    Err(error(
        KernelErrorCode::Unsupported,
        KernelStage::Construction,
        snapshot,
        "the faceted cut could not be regularized into a closed solid",
        diagnostics,
    ))
}

/// The edge-finish ladder beyond the six-plane cuboid: each exact rung runs
/// once, and its refusal either names the fault or hands the request to the
/// next rung. The faceted tier is the last rung and says so in the warnings,
/// because every other result this kernel publishes is exact.
fn regularized_edge_finish(
    input: &Snapshot,
    targets: &[EntityRef],
    kind: artificer_protocol::EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
    warnings: &mut Vec<ProtocolDiagnostic>,
) -> Result<Topology, KernelError> {
    match prism_edge_finish::build_prism_edge_finishes(
        input.id,
        &input.topology,
        targets,
        kind,
        distance,
        precision,
    ) {
        Ok(topology) => return Ok(topology),
        Err(prism_edge_finish::PrismEdgeFinishError::DistanceInvalid) => {
            return Err(simple_invalid_input(
                input.id,
                "PRISM_EDGE_FINISH_DISTANCE_INVALID",
                "The finish distance must fit inside both profile neighbours of every selected vertical edge.",
            ));
        }
        Err(
            prism_edge_finish::PrismEdgeFinishError::TargetInvalid
            | prism_edge_finish::PrismEdgeFinishError::DomainUnsupported
            | prism_edge_finish::PrismEdgeFinishError::ConstructionFailed,
        ) => {}
    }
    match section_revolve::build_rim_blend(
        input.id,
        &input.topology,
        targets,
        kind,
        distance,
        precision,
    ) {
        Ok(topology) => return Ok(topology),
        Err(section_revolve::RimBlendError::DistanceInvalid) => {
            return Err(simple_invalid_input(
                input.id,
                "RIM_BLEND_DISTANCE_INVALID",
                "The rim fillet radius must stay inside both the wall radius and the wall height.",
            ));
        }
        Err(_) => {}
    }
    match rim_loop_blend::build_rim_loop_blend(
        input.id,
        &input.topology,
        targets,
        kind,
        distance,
        precision,
    ) {
        Ok(topology) => return Ok(topology),
        Err(rim_loop_blend::RimLoopBlendError::DistanceInvalid) => {
            return Err(simple_invalid_input(
                input.id,
                "RIM_LOOP_DISTANCE_INVALID",
                "The rim-loop finish distance must leave a usable cap and wall.",
            ));
        }
        // A sharp reflex corner between two straight runs mitres exactly
        // through an elliptical seam; one that involves an arc would need a
        // quartic, so the faceted tier below approximates it and is labelled
        // as such.
        Err(
            rim_loop_blend::RimLoopBlendError::ReflexCorner
            | rim_loop_blend::RimLoopBlendError::TargetInvalid
            | rim_loop_blend::RimLoopBlendError::DomainUnsupported,
        ) => {}
    }

    let scene = NativeKernel::authoritative_scene(input);
    let faceted = faceted_boolean::finish_edges(
        Some(&input.topology),
        &scene,
        targets,
        kind,
        distance,
        precision,
    );
    let regularized = if input.topology.faces.len() == 6 {
        faceted
    } else {
        faceted
            .filter(|topology| {
                validator::validate(topology, precision.linear_agreement)
                    .diagnostics
                    .is_empty()
            })
            .or_else(|| finish_logical_successor_edges(input, targets, kind, distance, precision))
    };
    let topology = regularized.ok_or_else(|| {
        simple_invalid_input(
            input.id,
            "EDGE_FINISH_BLEND_UNSUPPORTED",
            "The selected edge neighbourhoods could not form a certified regularized corner blend.",
        )
    })?;
    warnings.push(approximation_warning(
        "EDGE_FINISH_FACETED_APPROXIMATION",
        "This finish runs where no exact blend exists in this kernel's line-and-circle vocabulary - \
         a hole with sharp concave corners, or edges the exact rungs cannot own - so the body was \
         rebuilt from a tessellation. Its blend faces, edges, and measures approximate the true \
         solid rather than certifying it.",
    ));
    Ok(topology)
}

fn edge_finish_error(
    snapshot: SnapshotId,
    reason: edge_finish::EdgeFinishError,
    many: bool,
) -> KernelError {
    let (code, message) = match (reason, many) {
        (edge_finish::EdgeFinishError::TargetInvalid, false) => (
            "EDGE_FINISH_TARGET_INVALID",
            "The selected edge is not owned by this snapshot.",
        ),
        (edge_finish::EdgeFinishError::TargetInvalid, true) => (
            "EDGE_FINISH_TARGET_INVALID",
            "Every selected edge must be unique and owned by this snapshot.",
        ),
        (edge_finish::EdgeFinishError::DistanceInvalid, false) => (
            "EDGE_FINISH_DISTANCE_INVALID",
            "The edge-finish distance must fit inside both adjacent faces.",
        ),
        (edge_finish::EdgeFinishError::DistanceInvalid, true) => (
            "EDGE_FINISH_DISTANCE_INVALID",
            "The edge-finish distance must fit inside every adjacent face.",
        ),
        (edge_finish::EdgeFinishError::ConstructionFailed, false) => (
            "EDGE_FINISH_CONSTRUCTION_FAILED",
            "The exact chamfer or fillet profile could not be certified.",
        ),
        (edge_finish::EdgeFinishError::ConstructionFailed, true) => (
            "EDGE_FINISH_CONSTRUCTION_FAILED",
            "The exact multi-edge chamfer or fillet profile could not be certified.",
        ),
        (edge_finish::EdgeFinishError::DomainUnsupported, _) => {
            unreachable!("a domain refusal enters the regularized ladder instead")
        }
    };
    simple_invalid_input(snapshot, code, message)
}

fn finish_logical_successor_edges(
    input: &Snapshot,
    targets: &[EntityRef],
    kind: artificer_protocol::EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Option<Topology> {
    let source_scene = NativeKernel::authoritative_scene(input);
    let mut source_segments = Vec::with_capacity(targets.len());
    for target in targets {
        let matching = source_scene
            .edges
            .iter()
            .filter(|edge| edge.source_edge == *target && !edge.is_smooth)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return None;
        }
        source_segments.push(matching[0].endpoints);
    }

    let mut current = input.clone();
    for source in source_segments {
        let scene = NativeKernel::authoritative_scene(&current);
        let target = resolve_successor_edge(&scene, source, precision)?;
        let topology = faceted_boolean::finish_edges(
            Some(&current.topology),
            &scene,
            &[target],
            kind,
            distance,
            precision,
        )?;
        let validation = validator::validate(&topology, precision.linear_agreement);
        if !validation.diagnostics.is_empty() {
            return None;
        }
        let semantic_digest = semantic_digest(&topology, precision);
        current = Snapshot {
            id: snapshot_id(semantic_digest),
            semantic_digest,
            precision: Some(precision),
            measures: public_measures(validation.measures),
            topology,
        };
    }
    Some(current.topology)
}

fn resolve_successor_edge(
    scene: &DebugScene,
    source: [ProtocolPoint3; 2],
    precision: PrecisionPolicy,
) -> Option<EntityRef> {
    let source_start = internal_protocol_point(source[0]);
    let source_end = internal_protocol_point(source[1]);
    let source_vector = source_end - source_start;
    let source_length = source_vector.length();
    if source_length <= precision.min_feature_size {
        return None;
    }
    let direction = source_vector / source_length;
    let tolerance = precision
        .modeling_resolution
        .max(precision.linear_agreement)
        * 64.0;
    scene
        .edges
        .iter()
        .filter(|edge| !edge.is_smooth)
        .filter_map(|edge| {
            let start = internal_protocol_point(edge.endpoints[0]);
            let end = internal_protocol_point(edge.endpoints[1]);
            let vector = end - start;
            let length = vector.length();
            if length <= precision.min_feature_size
                || direction.cross(vector / length).length() > 1.0e-6
            {
                return None;
            }
            let line_distance = |point: Point3| {
                let relative = point - source_start;
                (relative - direction * relative.dot(direction)).length()
            };
            if line_distance(start).max(line_distance(end)) > tolerance {
                return None;
            }
            let first = (start - source_start).dot(direction);
            let second = (end - source_start).dot(direction);
            let overlap = second.max(first).min(source_length) - second.min(first).max(0.0);
            (overlap > precision.min_feature_size).then_some((overlap, edge.source_edge))
        })
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, edge)| edge)
}

const fn internal_protocol_point(point: ProtocolPoint3) -> Point3 {
    Point3::new(point.x, point.y, point.z)
}

fn resolve_measure_entity(
    snapshot: &Snapshot,
    entity: EntityRef,
    expected: EntityKind,
    label: &str,
) -> Result<usize, KernelError> {
    if entity.snapshot != snapshot.id || entity.kind != expected {
        return Err(error(
            KernelErrorCode::InvalidInput,
            KernelStage::Preflight,
            snapshot.id,
            format!("the requested {label} is not owned by this snapshot"),
            Vec::new(),
        ));
    }
    let index = match expected {
        EntityKind::Edge => snapshot
            .topology
            .edges
            .iter()
            .position(|record| record.id.get() == entity.entity.0),
        EntityKind::Face => snapshot
            .topology
            .faces
            .iter()
            .position(|record| record.id.get() == entity.entity.0),
        _ => None,
    };
    index.ok_or_else(|| {
        error(
            KernelErrorCode::InvalidInput,
            KernelStage::Preflight,
            snapshot.id,
            format!("the requested {label} does not exist in this snapshot"),
            Vec::new(),
        )
    })
}

/// Which chordal deviation budget an arc-sampling site is allowed to spend.
///
/// `Authoritative` sampling feeds results that downstream modeling consumes
/// (for example the planar face-support boundary a sketch binds to), so it
/// honours the kernel approximation budget exactly. `Display` sampling feeds
/// only source-mapped diagnostic tessellation; display sampling never becomes
/// modeling authority, so it may spend a far larger, radius-proportional
/// presentation budget. Reusing the kernel budget (10 nm by default) for
/// display would emit thousands of segments per circle on palm-sized turned
/// parts and drown the interactive viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ChordBudget {
    Authoritative,
    Display,
    /// Display sampling additionally coarsened for bodies that project small
    /// on screen. The multiplier is clamped, and `arc_subdivisions` keeps its
    /// eight-chords-per-turn floor, so silhouettes degrade gracefully.
    DisplayScaled(f64),
}

impl ChordBudget {
    fn tolerance(self, radius: f64, precision: PrecisionPolicy) -> f64 {
        let authoritative = precision
            .approximation_budget
            .max(precision.modeling_resolution);
        // Sagitta of roughly radius/2500 with absolute floors keeps
        // silhouettes visually smooth (a 25 mm radius renders with ~110
        // chords per full circle) while bounding density for large parts.
        let display = (radius * 4.0e-4).clamp(5.0e-3, 0.1).max(authoritative);
        let tolerance = match self {
            Self::Authoritative => authoritative,
            Self::Display => display,
            Self::DisplayScaled(scale) => {
                if scale < 1.0 {
                    (display * scale.clamp(0.25, 1.0)).max(authoritative)
                } else {
                    display * scale.clamp(1.0, 64.0)
                }
            }
        };
        tolerance.min(radius)
    }
}

fn arc_subdivisions(
    radius: f64,
    sweep: f64,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> usize {
    let tolerance = budget.tolerance(radius, precision);
    let maximum_angle = if tolerance >= radius {
        std::f64::consts::FRAC_PI_2
    } else {
        (2.0 * (1.0 - tolerance / radius).acos()).max(precision.angular_agreement_radians)
    };
    let requested = (sweep.abs() / maximum_angle).ceil() as usize;
    let minimum = match budget {
        ChordBudget::DisplayScaled(scale) if scale <= 0.5 => {
            (sweep.abs() / (std::f64::consts::TAU / 128.0)).ceil() as usize
        }
        _ => (sweep.abs() / std::f64::consts::FRAC_PI_4).ceil() as usize,
    };
    let maximum = 1_usize << precision.max_subdivisions.min(12);
    requested.max(minimum).max(1).min(maximum)
}

fn sampled_edge_segments(
    edge: topology::Edge,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Vec<[Point3; 2]> {
    let subdivisions = match edge.curve {
        Curve3::Line { .. } => 1,
        Curve3::Circle { radius, .. } => arc_subdivisions(
            radius,
            edge.parameter_range.end - edge.parameter_range.start,
            budget,
            precision,
        ),
        // The semi-major axis bounds the sagitta of every chord.
        Curve3::Ellipse { major_radius, .. } => arc_subdivisions(
            major_radius,
            edge.parameter_range.end - edge.parameter_range.start,
            budget,
            precision,
        ),
    };
    (0..subdivisions)
        .map(|index| {
            let start_fraction = index as f64 / subdivisions as f64;
            let end_fraction = (index + 1) as f64 / subdivisions as f64;
            let range = edge.parameter_range;
            [
                edge.curve
                    .evaluate((range.end - range.start).mul_add(start_fraction, range.start)),
                edge.curve
                    .evaluate((range.end - range.start).mul_add(end_fraction, range.start)),
            ]
        })
        .collect()
}

fn linear_face_feature_owner_supported(topology: &Topology, face: EntityRef) -> bool {
    let Some(face_index) = topology
        .faces
        .iter()
        .position(|record| record.id.get() == face.entity.0)
    else {
        return false;
    };
    let mut owners = topology.solids.iter().filter_map(|solid| {
        solid
            .value
            .shells()
            .filter_map(|key| topology.shells.get(key.0))
            .find(|shell| shell.value.faces.iter().any(|key| key.0 == face_index))
    });
    let Some(owner) = owners.next() else {
        return false;
    };
    if owners.next().is_some() {
        return false;
    }
    owner.value.faces.iter().all(|key| {
        let Some(face) = topology.faces.get(key.0) else {
            return false;
        };
        face.value.surface.as_plane().is_some()
            && validator::face_polygon(topology, face.value.outer_loop)
                .is_some_and(|polygon| polygon.len() >= 3)
            && face.value.inner_loops.iter().all(|inner| {
                validator::face_polygon(topology, *inner).is_some_and(|polygon| polygon.len() >= 3)
            })
    })
}

fn sampled_loop_polygon(
    topology: &Topology,
    loop_key: topology::LoopKey,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Option<Vec<Point3>> {
    let loop_record = topology.loop_record(loop_key)?;
    let mut polygon = Vec::new();
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology.coedge(*coedge_key)?.value;
        let edge = topology.edge(coedge.edge)?.value;
        let range = edge.parameter_range;
        let subdivisions = match edge.curve {
            Curve3::Line { .. } => 1,
            Curve3::Circle { radius, .. } => {
                arc_subdivisions(radius, range.end - range.start, budget, precision)
            }
            Curve3::Ellipse { major_radius, .. } => {
                arc_subdivisions(major_radius, range.end - range.start, budget, precision)
            }
        };
        // Sample in the edge's own forward parameterization and reverse the
        // resulting points, rather than reversing the interval and sampling
        // backwards. The two are algebraically the same walk but not the same
        // bits: `mul_add` rounds once, so `fma(start - end, k/n, end)` can land
        // a unit in the last place away from `fma(end - start, (n - k)/n,
        // start)`. Geometrically that is nothing, and downstream it is fatal —
        // a face's boundary vertices are matched against the edge
        // tessellation's chords by exact identity, and a drifted vertex leaves
        // the chord belonging to no triangle of this face. A rim then paints
        // as a dashed arc wherever its other owner is back-facing. Sharing one
        // parameterization makes that agreement structural rather than lucky.
        let sample = |index: usize| {
            let fraction = index as f64 / subdivisions as f64;
            edge.curve
                .evaluate((range.end - range.start).mul_add(fraction, range.start))
        };
        match coedge.orientation {
            Orientation::Forward => polygon.extend((0..subdivisions).map(sample)),
            Orientation::Reverse => polygon.extend((1..=subdivisions).rev().map(sample)),
        }
    }
    Some(polygon)
}

/// Projects one loop's exact edge curves into a planar face frame.
///
/// The frame axes must be orthonormal and span the face plane. A circular edge
/// of a planar face has its axis along the face normal, so its in-plane axes
/// project to an orthonormal pair and the arc stays exact. Any edge whose
/// projection is not an isometry is dropped rather than approximated, keeping
/// the returned set trustworthy for reference use.
fn face_frame_loop_curves(
    topology: &Topology,
    loop_key: topology::LoopKey,
    origin: Point3,
    u: Vector3,
    v: Vector3,
) -> Option<Vec<FaceBoundaryCurve2>> {
    /// Squared-length slack for accepting a projected circle axis as unit.
    /// Face edges are constructed in-plane, so this only rejects genuinely
    /// out-of-plane input rather than absorbing drift.
    const AXIS_TOLERANCE: f64 = 1e-9;

    let project = |point: Point3| {
        let relative = point - origin;
        ProtocolPoint2::new(relative.dot(u), relative.dot(v))
    };
    let project_direction = |direction: Vector3| [direction.dot(u), direction.dot(v)];

    let loop_record = topology.loop_record(loop_key)?;
    let mut curves = Vec::with_capacity(loop_record.value.coedges.len());
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology.coedge(*coedge_key)?.value;
        let edge = topology.edge(coedge.edge)?.value;
        let (start, end) = match coedge.orientation {
            Orientation::Forward => (edge.parameter_range.start, edge.parameter_range.end),
            Orientation::Reverse => (edge.parameter_range.end, edge.parameter_range.start),
        };
        let curve = match edge.curve {
            // No planar reference curve exists for an ellipse yet.
            Curve3::Ellipse { .. } => return None,
            Curve3::Line { .. } => FaceBoundaryCurve2::Segment {
                endpoints: [
                    project(edge.curve.evaluate(start)),
                    project(edge.curve.evaluate(end)),
                ],
            },
            Curve3::Circle {
                center,
                u: circle_u,
                v: circle_v,
                radius,
            } => {
                let planar_u = project_direction(circle_u);
                let planar_v = project_direction(circle_v);
                let unit = |axis: [f64; 2]| {
                    (axis[0].mul_add(axis[0], axis[1] * axis[1]) - 1.0).abs() <= AXIS_TOLERANCE
                };
                let orthogonal = planar_u[0]
                    .mul_add(planar_v[0], planar_u[1] * planar_v[1])
                    .abs()
                    <= AXIS_TOLERANCE;
                if !unit(planar_u) || !unit(planar_v) || !orthogonal {
                    continue;
                }
                FaceBoundaryCurve2::Arc {
                    center: project(center),
                    u: planar_u,
                    v: planar_v,
                    radius,
                    start,
                    end,
                }
            }
        };
        if curve.is_finite() {
            curves.push(curve);
        }
    }
    Some(curves)
}

/// Exact area of a torus face from its line p-curve boundary via Green's
/// theorem: `A = r (R ∮u dv + r ∮u cos v dv)` with `u` the azimuth and `v`
/// the minor angle. Both boundary integrals are closed-form per segment, so
/// no tolerance enters the measure.
fn torus_face_area(
    topology: &Topology,
    face: &topology::Face,
    torus: topology::Torus,
) -> Option<f64> {
    let mut parameter_area = 0.0;
    let mut cosine_moment = 0.0;
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            let [start, end] = coedge.pcurve_endpoints();
            let (u0, v0, u1, v1) = (start.x, start.y, end.x, end.y);
            let delta_u = u1 - u0;
            let delta_v = v1 - v0;
            parameter_area += 0.5 * (u0 + u1) * delta_v;
            if delta_v.abs() > 1.0e-14 {
                cosine_moment += u0 * (v1.sin() - v0.sin())
                    + delta_u * (v1.sin() + (v1.cos() - v0.cos()) / delta_v);
            }
        }
    }
    let area = torus.minor_radius
        * (torus.major_radius * parameter_area + torus.minor_radius * cosine_moment);
    area.is_finite().then_some(area.abs())
}

/// A precomputed sampling grid for one revolved face: the radial direction
/// per azimuth column and the (cos, sin) pair per latitude row.
///
/// The naive quad loop calls the surface's `evaluate` four times per cell, so
/// every interior grid point pays its two sine/cosine pairs roughly four
/// times over. At the authoritative chord budget a single blend band easily
/// reaches millions of cells, which made trig the dominant cost of export
/// tessellation. Tabulating the azimuth directions and latitude angles once
/// reduces each grid point to a handful of fused multiply-adds, and building
/// each row a single time removes the fourfold duplication as well.
struct RevolvedGrid {
    columns: Vec<topology::Vector3>,
    rows: Vec<(f64, f64)>,
}

impl RevolvedGrid {
    fn new(
        radial_u: topology::Vector3,
        radial_v: topology::Vector3,
        angular_sign: f64,
        (u_min, u_max): (f64, f64),
        azimuthal: usize,
        (v_min, v_max): (f64, f64),
        meridional: usize,
    ) -> Self {
        let columns = (0..=azimuthal)
            .map(|column| {
                let u = (u_max - u_min).mul_add(column as f64 / azimuthal as f64, u_min);
                let (sin, cos) = (angular_sign * u).sin_cos();
                radial_u * cos + radial_v * sin
            })
            .collect();
        let rows = (0..=meridional)
            .map(|row| {
                let v = (v_max - v_min).mul_add(row as f64 / meridional as f64, v_min);
                v.sin_cos()
            })
            .collect();
        Self { columns, rows }
    }
}

fn tessellate_torus_face(
    topology: &Topology,
    face: &topology::Face,
    torus: topology::Torus,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Vec<[Point3; 3]> {
    let Some(loop_record) = topology.loop_record(face.outer_loop) else {
        return Vec::new();
    };
    let parameters = loop_record
        .value
        .coedges
        .iter()
        .filter_map(|coedge_key| topology.coedge(*coedge_key))
        .flat_map(|coedge| coedge.value.pcurve_endpoints())
        .collect::<Vec<_>>();
    let Some(first) = parameters.first().copied() else {
        return Vec::new();
    };
    let (mut u_min, mut u_max, mut v_min, mut v_max) = (first.x, first.x, first.y, first.y);
    for point in &parameters[1..] {
        u_min = u_min.min(point.x);
        u_max = u_max.max(point.x);
        v_min = v_min.min(point.y);
        v_max = v_max.max(point.y);
    }
    if u_max <= u_min || v_max <= v_min {
        return Vec::new();
    }
    let azimuthal = arc_subdivisions(
        torus.major_radius + torus.minor_radius,
        u_max - u_min,
        budget,
        precision,
    );
    let minor = arc_subdivisions(torus.minor_radius, v_max - v_min, budget, precision);
    let grid = RevolvedGrid::new(
        torus.radial_u,
        torus.radial_v,
        torus.angular_sign,
        (u_min, u_max),
        azimuthal,
        (v_min, v_max),
        minor,
    );
    // P(u, v) = origin + radial(u)·(R + r·cos v) + axis·r·sin v.
    let ring_row = |row: usize| -> Vec<Point3> {
        let (sin_v, cos_v) = grid.rows[row];
        let ring = torus.minor_radius.mul_add(cos_v, torus.major_radius);
        let lift = torus.axis * (torus.minor_radius * sin_v);
        grid.columns
            .iter()
            .map(|radial| torus.origin + *radial * ring + lift)
            .collect()
    };
    // One contiguous fill: the grid is memory-bound at export density, so a
    // single sweep over shared row buffers beats any further fan-out.
    let mut triangles = Vec::with_capacity(azimuthal * minor * 2);
    let mut low = ring_row(0);
    for row in 0..minor {
        let high = ring_row(row + 1);
        for column in 0..azimuthal {
            let a = low[column];
            let b = low[column + 1];
            let c = high[column + 1];
            let d = high[column];
            triangles.push([a, b, c]);
            triangles.push([a, c, d]);
        }
        low = high;
    }
    triangles
}

/// Exact area of a sphere patch over its rectangular parameter domain:
/// `A = r² · Δu · (sin v₁ − sin v₀)`.
fn sphere_face_area(
    topology: &Topology,
    face: &topology::Face,
    sphere: topology::Sphere,
) -> Option<f64> {
    let (u_min, u_max, v_min, v_max) = face_parameter_bounds(topology, face)?;
    let area = sphere.radius * sphere.radius * (u_max - u_min) * (v_max.sin() - v_min.sin());
    area.is_finite().then_some(area.abs())
}

/// Parameter-space extent of a face's outer loop.
fn face_parameter_bounds(
    topology: &Topology,
    face: &topology::Face,
) -> Option<(f64, f64, f64, f64)> {
    let loop_record = topology.loop_record(face.outer_loop)?;
    let mut u_min = f64::INFINITY;
    let mut u_max = f64::NEG_INFINITY;
    let mut v_min = f64::INFINITY;
    let mut v_max = f64::NEG_INFINITY;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology.coedge(*coedge_key)?.value;
        for point in coedge.pcurve_endpoints() {
            u_min = u_min.min(point.x);
            u_max = u_max.max(point.x);
            v_min = v_min.min(point.y);
            v_max = v_max.max(point.y);
        }
    }
    (u_min < u_max && v_min < v_max).then_some((u_min, u_max, v_min, v_max))
}

fn tessellate_sphere_face(
    topology: &Topology,
    face: &topology::Face,
    sphere: topology::Sphere,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Vec<[Point3; 3]> {
    let Some((u_min, u_max, v_min, v_max)) = face_parameter_bounds(topology, face) else {
        return Vec::new();
    };
    let azimuthal = arc_subdivisions(sphere.radius, u_max - u_min, budget, precision);
    let meridional = arc_subdivisions(sphere.radius, v_max - v_min, budget, precision);
    let grid = RevolvedGrid::new(
        sphere.radial_u,
        sphere.radial_v,
        sphere.angular_sign,
        (u_min, u_max),
        azimuthal,
        (v_min, v_max),
        meridional,
    );
    // P(u, v) = origin + radial(u)·r·cos v + axis·r·sin v.
    let ring_row = |row: usize| -> Vec<Point3> {
        let (sin_v, cos_v) = grid.rows[row];
        let ring = sphere.radius * cos_v;
        let lift = sphere.axis * (sphere.radius * sin_v);
        grid.columns
            .iter()
            .map(|radial| sphere.origin + *radial * ring + lift)
            .collect()
    };
    let mut triangles = Vec::with_capacity(azimuthal * meridional * 2);
    let mut low = ring_row(0);
    let mut low_degenerate = grid.rows[0].1.abs() <= f64::EPSILON;
    for row in 0..meridional {
        let high = ring_row(row + 1);
        // The ring collapses at a pole; keep the surviving triangle.
        let high_degenerate = grid.rows[row + 1].1.abs() <= f64::EPSILON;
        for column in 0..azimuthal {
            let a = low[column];
            let b = low[column + 1];
            let c = high[column + 1];
            let d = high[column];
            if low_degenerate && high_degenerate {
                continue;
            }
            if low_degenerate {
                triangles.push([a, c, d]);
            } else if high_degenerate {
                triangles.push([a, b, c]);
            } else {
                triangles.push([a, b, c]);
                triangles.push([a, c, d]);
            }
        }
        low = high;
        low_degenerate = high_degenerate;
    }
    triangles
}

/// Exact lateral area of a cone-frustum face from its rectangular p-curve
/// bounds: `A = sqrt(1 + slope^2) * sweep * mean_ring_radius * dv`.
fn cone_face_area(topology: &Topology, face: &topology::Face, cone: topology::Cone) -> Option<f64> {
    let mut u_min = f64::INFINITY;
    let mut u_max = f64::NEG_INFINITY;
    let mut v_min = f64::INFINITY;
    let mut v_max = f64::NEG_INFINITY;
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            for point in coedge.pcurve_endpoints() {
                u_min = u_min.min(point.x);
                u_max = u_max.max(point.x);
                v_min = v_min.min(point.y);
                v_max = v_max.max(point.y);
            }
        }
    }
    if u_max <= u_min || v_max <= v_min {
        return None;
    }
    let slant = (1.0 + cone.slope * cone.slope).sqrt();
    let mean_radius = 0.5 * (cone.ring_radius(v_min) + cone.ring_radius(v_max));
    let area = slant * (u_max - u_min) * mean_radius * (v_max - v_min);
    area.is_finite().then_some(area.abs())
}

fn tessellate_cone_face(
    topology: &Topology,
    face: &topology::Face,
    cone: topology::Cone,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Vec<[Point3; 3]> {
    let Some(loop_record) = topology.loop_record(face.outer_loop) else {
        return Vec::new();
    };
    let parameters = loop_record
        .value
        .coedges
        .iter()
        .filter_map(|coedge_key| topology.coedge(*coedge_key))
        .flat_map(|coedge| coedge.value.pcurve_endpoints())
        .collect::<Vec<_>>();
    let Some(first) = parameters.first().copied() else {
        return Vec::new();
    };
    let (mut u_min, mut u_max, mut v_min, mut v_max) = (first.x, first.x, first.y, first.y);
    for point in &parameters[1..] {
        u_min = u_min.min(point.x);
        u_max = u_max.max(point.x);
        v_min = v_min.min(point.y);
        v_max = v_max.max(point.y);
    }
    if u_max <= u_min || v_max <= v_min {
        return Vec::new();
    }
    let widest = cone
        .ring_radius(v_min)
        .abs()
        .max(cone.ring_radius(v_max).abs());
    let azimuthal = arc_subdivisions(widest, u_max - u_min, budget, precision);
    // The frustum is ruled along v: one band of quads suffices.
    let mut triangles = Vec::with_capacity(azimuthal * 2);
    for column in 0..azimuthal {
        let u0 = (u_max - u_min).mul_add(column as f64 / azimuthal as f64, u_min);
        let u1 = (u_max - u_min).mul_add((column + 1) as f64 / azimuthal as f64, u_min);
        let a = cone.evaluate(topology::Point2::new(u0, v_min));
        let b = cone.evaluate(topology::Point2::new(u1, v_min));
        let c = cone.evaluate(topology::Point2::new(u1, v_max));
        let d = cone.evaluate(topology::Point2::new(u0, v_max));
        triangles.push([a, b, c]);
        triangles.push([a, c, d]);
    }
    triangles
}

fn tessellate_cylinder_face(
    topology: &Topology,
    face: &topology::Face,
    cylinder: topology::Cylinder,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Vec<[Point3; 3]> {
    if !face.inner_loops.is_empty() {
        return Vec::new();
    }
    let Some(loop_record) = topology.loop_record(face.outer_loop) else {
        return Vec::new();
    };
    let coedges = loop_record
        .value
        .coedges
        .iter()
        .filter_map(|coedge_key| topology.coedge(*coedge_key))
        .map(|coedge| coedge.value)
        .collect::<Vec<_>>();
    if coedges
        .iter()
        .any(|coedge| matches!(coedge.pcurve, Curve2::Harmonic { .. }))
    {
        return tessellate_harmonic_cylinder_face(&coedges, cylinder, budget, precision);
    }
    let parameters = coedges
        .iter()
        .flat_map(|coedge| coedge.pcurve_endpoints())
        .collect::<Vec<_>>();
    let Some(first) = parameters.first().copied() else {
        return Vec::new();
    };
    let (mut u_min, mut u_max, mut v_min, mut v_max) = (first.x, first.x, first.y, first.y);
    for point in &parameters[1..] {
        u_min = u_min.min(point.x);
        u_max = u_max.max(point.x);
        v_min = v_min.min(point.y);
        v_max = v_max.max(point.y);
    }
    if u_max <= u_min || v_max <= v_min {
        return Vec::new();
    }
    let subdivisions = arc_subdivisions(cylinder.radius, u_max - u_min, budget, precision);
    let mut triangles = Vec::with_capacity(subdivisions * 2);
    for index in 0..subdivisions {
        let first = index as f64 / subdivisions as f64;
        let second = (index + 1) as f64 / subdivisions as f64;
        let u0 = (u_max - u_min).mul_add(first, u_min);
        let u1 = (u_max - u_min).mul_add(second, u_min);
        let p00 = cylinder.evaluate(topology::Point2::new(u0, v_min));
        let p10 = cylinder.evaluate(topology::Point2::new(u1, v_min));
        let p11 = cylinder.evaluate(topology::Point2::new(u1, v_max));
        let p01 = cylinder.evaluate(topology::Point2::new(u0, v_max));
        triangles.push([p00, p10, p11]);
        triangles.push([p00, p11, p01]);
    }
    triangles
}

/// Tessellates a cylinder face whose parameter region is bounded by a
/// harmonic — the mitre seam of a fillet turning a reflex corner — as a
/// sequence of azimuth strips. Each strip's axial extent is read off the
/// boundary at its two azimuths, which is exact for the azimuth-monotone
/// regions such seams bound.
fn tessellate_harmonic_cylinder_face(
    coedges: &[topology::Coedge],
    cylinder: topology::Cylinder,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Vec<[Point3; 3]> {
    let mut u_min = f64::INFINITY;
    let mut u_max = f64::NEG_INFINITY;
    for coedge in coedges {
        for point in coedge.pcurve_endpoints() {
            u_min = u_min.min(point.x);
            u_max = u_max.max(point.x);
        }
    }
    if u_max.partial_cmp(&u_min) != Some(std::cmp::Ordering::Greater) {
        return Vec::new();
    }
    let tolerance = (u_max - u_min) * 1.0e-9;
    // The axial interval the region covers at one azimuth: the lowest and
    // highest boundary crossings of that vertical line.
    let extent = |theta: f64| -> Option<(f64, f64)> {
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        let mut note = |v: f64| {
            low = low.min(v);
            high = high.max(v);
        };
        for coedge in coedges {
            let range = coedge.parameter_range;
            match coedge.pcurve {
                Curve2::Line { .. } => {
                    let [start, end] = coedge.pcurve_endpoints();
                    let across = end.x - start.x;
                    if across.abs() <= tolerance {
                        if (theta - start.x).abs() <= tolerance {
                            note(start.y);
                            note(end.y);
                        }
                    } else if (theta - start.x.min(end.x)) >= -tolerance
                        && (start.x.max(end.x) - theta) >= -tolerance
                    {
                        note(start.y + (end.y - start.y) * (theta - start.x) / across);
                    }
                }
                Curve2::Harmonic { .. } => {
                    let from = range.start.min(range.end);
                    let to = range.start.max(range.end);
                    if theta >= from - tolerance && theta <= to + tolerance {
                        note(coedge.pcurve.evaluate(theta.clamp(from, to)).y);
                    }
                }
                Curve2::Circle { .. } => return None,
            }
        }
        (low.is_finite() && high.is_finite() && high >= low).then_some((low, high))
    };
    let subdivisions = arc_subdivisions(cylinder.radius, u_max - u_min, budget, precision).max(4);
    let mut triangles = Vec::with_capacity(subdivisions * 2);
    for index in 0..subdivisions {
        let u0 = (u_max - u_min).mul_add(index as f64 / subdivisions as f64, u_min);
        let u1 = (u_max - u_min).mul_add((index + 1) as f64 / subdivisions as f64, u_min);
        let (Some((low0, high0)), Some((low1, high1))) = (extent(u0), extent(u1)) else {
            return Vec::new();
        };
        let p00 = cylinder.evaluate(topology::Point2::new(u0, low0));
        let p10 = cylinder.evaluate(topology::Point2::new(u1, low1));
        let p11 = cylinder.evaluate(topology::Point2::new(u1, high1));
        let p01 = cylinder.evaluate(topology::Point2::new(u0, high0));
        if high1 > low1 {
            triangles.push([p00, p10, p11]);
        }
        if high0 > low0 {
            triangles.push([p00, p11, p01]);
        }
    }
    triangles
}

#[derive(Clone, Copy)]
struct TessellationVertex {
    point: Point3,
    projected: topology::Point2,
}

fn triangulate_face_boundaries(
    boundaries: &[Vec<Point3>],
    plane: topology::Plane,
    fallback: TessellationFallback,
) -> Vec<[Point3; 3]> {
    let Some(outer) = boundaries.first() else {
        return Vec::new();
    };
    if boundaries.len() == 1 {
        return triangulate_face_polygon(outer, plane, fallback)
            .into_iter()
            .map(|triangle| triangle.map(|vertex| outer[vertex]))
            .collect();
    }

    let projected_boundaries = boundaries
        .iter()
        .map(|boundary| {
            boundary
                .iter()
                .map(|point| plane.project(*point))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut polygon = outer
        .iter()
        .zip(&projected_boundaries[0])
        .map(|(point, projected)| TessellationVertex {
            point: *point,
            projected: *projected,
        })
        .collect::<Vec<_>>();

    // Stitch each clockwise void boundary into the counter-clockwise outer
    // walk through a deterministic, visibility-tested zero-width bridge. The
    // bridge is display-only; authoritative topology retains distinct loops.
    let mut hole_order = (1..boundaries.len()).collect::<Vec<_>>();
    hole_order.sort_by(|left, right| {
        rightmost_vertex(&projected_boundaries[*right])
            .map(|index| projected_boundaries[*right][index])
            .unwrap_or_default()
            .x
            .total_cmp(
                &rightmost_vertex(&projected_boundaries[*left])
                    .map(|index| projected_boundaries[*left][index])
                    .unwrap_or_default()
                    .x,
            )
            .then_with(|| left.cmp(right))
    });

    for boundary_index in hole_order {
        let hole = &boundaries[boundary_index];
        let projected_hole = &projected_boundaries[boundary_index];
        let Some(hole_vertex) = rightmost_vertex(projected_hole) else {
            return Vec::new();
        };
        let hole_point = projected_hole[hole_vertex];
        let mut candidates = (0..polygon.len()).collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            squared_distance_2d(polygon[*left].projected, hole_point)
                .total_cmp(&squared_distance_2d(polygon[*right].projected, hole_point))
                .then_with(|| left.cmp(right))
        });
        let visible = candidates.iter().copied().find(|candidate| {
            bridge_is_visible(
                polygon[*candidate].projected,
                hole_point,
                *candidate,
                hole_vertex,
                &polygon,
                boundary_index,
                &projected_boundaries,
            )
        });
        let outer_vertex =
            match (visible, fallback) {
                (Some(vertex), _) => vertex,
                // Never fill a void when authoritative tessellation cannot
                // certify a bridge. Omitting the one face is safer than handing
                // the Boolean tier material where topology says there is none.
                (None, TessellationFallback::Refuse) => return Vec::new(),
                // For display only, accept the nearest bridge whose midpoint at
                // least lies inside the outer boundary and outside every other
                // hole; a bridge that crosses another hole would overlap it.
                (None, TessellationFallback::Display) => {
                    let Some(vertex) = candidates.iter().copied().find(|candidate| {
                        let outer = polygon[*candidate].projected;
                        let midpoint = topology::Point2::new(
                            (outer.x + hole_point.x) * 0.5,
                            (outer.y + hole_point.y) * 0.5,
                        );
                        point_in_polygon_2d(midpoint, &projected_boundaries[0])
                            && projected_boundaries.iter().enumerate().skip(1).all(
                                |(index, hole)| {
                                    index == boundary_index || !point_in_polygon_2d(midpoint, hole)
                                },
                            )
                    }) else {
                        return Vec::new();
                    };
                    vertex
                }
            };

        let outer_bridge = polygon[outer_vertex];
        let inner_bridge = TessellationVertex {
            point: hole[hole_vertex],
            projected: hole_point,
        };
        let mut stitched = Vec::with_capacity(polygon.len() + hole.len() + 2);
        stitched.extend_from_slice(&polygon[..=outer_vertex]);
        stitched.push(inner_bridge);
        for offset in 1..hole.len() {
            let index = (hole_vertex + offset) % hole.len();
            stitched.push(TessellationVertex {
                point: hole[index],
                projected: projected_hole[index],
            });
        }
        stitched.push(inner_bridge);
        stitched.push(outer_bridge);
        stitched.extend_from_slice(&polygon[outer_vertex + 1..]);
        polygon = stitched;
    }

    let projected = polygon
        .iter()
        .map(|vertex| vertex.projected)
        .collect::<Vec<_>>();
    // A stitched polygon is never fanned: a fan from one vertex over a loop
    // that carries bridges fills the very holes the bridges keep open. When
    // the clip cannot resolve it, the face is omitted under either budget.
    ear_clip_polygon(&projected, fallback)
        .unwrap_or_default()
        .into_iter()
        .map(|triangle| triangle.map(|vertex| polygon[vertex].point))
        .collect()
}

fn rightmost_vertex(polygon: &[topology::Point2]) -> Option<usize> {
    polygon
        .iter()
        .enumerate()
        .map(|(index, point)| (index, *point))
        .max_by(|(left_index, left), (right_index, right)| {
            left.x
                .total_cmp(&right.x)
                .then_with(|| right.y.total_cmp(&left.y))
                .then_with(|| right_index.cmp(left_index))
        })
        .map(|(index, _)| index)
}

fn bridge_is_visible(
    outer: topology::Point2,
    inner: topology::Point2,
    outer_index: usize,
    inner_index: usize,
    polygon: &[TessellationVertex],
    active_hole: usize,
    boundaries: &[Vec<topology::Point2>],
) -> bool {
    if same_point_2d(outer, inner) {
        return false;
    }
    for edge_start in 0..polygon.len() {
        let edge_end = (edge_start + 1) % polygon.len();
        if edge_start == outer_index || edge_end == outer_index {
            continue;
        }
        let start = polygon[edge_start].projected;
        let end = polygon[edge_end].projected;
        if same_point_2d(start, outer)
            || same_point_2d(end, outer)
            || same_point_2d(start, inner)
            || same_point_2d(end, inner)
        {
            continue;
        }
        if segments_intersect_2d(outer, inner, start, end) {
            return false;
        }
    }
    for (boundary_index, boundary) in boundaries.iter().enumerate().skip(1) {
        for edge_start in 0..boundary.len() {
            let edge_end = (edge_start + 1) % boundary.len();
            if boundary_index == active_hole
                && (edge_start == inner_index || edge_end == inner_index)
            {
                continue;
            }
            let start = boundary[edge_start];
            let end = boundary[edge_end];
            if same_point_2d(start, outer)
                || same_point_2d(end, outer)
                || same_point_2d(start, inner)
                || same_point_2d(end, inner)
            {
                continue;
            }
            if segments_intersect_2d(outer, inner, start, end) {
                return false;
            }
        }
    }
    let midpoint = topology::Point2::new((outer.x + inner.x) * 0.5, (outer.y + inner.y) * 0.5);
    point_in_polygon_2d(midpoint, &boundaries[0])
        && boundaries
            .iter()
            .enumerate()
            .skip(1)
            .all(|(index, hole)| index == active_hole || !point_in_polygon_2d(midpoint, hole))
}

/// How a face that the exact stitching cannot triangulate is treated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TessellationFallback {
    /// Authoritative sampling: a face that cannot be certified is omitted,
    /// never guessed, because this scene feeds the faceted Boolean tier and
    /// faceted interchange export.
    Refuse,
    /// Display sampling: a hole may be bridged by a containment test and a
    /// stalled clip may fall back to a fan, so the viewer sees material where
    /// the topology certifies material even when the diagnostic stitch is
    /// numerically unresolved. Nothing here reaches a snapshot or a measure.
    Display,
}

fn point_strictly_in_triangle(
    point: topology::Point2,
    first: topology::Point2,
    second: topology::Point2,
    third: topology::Point2,
    tolerance: f64,
) -> bool {
    let a = signed_area_2d(first, second, point);
    let b = signed_area_2d(second, third, point);
    let c = signed_area_2d(third, first, point);
    a > tolerance && b > tolerance && c > tolerance
}

/// The squared diagonal of a polygon's bounding box: the scale every area
/// tolerance below is relative to, so a part in metres and one in microns
/// clip the same way.
fn projected_extent_squared(projected: &[topology::Point2]) -> f64 {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for point in projected {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    let extent = (max_x - min_x).mul_add(max_x - min_x, (max_y - min_y).powi(2));
    if extent.is_finite() && extent > 0.0 {
        extent
    } else {
        1.0
    }
}

fn ear_clip_polygon(
    projected: &[topology::Point2],
    fallback: TessellationFallback,
) -> Option<Vec<[usize; 3]>> {
    if projected.len() < 3 {
        return Some(Vec::new());
    }
    let extent = projected_extent_squared(projected);
    let area_tolerance = extent * 1.0e-12;
    let collinear_tolerance = extent * 1.0e-10;
    let mut remaining = (0..projected.len()).collect::<Vec<_>>();
    let mut triangles = Vec::with_capacity(projected.len().saturating_sub(2));

    let mut stalled_iterations = 0;
    while remaining.len() > 3 {
        let mut best_ear = None;

        for current in 0..remaining.len() {
            let previous = (current + remaining.len() - 1) % remaining.len();
            let next = (current + 1) % remaining.len();
            let p_prev = projected[remaining[previous]];
            let p_curr = projected[remaining[current]];
            let p_next = projected[remaining[next]];

            // A bridge stitches a hole in through two coincident vertices;
            // the zero-width ear between them is removed without a triangle.
            if same_point_2d(p_prev, p_curr)
                || same_point_2d(p_curr, p_next)
                || same_point_2d(p_prev, p_next)
            {
                best_ear = Some((
                    current,
                    [remaining[previous], remaining[current], remaining[next]],
                ));
                break;
            }

            let area = signed_area_2d(p_prev, p_curr, p_next);
            if area <= area_tolerance {
                continue;
            }

            let triangle = [remaining[previous], remaining[current], remaining[next]];
            let has_interior_vertex = remaining.iter().copied().any(|candidate| {
                if candidate == triangle[0] || candidate == triangle[1] || candidate == triangle[2]
                {
                    return false;
                }
                let pt = projected[candidate];
                if same_point_2d(pt, p_prev)
                    || same_point_2d(pt, p_curr)
                    || same_point_2d(pt, p_next)
                {
                    return false;
                }
                point_strictly_in_triangle(pt, p_prev, p_curr, p_next, area_tolerance)
            });

            if !has_interior_vertex {
                best_ear = Some((current, triangle));
                break;
            }
        }

        if let Some((current, triangle)) = best_ear {
            if signed_area_2d(
                projected[triangle[0]],
                projected[triangle[1]],
                projected[triangle[2]],
            ) > area_tolerance
            {
                triangles.push(triangle);
            }
            remaining.remove(current);
            stalled_iterations = 0;
        } else {
            // No ear: the polygon is not simple at this tolerance. An
            // authoritative scene must not guess; a display scene may drop a
            // collinear vertex and, failing that, fan what remains.
            if fallback == TessellationFallback::Refuse {
                return None;
            }
            stalled_iterations += 1;
            if stalled_iterations > remaining.len() {
                for k in 1..remaining.len().saturating_sub(1) {
                    let t = [remaining[0], remaining[k], remaining[k + 1]];
                    if signed_area_2d(projected[t[0]], projected[t[1]], projected[t[2]]).abs()
                        > area_tolerance
                    {
                        triangles.push(t);
                    }
                }
                break;
            }
            let mut removed = false;
            for current in 0..remaining.len() {
                let previous = (current + remaining.len() - 1) % remaining.len();
                let next = (current + 1) % remaining.len();
                let area = signed_area_2d(
                    projected[remaining[previous]],
                    projected[remaining[current]],
                    projected[remaining[next]],
                );
                if area.abs() <= collinear_tolerance {
                    remaining.remove(current);
                    removed = true;
                    break;
                }
            }
            if !removed && !remaining.is_empty() {
                remaining.remove(0);
            }
        }
    }

    if remaining.len() == 3 {
        let t = [remaining[0], remaining[1], remaining[2]];
        if signed_area_2d(projected[t[0]], projected[t[1]], projected[t[2]]).abs() > area_tolerance
        {
            triangles.push(t);
        }
    }

    Some(triangles)
}

fn signed_area_2d(
    first: topology::Point2,
    second: topology::Point2,
    third: topology::Point2,
) -> f64 {
    (second.x - first.x) * (third.y - first.y) - (second.y - first.y) * (third.x - first.x)
}

fn segments_intersect_2d(
    first_start: topology::Point2,
    first_end: topology::Point2,
    second_start: topology::Point2,
    second_end: topology::Point2,
) -> bool {
    let orientations = [
        signed_area_2d(first_start, first_end, second_start),
        signed_area_2d(first_start, first_end, second_end),
        signed_area_2d(second_start, second_end, first_start),
        signed_area_2d(second_start, second_end, first_end),
    ];
    if orientations.contains(&0.0) {
        return point_on_segment_2d(first_start, second_start, second_end)
            || point_on_segment_2d(first_end, second_start, second_end)
            || point_on_segment_2d(second_start, first_start, first_end)
            || point_on_segment_2d(second_end, first_start, first_end);
    }
    orientations[0].is_sign_positive() != orientations[1].is_sign_positive()
        && orientations[2].is_sign_positive() != orientations[3].is_sign_positive()
}

fn point_on_segment_2d(
    point: topology::Point2,
    start: topology::Point2,
    end: topology::Point2,
) -> bool {
    signed_area_2d(start, end, point) == 0.0
        && point.x >= start.x.min(end.x)
        && point.x <= start.x.max(end.x)
        && point.y >= start.y.min(end.y)
        && point.y <= start.y.max(end.y)
}

fn point_in_polygon_2d(point: topology::Point2, polygon: &[topology::Point2]) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let start = polygon[index];
        let end = polygon[(index + 1) % polygon.len()];
        if point_on_segment_2d(point, start, end) {
            return true;
        }
        if (start.y > point.y) != (end.y > point.y)
            && point.x < (end.x - start.x) * (point.y - start.y) / (end.y - start.y) + start.x
        {
            inside = !inside;
        }
    }
    inside
}

fn squared_distance_2d(left: topology::Point2, right: topology::Point2) -> f64 {
    (left.x - right.x).mul_add(left.x - right.x, (left.y - right.y).powi(2))
}

fn same_point_2d(left: topology::Point2, right: topology::Point2) -> bool {
    left.x == right.x && left.y == right.y
}

fn triangulate_face_polygon(
    polygon: &[Point3],
    plane: topology::Plane,
    fallback: TessellationFallback,
) -> Vec<[usize; 3]> {
    if polygon.len() < 3 {
        return Vec::new();
    }
    let projected = polygon
        .iter()
        .map(|point| plane.project(*point))
        .collect::<Vec<_>>();
    ear_clip_polygon(&projected, fallback).unwrap_or_else(|| match fallback {
        // Publication never depends on diagnostic tessellation. A fan is
        // retained only as a fail-soft visualization for a numerically
        // unresolved hole-free face; authoritative topology and validation
        // remain unchanged.
        TessellationFallback::Display => (1..polygon.len() - 1)
            .map(|index| [0, index, index + 1])
            .collect(),
        TessellationFallback::Refuse => Vec::new(),
    })
}

#[cfg(test)]
fn point_in_or_on_triangle(
    point: topology::Point2,
    first: topology::Point2,
    second: topology::Point2,
    third: topology::Point2,
) -> bool {
    signed_area_2d(first, second, point) >= 0.0
        && signed_area_2d(second, third, point) >= 0.0
        && signed_area_2d(third, first, point) >= 0.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryMode {
    Generated,
    OneToOne,
    Extrusion {
        profile_vertices: usize,
    },
    FaceFeature {
        operation: FaceExtrusionOperation,
        target_face: EntityRef,
        exit_face: Option<EntityRef>,
    },
    RegularizedFaceFeature,
    FacePushPull {
        target_face: EntityRef,
    },
}

fn validate_precision(snapshot: SnapshotId, precision: PrecisionPolicy) -> Result<(), KernelError> {
    let positive_finite = [
        precision.modeling_resolution,
        precision.linear_agreement,
        precision.angular_agreement_radians,
        precision.parameter_resolution,
        precision.approximation_budget,
        precision.max_entity_uncertainty,
        precision.max_operation_uncertainty,
        precision.max_abs_coordinate,
        precision.min_feature_size,
    ]
    .into_iter()
    .all(|value| value.is_finite() && value > 0.0);
    let coherent = precision.max_iterations > 0
        && precision.max_subdivisions > 0
        && precision.min_feature_size >= precision.modeling_resolution
        && precision.max_operation_uncertainty >= precision.max_entity_uncertainty;
    if positive_finite && coherent {
        return Ok(());
    }
    Err(error(
        KernelErrorCode::InvalidInput,
        KernelStage::Preflight,
        snapshot,
        "precision policy is non-finite, non-positive, or internally inconsistent",
        vec![simple_diagnostic(
            "PRECISION_POLICY_INVALID",
            KernelStage::Preflight,
            "Precision values and resource limits must form a finite, positive, coherent policy.",
        )],
    ))
}

fn validate_cuboid_input(
    snapshot: SnapshotId,
    origin: ProtocolPoint3,
    extents: [f64; 3],
    precision: PrecisionPolicy,
) -> Result<(), KernelError> {
    if !origin.is_finite() || extents.iter().any(|extent| !extent.is_finite()) {
        return Err(error(
            KernelErrorCode::InvalidInput,
            KernelStage::Preflight,
            snapshot,
            "cuboid origin and extents must be finite",
            vec![simple_diagnostic(
                "CUBOID_INPUT_NON_FINITE",
                KernelStage::Preflight,
                "At least one cuboid coordinate or extent is NaN or infinite.",
            )],
        ));
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if extents.iter().any(|extent| *extent <= minimum) {
        let measured = extents.into_iter().fold(f64::INFINITY, f64::min);
        let mut diagnostic = simple_diagnostic(
            "CUBOID_EXTENT_TOO_SMALL",
            KernelStage::Preflight,
            "Each cuboid extent must exceed the supported minimum feature size.",
        );
        attach_measurement(
            &mut diagnostic,
            QuantityKind::Length,
            measured,
            NumericInterval {
                min: Some(minimum),
                max: None,
            },
        );
        return Err(error(
            KernelErrorCode::InvalidInput,
            KernelStage::Preflight,
            snapshot,
            "cuboid extents must be strictly larger than the active minimum feature size",
            vec![diagnostic],
        ));
    }

    let end = [
        origin.x + extents[0],
        origin.y + extents[1],
        origin.z + extents[2],
    ];
    let coordinates = [origin.x, origin.y, origin.z, end[0], end[1], end[2]];
    if coordinates
        .into_iter()
        .any(|value| !value.is_finite() || value.abs() > precision.max_abs_coordinate)
    {
        return Err(error(
            KernelErrorCode::ResourceLimitExceeded,
            KernelStage::Preflight,
            snapshot,
            "cuboid exceeds the active coordinate envelope",
            vec![simple_diagnostic(
                "COORDINATE_LIMIT_EXCEEDED",
                KernelStage::Preflight,
                "Origin or derived corner lies outside the configured coordinate limit.",
            )],
        ));
    }
    Ok(())
}

fn validate_extrusion_source(input: &Snapshot) -> Result<(), KernelError> {
    if input.counts().total() == 0 {
        return Ok(());
    }
    Err(error(
        KernelErrorCode::Unsupported,
        KernelStage::Preflight,
        input.id,
        "the experimental extrusion constructor requires an empty input snapshot",
        vec![simple_diagnostic(
            "EXTRUDE_SOURCE_NOT_EMPTY",
            KernelStage::Preflight,
            "ExtrudePolygon v0 constructs one new solid only from the empty snapshot.",
        )],
    ))
}

fn extrusion_input_error(snapshot: SnapshotId, reason: ExtrusionInputError) -> KernelError {
    let (code, diagnostic, message) = match reason {
        ExtrusionInputError::NonFinite => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_INPUT_NON_FINITE",
            "extrusion frame, profile, and distance must be finite",
        ),
        ExtrusionInputError::TooFewVertices => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_TOO_FEW_VERTICES",
            "a polygon extrusion requires at least three vertices",
        ),
        ExtrusionInputError::TooManyVertices => (
            KernelErrorCode::ResourceLimitExceeded,
            "EXTRUDE_TOO_MANY_VERTICES",
            "ExtrudePolygon v0 supports at most 256 profile vertices",
        ),
        ExtrusionInputError::RepeatedVertex => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_REPEATED_VERTEX",
            "the polygon profile contains a repeated vertex",
        ),
        ExtrusionInputError::SelfIntersecting => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_PROFILE_SELF_INTERSECTING",
            "the polygon profile must be simple and non-self-intersecting",
        ),
        ExtrusionInputError::NumericallyIndeterminate => (
            KernelErrorCode::NumericallyIndeterminate,
            "EXTRUDE_PROFILE_NUMERICALLY_INDETERMINATE",
            "the polygon profile could not be certified by the active numerical filters",
        ),
        ExtrusionInputError::DegenerateFrame => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_FRAME_DEGENERATE",
            "the planar frame axes must be non-zero and non-parallel",
        ),
        ExtrusionInputError::NonPositiveDistance => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_DISTANCE_NON_POSITIVE",
            "extrusion distance must be strictly positive",
        ),
        ExtrusionInputError::FeatureTooSmall => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_FEATURE_TOO_SMALL",
            "profile edges, internal separations, and extrusion distance must exceed the active minimum feature size",
        ),
        ExtrusionInputError::AreaTooSmall => (
            KernelErrorCode::InvalidInput,
            "EXTRUDE_AREA_TOO_SMALL",
            "polygon area must exceed the active minimum area scale",
        ),
        ExtrusionInputError::CoordinateLimit => (
            KernelErrorCode::ResourceLimitExceeded,
            "EXTRUDE_COORDINATE_LIMIT_EXCEEDED",
            "extrusion parameters or derived world points exceed the active coordinate envelope",
        ),
        ExtrusionInputError::PrecisionUnrepresentable => (
            KernelErrorCode::NumericallyIndeterminate,
            "EXTRUDE_PRECISION_UNREPRESENTABLE",
            "extrusion placement cannot preserve the requested geometry within the active numerical agreement",
        ),
    };
    error(
        code,
        KernelStage::Preflight,
        snapshot,
        message,
        vec![simple_diagnostic(
            diagnostic,
            KernelStage::Preflight,
            message,
        )],
    )
}

fn face_feature_input_error(snapshot: SnapshotId, reason: FaceFeatureInputError) -> KernelError {
    let (code, diagnostic, message) = match reason {
        FaceFeatureInputError::NonFinite => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_INPUT_NON_FINITE",
            "face extrusion inputs must be finite",
        ),
        FaceFeatureInputError::SourceUnsupported => (
            KernelErrorCode::Unsupported,
            "FACE_FEATURE_SOURCE_UNSUPPORTED",
            "face extrusion requires one valid linear-faced shell containing one solid",
        ),
        FaceFeatureInputError::TargetSnapshotMismatch => (
            KernelErrorCode::StaleSnapshot,
            "FACE_FEATURE_TARGET_STALE",
            "the selected face belongs to a different snapshot",
        ),
        FaceFeatureInputError::TargetNotFace => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_TARGET_NOT_FACE",
            "the extrusion target must be a face",
        ),
        FaceFeatureInputError::TargetMissing => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_TARGET_MISSING",
            "the selected face cannot be resolved in the input snapshot",
        ),
        FaceFeatureInputError::TargetNotPlanar => (
            KernelErrorCode::Unsupported,
            "FACE_FEATURE_TARGET_NOT_PLANAR",
            "face extrusion requires a planar target face",
        ),
        FaceFeatureInputError::TargetNotAlignedToFrame => (
            KernelErrorCode::Unsupported,
            "FACE_FEATURE_TARGET_NOT_ALIGNED_TO_FRAME",
            "face extrusion requires a solid whose faces align with the sketch frame",
        ),
        FaceFeatureInputError::TargetDegenerate => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_TARGET_DEGENERATE",
            "the selected face is below the active minimum feature size",
        ),
        FaceFeatureInputError::FrameNotOnTarget => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_FRAME_OFF_TARGET",
            "the sketch frame is not on the selected face",
        ),
        FaceFeatureInputError::FrameNotOrthonormal => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_FRAME_NOT_ORTHONORMAL",
            "the sketch frame axes must be orthonormal",
        ),
        FaceFeatureInputError::FrameOffTargetPlane => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_FRAME_OFF_TARGET_PLANE",
            "the sketch frame must lie in the target face's plane and face outward",
        ),
        FaceFeatureInputError::TooFewVertices => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_TOO_FEW_VERTICES",
            "a face-extrusion profile requires at least three vertices",
        ),
        FaceFeatureInputError::TooManyVertices => (
            KernelErrorCode::ResourceLimitExceeded,
            "FACE_FEATURE_TOO_MANY_VERTICES",
            "a face-extrusion profile supports at most 256 vertices",
        ),
        FaceFeatureInputError::RepeatedVertex => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_REPEATED_VERTEX",
            "the face-extrusion profile contains a repeated vertex",
        ),
        FaceFeatureInputError::SelfIntersecting => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_PROFILE_SELF_INTERSECTING",
            "the face-extrusion profile must be simple and non-self-intersecting",
        ),
        FaceFeatureInputError::ProfileIndeterminate => (
            KernelErrorCode::NumericallyIndeterminate,
            "FACE_FEATURE_PROFILE_NUMERICALLY_INDETERMINATE",
            "the face-extrusion profile could not be certified by the active numerical filters",
        ),
        FaceFeatureInputError::ProfileOutsideFace => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_PROFILE_OUTSIDE_FACE",
            "the profile must lie strictly inside selected face material and outside its voids",
        ),
        FaceFeatureInputError::ProfileHoleInvalid => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_PROFILE_HOLE_INVALID",
            "profile holes must lie strictly inside their outer loop and remain pairwise disjoint",
        ),
        FaceFeatureInputError::NonPositiveDistance => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_DISTANCE_NON_POSITIVE",
            "face extrusion distance must be strictly positive",
        ),
        FaceFeatureInputError::FeatureTooSmall => (
            KernelErrorCode::InvalidInput,
            "FACE_FEATURE_TOO_SMALL",
            "the face extrusion is at or below the active minimum feature size",
        ),
        FaceFeatureInputError::CutTooDeep => (
            KernelErrorCode::Unsupported,
            "FACE_FEATURE_CUT_EXIT_UNRESOLVED",
            "the cut reaches a boundary that cannot yet be resolved as a certified exit",
        ),
        FaceFeatureInputError::SweepCollision => (
            KernelErrorCode::Unsupported,
            "FACE_FEATURE_SWEEP_COLLISION",
            "the added feature would contact or cross another source boundary",
        ),
        FaceFeatureInputError::CoordinateLimit => (
            KernelErrorCode::ResourceLimitExceeded,
            "FACE_FEATURE_COORDINATE_LIMIT",
            "the face extrusion exceeds the active coordinate envelope",
        ),
        FaceFeatureInputError::NumericallyUnrepresentable => (
            KernelErrorCode::NumericallyIndeterminate,
            "FACE_FEATURE_PRECISION_UNREPRESENTABLE",
            "face extrusion depth cannot be represented distinctly at this placement",
        ),
    };
    error(
        code,
        KernelStage::Preflight,
        snapshot,
        message,
        vec![simple_diagnostic(
            diagnostic,
            KernelStage::Preflight,
            message,
        )],
    )
}

fn loft_input_error(snapshot: SnapshotId, reason: loft::LoftInputError) -> KernelError {
    use loft::LoftInputError;
    match reason {
        LoftInputError::Profile(reason) => planar_profile_input_error(snapshot, reason),
        LoftInputError::OffsetNonFinite => planar_profile_error(
            snapshot,
            KernelErrorCode::InvalidInput,
            "LOFT_OFFSET_NON_FINITE",
            "the loft section offset must be a finite length",
        ),
        LoftInputError::OffsetInfeasible(loop_offset::LoopOffsetError::RadiusTooLarge) => {
            planar_profile_error(
                snapshot,
                KernelErrorCode::InvalidInput,
                "LOFT_SECTION_COLLAPSES",
                "the offset section collapses: an edge or an arc vanishes before the section is reached",
            )
        }
        LoftInputError::OffsetInfeasible(loop_offset::LoopOffsetError::SelfIntersects) => {
            planar_profile_error(
                snapshot,
                KernelErrorCode::InvalidInput,
                "LOFT_SECTION_SELF_INTERSECTS",
                "the offset section crosses itself: a neck of the profile closes up",
            )
        }
        LoftInputError::OffsetInfeasible(_) => planar_profile_error(
            snapshot,
            KernelErrorCode::NumericallyIndeterminate,
            "LOFT_SECTION_DEGENERATE",
            "the offset section could not be formed from this profile",
        ),
        LoftInputError::CornerNotTangent => planar_profile_error(
            snapshot,
            KernelErrorCode::Unsupported,
            "LOFT_CORNER_NOT_TANGENT",
            "a sharp corner of the profile involves an arc; its drafted walls would meet in a \
             conic, which is outside the line-and-circle vocabulary. Make the arc tangent to \
             its neighbours or replace it with straight edges.",
        ),
        LoftInputError::CoordinateLimit => planar_profile_error(
            snapshot,
            KernelErrorCode::InvalidInput,
            "LOFT_SECTION_COORDINATE_LIMIT",
            "the offset section leaves the certified coordinate range",
        ),
    }
}

fn planar_profile_input_error(
    snapshot: SnapshotId,
    reason: PlanarProfileInputError,
) -> KernelError {
    match reason {
        PlanarProfileInputError::Extrusion(reason) => extrusion_input_error(snapshot, reason),
        PlanarProfileInputError::FaceFeature(reason) => face_feature_input_error(snapshot, reason),
        PlanarProfileInputError::EmptyProfile => planar_profile_error(
            snapshot,
            KernelErrorCode::InvalidInput,
            "PLANAR_PROFILE_EMPTY",
            "a planar profile must contain at least one material region",
        ),
        PlanarProfileInputError::TooManyRegions => planar_profile_error(
            snapshot,
            KernelErrorCode::ResourceLimitExceeded,
            "PLANAR_PROFILE_TOO_MANY_REGIONS",
            "the planar profile exceeds the supported material-region limit",
        ),
        PlanarProfileInputError::TooManyLoops => planar_profile_error(
            snapshot,
            KernelErrorCode::ResourceLimitExceeded,
            "PLANAR_PROFILE_TOO_MANY_LOOPS",
            "the planar profile exceeds the supported boundary-loop limit",
        ),
        PlanarProfileInputError::TooManyCurves => planar_profile_error(
            snapshot,
            KernelErrorCode::ResourceLimitExceeded,
            "PLANAR_PROFILE_TOO_MANY_CURVES",
            "the planar profile exceeds the supported exact-curve limit",
        ),
        PlanarProfileInputError::EmptyLoop => planar_profile_error(
            snapshot,
            KernelErrorCode::InvalidInput,
            "PLANAR_PROFILE_LOOP_EMPTY",
            "every planar profile boundary must contain at least one exact curve",
        ),
        PlanarProfileInputError::DisconnectedLoop => planar_profile_error(
            snapshot,
            KernelErrorCode::InvalidInput,
            "PLANAR_PROFILE_LOOP_DISCONNECTED",
            "planar profile curve uses must form an exactly connected closed loop",
        ),
        PlanarProfileInputError::AnalyticCurve => planar_profile_error(
            snapshot,
            KernelErrorCode::Unsupported,
            "PLANAR_PROFILE_ANALYTIC_ROUTE_REQUIRED",
            "the profile contains analytic curves that require the native analytic extrusion path",
        ),
        PlanarProfileInputError::OverlappingRegions => planar_profile_error(
            snapshot,
            KernelErrorCode::InvalidInput,
            "PLANAR_PROFILE_REGIONS_OVERLAP",
            "planar profile material regions must be pairwise disjoint",
        ),
        PlanarProfileInputError::HoledFrameUnsupported => planar_profile_error(
            snapshot,
            KernelErrorCode::Unsupported,
            "PLANAR_PROFILE_HOLED_FRAME_UNSUPPORTED",
            "linear profile holes require a planar cap aligned with the extrusion frame",
        ),
    }
}

fn planar_profile_error(
    snapshot: SnapshotId,
    code: KernelErrorCode,
    diagnostic: &'static str,
    message: &'static str,
) -> KernelError {
    error(
        code,
        KernelStage::Preflight,
        snapshot,
        message,
        vec![simple_diagnostic(
            diagnostic,
            KernelStage::Preflight,
            message,
        )],
    )
}

fn revolve_input_error(snapshot: SnapshotId, reason: revolve::RevolveInputError) -> KernelError {
    let (diagnostic, message) = match reason {
        revolve::RevolveInputError::Profile(reason) => {
            return planar_profile_input_error(snapshot, reason);
        }
        revolve::RevolveInputError::SingleRegionOnly => (
            "REVOLVE_SINGLE_REGION_ONLY",
            "A revolve sweeps exactly one material region without holes; a hole would sweep a cavity of revolution, which needs a Boolean rather than a section chain.",
        ),
        revolve::RevolveInputError::DegenerateAxis => (
            "REVOLVE_AXIS_DEGENERATE",
            "The revolve axis endpoints coincide, so no axis is defined.",
        ),
        revolve::RevolveInputError::ProfileCrossesAxis => (
            "REVOLVE_PROFILE_CROSSES_AXIS",
            "The profile has material on both sides of the axis; the sweep would pass through itself.",
        ),
        revolve::RevolveInputError::ObliqueAxisContact => (
            "REVOLVE_OBLIQUE_AXIS_CONTACT",
            "A straight profile segment meets the axis obliquely. It would sweep a cone apex, which is a singular point rather than a pole, and stays outside the certified domain.",
        ),
        revolve::RevolveInputError::SectionNotContiguous => (
            "REVOLVE_SECTION_NOT_CONTIGUOUS",
            "The profile does not form one contiguous section: it must close on itself clear of the axis, or begin and end on the axis.",
        ),
    };
    error(
        KernelErrorCode::Unsupported,
        KernelStage::Preflight,
        snapshot,
        "the revolve profile and axis leave the certified domain",
        vec![simple_diagnostic(
            diagnostic,
            KernelStage::Preflight,
            message,
        )],
    )
}

fn face_push_pull_input_error(snapshot: SnapshotId, reason: FacePushPullInputError) -> KernelError {
    let (code, diagnostic, message) = match reason {
        FacePushPullInputError::NonFinite => (
            KernelErrorCode::InvalidInput,
            "FACE_PUSH_PULL_INPUT_NON_FINITE",
            "face push/pull distance must be finite",
        ),
        FacePushPullInputError::SourceUnsupported => (
            KernelErrorCode::Unsupported,
            "FACE_PUSH_PULL_SOURCE_UNSUPPORTED",
            "face push/pull requires one valid linear-faced solid",
        ),
        FacePushPullInputError::TargetSnapshotMismatch => (
            KernelErrorCode::StaleSnapshot,
            "FACE_PUSH_PULL_TARGET_STALE",
            "the selected push/pull face belongs to a different snapshot",
        ),
        FacePushPullInputError::TargetNotFace => (
            KernelErrorCode::InvalidInput,
            "FACE_PUSH_PULL_TARGET_NOT_FACE",
            "the push/pull target must be a face",
        ),
        FacePushPullInputError::TargetMissing => (
            KernelErrorCode::InvalidInput,
            "FACE_PUSH_PULL_TARGET_MISSING",
            "the selected push/pull face cannot be resolved in the input snapshot",
        ),
        FacePushPullInputError::TargetHasHoles => (
            KernelErrorCode::Unsupported,
            "FACE_PUSH_PULL_TARGET_HAS_HOLES",
            "whole-face push/pull currently requires an unholed face",
        ),
        FacePushPullInputError::TargetNotPlanar => (
            KernelErrorCode::Unsupported,
            "FACE_PUSH_PULL_TARGET_NOT_PLANAR",
            "whole-face push/pull requires a planar face",
        ),
        FacePushPullInputError::TargetNotExtrusionCap => (
            KernelErrorCode::Unsupported,
            "FACE_PUSH_PULL_TARGET_NOT_EXTRUSION_CAP",
            "the selected face is not a certified exterior extrusion cap",
        ),
        FacePushPullInputError::NonDistinctDistance => (
            KernelErrorCode::InvalidInput,
            "FACE_PUSH_PULL_DISTANCE_ZERO",
            "face push/pull distance must be non-zero",
        ),
        FacePushPullInputError::FeatureTooSmall => (
            KernelErrorCode::InvalidInput,
            "FACE_PUSH_PULL_TOO_SMALL",
            "face push/pull distance is at or below the active minimum feature size",
        ),
        FacePushPullInputError::SupportContact => (
            KernelErrorCode::Unsupported,
            "FACE_PUSH_PULL_SUPPORT_CONTACT",
            "the requested inward move would contact or cross the cap support plane",
        ),
        FacePushPullInputError::CoordinateLimit => (
            KernelErrorCode::ResourceLimitExceeded,
            "FACE_PUSH_PULL_COORDINATE_LIMIT",
            "face push/pull exceeds the active coordinate envelope",
        ),
        FacePushPullInputError::NumericallyUnrepresentable => (
            KernelErrorCode::NumericallyIndeterminate,
            "FACE_PUSH_PULL_PRECISION_UNREPRESENTABLE",
            "face push/pull distance cannot be represented distinctly at this placement",
        ),
    };
    error(
        code,
        KernelStage::Preflight,
        snapshot,
        message,
        vec![simple_diagnostic(
            diagnostic,
            KernelStage::Preflight,
            message,
        )],
    )
}

fn validate_transform_source(input: &Snapshot) -> Result<(), KernelError> {
    if !input.topology.solids.is_empty() {
        return Ok(());
    }
    Err(error(
        KernelErrorCode::Unsupported,
        KernelStage::Preflight,
        input.id,
        "the experimental transform capability requires at least one committed solid",
        vec![simple_diagnostic(
            "TRANSFORM_SOURCE_UNSUPPORTED",
            KernelStage::Preflight,
            "Whole-snapshot transforms require a non-empty solid snapshot.",
        )],
    ))
}

fn validate_transform_input(
    snapshot: SnapshotId,
    transform: artificer_protocol::SimilarityTransform3,
) -> Result<Similarity, KernelError> {
    Similarity::from_protocol(transform).map_err(|reason| {
        let (message, diagnostic) = match reason {
            TransformInputError::NonFinite => (
                "transform components must be finite",
                "TRANSFORM_INPUT_NON_FINITE",
            ),
            TransformInputError::NonPositiveScale => (
                "uniform scale must be strictly positive",
                "TRANSFORM_SCALE_NON_POSITIVE",
            ),
            TransformInputError::ZeroQuaternion => (
                "rotation quaternion must be non-zero and normalizable",
                "TRANSFORM_QUATERNION_INVALID",
            ),
        };
        error(
            KernelErrorCode::InvalidInput,
            KernelStage::Preflight,
            snapshot,
            message,
            vec![simple_diagnostic(
                diagnostic,
                KernelStage::Preflight,
                message,
            )],
        )
    })
}

fn validate_transform_candidate(
    snapshot: SnapshotId,
    input: &Topology,
    candidate: &Topology,
    transform: Similarity,
    precision: PrecisionPolicy,
) -> Result<(), KernelError> {
    let coordinate_limit = precision.max_abs_coordinate;
    let exact_bounds = validator::calculate_measures(candidate).bounds;
    let world_coordinates = candidate
        .vertices
        .iter()
        .flat_map(|vertex| {
            let point = vertex.value.point;
            [point.x, point.y, point.z]
        })
        .chain(candidate.edges.iter().flat_map(|edge| {
            edge.value
                .endpoints()
                .into_iter()
                .flat_map(|point| [point.x, point.y, point.z])
        }))
        .chain(candidate.faces.iter().flat_map(|face| {
            let point = match face.value.surface {
                Surface::Plane(plane) => plane.origin,
                Surface::Cylinder(cylinder) => cylinder.origin,
                Surface::Torus(torus) => torus.origin,
                Surface::Cone(cone) => cone.origin,
                Surface::Sphere(sphere) => sphere.origin,
            };
            [point.x, point.y, point.z]
        }))
        .chain(exact_bounds.into_iter().flat_map(|bounds| {
            [
                bounds.min.x,
                bounds.min.y,
                bounds.min.z,
                bounds.max.x,
                bounds.max.y,
                bounds.max.z,
            ]
        }));
    let parameter_coordinates = candidate.coedges.iter().flat_map(|coedge| {
        let endpoints = coedge
            .value
            .pcurve_endpoints()
            .into_iter()
            .flat_map(|point| [point.x, point.y])
            .collect::<Vec<_>>();
        let carrier = match coedge.value.pcurve {
            Curve2::Line { .. } => Vec::new(),
            Curve2::Harmonic {
                mean, amplitude, ..
            } => vec![mean.abs() + amplitude.abs()],
            Curve2::Circle {
                center,
                u,
                v,
                radius,
            } => vec![
                center.x.abs() + radius * u.x.hypot(v.x),
                center.y.abs() + radius * u.y.hypot(v.y),
            ],
        };
        endpoints.into_iter().chain(carrier)
    });
    if world_coordinates
        .chain(parameter_coordinates)
        .any(|value| !value.is_finite() || value.abs() > coordinate_limit)
    {
        return Err(error(
            KernelErrorCode::ResourceLimitExceeded,
            KernelStage::Construction,
            snapshot,
            "transformed geometry exceeds the active coordinate envelope",
            vec![simple_diagnostic(
                "TRANSFORM_COORDINATE_LIMIT_EXCEEDED",
                KernelStage::Construction,
                "A derived world or surface coordinate is non-finite or outside the configured limit.",
            )],
        ));
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let mut shortest = f64::INFINITY;
    for edge in &candidate.edges {
        let represented = match edge.value.curve {
            Curve3::Line { .. } => {
                let endpoints = edge.value.endpoints();
                endpoints[0].distance(endpoints[1])
            }
            Curve3::Circle { .. } | Curve3::Ellipse { .. } => edge.value.length(),
        };
        shortest = shortest.min(represented);
    }
    if !shortest.is_finite() || shortest <= minimum {
        let mut diagnostic = simple_diagnostic(
            "TRANSFORM_FEATURE_TOO_SMALL",
            KernelStage::Preflight,
            "The transformed solid would contain an edge at or below the supported feature size.",
        );
        attach_measurement(
            &mut diagnostic,
            QuantityKind::Length,
            shortest,
            NumericInterval {
                min: Some(minimum),
                max: None,
            },
        );
        return Err(error(
            KernelErrorCode::InvalidInput,
            KernelStage::Preflight,
            snapshot,
            "uniform scale would shrink a supported feature below the active minimum",
            vec![diagnostic],
        ));
    }

    // Translation at large magnitudes can destroy requested feature accuracy
    // even while all coordinates remain finite. Compare represented output
    // edge lengths to the mathematically expected scaled input lengths.
    let worst_length_error = input
        .edges
        .iter()
        .zip(&candidate.edges)
        .map(|(before, after)| {
            let expected = before.value.length() * transform.scale();
            let represented = match after.value.curve {
                Curve3::Line { .. } => {
                    let endpoints = after.value.endpoints();
                    endpoints[0].distance(endpoints[1])
                }
                Curve3::Circle { .. } | Curve3::Ellipse { .. } => after.value.length(),
            };
            (represented - expected).abs()
        })
        .fold(0.0_f64, f64::max);
    if !worst_length_error.is_finite() || worst_length_error > precision.linear_agreement {
        let mut diagnostic = simple_diagnostic(
            "TRANSFORM_PRECISION_UNREPRESENTABLE",
            KernelStage::Construction,
            "The requested placement cannot preserve edge lengths within the active agreement tolerance.",
        );
        attach_measurement(
            &mut diagnostic,
            QuantityKind::Length,
            worst_length_error,
            NumericInterval {
                min: None,
                max: Some(precision.linear_agreement),
            },
        );
        return Err(error(
            KernelErrorCode::NumericallyIndeterminate,
            KernelStage::Construction,
            snapshot,
            "transform cannot be represented within the requested numerical agreement",
            vec![diagnostic],
        ));
    }
    Ok(())
}

fn check_cancelled(
    snapshot: SnapshotId,
    cancellation: &CancellationToken,
    stage: KernelStage,
) -> Result<(), KernelError> {
    if !cancellation.is_cancelled() {
        return Ok(());
    }
    Err(error(
        KernelErrorCode::Cancelled,
        stage,
        snapshot,
        "operation cancelled; no snapshot was committed",
        vec![simple_diagnostic(
            "OPERATION_CANCELLED",
            stage,
            "The cancellation token was set before publication.",
        )],
    ))
}

fn protocol_validation(
    snapshot: SnapshotId,
    profile: ValidationProfile,
    report: &validator::ValidationReport,
) -> ProtocolValidationReport {
    let mut diagnostics = report
        .diagnostics
        .iter()
        .map(|diagnostic| validator_diagnostic(snapshot, diagnostic))
        .collect::<Vec<_>>();

    let profile_missing = match profile {
        ValidationProfile::Topology => false,
        ValidationProfile::ClosedShell => report.counts.shells == 0,
        ValidationProfile::Solid => report.counts.solids == 0,
    };
    if profile_missing {
        diagnostics.push(simple_diagnostic(
            "VALIDATION_PROFILE_NOT_SATISFIED",
            KernelStage::Validation,
            "The snapshot does not contain the topology required by the validation profile.",
        ));
    }
    let mut result = ProtocolValidationReport {
        profile,
        valid: diagnostics.is_empty(),
        diagnostics,
    };
    result.sort_diagnostics();
    result
}

fn validator_diagnostic(
    _snapshot: SnapshotId,
    diagnostic: &validator::Diagnostic,
) -> ProtocolDiagnostic {
    let measurement = diagnostic.measured.map(|measured| {
        let allowed = match diagnostic.code {
            validator::DiagnosticCode::FaceOrientationInvalid
            | validator::DiagnosticCode::SolidVolumeNonPositive => NumericInterval {
                min: diagnostic.allowed,
                max: None,
            },
            validator::DiagnosticCode::EdgeUseCount
            | validator::DiagnosticCode::CoedgeUseCount
            | validator::DiagnosticCode::LoopUseCount
            | validator::DiagnosticCode::FaceUseCount
            | validator::DiagnosticCode::ShellUseCount
            | validator::DiagnosticCode::ShellDisconnected
            | validator::DiagnosticCode::EulerCharacteristicInvalid => NumericInterval {
                min: diagnostic.allowed,
                max: diagnostic.allowed,
            },
            validator::DiagnosticCode::DanglingEntityReference
            | validator::DiagnosticCode::EdgeEndpointMismatch
            | validator::DiagnosticCode::CurveFrameInvalid
            | validator::DiagnosticCode::ParameterRangeInvalid
            | validator::DiagnosticCode::EntityNotFinite
            | validator::DiagnosticCode::FaceLoopIntersection
            | validator::DiagnosticCode::FaceHoleOutside
            | validator::DiagnosticCode::LoopNotClosed
            | validator::DiagnosticCode::LoopTooShort
            | validator::DiagnosticCode::PcurveEndpointMismatch
            | validator::DiagnosticCode::PcurveLocusMismatch
            | validator::DiagnosticCode::SurfaceFrameInvalid
            | validator::DiagnosticCode::EdgeUseOrientation => NumericInterval {
                min: None,
                max: diagnostic.allowed,
            },
        };
        let quantity = match diagnostic.code {
            validator::DiagnosticCode::EdgeEndpointMismatch
            | validator::DiagnosticCode::PcurveEndpointMismatch
            | validator::DiagnosticCode::PcurveLocusMismatch => QuantityKind::Length,
            _ => QuantityKind::Unitless,
        };
        (quantity, measured, allowed)
    });
    let mut result = ProtocolDiagnostic {
        code: ProtocolDiagnosticCode::new(diagnostic.code.as_str()),
        severity: DiagnosticSeverity::Error,
        stage: KernelStage::Validation,
        message: diagnostic.code.as_str().replace('_', " ").to_lowercase(),
        subjects: Vec::new(),
        path: diagnostic.path.split('/').map(str::to_owned).collect(),
        measurement: None,
        details: BTreeMap::new(),
    };
    if let Some((quantity, measured, allowed)) = measurement {
        attach_measurement(&mut result, quantity, measured, allowed);
    }
    result
}

fn attach_measurement(
    diagnostic: &mut ProtocolDiagnostic,
    quantity: QuantityKind,
    measured: f64,
    allowed: NumericInterval,
) {
    let allowed_is_finite =
        allowed.min.is_none_or(f64::is_finite) && allowed.max.is_none_or(f64::is_finite);
    if measured.is_finite() && allowed_is_finite {
        diagnostic.measurement = Some(DiagnosticMeasurement {
            quantity,
            measured,
            allowed,
        });
    } else {
        diagnostic.details.insert(
            "measurement_status".to_owned(),
            "omitted_non_finite".to_owned(),
        );
        diagnostic.details.insert(
            "measured_class".to_owned(),
            float_class(measured).to_owned(),
        );
    }
}

fn float_class(value: f64) -> &'static str {
    if value.is_nan() {
        "nan"
    } else if value == f64::INFINITY {
        "positive_infinity"
    } else if value == f64::NEG_INFINITY {
        "negative_infinity"
    } else {
        "finite"
    }
}

fn simple_diagnostic(code: &str, stage: KernelStage, message: &str) -> ProtocolDiagnostic {
    ProtocolDiagnostic {
        code: ProtocolDiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        stage,
        message: message.to_owned(),
        subjects: Vec::new(),
        path: Vec::new(),
        measurement: None,
        details: BTreeMap::new(),
    }
}

/// A caveat attached to a published result rather than a refusal of it.
///
/// Everything this kernel publishes means "certified" unless it says otherwise,
/// so the one path that publishes an approximation has to say otherwise.
fn approximation_warning(code: &str, message: &str) -> ProtocolDiagnostic {
    ProtocolDiagnostic {
        code: ProtocolDiagnosticCode::new(code),
        severity: DiagnosticSeverity::Warning,
        stage: KernelStage::Construction,
        message: message.to_owned(),
        subjects: Vec::new(),
        path: Vec::new(),
        measurement: None,
        details: BTreeMap::new(),
    }
}

fn error(
    code: KernelErrorCode,
    stage: KernelStage,
    input_snapshot: SnapshotId,
    message: impl Into<String>,
    diagnostics: Vec<ProtocolDiagnostic>,
) -> KernelError {
    KernelError {
        code,
        stage,
        input_snapshot,
        message: message.into(),
        diagnostics,
        details: BTreeMap::new(),
    }
}

fn generated_history(snapshot: &Snapshot) -> Vec<HistoryRecord> {
    let mut history = Vec::with_capacity(snapshot.counts().total() as usize);
    let mut add = |kind: EntityKind, id: u64, ordinal: usize| {
        history.push(HistoryRecord {
            relation: HistoryRelation::Generated,
            inputs: Vec::new(),
            outputs: vec![entity_ref(snapshot.id, id, kind)],
            role: Some(OperationRole::new(kind.to_string(), Some(ordinal as u32))),
        });
    };
    for (ordinal, record) in snapshot.topology.vertices.iter().enumerate() {
        add(EntityKind::Vertex, record.id.get(), ordinal);
    }
    for (ordinal, record) in snapshot.topology.edges.iter().enumerate() {
        add(EntityKind::Edge, record.id.get(), ordinal);
    }
    for (ordinal, record) in snapshot.topology.coedges.iter().enumerate() {
        add(EntityKind::Coedge, record.id.get(), ordinal);
    }
    for (ordinal, record) in snapshot.topology.loops.iter().enumerate() {
        add(EntityKind::Loop, record.id.get(), ordinal);
    }
    for (ordinal, record) in snapshot.topology.faces.iter().enumerate() {
        add(EntityKind::Face, record.id.get(), ordinal);
    }
    for (ordinal, record) in snapshot.topology.shells.iter().enumerate() {
        add(EntityKind::Shell, record.id.get(), ordinal);
    }
    for (ordinal, record) in snapshot.topology.solids.iter().enumerate() {
        add(EntityKind::Solid, record.id.get(), ordinal);
    }
    history
}

fn regularized_face_feature_history(input: &Snapshot, output: &Snapshot) -> Vec<HistoryRecord> {
    let mut history = generated_history(output);
    let mut deleted = |kind: EntityKind, id: u64, ordinal: usize| {
        history.push(HistoryRecord {
            relation: HistoryRelation::Deleted,
            inputs: vec![entity_ref(input.id, id, kind)],
            outputs: Vec::new(),
            role: Some(OperationRole::new(
                "face_extrude.regularized_source",
                Some(ordinal as u32),
            )),
        });
    };
    for (ordinal, record) in input.topology.vertices.iter().enumerate() {
        deleted(EntityKind::Vertex, record.id.get(), ordinal);
    }
    for (ordinal, record) in input.topology.edges.iter().enumerate() {
        deleted(EntityKind::Edge, record.id.get(), ordinal);
    }
    for (ordinal, record) in input.topology.coedges.iter().enumerate() {
        deleted(EntityKind::Coedge, record.id.get(), ordinal);
    }
    for (ordinal, record) in input.topology.loops.iter().enumerate() {
        deleted(EntityKind::Loop, record.id.get(), ordinal);
    }
    for (ordinal, record) in input.topology.faces.iter().enumerate() {
        deleted(EntityKind::Face, record.id.get(), ordinal);
    }
    for (ordinal, record) in input.topology.shells.iter().enumerate() {
        deleted(EntityKind::Shell, record.id.get(), ordinal);
    }
    for (ordinal, record) in input.topology.solids.iter().enumerate() {
        deleted(EntityKind::Solid, record.id.get(), ordinal);
    }
    history
}

fn boolean_tool_deleted_history(tool: &Snapshot) -> Vec<HistoryRecord> {
    let mut history = Vec::new();
    let mut deleted = |kind: EntityKind, id: u64, ordinal: usize| {
        history.push(HistoryRecord {
            relation: HistoryRelation::Deleted,
            inputs: vec![entity_ref(tool.id, id, kind)],
            outputs: Vec::new(),
            role: Some(OperationRole::new(
                "boolean.tool_operand",
                Some(ordinal as u32),
            )),
        });
    };
    for (ordinal, record) in tool.topology.vertices.iter().enumerate() {
        deleted(EntityKind::Vertex, record.id.get(), ordinal);
    }
    for (ordinal, record) in tool.topology.edges.iter().enumerate() {
        deleted(EntityKind::Edge, record.id.get(), ordinal);
    }
    for (ordinal, record) in tool.topology.coedges.iter().enumerate() {
        deleted(EntityKind::Coedge, record.id.get(), ordinal);
    }
    for (ordinal, record) in tool.topology.loops.iter().enumerate() {
        deleted(EntityKind::Loop, record.id.get(), ordinal);
    }
    for (ordinal, record) in tool.topology.faces.iter().enumerate() {
        deleted(EntityKind::Face, record.id.get(), ordinal);
    }
    for (ordinal, record) in tool.topology.shells.iter().enumerate() {
        deleted(EntityKind::Shell, record.id.get(), ordinal);
    }
    for (ordinal, record) in tool.topology.solids.iter().enumerate() {
        deleted(EntityKind::Solid, record.id.get(), ordinal);
    }
    history
}

fn extrusion_history(snapshot: &Snapshot, profile_vertices: usize) -> Vec<HistoryRecord> {
    let mut history = Vec::with_capacity(snapshot.counts().total() as usize);
    let mut add = |kind: EntityKind, id: u64, role: &'static str, ordinal: Option<usize>| {
        history.push(HistoryRecord {
            relation: HistoryRelation::Generated,
            inputs: Vec::new(),
            outputs: vec![entity_ref(snapshot.id, id, kind)],
            role: Some(OperationRole::new(
                role,
                ordinal.map(|ordinal| ordinal as u32),
            )),
        });
    };

    for (index, record) in snapshot.topology.vertices.iter().enumerate() {
        if index < profile_vertices {
            add(
                EntityKind::Vertex,
                record.id.get(),
                "extrude.bottom_vertex",
                Some(index),
            );
        } else {
            add(
                EntityKind::Vertex,
                record.id.get(),
                "extrude.top_vertex",
                Some(index - profile_vertices),
            );
        }
    }
    for (index, record) in snapshot.topology.edges.iter().enumerate() {
        let (role, ordinal) = if index < profile_vertices {
            ("extrude.bottom_edge", index)
        } else if index < profile_vertices * 2 {
            ("extrude.top_edge", index - profile_vertices)
        } else {
            ("extrude.side_edge", index - profile_vertices * 2)
        };
        add(EntityKind::Edge, record.id.get(), role, Some(ordinal));
    }
    for (index, record) in snapshot.topology.coedges.iter().enumerate() {
        add(
            EntityKind::Coedge,
            record.id.get(),
            "extrude.coedge",
            Some(index),
        );
    }
    for (index, record) in snapshot.topology.loops.iter().enumerate() {
        let (role, ordinal) = match index {
            0 => ("extrude.bottom_loop", None),
            1 => ("extrude.top_loop", None),
            side => ("extrude.side_loop", Some(side - 2)),
        };
        add(EntityKind::Loop, record.id.get(), role, ordinal);
    }
    for (index, record) in snapshot.topology.faces.iter().enumerate() {
        let (role, ordinal) = match index {
            0 => ("extrude.bottom_face", None),
            1 => ("extrude.top_face", None),
            side => ("extrude.side_face", Some(side - 2)),
        };
        add(EntityKind::Face, record.id.get(), role, ordinal);
    }
    for record in &snapshot.topology.shells {
        add(EntityKind::Shell, record.id.get(), "extrude.shell", None);
    }
    for record in &snapshot.topology.solids {
        add(EntityKind::Solid, record.id.get(), "extrude.solid", None);
    }
    history
}

fn face_feature_history(
    input: &Snapshot,
    output: &Snapshot,
    target_face: EntityRef,
    exit_face: Option<EntityRef>,
    operation: FaceExtrusionOperation,
) -> Result<Vec<HistoryRecord>, KernelError> {
    let target_record = input
        .topology
        .faces
        .iter()
        .find(|record| record.id.get() == target_face.entity.0)
        .ok_or_else(|| {
            face_feature_history_error(
                input.id,
                "validated target face is missing from history input",
            )
        })?;
    let exit_record = exit_face
        .map(|exit| {
            input
                .topology
                .faces
                .iter()
                .find(|record| record.id.get() == exit.entity.0)
                .ok_or_else(|| {
                    face_feature_history_error(
                        input.id,
                        "validated through-cut exit face is missing from history input",
                    )
                })
        })
        .transpose()?;
    let mut history = Vec::with_capacity(output.counts().total() as usize);
    let mut covered_inputs = BTreeSet::new();
    let mut covered_outputs = BTreeSet::new();

    // The scaffold copies all eight source corners exactly.
    for (ordinal, input_vertex) in input.topology.vertices.iter().enumerate() {
        let output_vertex = output
            .topology
            .vertices
            .iter()
            .find(|candidate| {
                candidate.value.point == input_vertex.value.point
                    && !covered_outputs.contains(&entity_ref(
                        output.id,
                        candidate.id.get(),
                        EntityKind::Vertex,
                    ))
            })
            .ok_or_else(|| {
                face_feature_history_error(
                    input.id,
                    "a source corner did not survive the face feature",
                )
            })?;
        push_face_feature_history(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            HistoryRelation::Unchanged,
            vec![entity_ref(
                input.id,
                input_vertex.id.get(),
                EntityKind::Vertex,
            )],
            entity_ref(output.id, output_vertex.id.get(), EntityKind::Vertex),
            "face_extrude.preserved_vertex",
            Some(ordinal as u32),
        )?;
    }

    // Strictly inset v0 profiles do not split any of the twelve source edges.
    for (ordinal, input_edge) in input.topology.edges.iter().enumerate() {
        let output_edge = output
            .topology
            .edges
            .iter()
            .find(|candidate| {
                undirected_segments_equal(input_edge.value.endpoints(), candidate.value.endpoints())
                    && !covered_outputs.contains(&entity_ref(
                        output.id,
                        candidate.id.get(),
                        EntityKind::Edge,
                    ))
            })
            .ok_or_else(|| {
                face_feature_history_error(
                    input.id,
                    "a source edge did not survive the face feature",
                )
            })?;
        push_face_feature_history(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            HistoryRelation::Unchanged,
            vec![entity_ref(input.id, input_edge.id.get(), EntityKind::Edge)],
            entity_ref(output.id, output_edge.id.get(), EntityKind::Edge),
            "face_extrude.preserved_edge",
            Some(ordinal as u32),
        )?;
    }

    // Each non-target face remains one exact patch. Its loop and oriented edge
    // uses survive even if the rebuilt face chooses a different local p-curve origin.
    for (ordinal, input_face) in input.topology.faces.iter().enumerate() {
        if input_face.id == target_record.id
            || exit_record.is_some_and(|exit| input_face.id == exit.id)
        {
            continue;
        }
        let input_polygon = validator::face_polygon(&input.topology, input_face.value.outer_loop)
            .ok_or_else(|| {
            face_feature_history_error(input.id, "a source face boundary is unavailable")
        })?;
        let (_, output_face) = output
            .topology
            .faces
            .iter()
            .enumerate()
            .find(|(_, candidate)| {
                candidate.value.role == input_face.value.role
                    && validator::face_polygon(&output.topology, candidate.value.outer_loop)
                        .is_some_and(|polygon| oriented_polygons_equal(&input_polygon, &polygon))
                    && !covered_outputs.contains(&entity_ref(
                        output.id,
                        candidate.id.get(),
                        EntityKind::Face,
                    ))
            })
            .ok_or_else(|| {
                face_feature_history_error(input.id, "a non-target source face did not survive")
            })?;
        push_face_feature_history(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            HistoryRelation::Unchanged,
            vec![entity_ref(input.id, input_face.id.get(), EntityKind::Face)],
            entity_ref(output.id, output_face.id.get(), EntityKind::Face),
            "face_extrude.preserved_face",
            Some(ordinal as u32),
        )?;

        for (loop_ordinal, input_loop_key) in input_face.value.loops().enumerate() {
            let input_loop = input.topology.loop_record(input_loop_key).ok_or_else(|| {
                face_feature_history_error(input.id, "a source loop is unavailable")
            })?;
            let input_loop_polygon = validator::face_polygon(&input.topology, input_loop_key)
                .ok_or_else(|| {
                    face_feature_history_error(input.id, "a source loop boundary is unavailable")
                })?;
            let output_loop_key = output_face
                .value
                .loops()
                .find(|candidate| {
                    validator::face_polygon(&output.topology, *candidate).is_some_and(|polygon| {
                        oriented_polygons_equal(&input_loop_polygon, &polygon)
                    })
                })
                .ok_or_else(|| {
                    face_feature_history_error(input.id, "a preserved loop is unavailable")
                })?;
            let output_loop = output
                .topology
                .loop_record(output_loop_key)
                .ok_or_else(|| {
                    face_feature_history_error(input.id, "a preserved loop is unavailable")
                })?;
            push_face_feature_history(
                &mut history,
                &mut covered_inputs,
                &mut covered_outputs,
                input.id,
                HistoryRelation::Unchanged,
                vec![entity_ref(input.id, input_loop.id.get(), EntityKind::Loop)],
                entity_ref(output.id, output_loop.id.get(), EntityKind::Loop),
                "face_extrude.preserved_loop",
                Some((ordinal * 257 + loop_ordinal) as u32),
            )?;
            map_face_coedges(
                input,
                output,
                &input_loop.value.coedges,
                &output_loop.value.coedges,
                HistoryRelation::Unchanged,
                "face_extrude.preserved_coedge",
                &mut history,
                &mut covered_inputs,
                &mut covered_outputs,
            )?;
        }
    }

    map_modified_face_patches(
        input,
        output,
        target_record,
        "face_extrude.target_face_patch",
        "face_extrude.target_loop_patch",
        "face_extrude.target_boundary_coedge",
        &mut history,
        &mut covered_inputs,
        &mut covered_outputs,
    )?;
    if let Some(exit_record) = exit_record {
        map_modified_face_patches(
            input,
            output,
            exit_record,
            "face_extrude.exit_face_patch",
            "face_extrude.exit_loop_patch",
            "face_extrude.exit_boundary_coedge",
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
        )?;
    }

    if input.topology.shells.len() > output.topology.shells.len()
        || input.topology.solids.len() > output.topology.solids.len()
    {
        return Err(face_feature_history_error(
            input.id,
            "the face feature unexpectedly removed a source shell or solid",
        ));
    }
    let target_face_index = input
        .topology
        .faces
        .iter()
        .position(|face| face.id == target_record.id)
        .expect("validated target face remains in history input");
    let target_shell_index = input
        .topology
        .shells
        .iter()
        .position(|shell| {
            shell
                .value
                .faces
                .contains(&topology::FaceKey(target_face_index))
        })
        .ok_or_else(|| {
            face_feature_history_error(input.id, "target face has no owning source shell")
        })?;
    let target_solid_index = input
        .topology
        .solids
        .iter()
        .position(|solid| {
            solid
                .value
                .shells()
                .any(|shell| shell == topology::ShellKey(target_shell_index))
        })
        .ok_or_else(|| {
            face_feature_history_error(input.id, "target shell has no owning source solid")
        })?;
    for (ordinal, (source, result)) in input
        .topology
        .shells
        .iter()
        .zip(&output.topology.shells)
        .enumerate()
    {
        let mut outputs = vec![entity_ref(output.id, result.id.get(), EntityKind::Shell)];
        if ordinal == target_shell_index {
            outputs.extend(
                output
                    .topology
                    .shells
                    .iter()
                    .skip(input.topology.shells.len())
                    .map(|shell| entity_ref(output.id, shell.id.get(), EntityKind::Shell)),
            );
        }
        push_face_feature_history_outputs(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            HistoryRelation::Modified,
            vec![entity_ref(input.id, source.id.get(), EntityKind::Shell)],
            outputs,
            "face_extrude.shell",
            Some(ordinal as u32),
        )?;
    }
    for (ordinal, (source, result)) in input
        .topology
        .solids
        .iter()
        .zip(&output.topology.solids)
        .enumerate()
    {
        let mut outputs = vec![entity_ref(output.id, result.id.get(), EntityKind::Solid)];
        if ordinal == target_solid_index {
            outputs.extend(
                output
                    .topology
                    .solids
                    .iter()
                    .skip(input.topology.solids.len())
                    .map(|solid| entity_ref(output.id, solid.id.get(), EntityKind::Solid)),
            );
        }
        push_face_feature_history_outputs(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            HistoryRelation::Modified,
            vec![entity_ref(input.id, source.id.get(), EntityKind::Solid)],
            outputs,
            "face_extrude.solid",
            Some(ordinal as u32),
        )?;
    }

    // Everything still uncovered is genuinely introduced by the feature.
    for (ordinal, record) in output.topology.vertices.iter().enumerate() {
        push_generated_face_feature_entity(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            entity_ref(output.id, record.id.get(), EntityKind::Vertex),
            "face_extrude.feature_vertex",
            Some(ordinal as u32),
        )?;
    }
    for (ordinal, record) in output.topology.edges.iter().enumerate() {
        push_generated_face_feature_entity(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            entity_ref(output.id, record.id.get(), EntityKind::Edge),
            "face_extrude.feature_edge",
            Some(ordinal as u32),
        )?;
    }
    for (ordinal, record) in output.topology.coedges.iter().enumerate() {
        push_generated_face_feature_entity(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            entity_ref(output.id, record.id.get(), EntityKind::Coedge),
            "face_extrude.feature_coedge",
            Some(ordinal as u32),
        )?;
    }
    for (ordinal, record) in output.topology.loops.iter().enumerate() {
        push_generated_face_feature_entity(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            entity_ref(output.id, record.id.get(), EntityKind::Loop),
            "face_extrude.feature_loop",
            Some(ordinal as u32),
        )?;
    }
    for (face_ordinal, record) in output.topology.faces.iter().enumerate() {
        let output_ref = entity_ref(output.id, record.id.get(), EntityKind::Face);
        if covered_outputs.contains(&output_ref) {
            continue;
        }
        let (role, ordinal) = match (operation, record.value.role) {
            (FaceExtrusionOperation::Add, FaceRole::FeatureEnd) => {
                ("face_extrude.boss.end_face", None)
            }
            (FaceExtrusionOperation::Cut, FaceRole::FeatureEnd) => {
                ("face_extrude.pocket.floor_face", None)
            }
            (FaceExtrusionOperation::Add, FaceRole::FeatureSide(side)) => {
                ("face_extrude.boss.side_face", Some(side))
            }
            (FaceExtrusionOperation::Cut, FaceRole::FeatureSide(side)) => {
                ("face_extrude.pocket.wall_face", Some(side))
            }
            (_, role) if role == target_record.value.role => (
                "face_extrude.profile_hole.support_face",
                Some(face_ordinal as u32),
            ),
            (_, role) if exit_record.is_some_and(|exit| role == exit.value.role) => (
                "face_extrude.profile_hole.exit_face",
                Some(face_ordinal as u32),
            ),
            (_, _) => {
                return Err(face_feature_history_error(
                    input.id,
                    "an unmatched base face would be misclassified as generated",
                ));
            }
        };
        push_generated_face_feature_entity(
            &mut history,
            &mut covered_inputs,
            &mut covered_outputs,
            input.id,
            output_ref,
            role,
            ordinal,
        )?;
    }

    if covered_outputs.len() != output.counts().total() as usize {
        return Err(face_feature_history_error(
            input.id,
            "face-feature history does not cover every output exactly once",
        ));
    }
    if covered_inputs.len() != input.counts().total() as usize {
        return Err(face_feature_history_error(
            input.id,
            "face-feature history does not cover every input entity",
        ));
    }
    Ok(history)
}

#[allow(clippy::too_many_arguments)]
fn map_modified_face_patches(
    input: &Snapshot,
    output: &Snapshot,
    source_face: &topology::Record<topology::Face>,
    face_role: &'static str,
    loop_role: &'static str,
    coedge_role: &'static str,
    history: &mut Vec<HistoryRecord>,
    covered_inputs: &mut BTreeSet<EntityRef>,
    covered_outputs: &mut BTreeSet<EntityRef>,
) -> Result<(), KernelError> {
    let source_loop = input
        .topology
        .loop_record(source_face.value.outer_loop)
        .ok_or_else(|| {
            face_feature_history_error(input.id, "a modified source loop is unavailable")
        })?;
    let boundary_segments = source_loop
        .value
        .coedges
        .iter()
        .copied()
        .map(|coedge| oriented_coedge_points(&input.topology, coedge))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            face_feature_history_error(input.id, "a modified face boundary is unavailable")
        })?;
    let tolerance = input.precision.unwrap_or_default().linear_agreement;
    let source_plane = source_face.value.surface.as_plane().ok_or_else(|| {
        face_feature_history_error(input.id, "a modified source face is non-planar")
    })?;
    let patch_indices = output
        .topology
        .faces
        .iter()
        .enumerate()
        .filter_map(|(index, face)| {
            validator::face_polygon(&output.topology, face.value.outer_loop)
                .filter(|polygon| polygon_on_plane(polygon, source_plane, tolerance))
                .filter(|polygon| {
                    (0..polygon.len()).any(|edge| {
                        let segment = [polygon[edge], polygon[(edge + 1) % polygon.len()]];
                        boundary_segments
                            .iter()
                            .any(|source| undirected_segments_equal(*source, segment))
                    })
                })
                .map(|_| index)
        })
        .collect::<Vec<_>>();
    if patch_indices.is_empty() {
        return Err(face_feature_history_error(
            input.id,
            "a modified face did not produce boundary-preserving scaffold patches",
        ));
    }

    let input_face = entity_ref(input.id, source_face.id.get(), EntityKind::Face);
    let input_loop = entity_ref(input.id, source_loop.id.get(), EntityKind::Loop);
    let mut output_coedges = Vec::new();
    for (patch_ordinal, output_face_index) in patch_indices.iter().copied().enumerate() {
        let output_face = &output.topology.faces[output_face_index];
        let output_loop = output
            .topology
            .loop_record(output_face.value.outer_loop)
            .ok_or_else(|| {
                face_feature_history_error(input.id, "a modified patch loop is unavailable")
            })?;
        push_face_feature_history(
            history,
            covered_inputs,
            covered_outputs,
            input.id,
            HistoryRelation::Modified,
            vec![input_face],
            entity_ref(output.id, output_face.id.get(), EntityKind::Face),
            face_role,
            Some(patch_ordinal as u32),
        )?;
        push_face_feature_history(
            history,
            covered_inputs,
            covered_outputs,
            input.id,
            HistoryRelation::Modified,
            vec![input_loop],
            entity_ref(output.id, output_loop.id.get(), EntityKind::Loop),
            loop_role,
            Some(patch_ordinal as u32),
        )?;
        output_coedges.extend(output_loop.value.coedges.iter().copied());
    }
    map_face_coedges(
        input,
        output,
        &source_loop.value.coedges,
        &output_coedges,
        HistoryRelation::Modified,
        coedge_role,
        history,
        covered_inputs,
        covered_outputs,
    )?;

    for (inner_ordinal, input_loop_key) in source_face.value.inner_loops.iter().copied().enumerate()
    {
        let input_loop = input.topology.loop_record(input_loop_key).ok_or_else(|| {
            face_feature_history_error(input.id, "a modified inner loop is unavailable")
        })?;
        let input_polygon =
            validator::face_polygon(&input.topology, input_loop_key).ok_or_else(|| {
                face_feature_history_error(
                    input.id,
                    "a modified inner-loop boundary is unavailable",
                )
            })?;
        let output_loop_key = output
            .topology
            .faces
            .iter()
            .filter(|face| {
                face.value.surface.as_plane().is_some_and(|plane| {
                    (plane.origin - source_plane.origin)
                        .dot(source_plane.normal)
                        .abs()
                        <= tolerance
                        && plane.normal.dot(source_plane.normal) > 0.0
                })
            })
            .flat_map(|face| face.value.inner_loops.iter().copied())
            .find(|candidate| {
                validator::face_polygon(&output.topology, *candidate)
                    .is_some_and(|polygon| oriented_polygons_equal(&input_polygon, &polygon))
            })
            .ok_or_else(|| {
                face_feature_history_error(input.id, "a modified face lost an existing inner loop")
            })?;
        let output_loop = output
            .topology
            .loop_record(output_loop_key)
            .ok_or_else(|| {
                face_feature_history_error(input.id, "a mapped inner loop is unavailable")
            })?;
        push_face_feature_history(
            history,
            covered_inputs,
            covered_outputs,
            input.id,
            HistoryRelation::Unchanged,
            vec![entity_ref(input.id, input_loop.id.get(), EntityKind::Loop)],
            entity_ref(output.id, output_loop.id.get(), EntityKind::Loop),
            "face_extrude.preserved_inner_loop",
            Some(inner_ordinal as u32),
        )?;
        map_face_coedges(
            input,
            output,
            &input_loop.value.coedges,
            &output_loop.value.coedges,
            HistoryRelation::Unchanged,
            "face_extrude.preserved_inner_coedge",
            history,
            covered_inputs,
            covered_outputs,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn map_face_coedges(
    input: &Snapshot,
    output: &Snapshot,
    input_keys: &[topology::CoedgeKey],
    output_keys: &[topology::CoedgeKey],
    relation: HistoryRelation,
    role: &'static str,
    history: &mut Vec<HistoryRecord>,
    covered_inputs: &mut BTreeSet<EntityRef>,
    covered_outputs: &mut BTreeSet<EntityRef>,
) -> Result<(), KernelError> {
    for (ordinal, input_key) in input_keys.iter().copied().enumerate() {
        let input_record = input.topology.coedge(input_key).ok_or_else(|| {
            face_feature_history_error(input.id, "a source coedge is unavailable")
        })?;
        let input_points = oriented_coedge_points(&input.topology, input_key).ok_or_else(|| {
            face_feature_history_error(input.id, "a source coedge has no oriented edge")
        })?;
        let output_key = output_keys
            .iter()
            .copied()
            .find(|candidate| {
                let Some(record) = output.topology.coedge(*candidate) else {
                    return false;
                };
                let output_ref = entity_ref(output.id, record.id.get(), EntityKind::Coedge);
                !covered_outputs.contains(&output_ref)
                    && oriented_coedge_points(&output.topology, *candidate) == Some(input_points)
            })
            .ok_or_else(|| {
                face_feature_history_error(input.id, "an oriented source edge use did not survive")
            })?;
        let output_record = output.topology.coedge(output_key).ok_or_else(|| {
            face_feature_history_error(input.id, "a mapped output coedge is unavailable")
        })?;
        push_face_feature_history(
            history,
            covered_inputs,
            covered_outputs,
            input.id,
            relation,
            vec![entity_ref(
                input.id,
                input_record.id.get(),
                EntityKind::Coedge,
            )],
            entity_ref(output.id, output_record.id.get(), EntityKind::Coedge),
            role,
            Some(ordinal as u32),
        )?;
    }
    Ok(())
}

fn oriented_coedge_points(topology: &Topology, key: topology::CoedgeKey) -> Option<[Point3; 2]> {
    let coedge = topology.coedge(key)?;
    topology
        .oriented_edge_vertices(&coedge.value)
        .map(|(_, points)| points)
}

const fn undirected_segments_equal(left: [Point3; 2], right: [Point3; 2]) -> bool {
    (left[0].x == right[0].x
        && left[0].y == right[0].y
        && left[0].z == right[0].z
        && left[1].x == right[1].x
        && left[1].y == right[1].y
        && left[1].z == right[1].z)
        || (left[0].x == right[1].x
            && left[0].y == right[1].y
            && left[0].z == right[1].z
            && left[1].x == right[0].x
            && left[1].y == right[0].y
            && left[1].z == right[0].z)
}

fn oriented_polygons_equal(left: &[Point3], right: &[Point3]) -> bool {
    left.len() == right.len()
        && !left.is_empty()
        && (0..right.len()).any(|offset| {
            left.iter()
                .enumerate()
                .all(|(index, point)| *point == right[(index + offset) % right.len()])
        })
}

fn polygon_on_plane(points: &[Point3], plane: topology::Plane, tolerance: f64) -> bool {
    let normal_length = plane.normal.length();
    normal_length.is_finite()
        && normal_length > f64::EPSILON
        && points.iter().all(|point| {
            ((*point - plane.origin).dot(plane.normal) / normal_length).abs() <= tolerance
        })
}

#[allow(clippy::too_many_arguments)]
fn push_face_feature_history(
    history: &mut Vec<HistoryRecord>,
    covered_inputs: &mut BTreeSet<EntityRef>,
    covered_outputs: &mut BTreeSet<EntityRef>,
    input_snapshot: SnapshotId,
    relation: HistoryRelation,
    inputs: Vec<EntityRef>,
    output: EntityRef,
    role: &'static str,
    ordinal: Option<u32>,
) -> Result<(), KernelError> {
    push_face_feature_history_outputs(
        history,
        covered_inputs,
        covered_outputs,
        input_snapshot,
        relation,
        inputs,
        vec![output],
        role,
        ordinal,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_face_feature_history_outputs(
    history: &mut Vec<HistoryRecord>,
    covered_inputs: &mut BTreeSet<EntityRef>,
    covered_outputs: &mut BTreeSet<EntityRef>,
    input_snapshot: SnapshotId,
    relation: HistoryRelation,
    inputs: Vec<EntityRef>,
    outputs: Vec<EntityRef>,
    role: &'static str,
    ordinal: Option<u32>,
) -> Result<(), KernelError> {
    if outputs.is_empty()
        || outputs
            .iter()
            .any(|output| !covered_outputs.insert(*output))
    {
        return Err(face_feature_history_error(
            input_snapshot,
            "one face-feature output received more than one history record",
        ));
    }
    covered_inputs.extend(inputs.iter().copied());
    history.push(HistoryRecord {
        relation,
        inputs,
        outputs,
        role: Some(OperationRole::new(role, ordinal)),
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_generated_face_feature_entity(
    history: &mut Vec<HistoryRecord>,
    covered_inputs: &mut BTreeSet<EntityRef>,
    covered_outputs: &mut BTreeSet<EntityRef>,
    input_snapshot: SnapshotId,
    output: EntityRef,
    role: &'static str,
    ordinal: Option<u32>,
) -> Result<(), KernelError> {
    if covered_outputs.contains(&output) {
        return Ok(());
    }
    push_face_feature_history(
        history,
        covered_inputs,
        covered_outputs,
        input_snapshot,
        HistoryRelation::Generated,
        Vec::new(),
        output,
        role,
        ordinal,
    )
}

fn face_feature_history_error(snapshot: SnapshotId, message: &str) -> KernelError {
    error(
        KernelErrorCode::InternalFailure,
        KernelStage::Commit,
        snapshot,
        message,
        vec![simple_diagnostic(
            "FACE_FEATURE_HISTORY_INCOMPLETE",
            KernelStage::Commit,
            message,
        )],
    )
}

fn transformed_history(input: &Snapshot, output: &Snapshot) -> Vec<HistoryRecord> {
    let relation = if input.semantic_digest == output.semantic_digest {
        HistoryRelation::Unchanged
    } else {
        HistoryRelation::Modified
    };
    let mut history = Vec::with_capacity(output.counts().total() as usize);
    let mut add = |kind: EntityKind, input_id: u64, output_id: u64| {
        history.push(HistoryRecord {
            relation,
            inputs: vec![entity_ref(input.id, input_id, kind)],
            outputs: vec![entity_ref(output.id, output_id, kind)],
            role: None,
        });
    };
    for (before, after) in input
        .topology
        .vertices
        .iter()
        .zip(&output.topology.vertices)
    {
        add(EntityKind::Vertex, before.id.get(), after.id.get());
    }
    for (before, after) in input.topology.edges.iter().zip(&output.topology.edges) {
        add(EntityKind::Edge, before.id.get(), after.id.get());
    }
    for (before, after) in input.topology.coedges.iter().zip(&output.topology.coedges) {
        add(EntityKind::Coedge, before.id.get(), after.id.get());
    }
    for (before, after) in input.topology.loops.iter().zip(&output.topology.loops) {
        add(EntityKind::Loop, before.id.get(), after.id.get());
    }
    for (before, after) in input.topology.faces.iter().zip(&output.topology.faces) {
        add(EntityKind::Face, before.id.get(), after.id.get());
    }
    for (before, after) in input.topology.shells.iter().zip(&output.topology.shells) {
        add(EntityKind::Shell, before.id.get(), after.id.get());
    }
    for (before, after) in input.topology.solids.iter().zip(&output.topology.solids) {
        add(EntityKind::Solid, before.id.get(), after.id.get());
    }
    history
}

fn face_push_pull_history(
    input: &Snapshot,
    output: &Snapshot,
    target_face: EntityRef,
) -> Result<Vec<HistoryRecord>, KernelError> {
    if input.counts() != output.counts() {
        return Err(face_push_pull_history_error(
            input.id,
            "a topology-preserving push/pull changed entity cardinality",
        ));
    }
    let target_index = input
        .topology
        .faces
        .iter()
        .position(|face| face.id.get() == target_face.entity.0)
        .ok_or_else(|| {
            face_push_pull_history_error(input.id, "the validated target face is missing")
        })?;

    let same_ids = input
        .topology
        .vertices
        .iter()
        .zip(&output.topology.vertices)
        .all(|(before, after)| before.id == after.id)
        && input
            .topology
            .edges
            .iter()
            .zip(&output.topology.edges)
            .all(|(before, after)| before.id == after.id)
        && input
            .topology
            .coedges
            .iter()
            .zip(&output.topology.coedges)
            .all(|(before, after)| before.id == after.id)
        && input
            .topology
            .loops
            .iter()
            .zip(&output.topology.loops)
            .all(|(before, after)| before.id == after.id)
        && input
            .topology
            .faces
            .iter()
            .zip(&output.topology.faces)
            .all(|(before, after)| before.id == after.id)
        && input
            .topology
            .shells
            .iter()
            .zip(&output.topology.shells)
            .all(|(before, after)| before.id == after.id)
        && input
            .topology
            .solids
            .iter()
            .zip(&output.topology.solids)
            .all(|(before, after)| before.id == after.id);
    if !same_ids {
        return Err(face_push_pull_history_error(
            input.id,
            "a topology-preserving push/pull changed entity identity",
        ));
    }

    let modified_vertices = input
        .topology
        .vertices
        .iter()
        .zip(&output.topology.vertices)
        .map(|(before, after)| before.value.point != after.value.point)
        .collect::<Vec<_>>();
    let modified_edges = input
        .topology
        .edges
        .iter()
        .zip(&output.topology.edges)
        .map(|(before, after)| {
            before.value.curve != after.value.curve
                || before.value.parameter_range != after.value.parameter_range
        })
        .collect::<Vec<_>>();
    let modified_coedges = input
        .topology
        .coedges
        .iter()
        .zip(&output.topology.coedges)
        .map(|(before, after)| {
            before.value.pcurve != after.value.pcurve
                || before.value.parameter_range != after.value.parameter_range
                || modified_edges[before.value.edge.0]
        })
        .collect::<Vec<_>>();
    let modified_loops = input
        .topology
        .loops
        .iter()
        .map(|record| {
            record
                .value
                .coedges
                .iter()
                .any(|coedge| modified_coedges[coedge.0])
        })
        .collect::<Vec<_>>();
    let modified_faces = input
        .topology
        .faces
        .iter()
        .zip(&output.topology.faces)
        .map(|(before, after)| {
            before.value.surface != after.value.surface
                || before
                    .value
                    .loops()
                    .any(|loop_key| modified_loops[loop_key.0])
        })
        .collect::<Vec<_>>();

    let mut history = Vec::with_capacity(output.counts().total() as usize);
    let mut push = |kind: EntityKind,
                    input_id: u64,
                    output_id: u64,
                    modified: bool,
                    role: &'static str,
                    ordinal: Option<u32>| {
        history.push(HistoryRecord {
            relation: if modified {
                HistoryRelation::Modified
            } else {
                HistoryRelation::Unchanged
            },
            inputs: vec![entity_ref(input.id, input_id, kind)],
            outputs: vec![entity_ref(output.id, output_id, kind)],
            role: Some(OperationRole::new(role, ordinal)),
        });
    };

    for (index, (before, after)) in input
        .topology
        .vertices
        .iter()
        .zip(&output.topology.vertices)
        .enumerate()
    {
        push(
            EntityKind::Vertex,
            before.id.get(),
            after.id.get(),
            modified_vertices[index],
            if modified_vertices[index] {
                "face_push_pull.moved_vertex"
            } else {
                "face_push_pull.preserved_vertex"
            },
            Some(index as u32),
        );
    }
    for (index, (before, after)) in input
        .topology
        .edges
        .iter()
        .zip(&output.topology.edges)
        .enumerate()
    {
        push(
            EntityKind::Edge,
            before.id.get(),
            after.id.get(),
            modified_edges[index],
            if modified_edges[index] {
                "face_push_pull.modified_edge"
            } else {
                "face_push_pull.preserved_edge"
            },
            Some(index as u32),
        );
    }
    for (index, (before, after)) in input
        .topology
        .coedges
        .iter()
        .zip(&output.topology.coedges)
        .enumerate()
    {
        push(
            EntityKind::Coedge,
            before.id.get(),
            after.id.get(),
            modified_coedges[index],
            if modified_coedges[index] {
                "face_push_pull.modified_coedge"
            } else {
                "face_push_pull.preserved_coedge"
            },
            Some(index as u32),
        );
    }
    for (index, (before, after)) in input
        .topology
        .loops
        .iter()
        .zip(&output.topology.loops)
        .enumerate()
    {
        push(
            EntityKind::Loop,
            before.id.get(),
            after.id.get(),
            modified_loops[index],
            if modified_loops[index] {
                "face_push_pull.modified_loop"
            } else {
                "face_push_pull.preserved_loop"
            },
            Some(index as u32),
        );
    }
    for (index, (before, after)) in input
        .topology
        .faces
        .iter()
        .zip(&output.topology.faces)
        .enumerate()
    {
        let (role, ordinal) = if index == target_index {
            ("face_push_pull.target_face", None)
        } else if modified_faces[index] {
            ("face_push_pull.side_face", Some(index as u32))
        } else {
            ("face_push_pull.preserved_face", Some(index as u32))
        };
        push(
            EntityKind::Face,
            before.id.get(),
            after.id.get(),
            modified_faces[index],
            role,
            ordinal,
        );
    }
    for (index, (before, after)) in input
        .topology
        .shells
        .iter()
        .zip(&output.topology.shells)
        .enumerate()
    {
        push(
            EntityKind::Shell,
            before.id.get(),
            after.id.get(),
            true,
            "face_push_pull.shell",
            Some(index as u32),
        );
    }
    for (index, (before, after)) in input
        .topology
        .solids
        .iter()
        .zip(&output.topology.solids)
        .enumerate()
    {
        push(
            EntityKind::Solid,
            before.id.get(),
            after.id.get(),
            true,
            "face_push_pull.solid",
            Some(index as u32),
        );
    }

    if history.len() != output.counts().total() as usize {
        return Err(face_push_pull_history_error(
            input.id,
            "push/pull history does not cover every output exactly once",
        ));
    }
    Ok(history)
}

fn face_push_pull_history_error(snapshot: SnapshotId, message: &str) -> KernelError {
    error(
        KernelErrorCode::InternalFailure,
        KernelStage::Commit,
        snapshot,
        message,
        vec![simple_diagnostic(
            "FACE_PUSH_PULL_HISTORY_INCOMPLETE",
            KernelStage::Commit,
            message,
        )],
    )
}

fn public_measures(measures: validator::ShapeMeasures) -> SnapshotMeasures {
    SnapshotMeasures {
        bounds: measures
            .bounds
            .map(|bounds| Aabb3::new(protocol_point(bounds.min), protocol_point(bounds.max))),
        surface_area: measures.surface_area,
        volume: measures.signed_volume,
        centroid: measures.centroid.map(protocol_point),
    }
}

fn circle_profile(center: ProtocolPoint2, radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center,
                    radius,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: Vec::new(),
        }],
    }
}

fn simple_invalid_input(
    snapshot: SnapshotId,
    code: &'static str,
    message: &'static str,
) -> KernelError {
    error(
        KernelErrorCode::InvalidInput,
        KernelStage::Preflight,
        snapshot,
        message,
        vec![simple_diagnostic(code, KernelStage::Preflight, message)],
    )
}

const fn protocol_counts(counts: TopologyCounts) -> ProtocolTopologyCounts {
    ProtocolTopologyCounts {
        vertices: counts.vertices as u64,
        edges: counts.edges as u64,
        coedges: counts.coedges as u64,
        loops: counts.loops as u64,
        faces: counts.faces as u64,
        shells: counts.shells as u64,
        solids: counts.solids as u64,
    }
}

const fn internal_point(point: ProtocolPoint3) -> Point3 {
    Point3::new(point.x, point.y, point.z)
}

const fn protocol_point(point: Point3) -> ProtocolPoint3 {
    ProtocolPoint3::new(point.x, point.y, point.z)
}

/// One display triangle carrying the carrier's exact normal at each vertex.
///
/// The chord triangle's own normal is used only where the closed form
/// degenerates — at a parameter singularity a tessellator should never emit —
/// which reproduces the previous flat shading rather than dropping the facet.
fn shaded_triangle(
    surface: Surface,
    vertices: [Point3; 3],
    source_face: EntityRef,
    role: FaceRole,
) -> DebugTriangle {
    let facet = facet_normal(vertices);
    DebugTriangle {
        vertices: vertices.map(protocol_point),
        normals: vertices
            .map(|vertex| protocol_vector(surface.outward_normal_at(vertex).unwrap_or(facet))),
        source_face,
        role,
    }
}

/// The chord triangle's unit normal under the outward winding every
/// tessellator emits.
fn facet_normal(vertices: [Point3; 3]) -> Vector3 {
    let normal = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
    let length = normal.length();
    if length.is_finite() && length > f64::EPSILON {
        normal / length
    } else {
        Vector3::new(0.0, 0.0, 1.0)
    }
}

const fn protocol_vector(vector: Vector3) -> ProtocolVector3 {
    ProtocolVector3::new(vector.x, vector.y, vector.z)
}

const fn entity_ref(snapshot: SnapshotId, id: u64, kind: EntityKind) -> EntityRef {
    EntityRef {
        snapshot,
        entity: ProtocolEntityId(id),
        kind,
    }
}

fn semantic_digest(topology: &Topology, precision: PrecisionPolicy) -> SemanticDigest {
    let mut hasher = Sha256::new();
    hasher.update(b"artificer.native.snapshot.v0");
    hash_precision(&mut hasher, precision);

    hash_collection_header(&mut hasher, b"vertices", topology.vertices.len());
    for vertex in &topology.vertices {
        hash_u64(&mut hasher, vertex.id.get());
        hash_point(&mut hasher, vertex.value.point);
    }
    hash_collection_header(&mut hasher, b"edges", topology.edges.len());
    for edge in &topology.edges {
        hash_u64(&mut hasher, edge.id.get());
        hash_u64(&mut hasher, edge.value.vertices[0].0 as u64);
        hash_u64(&mut hasher, edge.value.vertices[1].0 as u64);
        edge.value
            .endpoints()
            .into_iter()
            .for_each(|point| hash_point(&mut hasher, point));
        if let Curve3::Circle {
            center,
            u,
            v,
            radius,
        } = edge.value.curve
        {
            hasher.update(b"analytic-circle-curve-v0");
            hash_point(&mut hasher, center);
            for vector in [u, v] {
                hash_f64(&mut hasher, vector.x);
                hash_f64(&mut hasher, vector.y);
                hash_f64(&mut hasher, vector.z);
            }
            hash_f64(&mut hasher, radius);
            hash_f64(&mut hasher, edge.value.parameter_range.start);
            hash_f64(&mut hasher, edge.value.parameter_range.end);
        }
        if let Curve3::Ellipse {
            center,
            u,
            v,
            major_radius,
            minor_radius,
        } = edge.value.curve
        {
            hasher.update(b"analytic-ellipse-curve-v0");
            hash_point(&mut hasher, center);
            for vector in [u, v] {
                hash_f64(&mut hasher, vector.x);
                hash_f64(&mut hasher, vector.y);
                hash_f64(&mut hasher, vector.z);
            }
            hash_f64(&mut hasher, major_radius);
            hash_f64(&mut hasher, minor_radius);
            hash_f64(&mut hasher, edge.value.parameter_range.start);
            hash_f64(&mut hasher, edge.value.parameter_range.end);
        }
    }
    hash_collection_header(&mut hasher, b"coedges", topology.coedges.len());
    for coedge in &topology.coedges {
        hash_u64(&mut hasher, coedge.id.get());
        hash_u64(&mut hasher, coedge.value.edge.0 as u64);
        hasher.update([match coedge.value.orientation {
            Orientation::Forward => 0,
            Orientation::Reverse => 1,
        }]);
        for point in coedge.value.pcurve_endpoints() {
            hash_f64(&mut hasher, point.x);
            hash_f64(&mut hasher, point.y);
        }
        if let Curve2::Circle {
            center,
            u,
            v,
            radius,
        } = coedge.value.pcurve
        {
            hasher.update(b"analytic-circle-pcurve-v0");
            for value in [center.x, center.y, u.x, u.y, v.x, v.y, radius] {
                hash_f64(&mut hasher, value);
            }
            hash_f64(&mut hasher, coedge.value.parameter_range.start);
            hash_f64(&mut hasher, coedge.value.parameter_range.end);
        }
        if let Curve2::Harmonic {
            mean,
            amplitude,
            phase,
        } = coedge.value.pcurve
        {
            hasher.update(b"analytic-harmonic-pcurve-v0");
            for value in [mean, amplitude, phase] {
                hash_f64(&mut hasher, value);
            }
            hash_f64(&mut hasher, coedge.value.parameter_range.start);
            hash_f64(&mut hasher, coedge.value.parameter_range.end);
        }
    }
    hash_collection_header(&mut hasher, b"loops", topology.loops.len());
    for loop_record in &topology.loops {
        hash_u64(&mut hasher, loop_record.id.get());
        hash_u64(&mut hasher, loop_record.value.coedges.len() as u64);
        for coedge in &loop_record.value.coedges {
            hash_u64(&mut hasher, coedge.0 as u64);
        }
    }
    hash_collection_header(&mut hasher, b"faces", topology.faces.len());
    for face in &topology.faces {
        hash_u64(&mut hasher, face.id.get());
        match face.value.surface {
            Surface::Plane(plane) => {
                hash_point(&mut hasher, plane.origin);
                for vector in [plane.u, plane.v, plane.normal] {
                    hash_f64(&mut hasher, vector.x);
                    hash_f64(&mut hasher, vector.y);
                    hash_f64(&mut hasher, vector.z);
                }
            }
            Surface::Cylinder(cylinder) => {
                hasher.update(b"analytic-cylinder-surface-v0");
                hash_point(&mut hasher, cylinder.origin);
                for vector in [cylinder.axis, cylinder.radial_u, cylinder.radial_v] {
                    hash_f64(&mut hasher, vector.x);
                    hash_f64(&mut hasher, vector.y);
                    hash_f64(&mut hasher, vector.z);
                }
                hash_f64(&mut hasher, cylinder.radius);
                hash_f64(&mut hasher, cylinder.angular_sign);
            }
            Surface::Torus(torus) => {
                hasher.update(b"analytic-torus-surface-v0");
                hash_point(&mut hasher, torus.origin);
                for vector in [torus.axis, torus.radial_u, torus.radial_v] {
                    hash_f64(&mut hasher, vector.x);
                    hash_f64(&mut hasher, vector.y);
                    hash_f64(&mut hasher, vector.z);
                }
                hash_f64(&mut hasher, torus.major_radius);
                hash_f64(&mut hasher, torus.minor_radius);
                hash_f64(&mut hasher, torus.angular_sign);
            }
            Surface::Sphere(sphere) => {
                hasher.update(b"analytic-sphere-surface-v0");
                hash_point(&mut hasher, sphere.origin);
                for vector in [sphere.axis, sphere.radial_u, sphere.radial_v] {
                    hash_f64(&mut hasher, vector.x);
                    hash_f64(&mut hasher, vector.y);
                    hash_f64(&mut hasher, vector.z);
                }
                hash_f64(&mut hasher, sphere.radius);
                hash_f64(&mut hasher, sphere.angular_sign);
            }
            Surface::Cone(cone) => {
                hasher.update(b"analytic-cone-surface-v0");
                hash_point(&mut hasher, cone.origin);
                for vector in [cone.axis, cone.radial_u, cone.radial_v] {
                    hash_f64(&mut hasher, vector.x);
                    hash_f64(&mut hasher, vector.y);
                    hash_f64(&mut hasher, vector.z);
                }
                hash_f64(&mut hasher, cone.base_radius);
                hash_f64(&mut hasher, cone.slope);
                hash_f64(&mut hasher, cone.angular_sign);
            }
        }
        hash_u64(&mut hasher, face.value.outer_loop.0 as u64);
        // Preserve established digests for the pre-hole representation while
        // giving every hole-owned face an unambiguous, ordered extension.
        if !face.value.inner_loops.is_empty() {
            hash_collection_header(&mut hasher, b"inner-loops", face.value.inner_loops.len());
            for inner_loop in &face.value.inner_loops {
                hash_u64(&mut hasher, inner_loop.0 as u64);
            }
        }
        hash_face_role(&mut hasher, face.value.role);
    }
    hash_collection_header(&mut hasher, b"shells", topology.shells.len());
    for shell in &topology.shells {
        hash_u64(&mut hasher, shell.id.get());
        hash_u64(&mut hasher, shell.value.faces.len() as u64);
        for face in &shell.value.faces {
            hash_u64(&mut hasher, face.0 as u64);
        }
    }
    hash_collection_header(&mut hasher, b"solids", topology.solids.len());
    for solid in &topology.solids {
        hash_u64(&mut hasher, solid.id.get());
        hash_u64(&mut hasher, solid.value.outer_shell.0 as u64);
        hash_collection_header(&mut hasher, b"inner-shells", solid.value.inner_shells.len());
        for shell in &solid.value.inner_shells {
            hash_u64(&mut hasher, shell.0 as u64);
        }
    }
    SemanticDigest::new(hasher.finalize().into())
}

fn hash_collection_header(hasher: &mut Sha256, label: &[u8], length: usize) {
    hash_u64(hasher, label.len() as u64);
    hasher.update(label);
    hash_u64(hasher, length as u64);
}

fn hash_precision(hasher: &mut Sha256, precision: PrecisionPolicy) {
    hasher.update([match precision.unit {
        artificer_protocol::LengthUnit::Millimetre => 0,
        artificer_protocol::LengthUnit::Metre => 1,
        artificer_protocol::LengthUnit::Inch => 2,
    }]);
    for value in [
        precision.modeling_resolution,
        precision.linear_agreement,
        precision.angular_agreement_radians,
        precision.parameter_resolution,
        precision.approximation_budget,
        precision.max_entity_uncertainty,
        precision.max_operation_uncertainty,
        precision.max_abs_coordinate,
        precision.min_feature_size,
    ] {
        hash_f64(hasher, value);
    }
    hash_u64(hasher, u64::from(precision.max_iterations));
    hash_u64(hasher, u64::from(precision.max_subdivisions));
}

fn hash_point(hasher: &mut Sha256, point: Point3) {
    hash_f64(hasher, point.x);
    hash_f64(hasher, point.y);
    hash_f64(hasher, point.z);
}

fn hash_f64(hasher: &mut Sha256, value: f64) {
    let canonical = if value == 0.0 { 0.0 } else { value };
    hasher.update(canonical.to_bits().to_le_bytes());
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_le_bytes());
}

fn hash_face_role(hasher: &mut Sha256, role: FaceRole) {
    match role {
        FaceRole::NegativeX => hasher.update([0]),
        FaceRole::PositiveX => hasher.update([1]),
        FaceRole::NegativeY => hasher.update([2]),
        FaceRole::PositiveY => hasher.update([3]),
        FaceRole::NegativeZ => hasher.update([4]),
        FaceRole::PositiveZ => hasher.update([5]),
        FaceRole::ExtrusionBottom => hasher.update([6]),
        FaceRole::ExtrusionTop => hasher.update([7]),
        FaceRole::ExtrusionSide(ordinal) => {
            hasher.update([8]);
            hash_u64(hasher, u64::from(ordinal));
        }
        FaceRole::FeatureEnd => hasher.update([9]),
        FaceRole::FeatureSide(ordinal) => {
            hasher.update([10]);
            hash_u64(hasher, u64::from(ordinal));
        }
    }
}

fn digest_bytes(bytes: &[u8]) -> SemanticDigest {
    SemanticDigest::new(Sha256::digest(bytes).into())
}

fn snapshot_id(digest: SemanticDigest) -> SnapshotId {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    SnapshotId::new(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use artificer_protocol::{
        ArcDirection, HistoryRelation, KernelErrorCode, PlanarCurve2, PlanarFrame3, PlanarLoop2,
        PlanarProfile2, PlanarRegion2, Point2 as ProtocolPoint2, RequestId, RotationQuaternion,
        SimilarityTransform3, Vector3 as ProtocolVector3,
    };

    const EPSILON: f64 = 1.0e-9;

    fn request(expected_snapshot: SnapshotId) -> ExecuteRequest {
        ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("kernel-test"),
            expected_snapshot,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: ProtocolPoint3::new(0.0, 0.0, 0.0),
                size_x: 2.0,
                size_y: 3.0,
                size_z: 4.0,
            },
        }
    }

    fn canonical() -> ExecutionOutcome {
        let input = NativeKernel::empty();
        NativeKernel::execute(&input, &request(input.id()), &CancellationToken::new()).unwrap()
    }

    #[test]
    fn exact_edge_chamfer_and_fillet_are_valid_and_measure_preserving() {
        let radius = 0.4;
        for kind in [
            artificer_protocol::EdgeFinishKind::Chamfer,
            artificer_protocol::EdgeFinishKind::Fillet,
        ] {
            let base = canonical();
            let edge = NativeKernel::debug_scene(&base.snapshot).edges[0];
            let edge_length = ((edge.endpoints[1].x - edge.endpoints[0].x).powi(2)
                + (edge.endpoints[1].y - edge.endpoints[0].y).powi(2)
                + (edge.endpoints[1].z - edge.endpoints[0].z).powi(2))
            .sqrt();
            let removed_area = match kind {
                artificer_protocol::EdgeFinishKind::Chamfer => 0.5 * radius * radius,
                artificer_protocol::EdgeFinishKind::Fillet => {
                    radius * radius * (1.0 - std::f64::consts::PI / 4.0)
                }
            };
            let request = ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new(format!("edge-finish-{kind:?}")),
                expected_snapshot: base.snapshot.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::FinishEdge {
                    target_edge: edge.source_edge,
                    kind,
                    distance: radius,
                },
            };
            let finished =
                NativeKernel::execute(&base.snapshot, &request, &CancellationToken::new())
                    .expect("exact edge finish");
            assert!(NativeKernel::validate(&finished.snapshot, ValidationProfile::Solid).valid);
            let expected = 24.0 - removed_area * edge_length;
            assert!(
                (finished.snapshot.measures().volume - expected).abs() < 1.0e-8,
                "{kind:?}: expected {expected}, received {}",
                finished.snapshot.measures().volume
            );
            assert_eq!(
                finished
                    .snapshot
                    .topology
                    .faces
                    .iter()
                    .any(|face| matches!(face.value.surface, Surface::Cylinder(_))),
                kind == artificer_protocol::EdgeFinishKind::Fillet
            );
        }
    }

    #[test]
    fn every_cuboid_edge_finishes_and_parallel_sets_commit_atomically() {
        for kind in [
            artificer_protocol::EdgeFinishKind::Chamfer,
            artificer_protocol::EdgeFinishKind::Fillet,
        ] {
            let base = canonical();
            let scene = NativeKernel::debug_scene(&base.snapshot);
            for (index, edge) in scene.edges.iter().enumerate() {
                let request = ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!("every-edge-{kind:?}-{index}")),
                    expected_snapshot: base.snapshot.id(),
                    precision: PrecisionPolicy::default(),
                    command: KernelCommand::FinishEdge {
                        target_edge: edge.source_edge,
                        kind,
                        distance: 0.25,
                    },
                };
                let finished =
                    NativeKernel::execute(&base.snapshot, &request, &CancellationToken::new())
                        .unwrap_or_else(|error| panic!("edge {index} {kind:?}: {error}"));
                assert!(NativeKernel::validate(&finished.snapshot, ValidationProfile::Solid).valid);
            }

            let axis = |edge: &DebugEdge| {
                let delta = [
                    (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                    (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                    (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
                ];
                delta
                    .iter()
                    .enumerate()
                    .max_by(|left, right| left.1.total_cmp(right.1))
                    .unwrap()
                    .0
            };
            let target_edges = scene
                .edges
                .iter()
                .filter(|edge| axis(edge) == 0)
                .map(|edge| edge.source_edge)
                .collect::<Vec<_>>();
            assert_eq!(target_edges.len(), 4);
            let request = ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new(format!("parallel-edge-set-{kind:?}")),
                expected_snapshot: base.snapshot.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::FinishEdges {
                    target_edges,
                    kind,
                    distance: 0.25,
                },
            };
            let finished =
                NativeKernel::execute(&base.snapshot, &request, &CancellationToken::new())
                    .expect("parallel edge set");
            assert!(NativeKernel::validate(&finished.snapshot, ValidationProfile::Solid).valid);
            assert!(finished.snapshot.measures().volume < base.snapshot.measures().volume);
        }
    }

    #[test]
    fn edge_finish_rejects_stale_and_curved_targets_but_accepts_straight_successors() {
        let base = canonical();
        let edge = NativeKernel::debug_scene(&base.snapshot).edges[0].source_edge;
        let stale = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("edge-finish-stale"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::FinishEdge {
                target_edge: EntityRef {
                    snapshot: SnapshotId::ZERO,
                    ..edge
                },
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.4,
            },
        };
        let error = NativeKernel::execute(&base.snapshot, &stale, &CancellationToken::new())
            .expect_err("stale edge target must be rejected");
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.as_str() == "EDGE_FINISH_TARGET_INVALID" })
        );

        let first = ExecuteRequest {
            command: KernelCommand::FinishEdge {
                target_edge: edge,
                kind: artificer_protocol::EdgeFinishKind::Fillet,
                distance: 0.4,
            },
            request_id: RequestId::new("edge-finish-first"),
            ..request(base.snapshot.id())
        };
        let filleted = NativeKernel::execute(&base.snapshot, &first, &CancellationToken::new())
            .expect("first fillet");
        let scene = NativeKernel::debug_scene(&filleted.snapshot);
        let next_edge = scene
            .edges
            .iter()
            .find(|edge| {
                !edge.is_smooth
                    && scene
                        .edges
                        .iter()
                        .filter(|candidate| candidate.source_edge == edge.source_edge)
                        .count()
                        == 1
            })
            .expect("filleted prism retains straight successor edges")
            .source_edge;
        let second = ExecuteRequest {
            command: KernelCommand::FinishEdge {
                target_edge: next_edge,
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.2,
            },
            request_id: RequestId::new("edge-finish-successor-regularized"),
            expected_snapshot: filleted.snapshot.id(),
            ..request(filleted.snapshot.id())
        };
        let chamfered =
            NativeKernel::execute(&filleted.snapshot, &second, &CancellationToken::new())
                .expect("a visible straight successor beside the fillet must commit");
        assert!(NativeKernel::validate(&chamfered.snapshot, ValidationProfile::Solid).valid);

        let curved_edge = scene
            .edges
            .iter()
            .find(|edge| {
                scene
                    .edges
                    .iter()
                    .filter(|candidate| candidate.source_edge == edge.source_edge)
                    .count()
                    > 1
            })
            .expect("the exact fillet exposes a sampled curved carrier")
            .source_edge;
        let curved = ExecuteRequest {
            command: KernelCommand::FinishEdge {
                target_edge: curved_edge,
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.2,
            },
            request_id: RequestId::new("edge-finish-curved-carrier-rejected"),
            expected_snapshot: filleted.snapshot.id(),
            ..request(filleted.snapshot.id())
        };
        let error = NativeKernel::execute(&filleted.snapshot, &curved, &CancellationToken::new())
            .expect_err("a sampled curved carrier must not masquerade as one straight edge");
        assert_eq!(error.input_snapshot, filleted.snapshot.id());
    }

    #[test]
    fn perpendicular_cuboid_edges_regularize_as_one_atomic_finish() {
        for kind in [
            artificer_protocol::EdgeFinishKind::Chamfer,
            artificer_protocol::EdgeFinishKind::Fillet,
        ] {
            let base = canonical();
            let scene = NativeKernel::debug_scene(&base.snapshot);
            let axis = |edge: &DebugEdge| {
                let delta = [
                    (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                    (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                    (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
                ];
                delta
                    .iter()
                    .enumerate()
                    .max_by(|left, right| left.1.total_cmp(right.1))
                    .unwrap()
                    .0
            };
            let endpoints_touch = |left: &DebugEdge, right: &DebugEdge| {
                left.endpoints.iter().any(|left| {
                    right.endpoints.iter().any(|right| {
                        (left.x - right.x).abs() < 1.0e-9
                            && (left.y - right.y).abs() < 1.0e-9
                            && (left.z - right.z).abs() < 1.0e-9
                    })
                })
            };
            let mut targets = None;
            'search: for (index, left) in scene.edges.iter().enumerate() {
                for right in scene.edges.iter().skip(index + 1) {
                    if axis(left) != axis(right) && endpoints_touch(left, right) {
                        targets = Some(vec![left.source_edge, right.source_edge]);
                        break 'search;
                    }
                }
            }
            let request = ExecuteRequest {
                command: KernelCommand::FinishEdges {
                    target_edges: targets.expect("perpendicular cuboid edge pair"),
                    kind,
                    distance: 0.25,
                },
                request_id: RequestId::new(format!("perpendicular-edge-set-{kind:?}")),
                ..request(base.snapshot.id())
            };
            let finished =
                NativeKernel::execute(&base.snapshot, &request, &CancellationToken::new())
                    .expect("perpendicular edge finish should regularize");
            assert!(NativeKernel::validate(&finished.snapshot, ValidationProfile::Solid).valid);
            assert!(finished.snapshot.measures().volume < base.snapshot.measures().volume);
            let maximum_faces = if kind == artificer_protocol::EdgeFinishKind::Chamfer {
                20
            } else {
                100
            };
            assert!(
                finished.snapshot.counts().faces <= maximum_faces,
                "regularized finish must retain consolidated B-rep faces"
            );
            let presentation = NativeKernel::debug_scene(&finished.snapshot);
            if kind == artificer_protocol::EdgeFinishKind::Fillet {
                let smooth = presentation
                    .edges
                    .iter()
                    .filter(|edge| edge.is_smooth)
                    .count();
                assert!(smooth > 0, "fillet subdivision edges must be marked smooth");
                assert!(
                    smooth < presentation.edges.len(),
                    "mechanical crease edges must remain selectable"
                );
                let transition_rails = finished
                    .snapshot
                    .topology
                    .edges
                    .iter()
                    .enumerate()
                    .filter(|(edge_index, _)| {
                        let roles = finished
                            .snapshot
                            .topology
                            .faces
                            .iter()
                            .filter(|face| {
                                face.value.loops().any(|loop_key| {
                                    finished.snapshot.topology.loops[loop_key.0]
                                        .value
                                        .coedges
                                        .iter()
                                        .any(|coedge_key| {
                                            finished.snapshot.topology.coedges[coedge_key.0]
                                                .value
                                                .edge
                                                .0
                                                == *edge_index
                                        })
                                })
                            })
                            .map(|face| face.value.role)
                            .collect::<Vec<_>>();
                        matches!(roles.as_slice(), [first, second]
                            if matches!(first, FaceRole::FeatureSide(_))
                                != matches!(second, FaceRole::FeatureSide(_)))
                    })
                    .map(|(edge_index, _)| edge_index)
                    .collect::<Vec<_>>();
                assert!(
                    transition_rails.len() >= 2,
                    "a fillet must retain both tangent transition rails"
                );
                assert!(transition_rails.iter().all(|edge_index| {
                    !presentation_edge_is_smooth(&finished.snapshot.topology, *edge_index)
                }));
                let smooth_flags = presentation_smooth_edge_flags(&finished.snapshot.topology);
                let mut visible_degree = vec![0_usize; finished.snapshot.topology.vertices.len()];
                for (edge_index, edge) in finished.snapshot.topology.edges.iter().enumerate() {
                    if !smooth_flags[edge_index] {
                        for vertex in edge.value.vertices {
                            visible_degree[vertex.0] += 1;
                        }
                    }
                }
                assert!(
                    visible_degree.iter().all(|degree| *degree != 1),
                    "feature-boundary presentation chains must not end in open fragments"
                );
            }
        }
    }

    #[test]
    fn trihedral_fillet_presents_closed_rails_without_approximation_point_noise() {
        let base = canonical();
        let scene = NativeKernel::debug_scene(&base.snapshot);
        let corner = scene.edges[0].endpoints[0];
        let same = |point: ProtocolPoint3| {
            (point.x - corner.x).abs() <= 1.0e-9
                && (point.y - corner.y).abs() <= 1.0e-9
                && (point.z - corner.z).abs() <= 1.0e-9
        };
        let targets = scene
            .edges
            .iter()
            .filter(|edge| edge.endpoints.iter().copied().any(same))
            .map(|edge| edge.source_edge)
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 3, "a cuboid corner owns three edges");
        let request = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges: targets,
                kind: artificer_protocol::EdgeFinishKind::Fillet,
                distance: 0.25,
            },
            request_id: RequestId::new("trihedral-fillet-presentation"),
            ..request(base.snapshot.id())
        };
        let filleted = NativeKernel::execute(&base.snapshot, &request, &CancellationToken::new())
            .expect("three-edge corner fillet");
        let smooth = presentation_smooth_edge_flags(&filleted.snapshot.topology);
        let mut visible_degree = vec![0_usize; filleted.snapshot.topology.vertices.len()];
        for (edge_index, edge) in filleted.snapshot.topology.edges.iter().enumerate() {
            if !smooth[edge_index] {
                for vertex in edge.value.vertices {
                    visible_degree[vertex.0] += 1;
                }
            }
        }
        assert!(
            visible_degree.iter().all(|degree| *degree != 1),
            "trihedral feature rails must not contain dangling display fragments"
        );
        let presentation = NativeKernel::debug_scene(&filleted.snapshot);
        assert!(
            presentation.vertices.iter().any(|vertex| vertex.is_smooth),
            "the regularized corner should identify approximation-only vertices"
        );
        assert!(
            presentation.vertices.iter().any(|vertex| !vertex.is_smooth),
            "mechanical corner vertices must remain selectable"
        );
    }

    #[test]
    fn trihedral_fillet_successor_accepts_a_connected_u_chain_chamfer() {
        let base = canonical();
        let scene = NativeKernel::debug_scene(&base.snapshot);
        let same = |left: ProtocolPoint3, right: ProtocolPoint3| {
            (left.x - right.x).abs() <= 1.0e-9
                && (left.y - right.y).abs() <= 1.0e-9
                && (left.z - right.z).abs() <= 1.0e-9
        };
        let origin = ProtocolPoint3::new(0.0, 0.0, 0.0);
        let fillet_targets = scene
            .edges
            .iter()
            .filter(|edge| edge.endpoints.iter().any(|point| same(*point, origin)))
            .map(|edge| edge.source_edge)
            .collect::<Vec<_>>();
        assert_eq!(fillet_targets.len(), 3);
        let fillet_request = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges: fillet_targets,
                kind: artificer_protocol::EdgeFinishKind::Fillet,
                distance: 0.25,
            },
            request_id: RequestId::new("trihedral-fillet-before-u-chamfer"),
            ..request(base.snapshot.id())
        };
        let filleted =
            NativeKernel::execute(&base.snapshot, &fillet_request, &CancellationToken::new())
                .expect("three-edge corner fillet");
        let filleted_scene = NativeKernel::debug_scene(&filleted.snapshot);

        let expected_u = [
            [
                ProtocolPoint3::new(0.0, 0.0, 4.0),
                ProtocolPoint3::new(2.0, 0.0, 4.0),
            ],
            [
                ProtocolPoint3::new(2.0, 0.0, 4.0),
                ProtocolPoint3::new(2.0, 3.0, 4.0),
            ],
            [
                ProtocolPoint3::new(2.0, 3.0, 4.0),
                ProtocolPoint3::new(0.0, 3.0, 4.0),
            ],
        ];
        let targets = expected_u
            .into_iter()
            .map(|segment| {
                resolve_successor_edge(&filleted_scene, segment, PrecisionPolicy::default())
                    .expect("each logical U-chain edge survives the fillet")
            })
            .collect::<Vec<_>>();
        let chamfer_request = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges: targets,
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.25,
            },
            request_id: RequestId::new("connected-u-chamfer-after-trihedral-fillet"),
            ..request(filleted.snapshot.id())
        };
        let chamfered = NativeKernel::execute(
            &filleted.snapshot,
            &chamfer_request,
            &CancellationToken::new(),
        )
        .expect("connected U-chain chamfer should publish");
        assert!(NativeKernel::validate(&chamfered.snapshot, ValidationProfile::Solid).valid);
        assert!(chamfered.snapshot.measures().volume < filleted.snapshot.measures().volume);
    }

    #[test]
    fn chamfer_boundary_rails_accept_stacked_chamfer_and_fillet_features() {
        let base = canonical();
        let edge = NativeKernel::debug_scene(&base.snapshot).edges[0].source_edge;
        let first = ExecuteRequest {
            command: KernelCommand::FinishEdge {
                target_edge: edge,
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.3,
            },
            request_id: RequestId::new("stacked-finish-base-chamfer"),
            ..request(base.snapshot.id())
        };
        let chamfered = NativeKernel::execute(&base.snapshot, &first, &CancellationToken::new())
            .expect("base chamfer");
        let boundary = chamfered
            .snapshot
            .topology
            .edges
            .iter()
            .enumerate()
            .find_map(|(edge_index, edge)| {
                if presentation_edge_is_smooth(&chamfered.snapshot.topology, edge_index) {
                    return None;
                }
                let incident = chamfered
                    .snapshot
                    .topology
                    .faces
                    .iter()
                    .filter(|face| {
                        face.value.loops().any(|loop_key| {
                            chamfered.snapshot.topology.loops[loop_key.0]
                                .value
                                .coedges
                                .iter()
                                .any(|coedge_key| {
                                    chamfered.snapshot.topology.coedges[coedge_key.0]
                                        .value
                                        .edge
                                        .0
                                        == edge_index
                                })
                        })
                    })
                    .filter_map(|face| match face.value.surface {
                        Surface::Plane(plane) => Some(plane.normal),
                        Surface::Cylinder(_)
                        | Surface::Torus(_)
                        | Surface::Cone(_)
                        | Surface::Sphere(_) => None,
                    })
                    .collect::<Vec<_>>();
                (incident.len() == 2
                    && incident[0].dot(incident[1]).abs() > 1.0e-4
                    && incident[0].dot(incident[1]).abs() < 1.0 - 1.0e-4)
                    .then(|| entity_ref(chamfered.snapshot.id(), edge.id.get(), EntityKind::Edge))
            })
            .expect("the chamfer publishes a selectable boundary rail");

        for (kind, request_id) in [
            (
                artificer_protocol::EdgeFinishKind::Chamfer,
                "chamfer-on-chamfer",
            ),
            (
                artificer_protocol::EdgeFinishKind::Fillet,
                "fillet-on-chamfer",
            ),
        ] {
            let stacked = ExecuteRequest {
                command: KernelCommand::FinishEdge {
                    target_edge: boundary,
                    kind,
                    distance: 0.1,
                },
                request_id: RequestId::new(request_id),
                ..request(chamfered.snapshot.id())
            };
            let finished =
                NativeKernel::execute(&chamfered.snapshot, &stacked, &CancellationToken::new())
                    .expect("a chamfer boundary rail must accept another finish");
            assert!(NativeKernel::validate(&finished.snapshot, ValidationProfile::Solid).valid);
            assert!(finished.snapshot.measures().volume < chamfered.snapshot.measures().volume);
        }
    }

    #[test]
    fn intersecting_fillet_patch_rails_accept_a_second_finish() {
        let base = canonical();
        let scene = NativeKernel::debug_scene(&base.snapshot);
        let origin = ProtocolPoint3::new(0.0, 0.0, 0.0);
        let near = |point: ProtocolPoint3| {
            (point.x - origin.x).abs() <= 1.0e-9
                && (point.y - origin.y).abs() <= 1.0e-9
                && (point.z - origin.z).abs() <= 1.0e-9
        };
        let targets = scene
            .edges
            .iter()
            .filter(|edge| edge.endpoints.iter().copied().any(near))
            .map(|edge| edge.source_edge)
            .collect::<Vec<_>>();
        let first = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges: targets,
                kind: artificer_protocol::EdgeFinishKind::Fillet,
                distance: 0.25,
            },
            request_id: RequestId::new("intersecting-fillet-patch-base"),
            ..request(base.snapshot.id())
        };
        let filleted = NativeKernel::execute(&base.snapshot, &first, &CancellationToken::new())
            .expect("trihedral fillet");
        let rail = filleted
            .snapshot
            .topology
            .edges
            .iter()
            .enumerate()
            .find_map(|(edge_index, edge)| {
                if presentation_edge_is_smooth(&filleted.snapshot.topology, edge_index) {
                    return None;
                }
                let roles = filleted
                    .snapshot
                    .topology
                    .faces
                    .iter()
                    .filter(|face| {
                        face.value.loops().any(|loop_key| {
                            filleted.snapshot.topology.loops[loop_key.0]
                                .value
                                .coedges
                                .iter()
                                .any(|coedge_key| {
                                    filleted.snapshot.topology.coedges[coedge_key.0]
                                        .value
                                        .edge
                                        .0
                                        == edge_index
                                })
                        })
                    })
                    .map(|face| face.value.role)
                    .collect::<Vec<_>>();
                (roles.len() == 2
                    && roles
                        .iter()
                        .all(|role| matches!(role, FaceRole::FeatureSide(_))))
                .then(|| entity_ref(filleted.snapshot.id(), edge.id.get(), EntityKind::Edge))
            })
            .expect("the regularized corner has a real patch-intersection rail");

        for (kind, request_id) in [
            (
                artificer_protocol::EdgeFinishKind::Chamfer,
                "chamfer-on-fillet-intersection",
            ),
            (
                artificer_protocol::EdgeFinishKind::Fillet,
                "fillet-on-fillet-intersection",
            ),
        ] {
            let request = ExecuteRequest {
                command: KernelCommand::FinishEdge {
                    target_edge: rail,
                    kind,
                    distance: 0.01,
                },
                request_id: RequestId::new(request_id),
                ..request(filleted.snapshot.id())
            };
            let finished = NativeKernel::execute(
                &filleted.snapshot,
                &request,
                &CancellationToken::new(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "a genuine fillet-patch intersection must accept a second {kind:?}: {error:?}"
                )
            });
            assert!(NativeKernel::validate(&finished.snapshot, ValidationProfile::Solid).valid);
        }
    }

    #[test]
    fn regularized_fillet_successor_accepts_three_logical_chamfer_edges() {
        let base = canonical();
        let scene = NativeKernel::debug_scene(&base.snapshot);
        let axis = |edge: &DebugEdge| {
            let delta = [
                (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
            ];
            delta
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .unwrap()
                .0
        };
        let first = scene.edges[0];
        let same_point = |left: ProtocolPoint3, right: ProtocolPoint3| {
            (left.x - right.x).abs() < 1.0e-9
                && (left.y - right.y).abs() < 1.0e-9
                && (left.z - right.z).abs() < 1.0e-9
        };
        let second = scene
            .edges
            .iter()
            .copied()
            .find(|edge| {
                axis(edge) != axis(&first)
                    && first.endpoints.iter().any(|first| {
                        edge.endpoints
                            .iter()
                            .any(|second| same_point(*first, *second))
                    })
            })
            .expect("touching perpendicular edge");
        let shared = first
            .endpoints
            .iter()
            .copied()
            .find(|first| {
                second
                    .endpoints
                    .iter()
                    .any(|second| same_point(*first, *second))
            })
            .unwrap();
        let point_distance_squared = |point: ProtocolPoint3| {
            let delta = [point.x - shared.x, point.y - shared.y, point.z - shared.z];
            delta[0].mul_add(delta[0], delta[1].mul_add(delta[1], delta[2] * delta[2]))
        };
        let mut untouched = scene
            .edges
            .iter()
            .copied()
            .filter(|edge| {
                edge.source_edge != first.source_edge && edge.source_edge != second.source_edge
            })
            .collect::<Vec<_>>();
        untouched.sort_by(|left, right| {
            let clearance = |edge: &DebugEdge| {
                edge.endpoints
                    .iter()
                    .copied()
                    .map(point_distance_squared)
                    .fold(f64::INFINITY, f64::min)
            };
            clearance(right).total_cmp(&clearance(left))
        });
        let untouched = untouched.into_iter().take(3).collect::<Vec<_>>();
        let fillet_request = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges: vec![first.source_edge, second.source_edge],
                kind: artificer_protocol::EdgeFinishKind::Fillet,
                distance: 0.25,
            },
            request_id: RequestId::new("regularized-fillet-for-successor"),
            ..request(base.snapshot.id())
        };
        let filleted =
            NativeKernel::execute(&base.snapshot, &fillet_request, &CancellationToken::new())
                .expect("regularized fillet");
        let filleted_scene = NativeKernel::debug_scene(&filleted.snapshot);
        let target_edges = untouched
            .iter()
            .map(|edge| {
                resolve_successor_edge(&filleted_scene, edge.endpoints, PrecisionPolicy::default())
                    .expect("untouched edge remains logically selectable")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            target_edges.len(),
            3,
            "filleted successor retains logical crease edges"
        );
        let chamfer_request = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges,
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.1,
            },
            request_id: RequestId::new("logical-successor-chamfer"),
            ..request(filleted.snapshot.id())
        };
        let chamfered = NativeKernel::execute(
            &filleted.snapshot,
            &chamfer_request,
            &CancellationToken::new(),
        )
        .expect("logical successor edge should chamfer");
        assert!(NativeKernel::validate(&chamfered.snapshot, ValidationProfile::Solid).valid);
        assert!(chamfered.snapshot.measures().volume < filleted.snapshot.measures().volume);
        assert!(
            chamfered.snapshot.counts().faces <= 160,
            "successor operations must preserve consolidated source faces: {:?}",
            chamfered.snapshot.counts()
        );
    }

    #[test]
    fn transformed_three_edge_corner_chamfers_without_world_axis_assumptions() {
        let base = canonical();
        let half_angle = std::f64::consts::FRAC_PI_8;
        let transformed = execute_transform(
            &base.snapshot,
            SimilarityTransform3 {
                translation: ProtocolVector3::new(1.5, -0.75, 0.25),
                rotation: RotationQuaternion::new(
                    half_angle.cos(),
                    half_angle.sin() * 0.4,
                    half_angle.sin() * 0.5,
                    half_angle.sin() * 0.768_114_574_786_860_8,
                ),
                uniform_scale: 1.0,
            },
        );
        let scene = NativeKernel::debug_scene(&transformed.snapshot);
        let corner = scene.edges[0].endpoints[0];
        let same_point = |point: ProtocolPoint3| {
            (point.x - corner.x).abs() < 1.0e-9
                && (point.y - corner.y).abs() < 1.0e-9
                && (point.z - corner.z).abs() < 1.0e-9
        };
        let target_edges = scene
            .edges
            .iter()
            .filter(|edge| edge.endpoints.iter().copied().any(same_point))
            .map(|edge| edge.source_edge)
            .collect::<Vec<_>>();
        assert_eq!(target_edges.len(), 3);
        let request = ExecuteRequest {
            command: KernelCommand::FinishEdges {
                target_edges,
                kind: artificer_protocol::EdgeFinishKind::Chamfer,
                distance: 0.2,
            },
            request_id: RequestId::new("transformed-three-edge-chamfer"),
            ..request(transformed.snapshot.id())
        };
        let chamfered =
            NativeKernel::execute(&transformed.snapshot, &request, &CancellationToken::new())
                .expect("transformed corner chamfer");
        assert!(NativeKernel::validate(&chamfered.snapshot, ValidationProfile::Solid).valid);
        assert!(chamfered.snapshot.measures().volume < transformed.snapshot.measures().volume);
    }

    fn cuboid_at(origin: [f64; 3], size: [f64; 3]) -> ExecutionOutcome {
        let input = NativeKernel::empty();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("boolean-cuboid"),
            expected_snapshot: input.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: ProtocolPoint3::new(origin[0], origin[1], origin[2]),
                size_x: size[0],
                size_y: size[1],
                size_z: size[2],
            },
        };
        NativeKernel::execute(&input, &request, &CancellationToken::new()).expect("cuboid")
    }

    fn boolean_request(
        target: &Snapshot,
        tool: &Snapshot,
        operation: BooleanOperation,
    ) -> BooleanRequest {
        BooleanRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("boolean-test"),
            expected_target_snapshot: target.id(),
            expected_tool_snapshot: tool.id(),
            precision: PrecisionPolicy::default(),
            operation,
        }
    }

    #[test]
    fn a_curved_boolean_says_whether_its_carriers_can_be_intersected_at_all() {
        // Both refusals are "unsupported", but they mean different things: one
        // is a domain limit of the curve vocabulary, the other is a stage that
        // has not been written yet. The diagnostic has to distinguish them.
        let cuboid = cuboid_at([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]).snapshot;
        let upright = NativeKernel::execute(
            &NativeKernel::empty(),
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("boolean-cylinder"),
                expected_snapshot: NativeKernel::empty().id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeRevolvedAnnulus {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::new(2.0, 2.0, 0.0),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    inner_radius: 0.0,
                    outer_radius: 1.0,
                    height: 20.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("an upright cylinder")
        .snapshot;

        // The full-height cylinder pierces the cuboid, so the prism path
        // publishes the exact drilled solid rather than refusing.
        let drilled = NativeKernel::execute_boolean(
            &cuboid,
            &upright,
            &boolean_request(&cuboid, &upright, BooleanOperation::Difference),
            &CancellationToken::new(),
        )
        .expect("a through drill publishes via the exact prism path")
        .snapshot;
        assert!(NativeKernel::validate(&drilled, ValidationProfile::Solid).valid);
        let expected = 10.0f64.mul_add(100.0, -(std::f64::consts::PI * 10.0));
        assert!(
            ((drilled.measures().volume - expected) / expected).abs() < 1.0e-9,
            "drilled volume {} should equal {expected}",
            drilled.measures().volume
        );

        // A cylinder floating entirely inside the cuboid carves a closed
        // cavity, carried as an inner shell of the solid.
        let blind = NativeKernel::execute(
            &NativeKernel::empty(),
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("boolean-blind-cylinder"),
                expected_snapshot: NativeKernel::empty().id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeRevolvedAnnulus {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::new(2.0, 2.0, 4.0),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    inner_radius: 0.0,
                    outer_radius: 1.0,
                    height: 3.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("a blind cylinder")
        .snapshot;
        let hollowed = NativeKernel::execute_boolean(
            &cuboid,
            &blind,
            &boolean_request(&cuboid, &blind, BooleanOperation::Difference),
            &CancellationToken::new(),
        )
        .expect("an interior cylinder carves a cavity")
        .snapshot;
        assert!(NativeKernel::validate(&hollowed, ValidationProfile::Solid).valid);
        assert_eq!(hollowed.counts().shells, 2);
        let expected = 1000.0f64.mul_add(1.0, -(std::f64::consts::PI * 3.0));
        assert!(
            ((hollowed.measures().volume - expected) / expected).abs() < 1.0e-9,
            "hollowed volume {} should equal {expected}",
            hollowed.measures().volume
        );

        // Turn the cuboid so no face is perpendicular or parallel to the
        // cylinder's axis, and the pair genuinely leaves the vocabulary: the
        // carriers meet in ellipses.
        let turned = NativeKernel::execute(
            &cuboid,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("boolean-turn"),
                expected_snapshot: cuboid.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::TransformSnapshot {
                    transform: SimilarityTransform3 {
                        translation: ProtocolVector3::new(0.0, 0.0, 0.0),
                        rotation: {
                            let (sin, cos) = (0.35_f64).sin_cos();
                            RotationQuaternion::new(cos, sin, 0.0, 0.0)
                        },
                        uniform_scale: 1.0,
                    },
                },
            },
            &CancellationToken::new(),
        )
        .expect("a rotation is always exact")
        .snapshot;
        let refused = NativeKernel::execute_boolean(
            &turned,
            &upright,
            &boolean_request(&turned, &upright, BooleanOperation::Difference),
            &CancellationToken::new(),
        )
        .expect_err("an oblique plane through a cylinder is an ellipse");
        assert!(
            refused.diagnostics.iter().any(|diagnostic| {
                diagnostic.code.as_str() == "BOOLEAN_SURFACE_PAIR_UNSUPPORTED"
            }),
            "unexpected refusal: {refused:?}"
        );
        // The refusal names the pair rather than the whole operand.
        assert!(
            refused.diagnostics.iter().any(|diagnostic| {
                diagnostic.message.contains("plane") && diagnostic.message.contains("cylinder")
            }),
            "the refusal should name the carrier pair: {refused:?}"
        );
    }

    #[test]
    fn planar_boolean_union_difference_and_intersection_publish_valid_solids() {
        let target = cuboid_at([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let tool = cuboid_at([2.0, 1.0, 1.0], [4.0, 2.0, 2.0]);
        let expected = [
            (BooleanOperation::Union, 72.0),
            (BooleanOperation::Difference, 56.0),
            (BooleanOperation::Intersection, 8.0),
        ];
        for (operation, volume) in expected {
            let outcome = NativeKernel::execute_boolean(
                &target.snapshot,
                &tool.snapshot,
                &boolean_request(&target.snapshot, &tool.snapshot, operation),
                &CancellationToken::new(),
            )
            .expect("boolean result");
            assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
            assert!(
                (outcome.snapshot.measures().volume - volume).abs() < 1.0e-6,
                "{operation:?}: expected {volume}, received {}",
                outcome.snapshot.measures().volume
            );
        }
    }

    #[test]
    fn split_returns_complementary_independent_snapshots() {
        let target = cuboid_at([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let tool = cuboid_at([2.0, 1.0, 1.0], [4.0, 2.0, 2.0]);
        let split = NativeKernel::split_boolean(
            &target.snapshot,
            &tool.snapshot,
            &boolean_request(
                &target.snapshot,
                &tool.snapshot,
                BooleanOperation::Difference,
            ),
            &CancellationToken::new(),
        )
        .expect("split");
        assert!((split.remainder.snapshot.measures().volume - 56.0).abs() < 1.0e-6);
        assert!((split.overlap.snapshot.measures().volume - 8.0).abs() < 1.0e-6);
        assert_ne!(split.remainder.snapshot.id(), split.overlap.snapshot.id());
    }

    #[test]
    fn revolve_hole_and_rib_commands_publish_exact_analytic_features() {
        let empty = NativeKernel::empty();
        let revolve = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("revolve-annulus"),
            expected_snapshot: empty.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeRevolvedAnnulus {
                frame: PlanarFrame3::new(
                    ProtocolPoint3::default(),
                    ProtocolVector3::new(1.0, 0.0, 0.0),
                    ProtocolVector3::new(0.0, 1.0, 0.0),
                ),
                inner_radius: 1.0,
                outer_radius: 2.0,
                height: 3.0,
            },
        };
        let revolved = NativeKernel::execute(&empty, &revolve, &CancellationToken::new())
            .expect("revolved annulus");
        assert!((revolved.snapshot.measures().volume - 9.0 * std::f64::consts::PI).abs() < 1.0e-8);
        assert!(
            revolved
                .snapshot
                .topology
                .faces
                .iter()
                .any(|face| matches!(face.value.surface, Surface::Cylinder(_)))
        );

        let base = cuboid_at([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let support = NativeKernel::debug_scene(&base.snapshot)
            .triangles
            .iter()
            .find_map(|triangle| {
                NativeKernel::planar_face_support(&base.snapshot, triangle.source_face).ok()
            })
            .expect("planar support");
        let hole = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("drill-hole"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::DrillHole {
                target_face: support.face,
                frame: support.frame,
                center: ProtocolPoint2::new(0.0, 0.0),
                diameter: 1.0,
                depth: 10.0,
            },
        };
        let drilled = NativeKernel::execute(&base.snapshot, &hole, &CancellationToken::new())
            .expect("through hole");
        assert!(drilled.snapshot.measures().volume < 64.0);
        let drilled_scene = NativeKernel::debug_scene(&drilled.snapshot);
        assert!(
            drilled_scene.edges.iter().any(|edge| edge.is_smooth),
            "the analytic half-cylinder seam is a smooth presentation subdivision"
        );

        let rib = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("add-rib"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::AddRib {
                target_face: support.face,
                frame: support.frame,
                start: ProtocolPoint2::new(-0.75, 0.0),
                end: ProtocolPoint2::new(0.75, 0.0),
                thickness: 0.5,
                height: 1.0,
            },
        };
        let ribbed =
            NativeKernel::execute(&base.snapshot, &rib, &CancellationToken::new()).expect("rib");
        assert!(ribbed.snapshot.measures().volume > 64.0);
    }

    #[test]
    fn planar_face_support_publishes_exact_boundary_curves_for_a_drilled_face() {
        let base = cuboid_at([0.0, 0.0, 0.0], [4.0, 4.0, 4.0]);
        let support = NativeKernel::debug_scene(&base.snapshot)
            .triangles
            .iter()
            .find_map(|triangle| {
                NativeKernel::planar_face_support(&base.snapshot, triangle.source_face).ok()
            })
            .expect("planar support");
        assert_eq!(support.boundary_curves.len(), 4);
        assert!(
            support
                .boundary_curves
                .iter()
                .all(|curve| matches!(curve, FaceBoundaryCurve2::Segment { .. })),
            "a cuboid face is bounded by exact line segments"
        );
        // The analytic curves and the sampled polygon describe one loop, so a
        // planar face's segment endpoints are its boundary vertices exactly.
        for (index, curve) in support.boundary_curves.iter().enumerate() {
            let FaceBoundaryCurve2::Segment { endpoints } = curve else {
                unreachable!("cuboid boundary is segments");
            };
            let expected = support.boundary[index];
            assert!((endpoints[0].x - expected.x).abs() < 1.0e-12);
            assert!((endpoints[0].y - expected.y).abs() < 1.0e-12);
        }

        let hole = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("drill-reference-hole"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::DrillHole {
                target_face: support.face,
                frame: support.frame,
                center: ProtocolPoint2::new(0.5, -0.25),
                diameter: 1.5,
                depth: 10.0,
            },
        };
        let drilled = NativeKernel::execute(&base.snapshot, &hole, &CancellationToken::new())
            .expect("through hole");
        let annular = drilled
            .snapshot
            .topology
            .faces
            .iter()
            .find(|face| {
                !face.value.inner_loops.is_empty() && face.value.surface.as_plane().is_some()
            })
            .expect("the drilled face owns the hole loop");
        let annular_ref = entity_ref(drilled.snapshot.id, annular.id.get(), EntityKind::Face);
        let drilled_support = NativeKernel::planar_face_support(&drilled.snapshot, annular_ref)
            .expect("annular face support");
        assert_eq!(drilled_support.inner_boundary_curves.len(), 1);
        let arcs = &drilled_support.inner_boundary_curves[0];
        assert!(!arcs.is_empty());

        let mut swept = 0.0;
        for arc in arcs {
            let FaceBoundaryCurve2::Arc {
                radius, start, end, ..
            } = *arc
            else {
                panic!("a drilled hole is bounded by analytic arcs, received {arc:?}");
            };
            // The exact radius, not a chord fit of the sampled polygon.
            assert!(
                (radius - 0.75).abs() < 1.0e-12,
                "unexpected radius {radius}"
            );
            swept += (end - start).abs();
        }
        assert!(
            (swept - std::f64::consts::TAU).abs() < 1.0e-9,
            "the hole loop closes exactly once, swept {swept}"
        );

        // Every arc's centre is the same point, so a centre snap has one answer.
        let centres = arcs
            .iter()
            .filter_map(|arc| match arc {
                FaceBoundaryCurve2::Arc { center, .. } => Some(*center),
                FaceBoundaryCurve2::Segment { .. } => None,
            })
            .collect::<Vec<_>>();
        for centre in &centres {
            assert!((centre.x - centres[0].x).abs() < 1.0e-12);
            assert!((centre.y - centres[0].y).abs() < 1.0e-12);
        }

        // Sampling the analytic loop reproduces the polygon the same query
        // publishes, which is what lets snapping and fill agree on screen.
        let sampled = &drilled_support.inner_boundaries[0];
        for point in sampled {
            let distance = point
                .x
                .mul_add(1.0, -centres[0].x)
                .hypot(point.y - centres[0].y);
            assert!(
                (distance - 0.75).abs() < 1.0e-9,
                "sampled hole point is off the analytic circle by {}",
                (distance - 0.75).abs()
            );
        }
    }

    #[test]
    fn mirror_and_linear_pattern_preserve_valid_planar_solids() {
        let base = cuboid_at([0.0, 0.0, 0.0], [2.0, 3.0, 4.0]);
        let mirror = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("mirror"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MirrorSnapshot {
                plane_origin: ProtocolPoint3::default(),
                plane_normal: ProtocolVector3::new(1.0, 0.0, 0.0),
            },
        };
        let mirrored = NativeKernel::execute(&base.snapshot, &mirror, &CancellationToken::new())
            .expect("mirror");
        assert!((mirrored.snapshot.measures().volume - 24.0).abs() < 1.0e-8);
        assert!(mirrored.snapshot.measures().centroid.unwrap().x < 0.0);

        let pattern = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("linear-pattern"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::LinearPatternSnapshot {
                direction: ProtocolVector3::new(1.0, 0.0, 0.0),
                spacing: 5.0,
                count: 3,
            },
        };
        let patterned = NativeKernel::execute(&base.snapshot, &pattern, &CancellationToken::new())
            .expect("pattern");
        assert_eq!(patterned.snapshot.counts().solids, 3);
        assert!((patterned.snapshot.measures().volume - 72.0).abs() < 1.0e-7);
    }

    fn transform_request(input: &Snapshot, transform: SimilarityTransform3) -> ExecuteRequest {
        ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("kernel-transform-test"),
            expected_snapshot: input.id(),
            precision: input.precision_policy().unwrap_or_default(),
            command: KernelCommand::TransformSnapshot { transform },
        }
    }

    fn execute_transform(input: &Snapshot, transform: SimilarityTransform3) -> ExecutionOutcome {
        NativeKernel::execute(
            input,
            &transform_request(input, transform),
            &CancellationToken::new(),
        )
        .unwrap()
    }

    fn extrusion_request(
        input: &Snapshot,
        vertices: Vec<ProtocolPoint2>,
        distance: f64,
    ) -> ExecuteRequest {
        ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("kernel-extrusion-test"),
            expected_snapshot: input.id(),
            precision: input.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudePolygon {
                frame: PlanarFrame3::new(
                    ProtocolPoint3::default(),
                    ProtocolVector3::new(2.0, 0.0, 0.0),
                    ProtocolVector3::new(0.0, 3.0, 0.0),
                ),
                vertices,
                distance,
            },
        }
    }

    fn rectangle_profile() -> Vec<ProtocolPoint2> {
        vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(2.0, 0.0),
            ProtocolPoint2::new(2.0, 3.0),
            ProtocolPoint2::new(0.0, 3.0),
        ]
    }

    fn linear_region(outer: &[ProtocolPoint2], holes: &[Vec<ProtocolPoint2>]) -> PlanarRegion2 {
        PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(outer),
            holes: holes
                .iter()
                .map(|hole| PlanarLoop2::from_polygon(hole))
                .collect(),
        }
    }

    fn face_feature_request(
        input: &Snapshot,
        operation: FaceExtrusionOperation,
    ) -> (ExecuteRequest, EntityRef) {
        let target_face = NativeKernel::debug_scene(input)
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .expect("canonical positive-Z face")
            .source_face;
        let support =
            NativeKernel::planar_face_support(input, target_face).expect("canonical face support");
        (
            ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new(match operation {
                    FaceExtrusionOperation::Add => "kernel-face-feature-add-history",
                    FaceExtrusionOperation::Cut => "kernel-face-feature-cut-history",
                }),
                expected_snapshot: input.id(),
                precision: input.precision_policy().unwrap_or_default(),
                command: KernelCommand::ExtrudeFaceProfile {
                    target_face,
                    frame: support.frame,
                    vertices: vec![
                        ProtocolPoint2::new(-0.5, -0.75),
                        ProtocolPoint2::new(0.5, -0.75),
                        ProtocolPoint2::new(0.5, 0.75),
                        ProtocolPoint2::new(-0.5, 0.75),
                    ],
                    distance: 1.0,
                    operation,
                },
            },
            target_face,
        )
    }

    fn topology_entity_refs(snapshot: &Snapshot) -> BTreeSet<EntityRef> {
        let mut references = BTreeSet::new();
        let mut add = |kind, id| {
            references.insert(entity_ref(snapshot.id, id, kind));
        };
        for record in &snapshot.topology.vertices {
            add(EntityKind::Vertex, record.id.get());
        }
        for record in &snapshot.topology.edges {
            add(EntityKind::Edge, record.id.get());
        }
        for record in &snapshot.topology.coedges {
            add(EntityKind::Coedge, record.id.get());
        }
        for record in &snapshot.topology.loops {
            add(EntityKind::Loop, record.id.get());
        }
        for record in &snapshot.topology.faces {
            add(EntityKind::Face, record.id.get());
        }
        for record in &snapshot.topology.shells {
            add(EntityKind::Shell, record.id.get());
        }
        for record in &snapshot.topology.solids {
            add(EntityKind::Solid, record.id.get());
        }
        references
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected:.17e}, got {actual:.17e}"
        );
    }

    fn assert_point_close(actual: ProtocolPoint3, expected: ProtocolPoint3) {
        assert_close(actual.x, expected.x);
        assert_close(actual.y, expected.y);
        assert_close(actual.z, expected.z);
    }

    #[test]
    fn cuboid_is_a_valid_closed_brep_with_exact_measures() {
        let outcome = canonical();
        assert_eq!(
            outcome.snapshot.counts(),
            ProtocolTopologyCounts {
                vertices: 8,
                edges: 12,
                coedges: 24,
                loops: 6,
                faces: 6,
                shells: 1,
                solids: 1,
            }
        );
        let measures = outcome.snapshot.measures();
        assert_eq!(measures.surface_area, 52.0);
        assert_eq!(measures.volume, 24.0);
        assert_eq!(measures.centroid, Some(ProtocolPoint3::new(1.0, 1.5, 2.0)));
        assert_eq!(
            measures.bounds,
            Some(Aabb3::new(
                ProtocolPoint3::new(0.0, 0.0, 0.0),
                ProtocolPoint3::new(2.0, 3.0, 4.0),
            ))
        );
        assert!(outcome.report.validation.valid);
        assert_eq!(outcome.report.history.len(), 58);
    }

    #[test]
    fn debug_scene_is_complete_and_source_mapped() {
        let outcome = canonical();
        let scene = NativeKernel::debug_scene(&outcome.snapshot);
        assert_eq!(scene.triangles.len(), 12);
        assert_eq!(scene.edges.len(), 12);
        assert_eq!(scene.vertices.len(), 8);
        assert!(
            scene
                .triangles
                .iter()
                .all(
                    |triangle| triangle.source_face.snapshot == outcome.snapshot.id()
                        && triangle.source_face.kind == EntityKind::Face
                )
        );
        assert!(scene.vertices.iter().all(|vertex| {
            vertex.source_vertex.snapshot == outcome.snapshot.id()
                && vertex.source_vertex.kind == EntityKind::Vertex
        }));

        let edge = scene.edges[0].source_edge;
        assert!(NativeKernel::edge_length(&outcome.snapshot, edge).unwrap() > 0.0);
        let areas = scene
            .triangles
            .iter()
            .map(|triangle| triangle.source_face)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|face| NativeKernel::face_area(&outcome.snapshot, face).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(areas.len(), 6);
        assert_eq!(areas.iter().sum::<f64>(), 52.0);
        assert!(
            scene
                .edges
                .iter()
                .all(|edge| edge.source_edge.snapshot == outcome.snapshot.id()
                    && edge.source_edge.kind == EntityKind::Edge)
        );
    }

    #[test]
    fn face_feature_history_is_semantic_complete_and_mode_specific() {
        for operation in [FaceExtrusionOperation::Add, FaceExtrusionOperation::Cut] {
            let input = canonical().snapshot;
            let (request, target_face) = face_feature_request(&input, operation);
            let outcome = NativeKernel::execute(&input, &request, &CancellationToken::new())
                .expect("supported face feature");
            let history = &outcome.report.history;

            assert_eq!(history.len(), outcome.snapshot.counts().total() as usize);
            assert!(history.iter().all(|record| record.outputs.len() == 1));
            assert!(history.iter().all(|record| match record.relation {
                HistoryRelation::Generated => record.inputs.is_empty(),
                HistoryRelation::Unchanged | HistoryRelation::Modified => {
                    record.inputs.len() == 1
                }
                HistoryRelation::Deleted => false,
            }));

            let output_references = history
                .iter()
                .flat_map(|record| record.outputs.iter().copied())
                .collect::<Vec<_>>();
            assert_eq!(
                output_references.iter().copied().collect::<BTreeSet<_>>(),
                topology_entity_refs(&outcome.snapshot)
            );
            assert_eq!(
                output_references.len(),
                output_references
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .len(),
                "every output must be covered exactly once"
            );
            assert_eq!(
                history
                    .iter()
                    .flat_map(|record| record.inputs.iter().copied())
                    .collect::<BTreeSet<_>>(),
                topology_entity_refs(&input),
                "every input must participate in semantic history"
            );

            let relation_count = |relation| {
                history
                    .iter()
                    .filter(|record| record.relation == relation)
                    .count()
            };
            assert_eq!(relation_count(HistoryRelation::Unchanged), 50);
            assert_eq!(relation_count(HistoryRelation::Modified), 8);
            assert_eq!(relation_count(HistoryRelation::Generated), 55);
            assert_eq!(relation_count(HistoryRelation::Deleted), 0);
            let kind_relation_count = |kind, relation| {
                history
                    .iter()
                    .filter(|record| record.relation == relation && record.outputs[0].kind == kind)
                    .count()
            };
            for (kind, unchanged, modified, generated) in [
                (EntityKind::Vertex, 8, 0, 8),
                (EntityKind::Edge, 12, 0, 12),
                (EntityKind::Coedge, 20, 4, 24),
                (EntityKind::Loop, 5, 1, 6),
                (EntityKind::Face, 5, 1, 5),
                (EntityKind::Shell, 0, 1, 0),
                (EntityKind::Solid, 0, 1, 0),
            ] {
                assert_eq!(
                    kind_relation_count(kind, HistoryRelation::Unchanged),
                    unchanged,
                    "unexpected unchanged {kind} count"
                );
                assert_eq!(
                    kind_relation_count(kind, HistoryRelation::Modified),
                    modified,
                    "unexpected modified {kind} count"
                );
                assert_eq!(
                    kind_relation_count(kind, HistoryRelation::Generated),
                    generated,
                    "unexpected generated {kind} count"
                );
            }

            let target_patches = history
                .iter()
                .filter(|record| {
                    record.relation == HistoryRelation::Modified
                        && record.inputs.as_slice() == [target_face]
                        && record.outputs[0].kind == EntityKind::Face
                })
                .collect::<Vec<_>>();
            assert_eq!(target_patches.len(), 1);
            assert!(target_patches.iter().all(|record| {
                record
                    .role
                    .as_ref()
                    .is_some_and(|role| role.name == "face_extrude.target_face_patch")
            }));
            let target_record = input
                .topology
                .faces
                .iter()
                .find(|record| record.id.get() == target_face.entity.0)
                .expect("target face record");
            let target_loop = input
                .topology
                .loop_record(target_record.value.outer_loop)
                .expect("target loop record");
            let target_loop_ref = entity_ref(input.id, target_loop.id.get(), EntityKind::Loop);
            assert_eq!(
                history
                    .iter()
                    .filter(|record| {
                        record.relation == HistoryRelation::Modified
                            && record.inputs.as_slice() == [target_loop_ref]
                            && record.outputs[0].kind == EntityKind::Loop
                    })
                    .count(),
                1
            );
            let target_coedges = target_loop
                .value
                .coedges
                .iter()
                .map(|key| {
                    let record = input.topology.coedge(*key).expect("target coedge");
                    entity_ref(input.id, record.id.get(), EntityKind::Coedge)
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(
                history
                    .iter()
                    .filter(|record| {
                        record.relation == HistoryRelation::Modified
                            && record.outputs[0].kind == EntityKind::Coedge
                    })
                    .flat_map(|record| record.inputs.iter().copied())
                    .collect::<BTreeSet<_>>(),
                target_coedges
            );
            assert_eq!(
                history
                    .iter()
                    .filter(|record| {
                        record.relation == HistoryRelation::Unchanged
                            && record.outputs[0].kind == EntityKind::Face
                    })
                    .count(),
                5
            );
            assert!(history.iter().any(|record| {
                record.relation == HistoryRelation::Modified
                    && record.inputs[0].kind == EntityKind::Shell
                    && record.outputs[0].kind == EntityKind::Shell
            }));
            assert!(history.iter().any(|record| {
                record.relation == HistoryRelation::Modified
                    && record.inputs[0].kind == EntityKind::Solid
                    && record.outputs[0].kind == EntityKind::Solid
            }));

            let (end_role, side_role) = match operation {
                FaceExtrusionOperation::Add => {
                    ("face_extrude.boss.end_face", "face_extrude.boss.side_face")
                }
                FaceExtrusionOperation::Cut => (
                    "face_extrude.pocket.floor_face",
                    "face_extrude.pocket.wall_face",
                ),
            };
            let generated_face_roles = history
                .iter()
                .filter(|record| {
                    record.relation == HistoryRelation::Generated
                        && record.outputs[0].kind == EntityKind::Face
                })
                .filter_map(|record| record.role.as_ref())
                .collect::<Vec<_>>();
            assert_eq!(
                generated_face_roles
                    .iter()
                    .filter(|role| role.name == end_role)
                    .count(),
                1
            );
            assert_eq!(
                generated_face_roles
                    .iter()
                    .filter(|role| role.name == side_role)
                    .count(),
                4
            );
            assert_eq!(
                generated_face_roles
                    .iter()
                    .filter(|role| role.name == side_role)
                    .filter_map(|role| role.ordinal)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([0, 1, 2, 3])
            );
        }
    }

    #[test]
    fn public_circle_face_add_blind_and_through_cut_are_exact_and_source_mapped() {
        for (operation, distance, volume, surface) in [
            (
                FaceExtrusionOperation::Add,
                1.0,
                24.0 + 0.25 * std::f64::consts::PI,
                52.0 + std::f64::consts::PI,
            ),
            (
                FaceExtrusionOperation::Cut,
                1.0,
                24.0 - 0.25 * std::f64::consts::PI,
                52.0 + std::f64::consts::PI,
            ),
            (
                FaceExtrusionOperation::Cut,
                10.0,
                24.0 - std::f64::consts::PI,
                52.0 + 3.5 * std::f64::consts::PI,
            ),
        ] {
            let input = canonical().snapshot;
            let target_record = input
                .topology
                .faces
                .iter()
                .find(|face| face.value.role == FaceRole::PositiveZ)
                .expect("positive Z face");
            let target = entity_ref(input.id(), target_record.id.get(), EntityKind::Face);
            let profile = PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: ProtocolPoint2::new(1.0, 1.5),
                            radius: 0.5,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: Vec::new(),
                }],
            };
            let outcome = NativeKernel::execute(
                &input,
                &ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new("circle-face-feature"),
                    expected_snapshot: input.id(),
                    precision: input.precision_policy().unwrap(),
                    command: KernelCommand::ExtrudeFacePlanarProfile {
                        target_face: target,
                        frame: PlanarFrame3 {
                            origin: ProtocolPoint3::new(0.0, 0.0, 4.0),
                            u: ProtocolVector3::new(1.0, 0.0, 0.0),
                            v: ProtocolVector3::new(0.0, 1.0, 0.0),
                        },
                        profile: profile.clone(),
                        distance,
                        operation,
                    },
                },
                &CancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("{operation:?}: {error:#?}"));
            assert_close(outcome.snapshot.measures().volume, volume);
            assert_close(outcome.snapshot.measures().surface_area, surface);
            assert!(outcome.report.validation.valid);
            assert_eq!(
                outcome.report.history.len(),
                outcome.snapshot.counts().total() as usize
            );
            let scene = NativeKernel::debug_scene(&outcome.snapshot);
            assert!(
                scene
                    .triangles
                    .iter()
                    .any(|triangle| { matches!(triangle.role, FaceRole::FeatureSide(_)) })
            );
            let curved_source_counts =
                scene
                    .edges
                    .iter()
                    .fold(BTreeMap::<EntityRef, usize>::new(), |mut counts, edge| {
                        *counts.entry(edge.source_edge).or_default() += 1;
                        counts
                    });
            assert!(curved_source_counts.values().any(|count| *count > 4));

            let target_output = outcome
                .snapshot
                .topology
                .faces
                .iter()
                .find(|face| face.value.role == FaceRole::PositiveZ)
                .expect("target survives");
            let support = NativeKernel::planar_face_support(
                &outcome.snapshot,
                entity_ref(
                    outcome.snapshot.id(),
                    target_output.id.get(),
                    EntityKind::Face,
                ),
            )
            .expect("analytic shoulder remains sketch support");
            assert_eq!(support.inner_boundaries.len(), 1);
            assert!(support.inner_boundaries[0].len() > 8);
        }

        let input = canonical().snapshot;
        let digest = input.semantic_digest();
        let target_record = input
            .topology
            .faces
            .iter()
            .find(|face| face.value.role == FaceRole::PositiveZ)
            .unwrap();
        let rejected = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("circle-face-rejected"),
                expected_snapshot: input.id(),
                precision: input.precision_policy().unwrap(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: entity_ref(input.id(), target_record.id.get(), EntityKind::Face),
                    frame: PlanarFrame3 {
                        origin: ProtocolPoint3::new(0.0, 0.0, 4.0),
                        u: ProtocolVector3::new(1.0, 0.0, 0.0),
                        v: ProtocolVector3::new(0.0, 1.0, 0.0),
                    },
                    profile: PlanarProfile2 {
                        regions: vec![PlanarRegion2 {
                            outer: PlanarLoop2 {
                                curves: vec![PlanarCurve2::Circle {
                                    // Fully disjoint from the face: no
                                    // interface exists, so even the
                                    // boundary-crossing path must refuse.
                                    center: ProtocolPoint2::new(30.0, 30.0),
                                    radius: 10.0,
                                    direction: ArcDirection::CounterClockwise,
                                }],
                            },
                            holes: Vec::new(),
                        }],
                    },
                    distance: 1.0,
                    operation: FaceExtrusionOperation::Add,
                },
            },
            &CancellationToken::new(),
        );
        assert!(rejected.is_err());
        assert_eq!(input.semantic_digest(), digest);
    }

    #[test]
    fn invalid_extent_is_rejected_without_mutating_input() {
        let input = NativeKernel::empty();
        let mut invalid = request(input.id());
        let KernelCommand::MakeCuboid { ref mut size_x, .. } = invalid.command else {
            unreachable!("request helper always constructs a cuboid")
        };
        *size_x = 0.0;
        let error = NativeKernel::execute(&input, &invalid, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.code, KernelErrorCode::InvalidInput);
        assert_eq!(input.id(), SnapshotId::ZERO);
        assert_eq!(input.counts().total(), 0);
    }

    #[test]
    fn stale_snapshot_and_cancellation_fail_before_commit() {
        let input = NativeKernel::empty();
        let mut stale = request(SnapshotId::new([7; 16]));
        let error = NativeKernel::execute(&input, &stale, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.code, KernelErrorCode::StaleSnapshot);

        stale.expected_snapshot = input.id();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = NativeKernel::execute(&input, &stale, &cancellation).unwrap_err();
        assert_eq!(error.code, KernelErrorCode::Cancelled);
    }

    #[test]
    fn construction_is_deterministic() {
        let first = canonical();
        for _ in 0..100 {
            let next = canonical();
            assert_eq!(next.snapshot.id(), first.snapshot.id());
            assert_eq!(
                next.snapshot.semantic_digest(),
                first.snapshot.semantic_digest()
            );
            assert_eq!(next.report, first.report);
        }
    }

    #[test]
    fn committed_similarity_preserves_topology_and_transforms_measures() {
        let before = canonical().snapshot;
        let transform = SimilarityTransform3 {
            translation: ProtocolVector3::new(10.0, -5.0, 2.0),
            rotation: RotationQuaternion::new(
                std::f64::consts::FRAC_1_SQRT_2,
                0.0,
                0.0,
                std::f64::consts::FRAC_1_SQRT_2,
            ),
            uniform_scale: 2.0,
        };
        let outcome = execute_transform(&before, transform);

        assert_ne!(outcome.snapshot.id(), before.id());
        assert_ne!(outcome.snapshot.semantic_digest(), before.semantic_digest());
        assert_eq!(outcome.snapshot.counts(), before.counts());
        assert!(outcome.report.validation.valid);
        let measures = outcome.snapshot.measures();
        let bounds = measures.bounds.unwrap();
        assert_point_close(bounds.min, ProtocolPoint3::new(4.0, -5.0, 2.0));
        assert_point_close(bounds.max, ProtocolPoint3::new(10.0, -1.0, 10.0));
        assert_close(measures.surface_area, 208.0);
        assert_close(measures.volume, 192.0);
        assert_point_close(
            measures.centroid.unwrap(),
            ProtocolPoint3::new(7.0, -3.0, 6.0),
        );

        assert_eq!(outcome.report.history.len(), 58);
        assert!(outcome.report.history.iter().all(|record| {
            record.relation == HistoryRelation::Modified
                && record.inputs.len() == 1
                && record.outputs.len() == 1
                && record.inputs[0].snapshot == before.id()
                && record.outputs[0].snapshot == outcome.snapshot.id()
                && record.inputs[0].kind == record.outputs[0].kind
                && record.inputs[0].entity == record.outputs[0].entity
        }));

        let scene = NativeKernel::debug_scene(&outcome.snapshot);
        assert_eq!(scene.snapshot, outcome.snapshot.id());
        assert_eq!(scene.triangles.len(), 12);
        assert_eq!(scene.edges.len(), 12);
    }

    #[test]
    fn committed_similarity_transforms_a_multi_solid_body_group_atomically() {
        let input = NativeKernel::empty();
        let first = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(1.0, 0.0),
            ProtocolPoint2::new(1.0, 1.0),
            ProtocolPoint2::new(0.0, 1.0),
        ];
        let second = vec![
            ProtocolPoint2::new(3.0, 0.0),
            ProtocolPoint2::new(4.0, 0.0),
            ProtocolPoint2::new(4.0, 1.0),
            ProtocolPoint2::new(3.0, 1.0),
        ];
        let compound = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("multi-solid-transform-base"),
                expected_snapshot: input.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::default(),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    profile: PlanarProfile2 {
                        regions: vec![linear_region(&first, &[]), linear_region(&second, &[])],
                    },
                    distance: 2.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("two-solid body group");
        let before = compound.snapshot;

        let outcome = execute_transform(
            &before,
            SimilarityTransform3 {
                translation: ProtocolVector3::new(10.0, -2.0, 1.0),
                uniform_scale: 2.0,
                ..SimilarityTransform3::identity()
            },
        );

        assert_eq!(outcome.snapshot.counts(), before.counts());
        assert_eq!(outcome.snapshot.counts().solids, 2);
        assert_close(outcome.snapshot.measures().volume, 32.0);
        assert_point_close(
            outcome.snapshot.measures().centroid.unwrap(),
            ProtocolPoint3::new(14.0, -1.0, 3.0),
        );
        assert_eq!(
            outcome.report.history.len(),
            outcome.snapshot.counts().total() as usize
        );
        assert_eq!(
            outcome
                .report
                .history
                .iter()
                .filter(|record| record.outputs[0].kind == EntityKind::Solid)
                .count(),
            2
        );
    }

    #[test]
    fn transform_rejects_a_circle_whose_unsampled_carrier_crosses_the_coordinate_limit() {
        let input = NativeKernel::empty();
        let precision = PrecisionPolicy::default();
        let disk = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("analytic-transform-envelope-base"),
                expected_snapshot: input.id(),
                precision,
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::default(),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    profile: PlanarProfile2 {
                        regions: vec![PlanarRegion2 {
                            outer: PlanarLoop2 {
                                curves: vec![PlanarCurve2::Circle {
                                    center: ProtocolPoint2::new(0.0, 0.0),
                                    radius: 1.0,
                                    direction: ArcDirection::CounterClockwise,
                                }],
                            },
                            holes: Vec::new(),
                        }],
                    },
                    distance: 1.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("exact analytic disk");
        let before_digest = disk.snapshot.semantic_digest();

        let error = NativeKernel::execute(
            &disk.snapshot,
            &transform_request(
                &disk.snapshot,
                SimilarityTransform3 {
                    translation: ProtocolVector3::new(0.0, precision.max_abs_coordinate - 0.5, 0.0),
                    ..SimilarityTransform3::identity()
                },
            ),
            &CancellationToken::new(),
        )
        .expect_err("the exact circle carrier crosses the coordinate envelope");

        assert_eq!(error.code, KernelErrorCode::ResourceLimitExceeded);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "TRANSFORM_COORDINATE_LIMIT_EXCEEDED"
        );
        assert_eq!(disk.snapshot.semantic_digest(), before_digest);
    }

    #[test]
    fn convex_rectangle_extrudes_to_a_valid_watertight_prism() {
        let input = NativeKernel::empty();
        let outcome = NativeKernel::execute(
            &input,
            &extrusion_request(&input, rectangle_profile(), 4.0),
            &CancellationToken::new(),
        )
        .unwrap();

        assert_eq!(
            outcome.snapshot.counts(),
            ProtocolTopologyCounts {
                vertices: 8,
                edges: 12,
                coedges: 24,
                loops: 6,
                faces: 6,
                shells: 1,
                solids: 1,
            }
        );
        assert!(outcome.report.validation.valid);
        assert_eq!(outcome.snapshot.measures().surface_area, 52.0);
        assert_eq!(outcome.snapshot.measures().volume, 24.0);
        assert_eq!(
            outcome.snapshot.measures().centroid,
            Some(ProtocolPoint3::new(1.0, 1.5, 2.0))
        );
        assert_eq!(
            outcome.snapshot.measures().bounds,
            Some(Aabb3::new(
                ProtocolPoint3::new(0.0, 0.0, 0.0),
                ProtocolPoint3::new(2.0, 3.0, 4.0),
            ))
        );
        assert_eq!(
            outcome
                .snapshot
                .topology
                .faces
                .iter()
                .map(|face| face.value.role)
                .collect::<Vec<_>>(),
            vec![
                FaceRole::ExtrusionBottom,
                FaceRole::ExtrusionTop,
                FaceRole::ExtrusionSide(0),
                FaceRole::ExtrusionSide(1),
                FaceRole::ExtrusionSide(2),
                FaceRole::ExtrusionSide(3),
            ]
        );

        let scene = NativeKernel::debug_scene(&outcome.snapshot);
        assert_eq!(scene.triangles.len(), 12);
        assert_eq!(scene.edges.len(), 12);
        for edge in &scene.edges {
            let adjacent_faces = scene
                .triangles
                .iter()
                .filter(|triangle| {
                    edge.endpoints
                        .iter()
                        .all(|endpoint| triangle.vertices.contains(endpoint))
                })
                .map(|triangle| triangle.source_face)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                adjacent_faces.len(),
                2,
                "debug edge {:?} was not reused bit-identically by two faces",
                edge.source_edge
            );
        }

        assert_eq!(outcome.report.history.len(), 58);
        assert!(outcome.report.history.iter().all(|record| {
            record.relation == HistoryRelation::Generated
                && record.inputs.is_empty()
                && record.outputs.len() == 1
                && record.outputs[0].snapshot == outcome.snapshot.id()
                && record
                    .role
                    .as_ref()
                    .is_some_and(|role| role.name.starts_with("extrude."))
        }));
        let expected_outputs =
            outcome
                .snapshot
                .topology
                .vertices
                .iter()
                .map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Vertex)
                })
                .chain(outcome.snapshot.topology.edges.iter().map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Edge)
                }))
                .chain(outcome.snapshot.topology.coedges.iter().map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Coedge)
                }))
                .chain(outcome.snapshot.topology.loops.iter().map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Loop)
                }))
                .chain(outcome.snapshot.topology.faces.iter().map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Face)
                }))
                .chain(outcome.snapshot.topology.shells.iter().map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Shell)
                }))
                .chain(outcome.snapshot.topology.solids.iter().map(|record| {
                    entity_ref(outcome.snapshot.id(), record.id.get(), EntityKind::Solid)
                }))
                .collect::<std::collections::BTreeSet<_>>();
        let actual_outputs = outcome
            .report
            .history
            .iter()
            .map(|record| record.outputs[0])
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual_outputs.len(), outcome.report.history.len());
        assert_eq!(actual_outputs, expected_outputs);
        assert_eq!(
            outcome
                .report
                .history
                .iter()
                .map(|record| record.role.clone().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            outcome.report.history.len(),
            "generated extrusion roles must identify each live output uniquely"
        );
        for (name, ordinal) in [
            ("extrude.bottom_face", None),
            ("extrude.top_face", None),
            ("extrude.side_face", Some(0)),
            ("extrude.side_face", Some(1)),
            ("extrude.side_face", Some(2)),
            ("extrude.side_face", Some(3)),
        ] {
            assert!(outcome.report.history.iter().any(|record| {
                record.outputs[0].kind == EntityKind::Face
                    && record
                        .role
                        .as_ref()
                        .is_some_and(|role| role.name == name && role.ordinal == ordinal)
            }));
        }
    }

    #[test]
    fn asymmetric_convex_extrusions_have_closed_form_measures() {
        let input = NativeKernel::empty();
        let cases = [
            (
                vec![
                    ProtocolPoint2::new(0.0, 0.0),
                    ProtocolPoint2::new(4.0, 0.0),
                    ProtocolPoint2::new(0.0, 3.0),
                ],
                2.0,
                6.0,
                12.0,
                ProtocolPoint3::new(4.0 / 3.0, 1.0, 1.0),
            ),
            (
                vec![
                    ProtocolPoint2::new(0.0, 0.0),
                    ProtocolPoint2::new(4.0, 0.0),
                    ProtocolPoint2::new(3.0, 2.0),
                    ProtocolPoint2::new(0.0, 2.0),
                ],
                5.0,
                7.0,
                9.0 + 5.0_f64.sqrt(),
                ProtocolPoint3::new(37.0 / 21.0, 20.0 / 21.0, 2.5),
            ),
        ];

        for (profile, distance, area, perimeter, expected_centroid) in cases {
            let outcome = NativeKernel::execute(
                &input,
                &extrusion_request(&input, profile, distance),
                &CancellationToken::new(),
            )
            .unwrap();
            let measures = outcome.snapshot.measures();
            assert!(outcome.report.validation.valid);
            assert_close(measures.surface_area, 2.0 * area + perimeter * distance);
            assert_close(measures.volume, area * distance);
            assert_point_close(measures.centroid.unwrap(), expected_centroid);
        }
    }

    #[test]
    fn clockwise_profile_normalizes_to_the_same_snapshot() {
        let input = NativeKernel::empty();
        let counter_clockwise = NativeKernel::execute(
            &input,
            &extrusion_request(&input, rectangle_profile(), 4.0),
            &CancellationToken::new(),
        )
        .unwrap();
        let clockwise = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(0.0, 3.0),
            ProtocolPoint2::new(2.0, 3.0),
            ProtocolPoint2::new(2.0, 0.0),
        ];
        let clockwise = NativeKernel::execute(
            &input,
            &extrusion_request(&input, clockwise, 4.0),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(clockwise.snapshot.id(), counter_clockwise.snapshot.id());
        assert_eq!(clockwise.report, counter_clockwise.report);

        let shifted = vec![
            ProtocolPoint2::new(2.0, 3.0),
            ProtocolPoint2::new(0.0, 3.0),
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(2.0, 0.0),
        ];
        let shifted = NativeKernel::execute(
            &input,
            &extrusion_request(&input, shifted, 4.0),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(shifted.snapshot.id(), counter_clockwise.snapshot.id());
        assert_eq!(shifted.report, counter_clockwise.report);

        let mut skewed_frame = extrusion_request(&input, rectangle_profile(), 4.0);
        let KernelCommand::ExtrudePolygon { frame, .. } = &mut skewed_frame.command else {
            unreachable!("extrusion request constructed above")
        };
        frame.v = ProtocolVector3::new(1.0, 3.0, 0.0);
        let skewed_frame =
            NativeKernel::execute(&input, &skewed_frame, &CancellationToken::new()).unwrap();
        assert_eq!(skewed_frame.snapshot.id(), counter_clockwise.snapshot.id());
        assert_eq!(skewed_frame.report, counter_clockwise.report);
    }

    #[test]
    fn extrusion_is_equivariant_across_origin_plane_frames() {
        let input = NativeKernel::empty();
        let cases = [
            (
                ProtocolVector3::new(1.0, 0.0, 0.0),
                ProtocolVector3::new(0.0, 1.0, 0.0),
                Aabb3::new(
                    ProtocolPoint3::new(0.0, 0.0, 0.0),
                    ProtocolPoint3::new(2.0, 3.0, 4.0),
                ),
                ProtocolPoint3::new(1.0, 1.5, 2.0),
            ),
            (
                ProtocolVector3::new(0.0, 1.0, 0.0),
                ProtocolVector3::new(0.0, 0.0, 1.0),
                Aabb3::new(
                    ProtocolPoint3::new(0.0, 0.0, 0.0),
                    ProtocolPoint3::new(4.0, 2.0, 3.0),
                ),
                ProtocolPoint3::new(2.0, 1.0, 1.5),
            ),
            (
                ProtocolVector3::new(1.0, 0.0, 0.0),
                ProtocolVector3::new(0.0, 0.0, 1.0),
                Aabb3::new(
                    ProtocolPoint3::new(0.0, -4.0, 0.0),
                    ProtocolPoint3::new(2.0, 0.0, 3.0),
                ),
                ProtocolPoint3::new(1.0, -2.0, 1.5),
            ),
        ];

        for (u, v, expected_bounds, expected_centroid) in cases {
            let mut request = extrusion_request(&input, rectangle_profile(), 4.0);
            let KernelCommand::ExtrudePolygon { frame, .. } = &mut request.command else {
                unreachable!("extrusion request constructed above")
            };
            frame.u = u;
            frame.v = v;
            let outcome = NativeKernel::execute(&input, &request, &CancellationToken::new())
                .expect("all three origin-plane frames must construct the same prism");

            assert!(outcome.report.validation.valid);
            assert_eq!(outcome.snapshot.measures().surface_area, 52.0);
            assert_eq!(outcome.snapshot.measures().volume, 24.0);
            assert_eq!(outcome.snapshot.measures().bounds, Some(expected_bounds));
            assert_eq!(
                outcome.snapshot.measures().centroid,
                Some(expected_centroid)
            );
        }
    }

    #[test]
    fn translated_skew_extrusion_is_preflighted_for_representability() {
        let input = NativeKernel::empty();
        let mut supported = extrusion_request(&input, rectangle_profile(), 4.0);
        let KernelCommand::ExtrudePolygon { frame, .. } = &mut supported.command else {
            unreachable!("extrusion request constructed above")
        };
        frame.origin = ProtocolPoint3::new(1.0e6, -2.0e6, 3.0e6);
        frame.u = ProtocolVector3::new(1.0, 2.0, 3.0);
        frame.v = ProtocolVector3::new(-2.0, 5.0, 1.0);

        let outcome = NativeKernel::execute(&input, &supported, &CancellationToken::new())
            .expect("a representable translated skew frame must construct successfully");
        let measures = outcome.snapshot.measures();
        assert!(outcome.report.validation.valid);
        assert!((measures.surface_area - 52.0).abs() <= 1.0e-8);
        assert!((measures.volume - 24.0).abs() <= 1.0e-8);
        let u_length = 14.0_f64.sqrt();
        let v_length = 4_186.0_f64.sqrt();
        let normal_length = 299.0_f64.sqrt();
        assert_point_close(
            measures.centroid.unwrap(),
            ProtocolPoint3::new(
                1.0e6 + 1.0 / u_length - 1.5 * 39.0 / v_length - 2.0 * 13.0 / normal_length,
                -2.0e6 + 2.0 / u_length + 1.5 * 48.0 / v_length - 2.0 * 7.0 / normal_length,
                3.0e6 + 3.0 / u_length - 1.5 * 19.0 / v_length + 2.0 * 9.0 / normal_length,
            ),
        );

        let KernelCommand::ExtrudePolygon { frame, .. } = &mut supported.command else {
            unreachable!("extrusion request constructed above")
        };
        frame.origin = ProtocolPoint3::new(1.0e8, -2.0e8, 3.0e8);
        let error = NativeKernel::execute(&input, &supported, &CancellationToken::new())
            .expect_err("lossy placement must be rejected before generic topology validation");
        assert_eq!(error.code, KernelErrorCode::NumericallyIndeterminate);
        assert_eq!(error.stage, KernelStage::Preflight);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "EXTRUDE_PRECISION_UNREPRESENTABLE"
        );
        assert_eq!(input.counts().total(), 0);

        let tiny_profile = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(2.0e-5, 0.0),
            ProtocolPoint2::new(2.0e-5, 3.0e-5),
            ProtocolPoint2::new(0.0, 3.0e-5),
        ];
        let mut tiny = extrusion_request(&input, tiny_profile, 4.0e-5);
        let KernelCommand::ExtrudePolygon { frame, .. } = &mut tiny.command else {
            unreachable!("extrusion request constructed above")
        };
        frame.origin = ProtocolPoint3::new(999_999_999.0, 0.0, 0.0);
        frame.u = ProtocolVector3::new(1.0, 0.0, 0.0);
        frame.v = ProtocolVector3::new(0.0, 1.0, 0.0);
        let error = NativeKernel::execute(&input, &tiny, &CancellationToken::new())
            .expect_err("sub-ULP feature placement must not publish distorted measures");
        assert_eq!(error.code, KernelErrorCode::NumericallyIndeterminate);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "EXTRUDE_PRECISION_UNREPRESENTABLE"
        );
        assert_eq!(input.counts().total(), 0);
    }

    #[test]
    fn invalid_degenerate_extrusions_are_transactional() {
        let input = NativeKernel::empty();
        let cases = [
            (
                extrusion_request(
                    &input,
                    vec![ProtocolPoint2::new(0.0, 0.0), ProtocolPoint2::new(2.0, 0.0)],
                    4.0,
                ),
                "EXTRUDE_TOO_FEW_VERTICES",
            ),
            (
                extrusion_request(&input, rectangle_profile(), 0.0),
                "EXTRUDE_DISTANCE_NON_POSITIVE",
            ),
            (
                extrusion_request(
                    &input,
                    vec![
                        ProtocolPoint2::new(0.0, 0.0),
                        ProtocolPoint2::new(1.0e6, 0.0),
                        ProtocolPoint2::new(5.0e5, 1.0e-6),
                    ],
                    4.0,
                ),
                "EXTRUDE_FEATURE_TOO_SMALL",
            ),
        ];
        for (request, diagnostic) in cases {
            let error =
                NativeKernel::execute(&input, &request, &CancellationToken::new()).unwrap_err();
            assert_eq!(error.code, KernelErrorCode::InvalidInput);
            assert_eq!(error.diagnostics[0].code.as_str(), diagnostic);
            assert_eq!(input.id(), SnapshotId::ZERO);
            assert_eq!(input.counts().total(), 0);
        }

        let mut parallel = extrusion_request(&input, rectangle_profile(), 4.0);
        let KernelCommand::ExtrudePolygon { frame, .. } = &mut parallel.command else {
            unreachable!("extrusion request constructed above")
        };
        frame.v = ProtocolVector3::new(4.0, 0.0, 0.0);
        let error =
            NativeKernel::execute(&input, &parallel, &CancellationToken::new()).unwrap_err();
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "EXTRUDE_FRAME_DEGENERATE"
        );

        let too_many = vec![ProtocolPoint2::default(); 257];
        let error = NativeKernel::execute(
            &input,
            &extrusion_request(&input, too_many, 4.0),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, KernelErrorCode::ResourceLimitExceeded);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "EXTRUDE_TOO_MANY_VERTICES"
        );

        let committed = canonical().snapshot;
        let error = NativeKernel::execute(
            &committed,
            &extrusion_request(&committed, rectangle_profile(), 4.0),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, KernelErrorCode::Unsupported);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "EXTRUDE_SOURCE_NOT_EMPTY"
        );
        assert_eq!(committed.counts().solids, 1);
    }

    #[test]
    fn certified_concave_and_collinear_linear_profiles_extrude_transactionally() {
        let input = NativeKernel::empty();
        for (name, profile, area) in [
            (
                "concave",
                vec![
                    ProtocolPoint2::new(0.0, 0.0),
                    ProtocolPoint2::new(3.0, 0.0),
                    ProtocolPoint2::new(1.5, 1.0),
                    ProtocolPoint2::new(3.0, 3.0),
                    ProtocolPoint2::new(0.0, 3.0),
                ],
                6.75,
            ),
            (
                "collinear-segment",
                vec![
                    ProtocolPoint2::new(0.0, 0.0),
                    ProtocolPoint2::new(1.0, 0.0),
                    ProtocolPoint2::new(2.0, 0.0),
                    ProtocolPoint2::new(2.0, 2.0),
                    ProtocolPoint2::new(0.0, 2.0),
                ],
                4.0,
            ),
        ] {
            let request = extrusion_request(&input, profile, 4.0);
            let outcome = NativeKernel::execute(&input, &request, &CancellationToken::new())
                .unwrap_or_else(|error| panic!("{name} extrusion failed: {error}"));
            assert_close(outcome.snapshot.measures().volume, area * 4.0);
            assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
            assert_eq!(input.id(), SnapshotId::ZERO);
            assert_eq!(input.counts().total(), 0);
        }
    }

    #[test]
    fn extrusion_rejects_each_declared_profile_and_transaction_failure_mode() {
        let input = NativeKernel::empty();
        let self_crossing_star = vec![
            ProtocolPoint2::new(0.0, 3.0),
            ProtocolPoint2::new(2.0, -3.0),
            ProtocolPoint2::new(-3.0, 1.0),
            ProtocolPoint2::new(3.0, 1.0),
            ProtocolPoint2::new(-2.0, -3.0),
        ];
        let cases = [
            (
                extrusion_request(&input, self_crossing_star, 4.0),
                KernelErrorCode::InvalidInput,
                "EXTRUDE_PROFILE_SELF_INTERSECTING",
            ),
            (
                extrusion_request(
                    &input,
                    vec![
                        ProtocolPoint2::new(0.0, 0.0),
                        ProtocolPoint2::new(2.0, 0.0),
                        ProtocolPoint2::new(2.0, 2.0),
                        ProtocolPoint2::new(2.0, 0.0),
                    ],
                    4.0,
                ),
                KernelErrorCode::InvalidInput,
                "EXTRUDE_REPEATED_VERTEX",
            ),
            (
                extrusion_request(
                    &input,
                    vec![
                        ProtocolPoint2::new(0.0, 0.0),
                        ProtocolPoint2::new(f64::NAN, 0.0),
                        ProtocolPoint2::new(0.0, 2.0),
                    ],
                    4.0,
                ),
                KernelErrorCode::InvalidInput,
                "EXTRUDE_INPUT_NON_FINITE",
            ),
            (
                extrusion_request(
                    &input,
                    vec![
                        ProtocolPoint2::new(0.0, 0.0),
                        ProtocolPoint2::new(2.0e9, 0.0),
                        ProtocolPoint2::new(0.0, 2.0),
                    ],
                    4.0,
                ),
                KernelErrorCode::ResourceLimitExceeded,
                "EXTRUDE_COORDINATE_LIMIT_EXCEEDED",
            ),
        ];
        for (request, expected_code, diagnostic) in cases {
            let error = NativeKernel::execute(&input, &request, &CancellationToken::new())
                .expect_err("invalid extrusion must not publish a snapshot");
            assert_eq!(error.code, expected_code);
            assert_eq!(error.diagnostics[0].code.as_str(), diagnostic);
            assert_eq!(input.id(), SnapshotId::ZERO);
            assert_eq!(input.counts().total(), 0);
        }

        let mut stale = extrusion_request(&input, rectangle_profile(), 4.0);
        stale.expected_snapshot = SnapshotId::new([9; 16]);
        let error = NativeKernel::execute(&input, &stale, &CancellationToken::new())
            .expect_err("a stale extrusion request must fail before construction");
        assert_eq!(error.code, KernelErrorCode::StaleSnapshot);

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = NativeKernel::execute(
            &input,
            &extrusion_request(&input, rectangle_profile(), 4.0),
            &cancellation,
        )
        .expect_err("a cancelled extrusion request must fail before construction");
        assert_eq!(error.code, KernelErrorCode::Cancelled);
        assert_eq!(input.id(), SnapshotId::ZERO);
        assert_eq!(input.counts().total(), 0);
    }

    #[test]
    fn identity_transform_is_a_content_noop_with_unchanged_history() {
        let before = canonical().snapshot;
        let outcome = execute_transform(&before, SimilarityTransform3::identity());
        assert_eq!(outcome.snapshot.id(), before.id());
        assert_eq!(outcome.snapshot.semantic_digest(), before.semantic_digest());
        assert_eq!(outcome.snapshot.measures(), before.measures());
        assert_eq!(outcome.report.history.len(), 58);
        assert!(
            outcome
                .report
                .history
                .iter()
                .all(|record| record.relation == HistoryRelation::Unchanged)
        );
    }

    #[test]
    fn equivalent_quaternions_produce_identical_commits() {
        let before = canonical().snapshot;
        let make = |factor: f64| SimilarityTransform3 {
            translation: ProtocolVector3::new(3.0, 4.0, 5.0),
            rotation: RotationQuaternion::new(factor, 0.0, 0.0, factor),
            uniform_scale: 1.5,
        };
        let positive = execute_transform(&before, make(1.0));
        let negative = execute_transform(&before, make(-1.0));
        let scaled = execute_transform(&before, make(1.0e300));
        assert_eq!(positive.snapshot.id(), negative.snapshot.id());
        assert_eq!(positive.snapshot.id(), scaled.snapshot.id());
        assert_eq!(positive.report, negative.report);
        assert_eq!(positive.report, scaled.report);
    }

    #[test]
    fn translated_mass_properties_are_conditioned_away_from_world_origin() {
        let before = canonical().snapshot;
        let outcome = execute_transform(
            &before,
            SimilarityTransform3 {
                translation: ProtocolVector3::new(100_000_000.0, -100_000_000.0, 50_000_000.0),
                ..SimilarityTransform3::identity()
            },
        );
        let measures = outcome.snapshot.measures();
        assert_close(measures.surface_area, 52.0);
        assert_close(measures.volume, 24.0);
        assert_point_close(
            measures.centroid.unwrap(),
            ProtocolPoint3::new(100_000_001.0, -99_999_998.5, 50_000_002.0),
        );
    }

    #[test]
    fn malformed_or_unsupported_transforms_never_mutate_the_input() {
        let before = canonical().snapshot;
        let before_scene = NativeKernel::debug_scene(&before);
        let cases = [
            (
                SimilarityTransform3 {
                    uniform_scale: 0.0,
                    ..SimilarityTransform3::identity()
                },
                KernelErrorCode::InvalidInput,
            ),
            (
                SimilarityTransform3 {
                    uniform_scale: -1.0,
                    ..SimilarityTransform3::identity()
                },
                KernelErrorCode::InvalidInput,
            ),
            (
                SimilarityTransform3 {
                    rotation: RotationQuaternion::new(0.0, 0.0, 0.0, 0.0),
                    ..SimilarityTransform3::identity()
                },
                KernelErrorCode::InvalidInput,
            ),
            (
                SimilarityTransform3 {
                    translation: ProtocolVector3::new(f64::NAN, 0.0, 0.0),
                    ..SimilarityTransform3::identity()
                },
                KernelErrorCode::InvalidInput,
            ),
            (
                SimilarityTransform3 {
                    uniform_scale: 1.0e-6,
                    ..SimilarityTransform3::identity()
                },
                KernelErrorCode::InvalidInput,
            ),
            (
                SimilarityTransform3 {
                    translation: ProtocolVector3::new(1.0e9, 0.0, 0.0),
                    ..SimilarityTransform3::identity()
                },
                KernelErrorCode::ResourceLimitExceeded,
            ),
        ];
        for (transform, expected_code) in cases {
            let error = NativeKernel::execute(
                &before,
                &transform_request(&before, transform),
                &CancellationToken::new(),
            )
            .unwrap_err();
            assert_eq!(error.code, expected_code);
            assert_eq!(before.counts(), canonical().snapshot.counts());
            assert_eq!(NativeKernel::debug_scene(&before), before_scene);
        }

        let empty = NativeKernel::empty();
        let error = NativeKernel::execute(
            &empty,
            &transform_request(&empty, SimilarityTransform3::identity()),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, KernelErrorCode::Unsupported);
    }

    #[test]
    fn numerically_unrepresentable_large_placement_is_rejected() {
        let empty = NativeKernel::empty();
        let mut make_tiny = request(empty.id());
        make_tiny.command = KernelCommand::MakeCuboid {
            origin: ProtocolPoint3::default(),
            size_x: 2.0e-5,
            size_y: 3.0e-5,
            size_z: 4.0e-5,
        };
        let tiny = NativeKernel::execute(&empty, &make_tiny, &CancellationToken::new())
            .unwrap()
            .snapshot;
        let error = NativeKernel::execute(
            &tiny,
            &transform_request(
                &tiny,
                SimilarityTransform3 {
                    translation: ProtocolVector3::new(999_999_999.0, 0.0, 0.0),
                    ..SimilarityTransform3::identity()
                },
            ),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, KernelErrorCode::NumericallyIndeterminate);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "TRANSFORM_PRECISION_UNREPRESENTABLE"
        );
    }

    #[test]
    fn linear_planar_region_extrudes_a_watertight_hollow_prism() {
        let input = NativeKernel::empty();
        let outer = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(4.0, 0.0),
            ProtocolPoint2::new(4.0, 4.0),
            ProtocolPoint2::new(0.0, 4.0),
        ];
        let hole = vec![
            ProtocolPoint2::new(1.0, 1.0),
            ProtocolPoint2::new(3.0, 1.0),
            ProtocolPoint2::new(3.0, 3.0),
            ProtocolPoint2::new(1.0, 3.0),
        ];
        let outcome = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("linear-hollow-prism"),
                expected_snapshot: input.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::default(),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    profile: PlanarProfile2 {
                        regions: vec![linear_region(&outer, &[hole])],
                    },
                    distance: 2.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("one exact hollow prism");

        assert_close(outcome.snapshot.measures().volume, 24.0);
        assert_close(outcome.snapshot.measures().surface_area, 72.0);
        assert_point_close(
            outcome.snapshot.measures().centroid.unwrap(),
            ProtocolPoint3::new(2.0, 2.0, 1.0),
        );
        assert_eq!(outcome.snapshot.counts().solids, 1);
        assert_eq!(
            outcome
                .snapshot
                .topology
                .faces
                .iter()
                .filter(|face| !face.value.inner_loops.is_empty())
                .count(),
            2
        );
    }

    #[test]
    fn disjoint_linear_regions_publish_independent_exact_solids() {
        let input = NativeKernel::empty();
        let first = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(1.0, 0.0),
            ProtocolPoint2::new(1.0, 1.0),
            ProtocolPoint2::new(0.0, 1.0),
        ];
        let second = vec![
            ProtocolPoint2::new(3.0, 0.0),
            ProtocolPoint2::new(4.0, 0.0),
            ProtocolPoint2::new(4.0, 1.0),
            ProtocolPoint2::new(3.0, 1.0),
        ];
        let outcome = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("linear-disjoint-regions"),
                expected_snapshot: input.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::default(),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    profile: PlanarProfile2 {
                        regions: vec![linear_region(&first, &[]), linear_region(&second, &[])],
                    },
                    distance: 2.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("two disjoint prisms");

        assert_eq!(outcome.snapshot.counts().solids, 2);
        assert_close(outcome.snapshot.measures().volume, 4.0);
        assert_close(outcome.snapshot.measures().surface_area, 20.0);
        assert_point_close(
            outcome.snapshot.measures().centroid.unwrap(),
            ProtocolPoint3::new(2.0, 0.5, 1.0),
        );

        let reversed = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("linear-disjoint-regions-reversed"),
                expected_snapshot: input.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::default(),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    profile: PlanarProfile2 {
                        regions: vec![linear_region(&second, &[]), linear_region(&first, &[])],
                    },
                    distance: 2.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("declaration order cannot change disjoint solids");
        assert_eq!(
            outcome.snapshot.semantic_digest(),
            reversed.snapshot.semantic_digest()
        );
    }

    #[test]
    fn selected_face_linear_edit_preserves_sibling_solids_in_a_compound_snapshot() {
        let input = NativeKernel::empty();
        let first = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(1.0, 0.0),
            ProtocolPoint2::new(1.0, 1.0),
            ProtocolPoint2::new(0.0, 1.0),
        ];
        let second = vec![
            ProtocolPoint2::new(3.0, 0.0),
            ProtocolPoint2::new(4.0, 0.0),
            ProtocolPoint2::new(4.0, 1.0),
            ProtocolPoint2::new(3.0, 1.0),
        ];
        let compound = NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("compound-face-edit-base"),
                expected_snapshot: input.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePlanarProfile {
                    frame: PlanarFrame3::new(
                        ProtocolPoint3::default(),
                        ProtocolVector3::new(1.0, 0.0, 0.0),
                        ProtocolVector3::new(0.0, 1.0, 0.0),
                    ),
                    profile: PlanarProfile2 {
                        regions: vec![linear_region(&first, &[]), linear_region(&second, &[])],
                    },
                    distance: 2.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("two-solid compound");
        let target = NativeKernel::debug_scene(&compound.snapshot)
            .triangles
            .iter()
            .find(|triangle| {
                triangle.role == FaceRole::ExtrusionTop
                    && triangle.vertices.iter().all(|vertex| vertex.x < 2.0)
            })
            .expect("first solid top face")
            .source_face;
        let support = NativeKernel::planar_face_support(&compound.snapshot, target).unwrap();
        let [u_min, u_max, v_min, v_max] = support.boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u_min, u_max, v_min, v_max], point| {
                [
                    u_min.min(point.x),
                    u_max.max(point.x),
                    v_min.min(point.y),
                    v_max.max(point.y),
                ]
            },
        );
        let inset = [
            ProtocolPoint2::new(u_min + 0.25, v_min + 0.25),
            ProtocolPoint2::new(u_max - 0.25, v_min + 0.25),
            ProtocolPoint2::new(u_max - 0.25, v_max - 0.25),
            ProtocolPoint2::new(u_min + 0.25, v_max - 0.25),
        ];
        let edited = NativeKernel::execute(
            &compound.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("compound-face-edit"),
                expected_snapshot: compound.snapshot.id(),
                precision: compound.snapshot.precision_policy().unwrap(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: target,
                    frame: support.frame,
                    profile: PlanarProfile2::from_polygon(&inset),
                    distance: 1.0,
                    operation: FaceExtrusionOperation::Add,
                },
            },
            &CancellationToken::new(),
        )
        .expect("one solid is edited while its sibling is retained");

        assert_eq!(edited.snapshot.counts().solids, 2);
        assert_close(edited.snapshot.measures().volume, 4.25);
        let bounds = edited.snapshot.measures().bounds.unwrap();
        assert_close(bounds.min.x, 0.0);
        assert_close(bounds.max.x, 4.0);
        assert_close(bounds.max.z, 3.0);
        assert!(NativeKernel::validate(&edited.snapshot, ValidationProfile::Solid).valid);
    }

    #[test]
    fn linear_hole_order_and_winding_are_deterministic_and_touching_regions_fail() {
        let input = NativeKernel::empty();
        let outer = vec![
            ProtocolPoint2::new(0.0, 0.0),
            ProtocolPoint2::new(6.0, 0.0),
            ProtocolPoint2::new(6.0, 4.0),
            ProtocolPoint2::new(0.0, 4.0),
        ];
        let left = vec![
            ProtocolPoint2::new(1.0, 1.0),
            ProtocolPoint2::new(2.0, 1.0),
            ProtocolPoint2::new(2.0, 3.0),
            ProtocolPoint2::new(1.0, 3.0),
        ];
        let right = vec![
            ProtocolPoint2::new(4.0, 1.0),
            ProtocolPoint2::new(5.0, 1.0),
            ProtocolPoint2::new(5.0, 3.0),
            ProtocolPoint2::new(4.0, 3.0),
        ];
        let execute = |profile| {
            NativeKernel::execute(
                &input,
                &ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new("linear-hole-order"),
                    expected_snapshot: input.id(),
                    precision: PrecisionPolicy::default(),
                    command: KernelCommand::ExtrudePlanarProfile {
                        frame: PlanarFrame3::new(
                            ProtocolPoint3::default(),
                            ProtocolVector3::new(1.0, 0.0, 0.0),
                            ProtocolVector3::new(0.0, 1.0, 0.0),
                        ),
                        profile,
                        distance: 2.0,
                    },
                },
                &CancellationToken::new(),
            )
        };
        let first = execute(PlanarProfile2 {
            regions: vec![linear_region(&outer, &[left.clone(), right.clone()])],
        })
        .unwrap();
        let mut reversed_outer = outer.clone();
        reversed_outer.reverse();
        let mut reversed_left = left.clone();
        reversed_left.reverse();
        let mut reversed_right = right.clone();
        reversed_right.reverse();
        let second = execute(PlanarProfile2 {
            regions: vec![linear_region(
                &reversed_outer,
                &[reversed_right, reversed_left],
            )],
        })
        .unwrap();
        assert_eq!(
            first.snapshot.semantic_digest(),
            second.snapshot.semantic_digest()
        );

        let touching = vec![
            ProtocolPoint2::new(6.0, 1.0),
            ProtocolPoint2::new(7.0, 1.0),
            ProtocolPoint2::new(7.0, 2.0),
            ProtocolPoint2::new(6.0, 2.0),
        ];
        let error = execute(PlanarProfile2 {
            regions: vec![linear_region(&outer, &[]), linear_region(&touching, &[])],
        })
        .unwrap_err();
        assert_eq!(error.code, KernelErrorCode::InvalidInput);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "PLANAR_PROFILE_REGIONS_OVERLAP"
        );
    }

    #[test]
    fn selected_face_add_and_cut_preserve_linear_profile_holes() {
        for (operation, distance, expected_volume) in [
            (FaceExtrusionOperation::Add, 1.0, 25.25),
            (FaceExtrusionOperation::Cut, 1.0, 22.75),
            (FaceExtrusionOperation::Cut, 10.0, 19.0),
        ] {
            let input = canonical().snapshot;
            let target_face = NativeKernel::debug_scene(&input)
                .triangles
                .iter()
                .find(|triangle| triangle.role == FaceRole::PositiveZ)
                .expect("positive-Z target")
                .source_face;
            let support = NativeKernel::planar_face_support(&input, target_face).unwrap();
            let outer = vec![
                ProtocolPoint2::new(-0.5, -0.75),
                ProtocolPoint2::new(0.5, -0.75),
                ProtocolPoint2::new(0.5, 0.75),
                ProtocolPoint2::new(-0.5, 0.75),
            ];
            let hole = vec![
                ProtocolPoint2::new(-0.25, -0.25),
                ProtocolPoint2::new(0.25, -0.25),
                ProtocolPoint2::new(0.25, 0.25),
                ProtocolPoint2::new(-0.25, 0.25),
            ];
            let outcome = NativeKernel::execute(
                &input,
                &ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new("linear-face-region-hole"),
                    expected_snapshot: input.id(),
                    precision: input.precision_policy().unwrap(),
                    command: KernelCommand::ExtrudeFacePlanarProfile {
                        target_face,
                        frame: support.frame,
                        profile: PlanarProfile2 {
                            regions: vec![linear_region(&outer, &[hole])],
                        },
                        distance,
                        operation,
                    },
                },
                &CancellationToken::new(),
            )
            .expect("annular selected-face feature");

            assert_close(outcome.snapshot.measures().volume, expected_volume);
            assert_eq!(
                outcome.snapshot.counts().solids,
                if operation == FaceExtrusionOperation::Cut && distance > 4.0 {
                    2
                } else {
                    1
                }
            );
            let mut side_ordinals = outcome
                .snapshot
                .topology
                .faces
                .iter()
                .filter_map(|face| match face.value.role {
                    FaceRole::FeatureSide(ordinal) => Some(ordinal),
                    _ => None,
                })
                .collect::<Vec<_>>();
            side_ordinals.sort_unstable();
            side_ordinals.dedup();
            assert_eq!(side_ordinals, (0..8).collect::<Vec<_>>());
            assert!(
                outcome
                    .snapshot
                    .topology
                    .faces
                    .iter()
                    .any(|face| !face.value.inner_loops.is_empty())
            );
        }
    }

    #[test]
    fn crossing_cut_regularizes_the_previous_cut_and_publishes_a_new_snapshot() {
        let empty = NativeKernel::empty();
        let base = NativeKernel::execute(
            &empty,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("crossing-cut-base"),
                expected_snapshot: empty.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeCuboid {
                    origin: ProtocolPoint3::default(),
                    size_x: 10.0,
                    size_y: 10.0,
                    size_z: 10.0,
                },
            },
            &CancellationToken::new(),
        )
        .unwrap();
        let first_target = NativeKernel::debug_scene(&base.snapshot)
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .unwrap()
            .source_face;
        let first_support =
            NativeKernel::planar_face_support(&base.snapshot, first_target).unwrap();
        let center = |support: &PlanarFaceSupport| {
            let bounds = support.boundary.iter().fold(
                [
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                ],
                |bounds, point| {
                    [
                        bounds[0].min(point.x),
                        bounds[1].max(point.x),
                        bounds[2].min(point.y),
                        bounds[3].max(point.y),
                    ]
                },
            );
            ProtocolPoint2::new(0.5 * (bounds[0] + bounds[1]), 0.5 * (bounds[2] + bounds[3]))
        };
        let first_center = center(&first_support);
        let first = NativeKernel::execute(
            &base.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("crossing-cut-first"),
                expected_snapshot: base.snapshot.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: first_target,
                    frame: first_support.frame,
                    profile: PlanarProfile2 {
                        regions: vec![PlanarRegion2 {
                            outer: PlanarLoop2 {
                                curves: vec![PlanarCurve2::Circle {
                                    center: first_center,
                                    radius: 1.2,
                                    direction: ArcDirection::CounterClockwise,
                                }],
                            },
                            holes: Vec::new(),
                        }],
                    },
                    distance: 20.0,
                    operation: FaceExtrusionOperation::Cut,
                },
            },
            &CancellationToken::new(),
        )
        .expect("first through cut");
        let second_target = NativeKernel::debug_scene(&first.snapshot)
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveX)
            .unwrap()
            .source_face;
        let second_support =
            NativeKernel::planar_face_support(&first.snapshot, second_target).unwrap();
        let second_center = center(&second_support);
        let second = NativeKernel::execute(
            &first.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("crossing-cut-second"),
                expected_snapshot: first.snapshot.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: second_target,
                    frame: second_support.frame,
                    profile: PlanarProfile2 {
                        regions: vec![PlanarRegion2 {
                            outer: PlanarLoop2 {
                                curves: vec![PlanarCurve2::Circle {
                                    center: second_center,
                                    radius: 1.2,
                                    direction: ArcDirection::CounterClockwise,
                                }],
                            },
                            holes: Vec::new(),
                        }],
                    },
                    distance: 20.0,
                    operation: FaceExtrusionOperation::Cut,
                },
            },
            &CancellationToken::new(),
        )
        .expect("the second cut must traverse and merge with the first void");

        assert_ne!(first.snapshot.id(), second.snapshot.id());
        assert!(second.snapshot.measures().volume < first.snapshot.measures().volume);
        assert!(NativeKernel::validate(&second.snapshot, ValidationProfile::Solid).valid);
    }

    #[test]
    fn crossing_circular_cuts_in_the_canonical_body_publish_a_valid_successor() {
        let empty = NativeKernel::empty();
        let base = NativeKernel::execute(
            &empty,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("canonical-crossing-cut-base"),
                expected_snapshot: empty.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeCuboid {
                    origin: ProtocolPoint3::default(),
                    size_x: 2.0,
                    size_y: 3.0,
                    size_z: 4.0,
                },
            },
            &CancellationToken::new(),
        )
        .unwrap();
        let centered_circle = |snapshot: &Snapshot, face_id: u64, radius: f64| {
            let target = NativeKernel::debug_scene(snapshot)
                .triangles
                .iter()
                .find(|triangle| triangle.source_face.entity.0 == face_id)
                .unwrap_or_else(|| panic!("face {face_id} must exist"))
                .source_face;
            let support = NativeKernel::planar_face_support(snapshot, target).unwrap();
            let bounds = support.boundary.iter().fold(
                [
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                ],
                |bounds, point| {
                    [
                        bounds[0].min(point.x),
                        bounds[1].max(point.x),
                        bounds[2].min(point.y),
                        bounds[3].max(point.y),
                    ]
                },
            );
            (
                target,
                support.frame,
                PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: PlanarLoop2 {
                            curves: vec![PlanarCurve2::Circle {
                                center: ProtocolPoint2::new(
                                    0.5 * (bounds[0] + bounds[1]),
                                    0.5 * (bounds[2] + bounds[3]),
                                ),
                                radius,
                                direction: ArcDirection::CounterClockwise,
                            }],
                        },
                        holes: Vec::new(),
                    }],
                },
            )
        };
        let (first_target, first_frame, first_profile) = centered_circle(&base.snapshot, 56, 0.75);
        let first = NativeKernel::execute(
            &base.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("canonical-crossing-cut-first"),
                expected_snapshot: base.snapshot.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: first_target,
                    frame: first_frame,
                    profile: first_profile,
                    distance: 2.364_037_644_953_734_6,
                    operation: FaceExtrusionOperation::Cut,
                },
            },
            &CancellationToken::new(),
        )
        .expect("the first circular cut must commit");
        let (second_target, second_frame, second_profile) =
            centered_circle(&first.snapshot, 38, 0.75);
        let second = NativeKernel::execute(
            &first.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("canonical-crossing-cut-second"),
                expected_snapshot: first.snapshot.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: second_target,
                    frame: second_frame,
                    profile: second_profile,
                    distance: 3.887_032_001_710_053,
                    operation: FaceExtrusionOperation::Cut,
                },
            },
            &CancellationToken::new(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "crossing circular cut failed: {:?}",
                error
                    .diagnostics
                    .iter()
                    .map(|diagnostic| (&diagnostic.code, &diagnostic.path))
                    .collect::<Vec<_>>()
            )
        });

        assert_ne!(first.snapshot.id(), second.snapshot.id());
        assert!(second.snapshot.measures().volume < first.snapshot.measures().volume);
        assert!(NativeKernel::validate(&second.snapshot, ValidationProfile::Solid).valid);
        let presentation = NativeKernel::debug_scene(&second.snapshot);
        let visible_edges = presentation
            .edges
            .iter()
            .filter(|edge| !edge.is_smooth)
            .count();
        assert!(
            visible_edges * 7 < presentation.edges.len(),
            "crossing-cut tessellation must not publish its planar fragment fan: \
             {visible_edges} visible / {} total edges",
            presentation.edges.len(),
        );
        let logical_cylindrical_sides = second
            .snapshot
            .topology
            .faces
            .iter()
            .filter_map(|face| match face.value.role {
                FaceRole::FeatureSide(role) if role < u32::MAX - 1 => Some(role),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert!(
            logical_cylindrical_sides.len() <= 2,
            "two analytic circle cuts must retain at most two logical side owners, got \
             {logical_cylindrical_sides:?}"
        );
        let topology = &second.snapshot.topology;
        let prismatic_roles = presentation_prismatic_feature_roles(topology);
        assert_eq!(
            prismatic_roles, logical_cylindrical_sides,
            "each circle cut must publish one coherent prismatic carrier"
        );
        let smooth = presentation_smooth_edge_flags(topology);
        let incidence = edge_incident_face_indices(topology);
        assert!(smooth.iter().enumerate().all(|(edge_index, smooth)| {
            let classification = presentation_edge_classification(topology, &incidence, edge_index);
            classification
                .same_feature_side_role
                .filter(|role| prismatic_roles.contains(role))
                .is_none_or(|_| *smooth)
        }));
    }

    #[test]
    fn all_malformed_topologies_are_rejected_by_the_validator() {
        let mutations: [fn(&mut Topology); 9] = [
            validator::malformed::dangling_vertex,
            validator::malformed::endpoint_mismatch,
            validator::malformed::open_loop,
            validator::malformed::edge_used_once,
            validator::malformed::same_edge_use_orientation,
            validator::malformed::pcurve_mismatch,
            validator::malformed::bowed_pcurve_with_matching_endpoints,
            validator::malformed::reverse_face,
            validator::malformed::dangling_coedge,
        ];
        for mutate in mutations {
            let mut topology =
                build_cuboid(Point3::new(0.0, 0.0, 0.0), Vector3::new(2.0, 3.0, 4.0));
            mutate(&mut topology);
            let report = validator::validate(&topology, 1.0e-9);
            assert!(!report.is_valid());
        }
    }

    #[test]
    fn validator_rejects_bowed_pcurve_even_when_both_endpoints_match() {
        let mut topology = build_cuboid(Point3::new(0.0, 0.0, 0.0), Vector3::new(2.0, 3.0, 4.0));
        validator::malformed::bowed_pcurve_with_matching_endpoints(&mut topology);
        let report = validator::validate(&topology, 1.0e-9);
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == validator::DiagnosticCode::PcurveLocusMismatch
        }));
        assert!(report.diagnostics.iter().all(|diagnostic| {
            diagnostic.code != validator::DiagnosticCode::PcurveEndpointMismatch
        }));
    }

    #[test]
    fn diagnostic_tessellation_preserves_a_planar_face_hole() {
        let outer = vec![
            Point3::new(-2.0, -2.0, 0.0),
            Point3::new(2.0, -2.0, 0.0),
            Point3::new(2.0, 2.0, 0.0),
            Point3::new(-2.0, 2.0, 0.0),
        ];
        let inner = vec![
            Point3::new(-1.0, -1.0, 0.0),
            Point3::new(-1.0, 1.0, 0.0),
            Point3::new(1.0, 1.0, 0.0),
            Point3::new(1.0, -1.0, 0.0),
        ];
        let plane = topology::Plane::new(
            Point3::new(-2.0, -2.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );

        let triangles =
            triangulate_face_boundaries(&[outer, inner], plane, TessellationFallback::Refuse);
        assert_eq!(triangles.len(), 8);
        let area = triangles
            .iter()
            .map(|triangle| {
                (triangle[1] - triangle[0])
                    .cross(triangle[2] - triangle[0])
                    .length()
                    * 0.5
            })
            .sum::<f64>();
        assert_close(area, 12.0);
        let hole_center = plane.project(Point3::default());
        assert!(triangles.iter().all(|triangle| {
            !point_in_or_on_triangle(
                hole_center,
                plane.project(triangle[0]),
                plane.project(triangle[1]),
                plane.project(triangle[2]),
            )
        }));

        let outer = vec![
            Point3::new(-5.0, -5.0, 0.0),
            Point3::new(5.0, -5.0, 0.0),
            Point3::new(5.0, 5.0, 0.0),
            Point3::new(-5.0, 5.0, 0.0),
        ];
        let left_hole = vec![
            Point3::new(-3.0, -1.0, 0.0),
            Point3::new(-3.0, 1.0, 0.0),
            Point3::new(-1.0, 1.0, 0.0),
            Point3::new(-1.0, -1.0, 0.0),
        ];
        let right_hole = vec![
            Point3::new(1.0, -1.0, 0.0),
            Point3::new(1.0, 1.0, 0.0),
            Point3::new(3.0, 1.0, 0.0),
            Point3::new(3.0, -1.0, 0.0),
        ];
        let triangles = triangulate_face_boundaries(
            &[outer, left_hole, right_hole],
            plane,
            TessellationFallback::Refuse,
        );
        assert_eq!(triangles.len(), 14);
        let area = triangles
            .iter()
            .map(|triangle| {
                (triangle[1] - triangle[0])
                    .cross(triangle[2] - triangle[0])
                    .length()
                    * 0.5
            })
            .sum::<f64>();
        assert_close(area, 92.0);
        for hole_center in [Point3::new(-2.0, 0.0, 0.0), Point3::new(2.0, 0.0, 0.0)] {
            let hole_center = plane.project(hole_center);
            assert!(triangles.iter().all(|triangle| {
                !point_in_or_on_triangle(
                    hole_center,
                    plane.project(triangle[0]),
                    plane.project(triangle[1]),
                    plane.project(triangle[2]),
                )
            }));
        }
    }

    #[test]
    fn annular_face_is_authoritative_across_validation_measures_support_and_display() {
        let input = canonical().snapshot;
        let (request, _) = face_feature_request(&input, FaceExtrusionOperation::Add);
        let outcome = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect("rectangular boss with one annular shoulder");
        let snapshot = &outcome.snapshot;
        let annular_faces = snapshot
            .topology
            .faces
            .iter()
            .filter(|face| !face.value.inner_loops.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(annular_faces.len(), 1);
        assert_eq!(annular_faces[0].value.inner_loops.len(), 1);
        assert_eq!(
            snapshot.counts(),
            ProtocolTopologyCounts {
                vertices: 16,
                edges: 24,
                coedges: 48,
                loops: 12,
                faces: 11,
                shells: 1,
                solids: 1,
            }
        );
        assert!(NativeKernel::validate(snapshot, ValidationProfile::Solid).valid);
        assert_close(snapshot.measures().surface_area, 57.0);
        assert_close(snapshot.measures().volume, 25.5);
        assert_point_close(
            snapshot.measures().centroid.expect("solid centroid"),
            ProtocolPoint3::new(1.0, 1.5, 54.75 / 25.5),
        );

        let annular_ref = entity_ref(snapshot.id, annular_faces[0].id.get(), EntityKind::Face);
        let support = NativeKernel::planar_face_support(snapshot, annular_ref)
            .expect("annular face is exact sketch support");
        assert_eq!(support.boundary.len(), 4);
        assert_eq!(support.inner_boundaries.len(), 1);
        assert_eq!(support.inner_boundaries[0].len(), 4);
        assert_eq!(
            semantic_digest(&snapshot.topology, snapshot.precision_policy().unwrap()),
            snapshot.semantic_digest()
        );
        let mut without_face_ownership = snapshot.topology.clone();
        without_face_ownership
            .faces
            .iter_mut()
            .find(|face| !face.value.inner_loops.is_empty())
            .expect("annular face")
            .value
            .inner_loops
            .clear();
        assert_ne!(
            semantic_digest(
                &without_face_ownership,
                snapshot.precision_policy().unwrap()
            ),
            snapshot.semantic_digest(),
            "face-owned holes participate in deterministic snapshot identity"
        );
        let signed_area = |polygon: &[ProtocolPoint2]| {
            (0..polygon.len())
                .map(|index| {
                    let next = (index + 1) % polygon.len();
                    polygon[index].x * polygon[next].y - polygon[index].y * polygon[next].x
                })
                .sum::<f64>()
                * 0.5
        };
        assert!(signed_area(&support.boundary) > 0.0);
        assert!(signed_area(&support.inner_boundaries[0]) < 0.0);

        let triangles = NativeKernel::debug_scene(snapshot)
            .triangles
            .into_iter()
            .filter(|triangle| triangle.source_face == annular_ref)
            .collect::<Vec<_>>();
        assert_eq!(triangles.len(), 8);
        let plane = annular_faces[0]
            .value
            .surface
            .as_plane()
            .expect("annular shoulder is planar");
        let hole_center = plane.project(Point3::new(1.0, 1.5, 4.0));
        assert!(triangles.iter().all(|triangle| {
            !point_in_or_on_triangle(
                hole_center,
                plane.project(internal_point(triangle.vertices[0])),
                plane.project(internal_point(triangle.vertices[1])),
                plane.project(internal_point(triangle.vertices[2])),
            )
        }));

        let mut malformed = snapshot.topology.clone();
        let inner_loop = malformed
            .faces
            .iter()
            .find(|face| !face.value.inner_loops.is_empty())
            .expect("annular face")
            .value
            .inner_loops[0];
        let coedge_keys = &mut malformed.loops[inner_loop.0].value.coedges;
        coedge_keys.reverse();
        for coedge_key in coedge_keys.iter().copied() {
            let coedge = &mut malformed.coedges[coedge_key.0].value;
            coedge.orientation = coedge.orientation.reversed();
            coedge.parameter_range = coedge.parameter_range.reversed();
        }
        let malformed_report = validator::validate(&malformed, 1.0e-9);
        assert!(malformed_report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == validator::DiagnosticCode::FaceOrientationInvalid
                && diagnostic.path.contains("inner-loop/0")
        }));

        let translate_inner_boundary = |offset: f64| {
            let mut malformed = snapshot.topology.clone();
            let inner_loop = malformed
                .faces
                .iter()
                .find(|face| !face.value.inner_loops.is_empty())
                .expect("annular face")
                .value
                .inner_loops[0];
            let vertex_keys = malformed.loops[inner_loop.0]
                .value
                .coedges
                .iter()
                .filter_map(|coedge_key| {
                    let coedge = malformed.coedge(*coedge_key)?;
                    malformed
                        .oriented_edge_vertices(&coedge.value)
                        .map(|pair| pair.0[0])
                })
                .collect::<BTreeSet<_>>();
            for vertex_key in vertex_keys {
                malformed.vertices[vertex_key.0].value.point.x += offset;
            }
            for edge in &mut malformed.edges {
                let endpoints = edge
                    .value
                    .vertices
                    .map(|vertex_key| malformed.vertices[vertex_key.0].value.point);
                assert!(edge.value.set_line_endpoints(endpoints));
            }
            malformed
        };
        let intersecting_report = validator::validate(&translate_inner_boundary(1.0), 1.0e-9);
        assert!(intersecting_report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == validator::DiagnosticCode::FaceLoopIntersection
        }));
        let outside_report = validator::validate(&translate_inner_boundary(3.0), 1.0e-9);
        assert!(
            outside_report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == validator::DiagnosticCode::FaceHoleOutside)
        );
    }
}

//! Exact topology-preserving push/pull for one extrusion-cap face.
//!
//! This is intentionally narrower than a general offset or Boolean. The
//! selected face must be the unholed cap of an axis-aligned prism whose side
//! rails all terminate on one certified support plane. Moving the cap before
//! that support plane preserves the shell graph exactly; reaching or crossing
//! it would require entity deletion/merging and is rejected transactionally.

use std::collections::BTreeSet;

use artificer_protocol::{EntityKind, EntityRef, PrecisionPolicy, SnapshotId};

use crate::topology::{CoedgeKey, Curve2, Curve3, EdgeKey, FaceKey, Topology, Vector3, VertexKey};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FacePushPullInputError {
    NonFinite,
    SourceUnsupported,
    TargetSnapshotMismatch,
    TargetNotFace,
    TargetMissing,
    TargetHasHoles,
    TargetNotPlanar,
    TargetNotExtrusionCap,
    NonDistinctDistance,
    FeatureTooSmall,
    SupportContact,
    CoordinateLimit,
    NumericallyUnrepresentable,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedFacePushPull {
    pub(crate) target_face_index: usize,
    pub(crate) moved_vertices: Vec<VertexKey>,
    pub(crate) distance: f64,
    pub(crate) outward_normal: Vector3,
}

pub(crate) struct FacePushPullArguments<'a> {
    pub snapshot: SnapshotId,
    pub topology: &'a Topology,
    pub target_face: EntityRef,
    pub distance: f64,
    pub precision: PrecisionPolicy,
}

pub(crate) fn validate_face_push_pull_input(
    arguments: FacePushPullArguments<'_>,
) -> Result<ValidatedFacePushPull, FacePushPullInputError> {
    let FacePushPullArguments {
        snapshot,
        topology,
        target_face,
        distance,
        precision,
    } = arguments;
    if !distance.is_finite() {
        return Err(FacePushPullInputError::NonFinite);
    }
    if target_face.snapshot != snapshot {
        return Err(FacePushPullInputError::TargetSnapshotMismatch);
    }
    if target_face.kind != EntityKind::Face {
        return Err(FacePushPullInputError::TargetNotFace);
    }
    if distance == 0.0 {
        return Err(FacePushPullInputError::NonDistinctDistance);
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if distance.abs() <= minimum {
        return Err(FacePushPullInputError::FeatureTooSmall);
    }
    if topology.solids.len() != 1
        || topology.shells.len() != 1
        || topology.faces.is_empty()
        || topology.solids[0].value.outer_shell.0 != 0
        || topology.shells[0].value.faces.len() != topology.faces.len()
    {
        return Err(FacePushPullInputError::SourceUnsupported);
    }
    let all_faces = topology.shells[0]
        .value
        .faces
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if all_faces != (0..topology.faces.len()).map(FaceKey).collect() {
        return Err(FacePushPullInputError::SourceUnsupported);
    }
    if topology
        .edges
        .iter()
        .any(|edge| !matches!(edge.value.curve, Curve3::Line { .. }))
        || topology
            .coedges
            .iter()
            .any(|coedge| !matches!(coedge.value.pcurve, Curve2::Line { .. }))
        || topology
            .faces
            .iter()
            .any(|face| face.value.surface.as_plane().is_none())
    {
        return Err(FacePushPullInputError::SourceUnsupported);
    }

    let angular_tolerance = precision.angular_agreement_radians.max(1.0e-12);
    let target_face_index = topology
        .faces
        .iter()
        .position(|record| record.id.get() == target_face.entity.0)
        .ok_or(FacePushPullInputError::TargetMissing)?;
    let target = &topology.faces[target_face_index].value;
    if !target.inner_loops.is_empty() {
        return Err(FacePushPullInputError::TargetHasHoles);
    }
    let target_plane = target
        .surface
        .as_plane()
        .ok_or(FacePushPullInputError::TargetNotPlanar)?;
    // Everything below is expressed in the target's own outward normal, so the
    // face need not face along a world axis — only be planar, and be a cap.
    let normal_length = target_plane.normal.length();
    if !normal_length.is_finite() || normal_length <= f64::EPSILON {
        return Err(FacePushPullInputError::TargetNotPlanar);
    }
    let outward_normal = target_plane.normal / normal_length;

    let target_loop = topology
        .loop_record(target.outer_loop)
        .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
    if target_loop.value.coedges.len() < 3 {
        return Err(FacePushPullInputError::TargetNotExtrusionCap);
    }
    let target_edges = target_loop
        .value
        .coedges
        .iter()
        .copied()
        .map(|coedge_key| {
            topology
                .coedge(coedge_key)
                .map(|coedge| coedge.value.edge)
                .ok_or(FacePushPullInputError::TargetNotExtrusionCap)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if target_edges.len() != target_loop.value.coedges.len() {
        return Err(FacePushPullInputError::TargetNotExtrusionCap);
    }

    let mut moved_vertices = BTreeSet::new();
    for coedge_key in target_loop.value.coedges.iter().copied() {
        let coedge = topology
            .coedge(coedge_key)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
        let (vertices, _) = topology
            .oriented_edge_vertices(&coedge.value)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
        moved_vertices.extend(vertices);
    }
    if moved_vertices.len() != target_edges.len() {
        return Err(FacePushPullInputError::TargetNotExtrusionCap);
    }

    let target_coordinate = topology
        .vertex(
            *moved_vertices
                .first()
                .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?,
        )
        .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
        .value
        .point
        .as_vector()
        .dot(outward_normal);
    let linear_tolerance = precision.linear_agreement;
    for vertex_key in &moved_vertices {
        let point = topology
            .vertex(*vertex_key)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
            .value
            .point;
        if (point.as_vector().dot(outward_normal) - target_coordinate).abs() > linear_tolerance {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
    }

    // A cap must be an exterior extremum. This makes positive motion a
    // collision-free extension instead of a local offset into another lobe.
    if topology.vertices.iter().any(|vertex| {
        vertex.value.point.as_vector().dot(outward_normal) > target_coordinate + linear_tolerance
    }) {
        return Err(FacePushPullInputError::TargetNotExtrusionCap);
    }

    let mut rail_edges = BTreeSet::new();
    let mut support_depth = None::<f64>;
    for vertex_key in &moved_vertices {
        let incident = topology
            .edges
            .iter()
            .enumerate()
            .filter_map(|(index, edge)| {
                edge.value
                    .vertices
                    .contains(vertex_key)
                    .then_some(EdgeKey(index))
            })
            .collect::<Vec<_>>();
        let rails = incident
            .iter()
            .copied()
            .filter(|edge| !target_edges.contains(edge))
            .collect::<Vec<_>>();
        if incident.len() != 3 || rails.len() != 1 {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        let rail_key = rails[0];
        let rail = topology
            .edge(rail_key)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
        let other = if rail.value.vertices[0] == *vertex_key {
            rail.value.vertices[1]
        } else if rail.value.vertices[1] == *vertex_key {
            rail.value.vertices[0]
        } else {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        };
        if moved_vertices.contains(&other) {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        let cap_point = topology
            .vertex(*vertex_key)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
            .value
            .point;
        let support_point = topology
            .vertex(other)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
            .value
            .point;
        let rail_vector = support_point - cap_point;
        let signed_depth = rail_vector.dot(outward_normal);
        let tangent = rail_vector - outward_normal * signed_depth;
        if signed_depth >= -minimum || tangent.length() > linear_tolerance {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        let depth = -signed_depth;
        if support_depth.is_some_and(|reference| (reference - depth).abs() > linear_tolerance) {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        support_depth = Some(depth);
        rail_edges.insert(rail_key);
    }
    if rail_edges.len() != moved_vertices.len() {
        return Err(FacePushPullInputError::TargetNotExtrusionCap);
    }
    let support_depth = support_depth.ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;

    let mut side_faces = BTreeSet::new();
    for target_edge in &target_edges {
        let owners = face_owners_of_edge(topology, *target_edge)?;
        if owners.len() != 2 || !owners.contains(&FaceKey(target_face_index)) {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        let side = owners
            .into_iter()
            .find(|face| face.0 != target_face_index)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
        let side_face = &topology
            .face(side)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
            .value;
        let side_loop = topology
            .loop_record(side_face.outer_loop)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
        if !side_face.inner_loops.is_empty()
            || side_loop.value.coedges.len() != 4
            || side_face
                .surface
                .as_plane()
                .is_none_or(|plane| plane.normal.dot(outward_normal).abs() > angular_tolerance)
        {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        let side_edges = loop_edges(topology, &side_loop.value.coedges)?;
        if side_edges.intersection(&target_edges).count() != 1
            || side_edges.intersection(&rail_edges).count() != 2
        {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
        side_faces.insert(side);
    }
    if side_faces.len() != target_edges.len() {
        return Err(FacePushPullInputError::TargetNotExtrusionCap);
    }
    for rail in &rail_edges {
        let owners = face_owners_of_edge(topology, *rail)?;
        if owners.len() != 2 || owners.iter().any(|face| !side_faces.contains(face)) {
            return Err(FacePushPullInputError::TargetNotExtrusionCap);
        }
    }

    if distance.is_sign_negative() && support_depth + distance <= minimum {
        return Err(FacePushPullInputError::SupportContact);
    }
    let delta = outward_normal * distance;
    for vertex_key in &moved_vertices {
        let moved = topology
            .vertex(*vertex_key)
            .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
            .value
            .point
            + delta;
        if [moved.x, moved.y, moved.z].into_iter().any(|coordinate| {
            !coordinate.is_finite() || coordinate.abs() > precision.max_abs_coordinate
        }) {
            return Err(FacePushPullInputError::CoordinateLimit);
        }
        let represented = (moved
            - topology
                .vertex(*vertex_key)
                .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?
                .value
                .point)
            .dot(outward_normal);
        if (represented - distance).abs() > linear_tolerance {
            return Err(FacePushPullInputError::NumericallyUnrepresentable);
        }
    }

    Ok(ValidatedFacePushPull {
        target_face_index,
        moved_vertices: moved_vertices.into_iter().collect(),
        distance,
        outward_normal,
    })
}

pub(crate) fn build_face_push_pull(source: &Topology, input: &ValidatedFacePushPull) -> Topology {
    let mut candidate = source.clone();
    let delta = input.outward_normal * input.distance;
    for key in &input.moved_vertices {
        candidate.vertices[key.0].value.point = candidate.vertices[key.0].value.point + delta;
    }
    for edge in &mut candidate.edges {
        let endpoints = edge
            .value
            .vertices
            .map(|vertex| candidate.vertices[vertex.0].value.point);
        assert!(edge.value.set_line_endpoints(endpoints));
    }
    let target_plane = candidate.faces[input.target_face_index]
        .value
        .surface
        .as_plane_mut()
        .expect("validated push/pull source is planar");
    target_plane.origin = target_plane.origin + delta;

    let mut pcurves = Vec::with_capacity(candidate.coedges.len());
    for face in &candidate.faces {
        let plane = face
            .value
            .surface
            .as_plane()
            .expect("validated push/pull source is planar");
        for loop_key in face.value.loops() {
            let loop_record = candidate
                .loop_record(loop_key)
                .expect("validated face loop remains present after a topology-preserving move");
            for coedge_key in loop_record.value.coedges.iter().copied() {
                let coedge = candidate
                    .coedge(coedge_key)
                    .expect("validated coedge remains present after a topology-preserving move");
                let (_, endpoints) = candidate
                    .oriented_edge_vertices(&coedge.value)
                    .expect("validated edge remains present after a topology-preserving move");
                pcurves.push((coedge_key, endpoints.map(|point| plane.project(point))));
            }
        }
    }
    for (coedge, endpoints) in pcurves {
        assert!(
            candidate.coedges[coedge.0]
                .value
                .set_line_pcurve_endpoints(endpoints)
        );
    }
    candidate
}

fn face_owners_of_edge(
    topology: &Topology,
    edge: EdgeKey,
) -> Result<Vec<FaceKey>, FacePushPullInputError> {
    let mut owners = Vec::new();
    for (face_index, face) in topology.faces.iter().enumerate() {
        let mut owns = false;
        for loop_key in face.value.loops() {
            let loop_record = topology
                .loop_record(loop_key)
                .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
            for coedge_key in &loop_record.value.coedges {
                let coedge = topology
                    .coedge(*coedge_key)
                    .ok_or(FacePushPullInputError::TargetNotExtrusionCap)?;
                if coedge.value.edge == edge {
                    if owns {
                        return Err(FacePushPullInputError::TargetNotExtrusionCap);
                    }
                    owns = true;
                }
            }
        }
        if owns {
            owners.push(FaceKey(face_index));
        }
    }
    Ok(owners)
}

fn loop_edges(
    topology: &Topology,
    coedges: &[CoedgeKey],
) -> Result<BTreeSet<EdgeKey>, FacePushPullInputError> {
    coedges
        .iter()
        .map(|key| {
            topology
                .coedge(*key)
                .map(|coedge| coedge.value.edge)
                .ok_or(FacePushPullInputError::TargetNotExtrusionCap)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, HistoryRelation,
        KernelCommand, KernelErrorCode, PlanarFrame3, Point2, Point3 as ProtocolPoint3,
        PrecisionPolicy, RequestId, ValidationProfile, Vector3 as ProtocolVector3,
    };

    use super::*;
    use crate::{CancellationToken, FaceRole, NativeKernel, Snapshot, entity_ref};

    fn execute(
        input: &Snapshot,
        request_id: &str,
        command: KernelCommand,
    ) -> Result<crate::ExecutionOutcome, artificer_protocol::KernelError> {
        NativeKernel::execute(
            input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new(request_id),
                expected_snapshot: input.id(),
                precision: input.precision_policy().unwrap_or_default(),
                command,
            },
            &CancellationToken::new(),
        )
    }

    fn cuboid() -> Snapshot {
        let empty = NativeKernel::empty();
        execute(
            &empty,
            "push-pull/base",
            KernelCommand::MakeCuboid {
                origin: ProtocolPoint3::default(),
                size_x: 2.0,
                size_y: 3.0,
                size_z: 4.0,
            },
        )
        .expect("fixture cuboid")
        .snapshot
    }

    fn face_by_role(snapshot: &Snapshot, role: FaceRole) -> EntityRef {
        let face = snapshot
            .topology
            .faces
            .iter()
            .find(|face| face.value.role == role)
            .expect("requested face role exists");
        entity_ref(snapshot.id(), face.id.get(), EntityKind::Face)
    }

    fn push_pull(snapshot: &Snapshot, role: FaceRole, distance: f64) -> crate::ExecutionOutcome {
        execute(
            snapshot,
            "push-pull/face",
            KernelCommand::PushPullFace {
                target_face: face_by_role(snapshot, role),
                distance,
            },
        )
        .expect("certified face push/pull")
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1.0e-10,
            "expected {expected}, received {actual}"
        );
    }

    #[test]
    fn every_cuboid_cap_extends_and_shortens_with_exact_measures() {
        let cases = [
            (FaceRole::NegativeX, 12.0),
            (FaceRole::PositiveX, 12.0),
            (FaceRole::NegativeY, 8.0),
            (FaceRole::PositiveY, 8.0),
            (FaceRole::NegativeZ, 6.0),
            (FaceRole::PositiveZ, 6.0),
        ];
        for (role, face_area) in cases {
            let input = cuboid();
            let extended = push_pull(&input, role, 1.0);
            let shortened = push_pull(&input, role, -1.0);

            assert_eq!(extended.snapshot.counts(), input.counts());
            assert_eq!(shortened.snapshot.counts(), input.counts());
            assert_close(extended.snapshot.measures().volume, 24.0 + face_area);
            assert_close(shortened.snapshot.measures().volume, 24.0 - face_area);
            assert!(NativeKernel::validate(&extended.snapshot, ValidationProfile::Solid).valid);
            assert!(NativeKernel::validate(&shortened.snapshot, ValidationProfile::Solid).valid);
        }
    }

    #[test]
    fn a_rotated_cuboid_cap_still_extends_along_its_own_normal() {
        // Nothing in the operation reads a world axis, so a solid turned off
        // every one of them behaves exactly as the aligned fixture does.
        let input = cuboid();
        let turn = std::f64::consts::FRAC_PI_4;
        let rotated = execute(
            &input,
            "push-pull/rotate",
            KernelCommand::TransformSnapshot {
                transform: artificer_protocol::SimilarityTransform3 {
                    translation: artificer_protocol::Vector3::new(0.0, 0.0, 0.0),
                    rotation: {
                        // A turn about the (1,1,1) diagonal: no face of the
                        // result faces along any world axis.
                        let (sin, cos) = (turn / 2.0).sin_cos();
                        let scale = sin / 3.0f64.sqrt();
                        artificer_protocol::RotationQuaternion::new(cos, scale, scale, scale)
                    },
                    uniform_scale: 1.0,
                },
            },
        )
        .expect("a rotation is always exact")
        .snapshot;

        let target = face_by_role(&rotated, FaceRole::PositiveZ);
        let extended = execute(
            &rotated,
            "push-pull/rotated-face",
            KernelCommand::PushPullFace {
                target_face: target,
                distance: 1.0,
            },
        )
        .expect("a rotated cap must push/pull")
        .snapshot;
        assert!(NativeKernel::validate(&extended, ValidationProfile::Solid).valid);
        assert_eq!(extended.counts(), rotated.counts());
        // The 2 x 3 cap sweeps one more unit of length.
        assert_close(extended.measures().volume, 24.0 + 6.0);
    }

    #[test]
    fn push_pull_history_is_complete_precise_and_keeps_the_target_identity() {
        let input = cuboid();
        let target = face_by_role(&input, FaceRole::PositiveZ);
        let outcome = push_pull(&input, FaceRole::PositiveZ, 2.0);

        assert_eq!(
            outcome.report.history.len(),
            outcome.snapshot.counts().total() as usize
        );
        let target_record = outcome
            .report
            .history
            .iter()
            .find(|record| {
                record.role.as_ref().is_some_and(|role| {
                    role.name == "face_push_pull.target_face" && role.ordinal.is_none()
                })
            })
            .expect("one explicit target-face lineage record");
        assert_eq!(target_record.relation, HistoryRelation::Modified);
        assert_eq!(target_record.inputs, vec![target]);
        assert_eq!(target_record.outputs.len(), 1);
        assert_eq!(target_record.outputs[0].entity, target.entity);
        assert_eq!(target_record.outputs[0].snapshot, outcome.snapshot.id());
        assert!(outcome.report.history.iter().any(|record| {
            record.relation == HistoryRelation::Unchanged
                && record
                    .role
                    .as_ref()
                    .is_some_and(|role| role.name == "face_push_pull.preserved_face")
        }));
    }

    #[test]
    fn reaching_or_crossing_the_support_is_rejected_without_a_snapshot() {
        let input = cuboid();
        let target = face_by_role(&input, FaceRole::PositiveZ);
        for distance in [-4.0, -5.0] {
            let error = execute(
                &input,
                "push-pull/collapse",
                KernelCommand::PushPullFace {
                    target_face: target,
                    distance,
                },
            )
            .expect_err("contact/deletion requires a later topology-changing operation");
            assert_eq!(error.code, KernelErrorCode::Unsupported);
            assert_eq!(
                error.diagnostics[0].code.as_str(),
                "FACE_PUSH_PULL_SUPPORT_CONTACT"
            );
            assert_eq!(error.input_snapshot, input.id());
        }
        assert_close(input.measures().volume, 24.0);
    }

    #[test]
    fn an_arbitrary_linear_prism_cap_is_its_own_exact_profile() {
        let empty = NativeKernel::empty();
        let prism = execute(
            &empty,
            "push-pull/triangle",
            KernelCommand::ExtrudePolygon {
                frame: PlanarFrame3::new(
                    ProtocolPoint3::default(),
                    ProtocolVector3::new(1.0, 0.0, 0.0),
                    ProtocolVector3::new(0.0, 1.0, 0.0),
                ),
                vertices: vec![
                    Point2::new(0.0, 0.0),
                    Point2::new(2.0, 0.0),
                    Point2::new(0.0, 1.0),
                ],
                distance: 3.0,
            },
        )
        .expect("triangle prism")
        .snapshot;
        let outcome = push_pull(&prism, FaceRole::ExtrusionTop, 2.0);

        assert_close(prism.measures().volume, 3.0);
        assert_close(outcome.snapshot.measures().volume, 5.0);
        assert_eq!(outcome.snapshot.counts(), prism.counts());
    }

    #[test]
    fn a_concave_linear_prism_cap_pushes_without_convex_approximation() {
        let empty = NativeKernel::empty();
        let prism = execute(
            &empty,
            "push-pull/concave",
            KernelCommand::ExtrudePolygon {
                frame: PlanarFrame3::new(
                    ProtocolPoint3::default(),
                    ProtocolVector3::new(1.0, 0.0, 0.0),
                    ProtocolVector3::new(0.0, 1.0, 0.0),
                ),
                vertices: vec![
                    Point2::new(0.0, 0.0),
                    Point2::new(3.0, 0.0),
                    Point2::new(3.0, 1.0),
                    Point2::new(1.0, 1.0),
                    Point2::new(1.0, 3.0),
                    Point2::new(0.0, 3.0),
                ],
                distance: 3.0,
            },
        )
        .expect("concave prism")
        .snapshot;
        let outcome = push_pull(&prism, FaceRole::ExtrusionTop, 2.0);

        assert_close(prism.measures().volume, 15.0);
        assert_close(outcome.snapshot.measures().volume, 25.0);
        assert_eq!(outcome.snapshot.counts(), prism.counts());
    }

    #[test]
    fn a_boss_end_can_be_pushed_or_pulled_without_touching_inset_extrusion() {
        let base = cuboid();
        let support_face = face_by_role(&base, FaceRole::PositiveZ);
        let support = NativeKernel::planar_face_support(&base, support_face).expect("top support");
        let boss = execute(
            &base,
            "push-pull/boss",
            KernelCommand::ExtrudeFaceProfile {
                target_face: support.face,
                frame: support.frame,
                vertices: vec![
                    Point2::new(-0.5, -0.5),
                    Point2::new(0.5, -0.5),
                    Point2::new(0.5, 0.5),
                    Point2::new(-0.5, 0.5),
                ],
                distance: 2.0,
                operation: FaceExtrusionOperation::Add,
            },
        )
        .expect("inset boss")
        .snapshot;

        let extended = push_pull(&boss, FaceRole::FeatureEnd, 1.0);
        let shortened = push_pull(&boss, FaceRole::FeatureEnd, -1.0);
        assert_close(boss.measures().volume, 26.0);
        assert_close(extended.snapshot.measures().volume, 27.0);
        assert_close(shortened.snapshot.measures().volume, 25.0);
        assert_eq!(extended.snapshot.counts(), boss.counts());
        assert_eq!(shortened.snapshot.counts(), boss.counts());
    }

    #[test]
    fn an_annular_shoulder_is_rejected_instead_of_losing_its_void() {
        let base = cuboid();
        let support_face = face_by_role(&base, FaceRole::PositiveZ);
        let support = NativeKernel::planar_face_support(&base, support_face).expect("top support");
        let boss = execute(
            &base,
            "push-pull/annular-shoulder",
            KernelCommand::ExtrudeFaceProfile {
                target_face: support.face,
                frame: support.frame,
                vertices: vec![
                    Point2::new(-0.5, -0.5),
                    Point2::new(0.5, -0.5),
                    Point2::new(0.5, 0.5),
                    Point2::new(-0.5, 0.5),
                ],
                distance: 2.0,
                operation: FaceExtrusionOperation::Add,
            },
        )
        .expect("inset boss")
        .snapshot;
        let error = execute(
            &boss,
            "push-pull/reject-hole",
            KernelCommand::PushPullFace {
                target_face: face_by_role(&boss, FaceRole::PositiveZ),
                distance: 1.0,
            },
        )
        .expect_err("annular selected face must retain its exact hole topology");

        assert_eq!(error.code, KernelErrorCode::Unsupported);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "FACE_PUSH_PULL_TARGET_HAS_HOLES"
        );
    }

    #[test]
    fn repeated_execution_is_deterministic() {
        let input = cuboid();
        let command = KernelCommand::PushPullFace {
            target_face: face_by_role(&input, FaceRole::PositiveX),
            distance: 3.25,
        };
        let first = execute(&input, "push-pull/repeat", command.clone()).expect("first execution");
        let second = execute(&input, "push-pull/repeat", command).expect("second execution");

        assert_eq!(first.snapshot.id(), second.snapshot.id());
        assert_eq!(
            first.snapshot.semantic_digest(),
            second.snapshot.semantic_digest()
        );
        assert_eq!(first.report, second.report);
    }

    #[test]
    fn typed_non_finite_and_stale_inputs_receive_structured_rejections() {
        let input = cuboid();
        let target = face_by_role(&input, FaceRole::PositiveX);
        let non_finite = execute(
            &input,
            "push-pull/non-finite",
            KernelCommand::PushPullFace {
                target_face: target,
                distance: f64::NAN,
            },
        )
        .expect_err("typed NaN reaches kernel preflight");
        assert_eq!(non_finite.code, KernelErrorCode::InvalidInput);
        assert_eq!(
            non_finite.diagnostics[0].code.as_str(),
            "FACE_PUSH_PULL_INPUT_NON_FINITE"
        );

        let stale = execute(
            &input,
            "push-pull/stale",
            KernelCommand::PushPullFace {
                target_face: EntityRef {
                    snapshot: SnapshotId::ZERO,
                    ..target
                },
                distance: 1.0,
            },
        )
        .expect_err("snapshot-local target cannot be rebound implicitly");
        assert_eq!(stale.code, KernelErrorCode::StaleSnapshot);
        assert_eq!(
            stale.diagnostics[0].code.as_str(),
            "FACE_PUSH_PULL_TARGET_STALE"
        );
    }

    #[test]
    fn the_active_precision_policy_controls_minimum_motion() {
        let input = cuboid();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("push-pull/minimum"),
            expected_snapshot: input.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::PushPullFace {
                target_face: face_by_role(&input, FaceRole::PositiveZ),
                distance: PrecisionPolicy::default().min_feature_size,
            },
        };
        let error = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect_err("motion at the minimum is not a distinct feature");
        assert_eq!(error.code, KernelErrorCode::InvalidInput);
        assert_eq!(
            error.diagnostics[0].code.as_str(),
            "FACE_PUSH_PULL_TOO_SMALL"
        );
    }
}

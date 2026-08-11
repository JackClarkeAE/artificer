//! Narrow native topology edit for a linear-profile boss or pocket on an
//! axis-aligned planar face.
//!
//! This is not a general Boolean engine. It performs a bounded, exact local
//! rewrite for a certified simple polygon that lies strictly inside its support
//! patch. Coplanar material around the profile remains one face with a true
//! inner boundary loop, so a feature does not leak artificial shoulder seams
//! into topology. The ordinary kernel validator remains the publication
//! authority.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use artificer_geometry::{
    InvalidProfile, Orientation2, Point2, ProfileClassification, ProfileWinding, classify_profile,
    orient2d,
};
use artificer_protocol::{
    EntityKind, EntityRef, FaceExtrusionOperation, MAX_EXTRUSION_PROFILE_VERTICES,
    PlanarFrame3 as ProtocolPlanarFrame3, Point2 as ProtocolPoint2, PrecisionPolicy, SnapshotId,
};

use crate::topology::{
    Coedge, CoedgeKey, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop, LoopKey,
    Orientation, Plane, Point3, Record, Shell, ShellKey, Solid, Surface, Topology, Vector3, Vertex,
    VertexKey,
};
use crate::validator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FaceFeatureInputError {
    NonFinite,
    SourceUnsupported,
    TargetSnapshotMismatch,
    TargetNotFace,
    TargetMissing,
    /// The target face is not planar.
    TargetNotPlanar,
    /// The target face's own plane is not aligned to any of the sketch
    /// frame's axes, so the interval arithmetic below has nothing to bite on.
    TargetNotAlignedToFrame,
    /// The sketch frame's axes are not orthonormal.
    FrameNotOrthonormal,
    /// The sketch frame does not lie in the target face's plane, or faces the
    /// other way.
    FrameOffTargetPlane,
    TargetDegenerate,
    FrameNotOnTarget,

    TooFewVertices,
    TooManyVertices,
    RepeatedVertex,
    SelfIntersecting,
    ProfileIndeterminate,
    ProfileOutsideFace,
    ProfileHoleInvalid,
    NonPositiveDistance,
    FeatureTooSmall,
    CutTooDeep,
    SweepCollision,
    CoordinateLimit,
    NumericallyUnrepresentable,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedFaceFeature {
    basis: Basis,
    source_faces: Vec<SourceBoundaryFace>,
    target_face_index: usize,
    target_role: FaceRole,
    target_axis: usize,
    tangent_axes: [usize; 2],
    profile: Vec<Point3>,
    profile_holes: Vec<Vec<Point3>>,
    target_coordinate: f64,
    feature_end: f64,
    pub(crate) exit_face_index: Option<usize>,
}

pub(crate) struct FaceFeatureArguments<'a> {
    pub snapshot: SnapshotId,
    pub topology: &'a Topology,
    pub target_face: EntityRef,
    pub frame: ProtocolPlanarFrame3,
    pub vertices: &'a [ProtocolPoint2],
    pub distance: f64,
    pub operation: FaceExtrusionOperation,
    pub precision: PrecisionPolicy,
}

pub(crate) struct FaceRegionFeatureArguments<'a> {
    pub snapshot: SnapshotId,
    pub topology: &'a Topology,
    pub target_face: EntityRef,
    pub frame: ProtocolPlanarFrame3,
    pub vertices: &'a [ProtocolPoint2],
    pub holes: &'a [Vec<ProtocolPoint2>],
    pub distance: f64,
    pub operation: FaceExtrusionOperation,
    pub precision: PrecisionPolicy,
}

#[derive(Clone, Debug)]
struct BoundaryFace {
    outer: Vec<[usize; 3]>,
    inner_loops: Vec<Vec<[usize; 3]>>,
    role: FaceRole,
}

#[derive(Clone, Debug)]
struct SourceBoundaryFace {
    outer: Vec<Point3>,
    inner_loops: Vec<Vec<Point3>>,
    axis: Option<(usize, bool)>,
    role: FaceRole,
}

#[derive(Clone, Copy, Debug)]
struct FeatureSweep<'a> {
    target_face_index: usize,
    target_axis: usize,
    tangent_axes: [usize; 2],
    profile_bounds: [[f64; 2]; 2],
    target_coordinate: f64,
    feature_end: f64,
    profile: &'a [Point3],
    profile_holes: &'a [Vec<Point3>],
}

#[derive(Clone, Debug)]
struct ValidatedProfile {
    points: Vec<Point3>,
    bounds: [[f64; 2]; 2],
}

pub(crate) fn validate_face_feature_input(
    arguments: FaceFeatureArguments<'_>,
) -> Result<ValidatedFaceFeature, FaceFeatureInputError> {
    validate_face_region_feature_input(FaceRegionFeatureArguments {
        snapshot: arguments.snapshot,
        topology: arguments.topology,
        target_face: arguments.target_face,
        frame: arguments.frame,
        vertices: arguments.vertices,
        holes: &[],
        distance: arguments.distance,
        operation: arguments.operation,
        precision: arguments.precision,
    })
}

pub(crate) fn validate_face_region_feature_input(
    arguments: FaceRegionFeatureArguments<'_>,
) -> Result<ValidatedFaceFeature, FaceFeatureInputError> {
    let FaceRegionFeatureArguments {
        snapshot,
        topology,
        target_face,
        frame,
        vertices,
        holes,
        distance,
        operation,
        precision,
    } = arguments;
    if !frame.is_finite()
        || !distance.is_finite()
        || vertices.iter().any(|point| !point.is_finite())
        || holes.iter().flatten().any(|point| !point.is_finite())
    {
        return Err(FaceFeatureInputError::NonFinite);
    }
    if target_face.snapshot != snapshot {
        return Err(FaceFeatureInputError::TargetSnapshotMismatch);
    }
    if target_face.kind != EntityKind::Face {
        return Err(FaceFeatureInputError::TargetNotFace);
    }
    if distance <= 0.0 {
        return Err(FaceFeatureInputError::NonPositiveDistance);
    }

    let angular_tolerance = precision.angular_agreement_radians.max(1.0e-12);
    let linear_tolerance = precision.linear_agreement;
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let target_face_index = topology
        .faces
        .iter()
        .position(|record| record.id.get() == target_face.entity.0)
        .ok_or(FaceFeatureInputError::TargetMissing)?;
    // Everything below reads coordinates in the sketch's own frame, so the
    // solid may sit at any orientation as long as it is box-like in that
    // frame — the same domain as before, expressed relative to the sketch
    // rather than to the world.
    let basis = Basis::from_axes(
        protocol_vector(frame.u),
        protocol_vector(frame.v),
        angular_tolerance,
    )
    .ok_or(FaceFeatureInputError::FrameNotOrthonormal)?;
    let target_plane = topology.faces[target_face_index]
        .value
        .surface
        .as_plane()
        .ok_or(FaceFeatureInputError::TargetNotPlanar)?;
    // The sketch plane must be the target's plane, so the frame's own normal
    // is the target axis and the tangents are its first two axes.
    basis
        .classify(target_plane.normal, angular_tolerance)
        .filter(|(axis, _)| *axis == NORMAL_AXIS)
        .ok_or(FaceFeatureInputError::FrameOffTargetPlane)?;
    let source_faces = source_boundary_faces(basis, topology, linear_tolerance, angular_tolerance)?;
    let target = &source_faces[target_face_index];
    let (target_axis, target_positive) = target
        .axis
        .ok_or(FaceFeatureInputError::TargetNotAlignedToFrame)?;
    if target_axis != NORMAL_AXIS {
        return Err(FaceFeatureInputError::TargetNotAlignedToFrame);
    }
    let target_coordinate = basis.coordinate(target.outer[0], target_axis);
    let target_role = target.role;

    // The frame's normal must point the way the target face does, so a sketch
    // drawn on the outside stays on the outside.
    if !target_positive {
        return Err(FaceFeatureInputError::FrameOffTargetPlane);
    }
    if (basis.coordinate(protocol_point(frame.origin), target_axis) - target_coordinate).abs()
        > linear_tolerance
    {
        return Err(FaceFeatureInputError::FrameNotOnTarget);
    }

    let tangent_axes = [0, 1];
    let frame_u = basis.direction(0);
    let frame_v = basis.direction(1);
    let target_bounds = polygon_bounds(basis, &target.outer, tangent_axes);
    if target_bounds
        .iter()
        .any(|extent| extent[1] - extent[0] <= minimum)
    {
        return Err(FaceFeatureInputError::TargetDegenerate);
    }

    let frame_origin = protocol_point(frame.origin);
    let ValidatedProfile {
        points: world,
        bounds: profile_bounds,
    } = validate_profile(
        basis,
        vertices,
        frame_origin,
        frame_u,
        frame_v,
        target_axis,
        tangent_axes,
        minimum,
    )?;
    let profile_holes = holes
        .iter()
        .map(|hole| {
            validate_profile(
                basis,
                hole,
                frame_origin,
                frame_u,
                frame_v,
                target_axis,
                tangent_axes,
                minimum,
            )
            .map(|validated| validated.points)
        })
        .collect::<Result<Vec<_>, _>>()?;

    if !profile_holes_are_valid(basis, &world, &profile_holes, tangent_axes, minimum) {
        return Err(FaceFeatureInputError::ProfileHoleInvalid);
    }

    if !profile_region_is_strictly_inside_face(
        basis,
        &world,
        &profile_holes,
        target,
        tangent_axes,
        minimum,
    ) {
        return Err(FaceFeatureInputError::ProfileOutsideFace);
    }
    if distance <= minimum {
        return Err(FaceFeatureInputError::FeatureTooSmall);
    }

    let sign = if target_positive { 1.0 } else { -1.0 };
    let mut feature_end = match operation {
        FaceExtrusionOperation::Add => target_coordinate + sign * distance,
        FaceExtrusionOperation::Cut => target_coordinate - sign * distance,
    };
    if (feature_end - target_coordinate).abs() <= minimum {
        return Err(FaceFeatureInputError::NumericallyUnrepresentable);
    }
    if !feature_end.is_finite() || feature_end.abs() > precision.max_abs_coordinate {
        return Err(FaceFeatureInputError::CoordinateLimit);
    }
    if source_faces
        .iter()
        .flat_map(|face| {
            face.outer
                .iter()
                .chain(face.inner_loops.iter().flatten())
                .copied()
        })
        .flat_map(|point| [point.x, point.y, point.z])
        .chain(profile_bounds.iter().flatten().copied())
        .any(|coordinate| {
            !coordinate.is_finite() || coordinate.abs() > precision.max_abs_coordinate
        })
    {
        return Err(FaceFeatureInputError::CoordinateLimit);
    }

    let contacts = sweep_source_contacts(
        basis,
        &source_faces,
        FeatureSweep {
            target_face_index,
            target_axis,
            tangent_axes,
            profile_bounds,
            target_coordinate,
            feature_end,
            profile: &world,
            profile_holes: &profile_holes,
        },
        minimum,
    );
    let exit_face_index = match operation {
        FaceExtrusionOperation::Add if !contacts.is_empty() => {
            return Err(FaceFeatureInputError::SweepCollision);
        }
        FaceExtrusionOperation::Cut if !contacts.is_empty() => {
            let direction = -sign;
            let mut exits = contacts
                .iter()
                .copied()
                .filter_map(|index| {
                    through_exit_depth(
                        basis,
                        &source_faces[index],
                        target_axis,
                        target_positive,
                        tangent_axes,
                        &world,
                        &profile_holes,
                        target_coordinate,
                        direction,
                        minimum,
                    )
                    .map(|depth| (depth, index))
                })
                .collect::<Vec<_>>();
            exits.sort_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
            });
            let Some((exit_depth, exit_index)) = exits.first().copied() else {
                return Err(FaceFeatureInputError::CutTooDeep);
            };
            let exit_coordinate = target_coordinate + direction * exit_depth;
            let contacts_before_exit = contacts.iter().copied().any(|index| {
                index != exit_index
                    && face_bounds(basis, &source_faces[index])[target_axis]
                        .into_iter()
                        .map(|coordinate| (coordinate - target_coordinate) * direction)
                        .filter(|depth| *depth > minimum)
                        .min_by(f64::total_cmp)
                        .is_some_and(|depth| depth < exit_depth - linear_tolerance)
            });
            if contacts_before_exit {
                return Err(FaceFeatureInputError::CutTooDeep);
            }
            feature_end = exit_coordinate;
            Some(exit_index)
        }
        _ => None,
    };

    let represented_depth = (feature_end - target_coordinate).abs();
    let mut worst_error = if exit_face_index.is_some() {
        0.0
    } else {
        (represented_depth - distance).abs()
    };
    for (expected_loop, represented_loop) in std::iter::once(vertices)
        .chain(holes.iter().map(Vec::as_slice))
        .zip(std::iter::once(world.as_slice()).chain(profile_holes.iter().map(Vec::as_slice)))
    {
        let mut expected_distances = pair_distances_2d(expected_loop);
        let mut represented_distances = pair_distances_3d(represented_loop);
        expected_distances.sort_by(f64::total_cmp);
        represented_distances.sort_by(f64::total_cmp);
        for (expected, represented) in expected_distances.into_iter().zip(represented_distances) {
            worst_error = worst_error.max((represented - expected).abs());
        }
    }
    if !worst_error.is_finite() || worst_error > linear_tolerance {
        return Err(FaceFeatureInputError::NumericallyUnrepresentable);
    }

    Ok(ValidatedFaceFeature {
        basis,
        source_faces,
        target_face_index,
        target_role,
        target_axis,
        tangent_axes,
        profile: world,
        profile_holes,
        target_coordinate,
        feature_end,
        exit_face_index,
    })
}

fn pair_distances_2d(points: &[ProtocolPoint2]) -> Vec<f64> {
    let mut distances = Vec::with_capacity(points.len() * points.len().saturating_sub(1) / 2);
    for left in 0..points.len() {
        for right in left + 1..points.len() {
            distances
                .push((points[left].x - points[right].x).hypot(points[left].y - points[right].y));
        }
    }
    distances
}

fn pair_distances_3d(points: &[Point3]) -> Vec<f64> {
    let mut distances = Vec::with_capacity(points.len() * points.len().saturating_sub(1) / 2);
    for left in 0..points.len() {
        for right in left + 1..points.len() {
            distances.push(points[left].distance(points[right]));
        }
    }
    distances
}

#[allow(clippy::too_many_arguments)]
fn validate_profile(
    basis: Basis,
    vertices: &[ProtocolPoint2],
    frame_origin: Point3,
    frame_u: Vector3,
    frame_v: Vector3,
    target_axis: usize,
    tangent_axes: [usize; 2],
    minimum: f64,
) -> Result<ValidatedProfile, FaceFeatureInputError> {
    if vertices.len() < 3 {
        return Err(FaceFeatureInputError::TooFewVertices);
    }
    if vertices.len() > MAX_EXTRUSION_PROFILE_VERTICES {
        return Err(FaceFeatureInputError::TooManyVertices);
    }

    let mut profile = vertices
        .iter()
        .map(|point| Point2::new(point.x, point.y))
        .collect::<Vec<_>>();
    for index in 0..profile.len() {
        let next = profile[(index + 1) % profile.len()];
        let edge_length = (next.x - profile[index].x).hypot(next.y - profile[index].y);
        if !edge_length.is_finite() {
            return Err(FaceFeatureInputError::ProfileIndeterminate);
        }
        if edge_length <= minimum {
            return Err(FaceFeatureInputError::FeatureTooSmall);
        }
    }

    let mut closed = profile.clone();
    closed.push(profile[0]);
    let winding = match classify_profile(&closed) {
        ProfileClassification::Closed { winding } => winding,
        ProfileClassification::SelfIntersecting => {
            return Err(FaceFeatureInputError::SelfIntersecting);
        }
        ProfileClassification::Invalid(InvalidProfile::RepeatedVertex) => {
            return Err(FaceFeatureInputError::RepeatedVertex);
        }
        ProfileClassification::Invalid(InvalidProfile::TooFewVertices) => {
            return Err(FaceFeatureInputError::TooFewVertices);
        }
        ProfileClassification::Invalid(InvalidProfile::NonFiniteCoordinate) => {
            return Err(FaceFeatureInputError::NonFinite);
        }
        ProfileClassification::Open | ProfileClassification::Indeterminate => {
            return Err(FaceFeatureInputError::ProfileIndeterminate);
        }
    };
    validate_profile_separation(&profile, minimum)?;

    if winding == ProfileWinding::Clockwise {
        profile[1..].reverse();
    }
    let canonical_start = profile
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            left.x
                .total_cmp(&right.x)
                .then_with(|| left.y.total_cmp(&right.y))
        })
        .map(|(index, _)| index)
        .expect("a validated profile has a canonical start");
    profile.rotate_left(canonical_start);

    let points = profile
        .iter()
        .map(|point| frame_origin + frame_u * point.x + frame_v * point.y)
        .collect::<Vec<_>>();
    let mut bounds = [[f64::INFINITY, f64::NEG_INFINITY]; 2];
    let origin_height = basis.coordinate(frame_origin, target_axis);
    for point in &points {
        // Each profile point is built as `origin + u·x + v·y`, so its height
        // above the frame plane should be a rounding of the origin's. On an
        // axis-aligned frame that rounding is exactly zero; on a turned one it
        // is a few ulps of the coordinates involved, so the bound has to scale
        // with them rather than sit at an absolute epsilon.
        let height = basis.coordinate(*point, target_axis);
        let magnitude = [point.x, point.y, point.z, origin_height]
            .into_iter()
            .fold(1.0_f64, |largest, value| largest.max(value.abs()));
        if (height - origin_height).abs() > 8.0 * f64::EPSILON * magnitude {
            return Err(FaceFeatureInputError::ProfileIndeterminate);
        }
        for tangent in 0..2 {
            let coordinate = basis.coordinate(*point, tangent_axes[tangent]);
            bounds[tangent][0] = bounds[tangent][0].min(coordinate);
            bounds[tangent][1] = bounds[tangent][1].max(coordinate);
        }
    }
    if bounds.iter().any(|extent| extent[1] - extent[0] <= minimum) {
        return Err(FaceFeatureInputError::FeatureTooSmall);
    }
    Ok(ValidatedProfile { points, bounds })
}

fn validate_profile_separation(
    profile: &[Point2],
    minimum: f64,
) -> Result<(), FaceFeatureInputError> {
    for (vertex_index, point) in profile.iter().copied().enumerate() {
        for edge_start in 0..profile.len() {
            let edge_end = (edge_start + 1) % profile.len();
            if vertex_index == edge_start || vertex_index == edge_end {
                continue;
            }
            let distance = point_segment_distance(point, profile[edge_start], profile[edge_end]);
            if !distance.is_finite() {
                return Err(FaceFeatureInputError::ProfileIndeterminate);
            }
            if distance <= minimum {
                return Err(FaceFeatureInputError::FeatureTooSmall);
            }
        }
    }
    Ok(())
}

fn point_segment_distance(point: Point2, start: Point2, end: Point2) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx.mul_add(dx, dy * dy);
    if !length_squared.is_finite() || length_squared <= 0.0 {
        return f64::NAN;
    }
    let projection = ((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared;
    let parameter = projection.clamp(0.0, 1.0);
    (point.x - (start.x + parameter * dx)).hypot(point.y - (start.y + parameter * dy))
}

fn segments_intersect(
    first_start: Point2,
    first_end: Point2,
    second_start: Point2,
    second_end: Point2,
) -> bool {
    if first_start.x.min(first_end.x) > second_start.x.max(second_end.x)
        || second_start.x.min(second_end.x) > first_start.x.max(first_end.x)
        || first_start.y.min(first_end.y) > second_start.y.max(second_end.y)
        || second_start.y.min(second_end.y) > first_start.y.max(first_end.y)
    {
        return false;
    }
    let orientations = [
        orient2d(first_start, first_end, second_start),
        orient2d(first_start, first_end, second_end),
        orient2d(second_start, second_end, first_start),
        orient2d(second_start, second_end, first_end),
    ];
    if orientations.contains(&Orientation2::Indeterminate)
        || orientations.contains(&Orientation2::Collinear)
    {
        return true;
    }
    matches!(
        (orientations[0], orientations[1]),
        (Orientation2::Clockwise, Orientation2::CounterClockwise)
            | (Orientation2::CounterClockwise, Orientation2::Clockwise)
    ) && matches!(
        (orientations[2], orientations[3]),
        (Orientation2::Clockwise, Orientation2::CounterClockwise)
            | (Orientation2::CounterClockwise, Orientation2::Clockwise)
    )
}

fn polygon_normal(points: &[Point3]) -> Option<Vector3> {
    let mut normal = Vector3::new(0.0, 0.0, 0.0);
    for index in 0..points.len() {
        let current = points[index];
        let next = points[(index + 1) % points.len()];
        normal.x += (current.y - next.y) * (current.z + next.z);
        normal.y += (current.z - next.z) * (current.x + next.x);
        normal.z += (current.x - next.x) * (current.y + next.y);
    }
    let length = normal.length();
    (length.is_finite() && length > f64::EPSILON).then(|| normal / length)
}

pub(crate) fn build_face_feature(input: &ValidatedFaceFeature) -> Topology {
    let basis = input.basis;
    let mut coordinates = [Vec::new(), Vec::new(), Vec::new()];
    for face in &input.source_faces {
        for point in face.outer.iter().chain(face.inner_loops.iter().flatten()) {
            for (axis, values) in coordinates.iter_mut().enumerate() {
                values.push(basis.coordinate(*point, axis));
            }
        }
    }
    for tangent in 0..2 {
        coordinates[input.tangent_axes[tangent]].extend(
            input
                .profile
                .iter()
                .chain(input.profile_holes.iter().flatten())
                .map(|point| basis.coordinate(*point, input.tangent_axes[tangent])),
        );
    }
    coordinates[input.target_axis].push(input.target_coordinate);
    coordinates[input.target_axis].push(input.feature_end);
    for axis_coordinates in &mut coordinates {
        axis_coordinates.sort_by(f64::total_cmp);
        axis_coordinates.dedup_by(|left, right| *left == *right);
    }

    let end_index = coordinate_index(&coordinates[input.target_axis], input.feature_end);
    let start_index = coordinate_index(&coordinates[input.target_axis], input.target_coordinate);
    let on_plane = |point: Point3| {
        grid_vertex_on_plane(basis, &coordinates, point, input.target_axis, start_index)
    };

    let profile_edge_count =
        input.profile.len() + input.profile_holes.iter().map(Vec::len).sum::<usize>();
    let mut boundary_faces = Vec::with_capacity(input.source_faces.len() + profile_edge_count + 2);

    // A strict-inset, collision-free local feature leaves every non-target
    // face geometrically unchanged. Re-emitting those exact oriented polygons
    // retains all earlier features instead of reconstructing a global box.
    for (index, source) in input.source_faces.iter().enumerate() {
        if index == input.target_face_index || input.exit_face_index == Some(index) {
            continue;
        }
        boundary_faces.push(BoundaryFace {
            outer: source
                .outer
                .iter()
                .copied()
                .map(|point| grid_vertex(basis, &coordinates, point))
                .collect(),
            inner_loops: source
                .inner_loops
                .iter()
                .map(|loop_points| {
                    loop_points
                        .iter()
                        .copied()
                        .map(|point| grid_vertex(basis, &coordinates, point))
                        .collect()
                })
                .collect(),
            role: source.role,
        });
    }

    let inner_ring = input
        .profile
        .iter()
        .copied()
        .map(on_plane)
        .collect::<Vec<_>>();
    let mut known_points = BTreeMap::new();
    for source in &input.source_faces {
        for point in source
            .outer
            .iter()
            .chain(source.inner_loops.iter().flatten())
        {
            known_points.insert(grid_vertex(basis, &coordinates, *point), *point);
        }
    }
    for (grid, point) in inner_ring.iter().zip(&input.profile) {
        known_points.insert(*grid, *point);
    }
    let hole_rings = input
        .profile_holes
        .iter()
        .map(|hole| hole.iter().copied().map(on_plane).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    for (ring, hole) in hole_rings.iter().zip(input.profile_holes.iter()) {
        for (grid, point) in ring.iter().zip(hole) {
            known_points.insert(*grid, *point);
        }
    }
    let target = &input.source_faces[input.target_face_index];
    let (target_main_holes, target_island_holes) = partition_source_holes(
        basis,
        &target.inner_loops,
        &input.profile,
        &input.profile_holes,
        input.tangent_axes,
    );
    let mut target_holes = target_main_holes
        .iter()
        .map(|index| {
            target.inner_loops[*index]
                .iter()
                .copied()
                .map(|point| grid_vertex(basis, &coordinates, point))
                .collect()
        })
        .collect::<Vec<Vec<[usize; 3]>>>();
    let mut profile_hole = inner_ring.clone();
    profile_hole.reverse();
    target_holes.push(profile_hole);
    boundary_faces.push(BoundaryFace {
        outer: target
            .outer
            .iter()
            .copied()
            .map(|point| grid_vertex(basis, &coordinates, point))
            .collect(),
        inner_loops: target_holes,
        role: input.target_role,
    });

    // Material inside each profile hole remains on the support plane. Source
    // voids wholly contained by that profile void migrate onto this island,
    // preserving existing bosses/pockets rather than filling or duplicating
    // their boundary loops.
    for (hole_index, hole_ring) in hole_rings.iter().enumerate() {
        let inherited_holes = target_island_holes[hole_index]
            .iter()
            .map(|index| {
                target.inner_loops[*index]
                    .iter()
                    .copied()
                    .map(|point| grid_vertex(basis, &coordinates, point))
                    .collect()
            })
            .collect();
        boundary_faces.push(BoundaryFace {
            outer: hole_ring.clone(),
            inner_loops: inherited_holes,
            role: input.target_role,
        });
    }

    let end_ring = inner_ring
        .iter()
        .map(|grid| {
            let mut end = *grid;
            end[input.target_axis] = end_index;
            end
        })
        .collect::<Vec<_>>();
    let end_hole_rings = hole_rings
        .iter()
        .map(|hole| {
            hole.iter()
                .map(|grid| {
                    let mut end = *grid;
                    end[input.target_axis] = end_index;
                    end
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if let Some(exit_face_index) = input.exit_face_index {
        let exit = &input.source_faces[exit_face_index];
        let (exit_main_holes, exit_island_holes) = partition_source_holes(
            basis,
            &exit.inner_loops,
            &input.profile,
            &input.profile_holes,
            input.tangent_axes,
        );
        let mut exit_holes = exit_main_holes
            .iter()
            .map(|index| {
                exit.inner_loops[*index]
                    .iter()
                    .copied()
                    .map(|point| grid_vertex(basis, &coordinates, point))
                    .collect()
            })
            .collect::<Vec<Vec<[usize; 3]>>>();
        exit_holes.push(end_ring.clone());
        boundary_faces.push(BoundaryFace {
            outer: exit
                .outer
                .iter()
                .copied()
                .map(|point| grid_vertex(basis, &coordinates, point))
                .collect(),
            inner_loops: exit_holes,
            role: exit.role,
        });
        for (hole_index, hole_ring) in end_hole_rings.iter().enumerate() {
            let mut exit_island = hole_ring.clone();
            exit_island.reverse();
            let inherited_holes = exit_island_holes[hole_index]
                .iter()
                .map(|index| {
                    exit.inner_loops[*index]
                        .iter()
                        .copied()
                        .map(|point| grid_vertex(basis, &coordinates, point))
                        .collect()
                })
                .collect();
            boundary_faces.push(BoundaryFace {
                outer: exit_island,
                inner_loops: inherited_holes,
                role: exit.role,
            });
        }
    }
    if input.exit_face_index.is_none() {
        let end_holes = end_hole_rings
            .iter()
            .map(|ring| {
                let mut reversed = ring.clone();
                reversed.reverse();
                reversed
            })
            .collect();
        boundary_faces.push(BoundaryFace {
            outer: end_ring.clone(),
            inner_loops: end_holes,
            role: FaceRole::FeatureEnd,
        });
    }

    // The same winding works for both modes. Reversing the depth direction for
    // Cut naturally reverses each wall normal into the pocket void.
    let mut side_ordinal = 0_u32;
    for index in 0..inner_ring.len() {
        let next = (index + 1) % inner_ring.len();
        boundary_faces.push(BoundaryFace {
            outer: vec![
                inner_ring[index],
                inner_ring[next],
                end_ring[next],
                end_ring[index],
            ],
            inner_loops: Vec::new(),
            role: FaceRole::FeatureSide(side_ordinal),
        });
        side_ordinal += 1;
    }
    for hole_ring in &hole_rings {
        let end_hole_ring = hole_ring
            .iter()
            .map(|grid| {
                let mut end = *grid;
                end[input.target_axis] = end_index;
                end
            })
            .collect::<Vec<_>>();
        for index in 0..hole_ring.len() {
            let next = (index + 1) % hole_ring.len();
            boundary_faces.push(BoundaryFace {
                outer: vec![
                    hole_ring[next],
                    hole_ring[index],
                    end_hole_ring[index],
                    end_hole_ring[next],
                ],
                inner_loops: Vec::new(),
                role: FaceRole::FeatureSide(side_ordinal),
            });
            side_ordinal += 1;
        }
    }

    topology_from_boundary_faces(basis, &coordinates, &boundary_faces, &known_points)
}

fn source_boundary_faces(
    basis: Basis,
    topology: &Topology,
    linear_tolerance: f64,
    angular_tolerance: f64,
) -> Result<Vec<SourceBoundaryFace>, FaceFeatureInputError> {
    if topology.shells.len() != 1 || topology.solids.len() != 1 || topology.faces.is_empty() {
        return Err(FaceFeatureInputError::SourceUnsupported);
    }

    let mut source_faces = Vec::with_capacity(topology.faces.len());
    for face in &topology.faces {
        let outer = validator::face_polygon(topology, face.value.outer_loop)
            .ok_or(FaceFeatureInputError::SourceUnsupported)?;
        let inner_loops = face
            .value
            .inner_loops
            .iter()
            .copied()
            .map(|loop_key| {
                validator::face_polygon(topology, loop_key)
                    .ok_or(FaceFeatureInputError::SourceUnsupported)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if outer.len() < 3 || inner_loops.iter().any(|polygon| polygon.len() < 3) {
            return Err(FaceFeatureInputError::SourceUnsupported);
        }
        if outer
            .iter()
            .chain(inner_loops.iter().flatten())
            .any(|point| !point.is_finite())
        {
            return Err(FaceFeatureInputError::NonFinite);
        }
        let plane = face
            .value
            .surface
            .as_plane()
            .ok_or(FaceFeatureInputError::SourceUnsupported)?;
        let axis = basis.classify(plane.normal, angular_tolerance);
        if let Some((axis, _)) = axis {
            let coordinate = basis.coordinate(outer[0], axis);
            if outer
                .iter()
                .chain(inner_loops.iter().flatten())
                .any(|point| (basis.coordinate(*point, axis) - coordinate).abs() > linear_tolerance)
            {
                return Err(FaceFeatureInputError::SourceUnsupported);
            }
        }
        source_faces.push(SourceBoundaryFace {
            outer,
            inner_loops,
            axis,
            role: face.value.role,
        });
    }
    Ok(source_faces)
}

fn polygon_bounds(basis: Basis, points: &[Point3], tangent_axes: [usize; 2]) -> [[f64; 2]; 2] {
    let mut bounds = [[f64::INFINITY, f64::NEG_INFINITY]; 2];
    for point in points {
        for tangent in 0..2 {
            let coordinate = basis.coordinate(*point, tangent_axes[tangent]);
            bounds[tangent][0] = bounds[tangent][0].min(coordinate);
            bounds[tangent][1] = bounds[tangent][1].max(coordinate);
        }
    }
    bounds
}

fn profile_region_is_strictly_inside_face(
    basis: Basis,
    profile: &[Point3],
    profile_holes: &[Vec<Point3>],
    target: &SourceBoundaryFace,
    tangent_axes: [usize; 2],
    minimum: f64,
) -> bool {
    let project = |point: Point3| {
        Point2::new(
            basis.coordinate(point, tangent_axes[0]),
            basis.coordinate(point, tangent_axes[1]),
        )
    };
    let profile = profile.iter().copied().map(project).collect::<Vec<_>>();
    let profile_holes = profile_holes
        .iter()
        .map(|hole| hole.iter().copied().map(project).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let outer = target
        .outer
        .iter()
        .copied()
        .map(project)
        .collect::<Vec<_>>();
    let holes = target
        .inner_loops
        .iter()
        .map(|hole| hole.iter().copied().map(project).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if profile
        .iter()
        .copied()
        .any(|point| !point_in_polygon(point, &outer))
        || polygon_boundaries_touch_or_too_close(&profile, &outer, minimum)
    {
        return false;
    }

    let profile_boundaries = std::iter::once(profile.as_slice())
        .chain(profile_holes.iter().map(Vec::as_slice))
        .collect::<Vec<_>>();
    for target_hole in &holes {
        // A source void is harmless when it lies wholly inside a profile
        // void. Any source void boundary touching the swept material, or any
        // source void contained by swept material, makes the region invalid.
        if profile_boundaries
            .iter()
            .any(|boundary| polygon_boundaries_touch_or_too_close(boundary, target_hole, minimum))
            || profile
                .iter()
                .copied()
                .any(|point| point_in_polygon(point, target_hole))
            || target_hole
                .first()
                .copied()
                .is_some_and(|point| point_in_region(point, &profile, &profile_holes))
        {
            return false;
        }
    }
    true
}

fn profile_holes_are_valid(
    basis: Basis,
    outer: &[Point3],
    holes: &[Vec<Point3>],
    tangent_axes: [usize; 2],
    minimum: f64,
) -> bool {
    let project = |point: Point3| {
        Point2::new(
            basis.coordinate(point, tangent_axes[0]),
            basis.coordinate(point, tangent_axes[1]),
        )
    };
    let outer = outer.iter().copied().map(project).collect::<Vec<_>>();
    let holes = holes
        .iter()
        .map(|hole| hole.iter().copied().map(project).collect::<Vec<_>>())
        .collect::<Vec<_>>();

    for hole in &holes {
        if hole
            .iter()
            .copied()
            .any(|point| !point_in_polygon(point, &outer))
            || polygon_boundaries_touch_or_too_close(&outer, hole, minimum)
        {
            return false;
        }
    }
    for left in 0..holes.len() {
        for right in left + 1..holes.len() {
            if holes[left]
                .iter()
                .copied()
                .any(|point| point_in_polygon(point, &holes[right]))
                || holes[right]
                    .iter()
                    .copied()
                    .any(|point| point_in_polygon(point, &holes[left]))
                || polygon_boundaries_touch_or_too_close(&holes[left], &holes[right], minimum)
            {
                return false;
            }
        }
    }
    true
}

fn partition_source_holes(
    basis: Basis,
    source_holes: &[Vec<Point3>],
    profile: &[Point3],
    profile_holes: &[Vec<Point3>],
    tangent_axes: [usize; 2],
) -> (Vec<usize>, Vec<Vec<usize>>) {
    let profile = project_loop(basis, profile, tangent_axes);
    let profile_holes = profile_holes
        .iter()
        .map(|hole| project_loop(basis, hole, tangent_axes))
        .collect::<Vec<_>>();
    let mut main = Vec::new();
    let mut islands = vec![Vec::new(); profile_holes.len()];
    for (source_index, source_hole) in source_holes.iter().enumerate() {
        let Some(point) = source_hole
            .first()
            .copied()
            .map(|point| project_loop(basis, &[point], tangent_axes)[0])
        else {
            continue;
        };
        if point_in_polygon(point, &profile)
            && let Some(hole_index) = profile_holes
                .iter()
                .position(|hole| point_in_polygon(point, hole))
        {
            islands[hole_index].push(source_index);
        } else {
            main.push(source_index);
        }
    }
    (main, islands)
}

fn polygon_boundaries_touch_or_too_close(
    first: &[Point2],
    second: &[Point2],
    minimum: f64,
) -> bool {
    (0..first.len()).any(|first_index| {
        let first_next = (first_index + 1) % first.len();
        (0..second.len()).any(|second_index| {
            let second_next = (second_index + 1) % second.len();
            segments_intersect(
                first[first_index],
                first[first_next],
                second[second_index],
                second[second_next],
            ) || point_segment_distance(
                first[first_index],
                second[second_index],
                second[second_next],
            ) <= minimum
                || point_segment_distance(
                    second[second_index],
                    first[first_index],
                    first[first_next],
                ) <= minimum
        })
    })
}

fn sweep_source_contacts(
    basis: Basis,
    source_faces: &[SourceBoundaryFace],
    sweep: FeatureSweep<'_>,
    clearance: f64,
) -> Vec<usize> {
    let mut prism = [[0.0; 2]; 3];
    prism[sweep.target_axis] = [
        sweep.target_coordinate.min(sweep.feature_end),
        sweep.target_coordinate.max(sweep.feature_end),
    ];
    for tangent in 0..2 {
        prism[sweep.tangent_axes[tangent]] = sweep.profile_bounds[tangent];
    }

    source_faces
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != sweep.target_face_index)
        .filter(|(_, face)| {
            let bounds = face_bounds(basis, face);
            let lies_on_start_plane = bounds[sweep.target_axis]
                .iter()
                .all(|coordinate| (*coordinate - sweep.target_coordinate).abs() <= clearance);
            if lies_on_start_plane {
                return false;
            }
            let bounds_overlap = !(0..3).any(|axis| {
                bounds[axis][1] <= prism[axis][0] - clearance
                    || bounds[axis][0] >= prism[axis][1] + clearance
            });
            if !bounds_overlap {
                return false;
            }
            if face.axis.is_some_and(|(axis, _)| axis == sweep.target_axis) {
                return planar_face_footprint_overlaps(
                    basis,
                    face,
                    sweep.profile,
                    sweep.profile_holes,
                    sweep.tangent_axes,
                );
            }
            projected_face_boundary_contacts_region(
                basis,
                face,
                sweep.profile,
                sweep.profile_holes,
                sweep.tangent_axes,
            )
        })
        .map(|(index, _)| index)
        .collect()
}

fn planar_face_footprint_overlaps(
    basis: Basis,
    face: &SourceBoundaryFace,
    footprint: &[Point3],
    footprint_holes: &[Vec<Point3>],
    tangent_axes: [usize; 2],
) -> bool {
    let face_outer = project_loop(basis, &face.outer, tangent_axes);
    let face_holes = face
        .inner_loops
        .iter()
        .map(|hole| project_loop(basis, hole, tangent_axes))
        .collect::<Vec<_>>();
    let footprint_outer = project_loop(basis, footprint, tangent_axes);
    let footprint_holes = footprint_holes
        .iter()
        .map(|hole| project_loop(basis, hole, tangent_axes))
        .collect::<Vec<_>>();
    material_regions_overlap(&face_outer, &face_holes, &footprint_outer, &footprint_holes)
}

fn projected_face_boundary_contacts_region(
    basis: Basis,
    face: &SourceBoundaryFace,
    footprint: &[Point3],
    footprint_holes: &[Vec<Point3>],
    tangent_axes: [usize; 2],
) -> bool {
    let footprint_outer = project_loop(basis, footprint, tangent_axes);
    let footprint_holes = footprint_holes
        .iter()
        .map(|hole| project_loop(basis, hole, tangent_axes))
        .collect::<Vec<_>>();
    let footprint_boundaries = std::iter::once(footprint_outer.as_slice())
        .chain(footprint_holes.iter().map(Vec::as_slice))
        .collect::<Vec<_>>();
    std::iter::once(face.outer.as_slice())
        .chain(face.inner_loops.iter().map(Vec::as_slice))
        .map(|boundary| project_loop(basis, boundary, tangent_axes))
        .any(|boundary| {
            boundary
                .iter()
                .copied()
                .any(|point| point_in_region(point, &footprint_outer, &footprint_holes))
                || footprint_boundaries
                    .iter()
                    .any(|region_boundary| polygon_boundaries_intersect(&boundary, region_boundary))
        })
}

fn material_regions_overlap(
    first_outer: &[Point2],
    first_holes: &[Vec<Point2>],
    second_outer: &[Point2],
    second_holes: &[Vec<Point2>],
) -> bool {
    let first_boundaries = std::iter::once(first_outer)
        .chain(first_holes.iter().map(Vec::as_slice))
        .collect::<Vec<_>>();
    let second_boundaries = std::iter::once(second_outer)
        .chain(second_holes.iter().map(Vec::as_slice))
        .collect::<Vec<_>>();
    first_boundaries.iter().any(|first| {
        second_boundaries
            .iter()
            .any(|second| polygon_boundaries_intersect(first, second))
    }) || first_boundaries
        .iter()
        .flat_map(|boundary| boundary.iter())
        .copied()
        .any(|point| point_in_region(point, second_outer, second_holes))
        || second_boundaries
            .iter()
            .flat_map(|boundary| boundary.iter())
            .copied()
            .any(|point| point_in_region(point, first_outer, first_holes))
}

fn polygon_boundaries_intersect(first: &[Point2], second: &[Point2]) -> bool {
    (0..first.len()).any(|first_index| {
        (0..second.len()).any(|second_index| {
            segments_intersect(
                first[first_index],
                first[(first_index + 1) % first.len()],
                second[second_index],
                second[(second_index + 1) % second.len()],
            )
        })
    })
}

fn project_loop(basis: Basis, points: &[Point3], tangent_axes: [usize; 2]) -> Vec<Point2> {
    points
        .iter()
        .map(|point| {
            Point2::new(
                basis.coordinate(*point, tangent_axes[0]),
                basis.coordinate(*point, tangent_axes[1]),
            )
        })
        .collect()
}

fn point_in_region(point: Point2, outer: &[Point2], holes: &[Vec<Point2>]) -> bool {
    point_in_polygon(point, outer) && !holes.iter().any(|hole| point_in_polygon(point, hole))
}

fn point_in_polygon(point: Point2, polygon: &[Point2]) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let first = polygon[index];
        let second = polygon[(index + 1) % polygon.len()];
        if point_segment_distance(point, first, second) == 0.0 {
            return true;
        }
        let crosses = (first.y > point.y) != (second.y > point.y)
            && point.x
                < (second.x - first.x) * (point.y - first.y) / (second.y - first.y) + first.x;
        if crosses {
            inside = !inside;
        }
    }
    inside
}

fn face_bounds(basis: Basis, face: &SourceBoundaryFace) -> [[f64; 2]; 3] {
    let mut bounds = [[f64::INFINITY, f64::NEG_INFINITY]; 3];
    for point in face.outer.iter().chain(face.inner_loops.iter().flatten()) {
        for (axis, extent) in bounds.iter_mut().enumerate() {
            let coordinate = basis.coordinate(*point, axis);
            extent[0] = extent[0].min(coordinate);
            extent[1] = extent[1].max(coordinate);
        }
    }
    bounds
}

#[allow(clippy::too_many_arguments)]
fn through_exit_depth(
    basis: Basis,
    face: &SourceBoundaryFace,
    target_axis: usize,
    target_positive: bool,
    tangent_axes: [usize; 2],
    profile: &[Point3],
    profile_holes: &[Vec<Point3>],
    target_coordinate: f64,
    direction: f64,
    minimum: f64,
) -> Option<f64> {
    let (axis, positive) = face.axis?;
    if axis != target_axis || positive == target_positive {
        return None;
    }
    let coordinate = basis.coordinate(face.outer[0], target_axis);
    let depth = (coordinate - target_coordinate) * direction;
    if depth <= minimum {
        return None;
    }
    profile_region_is_strictly_inside_face(
        basis,
        profile,
        profile_holes,
        face,
        tangent_axes,
        minimum,
    )
    .then_some(depth)
}

fn topology_from_boundary_faces(
    basis: Basis,
    coordinates: &[Vec<f64>; 3],
    boundary_faces: &[BoundaryFace],
    known: &BTreeMap<[usize; 3], Point3>,
) -> Topology {
    let mut next_id = 1_u64;
    let mut grid_vertices = BTreeSet::new();
    for face in boundary_faces {
        grid_vertices.extend(face.outer.iter().copied());
        grid_vertices.extend(face.inner_loops.iter().flatten().copied());
    }
    let mut vertex_keys = BTreeMap::new();
    let mut vertices = Vec::with_capacity(grid_vertices.len());
    for grid in grid_vertices {
        let key = VertexKey(vertices.len());
        vertex_keys.insert(grid, key);
        vertices.push(Record {
            id: allocate_id(&mut next_id),
            value: Vertex {
                // A grid node that came from an input point keeps that
                // point exactly. Reconstructing it from its own coordinates
                // would round-trip through the frame and move it by ulps,
                // which is enough to break the identity the history mapping
                // and the source-corner audit rely on.
                point: known
                    .get(&grid)
                    .copied()
                    .unwrap_or_else(|| grid_point(basis, coordinates, grid)),
            },
        });
    }

    let mut edge_pairs = BTreeSet::new();
    for face in boundary_faces {
        for boundary_loop in
            std::iter::once(face.outer.as_slice()).chain(face.inner_loops.iter().map(Vec::as_slice))
        {
            for index in 0..boundary_loop.len() {
                let first = vertex_keys[&boundary_loop[index]].0;
                let second = vertex_keys[&boundary_loop[(index + 1) % boundary_loop.len()]].0;
                edge_pairs.insert(ordered_pair(first, second));
            }
        }
    }
    let mut edge_keys = BTreeMap::new();
    let mut edges = Vec::with_capacity(edge_pairs.len());
    for pair in edge_pairs {
        let key = EdgeKey(edges.len());
        edge_keys.insert(pair, key);
        let vertex_pair = [VertexKey(pair.0), VertexKey(pair.1)];
        edges.push(Record {
            id: allocate_id(&mut next_id),
            value: Edge::line(
                vertex_pair,
                vertex_pair.map(|key| vertices[key.0].value.point),
            ),
        });
    }

    let coedge_count = boundary_faces
        .iter()
        .map(|face| face.outer.len() + face.inner_loops.iter().map(Vec::len).sum::<usize>())
        .sum();
    let mut coedges = Vec::with_capacity(coedge_count);
    let mut loops = Vec::with_capacity(boundary_faces.len());
    let mut faces = Vec::with_capacity(boundary_faces.len());
    for boundary in boundary_faces {
        let outer_points = boundary
            .outer
            .iter()
            .map(|grid| vertices[vertex_keys[grid].0].value.point)
            .collect::<Vec<_>>();
        let normal =
            polygon_normal(&outer_points).expect("validated boundary face has an area normal");
        let u = normalized(outer_points[1] - outer_points[0]);
        let v = normal.cross(u);
        let plane = Plane::new(outer_points[0], u, v);
        let outer_loop = append_boundary_loop(
            &boundary.outer,
            plane,
            &vertex_keys,
            &vertices,
            &edge_keys,
            &mut coedges,
            &mut loops,
            &mut next_id,
        );
        let inner_loops = boundary
            .inner_loops
            .iter()
            .map(|boundary_loop| {
                append_boundary_loop(
                    boundary_loop,
                    plane,
                    &vertex_keys,
                    &vertices,
                    &edge_keys,
                    &mut coedges,
                    &mut loops,
                    &mut next_id,
                )
            })
            .collect();
        faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: Surface::Plane(plane),
                outer_loop,
                inner_loops,
                role: boundary.role,
            },
        });
    }

    // A through-cut profile containing holes can leave exact disconnected
    // material islands. Partition the validated boundary by shared-edge
    // connectivity so each closed component becomes its own shell/solid
    // instead of publishing one invalid disconnected shell.
    let mut edge_faces = vec![Vec::<FaceKey>::new(); edges.len()];
    for (face_index, face) in faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            for coedge_key in &loops[loop_key.0].value.coedges {
                edge_faces[coedges[coedge_key.0].value.edge.0].push(FaceKey(face_index));
            }
        }
    }
    let mut visited = vec![false; faces.len()];
    let mut shell_faces = Vec::<Vec<FaceKey>>::new();
    for seed in 0..faces.len() {
        if visited[seed] {
            continue;
        }
        visited[seed] = true;
        let mut queue = VecDeque::from([FaceKey(seed)]);
        let mut component = Vec::new();
        while let Some(face_key) = queue.pop_front() {
            component.push(face_key);
            for loop_key in faces[face_key.0].value.loops() {
                for coedge_key in &loops[loop_key.0].value.coedges {
                    for adjacent in &edge_faces[coedges[coedge_key.0].value.edge.0] {
                        if !visited[adjacent.0] {
                            visited[adjacent.0] = true;
                            queue.push_back(*adjacent);
                        }
                    }
                }
            }
        }
        component.sort_by_key(|key| key.0);
        shell_faces.push(component);
    }
    let shells = shell_faces
        .into_iter()
        .map(|faces| Record {
            id: allocate_id(&mut next_id),
            value: Shell { faces },
        })
        .collect::<Vec<_>>();
    let solids = (0..shells.len())
        .map(|index| Record {
            id: allocate_id(&mut next_id),
            value: Solid {
                outer_shell: ShellKey(index),
                inner_shells: Vec::new(),
            },
        })
        .collect();
    Topology {
        vertices,
        edges,
        coedges,
        loops,
        faces,
        shells,
        solids,
    }
}

#[allow(clippy::too_many_arguments)]
fn append_boundary_loop(
    boundary: &[[usize; 3]],
    plane: Plane,
    vertex_keys: &BTreeMap<[usize; 3], VertexKey>,
    vertices: &[Record<Vertex>],
    edge_keys: &BTreeMap<(usize, usize), EdgeKey>,
    coedges: &mut Vec<Record<Coedge>>,
    loops: &mut Vec<Record<Loop>>,
    next_id: &mut u64,
) -> LoopKey {
    let keys = boundary
        .iter()
        .map(|grid| vertex_keys[grid])
        .collect::<Vec<_>>();
    let points = keys
        .iter()
        .map(|key| vertices[key.0].value.point)
        .collect::<Vec<_>>();
    let mut loop_coedges = Vec::with_capacity(keys.len());
    for index in 0..keys.len() {
        let first = keys[index];
        let second = keys[(index + 1) % keys.len()];
        let pair = ordered_pair(first.0, second.0);
        let edge_key = edge_keys[&pair];
        let orientation = if pair == (first.0, second.0) {
            Orientation::Forward
        } else {
            Orientation::Reverse
        };
        let curve_endpoints = [points[index], points[(index + 1) % points.len()]];
        let coedge_key = CoedgeKey(coedges.len());
        coedges.push(Record {
            id: allocate_id(next_id),
            value: Coedge::line(
                edge_key,
                orientation,
                curve_endpoints.map(|point| plane.project(point)),
            ),
        });
        loop_coedges.push(coedge_key);
    }
    let loop_key = LoopKey(loops.len());
    loops.push(Record {
        id: allocate_id(next_id),
        value: Loop {
            coedges: loop_coedges,
        },
    });
    loop_key
}

/// The right-handed orthonormal frame every coordinate in this module is
/// read against: the sketch frame's own `u`, `v`, and normal.
///
/// The regularized face-feature algorithm reasons in axis-aligned intervals
/// and grids, which is what keeps it exact and tolerance-free. Nothing about
/// it requires those axes to be the *world* axes, though — only that one frame
/// is used consistently. Resolving the axis index against the sketch frame
/// therefore lifts the operation onto arbitrarily oriented solids without
/// changing a single interval test.
/// The frame axis a sketch's own normal occupies.
const NORMAL_AXIS: usize = 2;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Basis {
    directions: [Vector3; 3],
}

impl Basis {
    /// Builds the frame from a sketch's own axes, rejecting any pair that is
    /// not orthonormal — a skewed frame would make the interval arithmetic
    /// below meaningless rather than merely rotated.
    fn from_axes(u: Vector3, v: Vector3, tolerance: f64) -> Option<Self> {
        let (u_length, v_length) = (u.length(), v.length());
        if !u_length.is_finite() || !v_length.is_finite() {
            return None;
        }
        if u_length <= f64::EPSILON || v_length <= f64::EPSILON {
            return None;
        }
        let u = u / u_length;
        let v = v / v_length;
        if u.dot(v).abs() > tolerance {
            return None;
        }
        Some(Self {
            directions: [u, v, u.cross(v)],
        })
    }

    const fn direction(self, axis: usize) -> Vector3 {
        self.directions[axis]
    }

    fn coordinate(self, point: Point3, axis: usize) -> f64 {
        point.as_vector().dot(self.directions[axis])
    }

    /// Which of the three axes a direction lies along, and in which sense, or
    /// `None` when it lies along none of them.
    fn classify(self, vector: Vector3, tolerance: f64) -> Option<(usize, bool)> {
        let length = vector.length();
        if !length.is_finite() || length <= f64::EPSILON {
            return None;
        }
        let normalized = vector / length;
        let components = [0, 1, 2].map(|axis| normalized.dot(self.directions[axis]));
        let axis = components
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.abs().total_cmp(&right.1.abs()))?
            .0;
        if (components[axis].abs() - 1.0).abs() > tolerance
            || components
                .iter()
                .enumerate()
                .any(|(index, value)| index != axis && value.abs() > tolerance)
        {
            return None;
        }
        Some((axis, components[axis].is_sign_positive()))
    }

    /// Rebuilds a point from one coordinate per axis. Exact for an orthonormal
    /// frame, where a point is the sum of its own projections.
    fn point(self, coordinates: [f64; 3]) -> Point3 {
        let sum = self.directions[0] * coordinates[0]
            + self.directions[1] * coordinates[1]
            + self.directions[2] * coordinates[2];
        Point3::new(sum.x, sum.y, sum.z)
    }
}

fn normalized(vector: Vector3) -> Vector3 {
    vector / vector.length()
}

fn grid_point(basis: Basis, coordinates: &[Vec<f64>; 3], grid: [usize; 3]) -> Point3 {
    basis.point([
        coordinates[0][grid[0]],
        coordinates[1][grid[1]],
        coordinates[2][grid[2]],
    ])
}

fn grid_vertex(basis: Basis, coordinates: &[Vec<f64>; 3], point: Point3) -> [usize; 3] {
    [0, 1, 2].map(|axis| coordinate_index(&coordinates[axis], basis.coordinate(point, axis)))
}

/// The scaffold index of a profile point, whose height above the frame plane
/// is known rather than looked up.
///
/// The scaffold identifies vertices by exact coordinate equality, which is
/// what keeps it tolerance-free. A profile point's two tangent coordinates
/// went into the scaffold from this very call, so they match bit for bit; its
/// third coordinate did not, and on a turned frame recomputing it lands a few
/// ulps from the plane's own. Passing that index in keeps the identification
/// exact instead of reintroducing a nearest-match search.
fn grid_vertex_on_plane(
    basis: Basis,
    coordinates: &[Vec<f64>; 3],
    point: Point3,
    target_axis: usize,
    target_index: usize,
) -> [usize; 3] {
    [0, 1, 2].map(|axis| {
        if axis == target_axis {
            target_index
        } else {
            coordinate_index(&coordinates[axis], basis.coordinate(point, axis))
        }
    })
}

const fn ordered_pair(first: usize, second: usize) -> (usize, usize) {
    if first < second {
        (first, second)
    } else {
        (second, first)
    }
}

fn coordinate_index(coordinates: &[f64], wanted: f64) -> usize {
    coordinates
        .iter()
        .position(|coordinate| *coordinate == wanted)
        .expect("validated feature coordinates are present in the scaffold")
}

fn allocate_id(next: &mut u64) -> EntityId {
    let id = EntityId::from_raw(*next);
    *next += 1;
    id
}

const fn protocol_point(point: artificer_protocol::Point3) -> Point3 {
    Point3::new(point.x, point.y, point.z)
}

const fn protocol_vector(vector: artificer_protocol::Vector3) -> Vector3 {
    Vector3::new(vector.x, vector.y, vector.z)
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        CURRENT_PROTOCOL_VERSION, ExecuteRequest, HistoryRelation, KernelCommand, KernelErrorCode,
        PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3 as ProtocolPoint3, RequestId,
    };

    use super::*;
    use crate::{CancellationToken, FaceRole, NativeKernel, Snapshot, ValidationProfile};

    fn cuboid() -> Snapshot {
        cuboid_at(ProtocolPoint3::new(0.0, 0.0, 0.0))
    }

    fn cuboid_at(origin: ProtocolPoint3) -> Snapshot {
        let input = NativeKernel::empty();
        NativeKernel::execute(
            &input,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("face-feature-base"),
                expected_snapshot: input.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeCuboid {
                    origin,
                    size_x: 10.0,
                    size_y: 8.0,
                    size_z: 6.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("fixture cuboid")
        .snapshot
    }

    fn positive_z_support(snapshot: &Snapshot) -> crate::PlanarFaceSupport {
        support_by_role(snapshot, FaceRole::PositiveZ)
    }

    fn support_by_role(snapshot: &Snapshot, role: FaceRole) -> crate::PlanarFaceSupport {
        let face = NativeKernel::debug_scene(snapshot)
            .triangles
            .iter()
            .find(|triangle| triangle.role == role)
            .expect("requested cuboid face")
            .source_face;
        NativeKernel::planar_face_support(snapshot, face).expect("exact planar support")
    }

    fn feature_request(
        snapshot: &Snapshot,
        operation: FaceExtrusionOperation,
        distance: f64,
    ) -> ExecuteRequest {
        let support = positive_z_support(snapshot);
        ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(match operation {
                FaceExtrusionOperation::Add => "face-feature-add",
                FaceExtrusionOperation::Cut => "face-feature-cut",
            }),
            expected_snapshot: snapshot.id(),
            precision: snapshot.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudeFaceProfile {
                target_face: support.face,
                frame: support.frame,
                vertices: vec![
                    Point2::new(-2.0, -1.0),
                    Point2::new(2.0, -1.0),
                    Point2::new(2.0, 1.0),
                    Point2::new(-2.0, 1.0),
                ],
                distance,
                operation,
            },
        }
    }

    #[test]
    fn a_rotated_solid_takes_an_exact_face_feature_in_its_own_frame() {
        // Coordinates are read in the sketch's frame, not the world's, so a
        // solid turned off every world axis behaves exactly as the aligned
        // fixture does — same volume change, same topology counts.
        let aligned = cuboid();
        let turn = 0.7_f64;
        let (sin, cos) = (turn / 2.0).sin_cos();
        let scale = sin / 3.0f64.sqrt();
        let rotated = NativeKernel::execute(
            &aligned,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("face-feature-rotate"),
                expected_snapshot: aligned.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::TransformSnapshot {
                    transform: artificer_protocol::SimilarityTransform3 {
                        translation: artificer_protocol::Vector3::new(0.0, 0.0, 0.0),
                        rotation: artificer_protocol::RotationQuaternion::new(
                            cos, scale, scale, scale,
                        ),
                        uniform_scale: 1.0,
                    },
                },
            },
            &CancellationToken::new(),
        )
        .expect("a rotation is always exact")
        .snapshot;

        for (operation, sign) in [
            (FaceExtrusionOperation::Add, 1.0),
            (FaceExtrusionOperation::Cut, -1.0),
        ] {
            let aligned_result = NativeKernel::execute(
                &aligned,
                &feature_request(&aligned, operation, 1.5),
                &CancellationToken::new(),
            )
            .expect("the aligned fixture takes the feature")
            .snapshot;
            let support = support_by_role(&rotated, FaceRole::PositiveZ);
            let result = NativeKernel::execute(
                &rotated,
                &ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new("face-feature-rotated"),
                    expected_snapshot: rotated.id(),
                    precision: PrecisionPolicy::default(),
                    command: KernelCommand::ExtrudeFaceProfile {
                        target_face: support.face,
                        frame: support.frame,
                        vertices: vec![
                            Point2::new(-2.0, -1.0),
                            Point2::new(2.0, -1.0),
                            Point2::new(2.0, 1.0),
                            Point2::new(-2.0, 1.0),
                        ],
                        distance: 1.5,
                        operation,
                    },
                },
                &CancellationToken::new(),
            )
            .expect("a rotated solid must take the same feature")
            .snapshot;

            assert!(NativeKernel::validate(&result, ValidationProfile::Solid).valid);
            assert_eq!(result.counts(), aligned_result.counts());
            let expected = 480.0 + sign * 4.0 * 2.0 * 1.5;
            assert!(
                (result.measures().volume - expected).abs() < 1.0e-9,
                "rotated {operation:?} volume {} should equal {expected}",
                result.measures().volume
            );
        }
    }

    fn inset_profile(support: &crate::PlanarFaceSupport, fraction: f64) -> Vec<Point2> {
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
        let u_center = (u_min + u_max) * 0.5;
        let v_center = (v_min + v_max) * 0.5;
        let u_half = (u_max - u_min) * fraction * 0.5;
        let v_half = (v_max - v_min) * fraction * 0.5;
        vec![
            Point2::new(u_center - u_half, v_center - v_half),
            Point2::new(u_center + u_half, v_center - v_half),
            Point2::new(u_center + u_half, v_center + v_half),
            Point2::new(u_center - u_half, v_center + v_half),
        ]
    }

    fn feature_request_on_support(
        snapshot: &Snapshot,
        support: &crate::PlanarFaceSupport,
        operation: FaceExtrusionOperation,
        distance: f64,
        fraction: f64,
        request_id: &str,
    ) -> ExecuteRequest {
        ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(request_id),
            expected_snapshot: snapshot.id(),
            precision: snapshot.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudeFaceProfile {
                target_face: support.face,
                frame: support.frame,
                vertices: inset_profile(support, fraction),
                distance,
                operation,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn region_feature_request_on_support(
        snapshot: &Snapshot,
        support: &crate::PlanarFaceSupport,
        operation: FaceExtrusionOperation,
        distance: f64,
        outer: &[Point2],
        holes: &[Vec<Point2>],
        request_id: &str,
    ) -> ExecuteRequest {
        ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(request_id),
            expected_snapshot: snapshot.id(),
            precision: snapshot.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: support.face,
                frame: support.frame,
                profile: PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: PlanarLoop2::from_polygon(outer),
                        holes: holes
                            .iter()
                            .map(|hole| PlanarLoop2::from_polygon(hole))
                            .collect(),
                    }],
                },
                distance,
                operation,
            },
        }
    }

    fn generated_face(
        outcome: &crate::ExecutionOutcome,
        role_name: &str,
    ) -> crate::PlanarFaceSupport {
        generated_face_with_ordinal(outcome, role_name, None)
    }

    fn generated_face_with_ordinal(
        outcome: &crate::ExecutionOutcome,
        role_name: &str,
        ordinal: Option<u32>,
    ) -> crate::PlanarFaceSupport {
        let matches = outcome
            .report
            .history
            .iter()
            .filter(|record| {
                record.role.as_ref().is_some_and(|role| {
                    role.name == role_name
                        && ordinal.is_none_or(|ordinal| role.ordinal == Some(ordinal))
                })
            })
            .flat_map(|record| record.outputs.iter().copied())
            .filter(|output| output.kind == EntityKind::Face)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "one generated face for {role_name}");
        NativeKernel::planar_face_support(&outcome.snapshot, matches[0])
            .expect("generated end face has exact planar support")
    }

    fn entity_references(snapshot: &Snapshot) -> BTreeSet<EntityRef> {
        let mut references = BTreeSet::new();
        let mut add = |kind, id| {
            references.insert(crate::entity_ref(snapshot.id(), id, kind));
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
            (actual - expected).abs() <= 1.0e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn rectangular_boss_is_an_exact_hole_aware_solid() {
        let input = cuboid();
        let request = feature_request(&input, FaceExtrusionOperation::Add, 3.0);
        let first = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect("boss should publish");
        let replay = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect("boss replay should publish");

        assert_eq!(first.snapshot.id(), replay.snapshot.id());
        assert_eq!(
            first.snapshot.semantic_digest(),
            replay.snapshot.semantic_digest()
        );
        let counts = first.snapshot.counts();
        assert_eq!(
            (
                counts.vertices,
                counts.edges,
                counts.coedges,
                counts.loops,
                counts.faces,
                counts.shells,
                counts.solids,
            ),
            (16, 24, 48, 12, 11, 1, 1)
        );
        let measures = first.snapshot.measures();
        assert_close(measures.volume, 504.0);
        assert_close(measures.surface_area, 412.0);
        let centroid = measures.centroid.expect("solid centroid");
        assert_close(centroid.x, 5.0);
        assert_close(centroid.y, 4.0);
        assert_close(centroid.z, 45.0 / 14.0);
        let bounds = measures.bounds.expect("solid bounds");
        assert_close(bounds.max.z, 9.0);
        assert!(NativeKernel::validate(&first.snapshot, ValidationProfile::Solid).valid);
        let scene = NativeKernel::debug_scene(&first.snapshot);
        assert_eq!(scene.edges.len(), 24);
        assert_eq!(scene.triangles.len(), 28);
        assert_eq!(
            scene
                .triangles
                .iter()
                .filter(|triangle| triangle.role == FaceRole::FeatureEnd)
                .count(),
            2
        );
        assert!(first.report.history.iter().any(|record| {
            record
                .role
                .as_ref()
                .is_some_and(|role| role.name == "face_extrude.boss.end_face")
        }));
    }

    #[test]
    fn generated_end_and_floor_faces_support_add_cut_add_chains() {
        let input = cuboid();
        let first_request = feature_request(&input, FaceExtrusionOperation::Add, 3.0);
        let first = NativeKernel::execute(&input, &first_request, &CancellationToken::new())
            .expect("first boss");
        let boss_end = generated_face(&first, "face_extrude.boss.end_face");

        let second_request = feature_request_on_support(
            &first.snapshot,
            &boss_end,
            FaceExtrusionOperation::Cut,
            1.0,
            0.5,
            "chain-cut",
        );
        let second =
            NativeKernel::execute(&first.snapshot, &second_request, &CancellationToken::new())
                .expect("nested blind pocket");
        let pocket_floor = generated_face(&second, "face_extrude.pocket.floor_face");

        let third_request = feature_request_on_support(
            &second.snapshot,
            &pocket_floor,
            FaceExtrusionOperation::Add,
            0.5,
            0.5,
            "chain-add",
        );
        let third =
            NativeKernel::execute(&second.snapshot, &third_request, &CancellationToken::new())
                .expect("boss inside pocket");

        for ((outcome, expected, unchanged), source) in [
            (&first, (16, 24, 48, 12, 11), 50),
            (&second, (24, 36, 72, 18, 16), 105),
            (&third, (32, 48, 96, 24, 21), 160),
        ]
        .into_iter()
        .zip([&input, &first.snapshot, &second.snapshot])
        {
            let counts = outcome.snapshot.counts();
            assert_eq!(
                (
                    counts.vertices,
                    counts.edges,
                    counts.coedges,
                    counts.loops,
                    counts.faces,
                ),
                expected
            );
            assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
            assert_eq!(
                outcome.report.history.len(),
                outcome.snapshot.counts().total() as usize
            );
            let relation_count = |relation| {
                outcome
                    .report
                    .history
                    .iter()
                    .filter(|record| record.relation == relation)
                    .count()
            };
            assert_eq!(relation_count(HistoryRelation::Unchanged), unchanged);
            assert_eq!(relation_count(HistoryRelation::Modified), 8);
            assert_eq!(relation_count(HistoryRelation::Generated), 55);
            assert_eq!(relation_count(HistoryRelation::Deleted), 0);

            let output_references = outcome
                .report
                .history
                .iter()
                .flat_map(|record| record.outputs.iter().copied())
                .collect::<Vec<_>>();
            assert_eq!(
                output_references.iter().copied().collect::<BTreeSet<_>>(),
                entity_references(&outcome.snapshot)
            );
            assert_eq!(
                output_references.len(),
                output_references
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
                    .len(),
                "every output is covered exactly once"
            );
            assert_eq!(
                outcome
                    .report
                    .history
                    .iter()
                    .flat_map(|record| record.inputs.iter().copied())
                    .collect::<BTreeSet<_>>(),
                entity_references(source),
                "every input participates in repeated-feature history"
            );
        }

        assert_close(first.snapshot.measures().volume, 504.0);
        assert_close(first.snapshot.measures().surface_area, 412.0);
        assert_close(second.snapshot.measures().volume, 502.0);
        assert_close(second.snapshot.measures().surface_area, 418.0);
        assert_close(third.snapshot.measures().volume, 502.25);
        assert_close(third.snapshot.measures().surface_area, 419.5);
        let centroid = third.snapshot.measures().centroid.expect("solid centroid");
        assert_close(centroid.x, 5.0);
        assert_close(centroid.y, 4.0);
        assert_close(centroid.z, 1605.0625 / 502.25);
        let bounds = third.snapshot.measures().bounds.expect("solid bounds");
        assert_close(bounds.min.z, 0.0);
        assert_close(bounds.max.z, 9.0);

        let replay_first = NativeKernel::execute(&input, &first_request, &CancellationToken::new())
            .expect("first replay");
        let replay_second = NativeKernel::execute(
            &replay_first.snapshot,
            &second_request,
            &CancellationToken::new(),
        )
        .expect("second replay");
        let replay_third = NativeKernel::execute(
            &replay_second.snapshot,
            &third_request,
            &CancellationToken::new(),
        )
        .expect("third replay");
        assert_eq!(third.snapshot.id(), replay_third.snapshot.id());
        assert_eq!(
            third.snapshot.semantic_digest(),
            replay_third.snapshot.semantic_digest()
        );
    }

    #[test]
    fn cut_crosses_a_boss_support_interface_and_remains_blind_in_the_base() {
        let input = cuboid();
        let boss = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Add, 3.0),
            &CancellationToken::new(),
        )
        .expect("boss");
        let boss_end = generated_face(&boss, "face_extrude.boss.end_face");
        let cut = NativeKernel::execute(
            &boss.snapshot,
            &feature_request_on_support(
                &boss.snapshot,
                &boss_end,
                FaceExtrusionOperation::Cut,
                4.0,
                0.5,
                "cross-interface-blind-cut",
            ),
            &CancellationToken::new(),
        )
        .expect("the old coplanar shoulder surrounds, but does not intersect, the cut footprint");

        assert_close(cut.snapshot.measures().volume, 496.0);
        assert!(NativeKernel::validate(&cut.snapshot, ValidationProfile::Solid).valid);
        assert!(cut.report.history.iter().any(|record| {
            record
                .role
                .as_ref()
                .is_some_and(|role| role.name == "face_extrude.pocket.floor_face")
        }));
    }

    #[test]
    fn annular_add_ignores_existing_geometry_wholly_inside_its_profile_void() {
        let input = cuboid();
        let center_boss = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Add, 3.0),
            &CancellationToken::new(),
        )
        .expect("central boss");
        let shoulder = support_by_role(&center_boss.snapshot, FaceRole::PositiveZ);
        let outer = vec![
            Point2::new(-3.0, -2.0),
            Point2::new(3.0, -2.0),
            Point2::new(3.0, 2.0),
            Point2::new(-3.0, 2.0),
        ];
        let hole = vec![
            Point2::new(-2.5, -1.5),
            Point2::new(2.5, -1.5),
            Point2::new(2.5, 1.5),
            Point2::new(-2.5, 1.5),
        ];
        let annulus = NativeKernel::execute(
            &center_boss.snapshot,
            &region_feature_request_on_support(
                &center_boss.snapshot,
                &shoulder,
                FaceExtrusionOperation::Add,
                2.0,
                &outer,
                &[hole],
                "annular-add-around-existing-boss",
            ),
            &CancellationToken::new(),
        )
        .expect("the central boss lies wholly in the annular profile void");

        assert_close(annulus.snapshot.measures().volume, 522.0);
        assert_eq!(annulus.snapshot.counts().solids, 1);
        assert!(NativeKernel::validate(&annulus.snapshot, ValidationProfile::Solid).valid);
    }

    #[test]
    fn annular_cut_ignores_a_pocket_in_its_void_and_resolves_the_true_exit() {
        let input = cuboid();
        let center_pocket = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Cut, 2.0),
            &CancellationToken::new(),
        )
        .expect("central blind pocket");
        let shoulder = support_by_role(&center_pocket.snapshot, FaceRole::PositiveZ);
        let outer = vec![
            Point2::new(-3.0, -2.5),
            Point2::new(3.0, -2.5),
            Point2::new(3.0, 2.5),
            Point2::new(-3.0, 2.5),
        ];
        let hole = vec![
            Point2::new(-2.5, -1.5),
            Point2::new(2.5, -1.5),
            Point2::new(2.5, 1.5),
            Point2::new(-2.5, 1.5),
        ];
        let annular_cut = NativeKernel::execute(
            &center_pocket.snapshot,
            &region_feature_request_on_support(
                &center_pocket.snapshot,
                &shoulder,
                FaceExtrusionOperation::Cut,
                10.0,
                &outer,
                &[hole],
                "annular-through-cut-around-existing-pocket",
            ),
            &CancellationToken::new(),
        )
        .expect("the pocket floor in the void must not mask the bottom exit");

        assert_close(annular_cut.snapshot.measures().volume, 374.0);
        assert_eq!(annular_cut.snapshot.counts().solids, 2);
        assert!(annular_cut.report.history.iter().any(|record| {
            record
                .role
                .as_ref()
                .is_some_and(|role| role.name == "face_extrude.exit_face_patch")
        }));
        assert!(NativeKernel::validate(&annular_cut.snapshot, ValidationProfile::Solid).valid);
    }

    #[test]
    fn cut_through_a_generated_boss_side_is_exact_and_overtravel_is_stable() {
        let input = cuboid();
        let boss = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Add, 3.0),
            &CancellationToken::new(),
        )
        .expect("boss");
        let side = generated_face_with_ordinal(&boss, "face_extrude.boss.side_face", Some(0));
        let exact_request = feature_request_on_support(
            &boss.snapshot,
            &side,
            FaceExtrusionOperation::Cut,
            2.0,
            0.5,
            "generated-side-through-cut",
        );
        let exact =
            NativeKernel::execute(&boss.snapshot, &exact_request, &CancellationToken::new())
                .expect("generated side accepts an exact through cut");
        let overtravel = NativeKernel::execute(
            &boss.snapshot,
            &feature_request_on_support(
                &boss.snapshot,
                &side,
                FaceExtrusionOperation::Cut,
                4.0,
                0.5,
                "generated-side-through-cut-overtravel",
            ),
            &CancellationToken::new(),
        )
        .expect("cut tool overtravel past the first exit retains the same body");

        assert_close(exact.snapshot.measures().volume, 498.0);
        assert_eq!(exact.snapshot.id(), overtravel.snapshot.id());
        assert_eq!(
            exact.snapshot.semantic_digest(),
            overtravel.snapshot.semantic_digest()
        );
        assert!(NativeKernel::validate(&exact.snapshot, ValidationProfile::Solid).valid);
        assert!(exact.report.history.iter().any(|record| {
            record
                .role
                .as_ref()
                .is_some_and(|role| role.name == "face_extrude.exit_face_patch")
        }));
    }

    #[test]
    fn repeated_feature_chains_work_on_every_axis_sign() {
        for role in [
            FaceRole::NegativeX,
            FaceRole::PositiveX,
            FaceRole::NegativeY,
            FaceRole::PositiveY,
            FaceRole::NegativeZ,
            FaceRole::PositiveZ,
        ] {
            let input = cuboid();
            let support = support_by_role(&input, role);
            let first_profile = inset_profile(&support, 0.5);
            let first_area = (first_profile[1].x - first_profile[0].x).abs()
                * (first_profile[2].y - first_profile[1].y).abs();
            let first = NativeKernel::execute(
                &input,
                &feature_request_on_support(
                    &input,
                    &support,
                    FaceExtrusionOperation::Add,
                    1.0,
                    0.5,
                    "axis-chain-add",
                ),
                &CancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("first feature on {role:?} failed: {error}"));
            let end = generated_face(&first, "face_extrude.boss.end_face");
            let second_profile = inset_profile(&end, 0.5);
            let second_area = (second_profile[1].x - second_profile[0].x).abs()
                * (second_profile[2].y - second_profile[1].y).abs();
            let second = NativeKernel::execute(
                &first.snapshot,
                &feature_request_on_support(
                    &first.snapshot,
                    &end,
                    FaceExtrusionOperation::Cut,
                    0.5,
                    0.5,
                    "axis-chain-cut",
                ),
                &CancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("second feature on {role:?} failed: {error}"));

            assert_eq!(second.snapshot.counts().faces, 16);
            assert_close(
                second.snapshot.measures().volume,
                480.0 + first_area - second_area * 0.5,
            );
            assert!(NativeKernel::validate(&second.snapshot, ValidationProfile::Solid).valid);
        }
    }

    #[test]
    fn repeated_features_respect_face_holes_and_add_sweep_contacts() {
        let input = cuboid();
        let boss = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Add, 3.0),
            &CancellationToken::new(),
        )
        .expect("boss");

        let shoulder = support_by_role(&boss.snapshot, FaceRole::PositiveZ);
        let shoulder_error = NativeKernel::execute(
            &boss.snapshot,
            &feature_request_on_support(
                &boss.snapshot,
                &shoulder,
                FaceExtrusionOperation::Add,
                0.5,
                0.25,
                "profile-inside-existing-hole",
            ),
            &CancellationToken::new(),
        )
        .expect_err("a profile inside the shoulder's hole is not on face material");
        assert!(
            shoulder_error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code.as_str() == "FACE_FEATURE_PROFILE_OUTSIDE_FACE"
            })
        );

        let end = generated_face(&boss, "face_extrude.boss.end_face");
        let through = NativeKernel::execute(
            &boss.snapshot,
            &feature_request_on_support(
                &boss.snapshot,
                &end,
                FaceExtrusionOperation::Cut,
                9.0,
                0.5,
                "cut-to-opposite-wall",
            ),
            &CancellationToken::new(),
        )
        .expect("a cut reaching the opposite body wall becomes an exact through cut");
        assert_close(through.snapshot.measures().volume, 486.0);
        assert!(NativeKernel::validate(&through.snapshot, ValidationProfile::Solid).valid);
        assert_eq!(boss.snapshot.measures().volume, 504.0);

        let pocket = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Cut, 3.0),
            &CancellationToken::new(),
        )
        .expect("pocket");
        let pocket_wall =
            generated_face_with_ordinal(&pocket, "face_extrude.pocket.wall_face", Some(0));
        let collision_error = NativeKernel::execute(
            &pocket.snapshot,
            &feature_request_on_support(
                &pocket.snapshot,
                &pocket_wall,
                FaceExtrusionOperation::Add,
                3.0,
                0.5,
                "add-across-pocket",
            ),
            &CancellationToken::new(),
        )
        .expect_err("an Add crossing the pocket must fail before touching its opposite wall");
        assert!(
            collision_error
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.as_str() == "FACE_FEATURE_SWEEP_COLLISION" })
        );
        assert_eq!(pocket.snapshot.measures().volume, 456.0);
    }

    #[test]
    fn rectangle_order_normalizes_boundary_cycles_and_rejects_crossed_loops() {
        let input = cuboid();
        let baseline = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Add, 3.0),
            &CancellationToken::new(),
        )
        .expect("canonical rectangle")
        .snapshot;
        let [p0, p1, p2, p3] = [
            Point2::new(-2.0, -1.0),
            Point2::new(2.0, -1.0),
            Point2::new(2.0, 1.0),
            Point2::new(-2.0, 1.0),
        ];

        for (name, ordering) in [
            ("clockwise", vec![p0, p3, p2, p1]),
            ("cyclic-counter-clockwise", vec![p2, p3, p0, p1]),
            ("cyclic-clockwise", vec![p2, p1, p0, p3]),
        ] {
            let mut request = feature_request(&input, FaceExtrusionOperation::Add, 3.0);
            let KernelCommand::ExtrudeFaceProfile { vertices, .. } = &mut request.command else {
                unreachable!("face-feature helper constructs face extrusion")
            };
            *vertices = ordering;
            let outcome = NativeKernel::execute(&input, &request, &CancellationToken::new())
                .unwrap_or_else(|error| panic!("{name} boundary cycle was rejected: {error}"));
            assert_eq!(
                outcome.snapshot.semantic_digest(),
                baseline.semantic_digest()
            );
        }

        for ordering in [vec![p0, p2, p1, p3], vec![p0, p1, p3, p2]] {
            let mut request = feature_request(&input, FaceExtrusionOperation::Add, 3.0);
            let KernelCommand::ExtrudeFaceProfile { vertices, .. } = &mut request.command else {
                unreachable!("face-feature helper constructs face extrusion")
            };
            *vertices = ordering;
            let error = NativeKernel::execute(&input, &request, &CancellationToken::new())
                .expect_err("a crossed corner sequence is not a rectangular boundary loop");
            assert_eq!(error.code, KernelErrorCode::InvalidInput);
            assert!(error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code.as_str() == "FACE_FEATURE_PROFILE_SELF_INTERSECTING"
            }));
        }
    }

    #[test]
    fn certified_linear_triangle_and_concave_profiles_support_add_and_blind_cut() {
        let input = cuboid();
        let support = positive_z_support(&input);
        let cases = [
            (
                "triangle",
                vec![
                    Point2::new(-2.0, -1.0),
                    Point2::new(2.0, -1.0),
                    Point2::new(0.0, 2.0),
                ],
                6.0,
            ),
            (
                "concave",
                vec![
                    Point2::new(-2.0, -1.0),
                    Point2::new(2.0, -1.0),
                    Point2::new(2.0, 1.0),
                    Point2::new(0.0, 0.0),
                    Point2::new(-2.0, 1.0),
                ],
                6.0,
            ),
        ];
        for (name, vertices, area) in cases {
            for operation in [FaceExtrusionOperation::Add, FaceExtrusionOperation::Cut] {
                let request = ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!("linear-{name}-{operation:?}")),
                    expected_snapshot: input.id(),
                    precision: input.precision_policy().unwrap_or_default(),
                    command: KernelCommand::ExtrudeFaceProfile {
                        target_face: support.face,
                        frame: support.frame,
                        vertices: vertices.clone(),
                        distance: 1.0,
                        operation,
                    },
                };
                let output = NativeKernel::execute(&input, &request, &CancellationToken::new())
                    .unwrap_or_else(|error| panic!("{name} {operation:?} failed: {error}"));
                let expected = match operation {
                    FaceExtrusionOperation::Add => 480.0 + area,
                    FaceExtrusionOperation::Cut => 480.0 - area,
                };
                assert_close(output.snapshot.measures().volume, expected);
                assert!(NativeKernel::validate(&output.snapshot, ValidationProfile::Solid).valid);
            }
        }
    }

    #[test]
    fn large_placement_rejects_unrepresentable_depth_and_profile_geometry() {
        let depth_input = cuboid_at(ProtocolPoint3::new(0.0, 0.0, 999_999_980.0));
        NativeKernel::execute(
            &depth_input,
            &feature_request(&depth_input, FaceExtrusionOperation::Add, 1.0),
            &CancellationToken::new(),
        )
        .expect("an exactly representable depth remains supported at a large placement");

        let depth_error = NativeKernel::execute(
            &depth_input,
            &feature_request(&depth_input, FaceExtrusionOperation::Add, 1.000_000_01),
            &CancellationToken::new(),
        )
        .expect_err("rounded depth exceeds the active linear agreement");
        assert_eq!(depth_error.code, KernelErrorCode::NumericallyIndeterminate);
        assert!(depth_error.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "FACE_FEATURE_PRECISION_UNREPRESENTABLE"
        }));

        let profile_input = cuboid_at(ProtocolPoint3::new(999_999_980.0, 0.0, 0.0));
        NativeKernel::execute(
            &profile_input,
            &feature_request(&profile_input, FaceExtrusionOperation::Add, 1.0),
            &CancellationToken::new(),
        )
        .expect("an exactly representable rectangle remains supported at a large placement");

        let mut profile_request = feature_request(&profile_input, FaceExtrusionOperation::Add, 1.0);
        let KernelCommand::ExtrudeFaceProfile { vertices, .. } = &mut profile_request.command
        else {
            unreachable!("face-feature helper constructs face extrusion")
        };
        vertices[0].x = -2.000_000_01;
        vertices[3].x = -2.000_000_01;
        let profile_error =
            NativeKernel::execute(&profile_input, &profile_request, &CancellationToken::new())
                .expect_err("rounded profile chords exceed the active linear agreement");
        assert_eq!(
            profile_error.code,
            KernelErrorCode::NumericallyIndeterminate
        );
        assert!(profile_error.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "FACE_FEATURE_PRECISION_UNREPRESENTABLE"
        }));
    }

    #[test]
    fn rectangular_blind_cut_is_exact_and_retains_the_input_on_rejection() {
        let input = cuboid();
        let request = feature_request(&input, FaceExtrusionOperation::Cut, 3.0);
        let output = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect("blind pocket should publish");

        let counts = output.snapshot.counts();
        assert_eq!((counts.vertices, counts.edges, counts.faces), (16, 24, 11));
        let measures = output.snapshot.measures();
        assert_close(measures.volume, 456.0);
        assert_close(measures.surface_area, 412.0);
        let centroid = measures.centroid.expect("solid centroid");
        assert_close(centroid.x, 5.0);
        assert_close(centroid.y, 4.0);
        assert_close(centroid.z, 111.0 / 38.0);
        let bounds = measures.bounds.expect("solid bounds");
        assert_close(bounds.min.z, 0.0);
        assert_close(bounds.max.z, 6.0);
        assert!(output.report.history.iter().any(|record| {
            record
                .role
                .as_ref()
                .is_some_and(|role| role.name == "face_extrude.pocket.floor_face")
        }));

        let input_id = input.id();
        let through = NativeKernel::execute(
            &input,
            &feature_request(&input, FaceExtrusionOperation::Cut, 6.0),
            &CancellationToken::new(),
        )
        .expect("a cut reaching the opposite wall should publish a through hole");
        assert_close(through.snapshot.measures().volume, 432.0);
        assert_close(through.snapshot.measures().surface_area, 432.0);
        assert!(NativeKernel::validate(&through.snapshot, ValidationProfile::Solid).valid);
        assert_eq!(input.id(), input_id);
        assert_eq!(input.measures().volume, 480.0);
    }

    #[test]
    fn add_and_cut_are_valid_on_all_six_oriented_face_frames() {
        let input = cuboid();
        for role in [
            FaceRole::NegativeX,
            FaceRole::PositiveX,
            FaceRole::NegativeY,
            FaceRole::PositiveY,
            FaceRole::NegativeZ,
            FaceRole::PositiveZ,
        ] {
            let support = support_by_role(&input, role);
            let [u0, u1, v0, v1] = support.boundary.iter().fold(
                [
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                ],
                |[u0, u1, v0, v1], point| {
                    [
                        u0.min(point.x),
                        u1.max(point.x),
                        v0.min(point.y),
                        v1.max(point.y),
                    ]
                },
            );
            let profile = vec![
                Point2::new(u0 * 0.5, v0 * 0.5),
                Point2::new(u1 * 0.5, v0 * 0.5),
                Point2::new(u1 * 0.5, v1 * 0.5),
                Point2::new(u0 * 0.5, v1 * 0.5),
            ];
            let area = (u1 - u0) * 0.5 * (v1 - v0) * 0.5;
            for operation in [FaceExtrusionOperation::Add, FaceExtrusionOperation::Cut] {
                let request = ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!("all-faces-{role:?}-{operation:?}")),
                    expected_snapshot: input.id(),
                    precision: input.precision_policy().unwrap_or_default(),
                    command: KernelCommand::ExtrudeFaceProfile {
                        target_face: support.face,
                        frame: support.frame,
                        vertices: profile.clone(),
                        distance: 1.0,
                        operation,
                    },
                };
                let output = NativeKernel::execute(&input, &request, &CancellationToken::new())
                    .expect("every cuboid side supports the exact scaffold");
                let expected_volume = match operation {
                    FaceExtrusionOperation::Add => 480.0 + area,
                    FaceExtrusionOperation::Cut => 480.0 - area,
                };
                assert_close(output.snapshot.measures().volume, expected_volume);
                assert_eq!(output.snapshot.counts().faces, 11);
                assert!(NativeKernel::validate(&output.snapshot, ValidationProfile::Solid).valid);
            }
        }
    }

    #[test]
    fn mirrored_face_frame_is_rejected_before_construction() {
        let input = cuboid();
        let support = positive_z_support(&input);
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("mirrored-face-frame"),
            expected_snapshot: input.id(),
            precision: input.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudeFaceProfile {
                target_face: support.face,
                frame: ProtocolPlanarFrame3::new(
                    support.frame.origin,
                    support.frame.v,
                    support.frame.u,
                ),
                vertices: vec![
                    Point2::new(-2.0, -1.0),
                    Point2::new(2.0, -1.0),
                    Point2::new(2.0, 1.0),
                    Point2::new(-2.0, 1.0),
                ],
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            },
        };
        let error = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect_err("mirrored support must not silently flip the operation");
        assert!(error.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "FACE_FEATURE_FRAME_OFF_TARGET_PLANE"
        }));
    }

    #[test]
    fn rectangular_constructor_extrusion_can_feed_one_selected_face_feature() {
        let empty = NativeKernel::empty();
        let base = NativeKernel::execute(
            &empty,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("constructor-before-face-feature"),
                expected_snapshot: empty.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePolygon {
                    frame: ProtocolPlanarFrame3::new(
                        ProtocolPoint3::new(0.0, 0.0, 0.0),
                        artificer_protocol::Vector3::new(1.0, 0.0, 0.0),
                        artificer_protocol::Vector3::new(0.0, 1.0, 0.0),
                    ),
                    vertices: vec![
                        Point2::new(-2.0, -1.0),
                        Point2::new(2.0, -1.0),
                        Point2::new(2.0, 1.0),
                        Point2::new(-2.0, 1.0),
                    ],
                    distance: 3.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("rectangular constructor")
        .snapshot;
        let support = support_by_role(&base, FaceRole::ExtrusionTop);
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("feature-after-constructor"),
            expected_snapshot: base.id(),
            precision: base.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudeFaceProfile {
                target_face: support.face,
                frame: support.frame,
                vertices: vec![
                    Point2::new(-1.0, -0.5),
                    Point2::new(1.0, -0.5),
                    Point2::new(1.0, 0.5),
                    Point2::new(-1.0, 0.5),
                ],
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            },
        };
        let feature = NativeKernel::execute(&base, &request, &CancellationToken::new())
            .expect("selected extrusion top accepts a boss");
        assert_close(feature.snapshot.measures().volume, 26.0);
        assert_close(feature.snapshot.measures().surface_area, 58.0);
        assert_eq!(feature.snapshot.counts().faces, 11);
        assert_eq!(
            NativeKernel::debug_scene(&feature.snapshot)
                .triangles
                .iter()
                .filter(|triangle| triangle.role == FaceRole::ExtrusionTop)
                .count(),
            8,
            "the single hole-aware shoulder face retains its source-face meaning"
        );
    }

    #[test]
    fn triangular_constructor_end_hosts_repeated_inset_linear_features() {
        let empty = NativeKernel::empty();
        let base = NativeKernel::execute(
            &empty,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("triangle-constructor-before-face-feature"),
                expected_snapshot: empty.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::ExtrudePolygon {
                    frame: ProtocolPlanarFrame3::new(
                        ProtocolPoint3::new(0.0, 0.0, 0.0),
                        artificer_protocol::Vector3::new(1.0, 0.0, 0.0),
                        artificer_protocol::Vector3::new(0.0, 1.0, 0.0),
                    ),
                    vertices: vec![
                        Point2::new(-2.0, -1.0),
                        Point2::new(2.0, -1.0),
                        Point2::new(0.0, 2.0),
                    ],
                    distance: 3.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("triangular constructor")
        .snapshot;
        let top = support_by_role(&base, FaceRole::ExtrusionTop);
        let scaled = |support: &crate::PlanarFaceSupport, factor: f64| {
            support
                .boundary
                .iter()
                .map(|point| Point2::new(point.x * factor, point.y * factor))
                .collect::<Vec<_>>()
        };
        let add = NativeKernel::execute(
            &base,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("triangle-end-add"),
                expected_snapshot: base.id(),
                precision: base.precision_policy().unwrap_or_default(),
                command: KernelCommand::ExtrudeFaceProfile {
                    target_face: top.face,
                    frame: top.frame,
                    vertices: scaled(&top, 0.5),
                    distance: 1.0,
                    operation: FaceExtrusionOperation::Add,
                },
            },
            &CancellationToken::new(),
        )
        .expect("triangular end accepts an inset triangular boss");
        assert_close(add.snapshot.measures().volume, 19.5);

        let end = generated_face(&add, "face_extrude.boss.end_face");
        let cut = NativeKernel::execute(
            &add.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("triangle-end-repeated-cut"),
                expected_snapshot: add.snapshot.id(),
                precision: add.snapshot.precision_policy().unwrap_or_default(),
                command: KernelCommand::ExtrudeFaceProfile {
                    target_face: end.face,
                    frame: end.frame,
                    vertices: scaled(&end, 0.5),
                    distance: 0.5,
                    operation: FaceExtrusionOperation::Cut,
                },
            },
            &CancellationToken::new(),
        )
        .expect("generated triangular end accepts a subsequent inset cut");
        assert_close(cut.snapshot.measures().volume, 19.3125);
        assert!(NativeKernel::validate(&cut.snapshot, ValidationProfile::Solid).valid);
        assert_eq!(
            cut.report.history.len(),
            cut.snapshot.counts().total() as usize
        );
    }

    #[test]
    fn planar_support_query_is_exact_and_rejects_stale_wrong_kind_and_missing_refs() {
        let input = cuboid();
        let support = positive_z_support(&input);
        assert_eq!(support.face.snapshot, input.id());
        assert_eq!(support.support_digest, input.semantic_digest());
        assert_eq!(support.boundary.len(), 4);
        for point in &support.boundary {
            let world = protocol_point(support.frame.origin)
                + protocol_vector(support.frame.u) * point.x
                + protocol_vector(support.frame.v) * point.y;
            assert_close(world.z, 6.0);
        }

        for invalid in [
            EntityRef {
                snapshot: SnapshotId::ZERO,
                ..support.face
            },
            EntityRef {
                kind: EntityKind::Edge,
                ..support.face
            },
            EntityRef {
                entity: artificer_protocol::EntityId(999_999),
                ..support.face
            },
        ] {
            assert!(NativeKernel::planar_face_support(&input, invalid).is_err());
        }
    }
}

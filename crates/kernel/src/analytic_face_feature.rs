//! Exact local selected-face rewrite for one circular boss or pocket.
//!
//! The owning solid may already contain cylindrical sibling faces. Only the
//! selected planar support and, for through cuts, its certified opposite cap
//! are rewritten; authoritative circles remain two semicircle edges.

use artificer_protocol::{
    ArcDirection, EntityKind, EntityRef, FaceExtrusionOperation, MAX_PLANAR_PROFILE_CURVES,
    MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS, PlanarCurve2, PlanarFrame3,
    PlanarProfile2, PrecisionPolicy, SnapshotId,
};

use crate::face_feature::FaceFeatureInputError;
use crate::planar_profile::PlanarProfileInputError;
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Cylinder, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole,
    Loop, LoopKey, Orientation, ParameterRange, Plane, Point2, Point3, Record, Surface, Topology,
    Vector2, Vector3, Vertex, VertexKey,
};

#[derive(Clone, Debug)]
pub(crate) struct ValidatedAnalyticFaceFeature {
    pub(crate) topology: Topology,
    pub(crate) exit_face_index: Option<usize>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_analytic_face_feature(
    snapshot: SnapshotId,
    topology: &Topology,
    target_face: EntityRef,
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    distance: f64,
    operation: FaceExtrusionOperation,
    precision: PrecisionPolicy,
) -> Result<ValidatedAnalyticFaceFeature, PlanarProfileInputError> {
    if target_face.snapshot != snapshot {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::TargetSnapshotMismatch,
        ));
    }
    if target_face.kind != EntityKind::Face {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::TargetNotFace,
        ));
    }
    if !frame.is_finite() || !distance.is_finite() {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::NonFinite,
        ));
    }
    if distance <= 0.0 {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::NonPositiveDistance,
        ));
    }
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if distance <= minimum {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::FeatureTooSmall,
        ));
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return Err(PlanarProfileInputError::TooManyRegions);
    }
    if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
        return Err(PlanarProfileInputError::TooManyLoops);
    }
    if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
        return Err(PlanarProfileInputError::TooManyCurves);
    }
    let (local_center, radius) = extract_circle(profile, minimum)?;
    let target_face_index = topology
        .faces
        .iter()
        .position(|face| face.id.get() == target_face.entity.0)
        .ok_or(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::TargetMissing,
        ))?;
    let target_plane = topology.faces[target_face_index]
        .value
        .surface
        .as_plane()
        .ok_or(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::TargetNotAxisAligned,
        ))?;
    if !axis_aligned(target_plane.normal, precision.angular_agreement_radians) {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::TargetNotAxisAligned,
        ));
    }
    let frame_u = robust_unit(Vector3::new(frame.u.x, frame.u.y, frame.u.z)).ok_or(
        PlanarProfileInputError::FaceFeature(FaceFeatureInputError::FrameNotAxisAligned),
    )?;
    let frame_v = robust_unit(Vector3::new(frame.v.x, frame.v.y, frame.v.z)).ok_or(
        PlanarProfileInputError::FaceFeature(FaceFeatureInputError::FrameNotAxisAligned),
    )?;
    let frame_normal = robust_unit(frame_u.cross(frame_v)).ok_or(
        PlanarProfileInputError::FaceFeature(FaceFeatureInputError::FrameNotAxisAligned),
    )?;
    let angular_tolerance = precision.angular_agreement_radians.max(1.0e-12);
    if frame_u.dot(frame_v).abs() > angular_tolerance {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::FrameNotAxisAligned,
        ));
    }
    if 1.0 - frame_normal.dot(target_plane.normal) > angular_tolerance {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::FrameNotAxisAligned,
        ));
    }
    let frame_origin = Point3::new(frame.origin.x, frame.origin.y, frame.origin.z);
    if (frame_origin - target_plane.origin)
        .dot(target_plane.normal)
        .abs()
        > precision.linear_agreement
    {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::FrameNotOnTarget,
        ));
    }
    let world_center = frame_origin + frame_u * local_center.x + frame_v * local_center.y;
    let target_center = target_plane.project(world_center);
    if !circle_strictly_inside_face(
        topology,
        &topology.faces[target_face_index].value,
        target_center,
        radius,
        minimum,
    ) {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::ProfileOutsideFace,
        ));
    }

    let target_key = FaceKey(target_face_index);
    let owning_shell = topology
        .shells
        .iter()
        .position(|shell| shell.value.faces.contains(&target_key))
        .ok_or(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::SourceUnsupported,
        ))?;
    let cut_direction = target_plane.normal * -1.0;
    let mut exit = None;
    if operation == FaceExtrusionOperation::Cut {
        for face_key in &topology.shells[owning_shell].value.faces {
            if *face_key == target_key {
                continue;
            }
            let candidate = &topology.faces[face_key.0].value;
            let Some(plane) = candidate.surface.as_plane() else {
                continue;
            };
            if plane.normal.dot(target_plane.normal) > -1.0 + angular_tolerance {
                continue;
            }
            let depth = (plane.origin - world_center).dot(cut_direction);
            if depth <= minimum {
                continue;
            }
            let center = plane.project(world_center + cut_direction * depth);
            if !circle_strictly_inside_face(topology, candidate, center, radius, minimum) {
                continue;
            }
            if exit.is_none_or(|(best, _)| depth < best) {
                exit = Some((depth, face_key.0));
            }
        }
    }
    let (feature_distance, exit_face_index) = match operation {
        FaceExtrusionOperation::Add => (distance, None),
        FaceExtrusionOperation::Cut => match exit {
            Some((depth, index)) if distance >= depth - precision.linear_agreement => {
                (depth, Some(index))
            }
            _ => (distance, None),
        },
    };
    if operation == FaceExtrusionOperation::Cut
        && exit.is_some_and(|(depth, _)| distance > depth + precision.linear_agreement)
        && exit_face_index.is_none()
    {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::CutTooDeep,
        ));
    }
    let direction = match operation {
        FaceExtrusionOperation::Add => target_plane.normal,
        FaceExtrusionOperation::Cut => cut_direction,
    };
    let end_center = world_center + direction * feature_distance;
    let carrier_extents = [
        radius * target_plane.u.x.hypot(target_plane.v.x),
        radius * target_plane.u.y.hypot(target_plane.v.y),
        radius * target_plane.u.z.hypot(target_plane.v.z),
    ];
    if [world_center, end_center]
        .into_iter()
        .flat_map(|point| {
            [
                point.x - carrier_extents[0],
                point.x + carrier_extents[0],
                point.y - carrier_extents[1],
                point.y + carrier_extents[1],
                point.z - carrier_extents[2],
                point.z + carrier_extents[2],
            ]
        })
        .chain([radius, feature_distance])
        .any(|value| !value.is_finite() || value.abs() > precision.max_abs_coordinate)
    {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::CoordinateLimit,
        ));
    }
    if circular_sweep_contacts_source(
        topology,
        target_face_index,
        exit_face_index,
        target_plane,
        world_center,
        radius,
        direction,
        feature_distance,
        minimum,
        angular_tolerance,
    ) {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::SweepCollision,
        ));
    }

    let mut candidate = topology.clone();
    append_circle_feature(
        &mut candidate,
        owning_shell,
        target_face_index,
        exit_face_index,
        target_plane,
        world_center,
        radius,
        direction,
        feature_distance,
        operation,
    );
    Ok(ValidatedAnalyticFaceFeature {
        topology: candidate,
        exit_face_index,
    })
}

fn extract_circle(
    profile: &PlanarProfile2,
    minimum: f64,
) -> Result<(Point2, f64), PlanarProfileInputError> {
    if profile.regions.len() != 1
        || !profile.regions[0].holes.is_empty()
        || profile.regions[0].outer.curves.len() != 1
    {
        return Err(PlanarProfileInputError::AnalyticFaceProfileUnsupported);
    }
    let PlanarCurve2::Circle {
        center,
        radius,
        direction,
    } = profile.regions[0].outer.curves[0]
    else {
        return Err(PlanarProfileInputError::AnalyticFaceProfileUnsupported);
    };
    if !center.is_finite() || !radius.is_finite() {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::NonFinite,
        ));
    }
    if direction != ArcDirection::CounterClockwise || radius <= minimum {
        return Err(PlanarProfileInputError::FaceFeature(
            FaceFeatureInputError::FeatureTooSmall,
        ));
    }
    Ok((Point2::new(center.x, center.y), radius))
}

fn axis_aligned(normal: Vector3, tolerance: f64) -> bool {
    [normal.x.abs(), normal.y.abs(), normal.z.abs()]
        .into_iter()
        .filter(|component| *component > tolerance.max(1.0e-12))
        .count()
        == 1
}

fn robust_unit(vector: Vector3) -> Option<Vector3> {
    let scale = vector.x.abs().max(vector.y.abs()).max(vector.z.abs());
    if !scale.is_finite() || scale == 0.0 {
        return None;
    }
    let scaled = vector / scale;
    let length = scaled.length();
    (length.is_finite() && length > 0.0).then_some(scaled / length)
}

fn circle_strictly_inside_face(
    topology: &Topology,
    face: &Face,
    center: Point2,
    radius: f64,
    minimum: f64,
) -> bool {
    if !point_in_loop(topology, face.outer_loop, center)
        || minimum_distance_to_loop(topology, face.outer_loop, center) <= radius + minimum
    {
        return false;
    }
    face.inner_loops.iter().all(|loop_key| {
        !point_in_loop(topology, *loop_key, center)
            && minimum_distance_to_loop(topology, *loop_key, center) > radius + minimum
    })
}

fn point_in_loop(topology: &Topology, loop_key: LoopKey, point: Point2) -> bool {
    if let Some((center, radius)) = exact_full_circle_loop(topology, loop_key) {
        let relative = point - center;
        return relative.x.hypot(relative.y) < radius;
    }
    let Some(loop_record) = topology.loop_record(loop_key) else {
        return false;
    };
    let mut crossings = 0_usize;
    for coedge_key in &loop_record.value.coedges {
        let Some(coedge) = topology.coedge(*coedge_key) else {
            return false;
        };
        match coedge.value.pcurve {
            Curve2::Line { endpoints } => {
                let [start, end] = endpoints;
                if (start.y > point.y) != (end.y > point.y) {
                    let x =
                        (end.x - start.x).mul_add((point.y - start.y) / (end.y - start.y), start.x);
                    if x > point.x {
                        crossings += 1;
                    }
                }
            }
            Curve2::Circle {
                center,
                u,
                v,
                radius,
            } => {
                // Face circle pcurves use orthonormal frames; solve the ray in
                // their local coordinates and retain only parameters on use.
                let relative = point - center;
                let local_y = relative.x * v.x + relative.y * v.y;
                if local_y.abs() <= radius {
                    let principal = (local_y / radius).clamp(-1.0, 1.0).asin();
                    for parameter in [principal, std::f64::consts::PI - principal] {
                        if parameter_on_range(parameter, coedge.value.parameter_range, false) {
                            let candidate = Point2::new(
                                center.x + radius * (u.x * parameter.cos() + v.x * parameter.sin()),
                                center.y + radius * (u.y * parameter.cos() + v.y * parameter.sin()),
                            );
                            let tangent_y = radius
                                * (-u.y * parameter.sin() + v.y * parameter.cos())
                                * (coedge.value.parameter_range.end
                                    - coedge.value.parameter_range.start);
                            if candidate.x > point.x && tangent_y.abs() > f64::EPSILON * radius {
                                crossings += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    crossings % 2 == 1
}

fn minimum_distance_to_loop(topology: &Topology, loop_key: LoopKey, point: Point2) -> f64 {
    if let Some((center, radius)) = exact_full_circle_loop(topology, loop_key) {
        let relative = point - center;
        return (relative.x.hypot(relative.y) - radius).abs();
    }
    let Some(loop_record) = topology.loop_record(loop_key) else {
        return 0.0;
    };
    loop_record
        .value
        .coedges
        .iter()
        .filter_map(|key| topology.coedge(*key))
        .map(|coedge| match coedge.value.pcurve {
            Curve2::Line { endpoints } => point_line_distance(point, endpoints),
            Curve2::Circle {
                center,
                u,
                v,
                radius,
            } => {
                let relative = point - center;
                let angle = (relative.x * v.x + relative.y * v.y)
                    .atan2(relative.x * u.x + relative.y * u.y);
                if parameter_on_range(angle, coedge.value.parameter_range, true) {
                    (relative.x.hypot(relative.y) - radius).abs()
                } else {
                    let endpoints = coedge.value.pcurve_endpoints();
                    (point - endpoints[0])
                        .x
                        .hypot((point - endpoints[0]).y)
                        .min((point - endpoints[1]).x.hypot((point - endpoints[1]).y))
                }
            }
        })
        .fold(f64::INFINITY, f64::min)
}

fn exact_full_circle_loop(topology: &Topology, loop_key: LoopKey) -> Option<(Point2, f64)> {
    let loop_record = topology.loop_record(loop_key)?;
    if loop_record.value.coedges.len() != 2 {
        return None;
    }
    let mut circle: Option<(Point2, f64)> = None;
    let mut total_sweep = 0.0_f64;
    let mut sweep_sign = 0.0_f64;
    for coedge_key in &loop_record.value.coedges {
        let coedge = &topology.coedge(*coedge_key)?.value;
        let Curve2::Circle {
            center,
            u: _,
            v: _,
            radius,
        } = coedge.pcurve
        else {
            return None;
        };
        if !center.is_finite() || !radius.is_finite() || radius <= 0.0 {
            return None;
        }
        let sweep = coedge.parameter_range.end - coedge.parameter_range.start;
        if sweep == 0.0 || (sweep_sign != 0.0 && sweep.signum() != sweep_sign) {
            return None;
        }
        sweep_sign = sweep.signum();
        total_sweep += sweep.abs();
        if let Some((expected_center, expected_radius)) = circle {
            let scale = expected_radius.abs().max(radius.abs()).max(1.0);
            let tolerance = 128.0 * f64::EPSILON * scale;
            if (center.x - expected_center.x).abs() > tolerance
                || (center.y - expected_center.y).abs() > tolerance
                || (radius - expected_radius).abs() > tolerance
            {
                return None;
            }
        } else {
            circle = Some((center, radius));
        }
    }
    ((total_sweep - std::f64::consts::TAU).abs() <= 256.0 * f64::EPSILON).then_some(circle?)
}

fn point_line_distance(point: Point2, endpoints: [Point2; 2]) -> f64 {
    let direction = endpoints[1] - endpoints[0];
    let length_squared = direction.x * direction.x + direction.y * direction.y;
    let offset = point - endpoints[0];
    let parameter =
        ((offset.x * direction.x + offset.y * direction.y) / length_squared).clamp(0.0, 1.0);
    let closest = Point2::new(
        direction.x.mul_add(parameter, endpoints[0].x),
        direction.y.mul_add(parameter, endpoints[0].y),
    );
    (point - closest).x.hypot((point - closest).y)
}

fn parameter_on_range(parameter: f64, range: ParameterRange, include_end: bool) -> bool {
    let sweep = range.end - range.start;
    let directed = if sweep >= 0.0 {
        (parameter - range.start).rem_euclid(std::f64::consts::TAU)
    } else {
        (range.start - parameter).rem_euclid(std::f64::consts::TAU)
    };
    if include_end {
        directed <= sweep.abs() + 64.0 * f64::EPSILON
    } else {
        directed < sweep.abs() - 64.0 * f64::EPSILON || directed <= 64.0 * f64::EPSILON
    }
}

/// Conservatively certifies that the open circular sweep does not encounter
/// another committed boundary, including a sibling solid. The local rewrite
/// is not a general Boolean operation: any possible intermediate contact is
/// rejected before topology is changed. Parallel cylinders receive an exact
/// radial-distance test; perpendicular or otherwise unsupported cylinder
/// contacts are rejected from exact analytic face bounds.
#[allow(clippy::too_many_arguments)]
fn circular_sweep_contacts_source(
    topology: &Topology,
    target_face_index: usize,
    exit_face_index: Option<usize>,
    target_plane: Plane,
    center: Point3,
    radius: f64,
    direction: Vector3,
    distance: f64,
    clearance: f64,
    angular_tolerance: f64,
) -> bool {
    let basis = [target_plane.u, target_plane.v, direction];
    topology
        .faces
        .iter()
        .enumerate()
        .filter(|(face_index, _)| {
            *face_index != target_face_index && Some(*face_index) != exit_face_index
        })
        .any(|(_, face_record)| {
            let face = &face_record.value;
            let Some(bounds) = face_bounds_in_sweep_frame(topology, face, center, basis) else {
                // A committed face should always have an exact finite boundary.
                // If that invariant cannot be recovered here, construction is
                // conservatively unsupported rather than collision-blind.
                return true;
            };
            let lies_on_start_plane = bounds[2]
                .into_iter()
                .all(|coordinate| coordinate.abs() <= clearance);
            if lies_on_start_plane
                || bounds[2][1] <= clearance
                || bounds[2][0] > distance + clearance
            {
                return false;
            }
            if bounds[0][1] < -radius - clearance
                || bounds[0][0] > radius + clearance
                || bounds[1][1] < -radius - clearance
                || bounds[1][0] > radius + clearance
            {
                return false;
            }

            match face.surface {
                Surface::Plane(plane) => {
                    let Some(normal) = robust_unit(plane.normal) else {
                        return true;
                    };
                    let axial_alignment = normal.dot(direction).abs();
                    if axial_alignment >= 1.0 - angular_tolerance {
                        // A transverse plane only contacts the sweep when its
                        // trimmed material domain does. This is essential for
                        // a concentric hole passing through an earlier boss:
                        // the shoulder AABB overlaps, but its circular void
                        // contains the entire smaller sweep disk.
                        return circle_overlaps_planar_face_material(
                            topology, face, plane, center, radius, clearance,
                        );
                    }
                    if axial_alignment <= angular_tolerance {
                        // For a plane parallel to the axis, its exact distance
                        // from the sweep axis decides whether contact is possible.
                        return (center - plane.origin).dot(normal).abs() <= radius + clearance;
                    }
                    true
                }
                Surface::Cylinder(cylinder) => {
                    let Some(axis) = robust_unit(cylinder.axis) else {
                        return true;
                    };
                    if axis.dot(direction).abs() >= 1.0 - angular_tolerance {
                        let offset = cylinder.origin - center;
                        let perpendicular = offset - direction * offset.dot(direction);
                        let distance_to_surface = (perpendicular.length() - cylinder.radius).abs();
                        return distance_to_surface <= radius + clearance;
                    }
                    // The current local rewrite cannot split or merge a
                    // perpendicular cylindrical boundary. Exact bounds prove
                    // broad-phase overlap; reject the unsupported contact.
                    true
                }
            }
        })
}

fn circle_overlaps_planar_face_material(
    topology: &Topology,
    face: &Face,
    plane: Plane,
    world_center: Point3,
    radius: f64,
    clearance: f64,
) -> bool {
    let center = plane.project(world_center);
    let center_is_material = point_in_loop(topology, face.outer_loop, center)
        && !face
            .inner_loops
            .iter()
            .any(|loop_key| point_in_loop(topology, *loop_key, center));
    if center_is_material {
        return true;
    }
    face.loops()
        .any(|loop_key| minimum_distance_to_loop(topology, loop_key, center) <= radius + clearance)
}

fn face_bounds_in_sweep_frame(
    topology: &Topology,
    face: &Face,
    origin: Point3,
    basis: [Vector3; 3],
) -> Option<[[f64; 2]; 3]> {
    let mut bounds = [[f64::INFINITY, f64::NEG_INFINITY]; 3];
    let mut included = false;
    let mut include = |point: Point3| {
        if !point.is_finite() {
            return false;
        }
        for axis in 0..3 {
            let coordinate = (point - origin).dot(basis[axis]);
            if !coordinate.is_finite() {
                return false;
            }
            bounds[axis][0] = bounds[axis][0].min(coordinate);
            bounds[axis][1] = bounds[axis][1].max(coordinate);
        }
        included = true;
        true
    };

    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let edge = &topology.coedge(*coedge_key)?.value;
            let edge = &topology.edge(edge.edge)?.value;
            for endpoint in edge.endpoints() {
                if !include(endpoint) {
                    return None;
                }
            }
            let Curve3::Circle {
                center,
                u,
                v,
                radius,
            } = edge.curve
            else {
                continue;
            };
            for axis in basis {
                let u_component = u.dot(axis);
                let v_component = v.dot(axis);
                if u_component == 0.0 && v_component == 0.0 {
                    continue;
                }
                let extremum = v_component.atan2(u_component);
                for angle in [extremum, extremum + std::f64::consts::PI] {
                    if parameter_on_range(angle, edge.parameter_range, true)
                        && !include(
                            Curve3::Circle {
                                center,
                                u,
                                v,
                                radius,
                            }
                            .evaluate(angle),
                        )
                    {
                        return None;
                    }
                }
            }
        }
    }
    included.then_some(bounds)
}

#[allow(clippy::too_many_arguments)]
fn append_circle_feature(
    topology: &mut Topology,
    shell_index: usize,
    target_face_index: usize,
    exit_face_index: Option<usize>,
    target_plane: Plane,
    center: Point3,
    radius: f64,
    direction: Vector3,
    distance: f64,
    operation: FaceExtrusionOperation,
) {
    let mut next_id = topology
        .vertices
        .iter()
        .map(|record| record.id.get())
        .chain(topology.edges.iter().map(|record| record.id.get()))
        .chain(topology.coedges.iter().map(|record| record.id.get()))
        .chain(topology.loops.iter().map(|record| record.id.get()))
        .chain(topology.faces.iter().map(|record| record.id.get()))
        .chain(topology.shells.iter().map(|record| record.id.get()))
        .chain(topology.solids.iter().map(|record| record.id.get()))
        .max()
        .unwrap_or(0)
        + 1;
    let end_center = center + direction * distance;
    let seam_offsets = [target_plane.u * radius, target_plane.u * -radius];
    let mut target_vertices = Vec::new();
    let mut end_vertices = Vec::new();
    for offset in seam_offsets {
        target_vertices.push(push_vertex(topology, &mut next_id, center + offset));
    }
    for offset in seam_offsets {
        end_vertices.push(push_vertex(topology, &mut next_id, end_center + offset));
    }
    let target_edges = [
        push_circle_edge(
            topology,
            &mut next_id,
            [target_vertices[0], target_vertices[1]],
            center,
            target_plane,
            radius,
            ParameterRange::new(0.0, std::f64::consts::PI),
        ),
        push_circle_edge(
            topology,
            &mut next_id,
            [target_vertices[1], target_vertices[0]],
            center,
            target_plane,
            radius,
            ParameterRange::new(std::f64::consts::PI, std::f64::consts::TAU),
        ),
    ];
    let end_edges = [
        push_circle_edge(
            topology,
            &mut next_id,
            [end_vertices[0], end_vertices[1]],
            end_center,
            target_plane,
            radius,
            ParameterRange::new(0.0, std::f64::consts::PI),
        ),
        push_circle_edge(
            topology,
            &mut next_id,
            [end_vertices[1], end_vertices[0]],
            end_center,
            target_plane,
            radius,
            ParameterRange::new(std::f64::consts::PI, std::f64::consts::TAU),
        ),
    ];
    let vertical_edges = [
        push_line_edge(
            topology,
            &mut next_id,
            [target_vertices[0], end_vertices[0]],
        ),
        push_line_edge(
            topology,
            &mut next_id,
            [target_vertices[1], end_vertices[1]],
        ),
    ];

    let target_center = target_plane.project(center);
    let target_inner = push_circle_loop(
        topology,
        &mut next_id,
        target_edges,
        target_center,
        radius,
        true,
        target_plane.u,
        target_plane.v,
        target_plane,
    );
    topology.faces[target_face_index]
        .value
        .inner_loops
        .push(target_inner);

    let end_plane = if let Some(exit_index) = exit_face_index {
        topology.faces[exit_index]
            .value
            .surface
            .as_plane()
            .expect("certified exit is planar")
    } else {
        Plane::new(
            target_plane.origin + direction * distance,
            target_plane.u,
            target_plane.v,
        )
    };
    if let Some(exit_index) = exit_face_index {
        let exit_center = end_plane.project(end_center);
        let exit_loop = push_circle_loop(
            topology,
            &mut next_id,
            end_edges,
            exit_center,
            radius,
            false,
            target_plane.u,
            target_plane.v,
            end_plane,
        );
        topology.faces[exit_index].value.inner_loops.push(exit_loop);
    } else {
        let end_loop = push_circle_loop(
            topology,
            &mut next_id,
            end_edges,
            target_center,
            radius,
            false,
            target_plane.u,
            target_plane.v,
            end_plane,
        );
        let face_key = FaceKey(topology.faces.len());
        topology.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: Surface::Plane(end_plane),
                outer_loop: end_loop,
                inner_loops: Vec::new(),
                role: FaceRole::FeatureEnd,
            },
        });
        topology.shells[shell_index].value.faces.push(face_key);
    }

    for half in 0..2 {
        let start_angle = half as f64 * std::f64::consts::PI;
        let end_angle = start_angle + std::f64::consts::PI;
        let start_seam = half;
        let end_seam = 1 - half;
        let uses = vec![
            line_use(
                target_edges[half],
                Orientation::Forward,
                [Point2::new(start_angle, 0.0), Point2::new(end_angle, 0.0)],
            ),
            line_use(
                vertical_edges[end_seam],
                Orientation::Forward,
                [
                    Point2::new(end_angle, 0.0),
                    Point2::new(end_angle, distance),
                ],
            ),
            line_use(
                end_edges[half],
                Orientation::Reverse,
                [
                    Point2::new(end_angle, distance),
                    Point2::new(start_angle, distance),
                ],
            ),
            line_use(
                vertical_edges[start_seam],
                Orientation::Reverse,
                [
                    Point2::new(start_angle, distance),
                    Point2::new(start_angle, 0.0),
                ],
            ),
        ];
        let loop_key = push_loop(topology, &mut next_id, uses);
        let face_key = FaceKey(topology.faces.len());
        topology.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: Surface::Cylinder(Cylinder {
                    origin: center,
                    axis: direction,
                    radial_u: target_plane.u,
                    radial_v: target_plane.v * direction.dot(target_plane.normal),
                    radius,
                    angular_sign: direction.dot(target_plane.normal),
                }),
                outer_loop: loop_key,
                inner_loops: Vec::new(),
                role: FaceRole::FeatureSide(half as u32),
            },
        });
        topology.shells[shell_index].value.faces.push(face_key);
    }

    let _ = operation;
}

fn push_vertex(topology: &mut Topology, next_id: &mut u64, point: Point3) -> VertexKey {
    let key = VertexKey(topology.vertices.len());
    topology.vertices.push(Record {
        id: allocate_id(next_id),
        value: Vertex { point },
    });
    key
}

fn push_circle_edge(
    topology: &mut Topology,
    next_id: &mut u64,
    vertices: [VertexKey; 2],
    center: Point3,
    plane: Plane,
    radius: f64,
    range: ParameterRange,
) -> EdgeKey {
    let key = EdgeKey(topology.edges.len());
    topology.edges.push(Record {
        id: allocate_id(next_id),
        value: Edge {
            vertices,
            curve: Curve3::Circle {
                center,
                u: plane.u,
                v: plane.v,
                radius,
            },
            parameter_range: range,
        },
    });
    key
}

fn push_line_edge(topology: &mut Topology, next_id: &mut u64, vertices: [VertexKey; 2]) -> EdgeKey {
    let endpoints = vertices.map(|vertex| topology.vertices[vertex.0].value.point);
    let key = EdgeKey(topology.edges.len());
    topology.edges.push(Record {
        id: allocate_id(next_id),
        value: Edge::line(vertices, endpoints),
    });
    key
}

#[allow(clippy::too_many_arguments)]
fn push_circle_loop(
    topology: &mut Topology,
    next_id: &mut u64,
    edges: [EdgeKey; 2],
    center: Point2,
    radius: f64,
    reverse: bool,
    world_u: Vector3,
    world_v: Vector3,
    plane: Plane,
) -> LoopKey {
    let projected_u = Vector2::new(world_u.dot(plane.u), world_u.dot(plane.v));
    let projected_v = Vector2::new(world_v.dot(plane.u), world_v.dot(plane.v));
    let ranges = [
        ParameterRange::new(0.0, std::f64::consts::PI),
        ParameterRange::new(std::f64::consts::PI, std::f64::consts::TAU),
    ];
    let order: [usize; 2] = if reverse { [1, 0] } else { [0, 1] };
    let uses = order
        .into_iter()
        .map(|index| BoundaryUse {
            edge: edges[index],
            orientation: if reverse {
                Orientation::Reverse
            } else {
                Orientation::Forward
            },
            pcurve: Curve2::Circle {
                center,
                u: projected_u,
                v: projected_v,
                radius,
            },
            range: if reverse {
                ranges[index].reversed()
            } else {
                ranges[index]
            },
        })
        .collect();
    push_loop(topology, next_id, uses)
}

#[derive(Clone, Copy)]
struct BoundaryUse {
    edge: EdgeKey,
    orientation: Orientation,
    pcurve: Curve2,
    range: ParameterRange,
}

fn line_use(edge: EdgeKey, orientation: Orientation, endpoints: [Point2; 2]) -> BoundaryUse {
    let (pcurve, range) = Curve2::line_segment(endpoints);
    BoundaryUse {
        edge,
        orientation,
        pcurve,
        range,
    }
}

fn push_loop(topology: &mut Topology, next_id: &mut u64, uses: Vec<BoundaryUse>) -> LoopKey {
    let mut coedges = Vec::with_capacity(uses.len());
    for boundary in uses {
        let key = CoedgeKey(topology.coedges.len());
        topology.coedges.push(Record {
            id: allocate_id(next_id),
            value: Coedge {
                edge: boundary.edge,
                orientation: boundary.orientation,
                pcurve: boundary.pcurve,
                parameter_range: boundary.range,
            },
        });
        coedges.push(key);
    }
    let key = LoopKey(topology.loops.len());
    topology.loops.push(Record {
        id: allocate_id(next_id),
        value: Loop { coedges },
    });
    key
}

fn allocate_id(next_id: &mut u64) -> EntityId {
    let id = EntityId::from_raw(*next_id);
    *next_id += 1;
    id
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        ArcDirection, EntityId as ProtocolEntityId, EntityKind, EntityRef, FaceExtrusionOperation,
        PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
        Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy, SnapshotId,
        Vector3 as ProtocolVector3,
    };

    use super::validate_analytic_face_feature;
    use crate::cuboid::build_cuboid;
    use crate::face_feature::FaceFeatureInputError;
    use crate::planar_profile::PlanarProfileInputError;
    use crate::topology::{FaceRole, Point3, Vector3};
    use crate::validator;

    fn fixture() -> (
        SnapshotId,
        crate::topology::Topology,
        EntityRef,
        PlanarFrame3,
        PlanarProfile2,
    ) {
        let snapshot = SnapshotId::new([17; 16]);
        let topology = build_cuboid(Point3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0));
        let face = topology
            .faces
            .iter()
            .find(|face| face.value.role == FaceRole::PositiveZ)
            .expect("positive Z face");
        let target = EntityRef {
            snapshot,
            entity: ProtocolEntityId(face.id.get()),
            kind: EntityKind::Face,
        };
        let frame = PlanarFrame3 {
            origin: ProtocolPoint3::new(0.0, 0.0, 10.0),
            u: ProtocolVector3::new(1.0, 0.0, 0.0),
            v: ProtocolVector3::new(0.0, 1.0, 0.0),
        };
        let profile = PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        center: ProtocolPoint2::new(5.0, 5.0),
                        radius: 2.0,
                        direction: ArcDirection::CounterClockwise,
                    }],
                },
                holes: Vec::new(),
            }],
        };
        (snapshot, topology, target, frame, profile)
    }

    #[test]
    fn exact_circle_add_blind_cut_and_through_cut_validate() {
        for (operation, distance, expected_faces, expected_hole_faces, volume, surface) in [
            (
                FaceExtrusionOperation::Add,
                3.0,
                9,
                1,
                1000.0 + 12.0 * std::f64::consts::PI,
                600.0 + 12.0 * std::f64::consts::PI,
            ),
            (
                FaceExtrusionOperation::Cut,
                3.0,
                9,
                1,
                1000.0 - 12.0 * std::f64::consts::PI,
                600.0 + 12.0 * std::f64::consts::PI,
            ),
            (
                FaceExtrusionOperation::Cut,
                20.0,
                8,
                2,
                1000.0 - 40.0 * std::f64::consts::PI,
                600.0 + 32.0 * std::f64::consts::PI,
            ),
        ] {
            let (snapshot, topology, target, frame, profile) = fixture();
            let feature = validate_analytic_face_feature(
                snapshot,
                &topology,
                target,
                frame,
                &profile,
                distance,
                operation,
                PrecisionPolicy::default(),
            )
            .expect("exact circle feature validates");
            let report = validator::validate(&feature.topology, 1.0e-9);
            assert!(
                report.is_valid(),
                "{operation:?}: {:#?}",
                report.diagnostics
            );
            assert_eq!(feature.topology.faces.len(), expected_faces);
            assert!((report.measures.signed_volume - volume).abs() <= 1.0e-9);
            assert!((report.measures.surface_area - surface).abs() <= 1.0e-9);
            assert_eq!(
                feature
                    .topology
                    .faces
                    .iter()
                    .filter(|face| !face.value.inner_loops.is_empty())
                    .count(),
                expected_hole_faces
            );
            assert_eq!(
                feature.exit_face_index.is_some(),
                operation == FaceExtrusionOperation::Cut && distance > 10.0
            );
        }
    }

    #[test]
    fn every_cuboid_face_sign_accepts_exact_circle_add_blind_and_through_cut() {
        for role in [
            FaceRole::NegativeX,
            FaceRole::PositiveX,
            FaceRole::NegativeY,
            FaceRole::PositiveY,
            FaceRole::NegativeZ,
            FaceRole::PositiveZ,
        ] {
            for (operation, distance) in [
                (FaceExtrusionOperation::Add, 1.0),
                (FaceExtrusionOperation::Cut, 1.0),
                (FaceExtrusionOperation::Cut, 20.0),
            ] {
                let snapshot = SnapshotId::new([23; 16]);
                let topology =
                    build_cuboid(Point3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0));
                let face = topology
                    .faces
                    .iter()
                    .find(|face| face.value.role == role)
                    .expect("cuboid role");
                let plane = face.value.surface.as_plane().expect("cuboid plane");
                let boundary = validator::face_polygon(&topology, face.value.outer_loop)
                    .expect("cuboid boundary");
                let count = boundary.len() as f64;
                let center = Point3::new(
                    boundary.iter().map(|point| point.x).sum::<f64>() / count,
                    boundary.iter().map(|point| point.y).sum::<f64>() / count,
                    boundary.iter().map(|point| point.z).sum::<f64>() / count,
                );
                let target = EntityRef {
                    snapshot,
                    entity: ProtocolEntityId(face.id.get()),
                    kind: EntityKind::Face,
                };
                let frame = PlanarFrame3 {
                    origin: ProtocolPoint3::new(center.x, center.y, center.z),
                    u: ProtocolVector3::new(plane.u.x, plane.u.y, plane.u.z),
                    v: ProtocolVector3::new(plane.v.x, plane.v.y, plane.v.z),
                };
                let profile = PlanarProfile2 {
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
                };
                let feature = validate_analytic_face_feature(
                    snapshot,
                    &topology,
                    target,
                    frame,
                    &profile,
                    distance,
                    operation,
                    PrecisionPolicy::default(),
                )
                .unwrap_or_else(|error| panic!("{role:?} {operation:?}: {error:?}"));
                let report = validator::validate(&feature.topology, 1.0e-9);
                assert!(
                    report.is_valid(),
                    "{role:?} {operation:?}: {:#?}",
                    report.diagnostics
                );
            }
        }
    }

    #[test]
    fn perpendicular_cut_crossing_an_existing_cylinder_is_rejected_before_rewrite() {
        let (snapshot, topology, target, frame, profile) = fixture();
        let pocket = validate_analytic_face_feature(
            snapshot,
            &topology,
            target,
            frame,
            &profile,
            6.0,
            FaceExtrusionOperation::Cut,
            PrecisionPolicy::default(),
        )
        .expect("first exact pocket");
        let positive_x = pocket
            .topology
            .faces
            .iter()
            .find(|face| face.value.role == FaceRole::PositiveX)
            .expect("positive X source face");
        let side_target = EntityRef {
            snapshot,
            entity: ProtocolEntityId(positive_x.id.get()),
            kind: EntityKind::Face,
        };
        let crossing_circle = PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        // Positive X uses local Y/Z coordinates. The -X cut
                        // axis crosses the existing -Z pocket cylinder.
                        center: ProtocolPoint2::new(5.0, 7.0),
                        radius: 1.0,
                        direction: ArcDirection::CounterClockwise,
                    }],
                },
                holes: Vec::new(),
            }],
        };
        let error = validate_analytic_face_feature(
            snapshot,
            &pocket.topology,
            side_target,
            PlanarFrame3 {
                origin: ProtocolPoint3::new(10.0, 0.0, 0.0),
                u: ProtocolVector3::new(0.0, 1.0, 0.0),
                v: ProtocolVector3::new(0.0, 0.0, 1.0),
            },
            &crossing_circle,
            7.0,
            FaceExtrusionOperation::Cut,
            PrecisionPolicy::default(),
        )
        .expect_err("a perpendicular existing cylinder must stop the cut sweep");
        assert_eq!(
            error,
            PlanarProfileInputError::FaceFeature(FaceFeatureInputError::SweepCollision)
        );
        assert_eq!(pocket.topology.faces.len(), 9);
        assert!(validator::validate(&pocket.topology, 1.0e-9).is_valid());
    }
}

//! First exact 3D edge-finish domain: compatible complete axis-aligned cuboid edges.

use artificer_protocol::{
    ArcDirection, EdgeFinishKind, EntityKind, EntityRef, PlanarCurve2, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3,
    PrecisionPolicy, Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::{build_analytic_extrusion, validate_analytic_profile_extrusion};
use crate::topology::{Point3, Surface, Topology, Vector3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeFinishError {
    TargetInvalid,
    DomainUnsupported,
    DistanceInvalid,
    ConstructionFailed,
}

pub(crate) fn build_edge_finish(
    snapshot: artificer_protocol::SnapshotId,
    topology: &Topology,
    target: EntityRef,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, EdgeFinishError> {
    build_edge_finishes(snapshot, topology, &[target], kind, distance, precision)
}

pub(crate) fn build_edge_finishes(
    snapshot: artificer_protocol::SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, EdgeFinishError> {
    if targets.is_empty() {
        return Err(EdgeFinishError::TargetInvalid);
    }
    if targets
        .iter()
        .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(EdgeFinishError::TargetInvalid);
    }
    let mut target_ids = targets
        .iter()
        .map(|target| target.entity.0)
        .collect::<Vec<_>>();
    target_ids.sort_unstable();
    target_ids.dedup();
    if target_ids.len() != targets.len() {
        return Err(EdgeFinishError::TargetInvalid);
    }
    // A cuboid has twelve edges, so a selection of more than sixty-four is
    // certainly not this rung's to answer. Saying so as a domain refusal lets
    // the ladder carry it on — a rim of seventy edges is the rim-loop blend's
    // business — where calling it an invalid target would stop there with a
    // reason that is not true.
    if targets.len() > 64 {
        return Err(EdgeFinishError::DomainUnsupported);
    }
    if !is_axis_aligned_cuboid(topology, precision) {
        return Err(EdgeFinishError::DomainUnsupported);
    }
    let edge = topology
        .edges
        .iter()
        .find(|edge| edge.id.get() == targets[0].entity.0)
        .ok_or(EdgeFinishError::TargetInvalid)?;
    let [first_start, first_end] = edge.value.endpoints();
    let delta = first_end - first_start;
    let components = [delta.x.abs(), delta.y.abs(), delta.z.abs()];
    let varying = components
        .iter()
        .enumerate()
        .filter_map(|(axis, component)| (*component > precision.linear_agreement).then_some(axis))
        .collect::<Vec<_>>();
    if varying.len() != 1 {
        return Err(EdgeFinishError::DomainUnsupported);
    }
    let edge_axis = varying[0];
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for vertex in &topology.vertices {
        let coordinates = [
            vertex.value.point.x,
            vertex.value.point.y,
            vertex.value.point.z,
        ];
        for axis in 0..3 {
            min[axis] = min[axis].min(coordinates[axis]);
            max[axis] = max[axis].max(coordinates[axis]);
        }
    }
    let mut start_coordinates = [first_start.x, first_start.y, first_start.z];
    start_coordinates[edge_axis] = min[edge_axis];
    let mut end_coordinates = start_coordinates;
    end_coordinates[edge_axis] = max[edge_axis];
    let mut start = Point3::new(
        start_coordinates[0],
        start_coordinates[1],
        start_coordinates[2],
    );
    let mut end = Point3::new(end_coordinates[0], end_coordinates[1], end_coordinates[2]);
    let perpendicular = (0..3).filter(|axis| *axis != edge_axis).collect::<Vec<_>>();
    let inward = |axis: usize| {
        let coordinate = start_coordinates[axis];
        if (coordinate - min[axis]).abs() <= precision.linear_agreement {
            axis_vector(axis, 1.0)
        } else if (coordinate - max[axis]).abs() <= precision.linear_agreement {
            axis_vector(axis, -1.0)
        } else {
            Vector3::new(0.0, 0.0, 0.0)
        }
    };
    let mut u = inward(perpendicular[0]);
    let mut v = inward(perpendicular[1]);
    if u.length() == 0.0 || v.length() == 0.0 {
        return Err(EdgeFinishError::DomainUnsupported);
    }
    let mut width = max[perpendicular[0]] - min[perpendicular[0]];
    let mut height = max[perpendicular[1]] - min[perpendicular[1]];
    let mut normal = u.cross(v);
    if normal.dot(end - start) < 0.0 {
        std::mem::swap(&mut u, &mut v);
        std::mem::swap(&mut width, &mut height);
        normal = u.cross(v);
    }
    if normal.dot(end - start) < 0.0 {
        std::mem::swap(&mut start, &mut end);
    }
    let edge_length = start.distance(end);
    if !distance.is_finite()
        || distance < precision.min_feature_size
        || distance >= width.min(height) - precision.min_feature_size
    {
        return Err(EdgeFinishError::DistanceInvalid);
    }
    let mut corner_intervals: [Vec<(f64, f64)>; 4] = std::array::from_fn(|_| Vec::new());
    for target in targets {
        let edge = topology
            .edges
            .iter()
            .find(|edge| edge.id.get() == target.entity.0)
            .ok_or(EdgeFinishError::TargetInvalid)?;
        let [target_start, target_end] = edge.value.endpoints();
        let target_delta = target_end - target_start;
        let target_components = [
            target_delta.x.abs(),
            target_delta.y.abs(),
            target_delta.z.abs(),
        ];
        let target_varying = target_components
            .iter()
            .enumerate()
            .filter_map(|(axis, component)| {
                (*component > precision.linear_agreement).then_some(axis)
            })
            .collect::<Vec<_>>();
        if target_varying.as_slice() != [edge_axis] {
            return Err(EdgeFinishError::DomainUnsupported);
        }
        let target_low = [target_start.x, target_start.y, target_start.z][edge_axis]
            .min([target_end.x, target_end.y, target_end.z][edge_axis]);
        let target_high = [target_start.x, target_start.y, target_start.z][edge_axis]
            .max([target_end.x, target_end.y, target_end.z][edge_axis]);
        if target_low < min[edge_axis] - precision.linear_agreement
            || target_high > max[edge_axis] + precision.linear_agreement
        {
            return Err(EdgeFinishError::DomainUnsupported);
        }
        let local = target_start - start;
        let x = local.dot(u);
        let y = local.dot(v);
        let corner = match (
            near(x, 0.0, precision.linear_agreement),
            near(x, width, precision.linear_agreement),
            near(y, 0.0, precision.linear_agreement),
            near(y, height, precision.linear_agreement),
        ) {
            (true, _, true, _) => 0,
            (_, true, true, _) => 1,
            (_, true, _, true) => 2,
            (true, _, _, true) => 3,
            _ => return Err(EdgeFinishError::DomainUnsupported),
        };
        corner_intervals[corner].push((target_low, target_high));
    }
    let finished = std::array::from_fn(|corner| {
        !corner_intervals[corner].is_empty()
            && intervals_cover_extent(
                &mut corner_intervals[corner],
                min[edge_axis],
                max[edge_axis],
                precision.linear_agreement,
            )
    });
    if corner_intervals
        .iter()
        .enumerate()
        .any(|(corner, intervals)| !intervals.is_empty() && !finished[corner])
    {
        return Err(EdgeFinishError::DomainUnsupported);
    }
    let side_lengths = [width, height, width, height];
    for side in 0..4 {
        let setbacks = usize::from(finished[side]) + usize::from(finished[(side + 1) % 4]);
        if setbacks > 0
            && distance * setbacks as f64 > side_lengths[side] - precision.min_feature_size
        {
            return Err(EdgeFinishError::DistanceInvalid);
        }
    }

    let curves = finished_rectangle_curves(width, height, distance, kind, finished);
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves },
            holes: Vec::new(),
        }],
    };
    let frame = PlanarFrame3::new(
        protocol_point(start),
        protocol_vector(u),
        protocol_vector(v),
    );
    let validated = validate_analytic_profile_extrusion(frame, &profile, edge_length, precision)
        .map_err(|_| EdgeFinishError::ConstructionFailed)?;
    Ok(build_analytic_extrusion(&validated))
}

/// Whether the body is exactly the box its vertices span, with its faces on
/// the world axes.
///
/// This rung rebuilds the finished body from that box, so six planar faces are
/// not enough: a trapezoid prism has six too, and rebuilding it from its
/// bounding box fillets a body that was never there. Eight vertices, one on
/// each corner of the box, and six faces each square to an axis leave nothing
/// else a body could be.
fn is_axis_aligned_cuboid(topology: &Topology, precision: PrecisionPolicy) -> bool {
    if topology.solids.len() != 1
        || topology.faces.len() != 6
        || topology.vertices.len() != 8
        || topology.edges.len() != 12
    {
        return false;
    }
    let angular = precision.angular_agreement_radians.max(1.0e-12);
    let faces_on_axes = topology.faces.iter().all(|face| {
        let Surface::Plane(plane) = face.value.surface else {
            return false;
        };
        let length = plane.normal.length();
        if !length.is_finite() || length <= f64::EPSILON {
            return false;
        }
        let normal = [plane.normal.x, plane.normal.y, plane.normal.z].map(|value| value / length);
        normal
            .iter()
            .filter(|component| component.abs() > angular)
            .count()
            == 1
    });
    if !faces_on_axes {
        return false;
    }
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for vertex in &topology.vertices {
        let point = vertex.value.point;
        for (axis, value) in [point.x, point.y, point.z].into_iter().enumerate() {
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    let tolerance = precision.linear_agreement;
    if (0..3).any(|axis| max[axis] - min[axis] <= tolerance) {
        return false;
    }
    // Each vertex names one corner by which end of each axis it sits at; a
    // box has all eight, once each.
    let mut corners = [false; 8];
    for vertex in &topology.vertices {
        let point = vertex.value.point;
        let mut corner = 0;
        for (axis, value) in [point.x, point.y, point.z].into_iter().enumerate() {
            if near(value, max[axis], tolerance) {
                corner |= 1 << axis;
            } else if !near(value, min[axis], tolerance) {
                return false;
            }
        }
        if std::mem::replace(&mut corners[corner], true) {
            return false;
        }
    }
    true
}

fn intervals_cover_extent(
    intervals: &mut [(f64, f64)],
    extent_min: f64,
    extent_max: f64,
    tolerance: f64,
) -> bool {
    intervals.sort_by(|left, right| left.0.total_cmp(&right.0));
    let Some(first) = intervals.first().copied() else {
        return false;
    };
    if (first.0 - extent_min).abs() > tolerance {
        return false;
    }
    let mut covered = first.1;
    for (start, end) in intervals.iter().copied().skip(1) {
        if start > covered + tolerance {
            return false;
        }
        covered = covered.max(end);
    }
    (covered - extent_max).abs() <= tolerance
}

fn finished_rectangle_curves(
    width: f64,
    height: f64,
    distance: f64,
    kind: EdgeFinishKind,
    finished: [bool; 4],
) -> Vec<PlanarCurve2> {
    let corners = [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)];
    let incoming = [
        (0.0, distance),
        (width - distance, 0.0),
        (width, height - distance),
        (distance, height),
    ];
    let outgoing = [
        (distance, 0.0),
        (width, distance),
        (width - distance, height),
        (0.0, height - distance),
    ];
    let centers = [
        (distance, distance),
        (width - distance, distance),
        (width - distance, height - distance),
        (distance, height - distance),
    ];
    let incoming: [(f64, f64); 4] = std::array::from_fn(|index| {
        if finished[index] {
            incoming[index]
        } else {
            corners[index]
        }
    });
    let outgoing: [(f64, f64); 4] = std::array::from_fn(|index| {
        if finished[index] {
            outgoing[index]
        } else {
            corners[index]
        }
    });
    let mut curves = Vec::with_capacity(8);
    for index in 0..4 {
        let next = (index + 1) % 4;
        if outgoing[index] != incoming[next] {
            curves.push(line(outgoing[index], incoming[next]));
        }
        if finished[next] {
            curves.push(match kind {
                EdgeFinishKind::Chamfer => line(incoming[next], outgoing[next]),
                EdgeFinishKind::Fillet => PlanarCurve2::CircularArc {
                    center: ProtocolPoint2::new(centers[next].0, centers[next].1),
                    start: ProtocolPoint2::new(incoming[next].0, incoming[next].1),
                    end: ProtocolPoint2::new(outgoing[next].0, outgoing[next].1),
                    direction: ArcDirection::CounterClockwise,
                },
            });
        }
    }
    curves
}

fn near(left: f64, right: f64, tolerance: f64) -> bool {
    (left - right).abs() <= tolerance
}

fn line(start: (f64, f64), end: (f64, f64)) -> PlanarCurve2 {
    PlanarCurve2::Line {
        start: ProtocolPoint2::new(start.0, start.1),
        end: ProtocolPoint2::new(end.0, end.1),
    }
}

const fn axis_vector(axis: usize, sign: f64) -> Vector3 {
    match axis {
        0 => Vector3::new(sign, 0.0, 0.0),
        1 => Vector3::new(0.0, sign, 0.0),
        _ => Vector3::new(0.0, 0.0, sign),
    }
}

const fn protocol_point(point: Point3) -> ProtocolPoint3 {
    ProtocolPoint3::new(point.x, point.y, point.z)
}

const fn protocol_vector(vector: Vector3) -> ProtocolVector3 {
    ProtocolVector3::new(vector.x, vector.y, vector.z)
}

//! One exact selected-face Add/Cut path for certified planar profiles.
//!
//! This is the regularized local-prismatic subset of ADR 0020. It consumes the
//! same line/arc/circle region representation as standalone extrusion, imprints
//! every profile loop on a strictly containing planar support, and rebuilds the
//! retained planar cells plus exact planar/cylindrical sweep walls. Contacts
//! that would require splitting an existing non-transverse boundary remain a
//! typed, transactional rejection.

use std::collections::VecDeque;

use artificer_protocol::{
    EntityKind, EntityRef, FaceExtrusionOperation, PlanarFrame3, PlanarProfile2, PrecisionPolicy,
    SnapshotId,
};

use crate::analytic_extrusion::{
    AnalyticLoop, Frame, Segment, ValidatedAnalyticExtrusion, point_in_material, point_inside_loop,
    segment_clearance, topology_loop_segments, validate_analytic_profile_extrusion,
};
use crate::face_feature::FaceFeatureInputError;
use crate::planar_profile::PlanarProfileInputError;
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Cylinder, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole,
    Loop, LoopKey, Orientation, ParameterRange, Plane, Point2, Point3, Record, Surface, Topology,
    Vector2, Vector3, Vertex, VertexKey,
};

#[derive(Clone, Debug)]
pub(crate) struct ValidatedExactFaceFeature {
    pub(crate) topology: Topology,
    pub(crate) exit_face_index: Option<usize>,
}

#[derive(Clone, Debug)]
struct ProjectedRegion {
    loops: Vec<AnalyticLoop>,
}

#[derive(Clone, Debug)]
struct FeatureLoopKeys {
    start_edges: Vec<EdgeKey>,
    end_edges: Vec<EdgeKey>,
    sweep_edges: Vec<EdgeKey>,
}

#[derive(Clone, Copy)]
struct BoundaryUse {
    edge: EdgeKey,
    orientation: Orientation,
    pcurve: Curve2,
    range: ParameterRange,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_exact_face_feature(
    snapshot: SnapshotId,
    topology: &Topology,
    target_face: EntityRef,
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    distance: f64,
    operation: FaceExtrusionOperation,
    precision: PrecisionPolicy,
) -> Result<ValidatedExactFaceFeature, PlanarProfileInputError> {
    if target_face.snapshot != snapshot {
        return Err(face_error(FaceFeatureInputError::TargetSnapshotMismatch));
    }
    if target_face.kind != EntityKind::Face {
        return Err(face_error(FaceFeatureInputError::TargetNotFace));
    }

    // The shared exact validator is deliberately used for linear-only input as
    // well. There is no primitive-kind dispatch in the selected-face command.
    let extrusion = validate_analytic_profile_extrusion(frame, profile, distance, precision)?;
    let exact_frame = extrusion
        .regions
        .first()
        .map(|region| region.frame)
        .ok_or(PlanarProfileInputError::EmptyProfile)?;
    let target_face_index = topology
        .faces
        .iter()
        .position(|face| face.id.get() == target_face.entity.0)
        .ok_or_else(|| face_error(FaceFeatureInputError::TargetMissing))?;
    let target_plane = topology.faces[target_face_index]
        .value
        .surface
        .as_plane()
        .ok_or_else(|| face_error(FaceFeatureInputError::TargetNotPlanar))?;
    let target_normal = robust_unit(target_plane.normal)
        .ok_or_else(|| face_error(FaceFeatureInputError::TargetDegenerate))?;
    let angular_tolerance = precision.angular_agreement_radians.max(1.0e-12);
    // A reflected sketch frame carries an independently chosen sweep
    // direction. Parallel and anti-parallel normals are both valid; the
    // operation still determines whether that directed prism is added or
    // removed.
    if 1.0 - exact_frame.normal.dot(target_normal).abs() > angular_tolerance {
        return Err(face_error(FaceFeatureInputError::FrameOffTargetPlane));
    }
    if (exact_frame.origin - target_plane.origin)
        .dot(target_normal)
        .abs()
        > precision.linear_agreement
    {
        return Err(face_error(FaceFeatureInputError::FrameNotOnTarget));
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let projected = project_regions(&extrusion, target_plane, 0.0);
    if !profile_strictly_inside_face(
        topology,
        &topology.faces[target_face_index].value,
        &projected,
        minimum,
    ) {
        return Err(face_error(FaceFeatureInputError::ProfileOutsideFace));
    }

    let target_key = FaceKey(target_face_index);
    let shell_index = topology
        .shells
        .iter()
        .position(|shell| shell.value.faces.contains(&target_key))
        .ok_or_else(|| face_error(FaceFeatureInputError::SourceUnsupported))?;
    if topology
        .shells
        .iter()
        .filter(|shell| shell.value.faces.contains(&target_key))
        .count()
        != 1
    {
        return Err(face_error(FaceFeatureInputError::SourceUnsupported));
    }

    let direction = match operation {
        FaceExtrusionOperation::Add => target_normal,
        FaceExtrusionOperation::Cut => target_normal * -1.0,
    };
    let (feature_distance, exit_face_index) = match operation {
        FaceExtrusionOperation::Add => (distance, None),
        FaceExtrusionOperation::Cut => resolve_common_exit(
            topology,
            shell_index,
            target_face_index,
            target_plane,
            &extrusion,
            direction,
            distance,
            minimum,
            angular_tolerance,
            precision.linear_agreement,
        )?,
    };

    ensure_coordinate_envelope(
        &extrusion,
        direction,
        feature_distance,
        precision.max_abs_coordinate,
    )?;
    if sweep_contacts_source(
        topology,
        shell_index,
        target_face_index,
        exit_face_index,
        &extrusion,
        direction,
        feature_distance,
        minimum,
        angular_tolerance,
    ) {
        return Err(face_error(FaceFeatureInputError::SweepCollision));
    }
    // A truncating exit certifies "through" only if nothing lies beyond it.
    // The roof of an interior void — a slot or tunnel through the body —
    // passes the profile-containment test exactly as a true bottom face
    // does, yet material resumes past it, and the local rewrite would
    // silently leave that far side uncut. Sweeping the full requested depth
    // exposes any such resumption (the void's floor or crossing walls) as a
    // collision, which routes the cut to the real-difference fallback.
    if exit_face_index.is_some()
        && distance > feature_distance + precision.linear_agreement
        && sweep_contacts_source(
            topology,
            shell_index,
            target_face_index,
            exit_face_index,
            &extrusion,
            direction,
            distance,
            minimum,
            angular_tolerance,
        )
    {
        return Err(face_error(FaceFeatureInputError::SweepCollision));
    }

    let mut candidate = topology.clone();
    append_exact_feature(
        &mut candidate,
        shell_index,
        target_face_index,
        exit_face_index,
        target_plane,
        &extrusion,
        direction,
        feature_distance,
    );
    Ok(ValidatedExactFaceFeature {
        topology: candidate,
        exit_face_index,
    })
}

const fn face_error(error: FaceFeatureInputError) -> PlanarProfileInputError {
    PlanarProfileInputError::FaceFeature(error)
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

fn project_regions(
    extrusion: &ValidatedAnalyticExtrusion,
    plane: Plane,
    height: f64,
) -> Vec<ProjectedRegion> {
    extrusion
        .regions
        .iter()
        .map(|region| ProjectedRegion {
            loops: region
                .loops
                .iter()
                .map(|profile_loop| AnalyticLoop {
                    segments: profile_loop
                        .segments
                        .iter()
                        .copied()
                        .map(|segment| project_segment(segment, region.frame, plane, height))
                        .collect(),
                    signed_area: profile_loop.signed_area,
                })
                .collect(),
        })
        .collect()
}

fn project_segment(segment: Segment, frame: Frame, plane: Plane, height: f64) -> Segment {
    match segment {
        Segment::Line { start, end } => Segment::Line {
            start: plane.project(frame.point(start, height)),
            end: plane.project(frame.point(end, height)),
        },
        Segment::Arc {
            center,
            start,
            end,
            sweep,
            ..
        } => {
            let center = plane.project(frame.center(center, height));
            let start = plane.project(frame.point(start, height));
            let end = plane.project(frame.point(end, height));
            let radius = (start - center).x.hypot((start - center).y);
            Segment::Arc {
                center,
                start,
                end,
                radius,
                start_angle: (start.y - center.y).atan2(start.x - center.x),
                sweep,
            }
        }
        other @ (Segment::Ellipse { .. } | Segment::Harmonic { .. }) => other,
    }
}

fn profile_strictly_inside_face(
    topology: &Topology,
    face: &Face,
    regions: &[ProjectedRegion],
    minimum: f64,
) -> bool {
    let Some(face_boundaries) = face
        .loops()
        .map(|loop_key| topology_loop_segments(topology, loop_key))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    for region in regions {
        let Some(representative) = region
            .loops
            .first()
            .and_then(|profile_loop| profile_loop.segments.first())
            .map(|segment| segment.start())
        else {
            return false;
        };
        if !point_in_face_material(topology, face, representative, minimum) {
            return false;
        }
        if region.loops.iter().any(|profile_loop| {
            profile_loop.segments.iter().any(|profile_segment| {
                face_boundaries.iter().flatten().any(|face_segment| {
                    segment_clearance(*profile_segment, *face_segment) <= minimum
                })
            })
        }) {
            return false;
        }
    }

    // A profile that surrounds an existing support void would require
    // splitting/merging that void's side surfaces. Keep that contact explicit.
    for loop_key in &face.inner_loops {
        let Some(point) = topology_loop_segments(topology, *loop_key)
            .and_then(|segments| segments.first().copied())
            .map(Segment::start)
        else {
            return false;
        };
        if profile_material_at(point, regions) {
            return false;
        }
    }
    true
}

fn point_in_face_material(
    topology: &Topology,
    face: &Face,
    point: Point2,
    linear_tolerance: f64,
) -> bool {
    point_in_topology_loop(topology, face.outer_loop, point, linear_tolerance)
        && face
            .inner_loops
            .iter()
            .all(|loop_key| !point_in_topology_loop(topology, *loop_key, point, linear_tolerance))
}

fn profile_material_at(point: Point2, regions: &[ProjectedRegion]) -> bool {
    regions
        .iter()
        .any(|region| point_in_material(point, &region.loops))
}

fn point_in_topology_loop(
    topology: &Topology,
    loop_key: LoopKey,
    point: Point2,
    linear_tolerance: f64,
) -> bool {
    topology_loop_segments(topology, loop_key).is_some_and(|segments| {
        if let Some((center, radius)) = circular_loop_carrier(&segments, linear_tolerance) {
            return (point - center).x.hypot((point - center).y) <= radius + linear_tolerance;
        }
        let profile_loop = AnalyticLoop {
            segments,
            signed_area: 0.0,
        };
        point_inside_loop(point, &profile_loop)
    })
}

fn circular_loop_carrier(segments: &[Segment], linear_tolerance: f64) -> Option<(Point2, f64)> {
    let Segment::Arc {
        center,
        radius,
        sweep,
        ..
    } = *segments.first()?
    else {
        return None;
    };
    let orientation = sweep.signum();
    if orientation == 0.0 || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let mut total_sweep = 0.0;
    for segment in segments {
        let Segment::Arc {
            center: candidate_center,
            radius: candidate_radius,
            sweep: candidate_sweep,
            ..
        } = *segment
        else {
            return None;
        };
        if candidate_sweep.signum() != orientation
            || (candidate_center - center)
                .x
                .hypot((candidate_center - center).y)
                > linear_tolerance
            || (candidate_radius - radius).abs() > linear_tolerance
        {
            return None;
        }
        total_sweep += candidate_sweep;
    }
    ((total_sweep.abs() - std::f64::consts::TAU).abs() * radius <= linear_tolerance)
        .then_some((center, radius))
}

#[allow(clippy::too_many_arguments)]
fn resolve_common_exit(
    topology: &Topology,
    shell_index: usize,
    target_face_index: usize,
    target_plane: Plane,
    extrusion: &ValidatedAnalyticExtrusion,
    direction: Vector3,
    requested_distance: f64,
    minimum: f64,
    angular_tolerance: f64,
    linear_tolerance: f64,
) -> Result<(f64, Option<usize>), PlanarProfileInputError> {
    let mut exits = Vec::new();
    for face_key in &topology.shells[shell_index].value.faces {
        if face_key.0 == target_face_index {
            continue;
        }
        let face = &topology.faces[face_key.0].value;
        let Some(plane) = face.surface.as_plane() else {
            continue;
        };
        let Some(normal) = robust_unit(plane.normal) else {
            continue;
        };
        if normal.dot(target_plane.normal) > -1.0 + angular_tolerance {
            continue;
        }
        let depth = (plane.origin - target_plane.origin).dot(direction);
        if depth <= minimum {
            continue;
        }
        let projected = project_regions(extrusion, plane, -depth);
        if profile_strictly_inside_face(topology, face, &projected, minimum) {
            exits.push((depth, face_key.0));
        }
    }
    exits.sort_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(&right.1)));
    if let Some((depth, index)) = exits.first().copied()
        && requested_distance >= depth - linear_tolerance
    {
        return Ok((depth, Some(index)));
    }
    Ok((requested_distance, None))
}

fn ensure_coordinate_envelope(
    extrusion: &ValidatedAnalyticExtrusion,
    direction: Vector3,
    distance: f64,
    limit: f64,
) -> Result<(), PlanarProfileInputError> {
    for region in &extrusion.regions {
        for segment in region
            .loops
            .iter()
            .flat_map(|profile_loop| &profile_loop.segments)
        {
            let mut points = vec![segment.start(), segment.end()];
            if let Segment::Arc { center, radius, .. } = *segment {
                points.extend([
                    Point2::new(center.x - radius, center.y),
                    Point2::new(center.x + radius, center.y),
                    Point2::new(center.x, center.y - radius),
                    Point2::new(center.x, center.y + radius),
                ]);
            }
            for local in points {
                for world in [
                    region.frame.point(local, 0.0),
                    region.frame.point(local, 0.0) + direction * distance,
                ] {
                    if [world.x, world.y, world.z]
                        .into_iter()
                        .any(|coordinate| !coordinate.is_finite() || coordinate.abs() > limit)
                    {
                        return Err(face_error(FaceFeatureInputError::CoordinateLimit));
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn sweep_contacts_source(
    topology: &Topology,
    shell_index: usize,
    target_face_index: usize,
    exit_face_index: Option<usize>,
    extrusion: &ValidatedAnalyticExtrusion,
    direction: Vector3,
    distance: f64,
    clearance: f64,
    angular_tolerance: f64,
) -> bool {
    let frame = extrusion.regions[0].frame;
    let projected = project_regions(extrusion, Plane::new(frame.origin, frame.u, frame.v), 0.0);
    let Some(profile_bounds) = profile_bounds(&projected) else {
        return true;
    };
    let owner_faces = &topology.shells[shell_index].value.faces;
    topology
        .faces
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != target_face_index && Some(*index) != exit_face_index)
        .any(|(index, face_record)| {
            let face = &face_record.value;
            let Some(bounds) = face_bounds_in_sweep_frame(
                topology,
                face,
                frame.origin,
                [frame.u, frame.v, direction],
            ) else {
                return true;
            };
            let owner_face = owner_faces.contains(&FaceKey(index));
            let on_start = bounds[2]
                .into_iter()
                .all(|coordinate| coordinate.abs() <= clearance);
            if on_start && owner_face {
                return false;
            }
            if bounds[2][1] <= clearance || bounds[2][0] >= distance - clearance {
                return false;
            }
            if bounds[0][1] < profile_bounds[0][0] - clearance
                || bounds[0][0] > profile_bounds[0][1] + clearance
                || bounds[1][1] < profile_bounds[1][0] - clearance
                || bounds[1][0] > profile_bounds[1][1] + clearance
            {
                return false;
            }
            match face.surface {
                Surface::Plane(plane) => {
                    let Some(normal) = robust_unit(plane.normal) else {
                        return true;
                    };
                    let alignment = normal.dot(direction).abs();
                    if alignment >= 1.0 - angular_tolerance {
                        let depth = (plane.origin - frame.origin).dot(direction);
                        let at_plane =
                            project_regions(extrusion, plane, depth * direction.dot(frame.normal));
                        material_domains_overlap(topology, face, &at_plane, clearance)
                    } else if alignment <= angular_tolerance {
                        longitudinal_face_contacts_profile(
                            topology, face, frame, &projected, clearance,
                        )
                    } else {
                        // A local exact rewrite cannot split a longitudinal or
                        // oblique committed planar boundary.
                        true
                    }
                }
                // A blend band cannot be split or rebuilt by the local
                // prismatic rewrite; any potential contact rejects.
                Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => true,
                Surface::Cylinder(cylinder) => {
                    let Some(axis) = robust_unit(cylinder.axis) else {
                        return true;
                    };
                    if axis.dot(direction).abs() < 1.0 - angular_tolerance {
                        return true;
                    }
                    let center = Point2::new(
                        (cylinder.origin - frame.origin).dot(frame.u),
                        (cylinder.origin - frame.origin).dot(frame.v),
                    );
                    // A partial wall — the half-cylinder end of a slot —
                    // reaches only over its own arc; testing the whole
                    // carrier would find contact where the wall is not.
                    let arc = cylinder_face_arc(topology, face, cylinder, frame);
                    circle_boundary_contacts_profile(
                        center,
                        cylinder.radius,
                        arc,
                        &projected,
                        clearance,
                    )
                }
            }
        })
}

fn longitudinal_face_contacts_profile(
    topology: &Topology,
    face: &Face,
    frame: Frame,
    regions: &[ProjectedRegion],
    clearance: f64,
) -> bool {
    let mut footprints = Vec::new();
    let mut isolated = Vec::new();
    for loop_key in face.loops() {
        let Some(profile_loop) = topology.loop_record(loop_key) else {
            return true;
        };
        for coedge_key in &profile_loop.value.coedges {
            let Some(coedge) = topology.coedge(*coedge_key) else {
                return true;
            };
            let Some(edge) = topology.edge(coedge.value.edge) else {
                return true;
            };
            let [world_start, world_end] = edge.value.endpoints();
            let project = |point: Point3| {
                let relative = point - frame.origin;
                Point2::new(relative.dot(frame.u), relative.dot(frame.v))
            };
            let start = project(world_start);
            let end = project(world_end);
            if (end - start).x.hypot((end - start).y) > clearance {
                footprints.push(Segment::Line { start, end });
            } else {
                isolated.push(start);
            }
        }
    }
    if isolated
        .into_iter()
        .any(|point| profile_material_at(point, regions))
    {
        return true;
    }
    footprints.into_iter().any(|footprint| {
        let start = footprint.start();
        let end = footprint.end();
        let midpoint = Point2::new(0.5 * (start.x + end.x), 0.5 * (start.y + end.y));
        profile_material_at(start, regions)
            || profile_material_at(end, regions)
            || profile_material_at(midpoint, regions)
            || regions.iter().any(|region| {
                region.loops.iter().any(|profile_loop| {
                    profile_loop
                        .segments
                        .iter()
                        .any(|segment| segment_clearance(footprint, *segment) <= clearance)
                })
            })
    })
}

fn material_domains_overlap(
    topology: &Topology,
    face: &Face,
    regions: &[ProjectedRegion],
    clearance: f64,
) -> bool {
    for region in regions {
        let profile_point = region.loops[0].segments[0].start();
        if point_in_face_material(topology, face, profile_point, clearance) {
            return true;
        }
    }
    let Some(face_outer) = topology_loop_segments(topology, face.outer_loop) else {
        return true;
    };
    if profile_material_at(face_outer[0].start(), regions) {
        return true;
    }
    let Some(face_boundaries) = face
        .loops()
        .map(|loop_key| topology_loop_segments(topology, loop_key))
        .collect::<Option<Vec<_>>>()
    else {
        return true;
    };
    regions.iter().any(|region| {
        region.loops.iter().any(|profile_loop| {
            profile_loop.segments.iter().any(|profile_segment| {
                face_boundaries.iter().flatten().any(|face_segment| {
                    segment_clearance(*profile_segment, *face_segment) <= clearance
                })
            })
        })
    })
}

/// The arc a cylindrical face covers, in the sweep frame's plane: its
/// angular extent from its own parameters, or `None` when the face wraps a
/// full turn or its extent cannot be read.
fn cylinder_face_arc(
    topology: &Topology,
    face: &crate::topology::Face,
    cylinder: crate::topology::Cylinder,
    frame: crate::analytic_extrusion::Frame,
) -> Option<Segment> {
    let (u_min, u_max, _, _) = crate::validator::pcurve_extent(topology, face)?;
    let turn = u_max - u_min;
    if !turn.is_finite() || turn <= 0.0 || turn >= std::f64::consts::TAU - 1.0e-9 {
        return None;
    }
    let project = |u: f64| {
        let point = cylinder.evaluate(Point2::new(u, 0.0));
        Point2::new(
            (point - frame.origin).dot(frame.u),
            (point - frame.origin).dot(frame.v),
        )
    };
    let center = Point2::new(
        (cylinder.origin - frame.origin).dot(frame.u),
        (cylinder.origin - frame.origin).dot(frame.v),
    );
    let start = project(u_min);
    let end = project(u_max);
    let middle = project(0.5 * (u_min + u_max));
    let angle = |point: Point2| (point.y - center.y).atan2(point.x - center.x);
    let start_angle = angle(start);
    let mut sweep = (angle(end) - start_angle).rem_euclid(std::f64::consts::TAU);
    // The middle sample says which way round the arc runs.
    let to_middle = (angle(middle) - start_angle).rem_euclid(std::f64::consts::TAU);
    if to_middle > sweep {
        sweep -= std::f64::consts::TAU;
    }
    Some(Segment::Arc {
        center,
        start,
        end,
        radius: cylinder.radius,
        start_angle,
        sweep,
    })
}

fn circle_boundary_contacts_profile(
    center: Point2,
    radius: f64,
    arc: Option<Segment>,
    regions: &[ProjectedRegion],
    clearance: f64,
) -> bool {
    if let Some(arc) = arc {
        let touches = regions.iter().any(|region| {
            region.loops.iter().any(|profile_loop| {
                profile_loop
                    .segments
                    .iter()
                    .any(|segment| segment_clearance(*segment, arc) <= clearance)
            })
        });
        if touches {
            return true;
        }
        // The wall stands inside the pocket's material: the sweep would
        // cut through it.
        let Segment::Arc {
            start_angle, sweep, ..
        } = arc
        else {
            return true;
        };
        let middle = start_angle + 0.5 * sweep;
        return profile_material_at(
            Point2::new(
                center.x + radius * middle.cos(),
                center.y + radius * middle.sin(),
            ),
            regions,
        );
    }
    let circle = Segment::Arc {
        center,
        start: Point2::new(center.x + radius, center.y),
        end: Point2::new(center.x - radius, center.y),
        radius,
        start_angle: 0.0,
        sweep: std::f64::consts::PI,
    };
    let other_half = Segment::Arc {
        center,
        start: Point2::new(center.x - radius, center.y),
        end: Point2::new(center.x + radius, center.y),
        radius,
        start_angle: std::f64::consts::PI,
        sweep: std::f64::consts::PI,
    };
    if regions.iter().any(|region| {
        region.loops.iter().any(|profile_loop| {
            profile_loop.segments.iter().any(|segment| {
                segment_clearance(*segment, circle) <= clearance
                    || segment_clearance(*segment, other_half) <= clearance
            })
        })
    }) {
        return true;
    }
    profile_material_at(Point2::new(center.x + radius, center.y), regions)
}

fn profile_bounds(regions: &[ProjectedRegion]) -> Option<[[f64; 2]; 2]> {
    let mut bounds = [[f64::INFINITY, f64::NEG_INFINITY]; 2];
    let mut included = false;
    for segment in regions
        .iter()
        .flat_map(|region| &region.loops)
        .flat_map(|profile_loop| &profile_loop.segments)
    {
        for point in segment_extrema(*segment) {
            bounds[0][0] = bounds[0][0].min(point.x);
            bounds[0][1] = bounds[0][1].max(point.x);
            bounds[1][0] = bounds[1][0].min(point.y);
            bounds[1][1] = bounds[1][1].max(point.y);
            included = true;
        }
    }
    included.then_some(bounds)
}

fn segment_extrema(segment: Segment) -> Vec<Point2> {
    let mut points = vec![segment.start(), segment.end()];
    if let Segment::Arc {
        center,
        radius,
        start_angle,
        sweep,
        ..
    } = segment
    {
        for angle in [
            0.0,
            0.5 * std::f64::consts::PI,
            std::f64::consts::PI,
            1.5 * std::f64::consts::PI,
        ] {
            if angle_on_arc(angle, start_angle, sweep) {
                points.push(Point2::new(
                    radius.mul_add(angle.cos(), center.x),
                    radius.mul_add(angle.sin(), center.y),
                ));
            }
        }
    }
    points
}

fn angle_on_arc(angle: f64, start: f64, sweep: f64) -> bool {
    let progress = if sweep >= 0.0 {
        (angle - start).rem_euclid(std::f64::consts::TAU)
    } else {
        (start - angle).rem_euclid(std::f64::consts::TAU)
    };
    progress <= sweep.abs() + 64.0 * f64::EPSILON
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
        let profile_loop = topology.loop_record(loop_key)?;
        for coedge_key in &profile_loop.value.coedges {
            let edge = &topology
                .edge(topology.coedge(*coedge_key)?.value.edge)?
                .value;
            for point in edge.endpoints() {
                if !include(point) {
                    return None;
                }
            }
            if let Curve3::Circle {
                center,
                u,
                v,
                radius,
            } = edge.curve
            {
                for axis in basis {
                    let angle = v.dot(axis).atan2(u.dot(axis));
                    for extremum in [angle, angle + std::f64::consts::PI] {
                        if parameter_on_range(extremum, edge.parameter_range)
                            && !include(
                                Curve3::Circle {
                                    center,
                                    u,
                                    v,
                                    radius,
                                }
                                .evaluate(extremum),
                            )
                        {
                            return None;
                        }
                    }
                }
            }
        }
    }
    included.then_some(bounds)
}

fn parameter_on_range(parameter: f64, range: ParameterRange) -> bool {
    let sweep = range.end - range.start;
    let directed = if sweep >= 0.0 {
        (parameter - range.start).rem_euclid(std::f64::consts::TAU)
    } else {
        (range.start - parameter).rem_euclid(std::f64::consts::TAU)
    };
    directed <= sweep.abs() + 64.0 * f64::EPSILON
}

#[allow(clippy::too_many_arguments)]
fn append_exact_feature(
    topology: &mut Topology,
    shell_index: usize,
    target_face_index: usize,
    exit_face_index: Option<usize>,
    target_plane: Plane,
    extrusion: &ValidatedAnalyticExtrusion,
    direction: Vector3,
    distance: f64,
) {
    let mut next_id = next_entity_id(topology);
    let frame = extrusion.regions[0].frame;
    let mut keys = Vec::with_capacity(extrusion.regions.len());
    for region in &extrusion.regions {
        let mut region_keys = Vec::with_capacity(region.loops.len());
        for profile_loop in &region.loops {
            region_keys.push(build_loop_edges(
                topology,
                &mut next_id,
                frame,
                profile_loop,
                direction,
                distance,
            ));
        }
        keys.push(region_keys);
    }

    let parents = region_parent_holes(extrusion);
    let target_role = topology.faces[target_face_index].value.role;
    append_complement_faces(
        topology,
        &mut next_id,
        shell_index,
        target_face_index,
        target_plane,
        extrusion,
        &keys,
        &parents,
        true,
        0.0,
        target_role,
    );

    if let Some(exit_index) = exit_face_index {
        let exit_plane = topology.faces[exit_index]
            .value
            .surface
            .as_plane()
            .expect("validated exact exit is planar");
        let exit_role = topology.faces[exit_index].value.role;
        append_complement_faces(
            topology,
            &mut next_id,
            shell_index,
            exit_index,
            exit_plane,
            extrusion,
            &keys,
            &parents,
            false,
            direction.dot(frame.normal) * distance,
            exit_role,
        );
    } else {
        let end_plane = Plane::new(frame.origin + direction * distance, frame.u, frame.v);
        for (region_index, region) in extrusion.regions.iter().enumerate() {
            let outer_loop = push_cap_loop(
                topology,
                &mut next_id,
                &region.loops[0],
                &keys[region_index][0].end_edges,
                frame,
                end_plane,
                distance,
                direction,
                false,
            );
            let inner_loops = region.loops[1..]
                .iter()
                .enumerate()
                .map(|(hole_index, profile_loop)| {
                    push_cap_loop(
                        topology,
                        &mut next_id,
                        profile_loop,
                        &keys[region_index][hole_index + 1].end_edges,
                        frame,
                        end_plane,
                        distance,
                        direction,
                        false,
                    )
                })
                .collect();
            push_face(
                topology,
                &mut next_id,
                shell_index,
                Face {
                    surface: Surface::Plane(end_plane),
                    outer_loop,
                    inner_loops,
                    role: FaceRole::FeatureEnd,
                },
            );
        }
    }

    let direction_sign = direction.dot(frame.normal).signum();
    // Leave a one-value boundary between separate feature operations. A
    // single analytic carrier may own several exact face patches (for
    // example, the two semicircles used to represent a full cylinder), while
    // consecutive profile carriers within this operation remain sequential.
    // The gap lets downstream reconstruction distinguish stacked operations
    // even when both happen to contain only one circular carrier.
    let mut side_ordinal = topology
        .faces
        .iter()
        .filter_map(|face| match face.value.role {
            FaceRole::FeatureSide(ordinal) => Some(ordinal),
            _ => None,
        })
        .max()
        .map_or(0, |ordinal| ordinal.saturating_add(2));
    for (region_index, region) in extrusion.regions.iter().enumerate() {
        for (loop_index, profile_loop) in region.loops.iter().enumerate() {
            let loop_keys = &keys[region_index][loop_index];
            let count = profile_loop.segments.len();
            let mut previous = None::<Segment>;
            for (segment_index, segment) in profile_loop.segments.iter().copied().enumerate() {
                if previous.is_some_and(|previous| !previous.shares_side_carrier(segment)) {
                    side_ordinal = side_ordinal.saturating_add(1);
                }
                let next = (segment_index + 1) % count;
                push_feature_side(
                    topology,
                    &mut next_id,
                    shell_index,
                    frame,
                    segment,
                    direction,
                    direction_sign,
                    distance,
                    [
                        loop_keys.start_edges[segment_index],
                        loop_keys.sweep_edges[next],
                        loop_keys.end_edges[segment_index],
                        loop_keys.sweep_edges[segment_index],
                    ],
                    FaceRole::FeatureSide(side_ordinal),
                );
                previous = Some(segment);
            }
            side_ordinal = side_ordinal.saturating_add(1);
        }
    }
    split_shell_components(topology, shell_index, &mut next_id);
}

fn split_shell_components(topology: &mut Topology, shell_index: usize, next_id: &mut u64) {
    let shell_faces = topology.shells[shell_index].value.faces.clone();
    if shell_faces.len() <= 1 {
        return;
    }
    let mut edge_faces = vec![Vec::<FaceKey>::new(); topology.edges.len()];
    for face_key in &shell_faces {
        for loop_key in topology.faces[face_key.0].value.loops() {
            for coedge_key in &topology.loops[loop_key.0].value.coedges {
                edge_faces[topology.coedges[coedge_key.0].value.edge.0].push(*face_key);
            }
        }
    }
    let mut in_shell = vec![false; topology.faces.len()];
    for face in &shell_faces {
        in_shell[face.0] = true;
    }
    let mut visited = vec![false; topology.faces.len()];
    let mut components = Vec::<Vec<FaceKey>>::new();
    for seed in shell_faces {
        if visited[seed.0] {
            continue;
        }
        visited[seed.0] = true;
        let mut queue = VecDeque::from([seed]);
        let mut component = Vec::new();
        while let Some(face_key) = queue.pop_front() {
            component.push(face_key);
            for loop_key in topology.faces[face_key.0].value.loops() {
                for coedge_key in &topology.loops[loop_key.0].value.coedges {
                    for adjacent in &edge_faces[topology.coedges[coedge_key.0].value.edge.0] {
                        if in_shell[adjacent.0] && !visited[adjacent.0] {
                            visited[adjacent.0] = true;
                            queue.push_back(*adjacent);
                        }
                    }
                }
            }
        }
        component.sort_by_key(|face| face.0);
        components.push(component);
    }
    if components.len() <= 1 {
        return;
    }
    components.sort_by_key(|component| component[0].0);
    topology.shells[shell_index].value.faces = components.remove(0);
    for faces in components {
        let new_shell = crate::topology::ShellKey(topology.shells.len());
        topology.shells.push(Record {
            id: allocate_id(next_id),
            value: crate::topology::Shell { faces },
        });
        topology.solids.push(Record {
            id: allocate_id(next_id),
            value: crate::topology::Solid {
                outer_shell: new_shell,
                inner_shells: Vec::new(),
            },
        });
    }
}

fn build_loop_edges(
    topology: &mut Topology,
    next_id: &mut u64,
    frame: Frame,
    profile_loop: &AnalyticLoop,
    direction: Vector3,
    distance: f64,
) -> FeatureLoopKeys {
    let start_vertices = profile_loop
        .segments
        .iter()
        .map(|segment| push_vertex(topology, next_id, frame.point(segment.start(), 0.0)))
        .collect::<Vec<_>>();
    let end_vertices = profile_loop
        .segments
        .iter()
        .map(|segment| {
            push_vertex(
                topology,
                next_id,
                frame.point(segment.start(), 0.0) + direction * distance,
            )
        })
        .collect::<Vec<_>>();
    let count = profile_loop.segments.len();
    let start_edges = profile_loop
        .segments
        .iter()
        .copied()
        .enumerate()
        .map(|(index, segment)| {
            push_boundary_edge(
                topology,
                next_id,
                [start_vertices[index], start_vertices[(index + 1) % count]],
                segment,
                frame,
                direction,
                0.0,
            )
        })
        .collect();
    let end_edges = profile_loop
        .segments
        .iter()
        .copied()
        .enumerate()
        .map(|(index, segment)| {
            push_boundary_edge(
                topology,
                next_id,
                [end_vertices[index], end_vertices[(index + 1) % count]],
                segment,
                frame,
                direction,
                distance,
            )
        })
        .collect();
    let sweep_edges = (0..count)
        .map(|index| {
            let endpoints = [
                topology.vertices[start_vertices[index].0].value.point,
                topology.vertices[end_vertices[index].0].value.point,
            ];
            push_edge(
                topology,
                next_id,
                Edge::line([start_vertices[index], end_vertices[index]], endpoints),
            )
        })
        .collect();
    FeatureLoopKeys {
        start_edges,
        end_edges,
        sweep_edges,
    }
}

fn region_parent_holes(extrusion: &ValidatedAnalyticExtrusion) -> Vec<Option<(usize, usize)>> {
    extrusion
        .regions
        .iter()
        .enumerate()
        .map(|(region_index, region)| {
            let point = region.loops[0].segments[0].start();
            extrusion
                .regions
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != region_index)
                .flat_map(|(other, candidate)| {
                    candidate.loops[1..]
                        .iter()
                        .enumerate()
                        .filter(move |(_, hole)| point_inside_loop(point, hole))
                        .map(move |(hole, profile_loop)| {
                            (profile_loop.signed_area.abs(), other, hole + 1)
                        })
                })
                .min_by(|left, right| left.0.total_cmp(&right.0))
                .map(|(_, parent_region, parent_loop)| (parent_region, parent_loop))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn append_complement_faces(
    topology: &mut Topology,
    next_id: &mut u64,
    shell_index: usize,
    root_face_index: usize,
    plane: Plane,
    extrusion: &ValidatedAnalyticExtrusion,
    keys: &[Vec<FeatureLoopKeys>],
    parents: &[Option<(usize, usize)>],
    reverse: bool,
    projection_height: f64,
    role: FaceRole,
) {
    let projected = project_regions(extrusion, plane, projection_height);
    let existing_inner = std::mem::take(&mut topology.faces[root_face_index].value.inner_loops);
    let mut inherited = extrusion
        .regions
        .iter()
        .map(|region| vec![Vec::<LoopKey>::new(); region.loops.len()])
        .collect::<Vec<_>>();
    for loop_key in existing_inner {
        let owner = topology_loop_segments(topology, loop_key)
            .and_then(|segments| segments.first().copied())
            .map(Segment::start)
            .and_then(|point| {
                projected
                    .iter()
                    .enumerate()
                    .flat_map(|(region_index, region)| {
                        region.loops[1..]
                            .iter()
                            .enumerate()
                            .filter(move |(_, hole)| point_inside_loop(point, hole))
                            .map(move |(hole_index, hole)| {
                                (hole.signed_area.abs(), region_index, hole_index + 1)
                            })
                    })
                    .min_by(|left, right| left.0.total_cmp(&right.0))
                    .map(|(_, region_index, hole_index)| (region_index, hole_index))
            });
        if let Some((region_index, hole_index)) = owner {
            inherited[region_index][hole_index].push(loop_key);
        } else {
            topology.faces[root_face_index]
                .value
                .inner_loops
                .push(loop_key);
        }
    }
    for (region_index, region) in extrusion.regions.iter().enumerate() {
        if parents[region_index].is_none() {
            let loop_key = push_cap_loop(
                topology,
                next_id,
                &region.loops[0],
                if reverse {
                    &keys[region_index][0].start_edges
                } else {
                    &keys[region_index][0].end_edges
                },
                region.frame,
                plane,
                if reverse {
                    0.0
                } else {
                    keys_height(&keys[region_index][0], topology, region.frame, plane)
                },
                if reverse {
                    region.frame.normal
                } else {
                    region.frame.normal * -1.0
                },
                reverse,
            );
            topology.faces[root_face_index]
                .value
                .inner_loops
                .push(loop_key);
        }
    }
    for (region_index, region) in extrusion.regions.iter().enumerate() {
        for hole_index in 1..region.loops.len() {
            let edges = if reverse {
                &keys[region_index][hole_index].start_edges
            } else {
                &keys[region_index][hole_index].end_edges
            };
            let outer_loop = push_cap_loop(
                topology,
                next_id,
                &region.loops[hole_index],
                edges,
                region.frame,
                plane,
                0.0,
                region.frame.normal,
                reverse,
            );
            let mut inner_loops = std::mem::take(&mut inherited[region_index][hole_index]);
            inner_loops.extend(
                parents
                    .iter()
                    .enumerate()
                    .filter(|(_, parent)| **parent == Some((region_index, hole_index)))
                    .map(|(child, _)| {
                        let child_edges = if reverse {
                            &keys[child][0].start_edges
                        } else {
                            &keys[child][0].end_edges
                        };
                        push_cap_loop(
                            topology,
                            next_id,
                            &extrusion.regions[child].loops[0],
                            child_edges,
                            extrusion.regions[child].frame,
                            plane,
                            0.0,
                            extrusion.regions[child].frame.normal,
                            reverse,
                        )
                    }),
            );
            push_face(
                topology,
                next_id,
                shell_index,
                Face {
                    surface: Surface::Plane(plane),
                    outer_loop,
                    inner_loops,
                    role,
                },
            );
        }
    }
}

// The end edges already carry their world location. Cap pcurves are projected
// from those edges, so this compatibility helper intentionally returns zero.
fn keys_height(_keys: &FeatureLoopKeys, _topology: &Topology, _frame: Frame, _plane: Plane) -> f64 {
    0.0
}

#[allow(clippy::too_many_arguments)]
fn push_cap_loop(
    topology: &mut Topology,
    next_id: &mut u64,
    profile_loop: &AnalyticLoop,
    edges: &[EdgeKey],
    frame: Frame,
    plane: Plane,
    height: f64,
    direction: Vector3,
    reverse: bool,
) -> LoopKey {
    let count = profile_loop.segments.len();
    let order: Vec<usize> = if reverse {
        (0..count).rev().collect()
    } else {
        (0..count).collect()
    };
    let uses = order
        .into_iter()
        .map(|index| {
            let edge = edges[index];
            let edge_geometry = topology.edges[edge.0].value;
            let segment = profile_loop.segments[index];
            let (pcurve, range) = cap_pcurve_from_edge(edge_geometry, plane, reverse);
            let _ = (frame, height, direction, segment);
            BoundaryUse {
                edge,
                orientation: if reverse {
                    Orientation::Reverse
                } else {
                    Orientation::Forward
                },
                pcurve,
                range,
            }
        })
        .collect();
    push_loop(topology, next_id, uses)
}

fn cap_pcurve_from_edge(edge: Edge, plane: Plane, reverse: bool) -> (Curve2, ParameterRange) {
    match edge.curve {
        Curve3::Line { endpoints } => Curve2::line_segment(if reverse {
            [plane.project(endpoints[1]), plane.project(endpoints[0])]
        } else {
            endpoints.map(|point| plane.project(point))
        }),
        Curve3::Circle {
            center,
            u,
            v,
            radius,
        } => (
            Curve2::Circle {
                center: plane.project(center),
                u: Vector2::new(u.dot(plane.u), u.dot(plane.v)),
                v: Vector2::new(v.dot(plane.u), v.dot(plane.v)),
                radius,
            },
            if reverse {
                edge.parameter_range.reversed()
            } else {
                edge.parameter_range
            },
        ),
        // An ellipse never bounds a planar cap in this vocabulary: it is the
        // seam of two cylinders. Should one arrive here, the chord keeps the
        // loop closed and the validator's locus check names the mismatch.
        Curve3::Ellipse { .. } => {
            let endpoints = edge.endpoints();
            Curve2::line_segment(if reverse {
                [plane.project(endpoints[1]), plane.project(endpoints[0])]
            } else {
                endpoints.map(|point| plane.project(point))
            })
        }
    }
}

fn push_boundary_edge(
    topology: &mut Topology,
    next_id: &mut u64,
    vertices: [VertexKey; 2],
    segment: Segment,
    frame: Frame,
    direction: Vector3,
    height: f64,
) -> EdgeKey {
    let shift = direction * height;
    let edge = match segment {
        Segment::Line { start, end } => Edge::line(
            vertices,
            [
                frame.point(start, 0.0) + shift,
                frame.point(end, 0.0) + shift,
            ],
        ),
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => Edge {
            vertices,
            curve: Curve3::Circle {
                center: frame.center(center, 0.0) + shift,
                u: frame.u,
                v: frame.v,
                radius,
            },
            parameter_range: ParameterRange::new(start_angle, start_angle + sweep),
        },
        Segment::Ellipse { .. } | Segment::Harmonic { .. } => {
            unreachable!("planar profiles carry lines and arcs only")
        }
    };
    push_edge(topology, next_id, edge)
}

#[allow(clippy::too_many_arguments)]
fn push_feature_side(
    topology: &mut Topology,
    next_id: &mut u64,
    shell_index: usize,
    frame: Frame,
    segment: Segment,
    direction: Vector3,
    direction_sign: f64,
    distance: f64,
    edges: [EdgeKey; 4],
    role: FaceRole,
) {
    let (surface, start, right, end, left) = match segment {
        Segment::Line { start, end } => {
            let length = (end - start).x.hypot((end - start).y);
            let tangent =
                frame.u * ((end.x - start.x) / length) + frame.v * ((end.y - start.y) / length);
            (
                Surface::Plane(Plane::new(frame.point(start, 0.0), tangent, direction)),
                Curve2::line_segment([Point2::new(0.0, 0.0), Point2::new(length, 0.0)]),
                Curve2::line_segment([Point2::new(length, 0.0), Point2::new(length, distance)]),
                Curve2::line_segment([Point2::new(length, distance), Point2::new(0.0, distance)]),
                Curve2::line_segment([Point2::new(0.0, distance), Point2::new(0.0, 0.0)]),
            )
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let winding = sweep.signum();
            let start = winding * start_angle;
            let end = winding * (start_angle + sweep);
            (
                Surface::Cylinder(Cylinder {
                    origin: frame.center(center, 0.0),
                    axis: direction,
                    radial_u: frame.u,
                    radial_v: frame.v * direction_sign,
                    radius,
                    angular_sign: winding * direction_sign,
                }),
                Curve2::line_segment([Point2::new(start, 0.0), Point2::new(end, 0.0)]),
                Curve2::line_segment([Point2::new(end, 0.0), Point2::new(end, distance)]),
                Curve2::line_segment([Point2::new(end, distance), Point2::new(start, distance)]),
                Curve2::line_segment([Point2::new(start, distance), Point2::new(start, 0.0)]),
            )
        }
        Segment::Ellipse { .. } | Segment::Harmonic { .. } => {
            unreachable!("planar profiles carry lines and arcs only")
        }
    };
    let loop_key = push_loop(
        topology,
        next_id,
        [start, right, end, left]
            .into_iter()
            .zip([
                (edges[0], Orientation::Forward),
                (edges[1], Orientation::Forward),
                (edges[2], Orientation::Reverse),
                (edges[3], Orientation::Reverse),
            ])
            .map(|((pcurve, range), (edge, orientation))| BoundaryUse {
                edge,
                orientation,
                pcurve,
                range,
            })
            .collect(),
    );
    push_face(
        topology,
        next_id,
        shell_index,
        Face {
            surface,
            outer_loop: loop_key,
            inner_loops: Vec::new(),
            role,
        },
    );
}

fn push_vertex(topology: &mut Topology, next_id: &mut u64, point: Point3) -> VertexKey {
    let key = VertexKey(topology.vertices.len());
    topology.vertices.push(Record {
        id: allocate_id(next_id),
        value: Vertex { point },
    });
    key
}

fn push_edge(topology: &mut Topology, next_id: &mut u64, edge: Edge) -> EdgeKey {
    let key = EdgeKey(topology.edges.len());
    topology.edges.push(Record {
        id: allocate_id(next_id),
        value: edge,
    });
    key
}

fn push_loop(topology: &mut Topology, next_id: &mut u64, uses: Vec<BoundaryUse>) -> LoopKey {
    let coedges = uses
        .into_iter()
        .map(|boundary| {
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
            key
        })
        .collect();
    let key = LoopKey(topology.loops.len());
    topology.loops.push(Record {
        id: allocate_id(next_id),
        value: Loop { coedges },
    });
    key
}

fn push_face(
    topology: &mut Topology,
    next_id: &mut u64,
    shell_index: usize,
    face: Face,
) -> FaceKey {
    let key = FaceKey(topology.faces.len());
    topology.faces.push(Record {
        id: allocate_id(next_id),
        value: face,
    });
    topology.shells[shell_index].value.faces.push(key);
    key
}

fn next_entity_id(topology: &Topology) -> u64 {
    topology
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
        + 1
}

fn allocate_id(next_id: &mut u64) -> EntityId {
    let id = EntityId::from_raw(*next_id);
    *next_id += 1;
    id
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{
        ArcDirection, EntityId as ProtocolEntityId, PlanarCurve2, PlanarLoop2, PlanarRegion2,
        Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, RotationQuaternion,
        SimilarityTransform3, Vector3 as ProtocolVector3,
    };

    use super::*;
    use crate::cuboid::build_cuboid;
    use crate::transform::{Similarity, transform_topology};
    use crate::validator;

    fn rectangle(min: (f64, f64), max: (f64, f64)) -> PlanarLoop2 {
        PlanarLoop2::from_polygon(&[
            ProtocolPoint2::new(min.0, min.1),
            ProtocolPoint2::new(max.0, min.1),
            ProtocolPoint2::new(max.0, max.1),
            ProtocolPoint2::new(min.0, max.1),
        ])
    }

    fn circle(x: f64, y: f64, radius: f64, direction: ArcDirection) -> PlanarLoop2 {
        PlanarLoop2 {
            curves: vec![PlanarCurve2::Circle {
                center: ProtocolPoint2::new(x, y),
                radius,
                direction,
            }],
        }
    }

    fn mixed_half_disk() -> PlanarProfile2 {
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![
                        PlanarCurve2::Line {
                            start: ProtocolPoint2::new(2.0, 3.0),
                            end: ProtocolPoint2::new(4.0, 3.0),
                        },
                        PlanarCurve2::CircularArc {
                            center: ProtocolPoint2::new(3.0, 3.0),
                            start: ProtocolPoint2::new(4.0, 3.0),
                            end: ProtocolPoint2::new(2.0, 3.0),
                            direction: ArcDirection::CounterClockwise,
                        },
                    ],
                },
                holes: Vec::new(),
            }],
        }
    }

    fn fixture(rotated: bool) -> (SnapshotId, Topology, EntityRef, PlanarFrame3) {
        let snapshot = SnapshotId::new([91; 16]);
        let mut topology = build_cuboid(Point3::new(0.0, 0.0, 0.0), Vector3::new(6.0, 6.0, 4.0));
        if rotated {
            let half_angle = 0.31_f64;
            let similarity = Similarity::from_protocol(SimilarityTransform3 {
                translation: ProtocolVector3::new(7.0, -3.0, 2.0),
                rotation: RotationQuaternion::new(half_angle.cos(), half_angle.sin(), 0.0, 0.0),
                uniform_scale: 1.0,
            })
            .unwrap();
            topology = transform_topology(&topology, similarity);
        }
        let target_index = topology
            .faces
            .iter()
            .position(|face| face.value.role == FaceRole::PositiveZ)
            .unwrap();
        let plane = topology.faces[target_index]
            .value
            .surface
            .as_plane()
            .unwrap();
        let target = EntityRef {
            snapshot,
            entity: ProtocolEntityId(topology.faces[target_index].id.get()),
            kind: EntityKind::Face,
        };
        let frame = PlanarFrame3 {
            origin: ProtocolPoint3::new(plane.origin.x, plane.origin.y, plane.origin.z),
            u: ProtocolVector3::new(plane.u.x, plane.u.y, plane.u.z),
            v: ProtocolVector3::new(plane.v.x, plane.v.y, plane.v.z),
        };
        (snapshot, topology, target, frame)
    }

    fn execute(
        profile: &PlanarProfile2,
        operation: FaceExtrusionOperation,
        distance: f64,
        rotated: bool,
    ) -> ValidatedExactFaceFeature {
        let (snapshot, topology, target, frame) = fixture(rotated);
        validate_exact_face_feature(
            snapshot,
            &topology,
            target,
            frame,
            profile,
            distance,
            operation,
            PrecisionPolicy::default(),
        )
        .unwrap()
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1.0e-8 * expected.abs().max(1.0),
            "expected {expected:.17e}, got {actual:.17e}"
        );
    }

    #[test]
    fn mixed_line_arc_profile_adds_and_cuts_on_axis_aligned_and_rotated_supports() {
        let profile = mixed_half_disk();
        let area = 0.5 * std::f64::consts::PI;
        for rotated in [false, true] {
            for (operation, request_distance, effective_distance) in [
                (FaceExtrusionOperation::Add, 1.25, 1.25),
                (FaceExtrusionOperation::Cut, 1.25, 1.25),
                (FaceExtrusionOperation::Cut, 10.0, 4.0),
            ] {
                let feature = execute(&profile, operation, request_distance, rotated);
                let report = validator::validate(&feature.topology, 1.0e-9);
                assert!(
                    report.is_valid(),
                    "rotated={rotated} {operation:?}: {:#?}",
                    report.diagnostics
                );
                let sign = if operation == FaceExtrusionOperation::Add {
                    1.0
                } else {
                    -1.0
                };
                assert_close(
                    report.measures.signed_volume,
                    144.0 + sign * area * effective_distance,
                );
                assert!(
                    feature
                        .topology
                        .edges
                        .iter()
                        .any(|edge| { matches!(edge.value.curve, Curve3::Circle { .. }) })
                );
                assert!(
                    feature
                        .topology
                        .faces
                        .iter()
                        .any(|face| { matches!(face.value.surface, Surface::Cylinder(_)) })
                );
                assert_eq!(
                    feature.exit_face_index.is_some(),
                    operation == FaceExtrusionOperation::Cut && request_distance > 4.0
                );
            }
        }
    }

    #[test]
    fn analytic_holes_multiregions_and_parity_islands_remain_exact() {
        let annulus = PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: circle(3.0, 3.0, 1.5, ArcDirection::CounterClockwise),
                holes: vec![circle(3.0, 3.0, 0.5, ArcDirection::Clockwise)],
            }],
        };
        let multiregion = PlanarProfile2 {
            regions: vec![
                PlanarRegion2 {
                    outer: circle(1.5, 3.0, 0.5, ArcDirection::CounterClockwise),
                    holes: Vec::new(),
                },
                PlanarRegion2 {
                    outer: circle(4.5, 3.0, 0.5, ArcDirection::CounterClockwise),
                    holes: Vec::new(),
                },
            ],
        };
        let parity = PlanarProfile2 {
            regions: vec![
                PlanarRegion2 {
                    outer: circle(3.0, 3.0, 1.5, ArcDirection::CounterClockwise),
                    holes: vec![circle(3.0, 3.0, 0.9, ArcDirection::Clockwise)],
                },
                PlanarRegion2 {
                    outer: circle(3.0, 3.0, 0.4, ArcDirection::CounterClockwise),
                    holes: Vec::new(),
                },
            ],
        };
        for (profile, area, through_solids) in [
            (annulus, 2.0 * std::f64::consts::PI, 2),
            (multiregion, 0.5 * std::f64::consts::PI, 1),
            (parity, 1.6 * std::f64::consts::PI, 2),
        ] {
            for (operation, request_distance, effective_distance) in [
                (FaceExtrusionOperation::Add, 1.0, 1.0),
                (FaceExtrusionOperation::Cut, 1.0, 1.0),
                (FaceExtrusionOperation::Cut, 10.0, 4.0),
            ] {
                let feature = execute(&profile, operation, request_distance, false);
                let report = validator::validate(&feature.topology, 1.0e-9);
                assert!(
                    report.is_valid(),
                    "{operation:?}: {:#?}",
                    report.diagnostics
                );
                let sign = if operation == FaceExtrusionOperation::Add {
                    1.0
                } else {
                    -1.0
                };
                assert_close(
                    report.measures.signed_volume,
                    144.0 + sign * area * effective_distance,
                );
                assert_eq!(
                    feature.topology.solids.len(),
                    if operation == FaceExtrusionOperation::Cut && request_distance > 4.0 {
                        through_solids
                    } else {
                        1
                    }
                );
            }
        }
    }

    #[test]
    fn circular_contact_preflight_accepts_roundoff_but_rejects_real_intersection() {
        let disk = |center: Point2, radius: f64| {
            vec![ProjectedRegion {
                loops: vec![AnalyticLoop {
                    segments: vec![
                        Segment::Arc {
                            center,
                            start: Point2::new(center.x + radius, center.y),
                            end: Point2::new(center.x - radius, center.y),
                            radius,
                            start_angle: 0.0,
                            sweep: std::f64::consts::PI,
                        },
                        Segment::Arc {
                            center,
                            start: Point2::new(center.x - radius, center.y),
                            end: Point2::new(center.x + radius, center.y),
                            radius,
                            start_angle: std::f64::consts::PI,
                            sweep: std::f64::consts::PI,
                        },
                    ],
                    signed_area: std::f64::consts::PI * radius * radius,
                }],
            }]
        };
        let clearance = PrecisionPolicy::default()
            .modeling_resolution
            .max(PrecisionPolicy::default().min_feature_size);

        assert!(!circle_boundary_contacts_profile(
            Point2::new(0.0, 0.0),
            1.0,
            None,
            &disk(Point2::new(0.0, -1.7763568394002505e-15), 0.5),
            clearance,
        ));
        assert!(circle_boundary_contacts_profile(
            Point2::new(0.0, 0.0),
            1.0,
            None,
            &disk(Point2::new(0.6, 0.0), 0.5),
            clearance,
        ));
        // A half wall reaches only over its own arc: a profile inside the
        // carrier but clear of the wall is no contact.
        let right_half = Segment::Arc {
            center: Point2::new(0.0, 0.0),
            start: Point2::new(0.0, -1.0),
            end: Point2::new(0.0, 1.0),
            radius: 1.0,
            start_angle: -std::f64::consts::FRAC_PI_2,
            sweep: std::f64::consts::PI,
        };
        assert!(!circle_boundary_contacts_profile(
            Point2::new(0.0, 0.0),
            1.0,
            Some(right_half),
            &disk(Point2::new(-0.6, 0.0), 0.3),
            clearance,
        ));
        assert!(circle_boundary_contacts_profile(
            Point2::new(0.0, 0.0),
            1.0,
            Some(right_half),
            &disk(Point2::new(0.6, 0.0), 0.5),
            clearance,
        ));
    }

    #[test]
    fn tangential_support_contact_rejects_without_mutating_the_source() {
        let (snapshot, topology, target, frame) = fixture(false);
        let source_counts = crate::topology::TopologyCounts::from(&topology);
        let source_ids = topology
            .faces
            .iter()
            .map(|face| face.id)
            .collect::<Vec<_>>();
        let error = validate_exact_face_feature(
            snapshot,
            &topology,
            target,
            frame,
            &PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: rectangle((0.0, 1.0), (2.0, 3.0)),
                    holes: Vec::new(),
                }],
            },
            1.0,
            FaceExtrusionOperation::Add,
            PrecisionPolicy::default(),
        )
        .unwrap_err();
        assert_eq!(error, face_error(FaceFeatureInputError::ProfileOutsideFace));
        assert_eq!(
            crate::topology::TopologyCounts::from(&topology),
            source_counts
        );
        assert_eq!(
            topology
                .faces
                .iter()
                .map(|face| face.id)
                .collect::<Vec<_>>(),
            source_ids
        );
    }
}

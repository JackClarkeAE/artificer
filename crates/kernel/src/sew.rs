//! Sews independently built face pieces into closed shells and solids.
//!
//! The general Boolean's regularize stage leaves a bag of kept face pieces —
//! each one a surface carrier plus its 2D boundary loops — whose shared
//! boundaries were computed by *different* faces' 2D Booleans and therefore
//! agree only to rounding, not bit for bit. Sewing rebuilds the shared
//! topology: vertices weld by position, edges weld by their endpoints and
//! midpoint, coedges carry each face's own pcurves, and edge-connected
//! components become shells. A component with negative enclosed volume is a
//! cavity and attaches to the positive component that contains it as an
//! inner shell.
//!
//! Nothing sewn here is trusted by construction: the caller publishes the
//! result through the validator, whose edge-use, locus, orientation, and
//! Euler families check every weld this pass makes.

use artificer_protocol::PrecisionPolicy;

use crate::analytic_extrusion::Segment;
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop,
    LoopKey, Orientation, ParameterRange, Point2, Point3, Record, Shell, ShellKey, Solid, Surface,
    Topology, Vertex, VertexKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SewError {
    /// A piece's geometry could not be expressed or welded consistently.
    Inconsistent,
    /// A closed component enclosed no volume, or a cavity had no owner.
    Degenerate,
}

/// One face piece awaiting sewing: a surface carrier and its boundary loops
/// in that surface's own parameter space, outer loop first.
#[derive(Clone, Debug)]
pub(crate) struct SewFace {
    pub(crate) surface: Surface,
    pub(crate) loops: Vec<Vec<Segment>>,
    pub(crate) role: FaceRole,
}

/// Sews face pieces into a topology of one or more solids with cavities.
pub(crate) fn sew_shells(
    pieces: &[SewFace],
    precision: PrecisionPolicy,
) -> Result<Topology, SewError> {
    if pieces.is_empty() {
        return Err(SewError::Degenerate);
    }
    // The weld distance scales with the model, like the stacked builder's.
    let scale = pieces
        .iter()
        .flat_map(|piece| piece.loops.iter().flatten())
        .flat_map(|segment| [segment.start(), segment.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let weld = precision.linear_agreement.max(1.0e-12) * scale * 32.0;

    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    fn allocate(next_id: &mut u64) -> EntityId {
        let id = EntityId::from_raw(*next_id);
        *next_id += 1;
        id
    }

    // Vertex weld by position; edge weld by endpoint pair plus midpoint.
    fn find_vertex(
        topology: &mut Topology,
        next_id: &mut u64,
        weld: f64,
        point: Point3,
    ) -> VertexKey {
        if let Some(index) = topology
            .vertices
            .iter()
            .position(|candidate| (candidate.value.point - point).length() <= weld)
        {
            return VertexKey(index);
        }
        let key = VertexKey(topology.vertices.len());
        topology.vertices.push(Record {
            id: allocate(next_id),
            value: Vertex { point },
        });
        key
    }

    let mut face_keys: Vec<FaceKey> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let mut loop_keys = Vec::with_capacity(piece.loops.len());
        for segments in &piece.loops {
            if segments.is_empty() {
                return Err(SewError::Inconsistent);
            }
            let mut coedges = Vec::with_capacity(segments.len());
            for segment in segments {
                let start_world =
                    surface_point(piece.surface, segment.start()).ok_or(SewError::Inconsistent)?;
                let end_world =
                    surface_point(piece.surface, segment.end()).ok_or(SewError::Inconsistent)?;
                let middle_world = surface_point(piece.surface, segment_midpoint(*segment))
                    .ok_or(SewError::Inconsistent)?;
                let start_vertex = find_vertex(&mut topology, &mut next_id, weld, start_world);
                let end_vertex = find_vertex(&mut topology, &mut next_id, weld, end_world);

                let (curve, parameter_range) =
                    segment_curve(piece.surface, *segment).ok_or(SewError::Inconsistent)?;
                let found = topology.edges.iter().position(|edge| {
                    let vertices = edge.value.vertices;
                    let aligned = vertices == [start_vertex, end_vertex];
                    let swapped = vertices == [end_vertex, start_vertex];
                    if !aligned && !swapped {
                        return false;
                    }
                    let range = edge.value.parameter_range;
                    let middle = edge.value.curve.evaluate((range.start + range.end) / 2.0);
                    (middle - middle_world).length() <= weld
                });
                let (edge_key, orientation) = match found {
                    Some(index) => {
                        let aligned =
                            topology.edges[index].value.vertices == [start_vertex, end_vertex];
                        (
                            EdgeKey(index),
                            if aligned {
                                Orientation::Forward
                            } else {
                                Orientation::Reverse
                            },
                        )
                    }
                    None => {
                        let key = EdgeKey(topology.edges.len());
                        topology.edges.push(Record {
                            id: allocate(&mut next_id),
                            value: Edge {
                                vertices: [start_vertex, end_vertex],
                                curve,
                                parameter_range,
                            },
                        });
                        (key, Orientation::Forward)
                    }
                };
                let (pcurve, pcurve_range) = segment_pcurve(*segment);
                let coedge_key = CoedgeKey(topology.coedges.len());
                topology.coedges.push(Record {
                    id: allocate(&mut next_id),
                    value: Coedge {
                        edge: edge_key,
                        orientation,
                        pcurve,
                        parameter_range: pcurve_range,
                    },
                });
                coedges.push(coedge_key);
            }
            let loop_key = LoopKey(topology.loops.len());
            topology.loops.push(Record {
                id: allocate(&mut next_id),
                value: Loop { coedges },
            });
            loop_keys.push(loop_key);
        }
        let face_key = FaceKey(topology.faces.len());
        topology.faces.push(Record {
            id: allocate(&mut next_id),
            value: Face {
                surface: piece.surface,
                outer_loop: loop_keys[0],
                inner_loops: loop_keys[1..].to_vec(),
                role: piece.role,
            },
        });
        face_keys.push(face_key);
    }

    // Edge-connected components become shells.
    let mut face_edges: Vec<Vec<EdgeKey>> = vec![Vec::new(); topology.faces.len()];
    for (index, face) in topology.faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            for coedge_key in &topology.loops[loop_key.0].value.coedges {
                face_edges[index].push(topology.coedges[coedge_key.0].value.edge);
            }
        }
    }
    let mut edge_owners: Vec<Vec<usize>> = vec![Vec::new(); topology.edges.len()];
    for (face, edges) in face_edges.iter().enumerate() {
        for edge in edges {
            edge_owners[edge.0].push(face);
        }
    }
    let mut component = vec![usize::MAX; topology.faces.len()];
    let mut component_count = 0;
    for start in 0..topology.faces.len() {
        if component[start] != usize::MAX {
            continue;
        }
        let label = component_count;
        component_count += 1;
        let mut stack = vec![start];
        while let Some(face) = stack.pop() {
            if component[face] != usize::MAX {
                continue;
            }
            component[face] = label;
            for edge in &face_edges[face] {
                for owner in &edge_owners[edge.0] {
                    if component[*owner] == usize::MAX {
                        stack.push(*owner);
                    }
                }
            }
        }
    }

    // Component volumes decide which are solids and which are cavities.
    let mut volumes = Vec::with_capacity(component_count);
    let mut samples = Vec::with_capacity(component_count);
    for label in 0..component_count {
        let members: Vec<FaceKey> = (0..topology.faces.len())
            .filter(|face| component[*face] == label)
            .map(FaceKey)
            .collect();
        let mut probe = topology.clone();
        probe.shells = vec![Record {
            id: EntityId::from_raw(1_000_000),
            value: Shell {
                faces: members.clone(),
            },
        }];
        probe.solids = vec![Record {
            id: EntityId::from_raw(1_000_001),
            value: Solid {
                outer_shell: ShellKey(0),
                inner_shells: Vec::new(),
            },
        }];
        let measures = crate::validator::calculate_exact_shell_measures(&probe, None)
            .ok_or(SewError::Degenerate)?;
        if !measures.signed_volume.is_finite()
            || measures.signed_volume.abs() <= precision.min_feature_size.powi(3)
        {
            return Err(SewError::Degenerate);
        }
        volumes.push(measures.signed_volume);
        // A point on the component, for cavity ownership tests.
        let sample_face = &topology.faces[members[0].0].value;
        let sample_loop = &topology.loops[sample_face.outer_loop.0].value;
        let sample_coedge = &topology.coedges[sample_loop.coedges[0].0].value;
        let sample_2d = sample_coedge.pcurve.evaluate(
            (sample_coedge.parameter_range.start + sample_coedge.parameter_range.end) / 2.0,
        );
        samples.push(surface_point(sample_face.surface, sample_2d).ok_or(SewError::Inconsistent)?);
    }

    // Assemble shells and solids: positive components own themselves,
    // negative components are cavities of whichever positive component's
    // bounding solid contains them (resolved by the caller's classification
    // being regularized, the smallest enclosing positive volume wins).
    let mut shells = Vec::with_capacity(component_count);
    for label in 0..component_count {
        let members: Vec<FaceKey> = (0..topology.faces.len())
            .filter(|face| component[*face] == label)
            .map(FaceKey)
            .collect();
        shells.push(Record {
            id: allocate(&mut next_id),
            value: Shell { faces: members },
        });
    }
    topology.shells = shells;

    let mut solids: Vec<(usize, Record<Solid>)> = Vec::new();
    for (label, volume) in volumes.iter().enumerate() {
        if *volume > 0.0 {
            solids.push((
                label,
                Record {
                    id: allocate(&mut next_id),
                    value: Solid {
                        outer_shell: ShellKey(label),
                        inner_shells: Vec::new(),
                    },
                },
            ));
        }
    }
    if solids.is_empty() {
        return Err(SewError::Degenerate);
    }
    for label in 0..component_count {
        if volumes[label] > 0.0 {
            continue;
        }
        // Owner: the smallest positive component whose sample-containment
        // says it encloses this cavity's sample point.
        let mut owner: Option<(usize, f64)> = None;
        for (positive, _) in &solids {
            if point_in_component(&topology, &component, *positive, samples[label])
                && owner.is_none_or(|(_, volume)| volumes[*positive] < volume)
            {
                owner = Some((*positive, volumes[*positive]));
            }
        }
        let Some((owner_label, _)) = owner else {
            return Err(SewError::Degenerate);
        };
        let solid = solids
            .iter_mut()
            .find(|(label, _)| *label == owner_label)
            .expect("the owner is a recorded solid");
        solid.1.value.inner_shells.push(ShellKey(label));
    }
    topology.solids = solids.into_iter().map(|(_, record)| record).collect();
    Ok(topology)
}

/// Whether a point is inside the closed component labelled `wanted`, by ray
/// casting along a fixed set of directions and taking the first
/// non-degenerate parity.
fn point_in_component(
    topology: &Topology,
    component: &[usize],
    wanted: usize,
    point: Point3,
) -> bool {
    for direction in ray_directions() {
        let mut crossings = 0_usize;
        let mut degenerate = false;
        for (index, face) in topology.faces.iter().enumerate() {
            if component[index] != wanted {
                continue;
            }
            match ray_face_crossings(topology, &face.value, point, direction) {
                Some(count) => crossings += count,
                None => {
                    degenerate = true;
                    break;
                }
            }
        }
        if !degenerate {
            return crossings % 2 == 1;
        }
    }
    false
}

/// Deliberately awkward unit directions, so an axis-aligned model does not
/// immediately produce a degenerate ray.
pub(crate) fn ray_directions() -> [crate::topology::Vector3; 3] {
    let build = |x: f64, y: f64, z: f64| {
        let vector = crate::topology::Vector3::new(x, y, z);
        vector / vector.length()
    };
    [
        build(0.7368421052631579, 0.4210526315789474, 0.5286343612334802),
        build(
            -0.351_562_5,
            0.815_104_166_666_666_6,
            0.460_069_444_444_444_4,
        ),
        build(
            0.198_019_801_980_198,
            -0.554_455_445_544_554_4,
            0.808_255_772_646_536_4,
        ),
    ]
}

/// The number of times a ray from `point` along `direction` crosses one
/// face, or `None` when the hit is too close to the face boundary or the
/// carrier to trust.
pub(crate) fn ray_face_crossings(
    topology: &Topology,
    face: &Face,
    point: Point3,
    direction: crate::topology::Vector3,
) -> Option<usize> {
    let loops: Vec<Vec<Segment>> = face
        .loops()
        .map(|loop_key| crate::analytic_extrusion::topology_loop_segments(topology, loop_key))
        .collect::<Option<Vec<_>>>()?;
    let guard = 1.0e-9;
    match face.surface {
        Surface::Plane(plane) => {
            let denominator = plane.normal.dot(direction);
            let offset = (plane.origin - point).dot(plane.normal);
            if denominator.abs() <= guard * plane.normal.length() {
                // A grazing ray: reject unless the plane is clearly missed.
                return if offset.abs() > guard { Some(0) } else { None };
            }
            let parameter = offset / denominator;
            if parameter <= guard {
                return Some(usize::from(false));
            }
            let hit = point + direction * parameter;
            let local = Point2::new(
                (hit - plane.origin).dot(plane.u),
                (hit - plane.origin).dot(plane.v),
            );
            interior_parity(&loops, local, guard)
        }
        Surface::Cylinder(cylinder) => {
            // Solve |(p + t d) − axis line|² = r² in the plane ⟂ axis.
            let axis = cylinder.axis / cylinder.axis.length();
            let relative = point - cylinder.origin;
            let radial_point = relative - axis * relative.dot(axis);
            let radial_direction = direction - axis * direction.dot(axis);
            let a = radial_direction.dot(radial_direction);
            let b = 2.0 * radial_point.dot(radial_direction);
            let c = radial_point
                .dot(radial_point)
                .mul_add(1.0, -(cylinder.radius * cylinder.radius));
            if a.abs() <= guard {
                return if c.abs() > guard { Some(0) } else { None };
            }
            let discriminant = b.mul_add(b, -(4.0 * a * c));
            if discriminant.abs() <= guard {
                return None;
            }
            if discriminant < 0.0 {
                return Some(0);
            }
            let root = discriminant.sqrt();
            let mut crossings = 0;
            for t in [(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)] {
                if t <= guard {
                    continue;
                }
                let hit = point + direction * t;
                let offset = hit - cylinder.origin;
                let height = offset.dot(axis);
                let radial = offset - axis * height;
                let angle = radial
                    .dot(cylinder.radial_v)
                    .atan2(radial.dot(cylinder.radial_u));
                let u = cylinder.angular_sign * angle;
                // The face's parameter domain may sit in a shifted branch of
                // the angle; test the candidate at each whole turn nearby.
                let tau = std::f64::consts::TAU;
                let mut counted = false;
                for branch in [-1.0, 0.0, 1.0] {
                    let local = Point2::new(tau.mul_add(branch, u), height);
                    match interior_parity(&loops, local, guard) {
                        Some(1) => {
                            counted = true;
                            break;
                        }
                        Some(_) => {}
                        None => return None,
                    }
                }
                if counted {
                    crossings += 1;
                }
            }
            Some(crossings)
        }
        Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => None,
    }
}

/// Even-odd membership of a 2D point in a face's parameter loops, rejecting
/// hits within `guard` of any boundary segment.
fn interior_parity(loops: &[Vec<Segment>], point: Point2, guard: f64) -> Option<usize> {
    for segments in loops {
        for segment in segments {
            if segment_distance(*segment, point) <= guard {
                return None;
            }
        }
    }
    let wrapped: Vec<crate::analytic_extrusion::AnalyticLoop> = loops
        .iter()
        .map(|segments| crate::analytic_extrusion::AnalyticLoop {
            segments: segments.clone(),
            signed_area: 0.0,
        })
        .collect();
    let mut inside = false;
    for profile_loop in &wrapped {
        if crate::analytic_extrusion::point_inside_loop(point, profile_loop) {
            inside = !inside;
        }
    }
    Some(usize::from(inside))
}

fn segment_distance(segment: Segment, point: Point2) -> f64 {
    match segment {
        Segment::Line { start, end } => {
            let direction = Point2::new(end.x - start.x, end.y - start.y);
            let length_square = direction.x.mul_add(direction.x, direction.y * direction.y);
            if length_square <= f64::EPSILON {
                return (point.x - start.x).hypot(point.y - start.y);
            }
            let along = ((point.x - start.x)
                .mul_add(direction.x, (point.y - start.y) * direction.y)
                / length_square)
                .clamp(0.0, 1.0);
            let foot = Point2::new(
                direction.x.mul_add(along, start.x),
                direction.y.mul_add(along, start.y),
            );
            (point.x - foot.x).hypot(point.y - foot.y)
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            start,
            end,
        } => {
            let angle = (point.y - center.y).atan2(point.x - center.x);
            let progress = if sweep >= 0.0 {
                (angle - start_angle).rem_euclid(std::f64::consts::TAU) / sweep
            } else {
                (start_angle - angle).rem_euclid(std::f64::consts::TAU) / -sweep
            };
            if (0.0..=1.0).contains(&progress) {
                ((point.x - center.x).hypot(point.y - center.y) - radius).abs()
            } else {
                let to_start = (point.x - start.x).hypot(point.y - start.y);
                let to_end = (point.x - end.x).hypot(point.y - end.y);
                to_start.min(to_end)
            }
        }
    }
}

fn segment_midpoint(segment: Segment) -> Point2 {
    match segment {
        Segment::Line { start, end } => {
            Point2::new((start.x + end.x) / 2.0, (start.y + end.y) / 2.0)
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let angle = sweep.mul_add(0.5, start_angle);
            Point2::new(
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            )
        }
    }
}

/// A surface point from parameter coordinates, for the carriers the sewer
/// accepts.
fn surface_point(surface: Surface, point: Point2) -> Option<Point3> {
    match surface {
        Surface::Plane(plane) => Some(plane.evaluate(point)),
        Surface::Cylinder(cylinder) => Some(cylinder.evaluate(point)),
        Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => None,
    }
}

/// The 3D curve carried by one 2D boundary segment on a face.
fn segment_curve(surface: Surface, segment: Segment) -> Option<(Curve3, ParameterRange)> {
    match (surface, segment) {
        (Surface::Plane(plane), Segment::Line { start, end }) => Some(Curve3::line_segment([
            plane.evaluate(start),
            plane.evaluate(end),
        ])),
        (
            Surface::Plane(plane),
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            },
        ) => Some((
            Curve3::Circle {
                center: plane.evaluate(center),
                u: plane.u,
                v: plane.v,
                radius,
            },
            ParameterRange::new(start_angle, start_angle + sweep),
        )),
        (Surface::Cylinder(cylinder), Segment::Line { start, end }) => {
            let vertical = (start.x - end.x).abs() <= 1.0e-12;
            let horizontal = (start.y - end.y).abs() <= 1.0e-12;
            if vertical {
                Some(Curve3::line_segment([
                    cylinder.evaluate(start),
                    cylinder.evaluate(end),
                ]))
            } else if horizontal {
                let axis = cylinder.axis / cylinder.axis.length();
                Some((
                    Curve3::Circle {
                        center: cylinder.origin + axis * start.y,
                        u: cylinder.radial_u,
                        v: cylinder.radial_v,
                        radius: cylinder.radius,
                    },
                    ParameterRange::new(
                        cylinder.angular_sign * start.x,
                        cylinder.angular_sign * end.x,
                    ),
                ))
            } else {
                // A skewed parameter line would be a helix.
                None
            }
        }
        _ => None,
    }
}

/// The pcurve for one 2D boundary segment, in the face's parameter space.
fn segment_pcurve(segment: Segment) -> (Curve2, ParameterRange) {
    match segment {
        Segment::Line { start, end } => Curve2::line_segment([start, end]),
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => (
            Curve2::Circle {
                center,
                u: crate::topology::Vector2::new(1.0, 0.0),
                v: crate::topology::Vector2::new(0.0, 1.0),
                radius,
            },
            ParameterRange::new(start_angle, start_angle + sweep),
        ),
    }
}

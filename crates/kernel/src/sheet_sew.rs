//! Sews face pieces on any carrier into a sheet (ADR 0056, Track S).
//!
//! [`crate::sew::sew_shells`] sews the pieces a Boolean leaves on planes
//! and cylinders into closed solids. A sheet operation leaves pieces on
//! every carrier and need not close, so this is the same weld — vertices
//! by position, edges by their endpoints and midpoint — over the five
//! elementary carriers, with edge-connected components made into shells
//! and no solid assembled. Whether the result is a sheet or closes is for
//! the caller to decide.

use artificer_protocol::PrecisionPolicy;

use crate::analytic_boolean::CylinderSectionHarmonic;
use crate::analytic_extrusion::{Segment, allocate_id};
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Edge, EdgeKey, Face, FaceKey, FaceRole, Loop, LoopKey,
    Orientation, ParameterRange, Point2, Point3, Record, Shell, Surface, Topology, Vector2, Vertex,
    VertexKey,
};

/// One face piece awaiting sewing: a carrier and its boundary loops in the
/// carrier's own parameter space, outer loop first.
#[derive(Clone, Debug)]
pub(crate) struct SheetPiece {
    pub(crate) surface: Surface,
    pub(crate) loops: Vec<Vec<Segment>>,
    pub(crate) role: FaceRole,
}

/// Why pieces could not be sewn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SheetSewError {
    /// No piece at all.
    Empty,
    /// A piece's boundary has no exact curve on its carrier: a slanted line
    /// on a revolved carrier, or a carrier the sewer does not read.
    Inconsistent,
}

/// The weld distance for a set of pieces: the linear agreement scaled to
/// the pieces, as the Boolean's sewer scales it.
pub(crate) fn weld_distance(precision: PrecisionPolicy, scale: f64) -> f64 {
    precision.linear_agreement.max(1.0e-12) * scale.max(1.0) * 32.0
}

/// Sews pieces into faces sharing their vertices and edges, in one shell
/// per edge-connected component.
pub(crate) fn sew_pieces(
    pieces: &[SheetPiece],
    precision: PrecisionPolicy,
) -> Result<Topology, SheetSewError> {
    if pieces.is_empty() {
        return Err(SheetSewError::Empty);
    }
    let scale = pieces
        .iter()
        .flat_map(|piece| {
            piece
                .loops
                .iter()
                .flatten()
                .map(move |segment| piece.surface.evaluate(segment.start()))
        })
        .map(|point| point.x.abs().max(point.y.abs()).max(point.z.abs()))
        .fold(1.0_f64, f64::max);
    let weld = weld_distance(precision, scale);

    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    // A pole edge runs from a vertex to itself, so nothing about its ends
    // says which way a face walks it; its two uses are given opposite
    // senses in turn, which is all the edge-use family asks of a pole.
    let mut degenerate_uses: Vec<usize> = Vec::new();
    let find_vertex = |topology: &mut Topology, next_id: &mut u64, point: Point3| {
        if let Some(index) = topology
            .vertices
            .iter()
            .position(|candidate| (candidate.value.point - point).length() <= weld)
        {
            return VertexKey(index);
        }
        let key = VertexKey(topology.vertices.len());
        topology.vertices.push(Record {
            id: allocate_id(next_id),
            value: Vertex { point },
        });
        key
    };

    for piece in pieces {
        let mut loop_keys = Vec::with_capacity(piece.loops.len());
        for segments in &piece.loops {
            if segments.is_empty() {
                return Err(SheetSewError::Inconsistent);
            }
            let mut coedges = Vec::with_capacity(segments.len());
            for segment in segments {
                let (curve, range) =
                    segment_curve(piece.surface, *segment).ok_or(SheetSewError::Inconsistent)?;
                let start_world = curve.evaluate(range.start);
                let end_world = curve.evaluate(range.end);
                let middle_world = curve.evaluate((range.start + range.end) / 2.0);
                let start_vertex = find_vertex(&mut topology, &mut next_id, start_world);
                let end_vertex = find_vertex(&mut topology, &mut next_id, end_world);
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
                            id: allocate_id(&mut next_id),
                            value: Edge {
                                vertices: [start_vertex, end_vertex],
                                curve,
                                parameter_range: range,
                            },
                        });
                        degenerate_uses.push(0);
                        (key, Orientation::Forward)
                    }
                };
                let orientation = if start_vertex == end_vertex {
                    let uses = &mut degenerate_uses[edge_key.0];
                    *uses += 1;
                    if *uses % 2 == 1 {
                        Orientation::Forward
                    } else {
                        Orientation::Reverse
                    }
                } else {
                    orientation
                };
                let (pcurve, pcurve_range) = segment_pcurve(*segment);
                let coedge_key = CoedgeKey(topology.coedges.len());
                topology.coedges.push(Record {
                    id: allocate_id(&mut next_id),
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
                id: allocate_id(&mut next_id),
                value: Loop { coedges },
            });
            loop_keys.push(loop_key);
        }
        topology.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: piece.surface,
                outer_loop: loop_keys[0],
                inner_loops: loop_keys[1..].to_vec(),
                role: piece.role,
            },
        });
    }
    assign_shells(&mut topology, &mut next_id);
    Ok(topology)
}

/// Replaces the topology's shells with one per edge-connected component
/// of its faces, in the order the components' first faces come.
pub(crate) fn assign_shells(topology: &mut Topology, next_id: &mut u64) {
    let components = face_components(topology);
    let count = components
        .iter()
        .copied()
        .max()
        .map_or(0, |label| label + 1);
    topology.shells = (0..count)
        .map(|label| Record {
            id: allocate_id(next_id),
            value: Shell {
                faces: components
                    .iter()
                    .enumerate()
                    .filter(|(_, component)| **component == label)
                    .map(|(face, _)| FaceKey(face))
                    .collect(),
            },
        })
        .collect();
}

/// The edge-connected component of every face, labelled from zero in the
/// order the components are first met.
pub(crate) fn face_components(topology: &Topology) -> Vec<usize> {
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
    let mut count = 0;
    for start in 0..topology.faces.len() {
        if component[start] != usize::MAX {
            continue;
        }
        let label = count;
        count += 1;
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
    component
}

/// The 3D curve one boundary segment carries on a carrier, over the
/// parameter the segment's pcurve maps onto it.
pub(crate) fn segment_curve(
    surface: Surface,
    segment: Segment,
) -> Option<(Curve3, ParameterRange)> {
    const STRAIGHT: f64 = 1.0e-12;
    let vertical = |start: Point2, end: Point2| (start.x - end.x).abs() <= STRAIGHT;
    let horizontal = |start: Point2, end: Point2| (start.y - end.y).abs() <= STRAIGHT;
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
        (
            Surface::Plane(plane),
            Segment::Ellipse {
                center,
                u,
                major,
                minor,
                start_angle,
                sweep,
                ..
            },
        ) => Some((
            Curve3::Ellipse {
                center: plane.evaluate(center),
                u: plane.u * u.x + plane.v * u.y,
                v: plane.u * -u.y + plane.v * u.x,
                major_radius: major,
                minor_radius: minor,
            },
            ParameterRange::new(start_angle, start_angle + sweep),
        )),
        (Surface::Cylinder(cylinder), Segment::Line { start, end }) => {
            if vertical(start, end) {
                Some(Curve3::line_segment([
                    cylinder.evaluate(start),
                    cylinder.evaluate(end),
                ]))
            } else if horizontal(start, end) {
                Some((
                    Curve3::Circle {
                        center: cylinder.origin + cylinder.axis * start.y,
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
                None
            }
        }
        (
            Surface::Cylinder(cylinder),
            Segment::Harmonic {
                mean,
                amplitude,
                phase,
                start,
                end,
            },
        ) => {
            let section = CylinderSectionHarmonic {
                cylinder,
                mean,
                amplitude,
                phase,
            };
            let (center, u, v, major_radius, minor_radius) = section.ellipse()?;
            Some((
                Curve3::Ellipse {
                    center,
                    u,
                    v,
                    major_radius,
                    minor_radius,
                },
                ParameterRange::new(section.angle_at(start.x), section.angle_at(end.x)),
            ))
        }
        (Surface::Cone(cone), Segment::Line { start, end }) => {
            if vertical(start, end) {
                Some(Curve3::line_segment([
                    cone.evaluate(start),
                    cone.evaluate(end),
                ]))
            } else if horizontal(start, end) {
                let radius = cone.ring_radius(start.y);
                if radius.abs() <= STRAIGHT {
                    let apex = cone.evaluate(Point2::new(start.x, start.y));
                    return Some(Curve3::line_segment([apex, apex]));
                }
                Some((
                    Curve3::Circle {
                        center: cone.origin + cone.axis * start.y,
                        u: cone.radial_u,
                        v: cone.radial_v,
                        radius,
                    },
                    ParameterRange::new(cone.angular_sign * start.x, cone.angular_sign * end.x),
                ))
            } else {
                None
            }
        }
        (Surface::Sphere(sphere), Segment::Line { start, end }) => {
            if horizontal(start, end) {
                let (sin, cos) = start.y.sin_cos();
                let radius = sphere.radius * cos;
                if radius.abs() <= STRAIGHT * sphere.radius.abs().max(1.0) {
                    let pole = sphere.origin + sphere.axis * (sphere.radius * sin.signum());
                    return Some(Curve3::line_segment([pole, pole]));
                }
                Some((
                    Curve3::Circle {
                        center: sphere.origin + sphere.axis * (sphere.radius * sin),
                        u: sphere.radial_u,
                        v: sphere.radial_v,
                        radius,
                    },
                    ParameterRange::new(sphere.angular_sign * start.x, sphere.angular_sign * end.x),
                ))
            } else if vertical(start, end) {
                let angle = sphere.angular_sign * start.x;
                let radial = sphere.radial_u * angle.cos() + sphere.radial_v * angle.sin();
                Some((
                    Curve3::Circle {
                        center: sphere.origin,
                        u: radial,
                        v: sphere.axis,
                        radius: sphere.radius,
                    },
                    ParameterRange::new(start.y, end.y),
                ))
            } else {
                None
            }
        }
        (Surface::Torus(torus), Segment::Line { start, end }) => {
            if horizontal(start, end) {
                let (sin, cos) = start.y.sin_cos();
                Some((
                    Curve3::Circle {
                        center: torus.origin + torus.axis * (torus.minor_radius * sin),
                        u: torus.radial_u,
                        v: torus.radial_v,
                        radius: torus.major_radius + torus.minor_radius * cos,
                    },
                    ParameterRange::new(torus.angular_sign * start.x, torus.angular_sign * end.x),
                ))
            } else if vertical(start, end) {
                let angle = torus.angular_sign * start.x;
                let radial = torus.radial_u * angle.cos() + torus.radial_v * angle.sin();
                Some((
                    Curve3::Circle {
                        center: torus.origin + radial * torus.major_radius,
                        u: radial,
                        v: torus.axis,
                        radius: torus.minor_radius,
                    },
                    ParameterRange::new(start.y, end.y),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The pcurve of one boundary segment, in the face's parameter space.
pub(crate) fn segment_pcurve(segment: Segment) -> (Curve2, ParameterRange) {
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
                u: Vector2::new(1.0, 0.0),
                v: Vector2::new(0.0, 1.0),
                radius,
            },
            ParameterRange::new(start_angle, start_angle + sweep),
        ),
        Segment::Ellipse {
            center,
            u,
            major,
            minor,
            start_angle,
            sweep,
            ..
        } => (
            Curve2::Ellipse {
                center,
                u: Vector2::new(u.x, u.y),
                v: Vector2::new(-u.y, u.x),
                major_radius: major,
                minor_radius: minor,
            },
            ParameterRange::new(start_angle, start_angle + sweep),
        ),
        Segment::Harmonic {
            mean,
            amplitude,
            phase,
            start,
            end,
        } => (
            Curve2::Harmonic {
                mean,
                amplitude,
                phase,
            },
            ParameterRange::new(start.x, end.x),
        ),
        Segment::Trace {
            host,
            other,
            branch,
            shift,
            from,
            to,
            ..
        } => (
            Curve2::Trace {
                host,
                other,
                branch,
                on_other: false,
                shift,
            },
            ParameterRange::new(from, to),
        ),
    }
}

//! An exact fillet or chamfer around the rim of a hole through any wall.
//!
//! The rim-loop blend finishes the rim of a *prism cap* by re-extruding an
//! offset profile, which is the right tool when the finished face is a cap and
//! the wrong one when it is a side wall, a sloped face, or any face of a body
//! that is no longer a prism about that face's normal. Every drilled hole has
//! such a rim, and rounding it is the finish a user reaches for most.
//!
//! The construction is local. A hole rim is one closed circle where a plane
//! meets a bore whose axis is the plane's normal, and a rolling ball of radius
//! `d` sitting inside the material touches the plane along the circle of
//! radius `r + d` and the bore at depth `d`. Between those two circles it
//! sweeps a quarter torus: major radius `r + d`, minor radius `d`, centred on
//! the axis a depth `d` below the wall. A chamfer replaces the torus with the
//! cone through the same two circles. Nothing else moves: the wall's hole
//! grows to `r + d`, the bore's rim ring sinks to depth `d`, the generators
//! that ran up to it shorten, and the new band is one face per rim arc, so a
//! rim already split at the bore's seam keeps its split and a rim that is one
//! closed edge keeps its one seam edge, used twice, as a cylinder's is.
//!
//! No corner is closed here because there is none: the rim is one closed
//! edge, and a closed edge has no ends.

use std::collections::BTreeMap;

use artificer_protocol::{EdgeFinishKind, EntityKind, EntityRef, PrecisionPolicy, SnapshotId};

use crate::topology::{
    Coedge, CoedgeKey, Cone, Curve2, Curve3, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole,
    Loop, LoopKey, Orientation, ParameterRange, Point2, Point3, Record, Surface, Topology, Torus,
    Vector3, Vertex, VertexKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HoleRimBlendError {
    TargetInvalid,
    /// Not a hole rim between a wall and a bore, or not the whole of one.
    DomainUnsupported,
    /// The finish would run out of wall or out of bore.
    DistanceInvalid,
}

/// One arc of the rim as the body carries it.
struct RimArc {
    edge: EdgeKey,
    range: ParameterRange,
}

/// Fillets or chamfers one complete hole rim: every edge of one circle where
/// a planar face meets a bore whose axis is that face's normal.
pub(crate) fn build_hole_rim_blend(
    snapshot: SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, HoleRimBlendError> {
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(HoleRimBlendError::TargetInvalid);
    }
    let floor = precision.min_feature_size.max(1.0e-9);
    if !distance.is_finite() || distance < floor {
        return Err(HoleRimBlendError::DistanceInvalid);
    }
    let id_of = |id: u64| {
        topology
            .edges
            .iter()
            .position(|record| record.id.get() == id)
            .map(EdgeKey)
    };
    let mut arcs = Vec::with_capacity(targets.len());
    for target in targets {
        let key = id_of(target.entity.0).ok_or(HoleRimBlendError::TargetInvalid)?;
        arcs.push(RimArc {
            edge: key,
            range: topology.edges[key.0].value.parameter_range,
        });
    }

    // One circle, carried by every target.
    let Curve3::Circle {
        center,
        u,
        v,
        radius,
    } = topology.edges[arcs[0].edge.0].value.curve
    else {
        return Err(HoleRimBlendError::DomainUnsupported);
    };
    let agreement = precision.linear_agreement.max(1.0e-12) * radius.max(1.0);
    let same_circle = |curve: Curve3| match curve {
        Curve3::Circle {
            center: other_center,
            u: other_u,
            v: other_v,
            radius: other_radius,
        } => {
            (other_center - center).length() <= agreement
                && (other_radius - radius).abs() <= agreement
                && (other_u - u).length() <= 1.0e-9
                && (other_v - v).length() <= 1.0e-9
        }
        _ => false,
    };
    if arcs
        .iter()
        .any(|arc| !same_circle(topology.edges[arc.edge.0].value.curve))
    {
        return Err(HoleRimBlendError::DomainUnsupported);
    }
    let turn: f64 = arcs
        .iter()
        .map(|arc| (arc.range.end - arc.range.start).abs())
        .sum();
    if (turn - std::f64::consts::TAU).abs() > precision.angular_agreement_radians.max(1.0e-9) {
        return Err(HoleRimBlendError::DomainUnsupported);
    }

    // The two faces every arc borders: one plane, and a bore on its normal.
    let incident = crate::edge_incident_face_indices(topology);
    let mut wall: Option<usize> = None;
    let mut bores: Vec<usize> = Vec::new();
    for arc in &arcs {
        let faces = &incident[arc.edge.0];
        if faces.len() != 2 {
            return Err(HoleRimBlendError::DomainUnsupported);
        }
        for face in faces {
            match topology.faces[*face].value.surface {
                Surface::Plane(_) => {
                    if wall.is_some_and(|known| known != *face) {
                        return Err(HoleRimBlendError::DomainUnsupported);
                    }
                    wall = Some(*face);
                }
                Surface::Cylinder(_) => {
                    if !bores.contains(face) {
                        bores.push(*face);
                    }
                }
                _ => return Err(HoleRimBlendError::DomainUnsupported),
            }
        }
    }
    let wall = wall.ok_or(HoleRimBlendError::DomainUnsupported)?;
    if bores.is_empty() {
        return Err(HoleRimBlendError::DomainUnsupported);
    }
    let plane = topology.faces[wall]
        .value
        .surface
        .as_plane()
        .ok_or(HoleRimBlendError::DomainUnsupported)?;
    let normal = plane.normal / plane.normal.length();
    if !normal.is_finite() {
        return Err(HoleRimBlendError::DomainUnsupported);
    }
    if u.cross(v).cross(normal).length() > 1.0e-9
        || (center - plane.origin).dot(normal).abs() > agreement
    {
        return Err(HoleRimBlendError::DomainUnsupported);
    }
    for bore in &bores {
        let Surface::Cylinder(cylinder) = topology.faces[*bore].value.surface else {
            return Err(HoleRimBlendError::DomainUnsupported);
        };
        let axis = cylinder.axis / cylinder.axis.length();
        if axis.cross(normal).length() > 1.0e-9
            || (cylinder.radius - radius).abs() > agreement
            || (center - cylinder.origin).cross(axis).length() > agreement
        {
            return Err(HoleRimBlendError::DomainUnsupported);
        }
    }

    // A hole rim is an inner loop of the wall made of these arcs and nothing
    // else. The rim of a boss is the wall's outer loop, and is the rim-loop
    // blend's business.
    let rim_loop = topology.faces[wall]
        .value
        .inner_loops
        .iter()
        .copied()
        .find(|loop_key| {
            let coedges = &topology.loops[loop_key.0].value.coedges;
            coedges.len() == arcs.len()
                && coedges.iter().all(|coedge| {
                    let edge = topology.coedges[coedge.0].value.edge;
                    arcs.iter().any(|arc| arc.edge == edge)
                })
        })
        .ok_or(HoleRimBlendError::DomainUnsupported)?;
    // Room on the wall: every other loop of it stays clear of the grown hole.
    // The nearest approach is taken exactly rather than sampled: a straight
    // edge five away passes closest between any samples spread along it, and
    // a rim grown to 5.3 then crosses it unseen.
    let reach = radius + distance + floor;
    for loop_key in topology.faces[wall].value.loops() {
        if loop_key == rim_loop {
            continue;
        }
        for coedge in &topology.loops[loop_key.0].value.coedges {
            let edge = topology.edges[topology.coedges[coedge.0].value.edge.0].value;
            if distance_from_axis(edge, center, normal) < reach {
                return Err(HoleRimBlendError::DistanceInvalid);
            }
        }
    }

    // Depth runs into the material, opposite the wall's outward normal. Each
    // bore face's parameter `y` runs along its own axis; the rim ring sits at
    // `y_rim`, and the finish needs `distance` of bore beyond it.
    let mut bore_step: BTreeMap<usize, f64> = BTreeMap::new();
    for bore in &bores {
        let Surface::Cylinder(cylinder) = topology.faces[*bore].value.surface else {
            unreachable!("checked above");
        };
        let axis_length = cylinder.axis.length();
        let step = -(cylinder.axis.dot(normal) / axis_length).signum() * distance / axis_length;
        bore_step.insert(*bore, step);
        // The bore's extent along y past the rim.
        let rim_y = (center - cylinder.origin).dot(cylinder.axis) / (axis_length * axis_length);
        let far = topology.faces[*bore]
            .value
            .loops()
            .flat_map(|loop_key| topology.loops[loop_key.0].value.coedges.iter())
            .flat_map(|coedge| topology.coedges[coedge.0].value.pcurve_endpoints())
            .map(|point| (point.y - rim_y) * step.signum())
            .fold(0.0_f64, f64::max);
        if far * axis_length < distance + floor {
            return Err(HoleRimBlendError::DistanceInvalid);
        }
    }

    // The band's frame: the rim's own, made right-handed about the wall's
    // normal so the torus and cone azimuth is the rim's angle.
    let handed = u.cross(v).dot(normal).signum();
    let radial_v = v * handed;
    let angular_sign = handed;
    let depth = center + normal * (-distance);
    let radial_at = |angle: f64| u * angle.cos() + v * angle.sin();

    let mut result = topology.clone();
    let mut next_id = next_entity_id(topology);
    let mut allocate = || {
        let id = EntityId::from_raw(next_id);
        next_id += 1;
        id
    };

    // Each old rim vertex sinks to depth on the bore; a new vertex above it
    // on the wall takes its place on the grown hole, and a seam joins them.
    let mut wall_vertex: BTreeMap<usize, VertexKey> = BTreeMap::new();
    let mut seam_edge: BTreeMap<usize, EdgeKey> = BTreeMap::new();
    let mut angle_of: BTreeMap<usize, f64> = BTreeMap::new();
    for arc in &arcs {
        let edge = topology.edges[arc.edge.0].value;
        for (vertex, angle) in [
            (edge.vertices[0], arc.range.start),
            (edge.vertices[1], arc.range.end),
        ] {
            if wall_vertex.contains_key(&vertex.0) {
                continue;
            }
            angle_of.insert(vertex.0, angle);
            let radial = radial_at(angle);
            let on_wall = center + radial * (radius + distance);
            let on_bore = depth + radial * radius;
            result.vertices[vertex.0].value.point = on_bore;
            let above = VertexKey(result.vertices.len());
            result.vertices.push(Record {
                id: allocate(),
                value: Vertex { point: on_wall },
            });
            wall_vertex.insert(vertex.0, above);
            let seam = EdgeKey(result.edges.len());
            let (curve, parameter_range) = match kind {
                EdgeFinishKind::Fillet => (
                    Curve3::Circle {
                        center: depth + radial * (radius + distance),
                        u: radial,
                        v: normal,
                        radius: distance,
                    },
                    ParameterRange::new(std::f64::consts::FRAC_PI_2, std::f64::consts::PI),
                ),
                EdgeFinishKind::Chamfer => Curve3::line_segment([on_wall, on_bore]),
            };
            result.edges.push(Record {
                id: allocate(),
                value: Edge {
                    vertices: [above, vertex],
                    curve,
                    parameter_range,
                },
            });
            seam_edge.insert(vertex.0, seam);
        }
    }

    // Every generator that ran up to the rim now stops at depth.
    for (index, edge) in topology.edges.iter().enumerate() {
        if arcs.iter().any(|arc| arc.edge.0 == index) {
            continue;
        }
        let Curve3::Line { .. } = edge.value.curve else {
            continue;
        };
        let touches = [edge.value.vertices[0].0, edge.value.vertices[1].0]
            .into_iter()
            .filter(|vertex| angle_of.contains_key(vertex))
            .count();
        if touches == 0 {
            continue;
        }
        let endpoints = [
            result.vertices[edge.value.vertices[0].0].value.point,
            result.vertices[edge.value.vertices[1].0].value.point,
        ];
        result.edges[index].value.set_line_endpoints(endpoints);
        for coedge in result.coedges.iter_mut() {
            if coedge.value.edge.0 != index {
                continue;
            }
            let Some(bore) = bores.iter().find(|bore| {
                topology.faces[**bore].value.loops().any(|loop_key| {
                    topology.loops[loop_key.0]
                        .value
                        .coedges
                        .iter()
                        .any(|key| topology.coedges[key.0].id == coedge.id)
                })
            }) else {
                return Err(HoleRimBlendError::DomainUnsupported);
            };
            let step = bore_step[bore];
            let [mut start, mut end] = coedge.value.pcurve_endpoints();
            let (start_key, end_key) = match coedge.value.orientation {
                Orientation::Forward => (edge.value.vertices[0].0, edge.value.vertices[1].0),
                Orientation::Reverse => (edge.value.vertices[1].0, edge.value.vertices[0].0),
            };
            if angle_of.contains_key(&start_key) {
                start.y += step;
            }
            if angle_of.contains_key(&end_key) {
                end.y += step;
            }
            if !coedge.value.set_line_pcurve_endpoints([start, end]) {
                return Err(HoleRimBlendError::DomainUnsupported);
            }
        }
    }

    // The rim arcs themselves: each becomes two — the grown hole on the wall
    // and the sunk ring on the bore — and the band between them.
    let mut band_faces = Vec::with_capacity(arcs.len());
    let ordinal = |index: usize| FaceRole::FeatureSide(u32::try_from(index).unwrap_or(u32::MAX));
    for (index, arc) in arcs.iter().enumerate() {
        let old = topology.edges[arc.edge.0].value;
        let [from, to] = [old.vertices[0].0, old.vertices[1].0];
        let wall_edge = EdgeKey(result.edges.len());
        result.edges.push(Record {
            id: allocate(),
            value: Edge {
                vertices: [wall_vertex[&from], wall_vertex[&to]],
                curve: Curve3::Circle {
                    center,
                    u,
                    v,
                    radius: radius + distance,
                },
                parameter_range: arc.range,
            },
        });
        // The old edge keeps its key and its vertices and sinks to depth, so
        // every coedge the bore had on it stays valid.
        result.edges[arc.edge.0].value.curve = Curve3::Circle {
            center: depth,
            u,
            v,
            radius,
        };
        let bore_edge = arc.edge;
        // The wall's coedge moves to the grown hole; the bore's stays and
        // its pcurve sinks.
        for coedge in result.coedges.iter_mut() {
            if coedge.value.edge != arc.edge {
                continue;
            }
            let on_wall = topology.loops[rim_loop.0]
                .value
                .coedges
                .iter()
                .any(|key| topology.coedges[key.0].id == coedge.id);
            if on_wall {
                coedge.value.edge = wall_edge;
                match &mut coedge.value.pcurve {
                    Curve2::Circle { radius: grown, .. } => *grown = radius + distance,
                    _ => return Err(HoleRimBlendError::DomainUnsupported),
                }
            } else {
                let Some(bore) = bores.iter().find(|bore| {
                    topology.faces[**bore].value.loops().any(|loop_key| {
                        topology.loops[loop_key.0]
                            .value
                            .coedges
                            .iter()
                            .any(|key| topology.coedges[key.0].id == coedge.id)
                    })
                }) else {
                    return Err(HoleRimBlendError::DomainUnsupported);
                };
                let step = bore_step[bore];
                let [start, end] = coedge.value.pcurve_endpoints();
                if !coedge.value.set_line_pcurve_endpoints([
                    Point2::new(start.x, start.y + step),
                    Point2::new(end.x, end.y + step),
                ]) {
                    return Err(HoleRimBlendError::DomainUnsupported);
                }
            }
        }

        // The band over this arc: the wall's edge walked against the wall's
        // own use of it, down the seam, the bore's edge against the bore's
        // use, and back up — the winding that pairs every edge's two uses.
        let increasing = arc.range.end >= arc.range.start;
        let (lo, hi) = if increasing {
            (arc.range.start, arc.range.end)
        } else {
            (arc.range.end, arc.range.start)
        };
        let (lo_key, hi_key) = if increasing { (from, to) } else { (to, from) };
        let along = |forward_when_increasing: bool| {
            if increasing == forward_when_increasing {
                Orientation::Forward
            } else {
                Orientation::Reverse
            }
        };
        let (surface, uses): (Surface, Vec<(EdgeKey, Orientation, [Point2; 2])>) = match kind {
            EdgeFinishKind::Fillet => {
                let (top, side) = (std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
                (
                    Surface::Torus(Torus {
                        origin: depth,
                        axis: normal,
                        radial_u: u,
                        radial_v,
                        major_radius: radius + distance,
                        minor_radius: distance,
                        angular_sign,
                    }),
                    vec![
                        (
                            wall_edge,
                            along(true),
                            [Point2::new(lo, top), Point2::new(hi, top)],
                        ),
                        (
                            seam_edge[&hi_key],
                            Orientation::Forward,
                            [Point2::new(hi, top), Point2::new(hi, side)],
                        ),
                        (
                            bore_edge,
                            along(false),
                            [Point2::new(hi, side), Point2::new(lo, side)],
                        ),
                        (
                            seam_edge[&lo_key],
                            Orientation::Reverse,
                            [Point2::new(lo, side), Point2::new(lo, top)],
                        ),
                    ],
                )
            }
            EdgeFinishKind::Chamfer => (
                Surface::Cone(Cone {
                    origin: depth,
                    axis: normal,
                    radial_u: u,
                    radial_v,
                    base_radius: radius,
                    slope: 1.0,
                    // The cone's frame runs its azimuth the other way from the
                    // torus's, so the same walk winds the other way; the sign
                    // turns it back and the pcurves follow it.
                    angular_sign: -angular_sign,
                }),
                vec![
                    (
                        wall_edge,
                        along(true),
                        [Point2::new(-lo, distance), Point2::new(-hi, distance)],
                    ),
                    (
                        seam_edge[&hi_key],
                        Orientation::Forward,
                        [Point2::new(-hi, distance), Point2::new(-hi, 0.0)],
                    ),
                    (
                        bore_edge,
                        along(false),
                        [Point2::new(-hi, 0.0), Point2::new(-lo, 0.0)],
                    ),
                    (
                        seam_edge[&lo_key],
                        Orientation::Reverse,
                        [Point2::new(-lo, 0.0), Point2::new(-lo, distance)],
                    ),
                ],
            ),
        };
        let mut coedges = Vec::with_capacity(uses.len());
        for (edge, orientation, endpoints) in uses {
            let key = CoedgeKey(result.coedges.len());
            result.coedges.push(Record {
                id: allocate(),
                value: Coedge::line(edge, orientation, endpoints),
            });
            coedges.push(key);
        }
        let loop_key = LoopKey(result.loops.len());
        result.loops.push(Record {
            id: allocate(),
            value: Loop { coedges },
        });
        let face_key = FaceKey(result.faces.len());
        result.faces.push(Record {
            id: allocate(),
            value: Face {
                surface,
                outer_loop: loop_key,
                inner_loops: Vec::new(),
                role: ordinal(index),
            },
        });
        band_faces.push(face_key);
    }

    // The band belongs to the shell the wall does.
    let shell = result
        .shells
        .iter_mut()
        .find(|shell| shell.value.faces.contains(&FaceKey(wall)))
        .ok_or(HoleRimBlendError::DomainUnsupported)?;
    shell.value.faces.extend(band_faces);
    Ok(result)
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

/// The least distance from the hole's axis to an edge lying in the wall,
/// measured square to the axis.
///
/// Lines and circles, which is what a wall's other loops are made of almost
/// always, take their closed forms: the foot of the perpendicular on a
/// segment, and the radial nearest point on an arc when the arc reaches it.
/// Anything else is bracketed on a fine sampling and each bracket narrowed
/// to its minimum, which converges on the true nearest point rather than on
/// the nearest sample.
fn distance_from_axis(edge: Edge, center: Point3, normal: Vector3) -> f64 {
    let across = |point: Point3| {
        let offset = point - center;
        offset - normal * offset.dot(normal)
    };
    let range = edge.parameter_range;
    match edge.curve {
        Curve3::Line { endpoints } => {
            let start = across(endpoints[0]);
            let run = across(endpoints[1]) - start;
            let span = run.dot(run);
            let t = if span > 0.0 {
                (-start.dot(run) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (start + run * t).length()
        }
        Curve3::Circle {
            center: arc_center,
            u,
            v,
            radius,
        } if u.dot(normal).abs() <= 1.0e-9 && v.dot(normal).abs() <= 1.0e-9 => {
            let ends = [range.start, range.end].map(|t| across(edge.curve.evaluate(t)).length());
            let nearest_end = ends[0].min(ends[1]);
            // The circle's nearest point to the axis lies along the line from
            // its centre to the axis, which is inside the arc or not at all.
            let toward_axis = across(center) - across(arc_center);
            let reach = toward_axis.length();
            if reach <= f64::EPSILON {
                return radius.min(nearest_end);
            }
            let angle = toward_axis.dot(v).atan2(toward_axis.dot(u));
            let (low, high) = if range.start <= range.end {
                (range.start, range.end)
            } else {
                (range.end, range.start)
            };
            let on_arc =
                (angle - low).rem_euclid(std::f64::consts::TAU) <= high - low + f64::EPSILON;
            if on_arc {
                (reach - radius).abs().min(nearest_end)
            } else {
                nearest_end
            }
        }
        curve => {
            let distance_at = |t: f64| across(curve.evaluate(t)).length();
            let samples = 256_u32;
            let step = (range.end - range.start) / f64::from(samples);
            let values = (0..=samples)
                .map(|index| distance_at(range.start + step * f64::from(index)))
                .collect::<Vec<_>>();
            let mut least = values.iter().copied().fold(f64::INFINITY, f64::min);
            for index in 0..=samples as usize {
                let before = index.checked_sub(1).map_or(f64::INFINITY, |at| values[at]);
                let after = values.get(index + 1).copied().unwrap_or(f64::INFINITY);
                if values[index] > before || values[index] > after {
                    continue;
                }
                // Golden-section search over the two samples either side.
                let mut low = range.start + step * (index as f64 - 1.0).max(0.0);
                let mut high = range.start + step * (index as f64 + 1.0).min(f64::from(samples));
                let ratio = (5.0_f64.sqrt() - 1.0) / 2.0;
                for _ in 0..80 {
                    let first = high - (high - low) * ratio;
                    let second = low + (high - low) * ratio;
                    if distance_at(first) <= distance_at(second) {
                        high = second;
                    } else {
                        low = first;
                    }
                }
                least = least.min(distance_at((low + high) / 2.0));
            }
            least
        }
    }
}

//! An exact fillet or chamfer along a concave rim: where a cylindrical boss
//! stands on a plane, or where a cylindrical pocket's wall meets its floor
//! (ADR 0056, F2).
//!
//! The hole-rim blend rounds the *convex* rim of a bore, where the rolling
//! ball sits inside the material and the band takes material away. A boss
//! meeting its plate, a counterbore's floor meeting its bore wall, or a
//! blind pocket's floor rim is the mirror case: the ball sits in the air,
//! touching the plane at a circle of radius `R ± d` and the cylinder at
//! height `d` above the plane, and the band it sweeps *adds* the corner
//! region a ball cannot reach. The band is a quarter torus of major radius
//! `R + d` (a boss, whose material is inside the cylinder) or `R − d` (a
//! pocket, whose material is outside it), minor radius `d`, centred on the
//! axis a height `d` above the plane; a chamfer replaces it with the cone
//! through the same two circles.
//!
//! The construction is local, as the hole-rim blend's is. The plane's rim
//! loop grows (a boss) or shrinks (a pocket) to the contact radius, the
//! cylinder's rim ring rises by `d`, the generators that ran down to it
//! shorten, and the new band is one face per rim arc, so a rim split at the
//! cylinder's seam keeps its split. Nothing is cut and nothing is unioned:
//! the material added is exactly the band's own corner region, which is what
//! lets Pappus state its volume in closed form.
//!
//! A concave rim needs the cylinder to run *up* from the plane, on the side
//! the plane's outward normal points to. A cylinder running down into the
//! material is a hole rim or a cap rim, convex, and another rung's business.

use std::collections::BTreeMap;

use artificer_protocol::{EdgeFinishKind, EntityKind, EntityRef, PrecisionPolicy, SnapshotId};

use crate::hole_rim_blend::{distance_from_axis, next_entity_id};
use crate::topology::{
    Coedge, CoedgeKey, Cone, Curve2, Curve3, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole,
    Loop, LoopKey, Orientation, ParameterRange, Point2, Point3, Record, Surface, Topology, Torus,
    Vector3, Vertex, VertexKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConcaveRimBlendError {
    TargetInvalid,
    /// Not a concave rim between a plane and a cylinder standing on it, or
    /// not the whole of one.
    DomainUnsupported,
    /// The finish would run out of plane or out of cylinder wall.
    DistanceInvalid,
}

/// One arc of the rim as the body carries it.
struct RimArc {
    edge: EdgeKey,
    range: ParameterRange,
}

/// Which way the material lies across the rim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RimKind {
    /// Material inside the cylinder: a boss standing on the plane, whose
    /// rim is an inner loop of the plane face.
    Boss,
    /// Material outside the cylinder: a pocket or counterbore whose floor
    /// is the plane face and whose rim is that face's outer loop.
    Pocket,
}

/// Fillets or chamfers one complete concave rim: every edge of one circle
/// where a planar face meets a cylinder standing on it along the plane's
/// outward normal.
#[allow(clippy::too_many_lines)]
pub(crate) fn build_concave_rim_blend(
    snapshot: SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, ConcaveRimBlendError> {
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(ConcaveRimBlendError::TargetInvalid);
    }
    let floor = precision.min_feature_size.max(1.0e-9);
    if !distance.is_finite() || distance < floor {
        return Err(ConcaveRimBlendError::DistanceInvalid);
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
        let key = id_of(target.entity.0).ok_or(ConcaveRimBlendError::TargetInvalid)?;
        if arcs.iter().any(|arc: &RimArc| arc.edge == key) {
            return Err(ConcaveRimBlendError::TargetInvalid);
        }
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
        return Err(ConcaveRimBlendError::DomainUnsupported);
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
        return Err(ConcaveRimBlendError::DomainUnsupported);
    }
    let turn: f64 = arcs
        .iter()
        .map(|arc| (arc.range.end - arc.range.start).abs())
        .sum();
    if (turn - std::f64::consts::TAU).abs() > precision.angular_agreement_radians.max(1.0e-9) {
        return Err(ConcaveRimBlendError::DomainUnsupported);
    }

    // The two faces every arc borders: one plane, and a cylinder on its
    // normal.
    let incident = crate::edge_incident_face_indices(topology);
    let mut wall: Option<usize> = None;
    let mut cylinders: Vec<usize> = Vec::new();
    for arc in &arcs {
        let faces = &incident[arc.edge.0];
        if faces.len() != 2 {
            return Err(ConcaveRimBlendError::DomainUnsupported);
        }
        for face in faces {
            match topology.faces[*face].value.surface {
                Surface::Plane(_) => {
                    if wall.is_some_and(|known| known != *face) {
                        return Err(ConcaveRimBlendError::DomainUnsupported);
                    }
                    wall = Some(*face);
                }
                Surface::Cylinder(_) => {
                    if !cylinders.contains(face) {
                        cylinders.push(*face);
                    }
                }
                _ => return Err(ConcaveRimBlendError::DomainUnsupported),
            }
        }
    }
    let wall = wall.ok_or(ConcaveRimBlendError::DomainUnsupported)?;
    if cylinders.is_empty() {
        return Err(ConcaveRimBlendError::DomainUnsupported);
    }
    let plane = topology.faces[wall]
        .value
        .surface
        .as_plane()
        .ok_or(ConcaveRimBlendError::DomainUnsupported)?;
    let normal = plane.normal / plane.normal.length();
    if !normal.is_finite() {
        return Err(ConcaveRimBlendError::DomainUnsupported);
    }
    if u.cross(v).cross(normal).length() > 1.0e-9
        || (center - plane.origin).dot(normal).abs() > agreement
    {
        return Err(ConcaveRimBlendError::DomainUnsupported);
    }
    for face in &cylinders {
        let Surface::Cylinder(cylinder) = topology.faces[*face].value.surface else {
            return Err(ConcaveRimBlendError::DomainUnsupported);
        };
        let axis = cylinder.axis / cylinder.axis.length();
        if axis.cross(normal).length() > 1.0e-9
            || (cylinder.radius - radius).abs() > agreement
            || (center - cylinder.origin).cross(axis).length() > agreement
        {
            return Err(ConcaveRimBlendError::DomainUnsupported);
        }
    }

    // The rim is one whole loop of the plane face: its outer loop for a
    // pocket floor, an inner loop for a boss.
    let is_rim_loop = |loop_key: LoopKey| {
        let coedges = &topology.loops[loop_key.0].value.coedges;
        coedges.len() == arcs.len()
            && coedges.iter().all(|coedge| {
                let edge = topology.coedges[coedge.0].value.edge;
                arcs.iter().any(|arc| arc.edge == edge)
            })
    };
    let rim_loop = topology.faces[wall]
        .value
        .loops()
        .find(|loop_key| is_rim_loop(*loop_key))
        .ok_or(ConcaveRimBlendError::DomainUnsupported)?;
    let rim_is_outer = rim_loop == topology.faces[wall].value.outer_loop;

    // Which way the material lies: read from the cylinder's own outward
    // normal at a rim point, and cross-checked against the loop the rim is.
    let radial_at = |angle: f64| u * angle.cos() + v * angle.sin();
    let probe_angle = arcs[0].range.start + (arcs[0].range.end - arcs[0].range.start) * 0.5;
    let probe_radial = radial_at(probe_angle);
    let first_cylinder = cylinders
        .iter()
        .find(|face| incident[arcs[0].edge.0].contains(face))
        .ok_or(ConcaveRimBlendError::DomainUnsupported)?;
    let outward = topology.faces[*first_cylinder]
        .value
        .surface
        .outward_normal_at(center + probe_radial * radius)
        .ok_or(ConcaveRimBlendError::DomainUnsupported)?;
    let rim_kind = if outward.dot(probe_radial) > 0.5 {
        RimKind::Boss
    } else if outward.dot(probe_radial) < -0.5 {
        RimKind::Pocket
    } else {
        return Err(ConcaveRimBlendError::DomainUnsupported);
    };
    if (rim_kind == RimKind::Boss) == rim_is_outer {
        return Err(ConcaveRimBlendError::DomainUnsupported);
    }

    // The cylinder runs up from the plane, on the side the plane faces, and
    // not down into the material: that is what makes the rim concave. Each
    // cylinder face's parameter `y` runs along its own axis; the rim ring
    // sits at `y_rim`, and the finish needs `distance` of wall above it.
    let mut cylinder_step: BTreeMap<usize, f64> = BTreeMap::new();
    for face in &cylinders {
        let Surface::Cylinder(cylinder) = topology.faces[*face].value.surface else {
            unreachable!("checked above");
        };
        let axis_length = cylinder.axis.length();
        let step = (cylinder.axis.dot(normal) / axis_length).signum() * distance / axis_length;
        cylinder_step.insert(*face, step);
        let rim_y = (center - cylinder.origin).dot(cylinder.axis) / (axis_length * axis_length);
        let (mut lowest, mut highest) = (f64::INFINITY, f64::NEG_INFINITY);
        for loop_key in topology.faces[*face].value.loops() {
            for coedge in &topology.loops[loop_key.0].value.coedges {
                for point in topology.coedges[coedge.0].value.pcurve_endpoints() {
                    let along = (point.y - rim_y) * step.signum() * axis_length;
                    lowest = lowest.min(along);
                    highest = highest.max(along);
                }
            }
        }
        if !lowest.is_finite() || lowest < -agreement {
            // The wall continues past the plane: not a rim of it.
            return Err(ConcaveRimBlendError::DomainUnsupported);
        }
        if highest < distance + floor {
            return Err(ConcaveRimBlendError::DistanceInvalid);
        }
    }

    // Room on the plane. A boss's contact circle grows into the plane, so
    // every other loop of the plane must stay clear of it; a pocket's
    // shrinks, so everything inside the floor must stay inside it.
    let contact_radius = match rim_kind {
        RimKind::Boss => radius + distance,
        RimKind::Pocket => radius - distance,
    };
    if contact_radius < floor {
        return Err(ConcaveRimBlendError::DistanceInvalid);
    }
    for loop_key in topology.faces[wall].value.loops() {
        if loop_key == rim_loop {
            continue;
        }
        for coedge in &topology.loops[loop_key.0].value.coedges {
            let edge = topology.edges[topology.coedges[coedge.0].value.edge.0].value;
            let clear = match rim_kind {
                RimKind::Boss => distance_from_axis(edge, center, normal) >= contact_radius + floor,
                RimKind::Pocket => reach_from_axis(edge, center, normal) <= contact_radius - floor,
            };
            if !clear {
                return Err(ConcaveRimBlendError::DistanceInvalid);
            }
        }
    }

    // The band's frame: the rim's own, made right-handed about the plane's
    // normal. A torus written that way has its outward normal pointing away
    // from the ball centre, and a concave band faces the ball, so its
    // azimuth runs the other way (`angular_sign = −1`); the cone's sign is
    // whichever makes its normal face the air, which the two kinds of rim
    // disagree on. Each azimuth `x` of the band is `k·t` for the rim's own
    // parameter `t`.
    let handed = u.cross(v).dot(normal).signum();
    let radial_v = v * handed;
    let angular_sign = match (kind, rim_kind) {
        (EdgeFinishKind::Fillet, _) | (EdgeFinishKind::Chamfer, RimKind::Pocket) => -1.0,
        (EdgeFinishKind::Chamfer, RimKind::Boss) => 1.0,
    };
    let k = angular_sign * handed;
    let lift = center + normal * distance;
    // The torus's minor angle at the wall contact and at the plane contact.
    let (v_wall, v_plane) = match rim_kind {
        RimKind::Boss => (std::f64::consts::PI, 1.5 * std::f64::consts::PI),
        RimKind::Pocket => (0.0, -std::f64::consts::FRAC_PI_2),
    };

    let mut result = topology.clone();
    let mut next_id = next_entity_id(topology);
    let mut allocate = || {
        let id = EntityId::from_raw(next_id);
        next_id += 1;
        id
    };

    // Each old rim vertex rises to the wall contact on the cylinder; a new
    // vertex on the plane takes its place on the contact circle, and a seam
    // joins them: a quarter circle of the ball for a fillet, a slant for a
    // chamfer, stored from the wall contact to the plane contact.
    let mut plane_vertex: BTreeMap<usize, VertexKey> = BTreeMap::new();
    let mut seam_edge: BTreeMap<usize, EdgeKey> = BTreeMap::new();
    let mut angle_of: BTreeMap<usize, f64> = BTreeMap::new();
    for arc in &arcs {
        let edge = topology.edges[arc.edge.0].value;
        for (vertex, angle) in [
            (edge.vertices[0], arc.range.start),
            (edge.vertices[1], arc.range.end),
        ] {
            if plane_vertex.contains_key(&vertex.0) {
                continue;
            }
            angle_of.insert(vertex.0, angle);
            let radial = radial_at(angle);
            let on_plane = center + radial * contact_radius;
            let on_wall = lift + radial * radius;
            result.vertices[vertex.0].value.point = on_wall;
            let below = VertexKey(result.vertices.len());
            result.vertices.push(Record {
                id: allocate(),
                value: Vertex { point: on_plane },
            });
            plane_vertex.insert(vertex.0, below);
            let seam = EdgeKey(result.edges.len());
            let (curve, parameter_range) = match kind {
                EdgeFinishKind::Fillet => {
                    // The ball's meridian: centred on the tube's centre
                    // circle, its `u` pointing at the wall contact and its
                    // `v` down at the plane contact, a quarter turn apart.
                    let tube_center = lift + radial * contact_radius;
                    let toward_wall = match rim_kind {
                        RimKind::Boss => radial * -1.0,
                        RimKind::Pocket => radial,
                    };
                    (
                        Curve3::Circle {
                            center: tube_center,
                            u: toward_wall,
                            v: normal * -1.0,
                            radius: distance,
                        },
                        ParameterRange::new(0.0, std::f64::consts::FRAC_PI_2),
                    )
                }
                EdgeFinishKind::Chamfer => Curve3::line_segment([on_wall, on_plane]),
            };
            result.edges.push(Record {
                id: allocate(),
                value: Edge {
                    vertices: [vertex, below],
                    curve,
                    parameter_range,
                },
            });
            seam_edge.insert(vertex.0, seam);
        }
    }

    // Every generator that ran down to the rim now stops at the wall
    // contact, a height `distance` above the plane.
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
            let Some(owner) = topology.faces.iter().position(|face| {
                face.value.loops().any(|loop_key| {
                    topology.loops[loop_key.0]
                        .value
                        .coedges
                        .iter()
                        .any(|key| topology.coedges[key.0].id == coedge.id)
                })
            }) else {
                return Err(ConcaveRimBlendError::DomainUnsupported);
            };
            let (start_key, end_key) = match coedge.value.orientation {
                Orientation::Forward => (edge.value.vertices[0].0, edge.value.vertices[1].0),
                Orientation::Reverse => (edge.value.vertices[1].0, edge.value.vertices[0].0),
            };
            let [mut start, mut end] = coedge.value.pcurve_endpoints();
            if let Some(step) = cylinder_step.get(&owner) {
                if angle_of.contains_key(&start_key) {
                    start.y += step;
                }
                if angle_of.contains_key(&end_key) {
                    end.y += step;
                }
            } else if let Some(owner_plane) = topology.faces[owner].value.surface.as_plane() {
                // A generator shared with a flat face beside the cylinder: its
                // trace there is a shorter segment of the same line.
                if angle_of.contains_key(&start_key) {
                    start = owner_plane.project(result.vertices[start_key].value.point);
                }
                if angle_of.contains_key(&end_key) {
                    end = owner_plane.project(result.vertices[end_key].value.point);
                }
            } else {
                return Err(ConcaveRimBlendError::DomainUnsupported);
            }
            if !coedge.value.set_line_pcurve_endpoints([start, end]) {
                return Err(ConcaveRimBlendError::DomainUnsupported);
            }
        }
    }

    // The rim arcs themselves: each becomes two — the contact circle on the
    // plane and the raised ring on the cylinder — and the band between them.
    let mut band_faces = Vec::with_capacity(arcs.len());
    let ordinal = |index: usize| FaceRole::FeatureSide(u32::try_from(index).unwrap_or(u32::MAX));
    for (index, arc) in arcs.iter().enumerate() {
        let old = topology.edges[arc.edge.0].value;
        let [from, to] = [old.vertices[0].0, old.vertices[1].0];
        let plane_edge = EdgeKey(result.edges.len());
        result.edges.push(Record {
            id: allocate(),
            value: Edge {
                vertices: [plane_vertex[&from], plane_vertex[&to]],
                curve: Curve3::Circle {
                    center,
                    u,
                    v,
                    radius: contact_radius,
                },
                parameter_range: arc.range,
            },
        });
        // The old edge keeps its key and its vertices and rises to the wall
        // contact, so every coedge the cylinder had on it stays valid.
        result.edges[arc.edge.0].value.curve = Curve3::Circle {
            center: lift,
            u,
            v,
            radius,
        };
        let wall_edge = arc.edge;
        // The plane's coedge moves to the contact circle; the cylinder's
        // stays and its pcurve rises.
        for coedge in result.coedges.iter_mut() {
            if coedge.value.edge != arc.edge {
                continue;
            }
            let on_plane = topology.loops[rim_loop.0]
                .value
                .coedges
                .iter()
                .any(|key| topology.coedges[key.0].id == coedge.id);
            if on_plane {
                coedge.value.edge = plane_edge;
                match &mut coedge.value.pcurve {
                    Curve2::Circle { radius: grown, .. } => *grown = contact_radius,
                    _ => return Err(ConcaveRimBlendError::DomainUnsupported),
                }
            } else {
                let Some(owner) = cylinders.iter().find(|face| {
                    topology.faces[**face].value.loops().any(|loop_key| {
                        topology.loops[loop_key.0]
                            .value
                            .coedges
                            .iter()
                            .any(|key| topology.coedges[key.0].id == coedge.id)
                    })
                }) else {
                    return Err(ConcaveRimBlendError::DomainUnsupported);
                };
                let step = cylinder_step[owner];
                let [start, end] = coedge.value.pcurve_endpoints();
                if !coedge.value.set_line_pcurve_endpoints([
                    Point2::new(start.x, start.y + step),
                    Point2::new(end.x, end.y + step),
                ]) {
                    return Err(ConcaveRimBlendError::DomainUnsupported);
                }
            }
        }

        // The band over this arc, walked counter-clockwise in its own
        // parameters: along the lower ring in `+x`, up the seam at the far
        // end, back along the upper ring, and down the seam at the near end.
        // Which ring is lower depends on the kind of rim, and which vertex
        // is at the far end on which way the azimuth runs.
        let x_from = k * arc.range.start;
        let x_to = k * arc.range.end;
        let (lo, hi, lo_key, hi_key) = if x_from <= x_to {
            (x_from, x_to, from, to)
        } else {
            (x_to, x_from, to, from)
        };
        // Walking the arc from `lo_key` to `hi_key` runs its parameter from
        // the vertex at one end to the other; whether that is the edge's own
        // direction is the same question for both rings, which share the
        // rim's parameterization.
        let increasing_walk = if lo_key == from {
            Orientation::Forward
        } else {
            Orientation::Reverse
        };
        let decreasing_walk = increasing_walk.reversed();
        // A seam is stored from the wall contact to the plane contact.
        let wall_to_plane = Orientation::Forward;
        let plane_to_wall = Orientation::Reverse;
        let (surface, uses): (Surface, Vec<(EdgeKey, Orientation, [Point2; 2])>) = match kind {
            EdgeFinishKind::Fillet => {
                let torus = Surface::Torus(Torus {
                    origin: lift,
                    axis: normal,
                    radial_u: u,
                    radial_v,
                    major_radius: contact_radius,
                    minor_radius: distance,
                    angular_sign,
                });
                // A boss's band runs from the wall ring at `π` up to the
                // plane ring at `3π/2`; a pocket's from the plane ring at
                // `−π/2` up to the wall ring at `0`.
                let (lower_edge, upper_edge, rising, falling, v_lo, v_hi) = match rim_kind {
                    RimKind::Boss => (
                        wall_edge,
                        plane_edge,
                        wall_to_plane,
                        plane_to_wall,
                        v_wall,
                        v_plane,
                    ),
                    RimKind::Pocket => (
                        plane_edge,
                        wall_edge,
                        plane_to_wall,
                        wall_to_plane,
                        v_plane,
                        v_wall,
                    ),
                };
                (
                    torus,
                    vec![
                        (
                            lower_edge,
                            increasing_walk,
                            [Point2::new(lo, v_lo), Point2::new(hi, v_lo)],
                        ),
                        (
                            seam_edge[&hi_key],
                            rising,
                            [Point2::new(hi, v_lo), Point2::new(hi, v_hi)],
                        ),
                        (
                            upper_edge,
                            decreasing_walk,
                            [Point2::new(hi, v_hi), Point2::new(lo, v_hi)],
                        ),
                        (
                            seam_edge[&lo_key],
                            falling,
                            [Point2::new(lo, v_hi), Point2::new(lo, v_lo)],
                        ),
                    ],
                )
            }
            EdgeFinishKind::Chamfer => {
                // The cone stands on the plane: its ring is the contact circle
                // at `v = 0` and the wall's ring at `v = distance`.
                let cone = Surface::Cone(Cone {
                    origin: center,
                    axis: normal,
                    radial_u: u,
                    radial_v,
                    base_radius: contact_radius,
                    slope: (radius - contact_radius) / distance,
                    angular_sign,
                });
                (
                    cone,
                    vec![
                        (
                            plane_edge,
                            increasing_walk,
                            [Point2::new(lo, 0.0), Point2::new(hi, 0.0)],
                        ),
                        (
                            seam_edge[&hi_key],
                            plane_to_wall,
                            [Point2::new(hi, 0.0), Point2::new(hi, distance)],
                        ),
                        (
                            wall_edge,
                            decreasing_walk,
                            [Point2::new(hi, distance), Point2::new(lo, distance)],
                        ),
                        (
                            seam_edge[&lo_key],
                            wall_to_plane,
                            [Point2::new(lo, distance), Point2::new(lo, 0.0)],
                        ),
                    ],
                )
            }
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

    // The band belongs to the shell the plane does.
    let shell = result
        .shells
        .iter_mut()
        .find(|shell| shell.value.faces.contains(&FaceKey(wall)))
        .ok_or(ConcaveRimBlendError::DomainUnsupported)?;
    shell.value.faces.extend(band_faces);
    Ok(result)
}

/// The greatest distance from the axis to an edge lying in the plane,
/// measured square to the axis: the counterpart of the hole-rim blend's
/// nearest approach, for the loops a shrinking floor has to keep inside it.
///
/// A segment is farthest at an end. An arc in the plane is farthest along
/// the line from the axis through its centre when the arc reaches that
/// direction, and at an end otherwise. Anything else is sampled and each
/// bracket refined, which converges on the true farthest point.
fn reach_from_axis(edge: Edge, center: Point3, normal: Vector3) -> f64 {
    let across = |point: Point3| {
        let offset = point - center;
        offset - normal * offset.dot(normal)
    };
    let range = edge.parameter_range;
    match edge.curve {
        Curve3::Line { endpoints } => endpoints
            .into_iter()
            .map(|point| across(point).length())
            .fold(0.0_f64, f64::max),
        Curve3::Circle {
            center: arc_center,
            u,
            v,
            radius,
        } if u.dot(normal).abs() <= 1.0e-9 && v.dot(normal).abs() <= 1.0e-9 => {
            let ends = [range.start, range.end].map(|t| across(edge.curve.evaluate(t)).length());
            let farthest_end = ends[0].max(ends[1]);
            let away = across(arc_center);
            let reach = away.length();
            if reach <= f64::EPSILON {
                return radius.max(farthest_end);
            }
            let angle = away.dot(v).atan2(away.dot(u));
            let (low, high) = if range.start <= range.end {
                (range.start, range.end)
            } else {
                (range.end, range.start)
            };
            let on_arc =
                (angle - low).rem_euclid(std::f64::consts::TAU) <= high - low + f64::EPSILON;
            if on_arc {
                (reach + radius).max(farthest_end)
            } else {
                farthest_end
            }
        }
        curve => {
            let distance_at = |t: f64| across(curve.evaluate(t)).length();
            let samples = 256_u32;
            let step = (range.end - range.start) / f64::from(samples);
            let values = (0..=samples)
                .map(|index| distance_at(range.start + step * f64::from(index)))
                .collect::<Vec<_>>();
            let mut most = values.iter().copied().fold(0.0_f64, f64::max);
            for index in 0..=samples as usize {
                let before = index
                    .checked_sub(1)
                    .map_or(f64::NEG_INFINITY, |at| values[at]);
                let after = values.get(index + 1).copied().unwrap_or(f64::NEG_INFINITY);
                if values[index] < before || values[index] < after {
                    continue;
                }
                let mut low = range.start + step * (index as f64 - 1.0).max(0.0);
                let mut high = range.start + step * (index as f64 + 1.0).min(f64::from(samples));
                let ratio = (5.0_f64.sqrt() - 1.0) / 2.0;
                for _ in 0..80 {
                    let first = high - (high - low) * ratio;
                    let second = low + (high - low) * ratio;
                    if distance_at(first) >= distance_at(second) {
                        high = second;
                    } else {
                        low = first;
                    }
                }
                most = most.max(distance_at((low + high) / 2.0));
            }
            most
        }
    }
}

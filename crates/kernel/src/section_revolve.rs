//! General solids of revolution, and the rim blends that operate on them.
//!
//! A coaxial revolved solid is fully described by its closed (r, z) section:
//! a radial section line is a planar cap, an axial line is a cylinder, a
//! slanted line is a cone, and an arc is a torus. Reading that section back
//! out of committed topology and revolving it again turns every rim blend
//! into the same planar corner operation the prism paths use, which is what
//! makes blends stack — a chamfer's sharp rims can be filleted, and a
//! fillet's tangency rims are recognisably smooth and therefore refused.
//!
//! Full circles keep the two-semicircle representation of ADR 0016, with
//! seam vertices at azimuth `0` and `π`.

use artificer_protocol::{EdgeFinishKind, EntityKind, EntityRef, PrecisionPolicy, SnapshotId};

use crate::analytic_extrusion::{AnalyticLoop, Segment};
use crate::corner_blend::{CornerBlendError, corner_blend, segment_length};
use crate::topology::{
    Coedge, CoedgeKey, Cone, Curve2, Curve3, Cylinder, Edge, EdgeKey, EntityId, Face, FaceKey,
    FaceRole, Loop, LoopKey, Orientation, ParameterRange, Plane, Point2, Point3, Record, Shell,
    ShellKey, Solid, Sphere, Surface, Topology, Torus, Vector2, Vector3, Vertex, VertexKey,
};

const HALF_TURN: f64 = std::f64::consts::PI;
const FULL_TURN: f64 = std::f64::consts::TAU;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RimBlendError {
    TargetInvalid,
    DomainUnsupported,
    DistanceInvalid,
    /// The selected rim is tangent-continuous, so there is no corner to blend.
    SmoothRim,
}

/// A coaxial revolved solid as its (r, z) section.
///
/// The chain either closes through the axis — both ends sit at `r = 0`, and
/// the implicit axis segment joining them emits no face — or, for a tube,
/// closes on itself clear of the axis (`closed`).
#[derive(Debug)]
pub(crate) struct RzSection {
    center: Point3,
    axis: Vector3,
    radial_u: Vector3,
    radial_v: Vector3,
    segments: Vec<Segment>,
    roles: Vec<FaceRole>,
    /// True when the chain closes on itself clear of the axis — a section that
    /// sweeps a tube rather than a solid with a cap or pole on the axis. The
    /// last segment then meets the first ring instead of a new one.
    closed: bool,
}

impl RzSection {
    /// Builds a section directly, for callers that have one in hand rather
    /// than recovered from topology (the revolve command).
    pub(crate) const fn from_parts(
        center: Point3,
        axis: Vector3,
        radial_u: Vector3,
        radial_v: Vector3,
        segments: Vec<Segment>,
        roles: Vec<FaceRole>,
        closed: bool,
    ) -> Self {
        Self {
            center,
            axis,
            radial_u,
            radial_v,
            segments,
            roles,
            closed,
        }
    }
}

/// Recovers the section of any coaxial revolved solid built from planes,
/// cylinders, cones, and tori.
pub(crate) fn extract_rz_section(topology: &Topology) -> Result<RzSection, RimBlendError> {
    if topology.solids.len() != 1 {
        return Err(RimBlendError::DomainUnsupported);
    }
    let (axis, radial_u, radial_v, center) = section_frame(topology)?;
    let agreement = 1.0e-9 * section_scale(topology);

    // Each curved carrier appears as two half-faces; collect one section
    // segment per carrier and require the pairing to be exact.
    let mut pieces: Vec<(Segment, FaceRole, usize)> = Vec::new();
    for face in &topology.faces {
        let piece = match face.value.surface {
            Surface::Plane(plane) => {
                if plane.normal.cross(axis).length() > agreement {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let height = (plane.origin - center).dot(axis);
                let (inner, outer) = cap_radii(topology, &face.value)?;
                // A cap is a full disk from the axis outward, or an annulus
                // between two rims; its outward normal decides which way the
                // section travels.
                let outward = plane.normal.dot(axis);
                if outward >= 0.0 {
                    (
                        Segment::Line {
                            start: Point2::new(outer, height),
                            end: Point2::new(inner, height),
                        },
                        face.value.role,
                        1,
                    )
                } else {
                    (
                        Segment::Line {
                            start: Point2::new(inner, height),
                            end: Point2::new(outer, height),
                        },
                        face.value.role,
                        1,
                    )
                }
            }
            Surface::Cylinder(cylinder) => {
                if cylinder.axis.cross(axis).length() > agreement
                    || !on_axis(cylinder.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let (_, _, low, high) = parameter_bounds(topology, &face.value)?;
                let base = (cylinder.origin - center).dot(axis);
                (
                    Segment::Line {
                        start: Point2::new(cylinder.radius, base + low),
                        end: Point2::new(cylinder.radius, base + high),
                    },
                    face.value.role,
                    2,
                )
            }
            Surface::Cone(cone) => {
                if cone.axis.cross(axis).length() > agreement
                    || !on_axis(cone.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let (_, _, low, high) = parameter_bounds(topology, &face.value)?;
                let base = (cone.origin - center).dot(axis);
                (
                    Segment::Line {
                        start: Point2::new(cone.ring_radius(low), base + low),
                        end: Point2::new(cone.ring_radius(high), base + high),
                    },
                    face.value.role,
                    2,
                )
            }
            Surface::Sphere(sphere) => {
                if sphere.axis.cross(axis).length() > agreement
                    || !on_axis(sphere.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                // P(u, v) = origin + radial(u)·r·cos v + axis·r·sin v, so the
                // section is an arc of the same radius centred on the axis at
                // the sphere's own height. Closing this arm is what lets a
                // revolved sphere re-enter the blend ladder: a builder whose
                // output the extractor rejects would be a one-way door.
                let (_, _, low, high) = parameter_bounds(topology, &face.value)?;
                let center_height = (sphere.origin - center).dot(axis);
                // A concave band carries its axis against the section's, which
                // negates its minor angle with it. Reading the face's own
                // parameters back without that sign would mirror the arc in z.
                let sense = if sphere.axis.dot(axis) < 0.0 {
                    -1.0
                } else {
                    1.0
                };
                let point = |angle: f64| {
                    let angle = sense * angle;
                    Point2::new(
                        sphere.radius * angle.cos(),
                        center_height + sphere.radius * angle.sin(),
                    )
                };
                (
                    Segment::Arc {
                        center: Point2::new(0.0, center_height),
                        start: point(low),
                        end: point(high),
                        radius: sphere.radius,
                        start_angle: sense * low,
                        sweep: sense * (high - low),
                    },
                    face.value.role,
                    2,
                )
            }
            Surface::Torus(torus) => {
                if torus.axis.cross(axis).length() > agreement
                    || !on_axis(torus.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let (_, _, low, high) = parameter_bounds(topology, &face.value)?;
                let ring_height = (torus.origin - center).dot(axis);
                // As for a sphere: a band whose axis runs against the section's
                // measures its minor angle the other way.
                let sense = if torus.axis.dot(axis) < 0.0 {
                    -1.0
                } else {
                    1.0
                };
                let point = |angle: f64| {
                    let angle = sense * angle;
                    Point2::new(
                        torus.minor_radius.mul_add(angle.cos(), torus.major_radius),
                        ring_height + torus.minor_radius * angle.sin(),
                    )
                };
                (
                    Segment::Arc {
                        center: Point2::new(torus.major_radius, ring_height),
                        start: point(low),
                        end: point(high),
                        radius: torus.minor_radius,
                        start_angle: sense * low,
                        sweep: sense * (high - low),
                    },
                    face.value.role,
                    2,
                )
            }
        };
        pieces.push(piece);
    }

    // Deduplicate the half-face pairs: a carrier contributing two faces must
    // yield one section segment.
    let mut segments: Vec<(Segment, FaceRole)> = Vec::new();
    let mut seen = vec![false; pieces.len()];
    for index in 0..pieces.len() {
        if seen[index] {
            continue;
        }
        let (segment, role, expected) = pieces[index];
        let mut matches = 1;
        for other in index + 1..pieces.len() {
            if seen[other] {
                continue;
            }
            if segments_agree(segment, pieces[other].0, agreement) {
                seen[other] = true;
                matches += 1;
            }
        }
        seen[index] = true;
        if matches != expected {
            return Err(RimBlendError::DomainUnsupported);
        }
        segments.push((segment, role));
    }

    let (chained, closed) = chain_section(segments, agreement)?;
    let (segments, roles) = chained.into_iter().unzip();
    Ok(RzSection {
        center,
        axis,
        radial_u,
        radial_v,
        segments,
        roles,
        closed,
    })
}

fn section_frame(
    topology: &Topology,
) -> Result<(Vector3, Vector3, Vector3, Point3), RimBlendError> {
    for face in &topology.faces {
        let frame = match face.value.surface {
            Surface::Cylinder(cylinder) => Some((
                cylinder.axis,
                cylinder.radial_u,
                cylinder.radial_v,
                cylinder.origin,
            )),
            Surface::Cone(cone) => Some((cone.axis, cone.radial_u, cone.radial_v, cone.origin)),
            Surface::Torus(torus) => {
                Some((torus.axis, torus.radial_u, torus.radial_v, torus.origin))
            }
            Surface::Plane(_) | Surface::Sphere(_) => None,
        };
        if let Some((axis, radial_u, radial_v, origin)) = frame {
            // Anchor the section frame on the axis at the carrier's own
            // origin, so section heights are measured consistently.
            return Ok((axis, radial_u, radial_v, origin));
        }
    }
    Err(RimBlendError::DomainUnsupported)
}

fn section_scale(topology: &Topology) -> f64 {
    topology
        .vertices
        .iter()
        .map(|vertex| {
            vertex
                .value
                .point
                .x
                .abs()
                .max(vertex.value.point.y.abs())
                .max(vertex.value.point.z.abs())
        })
        .fold(1.0_f64, f64::max)
}

fn on_axis(point: Point3, center: Point3, axis: Vector3, agreement: f64) -> bool {
    let offset = point - center;
    (offset - axis * offset.dot(axis)).length() <= agreement
}

/// The `(inner, outer)` radii of a cap face: `(0, r)` for a full disk, and
/// the two rim radii for the washer face of a tube.
fn cap_radii(topology: &Topology, face: &Face) -> Result<(f64, f64), RimBlendError> {
    let outer = loop_circle_radius(topology, face.outer_loop)?;
    match face.inner_loops.as_slice() {
        [] => Ok((0.0, outer)),
        [hole] => {
            let inner = loop_circle_radius(topology, *hole)?;
            if inner >= outer {
                return Err(RimBlendError::DomainUnsupported);
            }
            Ok((inner, outer))
        }
        _ => Err(RimBlendError::DomainUnsupported),
    }
}

/// The radius of a loop made of one circle's coedges.
fn loop_circle_radius(topology: &Topology, loop_key: LoopKey) -> Result<f64, RimBlendError> {
    let loop_record = topology
        .loop_record(loop_key)
        .ok_or(RimBlendError::DomainUnsupported)?;
    let mut radius: Option<f64> = None;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology
            .coedge(*coedge_key)
            .ok_or(RimBlendError::DomainUnsupported)?
            .value;
        let Curve2::Circle { radius: r, .. } = coedge.pcurve else {
            return Err(RimBlendError::DomainUnsupported);
        };
        if radius.is_some_and(|existing: f64| (existing - r).abs() > 1.0e-9 * (1.0 + r.abs())) {
            return Err(RimBlendError::DomainUnsupported);
        }
        radius = Some(r);
    }
    radius.ok_or(RimBlendError::DomainUnsupported)
}

/// Parameter-space extent of a face's outer loop.
fn parameter_bounds(
    topology: &Topology,
    face: &Face,
) -> Result<(f64, f64, f64, f64), RimBlendError> {
    let loop_record = topology
        .loop_record(face.outer_loop)
        .ok_or(RimBlendError::DomainUnsupported)?;
    let mut u_min = f64::INFINITY;
    let mut u_max = f64::NEG_INFINITY;
    let mut v_min = f64::INFINITY;
    let mut v_max = f64::NEG_INFINITY;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology
            .coedge(*coedge_key)
            .ok_or(RimBlendError::DomainUnsupported)?
            .value;
        for point in coedge.pcurve_endpoints() {
            u_min = u_min.min(point.x);
            u_max = u_max.max(point.x);
            v_min = v_min.min(point.y);
            v_max = v_max.max(point.y);
        }
    }
    if !(u_min < u_max && v_min < v_max) {
        return Err(RimBlendError::DomainUnsupported);
    }
    Ok((u_min, u_max, v_min, v_max))
}

fn segments_agree(first: Segment, second: Segment, agreement: f64) -> bool {
    let same_point = |a: Point2, b: Point2| (a.x - b.x).hypot(a.y - b.y) <= agreement;
    match (first, second) {
        (
            Segment::Line {
                start: first_start,
                end: first_end,
            },
            Segment::Line {
                start: second_start,
                end: second_end,
            },
        ) => {
            (same_point(first_start, second_start) && same_point(first_end, second_end))
                || (same_point(first_start, second_end) && same_point(first_end, second_start))
        }
        (
            Segment::Arc {
                center: first_center,
                radius: first_radius,
                start: first_start,
                end: first_end,
                ..
            },
            Segment::Arc {
                center: second_center,
                radius: second_radius,
                start: second_start,
                end: second_end,
                ..
            },
        ) => {
            same_point(first_center, second_center)
                && (first_radius - second_radius).abs() <= agreement
                && ((same_point(first_start, second_start) && same_point(first_end, second_end))
                    || (same_point(first_start, second_end) && same_point(first_end, second_start)))
        }
        _ => false,
    }
}

/// Orders the section pieces into one chain, oriented counter-clockwise in
/// (r, z). A solid's chain runs from the axis, around the profile, and back
/// to the axis; a tube's touches the axis nowhere and closes on itself, which
/// the returned flag reports.
fn chain_section(
    mut pieces: Vec<(Segment, FaceRole)>,
    agreement: f64,
) -> Result<(Vec<(Segment, FaceRole)>, bool), RimBlendError> {
    if pieces.len() < 2 {
        return Err(RimBlendError::DomainUnsupported);
    }
    let same_point = |a: Point2, b: Point2| (a.x - b.x).hypot(a.y - b.y) <= agreement;
    let touches_axis = |segment: &Segment| {
        segment.start().x.abs() <= agreement || segment.end().x.abs() <= agreement
    };
    let closed = !pieces.iter().any(|(segment, _)| touches_axis(segment));

    // Start from the piece whose start lies on the axis; a tube may start
    // anywhere.
    let start_index = if closed {
        0
    } else {
        pieces
            .iter()
            .position(|(segment, _)| segment.start().x.abs() <= agreement)
            .ok_or(RimBlendError::DomainUnsupported)?
    };
    let mut chain = vec![pieces.remove(start_index)];
    while !pieces.is_empty() {
        let tail = chain.last().expect("chain is never empty").0.end();
        if !closed && tail.x.abs() <= agreement {
            break;
        }
        let next = pieces
            .iter()
            .position(|(segment, _)| same_point(segment.start(), tail))
            .or_else(|| {
                pieces
                    .iter()
                    .position(|(segment, _)| same_point(segment.end(), tail))
            })
            .ok_or(RimBlendError::DomainUnsupported)?;
        let (segment, role) = pieces.remove(next);
        let oriented = if same_point(segment.start(), tail) {
            segment
        } else {
            reversed(segment)
        };
        chain.push((oriented, role));
    }
    if !pieces.is_empty() {
        return Err(RimBlendError::DomainUnsupported);
    }
    let tail = chain.last().expect("chain is never empty").0.end();
    let head = chain.first().expect("chain is never empty").0.start();
    if closed {
        if !same_point(tail, head) {
            return Err(RimBlendError::DomainUnsupported);
        }
    } else if tail.x.abs() > agreement {
        return Err(RimBlendError::DomainUnsupported);
    }
    // Orient counter-clockwise: the closed section (through the axis, or on
    // itself) must have positive signed area with r as x and z as y.
    if section_signed_area(&chain) < 0.0 {
        chain.reverse();
        for entry in &mut chain {
            entry.0 = reversed(entry.0);
        }
    }
    Ok((chain, closed))
}

fn reversed(segment: Segment) -> Segment {
    match segment {
        Segment::Line { start, end } => Segment::Line {
            start: end,
            end: start,
        },
        Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep,
        } => Segment::Arc {
            center,
            start: end,
            end: start,
            radius,
            start_angle: start_angle + sweep,
            sweep: -sweep,
        },
        other @ (Segment::Ellipse { .. } | Segment::Harmonic { .. }) => other.reversed(),
    }
}

fn section_signed_area(chain: &[(Segment, FaceRole)]) -> f64 {
    let mut area = 0.0;
    for (segment, _) in chain {
        let start = segment.start();
        let end = segment.end();
        area += start.x.mul_add(end.y, -(start.y * end.x)) / 2.0;
        if let Segment::Arc { radius, sweep, .. } = *segment {
            area += 0.5 * radius * radius * (sweep - sweep.sin());
        }
    }
    // Close through the axis.
    if let (Some((first, _)), Some((last, _))) = (chain.first(), chain.last()) {
        let start = last.end();
        let end = first.start();
        area += start.x.mul_add(end.y, -(start.y * end.x)) / 2.0;
    }
    area
}

// ---------------------------------------------------------------------------
// Rim blends
// ---------------------------------------------------------------------------

/// Blends one or more full circular rims of a coaxial revolved solid.
pub(crate) fn build_rim_blend(
    snapshot: SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, RimBlendError> {
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(RimBlendError::TargetInvalid);
    }
    if !distance.is_finite() || distance < precision.min_feature_size {
        return Err(RimBlendError::DistanceInvalid);
    }
    let section = extract_rz_section(topology)?;
    let agreement = 1.0e-9 * section_scale(topology);

    // Every target resolves to a section vertex: the junction between two
    // consecutive section segments at radius r > 0.
    let mut vertices = Vec::new();
    for target in targets {
        let edge = topology
            .edges
            .iter()
            .find(|edge| edge.id.get() == target.entity.0)
            .ok_or(RimBlendError::TargetInvalid)?;
        let Curve3::Circle { center, radius, .. } = edge.value.curve else {
            return Err(RimBlendError::DomainUnsupported);
        };
        if !on_axis(center, section.center, section.axis, agreement) {
            return Err(RimBlendError::DomainUnsupported);
        }
        let height = (center - section.center).dot(section.axis);
        let located = section
            .segments
            .iter()
            .position(|segment| {
                (segment.start().x - radius).abs() <= agreement
                    && (segment.start().y - height).abs() <= agreement
            })
            .ok_or(RimBlendError::DomainUnsupported)?;
        if located == 0 && !section.closed {
            // The first section vertex sits on the axis, not on a rim.
            return Err(RimBlendError::DomainUnsupported);
        }
        vertices.push(located);
    }
    vertices.sort_unstable();
    vertices.dedup();

    let blended = blend_section(&section, &vertices, kind, distance, precision)?;
    Ok(build_revolved_topology(&RzSection {
        center: section.center,
        axis: section.axis,
        radial_u: section.radial_u,
        radial_v: section.radial_v,
        roles: blended.1,
        segments: blended.0,
        closed: section.closed,
    }))
}

type BlendedSection = (Vec<Segment>, Vec<FaceRole>);

fn blend_section(
    section: &RzSection,
    vertices: &[usize],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<BlendedSection, RimBlendError> {
    // The section is a closed loop in (r, z) with material to the left of
    // travel; the probe therefore answers "inside the revolved body".
    let closed = closed_section_loop(section);
    let analytic = [AnalyticLoop {
        signed_area: loop_signed_area(&closed),
        segments: closed.clone(),
    }];
    let probe = |point: Point2| crate::analytic_extrusion::point_in_material(point, &analytic);

    let count = section.segments.len();
    let mut new_start = vec![None; count];
    let mut new_end = vec![None; count];
    let mut consumed = vec![false; count];
    let mut connectors: Vec<(usize, Segment)> = Vec::with_capacity(vertices.len());
    for vertex in vertices {
        if (*vertex == 0 && !section.closed) || *vertex >= count {
            return Err(RimBlendError::DomainUnsupported);
        }
        // A tube's chain is cyclic: the rim at its first vertex is the corner
        // between the last segment and the first.
        let incoming_index = if *vertex == 0 { count - 1 } else { *vertex - 1 };
        let incoming = section.segments[incoming_index];
        let outgoing = section.segments[*vertex];
        let blend = corner_blend(incoming, outgoing, kind, distance, &probe, precision)
            .map_err(map_corner_error)?;
        new_end[incoming_index] = Some(blend.trimmed_incoming.end());
        new_start[*vertex] = Some(blend.trimmed_outgoing.start());
        consumed[incoming_index] |= blend.consumed.incoming;
        consumed[*vertex] |= blend.consumed.outgoing;
        connectors.push((*vertex, blend.connector));
    }

    let mut segments = Vec::with_capacity(count + connectors.len());
    let mut roles = Vec::with_capacity(count + connectors.len());
    // A consumed piece leaves the section, so the surviving pieces renumber.
    // `surviving` maps an original index to the position a connector placed
    // before it must take.
    let mut surviving = vec![0_usize; count + 1];
    for (index, segment) in section.segments.iter().enumerate() {
        surviving[index] = segments.len();
        let mut current = *segment;
        if let Some(start) = new_start[index] {
            current =
                crate::corner_blend::retarget_start(current, start).map_err(map_corner_error)?;
        }
        if let Some(end) = new_end[index] {
            current = crate::corner_blend::retarget_end(current, end).map_err(map_corner_error)?;
        }
        if segment_length(current) < precision.min_feature_size {
            // Legitimate when a blend ate the piece outright, and equally when
            // two blends met in its middle — filleting both rims of a cylinder
            // at its own radius trims the wall from each end onto one point.
            let met_in_the_middle = new_start[index]
                .zip(new_end[index])
                .is_some_and(|(a, b)| (a.x - b.x).hypot(a.y - b.y) <= precision.min_feature_size);
            if !consumed[index] && !met_in_the_middle {
                return Err(RimBlendError::DistanceInvalid);
            }
            continue;
        }
        segments.push(current);
        roles.push(section.roles[index]);
    }
    surviving[count] = segments.len();
    if segments.is_empty() && connectors.len() < 2 {
        return Err(RimBlendError::DistanceInvalid);
    }
    connectors.sort_by_key(|(vertex, _)| std::cmp::Reverse(*vertex));
    for (blend_ordinal, (vertex, connector)) in connectors.into_iter().enumerate() {
        let at = surviving[vertex].min(segments.len());
        segments.insert(at, connector);
        roles.insert(
            at,
            FaceRole::FeatureSide(u32::try_from(blend_ordinal).unwrap_or(u32::MAX)),
        );
    }
    // No section point may cross the axis.
    if segments
        .iter()
        .any(|segment| segment.start().x < -precision.min_feature_size)
    {
        return Err(RimBlendError::DistanceInvalid);
    }
    Ok((segments, roles))
}

/// The section closed through the axis, for material queries. A tube's
/// section already meets itself, so it gains no closing chord.
fn closed_section_loop(section: &RzSection) -> Vec<Segment> {
    let mut closed = section.segments.clone();
    if let (Some(first), Some(last)) = (section.segments.first(), section.segments.last()) {
        let start = last.end();
        let end = first.start();
        if (start.x - end.x).hypot(start.y - end.y) > axis_agreement(section) {
            closed.push(Segment::Line { start, end });
        }
    }
    closed
}

fn loop_signed_area(segments: &[Segment]) -> f64 {
    let Some(anchor) = segments.first().map(|segment| segment.start()) else {
        return 0.0;
    };
    segments
        .iter()
        .map(|segment| segment.translated(anchor).signed_area_contribution())
        .sum()
}

const fn map_corner_error(error: CornerBlendError) -> RimBlendError {
    match error {
        CornerBlendError::NoCorner => RimBlendError::SmoothRim,
        CornerBlendError::TrimTooLarge => RimBlendError::DistanceInvalid,
        CornerBlendError::NoSolution | CornerBlendError::Ambiguous => {
            RimBlendError::DomainUnsupported
        }
    }
}

// ---------------------------------------------------------------------------
// Revolving a section
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct RimCircle {
    vertices: [VertexKey; 2],
    edges: [EdgeKey; 2],
}

/// Where a section curve terminates on the axis, the ring it sweeps has zero
/// radius. The face still needs a fourth side to close in parameter space, so
/// one degenerate edge stands in for the whole singular iso-line — the
/// pole-closure vocabulary the validator already certifies. Both half-patches
/// share that one edge with opposite senses, so the edge-use family stays
/// exact without a pole exemption.
#[derive(Clone, Copy)]
struct Pole {
    vertex: VertexKey,
    edge: EdgeKey,
}

/// The ring a section vertex sweeps: a real circle, or a pole on the axis.
#[derive(Clone, Copy)]
enum Ring {
    Circle(RimCircle),
    Pole(Pole),
}

impl Ring {
    const fn as_circle(self) -> Option<RimCircle> {
        match self {
            Self::Circle(circle) => Some(circle),
            Self::Pole(_) => None,
        }
    }

    const fn vertex(self, half: usize) -> VertexKey {
        match self {
            Self::Circle(circle) => circle.vertices[half],
            Self::Pole(pole) => pole.vertex,
        }
    }
}

struct Builder<'a> {
    topology: Topology,
    next_id: u64,
    section: &'a RzSection,
}

impl Builder<'_> {
    fn point(&self, radius: f64, azimuth: f64, height: f64) -> Point3 {
        self.section.center
            + self.section.radial_u * (radius * azimuth.cos())
            + self.section.radial_v * (radius * azimuth.sin())
            + self.section.axis * height
    }

    fn allocate(&mut self) -> EntityId {
        let id = EntityId::from_raw(self.next_id);
        self.next_id += 1;
        id
    }

    fn vertex(&mut self, point: Point3) -> VertexKey {
        let key = VertexKey(self.topology.vertices.len());
        let id = self.allocate();
        self.topology.vertices.push(Record {
            id,
            value: Vertex { point },
        });
        key
    }

    fn edge(&mut self, edge: Edge) -> EdgeKey {
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record { id, value: edge });
        key
    }

    /// One full circle as two exact semicircle edges.
    fn rim_circle(&mut self, radius: f64, height: f64) -> RimCircle {
        let near = self.vertex(self.point(radius, 0.0, height));
        let far = self.vertex(self.point(radius, HALF_TURN, height));
        let curve = Curve3::Circle {
            center: self.section.center + self.section.axis * height,
            u: self.section.radial_u,
            v: self.section.radial_v,
            radius,
        };
        let first = self.edge(Edge {
            vertices: [near, far],
            curve,
            parameter_range: ParameterRange::new(0.0, HALF_TURN),
        });
        let second = self.edge(Edge {
            vertices: [far, near],
            curve,
            parameter_range: ParameterRange::new(HALF_TURN, FULL_TURN),
        });
        RimCircle {
            vertices: [near, far],
            edges: [first, second],
        }
    }

    /// The ring a section vertex sweeps. A point on the axis is a pole only
    /// when a curve meets it; a radial line ending on the axis closes a full
    /// disk cap and sweeps no ring at all.
    fn ring_at(&mut self, point: Point2, curved: bool) -> Option<Ring> {
        if point.x > 0.0 {
            Some(Ring::Circle(self.rim_circle(point.x, point.y)))
        } else if curved {
            Some(Ring::Pole(self.pole(point.y)))
        } else {
            None
        }
    }

    /// The degenerate ring on the axis at `height`.
    fn pole(&mut self, height: f64) -> Pole {
        let point = self.point(0.0, 0.0, height);
        let vertex = self.vertex(point);
        let edge = self.edge(Edge {
            vertices: [vertex, vertex],
            curve: Curve3::Line {
                endpoints: [point, point],
            },
            parameter_range: ParameterRange::new(0.0, 1.0),
        });
        Pole { vertex, edge }
    }

    /// The seam generator of an arc section segment, with either end free to
    /// be a pole where every azimuth converges on the one pole vertex.
    #[allow(clippy::too_many_arguments)]
    fn seam_minor_arc_ring(
        &mut self,
        low: Ring,
        high: Ring,
        half: usize,
        azimuth: f64,
        arc_center: Point2,
        radius: f64,
        angles: (f64, f64),
    ) -> EdgeKey {
        let radial = self.section.radial_u * azimuth.cos() + self.section.radial_v * azimuth.sin();
        let center = self.section.center + radial * arc_center.x + self.section.axis * arc_center.y;
        self.edge(Edge {
            vertices: [low.vertex(half), high.vertex(half)],
            curve: Curve3::Circle {
                center,
                u: radial,
                v: self.section.axis,
                radius,
            },
            parameter_range: ParameterRange::new(angles.0, angles.1),
        })
    }

    /// The seam generator of a line section segment: a straight edge in the
    /// azimuth plane joining the two rings.
    fn seam_line(&mut self, from: (&RimCircle, usize), to: (&RimCircle, usize)) -> EdgeKey {
        let start = self.topology.vertices[from.0.vertices[from.1].0]
            .value
            .point;
        let end = self.topology.vertices[to.0.vertices[to.1].0].value.point;
        self.edge(Edge::line(
            [from.0.vertices[from.1], to.0.vertices[to.1]],
            [start, end],
        ))
    }

    fn push_loop(&mut self, uses: Vec<(EdgeKey, Orientation, Curve2, ParameterRange)>) -> LoopKey {
        let mut coedges = Vec::with_capacity(uses.len());
        for (edge, orientation, pcurve, range) in uses {
            let key = CoedgeKey(self.topology.coedges.len());
            let id = self.allocate();
            self.topology.coedges.push(Record {
                id,
                value: Coedge {
                    edge,
                    orientation,
                    pcurve,
                    parameter_range: range,
                },
            });
            coedges.push(key);
        }
        let key = LoopKey(self.topology.loops.len());
        let id = self.allocate();
        self.topology.loops.push(Record {
            id,
            value: Loop { coedges },
        });
        key
    }

    fn push_face(&mut self, surface: Surface, outer_loop: LoopKey, role: FaceRole) {
        self.push_face_with_holes(surface, outer_loop, Vec::new(), role);
    }

    fn push_face_with_holes(
        &mut self,
        surface: Surface,
        outer_loop: LoopKey,
        inner_loops: Vec<LoopKey>,
        role: FaceRole,
    ) {
        let id = self.allocate();
        self.topology.faces.push(Record {
            id,
            value: Face {
                surface,
                outer_loop,
                inner_loops,
                role,
            },
        });
    }
}

fn line_pcurve(start: Point2, end: Point2) -> (Curve2, ParameterRange) {
    Curve2::line_segment([start, end])
}

fn cap_circle_pcurve(
    radius: f64,
    half: usize,
    reverse: bool,
    mirrored: bool,
) -> (Curve2, ParameterRange) {
    let (u, v) = if mirrored {
        (Vector2::new(0.0, 1.0), Vector2::new(1.0, 0.0))
    } else {
        (Vector2::new(1.0, 0.0), Vector2::new(0.0, 1.0))
    };
    let start = if half == 0 { 0.0 } else { HALF_TURN };
    let range = ParameterRange::new(start, start + HALF_TURN);
    (
        Curve2::Circle {
            center: Point2::new(0.0, 0.0),
            u,
            v,
            radius,
        },
        if reverse { range.reversed() } else { range },
    )
}

/// Revolves a closed (r, z) section a full turn.
pub(crate) fn build_revolved_topology(section: &RzSection) -> Topology {
    let mut builder = Builder {
        topology: Topology::default(),
        next_id: 1,
        section,
    };
    let count = section.segments.len();

    // One circle per section vertex with r > 0. Vertex `index` is the start of
    // segment `index`; the final vertex is the end of the last segment.
    let mut circles: Vec<Option<Ring>> = Vec::with_capacity(count + 1);
    for segment in &section.segments {
        circles.push(builder.ring_at(segment.start(), matches!(segment, Segment::Arc { .. })));
    }
    if section.closed {
        // A tube's chain returns to where it started, so the final ring is the
        // first one. Sweeping a second ring there would leave two coincident
        // circles and a shell that never closes.
        circles.push(circles[0]);
    } else {
        let last = section.segments[count - 1];
        circles.push(builder.ring_at(last.end(), matches!(last, Segment::Arc { .. })));
    }

    for (index, segment) in section.segments.iter().enumerate() {
        let role = section.roles[index];
        let start = segment.start();
        let end = segment.end();
        match *segment {
            Segment::Line { .. } if start.x <= 0.0 || end.x <= 0.0 => {
                // A radial line touching the axis is a full-disk cap.
                let (circle, radius, height, outward_up) = if start.x <= 0.0 {
                    (
                        circles[index + 1].and_then(Ring::as_circle),
                        end.x,
                        end.y,
                        // Travelling outward means material is below.
                        false,
                    )
                } else {
                    (
                        circles[index].and_then(Ring::as_circle),
                        start.x,
                        start.y,
                        true,
                    )
                };
                let Some(circle) = circle else { continue };
                let uses = if outward_up {
                    vec![
                        {
                            let (pcurve, range) = cap_circle_pcurve(radius, 0, false, false);
                            (circle.edges[0], Orientation::Forward, pcurve, range)
                        },
                        {
                            let (pcurve, range) = cap_circle_pcurve(radius, 1, false, false);
                            (circle.edges[1], Orientation::Forward, pcurve, range)
                        },
                    ]
                } else {
                    vec![
                        {
                            let (pcurve, range) = cap_circle_pcurve(radius, 0, true, true);
                            (circle.edges[0], Orientation::Reverse, pcurve, range)
                        },
                        {
                            let (pcurve, range) = cap_circle_pcurve(radius, 1, true, true);
                            (circle.edges[1], Orientation::Reverse, pcurve, range)
                        },
                    ]
                };
                let loop_key = builder.push_loop(uses);
                let plane = if outward_up {
                    Plane::new(
                        section.center + section.axis * height,
                        section.radial_u,
                        section.radial_v,
                    )
                } else {
                    Plane::new(
                        section.center + section.axis * height,
                        section.radial_v,
                        section.radial_u,
                    )
                };
                builder.push_face(Surface::Plane(plane), loop_key, role);
            }
            Segment::Line { .. }
                if (end.y - start.y).abs() <= axis_agreement(section)
                    && start.x > 0.0
                    && end.x > 0.0 =>
            {
                // A radial line clear of the axis sweeps a planar annulus: the
                // washer face of every tube, and the ledge of every stepped
                // shaft. Travelling outward puts material below it, exactly as
                // for a full-disk cap.
                let (Some(inner), Some(outer), outward_up) = (
                    circles[if start.x < end.x { index } else { index + 1 }]
                        .and_then(Ring::as_circle),
                    circles[if start.x < end.x { index + 1 } else { index }]
                        .and_then(Ring::as_circle),
                    start.x > end.x,
                ) else {
                    continue;
                };
                let (inner_radius, outer_radius) = (start.x.min(end.x), start.x.max(end.x));
                let height = start.y;
                let boundary = |circle: &RimCircle, radius: f64, hole: bool| {
                    let reverse = outward_up == hole;
                    let orientation = if reverse {
                        Orientation::Reverse
                    } else {
                        Orientation::Forward
                    };
                    (0..2)
                        .map(|half| {
                            let (pcurve, range) =
                                cap_circle_pcurve(radius, half, reverse, !outward_up);
                            (circle.edges[half], orientation, pcurve, range)
                        })
                        .collect::<Vec<_>>()
                };
                let outer_loop = builder.push_loop(boundary(&outer, outer_radius, false));
                let inner_loop = builder.push_loop(boundary(&inner, inner_radius, true));
                let plane = if outward_up {
                    Plane::new(
                        section.center + section.axis * height,
                        section.radial_u,
                        section.radial_v,
                    )
                } else {
                    Plane::new(
                        section.center + section.axis * height,
                        section.radial_v,
                        section.radial_u,
                    )
                };
                builder.push_face_with_holes(
                    Surface::Plane(plane),
                    outer_loop,
                    vec![inner_loop],
                    role,
                );
            }
            Segment::Line { .. } => {
                // A slanted line reaching the axis would sweep a cone apex,
                // which is a sharp singularity rather than a pole; that stays
                // outside the certified domain.
                // A section travelling down the page has material on the other
                // side of the band: it is the bore of a tube or the inside of
                // a cup, not an outside wall. The band is built from its lower
                // end either way, so the parameter height always increases,
                // and the descending case then reverses the face — for a
                // cylinder or a cone that is the angular sign alone, because
                // the validator holds their frames right-handed.
                let descending = end.y < start.y;
                let (base, top) = if descending {
                    (end, start)
                } else {
                    (start, end)
                };
                let (Some(low), Some(high)) = (
                    circles[if descending { index + 1 } else { index }].and_then(Ring::as_circle),
                    circles[if descending { index } else { index + 1 }].and_then(Ring::as_circle),
                ) else {
                    continue;
                };
                let seams = [
                    builder.seam_line((&low, 0), (&high, 0)),
                    builder.seam_line((&low, 1), (&high, 1)),
                ];
                let angular_sign = if descending { -1.0 } else { 1.0 };
                let slope = (top.x - base.x) / (top.y - base.y);
                let surface = if slope.abs() <= f64::EPSILON {
                    Surface::Cylinder(Cylinder {
                        origin: section.center + section.axis * base.y,
                        axis: section.axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        radius: base.x,
                        angular_sign,
                    })
                } else {
                    Surface::Cone(Cone {
                        origin: section.center + section.axis * base.y,
                        axis: section.axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        base_radius: base.x,
                        slope,
                        angular_sign,
                    })
                };
                push_band(
                    &mut builder,
                    surface,
                    Ring::Circle(low),
                    Ring::Circle(high),
                    seams,
                    (0.0, top.y - base.y),
                    role,
                    descending,
                );
            }
            Segment::Arc {
                center: arc_center,
                radius,
                start_angle,
                sweep,
                ..
            } => {
                let (Some(low), Some(high)) = (circles[index], circles[index + 1]) else {
                    continue;
                };
                // An arc swept the other way round is the concave case: the
                // band's material is on the far side. A torus and a sphere
                // reverse through a flipped axis rather than through the
                // angular sign alone, which negates the minor angle with it —
                // so the face's own parameters are the negated ones while the
                // seam edge keeps the section's.
                let reversed = sweep < 0.0;
                let sense = if reversed { -1.0 } else { 1.0 };
                let seam_angles = (start_angle, start_angle + sweep);
                let angles = (sense * seam_angles.0, sense * seam_angles.1);
                let seams = [
                    builder.seam_minor_arc_ring(low, high, 0, 0.0, arc_center, radius, seam_angles),
                    builder.seam_minor_arc_ring(
                        low,
                        high,
                        1,
                        HALF_TURN,
                        arc_center,
                        radius,
                        seam_angles,
                    ),
                ];
                // An arc centred on the axis sweeps a sphere. Emitting a torus
                // of zero major radius instead would be a carrier whose
                // parameterization collapses onto its own spine.
                let origin = section.center + section.axis * arc_center.y;
                let axis = section.axis * sense;
                let surface = if arc_center.x <= axis_agreement(section) {
                    Surface::Sphere(Sphere {
                        origin,
                        axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        radius,
                        angular_sign: sense,
                    })
                } else {
                    Surface::Torus(Torus {
                        origin,
                        axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        major_radius: arc_center.x,
                        minor_radius: radius,
                        angular_sign: sense,
                    })
                };
                push_band(
                    &mut builder,
                    surface,
                    low,
                    high,
                    seams,
                    angles,
                    role,
                    reversed,
                );
            }
            Segment::Ellipse { .. } | Segment::Harmonic { .. } => {
                unreachable!("revolved sections carry lines and arcs only")
            }
        }
    }

    let shell_key = ShellKey(builder.topology.shells.len());
    let shell_id = builder.allocate();
    let face_count = builder.topology.faces.len();
    builder.topology.shells.push(Record {
        id: shell_id,
        value: Shell {
            faces: (0..face_count).map(FaceKey).collect(),
        },
    });
    let solid_id = builder.allocate();
    builder.topology.solids.push(Record {
        id: solid_id,
        value: Solid {
            outer_shell: shell_key,
            inner_shells: Vec::new(),
        },
    });
    builder.topology
}

/// Opposite senses for the two halves sharing one degenerate pole edge.
const fn half_sense(half: usize) -> Orientation {
    if half == 0 {
        Orientation::Forward
    } else {
        Orientation::Reverse
    }
}

/// Scale-relative agreement for deciding a section point sits on the axis.
fn axis_agreement(section: &RzSection) -> f64 {
    let extent = section
        .segments
        .iter()
        .map(|segment| segment.start().x.abs().max(segment.start().y.abs()))
        .fold(1.0_f64, f64::max);
    1.0e-9 * extent
}

/// Emits the two half-faces of one revolved section segment.
///
/// `reversed` marks a band whose material lies on the far side — a bore, a cup
/// wall, a concave blend. Its carrier is already parameterized the other way
/// round in azimuth, so each half covers the opposite rim edge, traversed the
/// opposite way; everything else about the loop is unchanged.
#[allow(clippy::too_many_arguments)]
fn push_band(
    builder: &mut Builder<'_>,
    surface: Surface,
    low: Ring,
    high: Ring,
    seams: [EdgeKey; 2],
    parameters: (f64, f64),
    role: FaceRole,
    reversed: bool,
) {
    let (v_low, v_high) = parameters;
    for half in 0..2 {
        let (u0, u1) = if half == 0 {
            (0.0, HALF_TURN)
        } else {
            (HALF_TURN, FULL_TURN)
        };
        let (seam_up, seam_down) = if half == 0 {
            (seams[1], seams[0])
        } else {
            (seams[0], seams[1])
        };
        let rim = if reversed { 1 - half } else { half };
        let (forward, backward) = if reversed {
            (Orientation::Reverse, Orientation::Forward)
        } else {
            (Orientation::Forward, Orientation::Reverse)
        };
        // A pole contributes the singular iso-line itself. Its one degenerate
        // edge is shared by both halves, so they must traverse it in opposite
        // senses for the edge-use family to stay exact.
        let (low_edge, low_sense) = match low {
            Ring::Circle(circle) => (circle.edges[rim], forward),
            Ring::Pole(pole) => (pole.edge, half_sense(rim)),
        };
        let (high_edge, high_sense) = match high {
            Ring::Circle(circle) => (circle.edges[rim], backward),
            Ring::Pole(pole) => (pole.edge, half_sense(rim).reversed()),
        };
        let uses = vec![
            {
                let (pcurve, range) = line_pcurve(Point2::new(u0, v_low), Point2::new(u1, v_low));
                (low_edge, low_sense, pcurve, range)
            },
            {
                let (pcurve, range) = line_pcurve(Point2::new(u1, v_low), Point2::new(u1, v_high));
                (seam_up, Orientation::Forward, pcurve, range)
            },
            {
                let (pcurve, range) = line_pcurve(Point2::new(u1, v_high), Point2::new(u0, v_high));
                (high_edge, high_sense, pcurve, range)
            },
            {
                let (pcurve, range) = line_pcurve(Point2::new(u0, v_high), Point2::new(u0, v_low));
                (seam_down, Orientation::Reverse, pcurve, range)
            },
        ];
        let loop_key = builder.push_loop(uses);
        builder.push_face(surface, loop_key, role);
    }
}

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
//! seam vertices at azimuth `0` and `π`. A partial turn (ADR 0055) splits each
//! carrier halfway round instead, and is closed by two planar wedge faces;
//! [`extract_rz_section`] refuses those, so a partial revolve does not enter
//! the rim-blend or shell readings of a section.

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
    /// How far the section turns from azimuth zero, `radial_u`: a full
    /// turn, or less for a partial revolve read back from its topology.
    sweep: f64,
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
            sweep: FULL_TURN,
        }
    }
}

impl RzSection {
    /// The point on the axis that section heights are measured from.
    pub(crate) const fn center(&self) -> Point3 {
        self.center
    }

    /// The unit axis the section turns about.
    pub(crate) const fn axis(&self) -> Vector3 {
        self.axis
    }

    /// The radial direction the section's `r` runs along.
    pub(crate) const fn radial_u(&self) -> Vector3 {
        self.radial_u
    }

    /// The direction the turn runs towards from `radial_u`.
    pub(crate) const fn radial_v(&self) -> Vector3 {
        self.radial_v
    }

    /// The section chain, in order.
    pub(crate) fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Whether the chain closes on itself clear of the axis. A chain that
    /// does not closes through the axis instead, and its first and last
    /// points both sit on it.
    pub(crate) const fn is_closed(&self) -> bool {
        self.closed
    }

    /// How far the section turns: a full turn, or less.
    pub(crate) const fn sweep(&self) -> f64 {
        self.sweep
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
    // Each piece carries the azimuth it spans, for a curved band, so that a
    // partial turn's sweep can be read back from its carriers.
    let mut pieces: Vec<(Segment, FaceRole, usize, f64)> = Vec::new();
    // Each wedge face as the direction its half-plane leaves the axis in,
    // and its outward normal.
    let mut wedges: Vec<(Vector3, Vector3)> = Vec::new();
    for face in &topology.faces {
        let piece = match face.value.surface {
            // A plane holding the axis is one of the two wedge faces that
            // close a partial turn: the section itself, not a piece of it.
            Surface::Plane(plane)
                if plane.normal.dot(axis).abs() <= agreement
                    && on_axis(plane.origin, center, axis, agreement) =>
            {
                let direction = wedge_direction(topology, &face.value, center, axis)
                    .ok_or(RimBlendError::DomainUnsupported)?;
                wedges.push((direction, plane.normal));
                continue;
            }
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
                        0.0,
                    )
                } else {
                    (
                        Segment::Line {
                            start: Point2::new(inner, height),
                            end: Point2::new(outer, height),
                        },
                        face.value.role,
                        1,
                        0.0,
                    )
                }
            }
            Surface::Cylinder(cylinder) => {
                if cylinder.axis.cross(axis).length() > agreement
                    || !on_axis(cylinder.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let (u_low, u_high, low, high) = parameter_bounds(topology, &face.value)?;
                let base = (cylinder.origin - center).dot(axis);
                (
                    Segment::Line {
                        start: Point2::new(cylinder.radius, base + low),
                        end: Point2::new(cylinder.radius, base + high),
                    },
                    face.value.role,
                    2,
                    u_high - u_low,
                )
            }
            Surface::Cone(cone) => {
                if cone.axis.cross(axis).length() > agreement
                    || !on_axis(cone.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let (u_low, u_high, low, high) = parameter_bounds(topology, &face.value)?;
                let base = (cone.origin - center).dot(axis);
                (
                    Segment::Line {
                        start: Point2::new(cone.ring_radius(low), base + low),
                        end: Point2::new(cone.ring_radius(high), base + high),
                    },
                    face.value.role,
                    2,
                    u_high - u_low,
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
                let (u_low, u_high, low, high) = parameter_bounds(topology, &face.value)?;
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
                    u_high - u_low,
                )
            }
            Surface::Torus(torus) => {
                if torus.axis.cross(axis).length() > agreement
                    || !on_axis(torus.origin, center, axis, agreement)
                {
                    return Err(RimBlendError::DomainUnsupported);
                }
                let (u_low, u_high, low, high) = parameter_bounds(topology, &face.value)?;
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
                    u_high - u_low,
                )
            }
            // A ruled wall is not a surface of revolution: the body is not
            // one this section describes.
            Surface::Ruled(_) | Surface::Bspline(_) => {
                return Err(RimBlendError::DomainUnsupported);
            }
        };
        pieces.push(piece);
    }

    // Deduplicate the half-face pairs: a carrier contributing two faces must
    // yield one section segment.
    let mut segments: Vec<(Segment, FaceRole)> = Vec::new();
    let mut seen = vec![false; pieces.len()];
    // The azimuth every curved carrier's two halves span between them.
    let mut sweeps = Vec::new();
    for index in 0..pieces.len() {
        if seen[index] {
            continue;
        }
        let (segment, role, expected, span) = pieces[index];
        let mut matches = 1;
        let mut swept = span;
        for other in index + 1..pieces.len() {
            if seen[other] {
                continue;
            }
            if segments_agree(segment, pieces[other].0, agreement) {
                seen[other] = true;
                matches += 1;
                swept += pieces[other].3;
            }
        }
        seen[index] = true;
        if matches != expected {
            return Err(RimBlendError::DomainUnsupported);
        }
        if expected == 2 {
            sweeps.push(swept);
        }
        segments.push((segment, role));
    }
    // A full turn has no wedge faces; a partial one has its two, and every
    // carrier spans the same azimuth.
    let (sweep, radial_u, radial_v) = match wedges.as_slice() {
        [] => (FULL_TURN, radial_u, radial_v),
        [first, second] => {
            let sweep = sweeps
                .first()
                .copied()
                .ok_or(RimBlendError::DomainUnsupported)?;
            if sweeps
                .iter()
                .any(|other| (other - sweep).abs() > 1.0e-9 * FULL_TURN)
                || sweep >= FULL_TURN
            {
                return Err(RimBlendError::DomainUnsupported);
            }
            // The turn begins at the wedge whose material lies ahead of it,
            // turning about the axis; the other has its material behind.
            // Azimuth zero is read from the faces themselves, since a
            // carrier's own frame need not start where the material does.
            let ahead =
                |(direction, normal): (Vector3, Vector3)| normal.dot(axis.cross(direction)) < 0.0;
            let start = match (ahead(*first), ahead(*second)) {
                (true, false) => first.0,
                (false, true) => second.0,
                _ => return Err(RimBlendError::DomainUnsupported),
            };
            (sweep, start, axis.cross(start))
        }
        _ => return Err(RimBlendError::DomainUnsupported),
    };

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
        sweep,
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
            Surface::Plane(_) | Surface::Sphere(_) | Surface::Ruled(_) | Surface::Bspline(_) => {
                None
            }
        };
        if let Some((axis, radial_u, radial_v, origin)) = frame {
            // Anchor the section frame on the axis at the carrier's own
            // origin, so section heights are measured consistently.
            return Ok((axis, radial_u, radial_v, origin));
        }
    }
    // A body turned from arcs alone, a ball, has only spheres to say where
    // its axis is.
    topology
        .faces
        .iter()
        .find_map(|face| match face.value.surface {
            Surface::Sphere(sphere) => {
                Some((sphere.axis, sphere.radial_u, sphere.radial_v, sphere.origin))
            }
            _ => None,
        })
        .ok_or(RimBlendError::DomainUnsupported)
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

/// The direction a wedge face's half-plane leaves the axis in: towards the
/// boundary point of the face farthest from the axis.
pub(crate) fn wedge_direction(
    topology: &Topology,
    face: &Face,
    center: Point3,
    axis: Vector3,
) -> Option<Vector3> {
    let Surface::Plane(plane) = face.surface else {
        return None;
    };
    let loop_record = topology.loop_record(face.outer_loop)?;
    let mut farthest: Option<Vector3> = None;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology.coedge(*coedge_key)?.value;
        for point in coedge.pcurve_endpoints() {
            let offset = plane.origin + plane.u * point.x + plane.v * point.y - center;
            let radial = offset - axis * offset.dot(axis);
            if farthest.is_none_or(|best| radial.length() > best.length()) {
                farthest = Some(radial);
            }
        }
    }
    let radial = farthest?;
    let length = radial.length();
    (length.is_finite() && length > f64::EPSILON).then(|| radial / length)
}

fn on_axis(point: Point3, center: Point3, axis: Vector3, agreement: f64) -> bool {
    let offset = point - center;
    (offset - axis * offset.dot(axis)).length() <= agreement
}

/// The `(inner, outer)` radii of a cap face: `(0, r)` for a full disk, and
/// the two rim radii for the washer face of a tube.
fn cap_radii(topology: &Topology, face: &Face) -> Result<(f64, f64), RimBlendError> {
    // A partial turn's cap is a sector: one loop of arcs and the two
    // straight sides joining them, to the axis or to an inner rim.
    if face.inner_loops.is_empty()
        && let Some(radii) = sector_radii(topology, face.outer_loop)?
    {
        return Ok(radii);
    }
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

/// The `(inner, outer)` radii of a sector cap's one loop, or `None` for a
/// loop with no straight side, which is a full rim.
fn sector_radii(
    topology: &Topology,
    loop_key: LoopKey,
) -> Result<Option<(f64, f64)>, RimBlendError> {
    let loop_record = topology
        .loop_record(loop_key)
        .ok_or(RimBlendError::DomainUnsupported)?;
    let mut radii = Vec::<f64>::new();
    let mut straight = false;
    for coedge_key in &loop_record.value.coedges {
        let coedge = topology
            .coedge(*coedge_key)
            .ok_or(RimBlendError::DomainUnsupported)?
            .value;
        match coedge.pcurve {
            Curve2::Circle { radius, .. } => {
                if !radii
                    .iter()
                    .any(|existing| (existing - radius).abs() <= 1.0e-9 * (1.0 + radius.abs()))
                {
                    radii.push(radius);
                }
            }
            Curve2::Line { .. } => straight = true,
            _ => return Err(RimBlendError::DomainUnsupported),
        }
    }
    if !straight {
        return Ok(None);
    }
    match radii.as_slice() {
        [outer] => Ok(Some((0.0, *outer))),
        [first, second] => Ok(Some((first.min(*second), first.max(*second)))),
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
                start_angle: first_angle,
                sweep: first_sweep,
            },
            Segment::Arc {
                center: second_center,
                radius: second_radius,
                start: second_start,
                end: second_end,
                start_angle: second_angle,
                sweep: second_sweep,
            },
        ) => {
            // The two halves of one circle share their ends; the point
            // halfway round tells them apart.
            let middle = |center: Point2, radius: f64, angle: f64, sweep: f64| {
                let halfway = angle + sweep / 2.0;
                Point2::new(
                    center.x + radius * halfway.cos(),
                    center.y + radius * halfway.sin(),
                )
            };
            same_point(first_center, second_center)
                && (first_radius - second_radius).abs() <= agreement
                && ((same_point(first_start, second_start) && same_point(first_end, second_end))
                    || (same_point(first_start, second_end) && same_point(first_end, second_start)))
                && same_point(
                    middle(first_center, first_radius, first_angle, first_sweep),
                    middle(second_center, second_radius, second_angle, second_sweep),
                )
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
    // One piece is enough: a ball's section is a single arc from pole to
    // pole.
    if pieces.is_empty() {
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
        other @ (Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. }) => {
            other.reversed()
        }
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
    // A partial turn is rebuilt through the same span, its wedge faces
    // taking the blended section.
    Ok(build_turned_topology(
        &RzSection {
            center: section.center,
            axis: section.axis,
            radial_u: section.radial_u,
            radial_v: section.radial_v,
            roles: blended.1,
            segments: blended.0,
            closed: section.closed,
            sweep: section.sweep,
        },
        section.sweep,
    ))
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

/// The three azimuth stations of a turn: where it starts, halfway, and where
/// it ends. A full turn ends where it starts.
const STATIONS: usize = 3;

#[derive(Clone, Copy)]
struct RimCircle {
    /// The ring's vertices at each station. A full turn's last is its first.
    vertices: [VertexKey; STATIONS],
    /// The two arcs between consecutive stations, each at most half a turn.
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

/// The ring a section vertex sweeps: a real circle, a pole on the axis, or,
/// for a partial turn, the bare axis point a planar cap is centred on.
#[derive(Clone, Copy)]
enum Ring {
    Circle(RimCircle),
    Pole(Pole),
    Axis(VertexKey),
}

impl Ring {
    const fn as_circle(self) -> Option<RimCircle> {
        match self {
            Self::Circle(circle) => Some(circle),
            Self::Pole(_) | Self::Axis(_) => None,
        }
    }

    /// The ring's vertex at `station`; every station of an axis point is the
    /// one vertex.
    const fn vertex(self, station: usize) -> VertexKey {
        match self {
            Self::Circle(circle) => circle.vertices[station],
            Self::Pole(pole) => pole.vertex,
            Self::Axis(vertex) => vertex,
        }
    }
}

/// The edges one section segment leaves in the two wedge faces of a partial
/// turn: its generators at the first and last stations, and whether they run
/// the way the chain does.
#[derive(Clone, Copy)]
struct WedgeUse {
    generators: [EdgeKey; 2],
    along_chain: bool,
}

struct Builder<'a> {
    topology: Topology,
    next_id: u64,
    section: &'a RzSection,
    /// How far the section turns: a full turn, or less.
    sweep: f64,
}

impl Builder<'_> {
    fn partial(&self) -> bool {
        self.sweep < FULL_TURN
    }

    /// The azimuth of each station.
    fn stations(&self) -> [f64; STATIONS] {
        [0.0, self.sweep / 2.0, self.sweep]
    }

    /// The unit radial direction at `azimuth`.
    fn radial(&self, azimuth: f64) -> Vector3 {
        self.section.radial_u * azimuth.cos() + self.section.radial_v * azimuth.sin()
    }

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

    fn vertex_point(&self, vertex: VertexKey) -> Point3 {
        self.topology.vertices[vertex.0].value.point
    }

    /// One ring as two exact arcs, split halfway round the turn.
    fn rim_circle(&mut self, radius: f64, height: f64) -> RimCircle {
        let [_, middle, end] = self.stations();
        let near = self.vertex(self.point(radius, 0.0, height));
        let far = self.vertex(self.point(radius, middle, height));
        let last = if self.partial() {
            self.vertex(self.point(radius, end, height))
        } else {
            near
        };
        let curve = Curve3::Circle {
            center: self.section.center + self.section.axis * height,
            u: self.section.radial_u,
            v: self.section.radial_v,
            radius,
        };
        let first = self.edge(Edge {
            vertices: [near, far],
            curve,
            parameter_range: ParameterRange::new(0.0, middle),
        });
        let second = self.edge(Edge {
            vertices: [far, last],
            curve,
            parameter_range: ParameterRange::new(middle, end),
        });
        RimCircle {
            vertices: [near, far, last],
            edges: [first, second],
        }
    }

    /// The ring a section vertex sweeps. A point on the axis is a pole only
    /// when a curved band meets it: an arc, or a slanted line sweeping a cone
    /// to its apex. A radial line ending on the axis closes a planar cap
    /// instead: a full disk sweeps no ring at all, and a sector is centred on
    /// the bare axis point its two straight sides meet at.
    fn ring_at(&mut self, point: Point2, curved: bool) -> Option<Ring> {
        if point.x > 0.0 {
            Some(Ring::Circle(self.rim_circle(point.x, point.y)))
        } else if curved {
            Some(Ring::Pole(self.pole(point.y)))
        } else if self.partial() {
            Some(Ring::Axis(self.vertex(self.point(0.0, 0.0, point.y))))
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

    /// The generator of an arc section segment at one station, with either
    /// end free to be a pole where every azimuth converges on the one pole
    /// vertex. It runs from `low` to `high` over `angles`.
    fn seam_minor_arc_ring(
        &mut self,
        (low, high): (Ring, Ring),
        station: usize,
        arc_center: Point2,
        radius: f64,
        angles: (f64, f64),
    ) -> EdgeKey {
        let radial = self.radial(self.stations()[station]);
        let center = self.section.center + radial * arc_center.x + self.section.axis * arc_center.y;
        self.edge(Edge {
            vertices: [low.vertex(station), high.vertex(station)],
            curve: Curve3::Circle {
                center,
                u: radial,
                v: self.section.axis,
                radius,
            },
            parameter_range: ParameterRange::new(angles.0, angles.1),
        })
    }

    /// A straight generator: an edge in one station's half-plane joining two
    /// ring vertices.
    fn seam_line(&mut self, from: VertexKey, to: VertexKey) -> EdgeKey {
        let start = self.vertex_point(from);
        let end = self.vertex_point(to);
        self.edge(Edge::line([from, to], [start, end]))
    }

    /// A line section segment's generator at every station, from `low` to
    /// `high`. A full turn's last station is its first, so it reuses that
    /// generator rather than laying a second one on top of it.
    fn line_generators(&mut self, low: Ring, high: Ring) -> [EdgeKey; STATIONS] {
        let first = self.seam_line(low.vertex(0), high.vertex(0));
        let middle = self.seam_line(low.vertex(1), high.vertex(1));
        let last = if self.partial() {
            self.seam_line(low.vertex(2), high.vertex(2))
        } else {
            first
        };
        [first, middle, last]
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

type CoedgeUse = (EdgeKey, Orientation, Curve2, ParameterRange);

fn line_pcurve(start: Point2, end: Point2) -> (Curve2, ParameterRange) {
    Curve2::line_segment([start, end])
}

fn cap_circle_pcurve(
    radius: f64,
    half: usize,
    reverse: bool,
    mirrored: bool,
) -> (Curve2, ParameterRange) {
    let start = if half == 0 { 0.0 } else { HALF_TURN };
    let range = (start, start + HALF_TURN);
    cap_arc_pcurve(
        radius,
        if reverse { (range.1, range.0) } else { range },
        mirrored,
    )
}

/// A rim arc drawn in a cap's own plane, from azimuth `range.0` to `range.1`.
fn cap_arc_pcurve(radius: f64, range: (f64, f64), mirrored: bool) -> (Curve2, ParameterRange) {
    let (u, v) = if mirrored {
        (Vector2::new(0.0, 1.0), Vector2::new(1.0, 0.0))
    } else {
        (Vector2::new(1.0, 0.0), Vector2::new(0.0, 1.0))
    };
    (
        Curve2::Circle {
            center: Point2::new(0.0, 0.0),
            u,
            v,
            radius,
        },
        ParameterRange::new(range.0, range.1),
    )
}

/// A point at `radius` and `azimuth` in a cap's own plane.
fn cap_point(radius: f64, azimuth: f64, mirrored: bool) -> Point2 {
    let (x, y) = (radius * azimuth.cos(), radius * azimuth.sin());
    if mirrored {
        Point2::new(y, x)
    } else {
        Point2::new(x, y)
    }
}

/// Revolves a closed (r, z) section through `sweep` radians, from the
/// section's own half-plane (azimuth zero) towards `radial_v`.
///
/// Every curved carrier is split halfway round the turn, so each of its two
/// faces spans at most half a turn and a pole's one degenerate edge is always
/// shared by two faces in opposite senses. A full turn is the case whose
/// split falls at `π`, with seams at azimuth `0` and `π` (ADR 0016). Anything
/// less is closed by two planar wedge faces: the section itself at azimuth
/// zero, and its turned copy at `sweep`.
pub(crate) fn build_turned_topology(section: &RzSection, sweep: f64) -> Topology {
    build_turned_region(section, &[], sweep)
}

/// Revolves a section chain into a sheet (ADR 0056, Track S): the bands
/// every segment sweeps, exactly as a solid's are built, with no wedge
/// faces closing a partial turn and no solid. The chain may be open at both
/// ends, clear of the axis or on it, or closed on itself.
pub(crate) fn build_turned_sheet(section: &RzSection, sweep: f64) -> Topology {
    let mut builder = Builder {
        topology: Topology::default(),
        next_id: 1,
        section,
        sweep: sweep.min(FULL_TURN),
    };
    let _ = sweep_section(&mut builder, section);
    let shell_id = builder.allocate();
    let faces = (0..builder.topology.faces.len()).map(FaceKey).collect();
    builder.topology.shells.push(Record {
        id: shell_id,
        value: Shell { faces },
    });
    builder.topology
}

/// Revolves a region with holes through `sweep` radians. `outer` runs
/// anticlockwise and every hole clockwise, all in the one section frame, so
/// material lies on the left of each chain and the builder faces every band
/// the right way without knowing which is which.
///
/// Swept a full turn, each hole is a cavity: a closed shell of its own,
/// facing into it, held as an inner shell of the solid. Swept less, it is a
/// channel open at both ends, and its outline is a hole in each of the two
/// wedge faces.
pub(crate) fn build_turned_region(outer: &RzSection, holes: &[RzSection], sweep: f64) -> Topology {
    let mut builder = Builder {
        topology: Topology::default(),
        next_id: 1,
        section: outer,
        sweep: sweep.min(FULL_TURN),
    };
    let swept = sweep_section(&mut builder, outer);
    let outer_faces = builder.topology.faces.len();
    let mut swept_holes = Vec::with_capacity(holes.len());
    for hole in holes {
        let first = builder.topology.faces.len();
        let (circles, wedges) = sweep_section(&mut builder, hole);
        swept_holes.push((hole, circles, wedges, first..builder.topology.faces.len()));
    }

    if builder.partial() {
        let hole_wedges = swept_holes
            .iter()
            .map(|(hole, circles, wedges, _)| (*hole, circles.as_slice(), wedges.as_slice()))
            .collect::<Vec<_>>();
        push_wedges(&mut builder, (outer, &swept.0, &swept.1), &hole_wedges);
    }

    // A partial turn, or a region without holes, is one closed shell. A full
    // turn's holes are cavities, each a shell of its own inside the first.
    let cavities = if builder.partial() {
        Vec::new()
    } else {
        swept_holes
            .into_iter()
            .map(|(_, _, _, faces)| faces)
            .collect::<Vec<_>>()
    };
    let outer_range = if cavities.is_empty() {
        0..builder.topology.faces.len()
    } else {
        0..outer_faces
    };
    let shell_key = ShellKey(builder.topology.shells.len());
    let shell_id = builder.allocate();
    builder.topology.shells.push(Record {
        id: shell_id,
        value: Shell {
            faces: outer_range.map(FaceKey).collect(),
        },
    });
    let mut inner_shells = Vec::with_capacity(cavities.len());
    for faces in cavities {
        let key = ShellKey(builder.topology.shells.len());
        let id = builder.allocate();
        builder.topology.shells.push(Record {
            id,
            value: Shell {
                faces: faces.map(FaceKey).collect(),
            },
        });
        inner_shells.push(key);
    }
    let solid_id = builder.allocate();
    builder.topology.solids.push(Record {
        id: solid_id,
        value: Solid {
            outer_shell: shell_key,
            inner_shells,
        },
    });
    builder.topology
}

/// The rings and faces one section chain sweeps, and the generators it
/// leaves for the wedge faces of a partial turn.
fn sweep_section(
    builder: &mut Builder<'_>,
    section: &RzSection,
) -> (Vec<Option<Ring>>, Vec<Option<WedgeUse>>) {
    let count = section.segments.len();
    let stations = builder.stations();

    // One circle per section vertex with r > 0. Vertex `index` is the start of
    // segment `index`; the final vertex is the end of the last segment.
    // A segment sweeps a curved band, not a planar cap, when it is an arc or
    // a line that is not radial.
    let curved = |segment: &Segment| match *segment {
        Segment::Line { start, end } => (end.y - start.y).abs() > axis_agreement(section),
        _ => true,
    };
    let mut circles: Vec<Option<Ring>> = Vec::with_capacity(count + 1);
    for segment in &section.segments {
        circles.push(builder.ring_at(segment.start(), curved(segment)));
    }
    if section.closed {
        // A tube's chain returns to where it started, so the final ring is the
        // first one. Sweeping a second ring there would leave two coincident
        // circles and a shell that never closes.
        circles.push(circles[0]);
    } else {
        let last = section.segments[count - 1];
        circles.push(builder.ring_at(last.end(), curved(&last)));
    }

    let mut wedges: Vec<Option<WedgeUse>> = vec![None; count];
    for (index, segment) in section.segments.iter().enumerate() {
        let role = section.roles[index];
        let start = segment.start();
        let end = segment.end();
        match *segment {
            Segment::Line { .. }
                if (start.x <= 0.0 || end.x <= 0.0)
                    && (end.y - start.y).abs() <= axis_agreement(section) =>
            {
                // A radial line touching the axis is a planar cap: a full disk,
                // or a sector for a partial turn.
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
                let uses = if builder.partial() {
                    let (Some(from), Some(to)) = (circles[index], circles[index + 1]) else {
                        continue;
                    };
                    // The sector's straight sides run with the chain, from
                    // the rim in to the axis or from the axis out.
                    let generators = [
                        builder.seam_line(from.vertex(0), to.vertex(0)),
                        builder.seam_line(from.vertex(2), to.vertex(2)),
                    ];
                    wedges[index] = Some(WedgeUse {
                        generators,
                        along_chain: true,
                    });
                    sector_cap(&stations, circle, radius, generators, outward_up)
                } else if outward_up {
                    (0..2)
                        .map(|half| {
                            let (pcurve, range) = cap_circle_pcurve(radius, half, false, false);
                            (circle.edges[half], Orientation::Forward, pcurve, range)
                        })
                        .collect()
                } else {
                    (0..2)
                        .map(|half| {
                            let (pcurve, range) = cap_circle_pcurve(radius, half, true, true);
                            (circle.edges[half], Orientation::Reverse, pcurve, range)
                        })
                        .collect()
                };
                let loop_key = builder.push_loop(uses);
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
                if builder.partial() {
                    let (from, to) = if outward_up {
                        (outer, inner)
                    } else {
                        (inner, outer)
                    };
                    let generators = [
                        builder.seam_line(from.vertices[0], to.vertices[0]),
                        builder.seam_line(from.vertices[2], to.vertices[2]),
                    ];
                    wedges[index] = Some(WedgeUse {
                        generators,
                        along_chain: true,
                    });
                    let uses = sector_annulus(
                        &stations,
                        (inner, inner_radius),
                        (outer, outer_radius),
                        generators,
                        outward_up,
                    );
                    let loop_key = builder.push_loop(uses);
                    builder.push_face(Surface::Plane(plane), loop_key, role);
                    continue;
                }
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
                builder.push_face_with_holes(
                    Surface::Plane(plane),
                    outer_loop,
                    vec![inner_loop],
                    role,
                );
            }
            Segment::Line { .. } => {
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
                // Either end may be a cone's apex, a pole on the axis.
                let (Some(low), Some(high)) = (
                    circles[if descending { index + 1 } else { index }],
                    circles[if descending { index } else { index + 1 }],
                ) else {
                    continue;
                };
                let seams = builder.line_generators(low, high);
                wedges[index] = Some(WedgeUse {
                    generators: [seams[0], seams[2]],
                    along_chain: !descending,
                });
                let angular_sign = if descending { -1.0 } else { 1.0 };
                let slope = (top.x - base.x) / (top.y - base.y);
                let height = top.y - base.y;
                // A cone is anchored at a ring with a radius, so a band
                // whose base is the apex is anchored at its top instead and
                // runs up to zero from below.
                let (anchor, parameters) = if base.x > 0.0 {
                    (base, (0.0, height))
                } else {
                    (top, (-height, 0.0))
                };
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
                        origin: section.center + section.axis * anchor.y,
                        axis: section.axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        base_radius: anchor.x,
                        slope,
                        angular_sign,
                    })
                };
                push_band(
                    builder, surface, low, high, seams, parameters, role, descending,
                );
            }
            Segment::Arc {
                center: arc_center,
                radius,
                start_angle,
                sweep: arc_sweep,
                ..
            } => {
                // An arc swept clockwise is the concave case: the band's
                // material is on the far side, exactly as for a descending
                // line. It is built the same way — from its lower parameter
                // end, so the minor angle always increases, with the angular
                // sign alone reversing the face. (Flipping the carrier's axis
                // as well, as this once did, negates the minor angle with it;
                // the two reversals cancel and leave the face inside out.)
                let reversed = arc_sweep < 0.0;
                let (from, to) = (circles[index], circles[index + 1]);
                let (Some(low), Some(high), angles) = (if reversed {
                    (to, from, (start_angle + arc_sweep, start_angle))
                } else {
                    (from, to, (start_angle, start_angle + arc_sweep))
                }) else {
                    continue;
                };
                let first = builder.seam_minor_arc_ring((low, high), 0, arc_center, radius, angles);
                let middle =
                    builder.seam_minor_arc_ring((low, high), 1, arc_center, radius, angles);
                let last = if builder.partial() {
                    builder.seam_minor_arc_ring((low, high), 2, arc_center, radius, angles)
                } else {
                    first
                };
                wedges[index] = Some(WedgeUse {
                    generators: [first, last],
                    along_chain: !reversed,
                });
                let angular_sign = if reversed { -1.0 } else { 1.0 };
                // An arc centred on the axis sweeps a sphere. Emitting a torus
                // of zero major radius instead would be a carrier whose
                // parameterization collapses onto its own spine.
                let origin = section.center + section.axis * arc_center.y;
                let surface = if arc_center.x.abs() <= axis_agreement(section) {
                    Surface::Sphere(Sphere {
                        origin,
                        axis: section.axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        radius,
                        angular_sign,
                    })
                } else {
                    Surface::Torus(Torus {
                        origin,
                        axis: section.axis,
                        radial_u: section.radial_u,
                        radial_v: section.radial_v,
                        major_radius: arc_center.x,
                        minor_radius: radius,
                        angular_sign,
                    })
                };
                push_band(
                    builder,
                    surface,
                    low,
                    high,
                    [first, middle, last],
                    angles,
                    role,
                    reversed,
                );
            }
            Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => {
                unreachable!("revolved sections carry lines and arcs only")
            }
        }
    }

    (circles, wedges)
}

/// The boundary of a sector cap: out from the axis along the first station,
/// round the rim, and back in along the last — anticlockwise about the cap's
/// outward normal either way up. The generators run with the chain: inward
/// for a cap with material below (`outward_up`), outward otherwise.
fn sector_cap(
    stations: &[f64; STATIONS],
    circle: RimCircle,
    radius: f64,
    generators: [EdgeKey; 2],
    outward_up: bool,
) -> Vec<CoedgeUse> {
    let [first, middle, last] = *stations;
    let centre = Point2::new(0.0, 0.0);
    if outward_up {
        vec![
            {
                let (pcurve, range) = line_pcurve(centre, cap_point(radius, first, false));
                (generators[0], Orientation::Reverse, pcurve, range)
            },
            {
                let (pcurve, range) = cap_arc_pcurve(radius, (first, middle), false);
                (circle.edges[0], Orientation::Forward, pcurve, range)
            },
            {
                let (pcurve, range) = cap_arc_pcurve(radius, (middle, last), false);
                (circle.edges[1], Orientation::Forward, pcurve, range)
            },
            {
                let (pcurve, range) = line_pcurve(cap_point(radius, last, false), centre);
                (generators[1], Orientation::Forward, pcurve, range)
            },
        ]
    } else {
        vec![
            {
                let (pcurve, range) = line_pcurve(centre, cap_point(radius, last, true));
                (generators[1], Orientation::Forward, pcurve, range)
            },
            {
                let (pcurve, range) = cap_arc_pcurve(radius, (last, middle), true);
                (circle.edges[1], Orientation::Reverse, pcurve, range)
            },
            {
                let (pcurve, range) = cap_arc_pcurve(radius, (middle, first), true);
                (circle.edges[0], Orientation::Reverse, pcurve, range)
            },
            {
                let (pcurve, range) = line_pcurve(cap_point(radius, first, true), centre);
                (generators[0], Orientation::Reverse, pcurve, range)
            },
        ]
    }
}

/// The boundary of an annular sector: one loop round both rims and the two
/// straight sides, anticlockwise about the face's outward normal. The
/// generators run with the chain: inward for a face with material below
/// (`outward_up`), outward otherwise.
fn sector_annulus(
    stations: &[f64; STATIONS],
    (inner, inner_radius): (RimCircle, f64),
    (outer, outer_radius): (RimCircle, f64),
    generators: [EdgeKey; 2],
    outward_up: bool,
) -> Vec<CoedgeUse> {
    let [first, middle, last] = *stations;
    let mirrored = !outward_up;
    let arc = |circle: RimCircle, radius: f64, span: usize, forward: bool| {
        let range = if span == 0 {
            (first, middle)
        } else {
            (middle, last)
        };
        let (pcurve, range) = cap_arc_pcurve(
            radius,
            if forward { range } else { (range.1, range.0) },
            mirrored,
        );
        let orientation = if forward {
            Orientation::Forward
        } else {
            Orientation::Reverse
        };
        (circle.edges[span], orientation, pcurve, range)
    };
    let side = |edge: EdgeKey, azimuth: f64, outward: bool, orientation: Orientation| {
        let (from, to) = if outward {
            (inner_radius, outer_radius)
        } else {
            (outer_radius, inner_radius)
        };
        let (pcurve, range) = line_pcurve(
            cap_point(from, azimuth, mirrored),
            cap_point(to, azimuth, mirrored),
        );
        (edge, orientation, pcurve, range)
    };
    if outward_up {
        vec![
            side(generators[0], first, true, Orientation::Reverse),
            arc(outer, outer_radius, 0, true),
            arc(outer, outer_radius, 1, true),
            side(generators[1], last, false, Orientation::Forward),
            arc(inner, inner_radius, 1, false),
            arc(inner, inner_radius, 0, false),
        ]
    } else {
        vec![
            side(generators[1], last, true, Orientation::Forward),
            arc(outer, outer_radius, 1, false),
            arc(outer, outer_radius, 0, false),
            side(generators[0], first, false, Orientation::Reverse),
            arc(inner, inner_radius, 0, true),
            arc(inner, inner_radius, 1, true),
        ]
    }
}

/// One section chain as it appears in the two wedge faces: its generators at
/// the first station, in chain order, and at the last, against it. A chain
/// that closes through the axis is closed in both by the one axis edge they
/// share.
fn wedge_outline(
    builder: &mut Builder<'_>,
    section: &RzSection,
    circles: &[Option<Ring>],
    wedges: &[Option<WedgeUse>],
) -> (Vec<CoedgeUse>, Vec<CoedgeUse>) {
    let segments = &section.segments;
    // The axis edge runs the way the section closes: from the chain's last
    // point back down to its first.
    let axis_edge = if section.closed {
        None
    } else {
        match (circles.last().copied().flatten(), circles[0]) {
            (Some(last), Some(first)) => {
                let (from, to) = (last.vertex(0), first.vertex(0));
                Some(builder.seam_line(from, to))
            }
            _ => None,
        }
    };
    let closing = segments
        .first()
        .zip(segments.last())
        .map(|(first, last)| (last.end(), first.start()));

    // At azimuth zero the plane's frame is (radial, axis), so the section
    // appears exactly as it is, anticlockwise about the outward normal
    // `radial × axis = -radial_v`.
    let mut start_uses = Vec::with_capacity(segments.len() + 1);
    for (segment, wedge) in segments.iter().zip(wedges) {
        let Some(wedge) = wedge else { continue };
        let (pcurve, range) = section_pcurve(*segment, false);
        let orientation = if wedge.along_chain {
            Orientation::Forward
        } else {
            Orientation::Reverse
        };
        start_uses.push((wedge.generators[0], orientation, pcurve, range));
    }
    if let (Some(edge), Some((from, to))) = (axis_edge, closing) {
        let (pcurve, range) = line_pcurve(from, to);
        start_uses.push((edge, Orientation::Forward, pcurve, range));
    }

    // At the end of the sweep the frame is (axis, radial), whose normal is
    // the direction of turning; the section is drawn with its coordinates
    // swapped and walked backwards, which is anticlockwise again.
    let mut end_uses = Vec::with_capacity(segments.len() + 1);
    if let (Some(edge), Some((from, to))) = (axis_edge, closing) {
        let (pcurve, range) = line_pcurve(swapped(to), swapped(from));
        end_uses.push((edge, Orientation::Reverse, pcurve, range));
    }
    for (segment, wedge) in segments.iter().zip(wedges).rev() {
        let Some(wedge) = wedge else { continue };
        let (pcurve, range) = section_pcurve(*segment, true);
        let orientation = if wedge.along_chain {
            Orientation::Reverse
        } else {
            Orientation::Forward
        };
        end_uses.push((wedge.generators[1], orientation, pcurve, range));
    }
    (start_uses, end_uses)
}

/// One section chain as swept: the section, its rings, and its wedge uses.
type SweptSection<'a> = (&'a RzSection, &'a [Option<Ring>], &'a [Option<WedgeUse>]);

/// The two planar faces that close a partial turn: the section at azimuth
/// zero, and its turned copy at the end of the sweep. A hole's chain runs
/// clockwise, so its outline in each is already the right way round for a
/// hole in the face.
fn push_wedges(
    builder: &mut Builder<'_>,
    (outer, circles, wedges): SweptSection<'_>,
    holes: &[SweptSection<'_>],
) {
    let section = builder.section;
    let end = builder.sweep;
    let (outer_start, outer_end) = wedge_outline(builder, outer, circles, wedges);
    let hole_outlines = holes
        .iter()
        .map(|(hole, circles, wedges)| wedge_outline(builder, hole, circles, wedges))
        .collect::<Vec<_>>();

    let start_loop = builder.push_loop(outer_start);
    let start_holes = hole_outlines
        .iter()
        .map(|(start, _)| builder.push_loop(start.clone()))
        .collect();
    builder.push_face_with_holes(
        Surface::Plane(Plane::new(section.center, section.radial_u, section.axis)),
        start_loop,
        start_holes,
        FaceRole::ExtrusionBottom,
    );

    let end_loop = builder.push_loop(outer_end);
    let end_holes = hole_outlines
        .into_iter()
        .map(|(_, end)| builder.push_loop(end))
        .collect();
    builder.push_face_with_holes(
        Surface::Plane(Plane::new(
            section.center,
            section.axis,
            builder.radial(end),
        )),
        end_loop,
        end_holes,
        FaceRole::ExtrusionTop,
    );
}

/// A section segment drawn in a wedge face's plane: as `(r, z)` in chain
/// order for the first wedge, or as `(z, r)` against the chain for the last.
fn section_pcurve(segment: Segment, swap: bool) -> (Curve2, ParameterRange) {
    match segment {
        Segment::Line { start, end } => {
            if swap {
                line_pcurve(swapped(end), swapped(start))
            } else {
                line_pcurve(start, end)
            }
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let (center, u, v, range) = if swap {
                (
                    swapped(center),
                    Vector2::new(0.0, 1.0),
                    Vector2::new(1.0, 0.0),
                    ParameterRange::new(start_angle + sweep, start_angle),
                )
            } else {
                (
                    center,
                    Vector2::new(1.0, 0.0),
                    Vector2::new(0.0, 1.0),
                    ParameterRange::new(start_angle, start_angle + sweep),
                )
            };
            (
                Curve2::Circle {
                    center,
                    u,
                    v,
                    radius,
                },
                range,
            )
        }
        Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => {
            unreachable!("revolved sections carry lines and arcs only")
        }
    }
}

const fn swapped(point: Point2) -> Point2 {
    Point2::new(point.y, point.x)
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

/// Emits the two faces of one revolved section segment, split halfway round
/// the turn.
///
/// `reversed` marks a band whose material lies on the far side — a bore, a cup
/// wall, a concave blend. Its carrier is parameterized the other way round in
/// azimuth (`u` is minus the azimuth, taken in `[2π - sweep, 2π]`), so each
/// face covers the other arc of the rim, traversed the other way; everything
/// else about the loop is unchanged.
#[allow(clippy::too_many_arguments)]
fn push_band(
    builder: &mut Builder<'_>,
    surface: Surface,
    low: Ring,
    high: Ring,
    seams: [EdgeKey; STATIONS],
    parameters: (f64, f64),
    role: FaceRole,
    reversed: bool,
) {
    let (v_low, v_high) = parameters;
    let [_, middle, end] = builder.stations();
    for half in 0..2 {
        // The face's azimuth span in its own `u`, the generators at its two
        // ends, and the rim arc it runs along.
        let (u0, u1, seam_down, seam_up, rim) = match (reversed, half) {
            (false, 0) => (0.0, middle, seams[0], seams[1], 0),
            (false, _) => (middle, end, seams[1], seams[2], 1),
            (true, 0) => (FULL_TURN - end, FULL_TURN - middle, seams[2], seams[1], 1),
            (true, _) => (FULL_TURN - middle, FULL_TURN, seams[1], seams[0], 0),
        };
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
            Ring::Axis(_) => unreachable!("a curved band ends at a rim or a pole"),
        };
        let (high_edge, high_sense) = match high {
            Ring::Circle(circle) => (circle.edges[rim], backward),
            Ring::Pole(pole) => (pole.edge, half_sense(rim).reversed()),
            Ring::Axis(_) => unreachable!("a curved band ends at a rim or a pole"),
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

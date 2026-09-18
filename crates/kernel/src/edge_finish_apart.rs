//! ADR 0044's second answer: a finish built beside whatever already shapes the
//! corners it reaches, rather than as part of them.
//!
//! The ordinary edge finish owns a corner. Every edge meeting there is its
//! business, and a later feature arriving at the same vertex is refused,
//! because the committed one already decided what that corner looks like.
//! Standing apart means the opposite: this band is cut as though the others
//! were not there, the two meeting along a seam, and the corner keeps a point
//! of its own where all of them do.
//!
//! Stated as a solid rather than as a patch, that is simply the body less the
//! band's own removal — which is why it is built here as a Boolean rather than
//! as topology surgery on faces a previous feature owns.
//!
//! A bevel's removal is a half-space, and a fillet's is the curvilinear
//! triangle between the two walls and the band — the corner a rolling ball
//! cannot reach. Both are prisms swept along the edge, so a finish standing
//! apart is a prism against a prism, which is the reduction ADR 0025 built
//! first and the one that now carries a tangential contact.
//!
//! The fillet's tangency is what makes it delicate. Its band touches each wall
//! rather than crossing it, and a touch is only a touch if it is exact: the
//! tangency points are taken as the feet of the perpendiculars from the band's
//! own axis, not as `r/tan(θ/2)` along each face, because that trig round trip
//! lands a bit or two off and a flank plane 4e-16 outside the band does not
//! graze it at all — it misses, and every stage after is entitled to believe
//! the miss.
//!
//! A finish standing apart from a *band* — a corner an earlier feature
//! rounded, or a second edge running across the first — is cut by the general
//! engine rather than by the prism reduction, since no one axis reduces it.
//! That route now carries tangency too, so those work as well: three bands off
//! one corner, each stood apart from the others, land on the union of three
//! quarter-round prisms whichever order they are taken in.

use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EdgeFinishKind,
    EntityKind, EntityRef, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
    Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy, RequestId,
    Vector3 as ProtocolVector3,
};

use crate::Snapshot;
use crate::topology::{Curve3, Point3, Topology, Vector3};

/// Why a finish could not be stood apart from the corner it reaches.
#[derive(Clone, Debug)]
pub(crate) struct ApartRefusal {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> ApartRefusal {
    ApartRefusal {
        code,
        message: message.into(),
    }
}

/// Builds every selected edge's finish as its own cut, one after another.
pub(crate) fn build_edge_finishes_apart(
    input: &Snapshot,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, ApartRefusal> {
    if targets.is_empty() || targets.len() > 64 {
        return Err(refuse(
            "EDGE_FINISH_APART_TARGET_INVALID",
            "Standing a finish apart takes between one and sixty-four edges of this body.",
        ));
    }
    if targets
        .iter()
        .any(|target| target.snapshot != input.id() || target.kind != EntityKind::Edge)
    {
        return Err(refuse(
            "EDGE_FINISH_APART_TARGET_INVALID",
            "Every target must be an edge of the body this feature is being built on.",
        ));
    }
    if !distance.is_finite() || distance <= 0.0 {
        return Err(refuse(
            "EDGE_FINISH_APART_DISTANCE_INVALID",
            "A size must be a positive length.",
        ));
    }
    let reach = body_reach(&input.topology, distance);
    let mut body = input.clone();
    for target in targets {
        let tool = removal_tool(&body.topology, *target, kind, distance, reach, precision)?;
        // Through the engine's own dispatch, not straight into one of its
        // rungs. The prism reduction is what carries a cut like this — both
        // operands are prisms on parallel axes — and it is also where a
        // tangential contact is answered exactly. Reaching past it to the
        // general engine would take a slower route that refuses this shape.
        let request = BooleanRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("edge-finish-apart"),
            expected_target_snapshot: body.id(),
            expected_tool_snapshot: tool.id(),
            precision,
            operation: BooleanOperation::Difference,
        };
        body = crate::NativeKernel::execute_boolean(
            &body,
            &tool,
            &request,
            &crate::CancellationToken::new(),
        )
        .map_err(|error| {
            refuse(
                "EDGE_FINISH_APART_DOMAIN_UNSUPPORTED",
                format!(
                    "A finish of {distance:.6} standing apart here could not be cut: {error}. \
                     Join it to the finish that already shapes the corner instead."
                ),
            )
        })?
        .snapshot;
        // Certify each cut before the next one builds on it, so a refusal
        // names this route and what to do instead rather than arriving later
        // as a bare validation failure.
        let report = crate::validator::validate(&body.topology, precision.linear_agreement);
        if let Some(first) = report.diagnostics.first() {
            return Err(refuse(
                "EDGE_FINISH_APART_CONSTRUCTION_FAILED",
                format!(
                    "A finish of {distance:.6} standing apart here was cut but did not certify \
                     ({} at {}). Nothing is published from a route that cannot prove its own \
                     answer: join this finish to the one that already shapes the corner instead.",
                    first.code.as_str(),
                    first.path
                ),
            ));
        }
    }
    Ok(body.topology)
}

/// Far enough that a tool built at this size covers everything the body can
/// put behind one bevel plane.
///
/// The bounding box's diagonal is the longest run anything in the body can
/// make, so twice it, plus the setback, reaches past every corner from
/// anywhere on the bevel line. Making it larger than that does not buy
/// accuracy — the regularized Boolean's arithmetic, not the tool's size, is
/// what the last digits come from.
fn body_reach(topology: &Topology, distance: f64) -> f64 {
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for vertex in &topology.vertices {
        let point = vertex.value.point;
        for (axis, value) in [point.x, point.y, point.z].into_iter().enumerate() {
            low[axis] = low[axis].min(value);
            high[axis] = high[axis].max(value);
        }
    }
    let diagonal = (0..3)
        .map(|axis| (high[axis] - low[axis]).max(0.0))
        .fold(0.0_f64, |sum, side| side.mul_add(side, sum))
        .sqrt();
    (diagonal * 2.0 + distance * 2.0).max(distance * 4.0)
}

/// The solid a bevel of this edge removes: everything on the outer side of the
/// plane that cuts both its faces back by `distance`.
///
/// It is a prism swept along the edge, whose section is a triangle with one
/// side on the bevel line and an apex far outside the body, oversized at both
/// ends so nothing but that one side ever meets the body.
fn removal_tool(
    topology: &Topology,
    target: EntityRef,
    kind: EdgeFinishKind,
    distance: f64,
    reach: f64,
    precision: PrecisionPolicy,
) -> Result<Snapshot, ApartRefusal> {
    let edge = topology
        .edges
        .iter()
        .position(|edge| edge.id.get() == target.entity.0)
        .ok_or_else(|| {
            refuse(
                "EDGE_FINISH_APART_TARGET_INVALID",
                "That edge is not part of this body.",
            )
        })?;
    let Curve3::Line { endpoints } = topology.edges[edge].value.curve else {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "Standing a bevel apart takes a straight edge. A rim or an arc is finished by its own \
             exact route, which owns its corners.",
        ));
    };
    let along = difference(endpoints[1], endpoints[0]);
    let length = magnitude(along);
    if length <= precision.linear_agreement {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "That edge is too short to bevel.",
        ));
    }
    let along = scale(along, 1.0 / length);
    let faces = faces_of_edge(topology, edge).ok_or_else(|| {
        refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "That edge does not separate exactly two faces.",
        )
    })?;
    let normals = faces.map(|face| topology.faces[face].value.surface.as_plane());
    let ([Some(first), Some(second)], _) = (normals, ()) else {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "Standing a bevel apart takes an edge between two flat faces. Where one of them is a \
             blend or a hole wall, join the finish that already shapes this corner instead.",
        ));
    };
    // The way into each face from the edge, square to it and pointing at the
    // material: the one of the two that leans away from the other face.
    let into = |normal: Vector3, other: Vector3| {
        let across = cross(normal, along);
        let span = magnitude(across);
        if span <= f64::EPSILON {
            return None;
        }
        let across = scale(across, 1.0 / span);
        Some(if dot(across, other) < 0.0 {
            across
        } else {
            scale(across, -1.0)
        })
    };
    let (Some(out_first), Some(out_second)) = (
        into(first.normal, second.normal),
        into(second.normal, first.normal),
    ) else {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "That edge runs along one of its own faces, which leaves no direction to set back in.",
        ));
    };
    let anchor = endpoints[0];
    // How wide the material wedge is at this edge. A fillet's circle is
    // inscribed in it, so the angle sets both how far from the edge the band
    // touches each face and how deep its axis sits.
    let opening = dot(out_first, out_second).clamp(-1.0, 1.0).acos();
    let half = opening / 2.0;
    if half.sin().abs() <= precision.angular_agreement_radians.max(1.0e-12) {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "The two faces at that edge lie flat against each other, so a finish of them has no \
             width.",
        ));
    }
    let bisector = {
        let sum = Vector3::new(
            out_first.x + out_second.x,
            out_first.y + out_second.y,
            out_first.z + out_second.z,
        );
        let span = magnitude(sum);
        if span <= f64::EPSILON {
            return Err(refuse(
                "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
                "The two faces at that edge fold back on each other, which leaves no wedge to \
                 finish.",
            ));
        }
        scale(sum, 1.0 / span)
    };
    // Where the band's axis runs: on the bisector, far enough in that the
    // circle of this radius touches both walls.
    let axis = offset(anchor, scale(bisector, distance / half.sin()));
    // A chamfer sets back along each face by the distance itself. A fillet
    // touches each face at the foot of the perpendicular from its axis, and
    // that is where it is taken from rather than from `r/tan(θ/2)` along the
    // face: the trig round trip lands a bit or two off the true foot, and a
    // tangency a bit off is not a tangency at all — the flank plane then sits
    // outside the band by 4e-16 and the two stop touching, which every stage
    // after this one is entitled to believe.
    let (toe, heel) = match kind {
        EdgeFinishKind::Chamfer => (
            offset(anchor, scale(out_first, distance)),
            offset(anchor, scale(out_second, distance)),
        ),
        EdgeFinishKind::Fillet => (
            offset(axis, scale(first.normal, distance)),
            offset(axis, scale(second.normal, distance)),
        ),
    };
    let base = difference(heel, toe);
    let width = magnitude(base);
    if width <= precision.linear_agreement {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "The two faces at that edge lie flat against each other, so a finish of them has no \
             width.",
        ));
    }
    let base = scale(base, 1.0 / width);
    // Square to the bevel, pointing back at the edge. That is the side the
    // material being cut off is on: a bevel takes the sharp corner away, so
    // the tool's apex belongs behind the bevel line, not in front of it.
    let backward = {
        let candidate = cross(base, along);
        let span = magnitude(candidate);
        if span <= f64::EPSILON {
            return Err(refuse(
                "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
                "The bevel of that edge is degenerate.",
            ));
        }
        let candidate = scale(candidate, 1.0 / span);
        let inward = difference(anchor, midpoint(toe, heel));
        if dot(candidate, inward) < 0.0 {
            scale(candidate, -1.0)
        } else {
            candidate
        }
    };
    // A section frame square to the edge, laid on the faces themselves rather
    // than on the chord between them: the tool's own flanks then run along the
    // frame's axes, and the planes it is cut from come out axis-aligned in it
    // rather than at an angle to everything.
    let u = out_first;
    let v = cross(along, u);
    let plane = |point: Point3| {
        let delta = difference(point, anchor);
        ProtocolPoint2::new(dot(delta, u), dot(delta, v))
    };
    let start = offset(anchor, scale(along, -reach));
    let frame = PlanarFrame3::new(
        ProtocolPoint3::new(start.x, start.y, start.z),
        ProtocolVector3::new(u.x, u.y, u.z),
        ProtocolVector3::new(v.x, v.y, v.z),
    );
    let outer = match kind {
        // A triangle with one side on the bevel line and an apex far behind
        // it: everything the flat cut takes away, and more, safely outside.
        EdgeFinishKind::Chamfer => {
            let apex = offset(midpoint(toe, heel), scale(backward, reach));
            wound(vec![
                straight(plane(offset(toe, scale(base, -reach))), plane(apex)),
                straight(plane(apex), plane(offset(heel, scale(base, reach)))),
                straight(
                    plane(offset(heel, scale(base, reach))),
                    plane(offset(toe, scale(base, -reach))),
                ),
            ])
        }
        // The curvilinear triangle between the two walls and the band: the
        // corner the rolling ball cannot reach. Its two straight sides run
        // out past the body so nothing but the arc is ever in contact — and
        // the arc is tangent to each wall, which is what a fillet means and
        // what the Boolean now takes.
        EdgeFinishKind::Fillet => {
            let behind_first = offset(heel, scale(out_first, -reach));
            let behind_second = offset(toe, scale(out_second, -reach));
            let far = offset(
                offset(anchor, scale(out_first, -reach)),
                scale(out_second, -reach),
            );
            wound(vec![
                straight(plane(far), plane(behind_first)),
                straight(plane(behind_first), plane(heel)),
                // Tangent to both walls, bulging at the edge: the minor arc,
                // which is the only one a convex corner can mean.
                minor_arc(plane(heel), plane(toe), plane(axis)),
                straight(plane(toe), plane(behind_second)),
                straight(plane(behind_second), plane(far)),
            ])
        }
    };
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer,
            holes: Vec::new(),
        }],
    };
    let sweep = length + reach * 2.0;
    // Built by the same route any other body is: the extrusion command, run
    // on an empty snapshot. Calling the construction helpers directly would
    // skip the normalizing and certifying the command does on the way, and a
    // tool that is a solid in every respect but one is exactly the kind that
    // fails much later and says something else.
    let empty = crate::NativeKernel::empty();
    let request = crate::ExecuteRequest {
        protocol_version: artificer_protocol::CURRENT_PROTOCOL_VERSION,
        request_id: artificer_protocol::RequestId::new("edge-finish-apart-tool"),
        expected_snapshot: empty.id(),
        precision,
        command: artificer_protocol::KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance: sweep,
        },
    };
    crate::NativeKernel::execute(&empty, &request, &crate::CancellationToken::new())
        .map(|outcome| outcome.snapshot)
        .map_err(|error| {
            refuse(
                "EDGE_FINISH_APART_CONSTRUCTION_FAILED",
                format!("The finish's own solid could not be built ({error})."),
            )
        })
}

fn straight(start: ProtocolPoint2, end: ProtocolPoint2) -> PlanarCurve2 {
    PlanarCurve2::Line { start, end }
}

/// The shorter of the two arcs between these points about this centre.
///
/// A convex corner's fillet always wants the minor arc — the one that bulges
/// towards the edge. The major arc through the far side of the circle is a
/// different shape entirely, and picking it silently is how a removal solid
/// stops being a fillet without anything saying so.
fn minor_arc(start: ProtocolPoint2, end: ProtocolPoint2, center: ProtocolPoint2) -> PlanarCurve2 {
    let angle = |point: ProtocolPoint2| (point.y - center.y).atan2(point.x - center.x);
    let mut sweep = angle(end) - angle(start);
    while sweep <= -std::f64::consts::PI {
        sweep += std::f64::consts::TAU;
    }
    while sweep > std::f64::consts::PI {
        sweep -= std::f64::consts::TAU;
    }
    PlanarCurve2::CircularArc {
        center,
        start,
        end,
        direction: if sweep >= 0.0 {
            ArcDirection::CounterClockwise
        } else {
            ArcDirection::Clockwise
        },
    }
}

/// The same loop, wound so it encloses material rather than a hole.
///
/// Which way round the two faces happen to sit decides the sign, so it is
/// measured rather than assumed: the chord polygon's signed area has the same
/// sign as the loop's, a minor arc never being enough to turn it.
fn wound(curves: Vec<PlanarCurve2>) -> PlanarLoop2 {
    let ends = |curve: &PlanarCurve2| match curve {
        PlanarCurve2::Line { start, end } => (*start, *end),
        PlanarCurve2::CircularArc { start, end, .. } => (*start, *end),
        _ => unreachable!("this tool builds only lines and circular arcs"),
    };
    let twice_area: f64 = curves
        .iter()
        .map(|curve| {
            let (start, end) = ends(curve);
            start.x.mul_add(end.y, -(end.x * start.y))
        })
        .sum();
    if twice_area >= 0.0 {
        return PlanarLoop2 { curves };
    }
    PlanarLoop2 {
        curves: curves
            .into_iter()
            .rev()
            .map(|curve| match curve {
                PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
                    start: end,
                    end: start,
                },
                PlanarCurve2::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => PlanarCurve2::CircularArc {
                    center,
                    start: end,
                    end: start,
                    direction: match direction {
                        ArcDirection::Clockwise => ArcDirection::CounterClockwise,
                        ArcDirection::CounterClockwise => ArcDirection::Clockwise,
                    },
                },
                other => other,
            })
            .collect(),
    }
}

/// The two faces an edge separates, if it separates exactly two.
fn faces_of_edge(topology: &Topology, edge: usize) -> Option<[usize; 2]> {
    let mut found = Vec::new();
    for (index, face) in topology.faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            let record = topology.loop_record(loop_key)?;
            for coedge_key in &record.value.coedges {
                let coedge = topology.coedge(*coedge_key)?;
                if coedge.value.edge.0 == edge && !found.contains(&index) {
                    found.push(index);
                }
            }
        }
    }
    match found.as_slice() {
        [first, second] => Some([*first, *second]),
        _ => None,
    }
}

fn difference(to: Point3, from: Point3) -> Vector3 {
    Vector3::new(to.x - from.x, to.y - from.y, to.z - from.z)
}

fn offset(point: Point3, by: Vector3) -> Point3 {
    Point3::new(point.x + by.x, point.y + by.y, point.z + by.z)
}

fn midpoint(first: Point3, second: Point3) -> Point3 {
    Point3::new(
        (first.x + second.x) * 0.5,
        (first.y + second.y) * 0.5,
        (first.z + second.z) * 0.5,
    )
}

fn scale(vector: Vector3, by: f64) -> Vector3 {
    Vector3::new(vector.x * by, vector.y * by, vector.z * by)
}

fn dot(left: Vector3, right: Vector3) -> f64 {
    left.x
        .mul_add(right.x, left.y.mul_add(right.y, left.z * right.z))
}

fn cross(left: Vector3, right: Vector3) -> Vector3 {
    Vector3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

fn magnitude(vector: Vector3) -> f64 {
    dot(vector, vector).sqrt()
}

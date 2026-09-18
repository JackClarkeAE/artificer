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
//! A bevel's removal is a half-space. It crosses every face it meets, so the
//! analytic Boolean takes it and the result is exact: three bevels off one
//! corner, taken one at a time, land on `3·½d²L − d³ + d³/4` to the last digit
//! the measure carries.
//!
//! A fillet's removal does not. The band is *tangent* to the two walls it
//! rolls between — that is what makes it a fillet — and the engine fails
//! closed on tangential contact between operands (ADR 0025). So a fillet
//! standing apart is refused here by name. The seam it would need is not the
//! obstacle: two crossing equal cylinders meet in a planar ellipse the
//! vocabulary already carries. What is missing is a Boolean that will accept
//! an operand touching the target along a line, or a direct construction that
//! re-trims the bands a previous feature committed.

use artificer_protocol::{
    BooleanOperation, EdgeFinishKind, EntityKind, EntityRef, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3,
    PrecisionPolicy, SnapshotId, Vector3 as ProtocolVector3,
};

use crate::analytic_boolean::{AnalyticBooleanError, build_analytic_boolean};
use crate::planar_profile::validate_linear_profile_extrusion;
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
    snapshot: SnapshotId,
    topology: &Topology,
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
        .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(refuse(
            "EDGE_FINISH_APART_TARGET_INVALID",
            "Every target must be an edge of the body this feature is being built on.",
        ));
    }
    if kind != EdgeFinishKind::Chamfer {
        return Err(refuse(
            "EDGE_FINISH_APART_FILLET_UNSUPPORTED",
            "A fillet standing apart is not built yet. Its band is tangent to the two walls it \
             rolls between, and this release's Boolean fails closed where two solids touch along \
             a line rather than crossing. Join this fillet to the finish that already shapes the \
             corner, or bevel the edge instead.",
        ));
    }
    if !distance.is_finite() || distance <= 0.0 {
        return Err(refuse(
            "EDGE_FINISH_APART_DISTANCE_INVALID",
            "A setback must be a positive length.",
        ));
    }
    let reach = body_reach(topology, distance);
    let mut body = topology.clone();
    for target in targets {
        let tool = bevel_tool(&body, *target, distance, reach, precision)?;
        body = match build_analytic_boolean(&body, &tool, BooleanOperation::Difference, precision) {
            Ok(cut) => cut,
            Err(AnalyticBooleanError::EmptyResult) => {
                return Err(refuse(
                    "EDGE_FINISH_APART_DISTANCE_INVALID",
                    format!(
                        "A setback of {distance:.6} would take the whole body away. Use a smaller \
                         one."
                    ),
                ));
            }
            Err(AnalyticBooleanError::DomainUnsupported) => {
                return Err(refuse(
                    "EDGE_FINISH_APART_DOMAIN_UNSUPPORTED",
                    format!(
                        "A bevel of {distance:.6} standing apart here would have to cut a face \
                         this release's Boolean cannot cut — a blend an earlier feature left, a \
                         hole wall, or a face it would touch rather than cross. Join this chamfer \
                         to the finish that already shapes the corner instead."
                    ),
                ));
            }
        };
        // Certify each cut before the next one builds on it. A Boolean can sew
        // a shell the validator will not take — a corner already rounded is
        // where that happens today — and the generic refusal that follows says
        // only that something failed. Saying it here means saying which route
        // failed and what to do instead.
        let report = crate::validator::validate(&body, precision.linear_agreement);
        if let Some(first) = report.diagnostics.first() {
            return Err(refuse(
                "EDGE_FINISH_APART_CONSTRUCTION_FAILED",
                format!(
                    "A bevel of {distance:.6} standing apart here was cut but did not certify                      ({} at {}). Nothing is published from a route that cannot prove its own                      answer: join this chamfer to the finish that already shapes the corner                      instead.",
                    first.code.as_str(),
                    first.path
                ),
            ));
        }
    }
    Ok(body)
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
fn bevel_tool(
    topology: &Topology,
    target: EntityRef,
    distance: f64,
    reach: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, ApartRefusal> {
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
    // Where the bevel meets each face, and so the line it cuts along.
    let anchor = endpoints[0];
    let toe = offset(anchor, scale(out_first, distance));
    let heel = offset(anchor, scale(out_second, distance));
    let base = difference(heel, toe);
    let width = magnitude(base);
    if width <= precision.linear_agreement {
        return Err(refuse(
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED",
            "The two faces at that edge lie flat against each other, so a bevel of them has no \
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
    // A section frame square to the edge: `u` along the bevel, `v` outwards,
    // and the sweep along the edge itself.
    let u = base;
    let v = cross(along, u);
    let apex = offset(midpoint(toe, heel), scale(backward, reach));
    let plane = |point: Point3| {
        let delta = difference(point, anchor);
        ProtocolPoint2::new(dot(delta, u), dot(delta, v))
    };
    let section = [
        plane(offset(toe, scale(base, -reach))),
        plane(offset(heel, scale(base, reach))),
        plane(apex),
    ];
    let start = offset(anchor, scale(along, -reach));
    let frame = PlanarFrame3::new(
        ProtocolPoint3::new(start.x, start.y, start.z),
        ProtocolVector3::new(u.x, u.y, u.z),
        ProtocolVector3::new(v.x, v.y, v.z),
    );
    // Wound so the section encloses material rather than a hole, whichever way
    // the two faces happened to sit.
    let mut corners = section.to_vec();
    let turn = (corners[1].x - corners[0].x) * (corners[2].y - corners[0].y)
        - (corners[1].y - corners[0].y) * (corners[2].x - corners[0].x);
    if turn < 0.0 {
        corners.reverse();
    }
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&corners),
            holes: Vec::new(),
        }],
    };
    validate_linear_profile_extrusion(
        target.snapshot,
        frame,
        &profile,
        length + reach * 2.0,
        precision,
    )
    .map(|extrusion| extrusion.topology)
    .map_err(|reason| {
        refuse(
            "EDGE_FINISH_APART_CONSTRUCTION_FAILED",
            format!("The bevel's own solid could not be built ({reason:?})."),
        )
    })
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

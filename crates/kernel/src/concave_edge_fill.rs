//! Fillets and chamfers on concave straight edges between flat faces, on any
//! planar body (ADR 0056, F2 and F3).
//!
//! Every other rung of the edge-finish ladder removes material. A finish of a
//! reflex edge adds it: the rolling ball sits in the air of the corner and
//! the band it sweeps fills the region the ball cannot reach — the square of
//! side `r` less its inscribed quarter disc at a right angle, in general
//! `r²·cot(α/2) − ½r²(π − α)` for an air wedge of angle `α`. Where the body
//! is a prism about the edge, the prism rung already answers this through the
//! 2D corner blend. Where it is not — a drilled block with a step in it, an
//! L with a pocket — nothing did, and the request was refused by name.
//!
//! The route here builds that corner region as a solid of its own, swept
//! exactly along the edge, and unions it into the body through the Boolean
//! ladder, which carries planes and cylinders and resolves the tangential
//! contact of the band with the two faces (ADR 0045). Two details make the
//! union land in the engine's domain rather than at its edge:
//!
//! * The filler's two flat sides do not lie *on* the body's faces, where a
//!   coincident face and a tangent one meet along the same line and the
//!   general engine refuses; they sit a margin inside the material, so the
//!   only contacts are the band's tangency and the caps.
//! * The filler runs exactly between the faces the edge ends on, which this
//!   slice asks to be flat and square to the edge — a cap, a floor, the
//!   top of a block — so its caps lie in those faces' planes and merge into
//!   them. An edge ending against a leaning face is refused by name.
//!
//! The union certifies itself: the material added is at most the corner
//! region swept along the edge, which is a closed form; a fill that adds
//! more has poked out of a wall thinner than its margin or run past the
//! faces beside it, and is refused rather than published.
//!
//! A selection may mix concave edges with convex ones. The concave edges are
//! filled first. A convex edge that meets a filled edge at a corner is then
//! cut standing apart (ADR 0044) with its removal tool bounded exactly at the
//! band's tangency plane — the "planar cap" corner, exact, a step face where
//! the convex band ends against the concave one. Every other edge of the
//! selection goes back to the ladder on the filled body.

use artificer_protocol::{
    BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EdgeFinishKind,
    EntityId as ProtocolEntityId, EntityKind, EntityRef, PlanarFrame3, PlanarProfile2,
    PlanarRegion2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy, RequestId,
    Vector3 as ProtocolVector3,
};

use crate::edge_finish_apart::{
    body_reach, edge_is_reflex, minor_arc, oriented_faces_of_edge, removal_tool_between, straight,
    wound,
};
use crate::topology::{Curve3, Edge, Point3, Surface, Topology, Vector3};
use crate::{KernelError, Snapshot};

/// Why a concave edge could not be filled, with the code and sentence the
/// ladder publishes.
#[derive(Clone, Debug)]
pub(crate) struct FillRefusal {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> FillRefusal {
    FillRefusal {
        code,
        message: message.into(),
    }
}

/// Splits a selection into the concave straight edges between two flat faces
/// and everything else, each in selection order and without repeats.
pub(crate) fn partition(
    topology: &Topology,
    targets: &[EntityRef],
    precision: PrecisionPolicy,
) -> (Vec<EntityRef>, Vec<EntityRef>) {
    let mut concave = Vec::new();
    let mut rest = Vec::new();
    for target in targets {
        if concave.contains(target) || rest.contains(target) {
            continue;
        }
        if edge_is_reflex(topology, *target, precision) == Some(true) {
            concave.push(*target);
        } else {
            rest.push(*target);
        }
    }
    (concave, rest)
}

/// The ladder, handed the edges of a selection that were not filled; it
/// answers with the body and the rung that certified it.
pub(crate) type Ladder<'a> =
    &'a mut dyn FnMut(&Snapshot, &[EntityRef]) -> Result<(Topology, &'static str), KernelError>;

/// The rung this route publishes under.
pub(crate) const RUNG: &str = "edge-finish/concave-fill";

/// Fills every concave edge of the selection, cuts the convex edges that meet
/// one, and finishes the rest on the filled body: by the ladder where it
/// answers, and standing apart (ADR 0044) where the ladder refuses because
/// a fill or a bounded cut has left a curved face at one of their corners.
///
/// The rung comes back with the body: this route's own unless the ladder
/// answered on its faceted tier, whose rung and caveat then stand.
pub(crate) fn finish_with_fills(
    input: &Snapshot,
    concave: &[EntityRef],
    rest: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
    ladder: Ladder<'_>,
) -> Result<(Topology, &'static str), FillRefusal> {
    if !distance.is_finite() || distance < precision.min_feature_size {
        return Err(refuse(
            "CONCAVE_EDGE_DISTANCE_INVALID",
            "A concave edge finish needs a size above the feature floor.",
        ));
    }
    // Everything is remembered by geometry before the body changes: the
    // Boolean renumbers every entity, and a filled corner shortens the edges
    // beside it, so each later edge is found again as the straight survivor
    // of the one selected.
    let originals = |targets: &[EntityRef]| -> Result<Vec<Edge>, FillRefusal> {
        targets
            .iter()
            .map(|target| {
                edge_record(&input.topology, *target).ok_or_else(|| {
                    refuse(
                        "CONCAVE_EDGE_TARGET_INVALID",
                        "Every target must be an edge of the body this feature is being built on.",
                    )
                })
            })
            .collect()
    };
    let concave_edges = originals(concave)?;
    let rest_edges = originals(rest)?;
    // Which of the other edges meet a concave one at a vertex, read from the
    // original body where the vertices are still shared.
    let adjacent: Vec<bool> = rest_edges
        .iter()
        .map(|edge| {
            concave_edges.iter().any(|filled| {
                filled
                    .vertices
                    .iter()
                    .any(|vertex| edge.vertices.contains(vertex))
            })
        })
        .collect();

    let mut body = input.clone();
    for original in &concave_edges {
        let index = successor_of(&body.topology, original, precision).ok_or_else(|| {
            refuse(
                "CONCAVE_EDGE_TARGET_INVALID",
                "A selected concave edge could not be found again on the body after an earlier \
                 fill changed it.",
            )
        })?;
        body = fill_one(&body, index, kind, distance, precision)?;
    }

    // Convex edges that meet a fill: cut standing apart, bounded where the
    // fill's band begins.
    let mut free = Vec::new();
    for (original, touches_fill) in rest_edges.iter().zip(adjacent) {
        let index = successor_of(&body.topology, original, precision).ok_or_else(|| {
            refuse(
                "CONCAVE_EDGE_TARGET_INVALID",
                "A selected edge could not be found again on the body after the concave edges \
                 beside it were filled.",
            )
        })?;
        if touches_fill {
            body = cut_against_fill(&body, index, kind, distance, precision)?;
        } else {
            free.push(entity_of(&body, index));
        }
    }
    if free.is_empty() {
        return Ok((body.topology, RUNG));
    }
    // The rest of the selection, on the filled body: found again by geometry
    // once more, because the bounded cuts renumbered everything again.
    let not_found = || {
        refuse(
            "CONCAVE_EDGE_TARGET_INVALID",
            "A selected edge could not be found again on the filled body.",
        )
    };
    let free_edges = free
        .iter()
        .map(|target| edge_record(&body.topology, *target))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(not_found)?;
    let mapped = free_edges
        .iter()
        .map(|edge| successor_of(&body.topology, edge, precision).map(|index| entity_of(&body, index)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(not_found)?;
    let ladder_refusal = match ladder(&body, &mapped) {
        Ok((topology, rung)) => {
            return Ok((
                topology,
                if rung.ends_with("/faceted") {
                    rung
                } else {
                    RUNG
                },
            ));
        }
        Err(error) => error,
    };
    // The ladder had no answer: a corner it owns now meets a band a fill or
    // a bounded cut left. Each remaining edge is then cut standing apart,
    // run out where it leaves the body and bounded where it meets a band,
    // which is ADR 0044's second answer and exact.
    for original in &free_edges {
        let index = successor_of(&body.topology, original, precision).ok_or_else(not_found)?;
        body = cut_against_fill(&body, index, kind, distance, precision).map_err(|refusal| {
            refuse(
                refusal.code,
                format!(
                    "{} (The ladder had refused the convex edges of this selection on the \
                     filled body: {ladder_refusal}.)",
                    refusal.message
                ),
            )
        })?;
    }
    Ok((body.topology, RUNG))
}

// ---------------------------------------------------------------------------
// Reading the edge
// ---------------------------------------------------------------------------

/// A concave straight edge between two flat faces, read from the topology.
struct ConcaveEdge {
    endpoints: [Point3; 2],
    length: f64,
    /// Unit direction from the first endpoint to the second.
    along: Vector3,
    /// Outward normals of the face that walks the edge forward and of the
    /// one that walks it in reverse, unit.
    normals: [Vector3; 2],
    /// The way along each of those faces away from the edge, unit.
    into: [Vector3; 2],
    /// Unit, into the air between the two faces.
    air: Vector3,
    /// The angle of the air wedge, below a half turn.
    exterior: f64,
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > f64::EPSILON).then(|| vector / length)
}

fn read_concave_edge(
    topology: &Topology,
    edge: usize,
    precision: PrecisionPolicy,
) -> Result<ConcaveEdge, FillRefusal> {
    let not_concave = || {
        refuse(
            "CONCAVE_EDGE_UNSUPPORTED",
            "A concave edge finish takes a straight edge between two flat faces whose dihedral \
             runs through the air, and this edge is not one.",
        )
    };
    let Curve3::Line { endpoints } = topology.edges[edge].value.curve else {
        return Err(not_concave());
    };
    let length = (endpoints[1] - endpoints[0]).length();
    if !length.is_finite() || length <= precision.min_feature_size {
        return Err(not_concave());
    }
    let along = (endpoints[1] - endpoints[0]) / length;
    let faces = oriented_faces_of_edge(topology, edge).ok_or_else(not_concave)?;
    let [Some(forward), Some(reverse)] =
        faces.map(|face| topology.faces[face].value.surface.as_plane())
    else {
        return Err(not_concave());
    };
    let normals = [
        unit(forward.normal).ok_or_else(not_concave)?,
        unit(reverse.normal).ok_or_else(not_concave)?,
    ];
    let into = [
        unit(normals[0].cross(along)).ok_or_else(not_concave)?,
        unit(normals[1].cross(along * -1.0)).ok_or_else(not_concave)?,
    ];
    // The interior dihedral through the material, as the stand-apart route
    // reads it; a reflex edge's is above a half turn, and the air wedge is
    // what is left of the turn.
    let sine = -normals[0].dot(into[1]);
    let cosine = into[0].dot(into[1]);
    let interior = sine.atan2(cosine).rem_euclid(std::f64::consts::TAU);
    let angle_tolerance = precision.angular_agreement_radians.max(1.0e-9);
    if interior <= std::f64::consts::PI + angle_tolerance
        || interior >= std::f64::consts::TAU - angle_tolerance
    {
        return Err(not_concave());
    }
    let exterior = std::f64::consts::TAU - interior;
    let air = unit(normals[0] + normals[1]).ok_or_else(not_concave)?;
    Ok(ConcaveEdge {
        endpoints,
        length,
        along,
        normals,
        into,
        air,
        exterior,
    })
}

/// The corner region's section: where the finish meets each face, measured
/// from the edge along it, and the ball centre for a fillet.
struct Section {
    /// How far along each face the band or bevel reaches.
    setback: f64,
    /// The ball's centre in the air, a fillet only.
    centre: Option<Point3>,
    /// The region's area, for the volume the fill may add.
    area: f64,
}

fn section(edge: &ConcaveEdge, kind: EdgeFinishKind, distance: f64) -> Section {
    let half = edge.exterior / 2.0;
    match kind {
        EdgeFinishKind::Fillet => {
            let setback = distance * half.cos() / half.sin();
            let centre = edge.endpoints[0] + edge.air * (distance / half.sin());
            let area = distance * distance * (half.cos() / half.sin())
                - 0.5 * distance * distance * (std::f64::consts::PI - edge.exterior);
            Section {
                setback,
                centre: Some(centre),
                area,
            }
        }
        EdgeFinishKind::Chamfer => Section {
            setback: distance,
            centre: None,
            area: 0.5 * distance * distance * edge.exterior.sin(),
        },
    }
}

// ---------------------------------------------------------------------------
// Filling one edge
// ---------------------------------------------------------------------------

/// The margin the filler's flat sides sit inside the material, as a fraction
/// of the finish, and how many times it is halved before giving up.
const MARGIN_FRACTION: f64 = 0.5;
const MARGIN_RETRIES: usize = 6;

fn fill_one(
    body: &Snapshot,
    index: usize,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Snapshot, FillRefusal> {
    let edge = read_concave_edge(&body.topology, index, precision)?;
    let section = section(&edge, kind, distance);
    if !section.setback.is_finite() || section.setback <= precision.min_feature_size {
        return Err(refuse(
            "CONCAVE_EDGE_DISTANCE_INVALID",
            "The finish would meet the faces beside that edge too close to it to leave a band.",
        ));
    }
    // Both ends must stop against a flat face square to the edge, so the
    // filler's caps lie in those faces' planes.
    for vertex in body.topology.edges[index].value.vertices {
        end_is_square(&body.topology, vertex.0, index, edge.along, precision)?;
    }
    let expected = section.area * edge.length;
    // The band has to land on the faces: a foot just inside either face,
    // partway along the edge, is in material or the finish is wider than
    // the face beside it.
    if !feet_seated(&body.topology, &edge, &section, precision) {
        return Err(refuse(
            "CONCAVE_EDGE_DISTANCE_INVALID",
            format!(
                "A finish of {distance:.6} along that concave edge would meet one of the faces \
                 beside it beyond that face's own edge. Use a smaller size."
            ),
        ));
    }
    let mut margin = distance * MARGIN_FRACTION;
    let mut last_refusal = None;
    for _ in 0..MARGIN_RETRIES {
        if !strips_inside(&body.topology, &edge, &section, margin) {
            margin *= 0.5;
            continue;
        }
        let filler = filler_solid(&edge, &section, kind, margin, precision)?;
        match combine(body, &filler, BooleanOperation::Union, precision) {
            Ok(filled) => {
                let added = filled.measures().volume - body.measures().volume;
                let slack = precision
                    .linear_agreement
                    .max(1.0e-9)
                    .mul_add(expected.max(body.measures().volume), 1.0e-9);
                if added > expected + slack {
                    return Err(refuse(
                        "CONCAVE_EDGE_DISTANCE_INVALID",
                        format!(
                            "A finish of {distance:.6} along that concave edge would add {added:.6} \
                             where its own corner region is {expected:.6}: it runs past the faces \
                             beside the edge, or through a wall thinner than itself. Use a smaller \
                             size."
                        ),
                    ));
                }
                if added < -slack {
                    return Err(refuse(
                        "CONCAVE_EDGE_FILL_FAILED",
                        format!(
                            "Filling that concave edge lost {:.6} of the body's volume, which a \
                             union cannot do; nothing is published from a route that cannot \
                             prove its own answer.",
                            -added
                        ),
                    ));
                }
                return Ok(filled);
            }
            Err(error) => {
                last_refusal = Some(error);
                margin *= 0.5;
            }
        }
    }
    Err(last_refusal.map_or_else(
        || {
            refuse(
                "CONCAVE_EDGE_WALL_TOO_THIN",
                "The faces beside that concave edge have too little material behind them for the \
                 fill to seat in: the wall is thinner than a small fraction of the finish.",
            )
        },
        |error| {
            refuse(
                "CONCAVE_EDGE_FILL_FAILED",
                format!(
                    "The filler for that concave edge could not be joined to the body: {error}."
                ),
            )
        },
    ))
}

/// Whether the vertex at one end of the edge is a corner where the edge's two
/// faces meet a third flat face square to the edge: three edges, three flat
/// faces, the third one's normal along the edge.
fn end_is_square(
    topology: &Topology,
    vertex: usize,
    edge: usize,
    along: Vector3,
    precision: PrecisionPolicy,
) -> Result<(), FillRefusal> {
    let incident: Vec<usize> = topology
        .edges
        .iter()
        .enumerate()
        .filter(|(_, record)| record.value.vertices.iter().any(|key| key.0 == vertex))
        .map(|(index, _)| index)
        .collect();
    let leaning = || {
        refuse(
            "CONCAVE_EDGE_END_UNSUPPORTED",
            "A concave edge finish stops where the edge does, against the face across its end, \
             and this release carries that end only where the face is flat and square to the \
             edge — a cap, a floor, the top of a block. One end of the selected edge is not: \
             it meets a leaning face, a curved one, or more than two other edges.",
        )
    };
    if incident.len() != 3 {
        return Err(leaning());
    }
    let own_faces = oriented_faces_of_edge(topology, edge).ok_or_else(leaning)?;
    let mut caps = Vec::new();
    for other in incident {
        if other == edge {
            continue;
        }
        let faces = oriented_faces_of_edge(topology, other).ok_or_else(leaning)?;
        for face in faces {
            if !own_faces.contains(&face) && !caps.contains(&face) {
                caps.push(face);
            }
        }
    }
    let [cap] = caps.as_slice() else {
        return Err(leaning());
    };
    let Surface::Plane(plane) = topology.faces[*cap].value.surface else {
        return Err(leaning());
    };
    let normal = unit(plane.normal).ok_or_else(leaning)?;
    let angle_tolerance = precision.angular_agreement_radians.max(1.0e-9);
    if 1.0 - normal.dot(along).abs() > angle_tolerance {
        return Err(leaning());
    }
    Ok(())
}

/// Where the probes along an edge are taken: partway along it, never at an
/// end, where a corner's own geometry would answer instead of the wall's.
const STATIONS: [f64; 3] = [0.2, 0.5, 0.8];

/// Whether every probe point, carried to each station along the edge, is
/// inside the body.
fn all_inside(topology: &Topology, edge: &ConcaveEdge, points: &[Point3]) -> bool {
    STATIONS.into_iter().all(|fraction| {
        let shift = edge.along * (edge.length * fraction);
        points.iter().all(|point| {
            crate::analytic_boolean::point_in_solid(topology, *point + shift) == Some(true)
        })
    })
}

/// Whether the band's feet land on the faces: each foot, a hair inside its
/// face, is in material.
fn feet_seated(
    topology: &Topology,
    edge: &ConcaveEdge,
    section: &Section,
    precision: PrecisionPolicy,
) -> bool {
    let hair = precision.min_feature_size.max(1.0e-9) * 8.0;
    let anchor = edge.endpoints[0];
    let feet = [
        anchor + edge.into[0] * section.setback + edge.normals[0] * -hair,
        anchor + edge.into[1] * section.setback + edge.normals[1] * -hair,
    ];
    all_inside(topology, edge, &feet)
}

/// Whether the filler's margin strips lie in material: their far corners,
/// sampled partway along the edge, are all inside the body.
fn strips_inside(topology: &Topology, edge: &ConcaveEdge, section: &Section, margin: f64) -> bool {
    all_inside(topology, edge, &strip_corners(edge, section, margin))
}

/// The three far corners of the margin strips behind the first endpoint: the
/// foot on each face pushed a margin into the material, and the inner corner
/// a margin behind both faces.
fn strip_corners(edge: &ConcaveEdge, section: &Section, margin: f64) -> [Point3; 3] {
    let anchor = edge.endpoints[0];
    let foot = |slot: usize| anchor + edge.into[slot] * section.setback;
    // The inner corner sits a margin behind both face planes: along the
    // material's own bisector, which is opposite the air's, far enough that
    // its distance to each plane is the margin.
    let half = edge.exterior / 2.0;
    let inner = anchor + edge.air * (-margin / half.sin());
    [
        foot(0) + edge.normals[0] * -margin,
        inner,
        foot(1) + edge.normals[1] * -margin,
    ]
}

/// The filler as a solid: the corner region with its margin strips, swept
/// exactly from the edge's first endpoint to its second.
fn filler_solid(
    edge: &ConcaveEdge,
    section: &Section,
    kind: EdgeFinishKind,
    margin: f64,
    precision: PrecisionPolicy,
) -> Result<Snapshot, FillRefusal> {
    let anchor = edge.endpoints[0];
    // A section frame square to the edge, laid on the first face so the
    // filler's flanks come out axis-aligned in it.
    let u = edge.into[0];
    let v = edge.along.cross(u);
    let plane = |point: Point3| {
        let delta = point - anchor;
        ProtocolPoint2::new(delta.dot(u), delta.dot(v))
    };
    let [behind_first, inner, behind_second] = strip_corners(edge, section, margin);
    let first_foot = anchor + edge.into[0] * section.setback;
    let second_foot = anchor + edge.into[1] * section.setback;
    let closing = match (kind, section.centre) {
        (EdgeFinishKind::Fillet, Some(centre)) => {
            minor_arc(plane(second_foot), plane(first_foot), plane(centre))
        }
        _ => straight(plane(second_foot), plane(first_foot)),
    };
    let outer = wound(vec![
        straight(plane(first_foot), plane(behind_first)),
        straight(plane(behind_first), plane(inner)),
        straight(plane(inner), plane(behind_second)),
        straight(plane(behind_second), plane(second_foot)),
        closing,
    ]);
    let frame = PlanarFrame3::new(
        ProtocolPoint3::new(anchor.x, anchor.y, anchor.z),
        ProtocolVector3::new(u.x, u.y, u.z),
        ProtocolVector3::new(v.x, v.y, v.z),
    );
    let empty = crate::NativeKernel::empty();
    let request = crate::ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("concave-edge-filler"),
        expected_snapshot: empty.id(),
        precision,
        command: artificer_protocol::KernelCommand::ExtrudePlanarProfile {
            frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer,
                    holes: Vec::new(),
                }],
            },
            distance: edge.length,
        },
    };
    crate::NativeKernel::execute(&empty, &request, &crate::CancellationToken::new())
        .map(|outcome| outcome.snapshot)
        .map_err(|error| {
            refuse(
                "CONCAVE_EDGE_FILL_FAILED",
                format!("The filler for that concave edge could not be built ({error})."),
            )
        })
}

/// One Boolean through the engine's own dispatch, certified before it is
/// built on.
fn combine(
    body: &Snapshot,
    tool: &Snapshot,
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Snapshot, String> {
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("concave-edge-fill"),
        expected_target_snapshot: body.id(),
        expected_tool_snapshot: tool.id(),
        precision,
        operation,
    };
    let outcome =
        crate::NativeKernel::execute_boolean(body, tool, &request, &crate::CancellationToken::new())
            .map_err(|error| error.to_string())?;
    if outcome
        .report
        .rung
        .as_deref()
        .is_some_and(|rung| rung.ends_with("/faceted"))
    {
        return Err("the Boolean fell to the faceted tier".to_owned());
    }
    let report = crate::validator::validate(&outcome.snapshot.topology, precision.linear_agreement);
    if let Some(first) = report.diagnostics.first() {
        return Err(format!(
            "the result did not certify ({} at {})",
            first.code.as_str(),
            first.path
        ));
    }
    Ok(outcome.snapshot)
}

// ---------------------------------------------------------------------------
// Convex edges that meet a fill
// ---------------------------------------------------------------------------

/// Cuts a convex edge standing apart, with its removal tool bounded exactly
/// at every end that stops against a band a fill left, and run out past
/// every end that leaves the body.
fn cut_against_fill(
    body: &Snapshot,
    index: usize,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Snapshot, FillRefusal> {
    let record = body.topology.edges[index].value;
    let Curve3::Line { endpoints } = record.curve else {
        return Err(refuse(
            "CONCAVE_EDGE_MIXED_SELECTION_UNSUPPORTED",
            "A curved edge meets a concave edge of this selection at a corner, and this release \
             finishes the two in separate features.",
        ));
    };
    let reach = body_reach(&body.topology, distance);
    let mut overshoot = [reach; 2];
    for (slot, vertex) in record.vertices.iter().enumerate() {
        // An end that leaves the body is run out past, as the stand-apart
        // route runs every end; one that stops against material does so
        // where a band begins, and the tool's cap is that band's tangency
        // plane. Anything else has nowhere to end.
        if end_runs_out(&body.topology, &record, slot, distance, precision) {
            continue;
        }
        // A fill's band is a cylinder; a fill's bevel is a plane that leans
        // across the edge, unlike the edge's own faces and unlike a cap
        // square to it.
        let own_faces = oriented_faces_of_edge(&body.topology, index).unwrap_or([usize::MAX; 2]);
        let angle_tolerance = precision.angular_agreement_radians.max(1.0e-9);
        let along = unit(endpoints[1] - endpoints[0]).unwrap_or_default();
        let touches_band = body.topology.faces.iter().enumerate().any(|(face, record)| {
            if own_faces.contains(&face) || !face_has_vertex(&body.topology, face, vertex.0) {
                return false;
            }
            match record.value.surface {
                Surface::Cylinder(_) => true,
                Surface::Plane(plane) => unit(plane.normal).is_some_and(|normal| {
                    let cosine = normal.dot(along).abs();
                    cosine > angle_tolerance && cosine < 1.0 - angle_tolerance
                }),
                _ => false,
            }
        });
        if touches_band {
            overshoot[slot] = 0.0;
        } else {
            return Err(refuse(
                "CONCAVE_EDGE_MIXED_SELECTION_UNSUPPORTED",
                "A convex edge of this selection meets a concave one at one corner and runs on \
                 into material at the other, so its band has nowhere to end. Finish the concave \
                 edges as their own feature and the convex ones afterwards.",
            ));
        }
    }
    let target = entity_of(body, index);
    let tool = removal_tool_between(&body.topology, target, kind, distance, overshoot, precision)
        .map_err(|error| {
            refuse(
                "CONCAVE_EDGE_MIXED_SELECTION_UNSUPPORTED",
                format!(
                    "A convex edge of this selection meets a concave one, and its band could not \
                     be cut against the fill: {}",
                    error.message
                ),
            )
        })?;
    let cut = combine(body, &tool, BooleanOperation::Difference, precision).map_err(|error| {
        refuse(
            "CONCAVE_EDGE_MIXED_SELECTION_UNSUPPORTED",
            format!(
                "A convex edge of this selection meets a concave one, and its band could not be \
                 cut against the fill: {error}. Finish the concave edges as their own feature \
                 and the convex ones afterwards."
            ),
        )
    })?;
    // The cut may take at most the convex corner region along the tool.
    let length = (endpoints[1] - endpoints[0]).length() + overshoot[0] + overshoot[1];
    let removed = body.measures().volume - cut.measures().volume;
    let most = convex_corner_area(&body.topology, index, kind, distance, precision) * length;
    let slack = precision
        .linear_agreement
        .max(1.0e-9)
        .mul_add(body.measures().volume, 1.0e-9);
    if removed < -slack || removed > most + slack {
        return Err(refuse(
            "CONCAVE_EDGE_MIXED_SELECTION_UNSUPPORTED",
            format!(
                "Cutting a convex edge of this selection against the fill beside it removed \
                 {removed:.6} where its own corner region is at most {most:.6}; nothing is \
                 published from a route that cannot prove its own answer."
            ),
        ));
    }
    Ok(cut)
}

/// The section of a convex edge's removal: the corner a ball cannot reach
/// inside a material wedge of interior angle `θ`, `r²·cot(θ/2) − ½r²(π − θ)`,
/// or a bevel's triangle `½d²·sin θ`.
fn convex_corner_area(
    topology: &Topology,
    edge: usize,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> f64 {
    let Some(faces) = oriented_faces_of_edge(topology, edge) else {
        return f64::INFINITY;
    };
    let Curve3::Line { endpoints } = topology.edges[edge].value.curve else {
        return f64::INFINITY;
    };
    let [Some(forward), Some(reverse)] =
        faces.map(|face| topology.faces[face].value.surface.as_plane())
    else {
        return f64::INFINITY;
    };
    let (Some(along), Some(first), Some(second)) = (
        unit(endpoints[1] - endpoints[0]),
        unit(forward.normal),
        unit(reverse.normal),
    ) else {
        return f64::INFINITY;
    };
    let (Some(into_first), Some(into_second)) =
        (unit(first.cross(along)), unit(second.cross(along * -1.0)))
    else {
        return f64::INFINITY;
    };
    let sine = -first.dot(into_second);
    let cosine = into_first.dot(into_second);
    let interior = sine.atan2(cosine).rem_euclid(std::f64::consts::TAU);
    if interior <= precision.angular_agreement_radians.max(1.0e-9)
        || interior >= std::f64::consts::PI
    {
        return f64::INFINITY;
    }
    let half = interior / 2.0;
    match kind {
        EdgeFinishKind::Fillet => {
            distance * distance * (half.cos() / half.sin())
                - 0.5 * distance * distance * (std::f64::consts::PI - interior)
        }
        EdgeFinishKind::Chamfer => 0.5 * distance * distance * interior.sin(),
    }
}

/// Whether the edge, carried on past the end at `slot`, leaves the body:
/// a point a finish's width past that end, set into the material wedge, is
/// outside. This is the stand-apart route's own test, taken one end at a
/// time.
fn end_runs_out(
    topology: &Topology,
    record: &Edge,
    slot: usize,
    distance: f64,
    precision: PrecisionPolicy,
) -> bool {
    let Curve3::Line { endpoints } = record.curve else {
        return false;
    };
    let Some(along) = unit(endpoints[1] - endpoints[0]) else {
        return false;
    };
    let index = topology
        .edges
        .iter()
        .position(|candidate| candidate.value.vertices == record.vertices);
    let Some(index) = index else {
        return false;
    };
    let Some(faces) = oriented_faces_of_edge(topology, index) else {
        return false;
    };
    let [Some(forward), Some(reverse)] =
        faces.map(|face| topology.faces[face].value.surface.as_plane())
    else {
        return false;
    };
    let (Some(first), Some(second)) = (unit(forward.normal), unit(reverse.normal)) else {
        return false;
    };
    let (Some(into_first), Some(into_second)) =
        (unit(first.cross(along)), unit(second.cross(along * -1.0)))
    else {
        return false;
    };
    let Some(bisector) = unit(into_first + into_second) else {
        return false;
    };
    let _ = precision;
    let step = if slot == 0 { -distance } else { distance };
    let probe = endpoints[slot] + along * step + bisector * (distance * 0.25);
    crate::analytic_boolean::point_in_solid(topology, probe) == Some(false)
}

fn face_has_vertex(topology: &Topology, face: usize, vertex: usize) -> bool {
    topology.faces[face].value.loops().any(|loop_key| {
        topology.loops[loop_key.0].value.coedges.iter().any(|coedge| {
            let edge = topology.coedges[coedge.0].value.edge;
            topology.edges[edge.0]
                .value
                .vertices
                .iter()
                .any(|key| key.0 == vertex)
        })
    })
}

// ---------------------------------------------------------------------------
// Finding edges again
// ---------------------------------------------------------------------------

fn edge_record(topology: &Topology, target: EntityRef) -> Option<Edge> {
    if target.kind != EntityKind::Edge {
        return None;
    }
    topology
        .edges
        .iter()
        .find(|edge| edge.id.get() == target.entity.0)
        .map(|edge| edge.value)
}

fn entity_of(body: &Snapshot, index: usize) -> EntityRef {
    EntityRef {
        snapshot: body.id(),
        entity: ProtocolEntityId(body.topology.edges[index].id.get()),
        kind: EntityKind::Edge,
    }
}

/// The edge of the body that stands where `original` did: for a straight
/// edge, the collinear one overlapping it most — a fill shortens the edges
/// beside its corner, and the survivor lies inside what was selected; for
/// any other curve, the one that evaluates to the same points.
fn successor_of(topology: &Topology, original: &Edge, precision: PrecisionPolicy) -> Option<usize> {
    let tolerance = precision
        .modeling_resolution
        .max(precision.linear_agreement)
        * 64.0;
    if let Curve3::Line { endpoints } = original.curve {
        let direction = unit(endpoints[1] - endpoints[0])?;
        let length = (endpoints[1] - endpoints[0]).length();
        let line_distance = |point: Point3| {
            let relative = point - endpoints[0];
            (relative - direction * relative.dot(direction)).length()
        };
        return topology
            .edges
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let Curve3::Line {
                    endpoints: [start, end],
                } = candidate.value.curve
                else {
                    return None;
                };
                let vector = end - start;
                let span = vector.length();
                if span <= precision.min_feature_size
                    || direction.cross(vector / span).length() > 1.0e-6
                    || line_distance(start).max(line_distance(end)) > tolerance
                {
                    return None;
                }
                let first = (start - endpoints[0]).dot(direction);
                let second = (end - endpoints[0]).dot(direction);
                let overlap = second.max(first).min(length) - second.min(first).max(0.0);
                (overlap > precision.min_feature_size).then_some((overlap, index))
            })
            .max_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, index)| index);
    }
    let samples = [0.0, 0.37, 0.71, 1.0].map(|fraction| {
        original.curve.evaluate(
            (original.parameter_range.end - original.parameter_range.start)
                .mul_add(fraction, original.parameter_range.start),
        )
    });
    topology.edges.iter().position(|candidate| {
        let range = candidate.value.parameter_range;
        let same_kind = std::mem::discriminant(&candidate.value.curve)
            == std::mem::discriminant(&original.curve);
        same_kind
            && samples.iter().all(|sample| {
                // Either way round.
                [0.0, 0.37, 0.71, 1.0].iter().any(|fraction| {
                    let point = candidate
                        .value
                        .curve
                        .evaluate((range.end - range.start).mul_add(*fraction, range.start));
                    point.distance(*sample) <= tolerance
                }) || {
                    let flipped = [1.0, 0.63, 0.29, 0.0];
                    flipped.iter().any(|fraction| {
                        let point = candidate
                            .value
                            .curve
                            .evaluate((range.end - range.start).mul_add(*fraction, range.start));
                        point.distance(*sample) <= tolerance
                    })
                }
            })
    })
}

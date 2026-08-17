//! Why a filleted slot's presentation outline breaks.
//!
//! A slot extrudes to two planar walls joined by two half-cylinders. Filleting
//! the top rim replaces that rim with torus and cylinder blends, and where the
//! straight run meets the round end three blends converge on one vertex.
//!
//! `presentation_smooth_edge_flags` hides blend seams so a fillet does not draw
//! a wireframe of its own facets, then promotes hidden edges back where a
//! visible rail is left dangling. On this body it under-promotes: rails stop in
//! mid-air on the cap.
//!
//! This probe reports the numbers that decide the fix rather than a picture of
//! the symptom — for every free rail end, how many rails arrive visible and how
//! many hidden, and whether every display edge carries both of its incident
//! faces. If no hidden candidate exists at all the gap is missing topology and
//! no promotion rule can close it; if the faces are missing, the viewport has
//! no rule to draw the edge in the first place.

use std::collections::BTreeMap;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const HEIGHT: f64 = 10.0;
const HALF_LENGTH: f64 = 12.0;
const RADIUS: f64 = 6.0;
const FILLET: f64 = 2.0;

/// A slot: two straight runs closed by two half-circles.
fn slot_profile() -> PlanarProfile2 {
    let right = Point2::new(HALF_LENGTH, 0.0);
    let left = Point2::new(-HALF_LENGTH, 0.0);
    let curves = vec![
        PlanarCurve2::Line {
            start: Point2::new(-HALF_LENGTH, -RADIUS),
            end: Point2::new(HALF_LENGTH, -RADIUS),
        },
        PlanarCurve2::CircularArc {
            center: right,
            start: Point2::new(HALF_LENGTH, -RADIUS),
            end: Point2::new(HALF_LENGTH, RADIUS),
            direction: ArcDirection::CounterClockwise,
        },
        PlanarCurve2::Line {
            start: Point2::new(HALF_LENGTH, RADIUS),
            end: Point2::new(-HALF_LENGTH, RADIUS),
        },
        PlanarCurve2::CircularArc {
            center: left,
            start: Point2::new(-HALF_LENGTH, RADIUS),
            end: Point2::new(-HALF_LENGTH, -RADIUS),
            direction: ArcDirection::CounterClockwise,
        },
    ];
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves },
            holes: vec![],
        }],
    }
}

fn extruded_slot() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("slot-extrude"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: slot_profile(),
            distance: HEIGHT,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a slot profile must extrude")
        .snapshot
}

fn top_rim(snapshot: &Snapshot) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut seen = Vec::new();
    for edge in &scene.edges {
        let [first, second] = edge.endpoints;
        if (first.z - HEIGHT).abs() < 1.0e-9
            && (second.z - HEIGHT).abs() < 1.0e-9
            && !seen.contains(&edge.source_edge)
        {
            seen.push(edge.source_edge);
        }
    }
    seen
}

fn filleted_slot() -> Snapshot {
    let base = extruded_slot();
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("slot-rim-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: top_rim(&base),
            kind: EdgeFinishKind::Fillet,
            distance: FILLET,
        },
    };
    NativeKernel::execute(&base, &request, &CancellationToken::new())
        .expect("a slot's top rim must fillet")
        .snapshot
}

/// Quantised position, so the two ends of touching edges land in one bucket.
type VertexKey = (i64, i64, i64);

fn key(point: artificer_protocol::Point3) -> VertexKey {
    let quantise = |value: f64| (value * 1.0e7).round() as i64;
    (quantise(point.x), quantise(point.y), quantise(point.z))
}

/// Whether the triangles on *both* sides of an edge carry it.
///
/// The viewport removes hidden lines by testing each edge against the body's
/// triangles, excluding only triangles for which that exact edge is one of
/// their three edges — `triangle.model_edges.contains(&edge)`. Its own comment
/// names the hazard: an edge's incident facets "are coplanar with it by
/// construction and would otherwise self-occlude".
///
/// That exclusion is an exact endpoint match. If the two faces meeting along a
/// curved boundary tessellate it into *different* chord sets, then a chord
/// belonging to one face is not an edge of any triangle on the other, so those
/// neighbouring near-coplanar triangles become eligible occluders — and the
/// depth bias deciding between them is two parts per million of the depth span,
/// far below the chord error. An arc then survives chord by chord depending on
/// which side happens to land nearer, which is exactly a curve that renders
/// partway and stops.
#[test]
fn every_display_edge_is_carried_by_triangles_on_both_of_its_faces() {
    let filleted = filleted_slot();
    let scene = NativeKernel::display_scene_scaled(&filleted, 1.0);

    // Every triangle edge, keyed the way the viewport keys them: by endpoints.
    let mut carried = BTreeMap::<(VertexKey, VertexKey), Vec<EntityRef>>::new();
    for triangle in &scene.triangles {
        for pair in [[0, 1], [1, 2], [2, 0]] {
            let (a, b) = (
                key(triangle.vertices[pair[0]]),
                key(triangle.vertices[pair[1]]),
            );
            let ordered = if a <= b { (a, b) } else { (b, a) };
            carried
                .entry(ordered)
                .or_default()
                .push(triangle.source_face);
        }
    }

    let mut uncarried = 0_usize;
    let mut one_sided = 0_usize;
    for edge in &scene.edges {
        let (a, b) = (key(edge.endpoints[0]), key(edge.endpoints[1]));
        let ordered = if a <= b { (a, b) } else { (b, a) };
        let Some(faces) = carried.get(&ordered) else {
            uncarried += 1;
            continue;
        };
        let incident = edge
            .incident_faces
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let covered = incident.iter().filter(|face| faces.contains(face)).count();
        if covered < incident.len() {
            one_sided += 1;
        }
    }

    println!(
        "carrier check: {} display edges · {uncarried} matched by no triangle at all · \
         {one_sided} carried by only one of their two faces",
        scene.edges.len()
    );
    assert_eq!(
        uncarried, 0,
        "a display edge with no triangle along it has no surface to lie on"
    );
    // The mismatch itself is not a defect to remove here: two faces meeting on a
    // curve are free to chord it differently, and forcing a shared subdivision
    // would constrain tessellation for a rendering reason. What must hold is the
    // precondition the viewport's fix relies on — an edge its own facets fail to
    // carry has to name both faces, so the occlusion pass can exclude them by
    // identity instead of by matching endpoints.
    let unprotectable = scene
        .edges
        .iter()
        .filter(|edge| edge.incident_faces.iter().any(Option::is_none))
        .count();
    assert_eq!(
        unprotectable, 0,
        "{one_sided} edges are carried by one face only, so every edge must name \
         both of its faces for the topological exclusion to protect them"
    );
}

#[test]
fn slot_fillet_presentation_outline_is_continuous() {
    let filleted = filleted_slot();
    assert!(
        NativeKernel::validate(&filleted, ValidationProfile::Solid).valid,
        "the probe body itself must be a valid solid before its edges mean anything"
    );

    let scene = NativeKernel::display_scene_scaled(&filleted, 1.0);
    let mut at_vertex = BTreeMap::<VertexKey, Vec<usize>>::new();
    for (index, edge) in scene.edges.iter().enumerate() {
        for endpoint in edge.endpoints {
            at_vertex.entry(key(endpoint)).or_default().push(index);
        }
    }

    // A rail with a genuinely free end, not merely a corner. At a 90° corner no
    // rail continues another, so "nothing continues me" flags every corner on
    // the body and says nothing. A *break* is a vertex where one visible edge
    // terminates and nothing else meets it at all.
    let mut breaks = Vec::new();
    for (vertex, incident) in &at_vertex {
        let visible = incident
            .iter()
            .copied()
            .filter(|index| !scene.edges[*index].is_smooth)
            .count();
        let hidden = incident.len() - visible;
        if visible == 1 {
            breaks.push((*vertex, visible, hidden, incident.len()));
        }
    }

    let total = scene.edges.len();
    let smooth = scene.edges.iter().filter(|edge| edge.is_smooth).count();
    // The viewport decides outline-vs-crease per frame from `incident_faces`.
    // An edge missing one cannot be classified, and an unclassifiable edge is
    // one the viewport has no rule to draw.
    let orphaned = scene
        .edges
        .iter()
        .filter(|edge| edge.incident_faces.iter().any(Option::is_none))
        .count();
    let one_sided = scene
        .edges
        .iter()
        .filter(|edge| {
            edge.incident_faces
                .iter()
                .filter(|face| face.is_some())
                .count()
                == 1
        })
        .count();
    println!(
        "slot fillet: {total} display edges, {smooth} hidden as smooth, {} vertices\n\
         incident faces: {orphaned} edges missing at least one, {one_sided} with exactly one",
        at_vertex.len()
    );
    assert_eq!(
        orphaned, 0,
        "every display edge needs both incident faces or the viewport cannot classify it"
    );

    if !breaks.is_empty() {
        let mut report = format!(
            "{} free rail end(s) on a filleted slot's outline\n\
             vertex (mm) | visible | hidden | total incident\n",
            breaks.len()
        );
        for (vertex, visible, hidden, incident) in &breaks {
            report.push_str(&format!(
                "({:8.3},{:8.3},{:8.3}) | {visible:^7} | {hidden:^6} | {incident:^14}\n",
                vertex.0 as f64 / 1.0e7,
                vertex.1 as f64 / 1.0e7,
                vertex.2 as f64 / 1.0e7,
            ));
        }
        panic!("{report}");
    }
}

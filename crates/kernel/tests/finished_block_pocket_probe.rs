//! Chamfering a hexagonal pocket sunk into a block that already carries
//! finishes.
//!
//! A pocket cut into a pristine block chamfers cleanly. The reported part is
//! not that: its cube was filleted on some edges and chamfered on others
//! before the bores went through it, so the pocket's rim edges belong to a
//! body the regularized path has already rebuilt once. This probe reproduces
//! that order of operations.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};

const SIZE: f64 = 40.0;
const POCKET_DEPTH: f64 = 6.0;
const POCKET_HALF: f64 = 10.0;
const FINISH: f64 = 3.0;

fn execute(snapshot: &Snapshot, label: &str, command: KernelCommand) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
        .snapshot
}

fn finish(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
    label: &str,
) -> Result<Snapshot, artificer_protocol::KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: targets,
            kind,
            distance,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

/// The vertical edge standing at one corner of the block.
fn vertical_edge_at(snapshot: &Snapshot, x: f64, y: f64) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .edges
        .iter()
        .find(|edge| {
            edge.endpoints
                .iter()
                .all(|point| (point.x - x).abs() < 1.0e-9 && (point.y - y).abs() < 1.0e-9)
        })
        .unwrap_or_else(|| panic!("the block should stand an edge at ({x}, {y})"))
        .source_edge
}

fn top_face(snapshot: &Snapshot, height: f64) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .triangles
        .iter()
        .find(|triangle| {
            triangle
                .vertices
                .iter()
                .all(|vertex| (vertex.z - height).abs() < 1.0e-6)
        })
        .expect("the block should expose its top face")
        .source_face
}

/// A cube filleted on one vertical edge, chamfered on the opposite one, then
/// sunk with a hexagonal pocket — the reported part's order of operations.
fn finished_block_with_hexagonal_pocket() -> Snapshot {
    let block = execute(
        &NativeKernel::empty(),
        "finished-block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
    );
    let filleted = finish(
        &block,
        vec![vertical_edge_at(&block, 0.0, 0.0)],
        EdgeFinishKind::Fillet,
        FINISH,
        "block-fillet",
    )
    .expect("a cube's vertical edge should fillet");
    let chamfered = finish(
        &filleted,
        vec![vertical_edge_at(&filleted, SIZE, SIZE)],
        EdgeFinishKind::Chamfer,
        FINISH,
        "block-chamfer",
    )
    .expect("the opposite vertical edge should chamfer");

    let corner = |step: usize| {
        let angle = std::f64::consts::TAU * (step % 6) as f64 / 6.0;
        Point2::new(POCKET_HALF * angle.cos(), POCKET_HALF * angle.sin())
    };
    let hexagon = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: (0..6)
                    .map(|index| PlanarCurve2::Line {
                        start: corner(index),
                        end: corner(index + 1),
                    })
                    .collect(),
            },
            holes: vec![],
        }],
    };
    execute(
        &chamfered,
        "hex-pocket",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face(&chamfered, SIZE),
            frame: PlanarFrame3::new(
                Point3::new(SIZE / 2.0, SIZE / 2.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: hexagon,
            distance: POCKET_DEPTH,
            operation: FaceExtrusionOperation::Cut,
        },
    )
}

fn hexagon_rim_edges(snapshot: &Snapshot) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut found = Vec::new();
    for edge in &scene.edges {
        let rim = edge.endpoints.iter().all(|point| {
            (point.z - SIZE).abs() < 1.0e-9
                && (point.x - SIZE / 2.0).hypot(point.y - SIZE / 2.0) <= POCKET_HALF + 1.0e-6
        });
        if rim && !found.contains(&edge.source_edge) {
            found.push(edge.source_edge);
        }
    }
    assert_eq!(found.len(), 6, "a hexagonal pocket has six rim edges");
    found
}

/// The two cases from the report, on a body that already carries finishes:
/// two neighbouring sides of the pocket's rim, then the whole rim at once.
#[test]
fn a_hexagonal_pocket_in_a_finished_block_chamfers_in_part_and_in_whole() {
    let part = finished_block_with_hexagonal_pocket();
    let rim = hexagon_rim_edges(&part);
    let volume = |snapshot: &Snapshot| snapshot.measures().volume;
    let before = volume(&part);
    let distance = 1.5;
    let side = 2.0 * POCKET_HALF * (std::f64::consts::PI / 6.0).tan();
    let wedge = distance * distance / 2.0 * side;

    let pair = finish(
        &part,
        vec![rim[0], rim[1]],
        EdgeFinishKind::Chamfer,
        distance,
        "finished-hex-two-sides",
    )
    .expect("two neighbouring pocket sides should chamfer");
    let removed = before - volume(&pair);
    assert!(
        removed > wedge * 1.5 && removed < wedge * 2.5,
        "two sides should remove about two wedges of {wedge}, removed {removed}"
    );

    let whole = finish(
        &part,
        rim,
        EdgeFinishKind::Chamfer,
        distance,
        "finished-hex-whole-rim",
    )
    .expect("the whole pocket rim should chamfer as one feature");
    let removed = before - volume(&whole);
    assert!(
        removed > wedge * 5.0 && removed < wedge * 7.0,
        "the whole rim should remove about six wedges of {wedge}, removed {removed}"
    );
}

//! Chamfering the rim of a polygonal pocket.
//!
//! A pocket's rim edge is convex, with the cap on one side and a wall on the
//! other that is a fraction of the block around it. Both the part selection
//! and the whole closed loop must come back as a solid that removes the wedges
//! it describes and nothing else. Every measure below is derived from the
//! pocket's own dimensions rather than read back from the kernel.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};

const BLOCK: [f64; 3] = [80.0, 50.0, 20.0];
const POCKET_DEPTH: f64 = 4.0;
const POCKET_HALF: f64 = 12.0;

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
        .expect("the block should expose a face at that height")
        .source_face
}

/// A hexagonal pocket, sunk into the same block.
fn hexagon_pocketed_block() -> Snapshot {
    let block = execute(
        &NativeKernel::empty(),
        "hex-block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: BLOCK[0],
            size_y: BLOCK[1],
            size_z: BLOCK[2],
        },
    );
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
        &block,
        "hex-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face(&block, BLOCK[2]),
            frame: PlanarFrame3::new(
                Point3::new(40.0, 25.0, BLOCK[2]),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: hexagon,
            distance: POCKET_DEPTH,
            operation: FaceExtrusionOperation::Cut,
        },
    )
}

/// Every rim edge of the hexagonal pocket, in no particular order.
fn hexagon_rim_edges(snapshot: &Snapshot) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut found = Vec::new();
    for edge in &scene.edges {
        let rim = edge.endpoints.iter().all(|point| {
            (point.z - BLOCK[2]).abs() < 1.0e-9
                && (point.x - 40.0).hypot(point.y - 25.0) <= POCKET_HALF + 1.0e-6
        });
        if rim && !found.contains(&edge.source_edge) {
            found.push(edge.source_edge);
        }
    }
    assert_eq!(found.len(), 6, "a hexagonal pocket has six rim edges");
    found
}

fn chamfer_set(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
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
            kind: EdgeFinishKind::Chamfer,
            distance,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

/// The two cases from the report: two neighbouring sides of a hexagonal
/// pocket, then the whole rim at once. Both used to leave a body the
/// validator refused, so the preview came back empty and the command read as
/// "the kernel will not do this".
#[test]
fn a_hexagonal_pocket_rim_chamfers_in_part_and_in_whole() {
    let pocketed = hexagon_pocketed_block();
    let rim = hexagon_rim_edges(&pocketed);
    let volume = |snapshot: &Snapshot| snapshot.measures().volume;
    let before = volume(&pocketed);
    let distance = 1.5;
    let side = 2.0 * POCKET_HALF * (std::f64::consts::PI / 6.0).tan();

    let pair = chamfer_set(
        &pocketed,
        vec![rim[0], rim[1]],
        distance,
        "hex-pocket-two-sides",
    )
    .expect("two neighbouring pocket sides should chamfer");
    let removed = before - volume(&pair);
    let wedge = distance * distance / 2.0 * side;
    // Two wedges less the one mitre they share: the corner between them is
    // removed once, not twice. Over two full wedges would mean no mitre at
    // all, and each sweep had simply run on past the corner into the wall
    // beside it.
    assert!(
        removed > wedge * 1.75 && removed < wedge * 2.0,
        "two sides should remove two wedges of {wedge} less their shared mitre, removed {removed}"
    );

    let whole = chamfer_set(&pocketed, rim, distance, "hex-pocket-whole-rim")
        .expect("the whole pocket rim should chamfer as one feature");
    let removed = before - volume(&whole);
    // Closed, every corner is a mitre and the band is the prismatoid between
    // the rim and its outward offset — which for a regular hexagon at this
    // setback comes to six wedges.
    assert!(
        ((removed - wedge * 6.0) / (wedge * 6.0)).abs() < 0.01,
        "the whole rim should remove six wedges of {wedge}, removed {removed}"
    );
}

/// One rim edge of a square pocket, chamfered on its own.
///
/// This used to publish a shell the validator refused outright: eight edges
/// used once where every edge must be used twice. Two evaluations of the same
/// intersection differing in the last bit were rounded into neighbouring
/// vertex buckets, so one point became two and every face meeting there was
/// torn in half. The preview runs the same kernel as the commit, so the whole
/// command read as "the kernel will not do this".
#[test]
fn one_rim_edge_of_a_square_pocket_chamfers_to_a_closed_solid() {
    let block = execute(
        &NativeKernel::empty(),
        "square-pocket-block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: BLOCK[0],
            size_y: BLOCK[1],
            size_z: BLOCK[2],
        },
    );
    let face = {
        let scene = NativeKernel::debug_scene(&block);
        scene
            .triangles
            .iter()
            .find(|triangle| {
                triangle
                    .vertices
                    .iter()
                    .all(|vertex| (vertex.z - BLOCK[2]).abs() < 1.0e-6)
            })
            .expect("the block should expose its top face")
            .source_face
    };
    let corners = [
        (-POCKET_HALF, -POCKET_HALF),
        (POCKET_HALF, -POCKET_HALF),
        (POCKET_HALF, POCKET_HALF),
        (-POCKET_HALF, POCKET_HALF),
    ];
    let square = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: (0..4)
                    .map(|index| PlanarCurve2::Line {
                        start: Point2::new(corners[index].0, corners[index].1),
                        end: Point2::new(corners[(index + 1) % 4].0, corners[(index + 1) % 4].1),
                    })
                    .collect(),
            },
            holes: vec![],
        }],
    };
    let pocketed = execute(
        &block,
        "square-pocket-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: face,
            frame: PlanarFrame3::new(
                Point3::new(40.0, 25.0, BLOCK[2]),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: square,
            distance: POCKET_DEPTH,
            operation: FaceExtrusionOperation::Cut,
        },
    );
    let scene = NativeKernel::debug_scene(&pocketed);
    let target = scene
        .edges
        .iter()
        .find(|edge| {
            edge.endpoints.iter().all(|point| {
                (point.z - BLOCK[2]).abs() < 1.0e-9
                    && (point.x - 40.0).abs() <= POCKET_HALF + 1.0e-9
                    && (point.y - 25.0).abs() <= POCKET_HALF + 1.0e-9
            })
        })
        .expect("the pocket should present a rim edge")
        .source_edge;

    let distance = 1.5;
    let before = pocketed.measures().volume;
    let chamfered = chamfer_set(&pocketed, vec![target], distance, "square-pocket-chamfer")
        .expect("one pocket rim edge should chamfer to a closed solid");
    let removed = before - chamfered.measures().volume;
    let wedge = distance * distance / 2.0 * (POCKET_HALF * 2.0);
    // Its own wedge and nothing else. The sweep overshoots each endpoint to
    // give a mitre material to form from, which is free where the body ends
    // but not at a pocket corner, where the wall carries on: that overshoot
    // used to notch a full setback of unselected material off each end.
    assert!(
        ((removed - wedge) / wedge).abs() < 0.01,
        "the chamfer should remove its wedge of {wedge}, removed {removed}"
    );
}

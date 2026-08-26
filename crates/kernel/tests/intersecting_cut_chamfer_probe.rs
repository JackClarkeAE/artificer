use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, Vector3,
};

const SIZE: f64 = 40.0;

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
        .expect("the block should expose its top face")
        .source_face
}

fn outer_top_edge(snapshot: &Snapshot) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .edges
        .iter()
        .find(|edge| {
            edge.endpoints
                .iter()
                .all(|point| (point.z - SIZE).abs() < 1.0e-6)
                && edge.endpoints.iter().any(|point| point.x.abs() < 1.0e-6)
        })
        .expect("outer top edge should exist")
        .source_edge
}

#[test]
fn chamfer_cube_with_circle_and_slot_cuts() {
    let block = execute(
        &NativeKernel::empty(),
        "block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
    );

    // 1. Circle cut on top face
    let circle_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 6.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };

    let circle_cut = execute(
        &block,
        "circle-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face(&block, SIZE),
            frame: PlanarFrame3::new(
                Point3::new(20.0, 20.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: circle_profile,
            distance: 20.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    // 2. Slot cut on top face partially intersecting the circle
    let slot_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center: Point2::new(-5.0, 0.0),
                        start: Point2::new(-5.0, -3.0),
                        end: Point2::new(-5.0, 3.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(-5.0, 3.0),
                        end: Point2::new(5.0, 3.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(5.0, 0.0),
                        start: Point2::new(5.0, 3.0),
                        end: Point2::new(5.0, -3.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(5.0, -3.0),
                        end: Point2::new(-5.0, -3.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };

    let slot_cut = execute(
        &circle_cut,
        "slot-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face(&circle_cut, SIZE),
            frame: PlanarFrame3::new(
                Point3::new(25.0, 20.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: slot_profile,
            distance: 20.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    // 3. Chamfer the edge of the cube
    let edge = outer_top_edge(&slot_cut);
    let scene = NativeKernel::debug_scene(&slot_cut);
    println!(
        "Scene edges: {}, triangles: {}, vertices: {}",
        scene.edges.len(),
        scene.triangles.len(),
        scene.vertices.len()
    );
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("chamfer"),
        expected_snapshot: slot_cut.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: vec![edge],
            kind: EdgeFinishKind::Chamfer,
            distance: 2.0,
        },
    };
    let outcome = NativeKernel::execute(&slot_cut, &request, &CancellationToken::new());
    if let Err(err) = &outcome {
        println!("Outcome error: {err:?}");
    }
    assert!(outcome.is_ok(), "Chamfer failed: {:?}", outcome.err());
}

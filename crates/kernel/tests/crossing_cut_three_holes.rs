use artificer_kernel::{CancellationToken, FaceRole, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, Vector3,
};

#[test]
fn test_crossing_cut_on_body_with_three_holes() {
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );

    // Outer rectangle: 100 x 100
    let outer_curves = vec![
        PlanarCurve2::Line {
            start: Point2::new(0.0, 0.0),
            end: Point2::new(100.0, 0.0),
        },
        PlanarCurve2::Line {
            start: Point2::new(100.0, 0.0),
            end: Point2::new(100.0, 100.0),
        },
        PlanarCurve2::Line {
            start: Point2::new(100.0, 100.0),
            end: Point2::new(0.0, 100.0),
        },
        PlanarCurve2::Line {
            start: Point2::new(0.0, 100.0),
            end: Point2::new(0.0, 0.0),
        },
    ];

    // Three circular holes
    let hole1 = vec![PlanarCurve2::Circle {
        center: Point2::new(25.0, 25.0),
        radius: 8.0,
        direction: ArcDirection::CounterClockwise,
    }];
    let hole2 = vec![PlanarCurve2::Circle {
        center: Point2::new(75.0, 25.0),
        radius: 8.0,
        direction: ArcDirection::CounterClockwise,
    }];
    let hole3 = vec![PlanarCurve2::Circle {
        center: Point2::new(50.0, 75.0),
        radius: 8.0,
        direction: ArcDirection::CounterClockwise,
    }];

    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: outer_curves,
            },
            holes: vec![
                PlanarLoop2 { curves: hole1 },
                PlanarLoop2 { curves: hole2 },
                PlanarLoop2 { curves: hole3 },
            ],
        }],
    };

    // Step 1: Extrude base block with 3 holes
    let req1 = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("step1-extrude"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance: 40.0,
        },
    };

    let outcome1 = NativeKernel::execute(&NativeKernel::empty(), &req1, &CancellationToken::new())
        .expect("step 1 should succeed");

    println!("Base snapshot counts: {}", outcome1.snapshot.counts());
    let scene1 = NativeKernel::debug_scene(&outcome1.snapshot);
    let cap_triangles = scene1
        .triangles
        .iter()
        .filter(|t| t.role == FaceRole::ExtrusionTop || t.role == FaceRole::ExtrusionBottom)
        .count();
    println!("Base cap triangles: {}", cap_triangles);
    assert!(
        cap_triangles > 0,
        "Cap faces must be triangulated in debug scene"
    );

    // Find a side face (e.g. at Y=0)
    let side_face_ref = scene1
        .triangles
        .iter()
        .find(|t| {
            let [a, b, c] = t.vertices;
            let avg_y = (a.y + b.y + c.y) / 3.0;
            avg_y.abs() < 1e-4
        })
        .map(|t| t.source_face)
        .expect("must have a Y=0 side face");

    // Step 2: Cut a cylindrical pocket through the side face
    let cut_frame = PlanarFrame3::new(
        Point3::new(50.0, 0.0, 20.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    );

    let cut_circle = vec![PlanarCurve2::Circle {
        center: Point2::new(0.0, 0.0),
        radius: 10.0,
        direction: ArcDirection::CounterClockwise,
    }];

    let cut_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves: cut_circle },
            holes: vec![],
        }],
    };

    let req2 = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("step2-cut"),
        expected_snapshot: outcome1.snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: side_face_ref,
            frame: cut_frame,
            profile: cut_profile,
            distance: 30.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };

    let outcome2 = NativeKernel::execute(&outcome1.snapshot, &req2, &CancellationToken::new())
        .expect("step 2 cut must succeed without validation errors");

    println!("After cut counts: {}", outcome2.snapshot.counts());
    let scene2 = NativeKernel::debug_scene(&outcome2.snapshot);
    println!("Scene triangles after cut: {}", scene2.triangles.len());
    assert!(!scene2.triangles.is_empty());
}

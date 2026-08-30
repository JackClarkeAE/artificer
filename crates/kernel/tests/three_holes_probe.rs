use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
    Point2, Point3, PrecisionPolicy, RequestId, Vector3,
};

#[test]
fn test_extrusion_with_three_circle_holes() {
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );

    // Outer rectangle: 100 x 100
    let outer_curves = vec![
        PlanarCurve2::Line { start: Point2::new(0.0, 0.0), end: Point2::new(100.0, 0.0) },
        PlanarCurve2::Line { start: Point2::new(100.0, 0.0), end: Point2::new(100.0, 100.0) },
        PlanarCurve2::Line { start: Point2::new(100.0, 100.0), end: Point2::new(0.0, 100.0) },
        PlanarCurve2::Line { start: Point2::new(0.0, 100.0), end: Point2::new(0.0, 0.0) },
    ];

    // Three arbitrary circular holes
    let hole1 = vec![
        PlanarCurve2::Circle {
            center: Point2::new(30.0, 30.0),
            radius: 8.0,
            direction: ArcDirection::CounterClockwise,
        },
    ];
    let hole2 = vec![
        PlanarCurve2::Circle {
            center: Point2::new(70.0, 30.0),
            radius: 8.0,
            direction: ArcDirection::CounterClockwise,
        },
    ];
    let hole3 = vec![
        PlanarCurve2::Circle {
            center: Point2::new(50.0, 70.0),
            radius: 8.0,
            direction: ArcDirection::CounterClockwise,
        },
    ];

    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves: outer_curves },
            holes: vec![
                PlanarLoop2 { curves: hole1 },
                PlanarLoop2 { curves: hole2 },
                PlanarLoop2 { curves: hole3 },
            ],
        }],
    };

    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("extrude-three-holes"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance: 20.0,
        },
    };

    let outcome = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("extrude three holes should succeed");

    println!("Counts: {}", outcome.snapshot.counts());
    let scene = NativeKernel::debug_scene(&outcome.snapshot);
    println!("Scene triangles: {}, edges: {}", scene.triangles.len(), scene.edges.len());

    use artificer_kernel::FaceRole;
    let cap_triangles = scene.triangles.iter().filter(|t| t.role == FaceRole::ExtrusionTop || t.role == FaceRole::ExtrusionBottom).count();
    println!("Cap triangles count: {}", cap_triangles);
    assert!(cap_triangles > 0, "Cap faces must be triangulated in debug scene!");
}

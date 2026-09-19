//! When the exact route stands aside, the report says why.
//!
//! A slot or a bore cut across a bore of a different radius meets it in a
//! space quartic, which the curve vocabulary does not carry. The faceted tier
//! then answers or refuses, and either way the user used to read a message
//! about tessellation for a problem that was about vocabulary. The exact
//! route's own reason now travels with the outcome: a warning beside an
//! approximation, the first diagnostic of a refusal.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, DiagnosticSeverity, EntityRef, ExecuteRequest,
    FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, Vector3,
};

const SIZE: f64 = 40.0;

fn cuboid() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("decline-cuboid"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("the cuboid should build")
        .snapshot
}

fn face_where(snapshot: &Snapshot, pick: fn(Point3) -> bool) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    for triangle in &scene.triangles {
        let [a, b, c] = triangle.vertices;
        let centre = Point3::new(
            (a.x + b.x + c.x) / 3.0,
            (a.y + b.y + c.y) / 3.0,
            (a.z + b.z + c.z) / 3.0,
        );
        if pick(centre) {
            return triangle.source_face;
        }
    }
    panic!("the fixture should expose the requested face");
}

fn bore(
    snapshot: &Snapshot,
    face: EntityRef,
    frame: PlanarFrame3,
    radius: f64,
    label: &str,
) -> ExecuteRequest {
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: face,
            frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(0.0, 0.0),
                            radius,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: vec![],
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
    }
}

/// Two crossing bores of unequal radius: the exact route declines by name,
/// and whichever tier answers carries that name.
#[test]
fn a_crossing_bore_of_another_radius_says_why_the_exact_route_declined() {
    let block = cuboid();
    let top = face_where(&block, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    let bored = NativeKernel::execute(
        &block,
        &bore(
            &block,
            top,
            PlanarFrame3::new(
                Point3::new(SIZE / 2.0, SIZE / 2.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            8.0,
            "decline-first-bore",
        ),
        &CancellationToken::new(),
    )
    .expect("a bore through a block is exact")
    .snapshot;
    let side = face_where(&bored, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    let outcome = NativeKernel::execute(
        &bored,
        &bore(
            &bored,
            side,
            PlanarFrame3::new(
                Point3::new(SIZE, SIZE / 2.0, SIZE / 2.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            5.0,
            "decline-crossing-bore",
        ),
        &CancellationToken::new(),
    );
    let declined = |diagnostics: &[artificer_protocol::Diagnostic]| {
        diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code.as_str() == "FACE_FEATURE_EXACT_ROUTE_DECLINED")
            .cloned()
    };
    match outcome {
        Ok(outcome) => {
            let decline = declined(&outcome.report.warnings)
                .expect("an approximation says why the exact route stood aside");
            assert_eq!(decline.severity, DiagnosticSeverity::Warning);
            assert!(
                decline.message.contains("cylinder and cylinder"),
                "the pair is named: {}",
                decline.message
            );
            assert!(
                outcome
                    .report
                    .warnings
                    .iter()
                    .any(|warning| warning.code.as_str() == "FACE_FEATURE_FACETED_APPROXIMATION"),
                "and the approximation is still labelled as one"
            );
        }
        Err(error) => {
            let decline = declined(&error.diagnostics)
                .expect("a refusal says why the exact route stood aside");
            assert_eq!(decline.severity, DiagnosticSeverity::Error);
            assert!(
                decline.message.contains("cylinder and cylinder"),
                "the pair is named: {}",
                decline.message
            );
            assert_eq!(
                error.diagnostics[0].code.as_str(),
                "FACE_FEATURE_EXACT_ROUTE_DECLINED",
                "and it is the first thing said: {error:?}"
            );
        }
    }
}

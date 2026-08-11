use std::collections::BTreeSet;

use artificer_kernel::{CancellationToken, FaceRole, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, ValidationProfile,
};

fn cuboid() -> artificer_kernel::Snapshot {
    NativeKernel::execute(
        &NativeKernel::empty(),
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("general-face-base"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::default(),
                size_x: 4.0,
                size_y: 4.0,
                size_z: 4.0,
            },
        },
        &CancellationToken::new(),
    )
    .expect("base cuboid")
    .snapshot
}

fn mixed_profile() -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(-1.0, 0.0),
                        end: Point2::new(1.0, 0.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, 0.0),
                        start: Point2::new(1.0, 0.0),
                        end: Point2::new(-1.0, 0.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                ],
            },
            holes: Vec::new(),
        }],
    }
}

fn request(
    input: &artificer_kernel::Snapshot,
    operation: FaceExtrusionOperation,
    distance: f64,
) -> ExecuteRequest {
    let target = NativeKernel::debug_scene(input)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::PositiveZ)
        .expect("positive-Z support")
        .source_face;
    let support = NativeKernel::planar_face_support(input, target).expect("planar support");
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("general-face-mixed-profile"),
        expected_snapshot: input.id(),
        precision: input.precision_policy().unwrap_or_default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: target,
            frame: support.frame,
            profile: mixed_profile(),
            distance,
            operation,
        },
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1.0e-9 * expected.abs().max(1.0),
        "expected {expected:.17e}, got {actual:.17e}"
    );
}

#[test]
fn public_mixed_profile_command_is_exact_deterministic_and_source_mapped() {
    for (operation, distance, expected_volume) in [
        (
            FaceExtrusionOperation::Add,
            1.0,
            64.0 + 0.5 * std::f64::consts::PI,
        ),
        (
            FaceExtrusionOperation::Cut,
            1.0,
            64.0 - 0.5 * std::f64::consts::PI,
        ),
        (
            FaceExtrusionOperation::Cut,
            10.0,
            64.0 - 2.0 * std::f64::consts::PI,
        ),
    ] {
        let input = cuboid();
        let request = request(&input, operation, distance);
        let first = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect("mixed exact face profile");
        let replay = NativeKernel::execute(&input, &request, &CancellationToken::new())
            .expect("deterministic replay");

        assert_eq!(first.snapshot.id(), replay.snapshot.id());
        assert_eq!(
            first.snapshot.semantic_digest(),
            replay.snapshot.semantic_digest()
        );
        assert_close(first.snapshot.measures().volume, expected_volume);
        assert!(first.report.validation.valid);
        assert!(NativeKernel::validate(&first.snapshot, ValidationProfile::Solid).valid);
        assert_eq!(
            first.report.history.len(),
            first.snapshot.counts().total() as usize
        );
        let side_ordinals = first
            .report
            .history
            .iter()
            .filter_map(|record| record.role.as_ref())
            .filter(|role| {
                role.name == "face_extrude.boss.side_face"
                    || role.name == "face_extrude.pocket.wall_face"
            })
            .filter_map(|role| role.ordinal)
            .collect::<BTreeSet<_>>();
        assert_eq!(side_ordinals, BTreeSet::from([0, 1]));
        let scene = NativeKernel::debug_scene(&first.snapshot);
        assert!(
            scene
                .triangles
                .iter()
                .all(|triangle| { triangle.source_face.snapshot == first.snapshot.id() })
        );
        assert!(
            scene
                .edges
                .iter()
                .all(|edge| { edge.source_edge.snapshot == first.snapshot.id() })
        );
    }
}

#[test]
fn rejected_tangent_profile_retains_the_input_snapshot() {
    let input = cuboid();
    let target = NativeKernel::debug_scene(&input)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::PositiveZ)
        .unwrap()
        .source_face;
    let support = NativeKernel::planar_face_support(&input, target).unwrap();
    let before_id = input.id();
    let before_digest = input.semantic_digest();
    let before_measures = input.measures();
    let error = NativeKernel::execute(
        &input,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("general-face-tangent-rejection"),
            expected_snapshot: input.id(),
            precision: input.precision_policy().unwrap_or_default(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: target,
                frame: support.frame,
                profile: PlanarProfile2::from_polygon(&[
                    Point2::new(0.0, 1.0),
                    Point2::new(2.0, 1.0),
                    Point2::new(2.0, 3.0),
                    Point2::new(0.0, 3.0),
                ]),
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            },
        },
        &CancellationToken::new(),
    )
    .expect_err("support tangency is outside the regularized positive domain");
    assert_eq!(
        error.diagnostics[0].code.as_str(),
        "FACE_FEATURE_PROFILE_OUTSIDE_FACE"
    );
    assert_eq!(input.id(), before_id);
    assert_eq!(input.semantic_digest(), before_digest);
    assert_eq!(input.measures(), before_measures);
}

//! A second bore crossing an already-faceted body.
//!
//! One bore crossing another is answered by the faceted tier, which the
//! crossing-cut tests already cover. This is the step after that: cutting a
//! second crossing bore into the body the first one left behind. The kernel
//! does not yet own that construction, and what matters until it does is that
//! refusing is all it does. A snapshot is an immutable value, so the body the
//! user already has must survive a refused operation unchanged, and must go on
//! validating as the solid it was.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const BLOCK_X: f64 = 100.0;
const BLOCK_Y: f64 = 50.0;
const BLOCK_Z: f64 = 50.0;
const BORE_RADIUS: f64 = 13.0;

fn line(start: (f64, f64), end: (f64, f64)) -> PlanarCurve2 {
    PlanarCurve2::Line {
        start: Point2::new(start.0, start.1),
        end: Point2::new(end.0, end.1),
    }
}

fn circle(center: (f64, f64), radius: f64) -> Vec<PlanarCurve2> {
    vec![PlanarCurve2::Circle {
        center: Point2::new(center.0, center.1),
        radius,
        direction: ArcDirection::CounterClockwise,
    }]
}

/// The block with two bores already down through it, built exactly.
fn block_with_two_vertical_bores() -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    line((0.0, 0.0), (BLOCK_X, 0.0)),
                    line((BLOCK_X, 0.0), (BLOCK_X, BLOCK_Y)),
                    line((BLOCK_X, BLOCK_Y), (0.0, BLOCK_Y)),
                    line((0.0, BLOCK_Y), (0.0, 0.0)),
                ],
            },
            holes: vec![
                PlanarLoop2 {
                    curves: circle((25.0, 25.0), BORE_RADIUS),
                },
                PlanarLoop2 {
                    curves: circle((75.0, 25.0), BORE_RADIUS),
                },
            ],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("block-two-bores"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: BLOCK_Z,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a block with two bores is an exact extrusion")
        .snapshot
}

/// The face on the y = 0 wall, which both crossing bores are cut from.
fn front_face(snapshot: &Snapshot) -> EntityRef {
    NativeKernel::debug_scene(snapshot)
        .triangles
        .iter()
        .find(|triangle| {
            let [a, b, c] = triangle.vertices;
            ((a.y + b.y + c.y) / 3.0).abs() < 1.0e-6
        })
        .map(|triangle| triangle.source_face)
        .expect("the y = 0 wall")
}

fn crossing_bore(snapshot: &Snapshot, at_x: f64, label: &'static str) -> ExecuteRequest {
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: front_face(snapshot),
            frame: PlanarFrame3::new(
                Point3::new(at_x, 0.0, BLOCK_Z / 2.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: circle((0.0, 0.0), BORE_RADIUS),
                    },
                    holes: vec![],
                }],
            },
            distance: BLOCK_Y,
            operation: FaceExtrusionOperation::Cut,
        },
    }
}

#[test]
fn a_refused_second_crossing_bore_leaves_the_body_it_was_given_untouched() {
    let base = block_with_two_vertical_bores();
    let expected_base =
        BLOCK_X * BLOCK_Y * BLOCK_Z - 2.0 * PI * BORE_RADIUS * BORE_RADIUS * BLOCK_Z;
    let base_volume = base.measures().volume;
    assert!(
        ((base_volume - expected_base) / expected_base).abs() < 1.0e-9,
        "two vertical bores are exact: {base_volume} should be {expected_base}"
    );

    // The first crossing bore is the faceted tier's to own, and it says so.
    let first = NativeKernel::execute(
        &base,
        &crossing_bore(&base, 25.0, "first-crossing-bore"),
        &CancellationToken::new(),
    )
    .expect("the first crossing bore closes on the faceted tier");
    assert!(
        first
            .report
            .warnings
            .iter()
            .any(|warning| warning.code.as_str() == "FACE_FEATURE_FACETED_APPROXIMATION"),
        "a crossing bore is a labelled approximation: {:?}",
        first.report.warnings
    );
    let crossed = first.snapshot;
    let crossed_id = crossed.id();
    let crossed_volume = crossed.measures().volume;
    assert!(
        NativeKernel::validate(&crossed, ValidationProfile::Solid).valid,
        "the faceted body is a valid solid before the second bore"
    );

    // The second crossing bore is the step the kernel does not own yet.
    let second = NativeKernel::execute(
        &crossed,
        &crossing_bore(&crossed, 75.0, "second-crossing-bore"),
        &CancellationToken::new(),
    );

    // Whatever it decides, the body the user already had is untouched. A
    // snapshot is an immutable value, so this is a guarantee the design makes
    // rather than one the operation has to remember to keep; the test is here
    // because that is exactly the kind of guarantee that quietly stops being
    // true.
    assert_eq!(crossed.id(), crossed_id, "the input snapshot keeps its id");
    assert!(
        (crossed.measures().volume - crossed_volume).abs() < 1.0e-9,
        "the input body keeps its volume"
    );
    let after = NativeKernel::validate(&crossed, ValidationProfile::Solid);
    assert!(
        after.valid,
        "the input body still validates after the attempt: {:?}",
        after.diagnostics
    );

    match second {
        Err(error) => {
            // Refusing is the current answer, and it has to be a named one.
            assert!(
                !error.diagnostics.is_empty(),
                "a refusal names why: {error:?}"
            );
        }
        Ok(outcome) => {
            // If a later kernel owns this construction, it owes a valid solid
            // rather than a shell that merely got past execution.
            let validation = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
            assert!(
                validation.valid,
                "a second crossing bore that succeeds owes a valid solid: {:?}",
                validation.diagnostics
            );
            let volume = outcome.snapshot.measures().volume;
            assert!(
                volume < crossed_volume,
                "a cut removes material: {crossed_volume} then {volume}"
            );
        }
    }
}

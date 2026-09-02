//! A round cut through the side of a three-hole block that crosses two of
//! the holes perpendicularly and misses the third.
//!
//! The crossings meet in quartic curves, outside the line-and-circle
//! vocabulary, so the faceted tier answers and must say so; the cut still
//! owes a valid closed solid whose volume lies between the holed block and
//! the block minus the whole cutter.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

#[test]
fn a_side_cut_crossing_two_of_three_holes_closes_and_says_it_is_faceted() {
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
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
    // The cutter runs along +y from the y = 0 face, centred at x = 50 with
    // radius 10, to a depth of 30: it crosses the two holes at y = 15 and
    // misses the one at y = 75.
    let hole = |x: f64, y: f64| {
        vec![PlanarCurve2::Circle {
            center: Point2::new(x, y),
            radius: 8.0,
            direction: ArcDirection::CounterClockwise,
        }]
    };
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: outer_curves,
            },
            holes: vec![
                PlanarLoop2 {
                    curves: hole(40.0, 15.0),
                },
                PlanarLoop2 {
                    curves: hole(62.0, 15.0),
                },
                PlanarLoop2 {
                    curves: hole(50.0, 75.0),
                },
            ],
        }],
    };
    let extrude = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("three-holes-extrude"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance: 40.0,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &extrude, &CancellationToken::new())
        .expect("three holes extrude")
        .snapshot;
    let expected_base = 100.0 * 100.0 * 40.0 - 3.0 * PI * 64.0 * 40.0;
    let base_volume = base.measures().volume;
    assert!(
        ((base_volume - expected_base) / expected_base).abs() < 1.0e-9,
        "base volume {base_volume} should be {expected_base}"
    );

    let side_face = NativeKernel::debug_scene(&base)
        .triangles
        .iter()
        .find(|triangle| {
            let [a, b, c] = triangle.vertices;
            ((a.y + b.y + c.y) / 3.0).abs() < 1.0e-6
        })
        .map(|triangle| triangle.source_face)
        .expect("the y = 0 side face");
    let cut = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("three-holes-cut"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: side_face,
            frame: PlanarFrame3::new(
                Point3::new(50.0, 0.0, 20.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(0.0, 0.0),
                            radius: 10.0,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: vec![],
                }],
            },
            distance: 30.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    let outcome = NativeKernel::execute(&base, &cut, &CancellationToken::new())
        .expect("the crossing cut closes");

    assert!(
        outcome
            .report
            .warnings
            .iter()
            .any(|warning| warning.code.as_str() == "FACE_FEATURE_FACETED_APPROXIMATION"),
        "a crossing cut is a labelled approximation: {:?}",
        outcome.report.warnings
    );
    let validation = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
    let after = outcome.snapshot.measures().volume;
    let cutter = PI * 100.0 * 30.0;
    assert!(
        after < base_volume && after > base_volume - cutter,
        "volume {after} must lie between {base_volume} and {}",
        base_volume - cutter
    );
    assert!(
        !NativeKernel::debug_scene(&outcome.snapshot)
            .triangles
            .is_empty()
    );
}

//! A round cut through the side of a three-hole block that crosses two of
//! the holes perpendicularly and misses the third.
//!
//! The crossings meet in quartic curves — a cutter of radius 10 against holes
//! of radius 8 whose axes pass it at 10 and 12 — which the faceted tier used
//! to answer, labelled as an approximation. Since ADR 0047 the exact engine
//! carries the curve and closes the section it leaves, so the cut is exact:
//! no caveat, and a volume that is the block less the cutter plus the two
//! lenses it shares with the holes, each measured here by a quadrature of its
//! own.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

#[test]
fn a_side_cut_crossing_two_of_three_holes_is_exact() {
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
        outcome.report.warnings.is_empty(),
        "an exact cut carries no caveat: {:?}",
        outcome.report.warnings
    );
    let validation = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);

    // Each lens: across the cutter's axis at `x`, the cutter's chord in z is
    // `2√(100 − x²)` and the hole's chord in y is `2√(64 − (x − offset)²)`,
    // wholly inside the cutter's 30 of depth. Both are known; their product is
    // integrated over the stretch of `x` the two discs share.
    let lens = |offset: f64| {
        integrate(offset - 8.0, 10.0, &|x: f64| {
            4.0 * x.mul_add(-x, 100.0).max(0.0).sqrt()
                * (x - offset).mul_add(-(x - offset), 64.0).max(0.0).sqrt()
        })
    };
    let expected = PI.mul_add(-100.0 * 30.0, base_volume) + lens(10.0) + lens(12.0);
    let after = outcome.snapshot.measures().volume;
    assert!(
        ((after - expected) / expected).abs() < 1.0e-9,
        "volume {after} should be {expected}"
    );
    assert!(
        !NativeKernel::debug_scene(&outcome.snapshot)
            .triangles
            .is_empty()
    );
}

/// `∫ f` over `[from, to]`, walked through `x = from + (to − from)(3t² − 2t³)`
/// so the square roots that vanish at either end become smooth, then composite
/// Simpson. Independent of the kernel's own quadrature on purpose.
fn integrate(from: f64, to: f64, integrand: &dyn Fn(f64) -> f64) -> f64 {
    let panels = 20_000;
    let step = 1.0 / f64::from(panels);
    let span = to - from;
    (0..=panels)
        .map(|index| {
            let t = f64::from(index) * step;
            let x = span.mul_add(t * t * 2.0f64.mul_add(-t, 3.0), from);
            let rate = 6.0 * span * t * (1.0 - t);
            let weight = if index == 0 || index == panels {
                1.0
            } else if index % 2 == 1 {
                4.0
            } else {
                2.0
            };
            weight * integrand(x) * rate
        })
        .sum::<f64>()
        * step
        / 3.0
}

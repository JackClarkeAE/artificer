//! A cut through the side of a holed block that crosses the holes
//! perpendicularly.
//!
//! Two round bores that cross meet in quartic curves, and a round bore
//! crossing a square one meets it in ellipses; both are outside the
//! line-and-circle vocabulary, so every case here is answered by the faceted
//! tier and must say so. What the tier owes is a closed, valid solid whose
//! volume sits between the holed block and the block minus the whole cutter,
//! at every offset — including the ones where the cutter's silhouette lands
//! exactly on a hole's axis line, which is where welded sliver fragments used
//! to be dropped and the shell failed to close.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, KernelError, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIDE: f64 = 100.0;
const HEIGHT: f64 = 40.0;
const HOLE_RADIUS: f64 = 8.0;
const CUTTER_RADIUS: f64 = 10.0;

fn rect(center: (f64, f64), width: f64, height: f64) -> Vec<PlanarCurve2> {
    let (x0, x1) = (center.0 - width / 2.0, center.0 + width / 2.0);
    let (y0, y1) = (center.1 - height / 2.0, center.1 + height / 2.0);
    let corners = [
        Point2::new(x0, y0),
        Point2::new(x1, y0),
        Point2::new(x1, y1),
        Point2::new(x0, y1),
    ];
    (0..4)
        .map(|index| PlanarCurve2::Line {
            start: corners[index],
            end: corners[(index + 1) % 4],
        })
        .collect()
}

fn circle(center: (f64, f64), radius: f64) -> Vec<PlanarCurve2> {
    vec![PlanarCurve2::Circle {
        center: Point2::new(center.0, center.1),
        radius,
        direction: ArcDirection::CounterClockwise,
    }]
}

fn polygon(corners: &[(f64, f64)]) -> Vec<PlanarCurve2> {
    (0..corners.len())
        .map(|index| {
            let start = corners[index];
            let end = corners[(index + 1) % corners.len()];
            PlanarCurve2::Line {
                start: Point2::new(start.0, start.1),
                end: Point2::new(end.0, end.1),
            }
        })
        .collect()
}

fn block(holes: Vec<Vec<PlanarCurve2>>, label: &str) -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: rect((SIDE / 2.0, SIDE / 2.0), SIDE, SIDE),
            },
            holes: holes
                .into_iter()
                .map(|curves| PlanarLoop2 { curves })
                .collect(),
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: HEIGHT,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a holed block should extrude")
        .snapshot
}

fn y0_face(snapshot: &Snapshot) -> EntityRef {
    NativeKernel::debug_scene(snapshot)
        .triangles
        .iter()
        .find(|triangle| {
            let [a, b, c] = triangle.vertices;
            ((a.y + b.y + c.y) / 3.0).abs() < 1.0e-6
        })
        .map(|triangle| triangle.source_face)
        .expect("the y = 0 side face")
}

/// Cuts from the `y = 0` face along `+y` with the cutter centred at
/// `(x, z = HEIGHT / 2)`.
fn side_cut(
    snapshot: &Snapshot,
    x: f64,
    cutter: Vec<PlanarCurve2>,
    depth: f64,
    label: &str,
) -> Result<ExecutionOutcome, KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: y0_face(snapshot),
            frame: PlanarFrame3::new(
                Point3::new(x, 0.0, HEIGHT / 2.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 { curves: cutter },
                    holes: vec![],
                }],
            },
            distance: depth,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
}

/// The cut must publish a valid closed solid, labelled as an approximation,
/// whose volume lies between the holed block and that block minus the whole
/// cutter.
fn assert_crossing_cut(base: &Snapshot, outcome: &ExecutionOutcome, cutter_volume: f64) {
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
    let before = base.measures().volume;
    let after = outcome.snapshot.measures().volume;
    assert!(
        after < before && after > before - cutter_volume,
        "volume {after} must lie between {before} and {}",
        before - cutter_volume
    );
}

fn cylinder_volume(radius: f64, depth: f64) -> f64 {
    PI * radius * radius * depth
}

#[test]
fn a_round_cutter_crossing_a_round_and_a_square_hole_closes() {
    let base = block(
        vec![
            circle((40.0, 50.0), HOLE_RADIUS),
            rect((60.0, 50.0), 16.0, 16.0),
        ],
        "round-and-square",
    );
    for (depth, label) in [(60.0, "blind"), (120.0, "through")] {
        let outcome = side_cut(
            &base,
            50.0,
            circle((0.0, 0.0), CUTTER_RADIUS),
            depth,
            &format!("round-and-square-{label}"),
        )
        .expect("a round cutter crossing both holes");
        assert_crossing_cut(
            &base,
            &outcome,
            cylinder_volume(CUTTER_RADIUS, depth.min(SIDE)),
        );
    }
}

#[test]
fn a_round_cutter_crossing_two_round_holes_closes() {
    let base = block(
        vec![
            circle((40.0, 50.0), HOLE_RADIUS),
            circle((60.0, 50.0), HOLE_RADIUS),
        ],
        "two-round",
    );
    let outcome = side_cut(
        &base,
        50.0,
        circle((0.0, 0.0), CUTTER_RADIUS),
        60.0,
        "two-round",
    )
    .expect("a round cutter crossing two round holes");
    assert_crossing_cut(&base, &outcome, cylinder_volume(CUTTER_RADIUS, 60.0));
}

#[test]
fn a_square_cutter_crossing_two_holes_closes() {
    let base = block(
        vec![
            circle((40.0, 50.0), HOLE_RADIUS),
            rect((60.0, 50.0), 16.0, 16.0),
        ],
        "square-cutter-base",
    );
    let outcome = side_cut(
        &base,
        50.0,
        rect((0.0, 0.0), 20.0, 20.0),
        60.0,
        "square-cutter",
    )
    .expect("a square cutter crossing both holes");
    assert_crossing_cut(&base, &outcome, 20.0 * 20.0 * 60.0);
}

#[test]
fn a_round_cutter_crossing_a_triangular_and_an_l_shaped_hole_closes() {
    let triangle = polygon(&[(52.0, 42.0), (68.0, 42.0), (60.0, 58.0)]);
    let l_shape = polygon(&[
        (22.0, 42.0),
        (38.0, 42.0),
        (38.0, 50.0),
        (30.0, 50.0),
        (30.0, 58.0),
        (22.0, 58.0),
    ]);
    let base = block(vec![l_shape, triangle], "triangle-and-l");
    let outcome = side_cut(
        &base,
        45.0,
        circle((0.0, 0.0), CUTTER_RADIUS),
        60.0,
        "triangle-and-l",
    )
    .expect("a round cutter crossing arbitrary holes");
    assert_crossing_cut(&base, &outcome, cylinder_volume(CUTTER_RADIUS, 60.0));
}

#[test]
fn a_cutter_whose_silhouette_grazes_a_hole_axis_still_closes() {
    // One round hole at x = 40; the cutter's silhouette generatrix lies on
    // the hole's axis line whenever `offset == cutter radius`. Sweep the
    // band either side of it, including the off-grid case.
    let base = block(vec![circle((40.0, 50.0), HOLE_RADIUS)], "graze-base");
    for (x, radius) in [
        (49.0, CUTTER_RADIUS),
        (49.9, CUTTER_RADIUS),
        (50.0, CUTTER_RADIUS),
        (50.1, CUTTER_RADIUS),
        (51.0, CUTTER_RADIUS),
        (50.0, 9.9),
        (50.0, 10.1),
    ] {
        let outcome = side_cut(
            &base,
            x,
            circle((0.0, 0.0), radius),
            60.0,
            &format!("graze-{x}-{radius}"),
        )
        .unwrap_or_else(|error| panic!("offset {x}, radius {radius}: {error:?}"));
        assert_crossing_cut(&base, &outcome, cylinder_volume(radius, 60.0));
    }

    let off_grid = block(vec![circle((40.37, 50.0), 7.3)], "off-grid-base");
    let outcome = side_cut(&off_grid, 49.0, circle((0.0, 0.0), 9.7), 60.0, "off-grid")
        .expect("an off-grid grazing cut");
    assert_crossing_cut(&off_grid, &outcome, cylinder_volume(9.7, 60.0));
}

#[test]
fn a_cutter_that_misses_every_hole_stays_exact() {
    let base = block(
        vec![
            circle((30.0, 50.0), HOLE_RADIUS),
            rect((70.0, 50.0), 16.0, 16.0),
        ],
        "miss-base",
    );
    let outcome = side_cut(&base, 50.0, circle((0.0, 0.0), CUTTER_RADIUS), 30.0, "miss")
        .expect("a pocket that crosses nothing stays on the exact rung");
    assert!(
        outcome.report.warnings.is_empty(),
        "{:?}",
        outcome.report.warnings
    );
    let expected = base.measures().volume - cylinder_volume(CUTTER_RADIUS, 30.0);
    let actual = outcome.snapshot.measures().volume;
    assert!(
        ((actual - expected) / expected).abs() < 1.0e-9,
        "volume {actual} should be {expected}"
    );
}

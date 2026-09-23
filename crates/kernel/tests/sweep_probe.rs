//! Closed-form gates for sweeping a planar profile along a path (ADR 0055,
//! S2).
//!
//! Every expectation is derived here. A straight sweep is a prism, whatever
//! its lean, by Cavalieri. A sweep along an arc about an axis in the
//! profile's plane is a revolve, by Pappus. And a sweep whose path bends
//! nowhere more tightly than the profile is wide has, by Pappus again, the
//! profile's area times the length its centroid travels.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, KernelError,
    OperationReport, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
    Point2, Point3, PrecisionPolicy, RequestId, SolidOperation, SweepOrientation, SweepPath3,
    SweepSegment3, Tier, ValidationProfile, Vector3,
};

const PI: f64 = std::f64::consts::PI;

fn frame_xy() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

fn frame_xz() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    )
}

fn disc(center: (f64, f64), radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(center.0, center.1),
                    radius,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    }
}

fn line(start: [f64; 3], end: [f64; 3]) -> SweepSegment3 {
    SweepSegment3::Line {
        start: Point3::new(start[0], start[1], start[2]),
        end: Point3::new(end[0], end[1], end[2]),
    }
}

fn arc(center: [f64; 3], start: [f64; 3], normal: [f64; 3], sweep: f64) -> SweepSegment3 {
    SweepSegment3::Arc {
        center: Point3::new(center[0], center[1], center[2]),
        start: Point3::new(start[0], start[1], start[2]),
        normal: Vector3::new(normal[0], normal[1], normal[2]),
        sweep,
    }
}

fn sweep_into(
    body: &Snapshot,
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    segments: Vec<SweepSegment3>,
    orientation: SweepOrientation,
    operation: SolidOperation,
) -> Result<(Snapshot, OperationReport), KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("sweep"),
        expected_snapshot: body.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::SweepPlanarProfile {
            frame,
            profile,
            path: SweepPath3 { segments },
            orientation,
            operation,
        },
    };
    NativeKernel::execute(body, &request, &CancellationToken::new())
        .map(|outcome| (outcome.snapshot, outcome.report))
}

fn sweep(
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    segments: Vec<SweepSegment3>,
) -> Result<(Snapshot, OperationReport), KernelError> {
    sweep_into(
        &NativeKernel::empty(),
        frame,
        profile,
        segments,
        SweepOrientation::RotationMinimising,
        SolidOperation::New,
    )
}

fn assert_valid(snapshot: &Snapshot, what: &str) {
    let report = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(report.valid, "{what}: {:?}", report.diagnostics);
}

fn assert_close(actual: f64, expected: f64, relative: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < relative,
        "{what}: {actual} should be {expected}"
    );
}

fn square() -> PlanarProfile2 {
    PlanarProfile2::from_polygon(&[
        Point2::new(0.0, 0.0),
        Point2::new(2.0, 0.0),
        Point2::new(2.0, 3.0),
        Point2::new(0.0, 3.0),
    ])
}

#[test]
fn a_straight_sweep_is_a_prism_whatever_its_lean() {
    for (end, what) in [([1.0, 1.0, 10.0], "square"), ([4.0, 1.0, 10.0], "leaning")] {
        let (solid, report) =
            sweep(frame_xy(), square(), vec![line([1.0, 1.0, 0.0], end)]).expect(what);
        assert_valid(&solid, what);
        assert_close(solid.measures().volume, 6.0 * 10.0, 1.0e-12, what);
        assert_eq!(report.rung.as_deref(), Some("sweep/straight"), "{what}");
        assert_eq!(report.tier(), Tier::Exact, "{what}");
    }
    // Two lines that carry straight on are one.
    let (solid, report) = sweep(
        frame_xy(),
        square(),
        vec![
            line([1.0, 1.0, 0.0], [1.0, 1.0, 4.0]),
            line([1.0, 1.0, 4.0], [1.0, 1.0, 10.0]),
        ],
    )
    .expect("collinear lines");
    assert_close(solid.measures().volume, 60.0, 1.0e-12, "collinear");
    assert_eq!(report.rung.as_deref(), Some("sweep/straight"));
}

/// A disc on the XZ plane swept a quarter turn about Z is a quarter torus,
/// built as a revolve.
#[test]
fn a_sweep_along_an_arc_about_an_axis_in_the_profile_is_a_revolve() {
    let (solid, report) = sweep(
        frame_xz(),
        disc((10.0, 0.0), 1.0),
        vec![arc(
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            PI / 2.0,
        )],
    )
    .expect("an arc sweep");
    assert_valid(&solid, "quarter torus");
    assert_close(
        solid.measures().volume,
        PI * 1.0 * (10.0 * PI / 2.0),
        1.0e-9,
        "Pappus",
    );
    assert_eq!(report.rung.as_deref(), Some("sweep/revolve"));
    assert!(report.warnings.is_empty());
}

/// Up, round a bend and along: a pipe whose volume is its bore's area times
/// the length of its centreline, skinned to within the budget and saying so.
#[test]
fn a_sweep_round_a_bend_is_skinned_to_within_its_stated_budget() {
    let bend = 3.0;
    let segments = vec![
        line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
        arc([bend, 0.0, 5.0], [0.0, 0.0, 5.0], [0.0, 1.0, 0.0], PI / 2.0),
        line([bend, 0.0, 5.0 + bend], [bend + 5.0, 0.0, 5.0 + bend]),
    ];
    let (solid, report) =
        sweep(frame_xy(), disc((0.0, 0.0), 1.0), segments.clone()).expect("an elbow");
    assert_valid(&solid, "elbow");
    let length = 5.0 + bend * PI / 2.0 + 5.0;
    assert_close(solid.measures().volume, PI * length, 1.0e-4, "Pappus");
    assert_eq!(report.rung.as_deref(), Some("sweep/skinned"));
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "SWEEP_APPROXIMATION_TOLERANCE")
        .expect("an approximation says so");
    let measurement = warning.measurement.expect("with the departure it met");
    let budget = PrecisionPolicy::default().approximation_budget;
    assert!(measurement.measured <= budget, "{measurement:?}");
    assert_eq!(measurement.allowed.max, Some(budget));
    // The far cap stands square to the last line, where the path ends.
    let bounds = solid.measures().bounds.expect("bounds");
    assert_close(bounds.max.x, bend + 5.0, 1.0e-9, "the far end");

    let step = NativeKernel::export_step(&solid, "elbow").expect("the elbow exports");
    assert!(step.contains("B_SPLINE_SURFACE"));
}

/// Held fixed, a level disc only moves: every level slice of the solid is
/// the disc, so a path that keeps rising sweeps the disc's area times the
/// rise, by Cavalieri, however it leans.
#[test]
fn a_fixed_sweep_keeps_the_profile_level() {
    let lean = std::f64::consts::FRAC_1_SQRT_2;
    let bend = 3.0;
    let arc_end = [bend - bend * lean, 0.0, 5.0 + bend * lean];
    let end = [arc_end[0] + 4.0 * lean, 0.0, arc_end[2] + 4.0 * lean];
    let (fixed, report) = sweep_into(
        &NativeKernel::empty(),
        frame_xy(),
        disc((0.0, 0.0), 1.0),
        vec![
            line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
            arc([bend, 0.0, 5.0], [0.0, 0.0, 5.0], [0.0, 1.0, 0.0], PI / 4.0),
            line(arc_end, end),
        ],
        SweepOrientation::Fixed,
        SolidOperation::New,
    )
    .expect("a fixed sweep");
    assert_valid(&fixed, "fixed");
    assert_close(fixed.measures().volume, PI * end[2], 1.0e-4, "Cavalieri");
    assert_eq!(report.rung.as_deref(), Some("sweep/skinned"));
    let bounds = fixed.measures().bounds.expect("bounds");
    assert_close(bounds.max.z, end[2], 1.0e-9, "the disc stays level");
}

#[test]
fn every_path_a_sweep_cannot_follow_is_refused_by_name() {
    let code = |result: Result<(Snapshot, OperationReport), KernelError>| {
        result
            .expect_err("refused")
            .diagnostics
            .first()
            .map(|diagnostic| diagnostic.code.as_str().to_owned())
            .unwrap_or_default()
    };
    let profile = || disc((0.0, 0.0), 1.0);
    assert_eq!(
        code(sweep(frame_xy(), profile(), vec![])),
        "SWEEP_PATH_EMPTY"
    );
    assert_eq!(
        code(sweep(
            frame_xy(),
            profile(),
            vec![
                line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
                line([0.0, 0.0, 5.0], [5.0, 0.0, 5.0]),
            ],
        )),
        "SWEEP_PATH_CORNER"
    );
    assert_eq!(
        code(sweep(
            frame_xy(),
            profile(),
            vec![
                line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
                line([0.0, 1.0, 5.0], [0.0, 1.0, 9.0]),
            ],
        )),
        "SWEEP_PATH_GAP"
    );
    assert_eq!(
        code(sweep(
            frame_xz(),
            disc((10.0, 0.0), 1.0),
            vec![arc(
                [0.0, 0.0, 0.0],
                [10.0, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                2.0 * PI
            )],
        )),
        "SWEEP_PATH_CLOSED"
    );
    assert_eq!(
        code(sweep(
            frame_xy(),
            profile(),
            vec![line([0.0, 0.0, 0.0], [5.0, 0.0, 0.0])],
        )),
        "SWEEP_PROFILE_ALONG_PATH"
    );
    // A disc of radius 4 round a bend of radius 3.
    assert_eq!(
        code(sweep(
            frame_xy(),
            disc((0.0, 0.0), 4.0),
            vec![
                line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
                arc([3.0, 0.0, 5.0], [0.0, 0.0, 5.0], [0.0, 1.0, 0.0], PI / 2.0),
            ],
        )),
        "SWEEP_PROFILE_TOO_WIDE"
    );
}

/// A sweep joins the Boolean ladder like any other tool: a straight sweep
/// through a block takes its prism out of it exactly.
#[test]
fn a_straight_sweep_cuts_a_block_exactly() {
    let block = {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("block"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::new(-5.0, -5.0, 2.0),
                size_x: 20.0,
                size_y: 10.0,
                size_z: 4.0,
            },
        };
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("block")
            .snapshot
    };
    let (cut, report) = sweep_into(
        &block,
        frame_xy(),
        square(),
        vec![line([1.0, 1.0, 0.0], [1.0, 1.0, 10.0])],
        SweepOrientation::RotationMinimising,
        SolidOperation::Cut,
    )
    .expect("the sweep cuts the block");
    assert_valid(&cut, "cut");
    assert_close(
        cut.measures().volume,
        20.0 * 10.0 * 4.0 - 6.0 * 4.0,
        1.0e-12,
        "block less the swept square",
    );
    assert_eq!(report.rung.as_deref(), Some("sweep/boolean-prism"));
}

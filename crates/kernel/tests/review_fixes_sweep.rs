//! Regressions for a review of the sweep, the smooth loft and the spline
//! profile (ADR 0050, ADR 0055), each pinned to an answer computed here.
//!
//! A disc of radius `r` carried square to a path of length `L` by its
//! rotation-minimising frame, bent nowhere more tightly than `r` and clear
//! of itself, sweeps a tube of volume `πr²L` whatever the path's torsion (the
//! tube formula; Pappus for a plane path). A skinned body built a hundred
//! kilometres out is the one built at the origin, moved. The refusals are
//! of shapes whose fault is plain: a path back through the tube it has swept,
//! coils closer than the wire is thick, a profile leaning back so far on a
//! bend that it folds, and a path segment that stalls.
//!
//! Skinned sweeps are slow in a debug build — most of the time goes to
//! certifying and measuring walls of hundreds of control points — so the
//! bodies built here are skinned to a loose budget, from few copies.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, LoftOperation,
    LoftSection, OperationReport, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, SolidOperation, SweepOrientation,
    SweepPath3, SweepSegment3, ValidationProfile, Vector3,
};

fn frame(origin: [f64; 3], u: [f64; 3], v: [f64; 3]) -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(origin[0], origin[1], origin[2]),
        Vector3::new(u[0], u[1], u[2]),
        Vector3::new(v[0], v[1], v[2]),
    )
}

fn disc(radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
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
    }
}

fn square(side: f64) -> PlanarProfile2 {
    let half = side / 2.0;
    PlanarProfile2::from_polygon(&[
        Point2::new(-half, -half),
        Point2::new(half, -half),
        Point2::new(half, half),
        Point2::new(-half, half),
    ])
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

fn clamped_uniform(count: usize, degree: usize) -> Vec<f64> {
    let interior = count - degree - 1;
    let mut knots = vec![0.0; degree + 1];
    knots.extend((1..=interior).map(|index| index as f64 / (interior + 1) as f64));
    knots.extend(std::iter::repeat_n(1.0, degree + 1));
    knots
}

/// The cubic on clamped uniform knots with `points` for control points.
fn cubic(points: &[[f64; 3]]) -> SweepSegment3 {
    SweepSegment3::Spline {
        degree: 3,
        knots: clamped_uniform(points.len(), 3),
        points: points
            .iter()
            .map(|point| Point3::new(point[0], point[1], point[2]))
            .collect(),
    }
}

/// A point of a clamped B-spline, by de Boor's algorithm.
fn de_boor(degree: usize, knots: &[f64], points: &[[f64; 3]], t: f64) -> [f64; 3] {
    let span = (degree..points.len())
        .rfind(|index| knots[*index] <= t && knots[*index] < knots[index + 1])
        .unwrap_or(degree);
    let mut local = points[span - degree..=span].to_vec();
    for level in 1..=degree {
        for index in (level..=degree).rev() {
            let knot = span - degree + index;
            let alpha = (t - knots[knot]) / (knots[knot + degree + 1 - level] - knots[knot]);
            let (before, after) = (local[index - 1], local[index]);
            local[index] = std::array::from_fn(|axis| {
                (1.0 - alpha).mul_add(before[axis], alpha * after[axis])
            });
        }
    }
    local[degree]
}

/// The length of the cubic through `points`, by a fine polyline.
fn cubic_length(points: &[[f64; 3]]) -> f64 {
    let knots = clamped_uniform(points.len(), 3);
    let steps = 20_000;
    let mut previous = points[0];
    let mut length = 0.0;
    for step in 1..=steps {
        let t = f64::from(step) / f64::from(steps);
        let point = if step == steps {
            points[points.len() - 1]
        } else {
            de_boor(3, &knots, points, t)
        };
        length += (0..3)
            .map(|axis| (point[axis] - previous[axis]).powi(2))
            .sum::<f64>()
            .sqrt();
        previous = point;
    }
    length
}

/// A frame at the start of the cubic through `points`, square to the way it
/// leaves: along its first leg, since a clamped spline starts along it.
fn square_to(points: &[[f64; 3]]) -> PlanarFrame3 {
    let along = [0, 1, 2].map(|axis| points[1][axis] - points[0][axis]);
    let unit = |vector: [f64; 3]| {
        let length = vector.iter().map(|value| value * value).sum::<f64>().sqrt();
        vector.map(|value| value / length)
    };
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let along = unit(along);
    let u = unit(cross([0.0, 0.0, 1.0], along));
    frame(points[0], u, cross(along, u))
}

/// Points on a helix of `turns` about `z`, as control points.
fn helix(radius: f64, pitch: f64, turns: f64, count: usize) -> Vec<[f64; 3]> {
    (0..count)
        .map(|index| {
            let angle = turns * 2.0 * PI * index as f64 / (count - 1) as f64;
            [
                radius * angle.cos(),
                radius * angle.sin(),
                pitch * angle / (2.0 * PI),
            ]
        })
        .collect()
}

fn run(
    command: KernelCommand,
    precision: PrecisionPolicy,
) -> Result<(Snapshot, OperationReport), Vec<String>> {
    let empty = NativeKernel::empty();
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("review"),
        expected_snapshot: empty.id(),
        precision,
        command,
    };
    NativeKernel::execute(&empty, &request, &CancellationToken::new())
        .map(|outcome| (outcome.snapshot, outcome.report))
        .map_err(|error| {
            error
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str().to_owned())
                .collect()
        })
}

fn sweep(
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    segments: Vec<SweepSegment3>,
    budget: f64,
) -> Result<(Snapshot, OperationReport), Vec<String>> {
    run(
        KernelCommand::SweepPlanarProfile {
            frame,
            profile,
            path: SweepPath3 { segments },
            orientation: SweepOrientation::RotationMinimising,
            operation: SolidOperation::New,
        },
        PrecisionPolicy {
            approximation_budget: budget,
            ..PrecisionPolicy::default()
        },
    )
}

fn loft(sections: Vec<LoftSection>) -> Snapshot {
    let (snapshot, report) = run(
        KernelCommand::LoftPlanarSections {
            sections,
            operation: LoftOperation::New,
        },
        PrecisionPolicy::default(),
    )
    .unwrap_or_else(|codes| panic!("refused: {codes:?}"));
    assert_eq!(report.rung.as_deref(), Some("loft/skinned"));
    assert_valid(&snapshot);
    snapshot
}

fn refusal(result: Result<(Snapshot, OperationReport), Vec<String>>) -> String {
    match result {
        Ok((snapshot, _)) => panic!("should be refused, built {:?}", snapshot.counts()),
        Err(codes) => codes.first().cloned().unwrap_or_default(),
    }
}

fn assert_valid(snapshot: &Snapshot) {
    let report = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(report.valid, "{:#?}", report.diagnostics);
}

fn assert_relative(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < tolerance,
        "{what}: {actual} should be {expected}"
    );
}

/// How many copies of the profile a skinned sweep says it took.
fn copies(report: &OperationReport) -> usize {
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "SWEEP_APPROXIMATION_TOLERANCE")
        .expect("a skinned sweep says what it met");
    warning
        .message
        .split("skinned through ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|count| count.parse().ok())
        .expect("the count of copies")
}

/// Up a line, three quarters round a bend of 1.5 and back along a line
/// through the way up: a tube of radius one that crosses itself. It was
/// built as a valid solid whose volume counted the crossing twice.
#[test]
fn a_path_back_through_the_tube_it_swept_is_refused_by_name() {
    let crossing = sweep(
        frame([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]),
        disc(1.0),
        vec![
            line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
            arc([1.5, 0.0, 5.0], [0.0, 0.0, 5.0], [0.0, 1.0, 0.0], 1.5 * PI),
            line([1.5, 0.0, 3.5], [-3.0, 0.0, 3.5]),
        ],
        1.0e-5,
    );
    assert_eq!(refusal(crossing), "SWEEP_SELF_INTERSECTS");

    // Coils 1.5 apart, of a wire 2 across.
    let points = helix(5.0, 1.5, 1.5, 16);
    let spring = sweep(square_to(&points), disc(1.0), vec![cubic(&points)], 1.0e-2);
    assert_eq!(refusal(spring), "SWEEP_SELF_INTERSECTS");
}

/// A disc round one turn of a spring. The frame turns with the path, and a
/// whole circle cut at one fixed direction was cut wherever the rounding of
/// that direction's shadow fell once the copies stood edge-on to it: the
/// skin could not be brought within even a loose budget, while the same
/// disc drawn as two arcs was. Now it is a tube of the path's length.
#[test]
fn a_whole_circle_round_a_spring_follows_the_frame_that_carries_it() {
    let points = helix(5.0, 3.0, 1.0, 8);
    let (spring, report) = sweep(square_to(&points), disc(1.0), vec![cubic(&points)], 1.0e-2)
        .unwrap_or_else(|codes| panic!("refused: {codes:?}"));
    assert_eq!(report.rung.as_deref(), Some("sweep/skinned"));
    assert_valid(&spring);
    assert_relative(
        spring.measures().volume,
        PI * cubic_length(&points),
        1.0e-2,
        "the tube formula",
    );
}

/// Three circles a quarter turn round a bend, the last on a frame turned so
/// that the first circle's cut direction stands square to its plane. Cut
/// at that one direction, the last circle fell back to a cut a quarter turn
/// round, and the walls twisted and pinched; carried from section to
/// section, the cut turns with the planes, and the loft is the one built
/// from the unturned frame.
#[test]
fn circles_on_turning_planes_loft_without_a_twist() {
    let bend = 4.0;
    let at = |angle: f64| {
        let (sin, cos) = angle.sin_cos();
        LoftSection {
            frame: frame(
                [bend * cos, 0.0, bend * sin],
                [cos, 0.0, sin],
                [0.0, 1.0, 0.0],
            ),
            profile: disc(1.0),
        }
    };
    let last = |u: [f64; 3], v: [f64; 3]| LoftSection {
        frame: frame([0.0, 0.0, bend], u, v),
        profile: disc(1.0),
    };
    let plain = loft(vec![
        at(0.0),
        at(PI / 4.0),
        last([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
    ]);
    let turned = loft(vec![
        at(0.0),
        at(PI / 4.0),
        last([0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
    ]);
    let volume = plain.measures().volume;
    assert_relative(turned.measures().volume, volume, 1.0e-9, "the turned frame");
    // A smooth loft through three circles round the bend, near the quarter
    // torus.
    assert_relative(volume, PI * bend * PI / 2.0, 2.0e-2, "the quarter torus");
}

/// Skinned bodies a hundred kilometres out. Inversion stopped only on a
/// parameter step the rounding there seldom lets it reach, so a sweep could
/// measure a departure as infinite — an elbow skinned to the default budget
/// was refused — and the validator could not place a wall's own edges on
/// it; and a rate taken from coordinates that large kept only their last
/// few bits. The lofts here were refused for both; the elbow, skinned to a
/// loose budget from few copies so as to be quick, rarely met either. Now
/// each is the body built at the origin, moved.
#[test]
fn skinned_bodies_build_a_hundred_kilometres_out() {
    let x = 1.0e5;
    let bend = 3.0;
    let (elbow, report) = sweep(
        frame([x, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        disc(1.0),
        vec![
            line([x, 0.0, 0.0], [x, 0.0, 5.0]),
            arc(
                [x + bend, 0.0, 5.0],
                [x, 0.0, 5.0],
                [0.0, 1.0, 0.0],
                PI / 2.0,
            ),
            line(
                [x + bend, 0.0, 5.0 + bend],
                [x + bend + 5.0, 0.0, 5.0 + bend],
            ),
        ],
        1.0e-2,
    )
    .unwrap_or_else(|codes| panic!("refused: {codes:?}"));
    assert_eq!(report.rung.as_deref(), Some("sweep/skinned"));
    assert_valid(&elbow);
    assert_relative(
        elbow.measures().volume,
        PI * (5.0 + bend * PI / 2.0 + 5.0),
        2.0e-3,
        "Pappus",
    );

    let sections = |origin: [f64; 3]| {
        let at = |z: f64, profile: PlanarProfile2| LoftSection {
            frame: frame(
                [origin[0], origin[1], origin[2] + z],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
            ),
            profile,
        };
        [
            vec![
                at(0.0, square(20.0)),
                at(10.0, disc(8.0)),
                at(20.0, square(20.0)),
            ],
            vec![at(0.0, disc(7.0)), at(10.0, disc(7.0)), at(25.0, disc(7.0))],
        ]
    };
    let [waist, cylinder] =
        sections([0.0, 0.0, 0.0]).map(|sections| loft(sections).measures().volume);
    for origin in [[1.0e5, 1.0e5, 1.0e5], [3.0e5, -2.0e5, 1.0e5]] {
        let [far_waist, far_cylinder] =
            sections(origin).map(|sections| loft(sections).measures().volume);
        assert_relative(far_waist, waist, 1.0e-9, "square, circle, square");
        assert_relative(far_cylinder, cylinder, 1.0e-9, "three circles");
    }
}

/// A disc of radius 2 round a bend of radius 3 is inside the centre of
/// curvature square to the path, and was let through leaning sixty degrees
/// back on it — where its far side's motion has no part along its normal
/// and the wall folds — to be refused later by the loft for copies that
/// cross, which says nothing of why. It is too wide, and said to be.
#[test]
fn a_profile_leaning_back_on_a_bend_is_refused_as_too_wide() {
    let (sin, cos) = (PI / 3.0).sin_cos();
    let leaning = sweep(
        frame([0.0, 0.0, 0.0], [cos, 0.0, -sin], [0.0, 1.0, 0.0]),
        disc(2.0),
        vec![
            line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
            arc([3.0, 0.0, 5.0], [0.0, 0.0, 5.0], [0.0, 1.0, 0.0], PI / 2.0),
        ],
        1.0e-5,
    );
    assert_eq!(refusal(leaning), "SWEEP_PROFILE_TOO_WIDE");
}

/// A spline path that stops dead where three control points coincide, and
/// one shorter than the feature floor, are unsound segments. They reached
/// the frames and were refused for whatever those upset — a degenerate
/// extrusion frame, a profile too wide for a curvature the stall made up.
#[test]
fn a_spline_path_that_stalls_or_has_no_length_is_refused_as_invalid() {
    let stalls = [
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 2.0],
        [1.0, 0.0, 4.0],
        [1.0, 0.0, 4.0],
        [1.0, 0.0, 4.0],
        [0.0, 0.0, 6.0],
        [0.0, 0.0, 8.0],
    ];
    let tiny = [
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0e-6],
        [0.0, 1.0e-6, 2.0e-6],
        [0.0, 0.0, 3.0e-6],
    ];
    for points in [&stalls[..], &tiny[..]] {
        let result = sweep(
            frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            disc(0.3),
            vec![cubic(points)],
            1.0e-5,
        );
        assert_eq!(refusal(result), "SWEEP_PATH_INVALID");
    }
}

/// A spline path of many knot spans asked for two copies to each, which
/// the cap on copies never saw: a path of four hundred control points was
/// skinned through nearly eight hundred, the walls interpolated through
/// them by a dense solve, in minutes. The first copies are now spread over
/// the path within half the cap, and the skin stays within it. The spread
/// itself is pinned by a quick unit test in `sweep_profile`; this is the
/// whole sweep, which needs more than a hundred copies to say anything.
#[test]
#[ignore = "slow: about a minute in a debug build"]
fn a_path_of_many_spline_spans_is_skinned_within_the_cap_on_copies() {
    let count = 150;
    let points = (0..count)
        .map(|index| {
            let t = f64::from(index) / f64::from(count - 1);
            [3.0 * (4.0 * PI * t).sin(), 0.0, 60.0 * t]
        })
        .collect::<Vec<_>>();
    let (wave, report) = sweep(square_to(&points), disc(0.5), vec![cubic(&points)], 1.0e-3)
        .unwrap_or_else(|codes| panic!("refused: {codes:?}"));
    assert_valid(&wave);
    let taken = copies(&report);
    assert!(taken <= 257, "skinned through {taken} copies");
    assert_relative(
        wave.measures().volume,
        PI * 0.25 * cubic_length(&points),
        1.0e-3,
        "the tube formula",
    );
}

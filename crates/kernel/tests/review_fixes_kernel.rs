//! Regression tests for the kernel review pass: each test pins a defect a
//! reviewer confirmed, with the closed-form answer the kernel now gives.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, KernelError,
    PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, RevolveAngle, ValidationProfile, Vector3,
};

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    let tolerance = 1.0e-9 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not {expected} (off by {})",
        actual - expected
    );
}

/// A prism tool wholly above or below a box, or standing on its top, takes
/// nothing away. The prism reduction used to read a tool standing on the
/// top as piercing it and build a pocket whose floor lay above the box, a
/// taller solid that validated.
#[test]
fn a_difference_with_a_prism_that_misses_the_target_leaves_it_whole() {
    for (label, bottom) in [("above", 15.0), ("below", -10.0), ("on_top", 10.0)] {
        let session = run(&format!(
            "let b = box(size: [10, 10, 10], label: \"b\");
let t = cylinder(center: [5, 5, {bottom}], radius: 20, height: 5, label: \"t\");
difference(target: b, tool: t, label: \"d\");
"
        ));
        assert_close(session.snapshot.measures().volume, 1000.0, label);
        let bounds = session.snapshot.measures().bounds.expect("bounds");
        assert_close(bounds.max.z, 10.0, label);
    }
    // A tool that does reach into the top still cuts a pocket to its floor.
    let session = run("let b = box(size: [10, 10, 10], label: \"b\");
let t = cylinder(center: [5, 5, 6], radius: 2, height: 10, label: \"t\");
difference(target: b, tool: t, label: \"d\");
");
    assert_close(
        session.snapshot.measures().volume,
        1000.0 - PI * 4.0 * 4.0,
        "a blind pocket",
    );
}

/// A profile on the XZ plane turned about the Z axis, from an empty model.
fn revolve(profile: PlanarProfile2, angle: RevolveAngle) -> Result<Snapshot, KernelError> {
    revolve_about(profile, Point2::new(0.0, 1.0), angle)
}

/// A profile on the XZ plane turned about the line through its origin along
/// `direction`.
fn revolve_about(
    profile: PlanarProfile2,
    direction: Point2,
    angle: RevolveAngle,
) -> Result<Snapshot, KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("review-revolve"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::RevolvePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile,
            axis: PlanarAxis2::new(Point2::new(0.0, 0.0), direction),
            angle,
            operation: Default::default(),
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

fn polygon_loop(vertices: &[(f64, f64)]) -> PlanarLoop2 {
    PlanarLoop2::from_polygon(
        &vertices
            .iter()
            .map(|(x, y)| Point2::new(*x, *y))
            .collect::<Vec<_>>(),
    )
}

fn region(outer: PlanarLoop2, holes: Vec<PlanarLoop2>) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 { outer, holes }],
    }
}

fn circle(center: (f64, f64), radius: f64) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(center.0, center.1),
            radius,
            direction: ArcDirection::CounterClockwise,
        }],
    }
}

fn refused_with(result: Result<Snapshot, KernelError>, code: &str, what: &str) {
    let error = result
        .err()
        .unwrap_or_else(|| panic!("{what}: should be refused"));
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == code),
        "{what}: expected {code}, got {:?}",
        error.diagnostics
    );
}

fn assert_solid(snapshot: &Snapshot, volume: f64, what: &str) {
    let report = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(report.valid, "{what}: {:?}", report.diagnostics);
    assert_close(snapshot.measures().volume, volume, what);
}

/// A profile that meets the axis at one point, rather than along an edge on
/// it, would turn into a solid pinched to a point there. The builder used to
/// reach an `unreachable!` for a corner on the axis and panic.
#[test]
fn a_profile_touching_the_axis_at_a_point_is_refused_without_a_panic() {
    let corner = polygon_loop(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0)]);
    let notch = polygon_loop(&[
        (0.0, 5.0),
        (0.0, 2.0),
        (2.0, 1.0),
        (0.0, 0.0),
        (3.0, 0.0),
        (3.0, 5.0),
    ]);
    for angle in [RevolveAngle::FullTurn, RevolveAngle::partial(0.0, PI / 2.0)] {
        for (what, outer) in [("corner", corner.clone()), ("notch", notch.clone())] {
            refused_with(
                revolve(region(outer, vec![]), angle),
                "REVOLVE_PROFILE_PINCHED_ON_AXIS",
                what,
            );
        }
    }
    // A circle tangent to the axis turns into a horn torus.
    refused_with(
        revolve(
            region(circle((2.0, 5.0), 2.0), vec![]),
            RevolveAngle::FullTurn,
        ),
        "REVOLVE_PROFILE_PINCHED_ON_AXIS",
        "horn torus",
    );
}

/// An arc whose ends both lie beside the axis can still bulge across it; an
/// axis-side test on endpoints alone let one through to a self-crossing
/// sweep. The loop's own splitting puts an arc's extremes at its ends only
/// along the frame's axes, so the axis here leans.
#[test]
fn an_arc_bulging_across_a_leaning_axis_is_refused() {
    // A D shape: a straight side at r = 2 and an arc about (1.5, 2) whose
    // far point is at r = 1.5 - √4.25 < 0, all turned by 30° with the axis.
    let (sin, cos) = 30.0_f64.to_radians().sin_cos();
    let turn = |x: f64, y: f64| Point2::new(x * cos - y * sin, x * sin + y * cos);
    let d_shape = PlanarLoop2 {
        curves: vec![
            PlanarCurve2::Line {
                start: turn(2.0, 0.0),
                end: turn(2.0, 4.0),
            },
            PlanarCurve2::CircularArc {
                center: turn(1.5, 2.0),
                start: turn(2.0, 4.0),
                end: turn(2.0, 0.0),
                direction: ArcDirection::CounterClockwise,
            },
        ],
    };
    refused_with(
        revolve_about(
            region(d_shape, vec![]),
            turn(0.0, 1.0),
            RevolveAngle::FullTurn,
        ),
        "REVOLVE_PROFILE_CROSSES_AXIS",
        "arc across the axis",
    );
}

/// Where the profile's loop starts does not matter: a rectangle on the axis
/// whose loop begins partway round still turns into its cylinder. The chain
/// once had to begin or end with its run along the axis.
#[test]
fn a_loop_starting_anywhere_turns_the_same() {
    let starts: [&[(f64, f64)]; 3] = [
        &[(0.0, 0.0), (3.0, 0.0), (3.0, 4.0), (0.0, 4.0)],
        &[(3.0, 0.0), (3.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
        &[(3.0, 4.0), (0.0, 4.0), (0.0, 0.0), (3.0, 0.0)],
    ];
    for vertices in starts {
        let solid = revolve(
            region(polygon_loop(vertices), vec![]),
            RevolveAngle::FullTurn,
        )
        .unwrap_or_else(|error| panic!("{vertices:?}: {:?}", error.diagnostics));
        assert_solid(&solid, PI * 9.0 * 4.0, "cylinder");
    }
}

/// A hole must lie inside its region and apart from its other holes, and
/// regions apart from one another, as for an extrusion; the revolve checked
/// none of it and committed overlapping or inside-out solids.
#[test]
fn holes_and_regions_that_overlap_are_refused() {
    let block = || polygon_loop(&[(1.0, 0.0), (5.0, 0.0), (5.0, 4.0), (1.0, 4.0)]);
    let outside = polygon_loop(&[(7.0, 1.0), (8.0, 1.0), (8.0, 2.0), (7.0, 2.0)]);
    let straddling = polygon_loop(&[(4.0, 1.0), (6.0, 1.0), (6.0, 2.0), (4.0, 2.0)]);
    for (what, hole) in [("outside", outside), ("straddling", straddling)] {
        refused_with(
            revolve(region(block(), vec![hole]), RevolveAngle::FullTurn),
            "PLANAR_PROFILE_REGIONS_OVERLAP",
            what,
        );
    }
    let overlapping = PlanarProfile2 {
        regions: vec![
            PlanarRegion2 {
                outer: block(),
                holes: vec![],
            },
            PlanarRegion2 {
                outer: polygon_loop(&[(3.0, 1.0), (7.0, 1.0), (7.0, 3.0), (3.0, 3.0)]),
                holes: vec![],
            },
        ],
    };
    refused_with(
        revolve(overlapping, RevolveAngle::FullTurn),
        "PLANAR_PROFILE_REGIONS_OVERLAP",
        "overlapping regions",
    );
}

/// An arc centred across the axis, a gentle barrel's side, turns into the
/// inner lemon of a spindle torus. It used to be snapped onto the axis and
/// fail validation with a string of mismatches; it is now named.
#[test]
fn an_arc_centred_across_the_axis_is_named() {
    let barrel = PlanarLoop2 {
        curves: vec![
            PlanarCurve2::Line {
                start: Point2::new(0.0, 0.0),
                end: Point2::new(1.0, 0.0),
            },
            PlanarCurve2::CircularArc {
                center: Point2::new(-5.0, 1.0),
                start: Point2::new(1.0, 0.0),
                end: Point2::new(1.0, 2.0),
                direction: ArcDirection::CounterClockwise,
            },
            PlanarCurve2::Line {
                start: Point2::new(1.0, 2.0),
                end: Point2::new(0.0, 2.0),
            },
            PlanarCurve2::Line {
                start: Point2::new(0.0, 2.0),
                end: Point2::new(0.0, 0.0),
            },
        ],
    };
    refused_with(
        revolve(region(barrel, vec![]), RevolveAngle::FullTurn),
        "REVOLVE_ARC_CENTRE_ACROSS_AXIS",
        "barrel",
    );
}

fn rung_of(session: &Session, label: &str) -> String {
    session
        .report()
        .steps
        .iter()
        .find(|step| step.label == label)
        .and_then(|step| step.rung.clone())
        .unwrap_or_default()
}

/// Two caps of a turned body at one height — a disc and a ring, with a
/// groove between — are two faces. Opening the ring used to open the disc
/// as well, because an open face was matched to the section by its height
/// alone.
#[test]
fn opening_one_of_two_caps_at_one_height_opens_only_it() {
    let session = run("let s = sketch(on: \"XZ\", label: \"s\", entities: [
    line(start: [0, 0], end: [20, 0]),
    line(start: [20, 0], end: [20, 10]),
    line(start: [20, 10], end: [14, 10]),
    line(start: [14, 10], end: [14, 6]),
    line(start: [14, 6], end: [6, 6]),
    line(start: [6, 6], end: [6, 10]),
    line(start: [6, 10], end: [0, 10]),
    line(start: [0, 10], end: [0, 0]),
]);
let hub = revolve(sketch: s, axis: [0, 0, 1], label: \"hub\");
shell(open: nearest(point: [17, 0, 10], kind: \"face\"), wall: 2, label: \"cup\");
");
    // The core, offset 2 in and run out through the ring, is r ≤ 18 for
    // 2 ≤ z ≤ 4, r ≤ 4 up to the disc's underside at z = 8, and the ring's
    // bore 16 ≤ r ≤ 18 on up through the top.
    let body = PI * 400.0 * 10.0 - PI * (196.0 - 36.0) * 4.0;
    let core = PI * (324.0 * 2.0 + 16.0 * 4.0 + (324.0 - 256.0) * 6.0);
    assert_close(session.snapshot.measures().volume, body - core, "volume");
    assert_eq!(rung_of(&session, "cup"), "shell/open-revolve");
}

//! Regression tests for the kernel review pass: each test pins a defect a
//! reviewer confirmed, with the closed-form answer the kernel now gives.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EntityRef,
    ExecuteRequest, FaceExtrusionOperation, KernelCommand, KernelError, PlanarAxis2, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, RevolveAngle, RotationQuaternion, SimilarityTransform3, ValidationProfile, Vector3,
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

/// A 10 × 10 × 10 box with a 4 × 4 pocket cut up 8 from its underside,
/// then its top pushed down by `distance`.
fn pocketed_box_pushed(distance: f64) -> String {
    format!(
        "let b = box(size: [10, 10, 10], label: \"b\");
let under = sketch(on: faces(\"<Z\"), label: \"under\", entities: [
    rect(origin: [-2, -2], width: 4, height: 4),
]);
extrude(sketch: under, distance: 8, operation: \"cut\", label: \"pocket\");
push_pull(face: faces(\">Z\"), distance: {distance}, label: \"lower\");
"
    )
}

/// Pushing a top down past the ceiling of a pocket beneath it would leave
/// the pocket poking out through the new top. The push used to commit that
/// self-intersecting solid, which validated.
#[test]
fn pushing_a_cap_down_through_a_pocket_below_is_refused() {
    let session = run(&pocketed_box_pushed(-1.0));
    assert_close(
        session.snapshot.measures().volume,
        900.0 - 16.0 * 8.0,
        "a cap lowered clear of the pocket",
    );
    let mut session = Session::new();
    let outcome = session.run_script(
        &pocketed_box_pushed(-5.0),
        &BTreeMap::new(),
        &CancellationToken::default(),
    );
    let failure = outcome
        .failure
        .unwrap_or_else(|| panic!("committed volume {}", session.snapshot.measures().volume));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "FACE_PUSH_PULL_INTERIOR_CONTACT"),
        "{failure:?}"
    );
}

fn execute(base: &Snapshot, command: KernelCommand) -> Result<Snapshot, KernelError> {
    NativeKernel::execute(
        base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("review-step"),
            expected_snapshot: base.id(),
            precision: PrecisionPolicy::default(),
            command,
        },
        &CancellationToken::new(),
    )
    .map(|outcome| outcome.snapshot)
}

/// The face whose every display triangle satisfies `on`.
fn face_where(snapshot: &Snapshot, on: impl Fn(Point3) -> bool) -> EntityRef {
    NativeKernel::debug_scene(snapshot)
        .triangles
        .iter()
        .find(|triangle| triangle.vertices.iter().all(|point| on(*point)))
        .expect("the face")
        .source_face
}

fn rectangle(x: (f64, f64), y: (f64, f64)) -> PlanarProfile2 {
    region(
        polygon_loop(&[(x.0, y.0), (x.1, y.0), (x.1, y.1), (x.0, y.1)]),
        vec![],
    )
}

fn face_cut(
    base: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    distance: f64,
    operation: FaceExtrusionOperation,
) -> Result<Snapshot, KernelError> {
    execute(
        base,
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            frame,
            profile,
            distance,
            operation,
        },
    )
}

/// A 20 × 20 × 10 block bored through along X, radius 2, at y = 10, z = 5.
fn bored_block() -> Snapshot {
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 20.0,
            size_y: 20.0,
            size_z: 10.0,
        },
    )
    .expect("block");
    let side = face_where(&block, |point| point.x.abs() <= 1.0e-9);
    face_cut(
        &block,
        side,
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        region(circle((10.0, 5.0), 2.0), vec![]),
        20.0,
        FaceExtrusionOperation::Cut,
    )
    .expect("bore")
}

/// A sketch frame on a face may face either way; the cut still goes into
/// the body. Where the face-feature path handed a cut on to the general
/// engines — a sweep that meets other geometry, a profile past the face's
/// edge — a frame facing into the body used to have its axes swapped, which
/// moved the profile to its mirror image across the frame's diagonal, or
/// sent the tool out of the body where it removed nothing.
#[test]
fn a_cut_sketched_on_a_frame_facing_into_the_body_lands_where_it_was_drawn() {
    let base = bored_block();
    let top = face_where(&base, |point| (point.z - 10.0).abs() <= 1.0e-9);
    let origin = Point3::new(0.0, 0.0, 10.0);
    let (x, y) = (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0));
    // x in [2, 6], y in [4, 16], 8 deep: across the bore, which it shares
    // for a length of 4.
    let expected = 20.0 * 20.0 * 10.0 - PI * 4.0 * 20.0 - 4.0 * 12.0 * 8.0 + PI * 4.0 * 4.0;
    for (what, frame, profile) in [
        (
            "outward",
            PlanarFrame3::new(origin, x, y),
            rectangle((2.0, 6.0), (4.0, 16.0)),
        ),
        (
            "inward",
            PlanarFrame3::new(origin, y, x),
            rectangle((4.0, 16.0), (2.0, 6.0)),
        ),
    ] {
        let cut = face_cut(&base, top, frame, profile, 8.0, FaceExtrusionOperation::Cut)
            .unwrap_or_else(|error| panic!("{what}: {:?}", error.diagnostics));
        assert_solid(&cut, expected, what);
    }

    // A circle of radius 1 half over the block's x = 20 edge, from a frame
    // facing into the block: a cut 0.4 deep takes the half over the block,
    // and a boss 0.4 high, overhang and all, adds the whole of it.
    let inward = PlanarFrame3::new(origin, y, x);
    let over_the_edge = region(circle((3.0, 20.0), 1.0), vec![]);
    let half = PI * 0.4 / 2.0;
    for (operation, expected) in [
        (FaceExtrusionOperation::Cut, 4000.0 - PI * 80.0 - half),
        (FaceExtrusionOperation::Add, 4000.0 - PI * 80.0 + 2.0 * half),
    ] {
        let feature = face_cut(&base, top, inward, over_the_edge.clone(), 0.4, operation)
            .unwrap_or_else(|error| panic!("{operation:?}: {:?}", error.diagnostics));
        let report = NativeKernel::validate(&feature, ValidationProfile::Solid);
        assert!(report.valid, "{operation:?}: {:?}", report.diagnostics);
        let volume = feature.measures().volume;
        assert!(
            (volume - expected).abs() <= 1.0e-3 * half,
            "{operation:?}: {volume} is not {expected}"
        );
    }
}

/// A cut that stops short of the far face by less than the minimum feature
/// would leave a floor thinner than any feature the kernel keeps. It used to
/// build that film of a floor; it now goes through.
#[test]
fn a_cut_stopping_a_hair_short_of_the_far_face_goes_through() {
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        },
    )
    .expect("block");
    let top = face_where(&block, |point| (point.z - 10.0).abs() <= 1.0e-9);
    let floor = PrecisionPolicy::default().min_feature_size / 2.0;
    let cut = face_cut(
        &block,
        top,
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 10.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        rectangle((3.0, 7.0), (3.0, 7.0)),
        10.0 - floor,
        FaceExtrusionOperation::Cut,
    )
    .unwrap_or_else(|error| panic!("{:?}", error.diagnostics));
    assert_solid(&cut, 1000.0 - 16.0 * 10.0, "a through hole");
}

/// A mirror or a pattern can carry a body past the coordinate limit every
/// other construction keeps to. Both used to commit it unchecked, and a
/// mirror plane with a non-finite normal passed the zero-length test.
#[test]
fn a_mirror_or_pattern_past_the_coordinate_limit_is_refused() {
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        },
    )
    .expect("block");
    let limit = PrecisionPolicy::default().max_abs_coordinate;
    let codes = |result: Result<Snapshot, KernelError>| {
        result
            .err()
            .map(|error| {
                error
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code.as_str().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let mirror = |origin: Point3, normal: Vector3| {
        execute(
            &block,
            KernelCommand::MirrorSnapshot {
                plane_origin: origin,
                plane_normal: normal,
            },
        )
    };
    let far = mirror(
        Point3::new(0.6 * limit, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
    );
    assert!(
        codes(far).contains(&"TRANSFORM_COORDINATE_LIMIT_EXCEEDED".to_owned()),
        "a mirror past the limit"
    );
    let near = mirror(Point3::new(20.0, 0.0, 0.0), Vector3::new(1.0, 0.0, 0.0))
        .expect("a mirror within the limit");
    assert_solid(&near, 1000.0, "mirrored block");
    assert!(
        codes(mirror(
            Point3::new(20.0, 0.0, 0.0),
            Vector3::new(f64::NAN, 0.0, 0.0)
        ))
        .contains(&"MIRROR_DOMAIN_UNSUPPORTED".to_owned()),
        "a NaN normal"
    );
    let pattern = execute(
        &block,
        KernelCommand::LinearPatternSnapshot {
            direction: Vector3::new(1.0, 0.0, 0.0),
            spacing: 0.4 * limit,
            count: 5,
        },
    );
    assert!(
        codes(pattern).contains(&"TRANSFORM_COORDINATE_LIMIT_EXCEEDED".to_owned()),
        "a pattern past the limit"
    );
}

/// A 10-cube at `x0` less a 3 × 3 bar tilted through it, and the rung that
/// answered.
fn tilted_bar_cut(x0: f64) -> (Snapshot, Option<String>) {
    let cube = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(x0, 0.0, 0.0),
            size_x: 10.0,
            size_y: 10.0,
            size_z: 10.0,
        },
    )
    .expect("cube");
    let bar = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(-10.0, -1.5, -1.5),
            size_x: 20.0,
            size_y: 3.0,
            size_z: 3.0,
        },
    )
    .expect("bar");
    // 20° about (0, 1, 1): off every axis the cube is a prism along.
    let (sin, cos) = 10.0_f64.to_radians().sin_cos();
    let half = std::f64::consts::FRAC_1_SQRT_2 * sin;
    let bar = execute(
        &bar,
        KernelCommand::TransformSnapshot {
            transform: SimilarityTransform3 {
                translation: Vector3::new(x0 + 5.0, 5.0, 5.0),
                rotation: RotationQuaternion {
                    w: cos,
                    x: 0.0,
                    y: half,
                    z: half,
                },
                uniform_scale: 1.0,
            },
        },
    )
    .expect("tilted bar");
    let outcome = NativeKernel::execute_boolean(
        &cube,
        &bar,
        &BooleanRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("tilted-bar"),
            expected_target_snapshot: cube.id(),
            expected_tool_snapshot: bar.id(),
            precision: PrecisionPolicy::default(),
            operation: BooleanOperation::Difference,
        },
        &CancellationToken::new(),
    )
    .unwrap_or_else(|error| panic!("x0 = {x0}: {:?}", error.diagnostics));
    (outcome.snapshot, outcome.report.rung)
}

/// Where two planes meet, the exact engine drew their line a million units
/// either way of the point on it nearest the world origin, so a body far out
/// along the line lay off the end of it. The line is now drawn across the
/// face it is clipped to.
#[test]
fn a_boolean_far_along_a_plane_crossing_matches_one_at_the_origin() {
    let (near, near_rung) = tilted_bar_cut(0.0);
    let (far, far_rung) = tilted_bar_cut(3.0e6);
    let report = NativeKernel::validate(&far, ValidationProfile::Solid);
    assert!(report.valid, "{:?}", report.diagnostics);
    assert_eq!(near_rung, far_rung);
    assert!(
        (near.measures().volume - far.measures().volume).abs() <= 1.0e-6,
        "{} near, {} far",
        near.measures().volume,
        far.measures().volume
    );
    assert!(
        near.measures().volume < 1000.0 - 80.0,
        "the bar cuts the cube"
    );
}

/// A fillet on one bore's rim in a plate with two bores. The rim-loop rung's
/// check that the other loops stay on their side of the finished rim cast
/// its ray from each loop's seam, where two arcs meet only to within
/// rounding; from one bore's seam it slipped through the other's and read
/// the bore beside the finish as inside it, refusing a fillet with room to
/// spare.
#[test]
fn a_bore_rim_beside_another_bore_fillets() {
    let session = run("let s = sketch(on: \"XY\", label: \"s\", entities: [
    rect(origin: [-10, -6], width: 20, height: 12),
    circle(center: [-5, 0], radius: 4),
    circle(center: [5, 0], radius: 4),
]);
let plate = extrude(sketch: s, distance: 3, label: \"plate\");
fillet(edges: [nearest(point: [5, 4, 3], kind: \"edge\"), nearest(point: [5, -4, 3], kind: \"edge\")], radius: 0.4, label: \"f\");
");
    // The fillet takes the corner square of side r less its quarter disc,
    // turned about the bore's axis at its centroid's radius (Pappus).
    let (bore, r) = (4.0_f64, 0.4_f64);
    let square = r * r;
    let quarter = PI * r * r / 4.0;
    let moment = square * (bore + r / 2.0) - quarter * (bore + r - 4.0 * r / (3.0 * PI));
    let removed = 2.0 * PI * moment;
    assert_close(
        session.snapshot.measures().volume,
        (240.0 - 2.0 * PI * 16.0) * 3.0 - removed,
        "plate less one filleted rim",
    );
}

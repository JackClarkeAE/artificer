//! A profile that touches the face it is drawn on.
//!
//! An annular boss around a hole is the commonest boss there is, and its
//! inner rim *is* the hole's rim: not near it, on it. The face-feature gate
//! reads that as a profile outside face material and reformulates the
//! operation as a Boolean, and an add used to stop there — only a cut went on
//! to the general engine — so on any body the prism reduction could not carry
//! the boss was refused as though it had been drawn off the face. Since ADR
//! 0045 the general engine resolves a coincident boundary exactly, and an add
//! now reaches it.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIZE: f64 = 40.0;
const RADIUS: f64 = 8.0;
const BOSS_RADIUS: f64 = 12.0;
const BOSS_HEIGHT: f64 = 5.0;

fn cuboid() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("touching-cuboid"),
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

fn disc(radius: f64, direction: ArcDirection) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(0.0, 0.0),
            radius,
            direction,
        }],
    }
}

fn extrude(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    distance: f64,
    operation: FaceExtrusionOperation,
    label: &str,
) -> Result<Snapshot, String> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            frame,
            profile,
            distance,
            operation,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
        .map_err(|error| format!("{error:?}"))
}

fn through_cut(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    label: &str,
) -> Snapshot {
    extrude(
        snapshot,
        target_face,
        frame,
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: disc(RADIUS, ArcDirection::CounterClockwise),
                holes: vec![],
            }],
        },
        1_000.0,
        FaceExtrusionOperation::Cut,
        label,
    )
    .unwrap_or_else(|error| panic!("{label} should build: {error}"))
}

fn side_frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(SIZE, SIZE / 2.0, SIZE / 2.0),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    )
}

/// Two bores crossing, so the body is a prism about no axis and nothing short
/// of the general engine can add to it.
fn crossed_box() -> Snapshot {
    let box_body = cuboid();
    let top = face_where(&box_body, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    let bored = through_cut(
        &box_body,
        top,
        PlanarFrame3::new(
            Point3::new(SIZE / 2.0, SIZE / 2.0, SIZE),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        "touching-first-bore",
    );
    let side = face_where(&bored, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    through_cut(&bored, side, side_frame(), "touching-second-bore")
}

fn annulus() -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: disc(BOSS_RADIUS, ArcDirection::CounterClockwise),
            holes: vec![disc(RADIUS, ArcDirection::Clockwise)],
        }],
    }
}

/// The reported case: a boss around a bore, its inner rim on the bore's rim,
/// on a body the prism reduction cannot carry. The boss adds exactly its own
/// annular volume: its void lines up with the bore and adds nothing.
#[test]
fn an_annular_boss_around_a_bore_adds_exactly_its_own_volume() {
    let crossed = crossed_box();
    let before = crossed.measures().volume;
    let side = face_where(&crossed, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    let bossed = extrude(
        &crossed,
        side,
        side_frame(),
        annulus(),
        BOSS_HEIGHT,
        FaceExtrusionOperation::Add,
        "touching-boss",
    )
    .expect("a boss whose rim is the hole's rim is the commonest boss there is");
    assert!(NativeKernel::validate(&bossed, ValidationProfile::Solid).valid);
    let expected =
        before + std::f64::consts::PI * (BOSS_RADIUS * BOSS_RADIUS - RADIUS * RADIUS) * BOSS_HEIGHT;
    let volume = bossed.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "exactly the annulus, and no more: {volume} vs {expected}"
    );
}

/// The same boss cut instead of added — a counterbore — is exact too.
#[test]
fn an_annular_counterbore_around_a_bore_removes_exactly_its_own_volume() {
    let crossed = crossed_box();
    let before = crossed.measures().volume;
    let side = face_where(&crossed, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    let counterbored = extrude(
        &crossed,
        side,
        side_frame(),
        annulus(),
        BOSS_HEIGHT,
        FaceExtrusionOperation::Cut,
        "touching-counterbore",
    )
    .expect("a counterbore's inner rim is the bore's rim");
    assert!(NativeKernel::validate(&counterbored, ValidationProfile::Solid).valid);
    let expected =
        before - std::f64::consts::PI * (BOSS_RADIUS * BOSS_RADIUS - RADIUS * RADIUS) * BOSS_HEIGHT;
    let volume = counterbored.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "exactly the annulus: {volume} vs {expected}"
    );
}

/// An add whose profile misses the face altogether still refuses: the union
/// of two solids that never meet is not a solid.
#[test]
fn a_boss_that_misses_the_face_still_refuses() {
    let crossed = crossed_box();
    let side = face_where(&crossed, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    let off_face = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(SIZE * 2.0, 0.0),
                    radius: 3.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let error = extrude(
        &crossed,
        side,
        side_frame(),
        off_face,
        BOSS_HEIGHT,
        FaceExtrusionOperation::Add,
        "touching-miss",
    )
    .expect_err("nothing to add to");
    assert!(
        error.contains("FACE_FEATURE_PROFILE_OUTSIDE_FACE"),
        "refused by name: {error}"
    );
}

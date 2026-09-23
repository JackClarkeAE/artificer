//! What two crossing round cuts actually cost, and what they actually are.
//!
//! Reported from Windows testing as "the app started lagging incredibly badly".
//! The lag was a symptom. A box with one round through-cut is exact — eight
//! faces, and a volume that matches the closed form to the last digit. Adding a
//! second cut that crosses the first bore leaves the exact domain entirely:
//! `sweep_contacts_source` rejects any cylinder whose axis is not parallel to
//! the sweep, that refusal is the sole admission ticket to the faceted BSP
//! fallback, and the whole body is rebuilt from a tessellation.
//!
//! That is the history. Two equal-radius bores on perpendicular intersecting
//! axes meet in two ellipses — the Steinmetz case that ADR 0026 K1 named as the
//! missing curve vocabulary — and K1 has landed, so this body is now exact and
//! these gates pin the closed form rather than a bound on the error. Bores of
//! unequal radius, or on axes that do not meet, followed with ADR 0047: they
//! meet in a space quartic the kernel now carries, and are exact too. The
//! faceted tier's own contract — that an approximation says so — is held by
//! `report_tests.rs` and `exact_route_decline.rs` on a cut the exact engine
//! still does not carry.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIZE: f64 = 40.0;
const RADIUS: f64 = 8.0;

fn cuboid() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("crossing-cut-cuboid"),
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

fn through_cut_outcome(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    label: &str,
) -> artificer_kernel::ExecutionOutcome {
    through_cut_outcome_at(
        snapshot,
        target_face,
        frame,
        Point2::new(0.0, 0.0),
        RADIUS,
        label,
    )
}

fn through_cut_outcome_at(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    centre: Point2,
    radius: f64,
    label: &str,
) -> artificer_kernel::ExecutionOutcome {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: centre,
                            radius,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: vec![],
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
}

fn through_cut(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    label: &str,
) -> Snapshot {
    through_cut_outcome(snapshot, target_face, frame, label).snapshot
}

fn bored_box() -> Snapshot {
    let box_body = cuboid();
    let top = face_where(&box_body, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    through_cut(
        &box_body,
        top,
        PlanarFrame3::new(
            Point3::new(SIZE / 2.0, SIZE / 2.0, SIZE),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        "crossing-cut-first",
    )
}

fn crossed_box() -> Snapshot {
    let bored = bored_box();
    let side = face_where(&bored, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    through_cut(
        &bored,
        side,
        PlanarFrame3::new(
            Point3::new(SIZE, SIZE / 2.0, SIZE / 2.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        "crossing-cut-second",
    )
}

/// The single bore is exact, and stays that way: eight faces and a volume that
/// is the closed form, not near it.
#[test]
fn one_round_through_cut_is_exact() {
    let bored = bored_box();
    assert!(NativeKernel::validate(&bored, ValidationProfile::Solid).valid);
    assert_eq!(
        bored.counts().faces,
        8,
        "a bored box is six planar faces and two half-cylinder walls"
    );
    let expected = SIZE.powi(3) - std::f64::consts::PI * RADIUS * RADIUS * SIZE;
    let volume = bored.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "one bore must be exact: {volume} vs {expected}"
    );
}

/// The crossing cut is exact. This gate used to bound how far off it was and
/// to say, in its own assertion message, that the day the body became exact the
/// gate should be replaced by the closed form. That day came.
#[test]
fn crossing_cuts_are_exact_and_publish_the_steinmetz_closed_form() {
    let crossed = crossed_box();
    assert!(NativeKernel::validate(&crossed, ValidationProfile::Solid).valid);

    // Cube, less two bores, plus the Steinmetz solid they share — the volume
    // counted twice by subtracting each bore whole.
    let exact = SIZE.powi(3) - 2.0 * (std::f64::consts::PI * RADIUS * RADIUS * SIZE)
        + 16.0 * RADIUS.powi(3) / 3.0;
    let volume = crossed.measures().volume;
    assert!(
        ((volume - exact) / exact).abs() < 1.0e-12,
        "two crossing bores must be the closed form, not near it: {volume} vs {exact}"
    );

    // Ten faces, and each is a surface rather than a panel of one: the box's
    // six planes, plus the two half-walls each bore already has — the test
    // above pins a single bore at eight. So the crossing adds no face: the seam
    // the bores share parts neither wall, because each was already parted at
    // its own cylinder's seam. The faceted route reached this body as 2,959
    // faces before the coplanar merge of ADR 0039 and 229 after it; the exact
    // route does not fragment at all, because there is nothing to fragment.
    let counts = crossed.counts();
    assert_eq!(counts.faces, 10, "six planes and four half-bore walls");
    assert_eq!(counts.shells, 1);
}

/// The cost the user actually felt: the display scene is rebuilt on every
/// commit, and it used to be quadratic in face count.
#[test]
fn the_display_scene_of_a_faceted_body_builds_promptly() {
    let crossed = crossed_box();
    let started = std::time::Instant::now();
    let scene = NativeKernel::debug_scene(&crossed);
    let elapsed = started.elapsed();
    assert!(!scene.triangles.is_empty());
    // Release only. The figure that matters and the one that regressed is the
    // optimised one: this machine measured 87.8 ms before the edge
    // classification stopped scanning every face for every edge, and 8.8 ms
    // after. An unoptimised build is an order of magnitude slower for reasons
    // that have nothing to do with the algorithm, and asserting a wall clock
    // there just times a shared CI runner — which is how this gate first
    // failed, at 4.25 s against a 4 s budget on a Windows runner while the
    // algorithm was fine.
    if cfg!(debug_assertions) {
        eprintln!("display scene built in {elapsed:?} (unoptimised; not gated)");
        return;
    }
    assert!(
        elapsed < std::time::Duration::from_millis(40),
        "the display scene took {elapsed:?}; the edge classification is quadratic again"
    );
}

/// A bore of another radius crossing the first. This was the smallest change
/// that left the exact domain — the `r²` that cancels between two equal
/// cylinders' equations does not cancel here, and the seam really is a
/// quartic — so it was the test that the faceted tier labels itself. The
/// quartic is carried now (ADR 0047), and the crossing is exact: the cube, less
/// both bores, plus what they share, `4∫√((64 − y²)(36 − y²)) dy` over the
/// narrow bore's width, integrated here without the kernel.
#[test]
fn a_bore_of_another_radius_crossing_the_first_is_exact() {
    let bored = bored_box();
    let side = face_where(&bored, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    let narrow = RADIUS * 0.75;
    let outcome = through_cut_outcome_at(
        &bored,
        side,
        PlanarFrame3::new(
            Point3::new(SIZE, SIZE / 2.0, SIZE / 2.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        Point2::new(0.0, 0.0),
        narrow,
        "crossing-cut-unequal",
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact crossing carries no caveat: {:?}",
        outcome.report.warnings
    );
    assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
    // Walked through `y = −r + 2r(3t² − 2t³)`, whose rate vanishes at both
    // ends, the square roots there become smooth and Simpson converges.
    let panels = 20_000;
    let step = 1.0 / f64::from(panels);
    let shared = (0..=panels)
        .map(|index| {
            let t = f64::from(index) * step;
            let y = (2.0 * narrow).mul_add(t * t * 2.0f64.mul_add(-t, 3.0), -narrow);
            let rate = 12.0 * narrow * t * (1.0 - t);
            let weight = if index == 0 || index == panels {
                1.0
            } else if index % 2 == 1 {
                4.0
            } else {
                2.0
            };
            let chords = 4.0
                * y.mul_add(-y, RADIUS * RADIUS).max(0.0).sqrt()
                * y.mul_add(-y, narrow * narrow).max(0.0).sqrt();
            weight * chords * rate
        })
        .sum::<f64>()
        * step
        / 3.0;
    let expected =
        SIZE.powi(3) - std::f64::consts::PI * (RADIUS * RADIUS + narrow * narrow) * SIZE + shared;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "the unequal crossing is exact: {volume} vs {expected}"
    );
}

/// The exact route must stay silent. A caveat on every cut would be noise, and
/// noise is how a real caveat gets ignored.
#[test]
fn an_exact_cut_publishes_no_approximation_warning() {
    let box_body = cuboid();
    let top = face_where(&box_body, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    let outcome = through_cut_outcome(
        &box_body,
        top,
        PlanarFrame3::new(
            Point3::new(SIZE / 2.0, SIZE / 2.0, SIZE),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        "single-cut-warning",
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact cut must publish no warnings, got {:?}",
        outcome.report.warnings
    );
}

/// A second bore that misses the first stays exact — and this records WHY it
/// does, because the reason is not the guard people expect.
///
/// `sweep_contacts_source` rejects any cylinder whose axis is not parallel to
/// the sweep, on orientation alone, without asking whether it is anywhere near
/// the profile. That looks like it should send every second bore to the faceted
/// fallback. It does not, because the axis-aligned bounds test upstream culls
/// the first bore before the cylinder arm is ever reached. Tightening the
/// orientation guard into a real footprint test was tried and reverted: it only
/// changes the answer when the cut frame is oblique to the existing bore's
/// axis, which needs a deliberately rotated frame to reach, and an untested
/// tightening of an exactness guard is not worth carrying.
/// A second bore that misses the first must stay exact.
///
/// The guard used to reject any cylinder whose axis was not parallel to the
/// sweep, on orientation alone — so a bore anywhere inside the profile's
/// bounding box sent the whole cut to the faceted fallback even when the two
/// never came near each other. Testing the cylinder's real footprint keeps
/// these exact.
#[test]
fn a_second_bore_that_misses_the_first_stays_exact() {
    let bored = bored_box();
    let side = face_where(&bored, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    // Offset across the first bore's axis, not along it: the first bore spans
    // the whole height, so only a sideways offset actually clears it. Their
    // bounding boxes still overlap, so nothing short of a real footprint test
    // can tell these two apart.
    let outcome = through_cut_outcome_at(
        &bored,
        side,
        PlanarFrame3::new(
            Point3::new(SIZE, SIZE / 2.0, SIZE / 2.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        Point2::new(14.0, 0.0),
        4.0,
        "offset-second-bore",
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "a bore clear of the first must stay exact, got {:?}",
        outcome.report.warnings
    );
    let faces = outcome.snapshot.counts().faces;
    assert!(
        faces <= 12,
        "an exact second bore adds a wall and a cap, not a fan: {faces} faces"
    );
    let expected = SIZE.powi(3)
        - std::f64::consts::PI * RADIUS * RADIUS * SIZE
        - std::f64::consts::PI * 4.0 * 4.0 * SIZE;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "two clear bores must both be exact: {volume} vs {expected}"
    );
}

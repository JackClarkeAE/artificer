//! A cut that crosses an interior void must keep cutting on the far side.
//!
//! Reported from the workbench: a plate with a slot already through it, and a
//! profile cut from the large face whose sweep crosses the slot. The exact
//! rewrite used to certify the slot's roof as the cut's exit — it is an
//! anti-parallel planar face that strictly contains the profile, exactly like
//! a true bottom — and silently truncated the cut there, leaving the material
//! past the slot untouched in both the preview and the committed body. The
//! exit is only a through-exit when nothing lies beyond it; when material
//! resumes past the void, the cut must fall back to a real difference.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIZE: f64 = 40.0;

fn cuboid() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("slotted-cut-cuboid"),
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

fn rectangle_profile(min: Point2, max: Point2) -> PlanarProfile2 {
    let corners = [
        Point2::new(min.x, min.y),
        Point2::new(max.x, min.y),
        Point2::new(max.x, max.y),
        Point2::new(min.x, max.y),
    ];
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: (0..4)
                    .map(|index| PlanarCurve2::Line {
                        start: corners[index],
                        end: corners[(index + 1) % 4],
                    })
                    .collect(),
            },
            holes: vec![],
        }],
    }
}

fn rectangle_cut_outcome(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    min: Point2,
    max: Point2,
    distance: f64,
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
            profile: rectangle_profile(min, max),
            distance,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
}

/// The box with a rectangular tunnel through it along X: the slot spans
/// `y ∈ [10, 30]`, `z ∈ [18, 22]`, all the way from `x = 40` to `x = 0`.
fn slotted_box() -> Snapshot {
    let box_body = cuboid();
    let side = face_where(&box_body, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    let outcome = rectangle_cut_outcome(
        &box_body,
        side,
        PlanarFrame3::new(
            Point3::new(SIZE, 20.0, 20.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        Point2::new(-10.0, -2.0),
        Point2::new(10.0, 2.0),
        1_000.0,
        "slotted-cut-slot",
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "a straight-through slot has a clean exit and stays exact: {:?}",
        outcome.report.warnings
    );
    outcome.snapshot
}

/// The reported bug. A cut from the top face whose sweep crosses the tunnel
/// used to stop at the tunnel's roof: the preview showed the pocket ending
/// there and the committed body kept all the material below the tunnel.
#[test]
fn a_cut_crossing_an_interior_slot_removes_the_far_side_too() {
    let slotted = slotted_box();
    let top = face_where(&slotted, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    let outcome = rectangle_cut_outcome(
        &slotted,
        top,
        PlanarFrame3::new(
            Point3::new(20.0, 20.0, SIZE),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        Point2::new(-5.0, -5.0),
        Point2::new(5.0, 5.0),
        1_000.0,
        "slotted-cut-crossing",
    );
    assert!(
        NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid,
        "the crossing cut must still publish a valid solid"
    );

    // Box, less the tunnel, less the full-depth cut, plus their shared block.
    let tunnel = SIZE * 20.0 * 4.0;
    let cut = 10.0 * 10.0 * SIZE;
    let shared = 10.0 * 10.0 * 4.0;
    let expected = SIZE.powi(3) - tunnel - cut + shared;
    // The silently truncated pocket the bug used to publish, for contrast:
    // it stopped at the tunnel roof 18 deep.
    let truncated = SIZE.powi(3) - tunnel - 10.0 * 10.0 * 18.0;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-6,
        "the cut must continue past the slot: got {volume}, want {expected} \
         (the truncated bug would publish {truncated})"
    );

    // This body is all planes, and the general analytic engine now carries
    // it exactly: the cut is a certified difference, not a faceted rebuild,
    // so it carries no approximation caveat.
    assert!(
        outcome.report.warnings.is_empty(),
        "an all-plane crossing cut is exact: {:?}",
        outcome.report.warnings
    );
    assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
}

/// The canonicalized overtravel contract survives: past the last face there
/// is nothing, so a deep request through a plain box is still the exact
/// through cut, warning-free.
#[test]
fn overtravel_through_a_plain_box_stays_exact() {
    let box_body = cuboid();
    let top = face_where(&box_body, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    let outcome = rectangle_cut_outcome(
        &box_body,
        top,
        PlanarFrame3::new(
            Point3::new(20.0, 20.0, SIZE),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        Point2::new(-5.0, -5.0),
        Point2::new(5.0, 5.0),
        1_000.0,
        "slotted-cut-plain-overtravel",
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "a clean through cut must stay silent: {:?}",
        outcome.report.warnings
    );
    let expected = SIZE.powi(3) - 10.0 * 10.0 * SIZE;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "a plain through cut must stay exact: {volume} vs {expected}"
    );
}

/// A blind cut that ends inside solid material past the void removes both the
/// near material and the slice between the tunnel floor and the stop depth.
#[test]
fn a_blind_cut_past_the_void_cuts_the_resumed_material() {
    let slotted = slotted_box();
    let top = face_where(&slotted, |centre| (centre.z - SIZE).abs() < 1.0e-6);
    // Stop 30 deep: through the roof at 18, across the tunnel to 22, and
    // 8 further into the material below.
    let outcome = rectangle_cut_outcome(
        &slotted,
        top,
        PlanarFrame3::new(
            Point3::new(20.0, 20.0, SIZE),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        Point2::new(-5.0, -5.0),
        Point2::new(5.0, 5.0),
        30.0,
        "slotted-cut-blind",
    );
    assert!(
        NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid,
        "the blind crossing cut must still publish a valid solid"
    );
    let tunnel = SIZE * 20.0 * 4.0;
    // 18 of pocket above the tunnel, 8 below it; the 4 in between were
    // already the tunnel's void.
    let expected = SIZE.powi(3) - tunnel - 10.0 * 10.0 * (18.0 + 8.0);
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-6,
        "the blind cut must resume past the slot: got {volume}, want {expected}"
    );
}

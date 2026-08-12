//! Profiles that cross the boundary of the face they were sketched on.
//!
//! Half a circle over the edge of a plate is an everyday boss or notch, not
//! an input error. The face-feature path cannot host it — the profile leaves
//! the face — so the kernel reformulates it exactly: the profile becomes a
//! prism tool, a cut becomes the boundary-crossing difference the pocket
//! engine already certifies, and an add becomes the tool prism glued onto
//! the top cap with the overlap open, the kept top facing up, and the
//! overhang's underside facing down.
//!
//! Expectations are closed forms derived in the test, not pinned digests.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const EPSILON: f64 = 1.0e-9;
const PI: f64 = std::f64::consts::PI;

/// A 4 × 3 × 1 plate with its top face at z = 1.
fn plate() -> Snapshot {
    let input = NativeKernel::empty();
    NativeKernel::execute(
        &input,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("overhang-plate"),
            expected_snapshot: input.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x: 4.0,
                size_y: 3.0,
                size_z: 1.0,
            },
        },
        &CancellationToken::new(),
    )
    .expect("plate should build")
    .snapshot
}

fn top_face(snapshot: &Snapshot) -> artificer_protocol::EntityRef {
    NativeKernel::debug_scene(snapshot)
        .triangles
        .iter()
        .find(|triangle| {
            triangle
                .vertices
                .iter()
                .all(|point| (point.z - 1.0).abs() <= 1.0e-9)
        })
        .expect("top cap")
        .source_face
}

/// A circle of radius 1 centred on the plate's x = 4 edge: half on, half off.
fn crossing_circle() -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(4.0, 1.5),
                    radius: 1.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    }
}

fn face_feature(
    base: &Snapshot,
    operation: FaceExtrusionOperation,
    distance: f64,
) -> Result<Snapshot, String> {
    let target_face = top_face(base);
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 1.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    NativeKernel::execute(
        base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("overhang-feature"),
            expected_snapshot: base.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face,
                frame,
                profile: crossing_circle(),
                distance,
                operation,
            },
        },
        &CancellationToken::new(),
    )
    .map(|outcome| outcome.snapshot)
    .map_err(|error| format!("{error:?}"))
}

#[test]
fn a_boundary_crossing_circle_cuts_a_notch() {
    let base = plate();
    let cut = face_feature(&base, FaceExtrusionOperation::Cut, 0.4)
        .expect("a crossing cut is an ordinary notch");
    assert!(NativeKernel::validate(&cut, ValidationProfile::Solid).valid);
    // Half the cylinder lies over the plate; the notch removes that half.
    let removed = PI * 1.0 * 1.0 * 0.4 / 2.0;
    let expected = 4.0 * 3.0 * 1.0 - removed;
    let volume = cut.measures().volume;
    assert!(
        (volume - expected).abs() <= EPSILON,
        "cut volume {volume} vs {expected}"
    );
}

#[test]
fn a_boundary_crossing_circle_adds_an_overhanging_boss() {
    let base = plate();
    let add = face_feature(&base, FaceExtrusionOperation::Add, 2.0)
        .expect("a crossing add is an overhanging boss");
    let report = NativeKernel::validate(&add, ValidationProfile::Solid);
    assert!(report.valid, "boss should validate: {report:?}");
    // The boss sits wholly above the plate, so the union volume is the sum.
    let expected = 4.0 * 3.0 * 1.0 + PI * 1.0 * 1.0 * 2.0;
    let volume = add.measures().volume;
    assert!(
        (volume - expected).abs() <= EPSILON,
        "add volume {volume} vs {expected}"
    );
    // The glue is real topology, not just a volume: the overhang keeps a
    // downward-facing underside at z = 1 and the plate keeps its top outside
    // the boss, while the overlap is open interior. Sample the tessellation
    // for both sides of the interface plane.
    let scene = NativeKernel::debug_scene(&add);
    let mut down_at_interface = false;
    let mut up_at_interface = false;
    for triangle in &scene.triangles {
        if !triangle
            .vertices
            .iter()
            .all(|point| (point.z - 1.0).abs() <= 1.0e-9)
        {
            continue;
        }
        let [a, b, c] = triangle.vertices;
        let ux = b.x - a.x;
        let uy = b.y - a.y;
        let vx = c.x - a.x;
        let vy = c.y - a.y;
        let cross = ux * vy - uy * vx;
        let centroid_x = (a.x + b.x + c.x) / 3.0;
        // Outside the plate (x > 4): the overhang underside, facing down.
        // Inside the plate but outside the circle: the kept top, facing up.
        if centroid_x > 4.0 + 1.0e-6 {
            if cross < 0.0 {
                down_at_interface = true;
            }
        } else if cross > 0.0 {
            up_at_interface = true;
        }
    }
    assert!(
        down_at_interface,
        "the overhang must keep a downward underside at the interface"
    );
    assert!(
        up_at_interface,
        "the plate must keep its top face outside the boss"
    );
}

#[test]
fn a_circle_covering_the_whole_face_becomes_a_wide_flange() {
    // The tool prism swallows the entire cap: no kept top remains, and the
    // overhang is the full annulus between the plate outline and the circle.
    // A wider flange on a narrower post is an everyday shape, so this must
    // build, and the union volume is simply the sum of the two prisms.
    let base = plate();
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 1.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(2.0, 1.5),
                    radius: 10.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let outcome = NativeKernel::execute(
        &base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("covering-flange"),
            expected_snapshot: base.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: top_face(&base),
                frame,
                profile,
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            },
        },
        &CancellationToken::new(),
    )
    .expect("a covering add is a wide flange");
    let report = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(report.valid, "flange should validate: {report:?}");
    let expected = 4.0 * 3.0 * 1.0 + PI * 10.0 * 10.0 * 1.0;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        (volume - expected).abs() <= EPSILON,
        "{volume} vs {expected}"
    );
}

#[test]
fn a_fully_interior_circle_still_uses_the_exact_face_path() {
    // The fallback must not have widened into the certified interior case.
    let base = plate();
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 1.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(2.0, 1.5),
                    radius: 0.5,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let outcome = NativeKernel::execute(
        &base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("interior-control"),
            expected_snapshot: base.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: top_face(&base),
                frame,
                profile,
                distance: 0.5,
                operation: FaceExtrusionOperation::Add,
            },
        },
        &CancellationToken::new(),
    )
    .expect("interior add stays certified");
    let expected = 12.0 + PI * 0.25 * 0.5;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        (volume - expected).abs() <= EPSILON,
        "{volume} vs {expected}"
    );
}

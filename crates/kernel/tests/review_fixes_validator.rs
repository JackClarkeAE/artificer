//! Regressions for the validator's per-face measures found in review.
//!
//! A body's surface area is the sum of its faces' areas, and a face's centre
//! lies on the face. Neither needs a closed form to check: the body's own
//! measure is integrated face class by face class along a separate route, so
//! the per-face route agreeing with it — to the last few digits — is the
//! test. The bodies are the ones whose faces are bounded by what the
//! per-face route used to misread: harmonics on a cylinder cut obliquely,
//! and the quartic trace where two bores of unequal radius cross.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

fn execute(snapshot: &Snapshot, command: KernelCommand, label: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    let outcome = NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"));
    assert!(
        outcome.report.warnings.is_empty(),
        "{label} is exact: {:?}",
        outcome.report.warnings
    );
    let validation = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{label}: {:?}", validation.diagnostics);
    outcome.snapshot
}

fn region(outer: PlanarLoop2) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer,
            holes: vec![],
        }],
    }
}

fn polygon(corners: &[(f64, f64)]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: (0..corners.len())
            .map(|index| {
                let (x, y) = corners[index];
                let (next_x, next_y) = corners[(index + 1) % corners.len()];
                PlanarCurve2::Line {
                    start: Point2::new(x, y),
                    end: Point2::new(next_x, next_y),
                }
            })
            .collect(),
    }
}

fn circle(radius: f64) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(0.0, 0.0),
            radius,
            direction: ArcDirection::CounterClockwise,
        }],
    }
}

fn face_where(snapshot: &Snapshot, pick: impl Fn(Point3) -> bool) -> EntityRef {
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

fn bore(
    body: &Snapshot,
    face: EntityRef,
    frame: PlanarFrame3,
    radius: f64,
    label: &str,
) -> Snapshot {
    execute(
        body,
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: face,
            frame,
            profile: region(circle(radius)),
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
        label,
    )
}

/// Every face's own area adds up to the body's surface area, and every
/// face's centre lies within the body's bounds.
fn assert_faces_agree_with_the_body(body: &Snapshot, what: &str) {
    let measures = body.measures();
    let faces = NativeKernel::faces(body);
    let total: f64 = faces
        .iter()
        .map(|face| {
            NativeKernel::face_area(body, *face)
                .unwrap_or_else(|error| panic!("{what}: a face area: {error:?}"))
        })
        .sum();
    assert!(
        ((total - measures.surface_area) / measures.surface_area).abs() < 1.0e-9,
        "{what}: the faces' areas add to {total}, the body's to {}",
        measures.surface_area
    );
    let bounds = measures.bounds.expect("a solid has bounds");
    let slack = 1.0e-6 * (1.0 + measures.surface_area.sqrt());
    let descriptions = NativeKernel::describe_faces(body);
    assert_eq!(
        descriptions.len(),
        faces.len(),
        "{what}: every face describes"
    );
    for description in descriptions.values() {
        let centre = description.centre;
        let inside = centre.x >= bounds.min.x - slack
            && centre.x <= bounds.max.x + slack
            && centre.y >= bounds.min.y - slack
            && centre.y <= bounds.max.y + slack
            && centre.z >= bounds.min.z - slack
            && centre.z <= bounds.max.z + slack;
        assert!(
            inside,
            "{what}: a {:?} face's centre {centre:?} lies outside the body {bounds:?}",
            description.geometry
        );
    }
}

/// A radius-5 cylinder 20 tall, mitred at 30° through its axis at height 12:
/// its wall is two faces, each bounded above by a harmonic.
#[test]
fn a_mitred_cylinders_faces_add_up_to_its_surface() {
    let cylinder = execute(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: region(circle(5.0)),
            distance: 20.0,
        },
        "cylinder",
    );
    let slant = 30.0_f64.to_radians();
    let tool = execute(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 12.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, slant.cos(), slant.sin()),
            ),
            profile: region(polygon(&[
                (-20.0, -20.0),
                (20.0, -20.0),
                (20.0, 20.0),
                (-20.0, 20.0),
            ])),
            distance: 30.0,
        },
        "mitre tool",
    );
    let request = artificer_protocol::BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("mitre"),
        expected_target_snapshot: cylinder.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation: artificer_protocol::BooleanOperation::Difference,
    };
    let mitred =
        NativeKernel::execute_boolean(&cylinder, &tool, &request, &CancellationToken::new())
            .expect("the mitre cuts")
            .snapshot;
    assert_faces_agree_with_the_body(&mitred, "mitred cylinder");
}

/// A radius-5 hole bored square into a 45° chamfer of a block: it leaves
/// the block through a face it meets at 45°, so its wall is bounded by a
/// harmonic at one end and a circle at the other.
#[test]
fn an_angled_holes_faces_add_up_to_the_blocks_surface() {
    let depth = 30.0;
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: region(polygon(&[
                (0.0, 0.0),
                (60.0, 0.0),
                (60.0, 25.0),
                (45.0, 40.0),
                (0.0, 40.0),
            ])),
            distance: depth,
        },
        "chamfered block",
    );
    let chamfer = face_where(&block, |centre| (centre.x + centre.y - 85.0).abs() < 1.0e-6);
    let root_half = 0.5_f64.sqrt();
    let bored = bore(
        &block,
        chamfer,
        PlanarFrame3::new(
            Point3::new(55.0, 30.0, depth / 2.0),
            Vector3::new(0.0, 0.0, 1.0),
            Vector3::new(root_half, -root_half, 0.0),
        ),
        5.0,
        "angled hole",
    );
    assert_faces_agree_with_the_body(&bored, "angled hole");
}

/// A 40 block drilled down through its top at Ø16 and across through its
/// side at Ø20: the two bores meet in a quartic, which bounds a face of
/// each.
#[test]
fn crossing_bores_of_unequal_radius_add_up_to_the_blocks_surface() {
    let size = 40.0;
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: size,
            size_y: size,
            size_z: size,
        },
        "block",
    );
    let top = face_where(&block, |centre| (centre.z - size).abs() < 1.0e-6);
    let drilled = bore(
        &block,
        top,
        PlanarFrame3::new(
            Point3::new(size / 2.0, size / 2.0, size),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        8.0,
        "top drill",
    );
    let side = face_where(&drilled, |centre| (centre.x - size).abs() < 1.0e-6);
    let crossed = bore(
        &drilled,
        side,
        PlanarFrame3::new(
            Point3::new(size, size / 2.0, size / 2.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        10.0,
        "side drill",
    );
    assert_faces_agree_with_the_body(&crossed, "crossing bores");
}

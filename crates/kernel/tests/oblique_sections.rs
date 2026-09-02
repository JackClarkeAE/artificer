//! Oblique plane sections of cylinders are exact ellipses.
//!
//! A plane through a cylinder at any attitude meets it in an ellipse: an
//! elliptical chord on the plane and a harmonic trace on the cylinder. Both
//! are in the vocabulary now, so a mitred pipe end and an angled hole are
//! exact bodies whose measures close in closed form.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EntityRef,
    ExecuteRequest, FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, ValidationProfile,
    Vector3,
};

fn execute(snapshot: &Snapshot, label: &str, command: KernelCommand) -> Snapshot {
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
    outcome.snapshot
}

fn polygon(points: &[(f64, f64)]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: (0..points.len())
            .map(|index| {
                let (x, y) = points[index];
                let (nx, ny) = points[(index + 1) % points.len()];
                PlanarCurve2::Line {
                    start: Point2::new(x, y),
                    end: Point2::new(nx, ny),
                }
            })
            .collect(),
    }
}

fn circle(center: Point2, radius: f64) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center,
            radius,
            direction: ArcDirection::CounterClockwise,
        }],
    }
}

fn extrude(
    snapshot: &Snapshot,
    label: &str,
    frame: PlanarFrame3,
    outer: PlanarLoop2,
    distance: f64,
) -> Snapshot {
    execute(
        snapshot,
        label,
        KernelCommand::ExtrudePlanarProfile {
            frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer,
                    holes: vec![],
                }],
            },
            distance,
        },
    )
}

fn subtract(target: &Snapshot, tool: &Snapshot, label: &str) -> artificer_kernel::ExecutionOutcome {
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation: BooleanOperation::Difference,
    };
    NativeKernel::execute_boolean(target, tool, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
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

const RADIUS: f64 = 5.0;
const HEIGHT: f64 = 20.0;
const MITRE_HEIGHT: f64 = 12.0;

/// A cylinder cut by a plane at 30° through its axis at `MITRE_HEIGHT`.
///
/// The cut face is an ellipse with the cylinder's radius across and the
/// radius over the cosine of the slant along; the volume left is the mean
/// height, which is the height at the axis, times the disc.
#[test]
fn an_oblique_plane_mitres_a_cylinder_end_exactly() {
    let cylinder = extrude(
        &NativeKernel::empty(),
        "cylinder",
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        circle(Point2::new(0.0, 0.0), RADIUS),
        HEIGHT,
    );
    let slant = 30.0_f64.to_radians();
    let tool = extrude(
        &NativeKernel::empty(),
        "mitre-tool",
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, MITRE_HEIGHT),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, slant.cos(), slant.sin()),
        ),
        polygon(&[(-20.0, -20.0), (20.0, -20.0), (20.0, 20.0), (-20.0, 20.0)]),
        30.0,
    );
    let outcome = subtract(&cylinder, &tool, "mitre");
    assert!(
        outcome.report.warnings.is_empty(),
        "an oblique section is exact: {:?}",
        outcome.report.warnings
    );
    let mitred = outcome.snapshot;
    assert!(NativeKernel::validate(&mitred, ValidationProfile::Solid).valid);

    let measures = mitred.measures();
    let expected_volume = PI * RADIUS * RADIUS * MITRE_HEIGHT;
    assert!(
        ((measures.volume - expected_volume) / expected_volume).abs() < 1.0e-9,
        "volume {} should be {expected_volume}",
        measures.volume
    );
    let expected_area = PI * RADIUS * RADIUS
        + 2.0 * PI * RADIUS * MITRE_HEIGHT
        + PI * RADIUS * (RADIUS / slant.cos());
    assert!(
        ((measures.surface_area - expected_area) / expected_area).abs() < 1.0e-9,
        "surface area {} should be {expected_area}",
        measures.surface_area
    );
    // Bottom cap, the wall's two halves, elliptical cap.
    assert_eq!(mitred.counts().faces, 4);
}

/// A hole bored square into a 45° chamfer exits through a face it meets at
/// 45°: a circle in, an ellipse out, and a cylinder between them whose
/// volume is the disc times the axis length between the two planes.
#[test]
fn an_angled_hole_through_a_box_exits_as_an_ellipse() {
    let depth = 30.0;
    let block = extrude(
        &NativeKernel::empty(),
        "chamfered-block",
        PlanarFrame3::new(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        polygon(&[
            (0.0, 0.0),
            (60.0, 0.0),
            (60.0, 25.0),
            (45.0, 40.0),
            (0.0, 40.0),
        ]),
        depth,
    );
    let block_volume = (60.0 * 40.0 - 15.0 * 15.0 / 2.0) * depth;
    assert!((block.measures().volume - block_volume).abs() < 1.0e-9);

    let chamfer = face_where(&block, |centre| (centre.x + centre.y - 85.0).abs() < 1.0e-6);
    let root_half = 0.5_f64.sqrt();
    // The frame winds with the chamfer's outward normal, `(1, 1, 0)/√2`,
    // as a face sketch does; the cut runs in from there.
    let frame = PlanarFrame3::new(
        Point3::new(55.0, 30.0, depth / 2.0),
        Vector3::new(0.0, 0.0, 1.0),
        Vector3::new(root_half, -root_half, 0.0),
    );
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("angled-hole"),
        expected_snapshot: block.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: chamfer,
            frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle(Point2::new(0.0, 0.0), RADIUS),
                    holes: vec![],
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    let outcome = NativeKernel::execute(&block, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("the angled hole should build: {error:?}"));
    assert!(
        outcome.report.warnings.is_empty(),
        "an angled hole is exact: {:?}",
        outcome.report.warnings
    );
    let bored = outcome.snapshot;
    assert!(NativeKernel::validate(&bored, ValidationProfile::Solid).valid);
    // The axis runs from (55, 30) on the chamfer to (25, 0) on the y = 0
    // face: 30√2 long, and the exit plane is linear so the mean height is
    // the height at the axis.
    let axis_length = 30.0 * 2.0_f64.sqrt();
    let expected = block_volume - PI * RADIUS * RADIUS * axis_length;
    let volume = bored.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
    assert!(bored.counts().faces >= 8);

    // The body now carries an elliptical loop and harmonic traces. A further
    // feature reads them back as exact chords: a pocket from the top that
    // reaches the exit face, clear of the ellipse and of the chamfer's
    // corner, comes off exactly too.
    let top = face_where(&bored, |centre| (centre.z - depth).abs() < 1.0e-6);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("pocket-after-hole"),
        expected_snapshot: bored.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, depth),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: polygon(&[(47.0, -5.0), (58.0, -5.0), (58.0, 8.0), (47.0, 8.0)]),
                    holes: vec![],
                }],
            },
            distance: 8.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    let outcome = NativeKernel::execute(&bored, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("the pocket should build: {error:?}"));
    assert!(
        outcome.report.warnings.is_empty(),
        "a pocket on a body with an oblique hole is exact: {:?}",
        outcome.report.warnings
    );
    let pocketed = outcome.snapshot;
    assert!(NativeKernel::validate(&pocketed, ValidationProfile::Solid).valid);
    let expected = expected - 11.0 * 8.0 * 8.0;
    let volume = pocketed.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
}

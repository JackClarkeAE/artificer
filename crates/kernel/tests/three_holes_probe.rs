//! A block extruded from a profile with three round holes: exact topology,
//! exact volume, and a display tessellation that covers both caps.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, FaceRole, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, ValidationProfile, Vector3,
};

const SIDE: f64 = 100.0;
const HEIGHT: f64 = 20.0;
const HOLE_RADIUS: f64 = 8.0;

#[test]
fn a_block_with_three_round_holes_is_exact_and_displays_both_caps() {
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let corners = [
        Point2::new(0.0, 0.0),
        Point2::new(SIDE, 0.0),
        Point2::new(SIDE, SIDE),
        Point2::new(0.0, SIDE),
    ];
    let outer_curves = (0..4)
        .map(|index| PlanarCurve2::Line {
            start: corners[index],
            end: corners[(index + 1) % 4],
        })
        .collect();
    let hole = |x: f64, y: f64| PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(x, y),
            radius: HOLE_RADIUS,
            direction: ArcDirection::CounterClockwise,
        }],
    };
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: outer_curves,
            },
            holes: vec![hole(30.0, 30.0), hole(70.0, 30.0), hole(50.0, 70.0)],
        }],
    };

    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("extrude-three-holes"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance: HEIGHT,
        },
    };
    let outcome =
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("extrude three holes should succeed");
    let snapshot = &outcome.snapshot;
    assert!(
        outcome.report.warnings.is_empty(),
        "{:?}",
        outcome.report.warnings
    );
    assert!(NativeKernel::validate(snapshot, ValidationProfile::Solid).valid);

    // Two caps, four walls, and two half-cylinder walls per hole.
    let counts = snapshot.counts();
    assert_eq!(counts.solids, 1);
    assert_eq!(counts.faces, 6 + 3 * 2);
    let expected = SIDE * SIDE * HEIGHT - 3.0 * PI * HOLE_RADIUS * HOLE_RADIUS * HEIGHT;
    let volume = snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );

    // Both caps tessellate with their holes bridged in, so the display
    // covers the cap area (minus the holes) to within the chord error.
    let scene = NativeKernel::debug_scene(snapshot);
    let cap_area = |role: FaceRole| {
        scene
            .triangles
            .iter()
            .filter(|triangle| triangle.role == role)
            .map(|triangle| {
                let [a, b, c] = triangle.vertices;
                let ab = (b.x - a.x, b.y - a.y, b.z - a.z);
                let ac = (c.x - a.x, c.y - a.y, c.z - a.z);
                let cross = (
                    ab.1 * ac.2 - ab.2 * ac.1,
                    ab.2 * ac.0 - ab.0 * ac.2,
                    ab.0 * ac.1 - ab.1 * ac.0,
                );
                0.5 * (cross.0 * cross.0 + cross.1 * cross.1 + cross.2 * cross.2).sqrt()
            })
            .sum::<f64>()
    };
    let expected_cap = SIDE * SIDE - 3.0 * PI * HOLE_RADIUS * HOLE_RADIUS;
    for role in [FaceRole::ExtrusionTop, FaceRole::ExtrusionBottom] {
        let area = cap_area(role);
        assert!(
            ((area - expected_cap) / expected_cap).abs() < 1.0e-2,
            "{role:?} tessellated area {area} should be near {expected_cap}"
        );
    }
}

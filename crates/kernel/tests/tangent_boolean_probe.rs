//! Where a tangential Boolean actually fails, as opposed to where it is said to.
//!
//! A fillet's removal solid touches the body along the two lines its band is
//! tangent to. The intersection matrix already answers a plane grazing a
//! cylinder with the single generator they share, so the refusal is somewhere
//! after that. This narrows it down.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};

const SIDE: f64 = 10.0;
const RADIUS: f64 = 2.0;

fn run(input: &Snapshot, command: KernelCommand, label: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label}: {error}"))
        .snapshot
}

fn cube() -> Snapshot {
    run(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIDE,
            size_y: SIDE,
            size_z: SIDE,
        },
        "cube",
    )
}

/// What a fillet along the z edge at the origin takes away: the curvilinear
/// triangle between the two walls and the arc, swept past both ends. The
/// straight sides reach outside the body so only the arc is in contact — and
/// the arc is tangent to each wall, which is the whole point.
fn fillet_tool(radius: f64) -> Snapshot {
    let reach: f64 = std::env::var("PROBE_REACH")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(SIDE * 4.0);
    let outer = PlanarLoop2 {
        curves: vec![
            PlanarCurve2::Line {
                start: Point2::new(-reach, -reach),
                end: Point2::new(radius, -reach),
            },
            PlanarCurve2::Line {
                start: Point2::new(radius, -reach),
                end: Point2::new(radius, 0.0),
            },
            PlanarCurve2::CircularArc {
                start: Point2::new(radius, 0.0),
                end: Point2::new(0.0, radius),
                center: Point2::new(radius, radius),
                direction: ArcDirection::Clockwise,
            },
            PlanarCurve2::Line {
                start: Point2::new(0.0, radius),
                end: Point2::new(-reach, radius),
            },
            PlanarCurve2::Line {
                start: Point2::new(-reach, radius),
                end: Point2::new(-reach, -reach),
            },
        ],
    };
    run(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, -reach),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer,
                    holes: vec![],
                }],
            },
            distance: std::env::var("PROBE_SWEEP")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(reach * 3.0),
        },
        "fillet tool",
    )
}

#[test]
fn where_a_tangential_difference_fails() {
    let cube = cube();
    let tool = fillet_tool(RADIUS);
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("tangent-difference"),
        expected_target_snapshot: cube.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation: BooleanOperation::Difference,
    };
    match NativeKernel::execute_boolean(&cube, &tool, &request, &CancellationToken::new()) {
        Ok(outcome) => {
            // A cube with one edge rounded: the square section less the corner
            // the quarter-disc leaves, swept the height.
            let expected =
                SIDE.powi(3) - RADIUS * RADIUS * (1.0 - std::f64::consts::PI / 4.0) * SIDE;
            println!(
                "TANGENT ACCEPTED volume {} against {expected}",
                outcome.snapshot.measures().volume
            );
        }
        Err(error) => println!("TANGENT REFUSED {error}"),
    }
}

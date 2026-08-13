// Each bench binary includes this module whole and uses part of it, so
// fixtures unused by one of them are expected rather than dead.
#![allow(dead_code)]

//! Fixtures shared by the kernel benches (ADR 0026, V1).
//!
//! Every fixture is built from the public protocol, so a bench measures the
//! same path a command from the workbench takes — no private entry points and
//! no pre-warmed internal state. The curve sweeps stop at 256 because that is
//! `MAX_EXTRUSION_PROFILE_VERTICES`: the largest profile the protocol accepts
//! is the largest one worth timing.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};

/// A regular polygon of `sides` vertices, inscribed in `radius`.
///
/// Curve count is the axis these benches sweep: profile Boolean and
/// tessellation are both dominated by it, and a regular polygon makes the
/// count exact rather than approximate.
#[must_use]
pub fn regular_polygon(center: (f64, f64), radius: f64, sides: usize) -> PlanarProfile2 {
    let vertices = (0..sides)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / sides as f64;
            Point2::new(
                radius.mul_add(angle.cos(), center.0),
                radius.mul_add(angle.sin(), center.1),
            )
        })
        .collect::<Vec<_>>();
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&vertices),
            holes: vec![],
        }],
    }
}

#[must_use]
pub fn disc(center: (f64, f64), radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(center.0, center.1),
                    radius,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    }
}

#[must_use]
pub fn rectangle(min: (f64, f64), max: (f64, f64)) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&[
                Point2::new(min.0, min.1),
                Point2::new(max.0, min.1),
                Point2::new(max.0, max.1),
                Point2::new(min.0, max.1),
            ]),
            holes: vec![],
        }],
    }
}

#[must_use]
pub fn extrude_request(profile: PlanarProfile2, origin: Point3, height: f64) -> ExecuteRequest {
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("bench-extrude"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                origin,
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: height,
        },
    }
}

#[must_use]
pub fn extrude(profile: PlanarProfile2, origin: Point3, height: f64) -> Snapshot {
    NativeKernel::execute(
        &NativeKernel::empty(),
        &extrude_request(profile, origin, height),
        &CancellationToken::new(),
    )
    .expect("the bench fixture should extrude")
    .snapshot
}

#[must_use]
pub fn boolean_request(
    target: &Snapshot,
    tool: &Snapshot,
    operation: BooleanOperation,
) -> BooleanRequest {
    BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("bench-boolean"),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation,
    }
}

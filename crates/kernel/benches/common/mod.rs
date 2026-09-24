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

use std::collections::BTreeMap;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};

/// A square plate with an `n × n` grid of drilled holes, built through the
/// scripting API (ADR 0056 R2's scale fixture): `6 + 2·n²` faces, so `n = 8`
/// is a hundred-face body and `n = 22` a thousand-face one. The holes lie on
/// a 10 mm pitch through a 5 mm plate.
#[must_use]
pub fn drilled_plate(n: usize) -> Snapshot {
    let side = 10.0 * n as f64;
    let mut script = format!(
        "let plate = box(origin: [{}, {}, 0], size: [{side}, {side}, 5], label: \"plate\");\nlet top = plate.face(\"top_face\");\n",
        -side / 2.0,
        -side / 2.0
    );
    for i in 0..n {
        for j in 0..n {
            let x = -side / 2.0 + 5.0 + 10.0 * i as f64;
            let y = -side / 2.0 + 5.0 + 10.0 * j as f64;
            script.push_str(&format!(
                "drill(face: top, center: [{x}, {y}], diameter: 4, depth: 5, label: \"h_{i}_{j}\");\n"
            ));
        }
    }
    let mut session = Session::new();
    let outcome = session.run_script(&script, &BTreeMap::new(), &CancellationToken::default());
    assert!(
        outcome.succeeded(),
        "the drilled-plate bench fixture builds"
    );
    session.snapshot
}

/// The second plate of a two-plate Boolean bench: `(n−1)²` holes on the
/// half-pitch grid, four millimetres narrower, lifted half a thickness so
/// the slabs differ and the general analytic engine runs.
#[must_use]
pub fn second_drilled_plate(n: usize) -> Snapshot {
    let side = 10.0 * n as f64 - 4.0;
    let mut script = format!(
        "let plate = box(origin: [{}, {}, 2.5], size: [{side}, {side}, 5], label: \"plate\");\nlet top = plate.face(\"top_face\");\n",
        -side / 2.0,
        -side / 2.0
    );
    for i in 0..n - 1 {
        for j in 0..n - 1 {
            let base = -(10.0 * n as f64) / 2.0 + 5.0;
            let x = base + 10.0 * i as f64 + 5.0;
            let y = base + 10.0 * j as f64 + 5.0;
            script.push_str(&format!(
                "drill(face: top, center: [{x}, {y}], diameter: 4, depth: 5, label: \"h_{i}_{j}\");\n"
            ));
        }
    }
    let mut session = Session::new();
    let outcome = session.run_script(&script, &BTreeMap::new(), &CancellationToken::default());
    assert!(
        outcome.succeeded(),
        "the second drilled-plate bench fixture builds"
    );
    session.snapshot
}

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

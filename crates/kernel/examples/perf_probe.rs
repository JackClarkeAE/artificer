//! Timing probe for the operations a user actually feels.
use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, ExecuteRequest, KernelCommand, PlanarFrame3, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};
use std::time::Instant;

fn time<T>(label: &str, runs: u32, mut work: impl FnMut() -> T) -> T {
    let mut result = None;
    let start = Instant::now();
    for _ in 0..runs {
        result = Some(work());
    }
    let total = start.elapsed();
    println!(
        "{label}: {:.3} ms/run",
        total.as_secs_f64() * 1000.0 / f64::from(runs)
    );
    result.unwrap()
}

fn main() {
    let empty = NativeKernel::empty();
    // A 50mm diameter x 100mm cylinder — the case that felt slow in the app.
    let base = time("extrude cylinder", 20, || {
        NativeKernel::execute(
            &empty,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("perf-cyl"),
                expected_snapshot: empty.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeRevolvedAnnulus {
                    frame: PlanarFrame3::new(
                        Point3::new(0.0, 0.0, 0.0),
                        Vector3::new(1.0, 0.0, 0.0),
                        Vector3::new(0.0, 1.0, 0.0),
                    ),
                    inner_radius: 0.0,
                    outer_radius: 25.0,
                    height: 100.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("cylinder")
        .snapshot
    });

    let rim = NativeKernel::debug_scene(&base)
        .edges
        .iter()
        .find(|edge| (edge.endpoints[0].z - 100.0).abs() < 1.0e-9)
        .map(|edge| edge.source_edge)
        .expect("rim");
    let filleted = time("rim fillet 10mm", 20, || {
        NativeKernel::execute(
            &base,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("perf-fillet"),
                expected_snapshot: base.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::FinishEdge {
                    target_edge: rim,
                    kind: EdgeFinishKind::Fillet,
                    distance: 10.0,
                },
            },
            &CancellationToken::new(),
        )
        .expect("fillet")
        .snapshot
    });

    time("validate filleted", 20, || {
        NativeKernel::validate(&filleted, ValidationProfile::Solid)
    });
    time("debug_scene filleted", 20, || {
        NativeKernel::debug_scene(&filleted)
    });
    time("authoritative_scene filleted", 5, || {
        NativeKernel::authoritative_scene(&filleted)
    });
}

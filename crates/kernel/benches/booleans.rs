//! The Boolean ladder's rungs, measured separately (ADR 0026, V1).
//!
//! `prism_*` cases stay inside the co-directional prism reduction; the
//! `analytic_*` case crosses axes so it falls through to the general
//! imprint/classify/regularize/sew engine. Timing them apart is the point: a
//! regression in one rung is invisible in a number that averages both.

mod common;

use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{BooleanOperation, PlanarFrame3, Point3, Vector3};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn booleans(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("boolean");
    for sides in [4_usize, 64, 256] {
        let target = common::extrude(
            common::regular_polygon((0.0, 0.0), 25.0, sides),
            Point3::new(0.0, 0.0, 0.0),
            10.0,
        );
        let tool = common::extrude(
            common::regular_polygon((12.0, 0.0), 15.0, sides),
            Point3::new(0.0, 0.0, 2.0),
            6.0,
        );
        for (label, operation) in [
            ("prism_union", BooleanOperation::Union),
            ("prism_difference", BooleanOperation::Difference),
        ] {
            let request = common::boolean_request(&target, &tool, operation);
            group.bench_with_input(
                BenchmarkId::new(label, sides),
                &request,
                |bencher, request| {
                    bencher.iter(|| {
                        black_box(NativeKernel::execute_boolean(
                            &target,
                            &tool,
                            request,
                            &CancellationToken::new(),
                        ))
                    });
                },
            );
        }
    }

    // Crossed axes: the general engine, not the prism reduction.
    let target = common::extrude(
        common::rectangle((-20.0, -8.0), (20.0, 8.0)),
        Point3::new(0.0, 0.0, 0.0),
        16.0,
    );
    let tool = {
        let request = artificer_protocol::ExecuteRequest {
            protocol_version: artificer_protocol::CURRENT_PROTOCOL_VERSION,
            request_id: artificer_protocol::RequestId::new("bench-crossed-tool"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: artificer_protocol::PrecisionPolicy::default(),
            command: artificer_protocol::KernelCommand::ExtrudePlanarProfile {
                frame: PlanarFrame3::new(
                    Point3::new(0.0, -30.0, 8.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 0.0, 1.0),
                ),
                profile: common::rectangle((-6.0, -4.0), (6.0, 4.0)),
                distance: 60.0,
            },
        };
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("the crossed bench tool should extrude")
            .snapshot
    };
    let request = common::boolean_request(&target, &tool, BooleanOperation::Difference);
    group.bench_function("analytic_crossed_difference", |bencher| {
        bencher.iter(|| {
            black_box(NativeKernel::execute_boolean(
                &target,
                &tool,
                &request,
                &CancellationToken::new(),
            ))
        });
    });
    group.finish();
}

criterion_group!(benches, booleans);
criterion_main!(benches);

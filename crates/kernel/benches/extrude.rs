//! Analytic extrusion against profile complexity (ADR 0026, V1).

mod common;

use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::Point3;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn extrude(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("extrude");
    for sides in [4_usize, 64, 256] {
        let request = common::extrude_request(
            common::regular_polygon((0.0, 0.0), 25.0, sides),
            Point3::new(0.0, 0.0, 0.0),
            10.0,
        );
        group.bench_with_input(
            BenchmarkId::new("polygon_sides", sides),
            &request,
            |bencher, request| {
                bencher.iter(|| {
                    black_box(
                        NativeKernel::execute(
                            &NativeKernel::empty(),
                            request,
                            &CancellationToken::new(),
                        )
                        .expect("the bench profile should extrude"),
                    )
                });
            },
        );
    }
    let circle = common::extrude_request(
        common::disc((0.0, 0.0), 25.0),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
    );
    group.bench_function("analytic_circle", |bencher| {
        bencher.iter(|| {
            black_box(
                NativeKernel::execute(&NativeKernel::empty(), &circle, &CancellationToken::new())
                    .expect("the bench circle should extrude"),
            )
        });
    });
    group.finish();
}

criterion_group!(benches, extrude);
criterion_main!(benches);

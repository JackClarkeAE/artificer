//! The solid validator, which runs on every published snapshot (ADR 0026, V1).

mod common;

use artificer_kernel::NativeKernel;
use artificer_protocol::{Point3, ValidationProfile};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn validate(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("validate");
    for sides in [4_usize, 64, 256] {
        let snapshot = common::extrude(
            common::regular_polygon((0.0, 0.0), 25.0, sides),
            Point3::new(0.0, 0.0, 0.0),
            10.0,
        );
        group.bench_with_input(
            BenchmarkId::new("solid_polygon_sides", sides),
            &snapshot,
            |bencher, snapshot| {
                bencher
                    .iter(|| black_box(NativeKernel::validate(snapshot, ValidationProfile::Solid)));
            },
        );
    }
    // Drilled plates over 10² and 10³ faces (ADR 0056 R2): the validator's
    // families and the divergence-theorem measures over a large body.
    for n in [8_usize, 22] {
        let snapshot = common::drilled_plate(n);
        group.bench_with_input(
            BenchmarkId::new("drilled_plate_faces", snapshot.counts().faces),
            &snapshot,
            |bencher, snapshot| {
                bencher
                    .iter(|| black_box(NativeKernel::validate(snapshot, ValidationProfile::Solid)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, validate);
criterion_main!(benches);

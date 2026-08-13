//! Display and authoritative tessellation, measured apart (ADR 0026, V1).
//!
//! The two budgets differ by orders of magnitude in chord count, and only the
//! display one runs per frame. Benchmarking them together would hide a
//! regression in the one the frame budget actually pays for.

mod common;

use artificer_kernel::NativeKernel;
use artificer_protocol::Point3;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn tessellate(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("tessellate");
    let turned = common::extrude(
        common::disc((0.0, 0.0), 25.0),
        Point3::new(0.0, 0.0, 0.0),
        100.0,
    );
    group.bench_function("display_cylinder", |bencher| {
        bencher.iter(|| black_box(NativeKernel::debug_scene(&turned)));
    });
    group.bench_function("display_cylinder_scaled_quarter", |bencher| {
        bencher.iter(|| black_box(NativeKernel::display_scene_scaled(&turned, 0.25)));
    });
    group.bench_function("authoritative_cylinder", |bencher| {
        bencher.iter(|| black_box(NativeKernel::authoritative_scene(&turned)));
    });

    let prism = common::extrude(
        common::regular_polygon((0.0, 0.0), 25.0, 256),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
    );
    group.bench_function("display_prism_256", |bencher| {
        bencher.iter(|| black_box(NativeKernel::debug_scene(&prism)));
    });
    group.finish();
}

criterion_group!(benches, tessellate);
criterion_main!(benches);

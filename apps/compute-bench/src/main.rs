use std::hint::black_box;
use std::time::{Duration, Instant};

use artificer_compute::{ComputeConfig, ComputePool};
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, Point3, PrecisionPolicy, RequestId,
    ValidationProfile,
};
use artificer_sketch::{
    ArrangementInputCurve, ArrangementLimits, SketchEntityId, SketchPoint2, SketchPointId,
    build_arrangement_with_pool,
};

const SAMPLE_RUNS: usize = 5;

struct ResultRow {
    name: &'static str,
    work: &'static str,
    serial: Duration,
    parallel: Duration,
}

impl ResultRow {
    fn ratio(&self) -> f64 {
        self.serial.as_secs_f64() / self.parallel.as_secs_f64()
    }

    fn percent(&self) -> f64 {
        (1.0 - self.parallel.as_secs_f64() / self.serial.as_secs_f64()) * 100.0
    }
}

fn main() {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let serial = ComputePool::new(ComputeConfig::serial()).expect("serial pool");
    let parallel = ComputePool::new(ComputeConfig {
        threads: available,
        parallel_min_items: 32,
    })
    .expect("parallel pool");

    let snapshots = benchmark_snapshots(4_096);
    let curves = dense_grid(224);
    let projection_points = (0..3_000_000_u64)
        .map(|index| {
            let value = index as f64 * 0.000_031_25;
            [
                value.sin() * 100.0,
                value.cos() * 80.0,
                (value * 0.37).sin() * 60.0,
            ]
        })
        .collect::<Vec<_>>();

    let rows = vec![
        compare(
            "Dense sketch arrangement",
            "224 curves / 12,544 crossings",
            || {
                build_arrangement_with_pool(
                    &serial,
                    &curves,
                    &PrecisionPolicy::default(),
                    ArrangementLimits::default(),
                )
                .fragments
                .len()
            },
            || {
                build_arrangement_with_pool(
                    &parallel,
                    &curves,
                    &PrecisionPolicy::default(),
                    ArrangementLimits::default(),
                )
                .fragments
                .len()
            },
        ),
        compare(
            "B-rep validation batch",
            "4,096 immutable bodies",
            || {
                serial
                    .map("bench.validation", &snapshots, |_, snapshot| {
                        NativeKernel::validate_with_pool(
                            &serial,
                            snapshot,
                            ValidationProfile::Solid,
                        )
                        .diagnostics
                        .len()
                    })
                    .into_iter()
                    .sum::<usize>()
            },
            || {
                parallel
                    .map("bench.validation", &snapshots, |_, snapshot| {
                        NativeKernel::validate_with_pool(
                            &parallel,
                            snapshot,
                            ValidationProfile::Solid,
                        )
                        .diagnostics
                        .len()
                    })
                    .into_iter()
                    .sum::<usize>()
            },
        ),
        compare(
            "Display tessellation batch",
            "4,096 immutable bodies",
            || {
                serial
                    .map("bench.tessellation", &snapshots, |_, snapshot| {
                        NativeKernel::debug_scene_with_pool(&serial, snapshot)
                            .triangles
                            .len()
                    })
                    .into_iter()
                    .sum::<usize>()
            },
            || {
                parallel
                    .map("bench.tessellation", &snapshots, |_, snapshot| {
                        NativeKernel::debug_scene_with_pool(&parallel, snapshot)
                            .triangles
                            .len()
                    })
                    .into_iter()
                    .sum::<usize>()
            },
        ),
        compare(
            "Viewport projection math",
            "3,000,000 vertices",
            || projection_checksum(&serial, &projection_points),
            || projection_checksum(&parallel, &projection_points),
        ),
    ];

    println!("Artificer deterministic compute benchmark");
    println!("logical threads: serial=1 parallel={available}; samples={SAMPLE_RUNS} (median)");
    println!("| Heavy task | Workload | 1 thread | {available} threads | Speed-up | Time saved |");
    println!("|---|---:|---:|---:|---:|---:|");
    for row in rows {
        println!(
            "| {} | {} | {:.3} ms | {:.3} ms | {:.2}x | {:+.1}% |",
            row.name,
            row.work,
            row.serial.as_secs_f64() * 1_000.0,
            row.parallel.as_secs_f64() * 1_000.0,
            row.ratio(),
            row.percent(),
        );
    }
}

fn compare<S, P, T>(
    name: &'static str,
    work: &'static str,
    mut serial: S,
    mut parallel: P,
) -> ResultRow
where
    S: FnMut() -> T,
    P: FnMut() -> T,
{
    black_box(serial());
    black_box(parallel());
    ResultRow {
        name,
        work,
        serial: median(|| {
            black_box(serial());
        }),
        parallel: median(|| {
            black_box(parallel());
        }),
    }
}

fn median(mut operation: impl FnMut()) -> Duration {
    let mut samples = (0..SAMPLE_RUNS)
        .map(|_| {
            let started = Instant::now();
            operation();
            started.elapsed()
        })
        .collect::<Vec<_>>();
    samples.sort_unstable();
    samples[SAMPLE_RUNS / 2]
}

fn benchmark_snapshots(count: usize) -> Vec<Snapshot> {
    let empty = NativeKernel::empty();
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("parallel-benchmark-cuboid"),
        expected_snapshot: empty.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 20.0,
            size_y: 30.0,
            size_z: 40.0,
        },
    };
    let body = NativeKernel::execute(&empty, &request, &CancellationToken::new())
        .expect("benchmark body")
        .snapshot;
    vec![body; count]
}

fn dense_grid(curve_count: usize) -> Vec<ArrangementInputCurve> {
    let per_axis = curve_count / 2;
    let mut curves = Vec::with_capacity(per_axis * 2);
    let mut next_point = 1_u64;
    for index in 0..per_axis {
        let coordinate = index as f64 - per_axis as f64 / 2.0;
        curves.push(line(
            curves.len() as u64 + 1,
            &mut next_point,
            SketchPoint2::new(-100.0, coordinate),
            SketchPoint2::new(100.0, coordinate),
        ));
        curves.push(line(
            curves.len() as u64 + 1,
            &mut next_point,
            SketchPoint2::new(coordinate, -100.0),
            SketchPoint2::new(coordinate, 100.0),
        ));
    }
    curves
}

fn line(
    entity: u64,
    next_point: &mut u64,
    start: SketchPoint2,
    end: SketchPoint2,
) -> ArrangementInputCurve {
    let start_id = SketchPointId::new(*next_point).expect("point id");
    *next_point += 1;
    let end_id = SketchPointId::new(*next_point).expect("point id");
    *next_point += 1;
    ArrangementInputCurve::line(
        SketchEntityId::new(entity).expect("entity id"),
        start_id,
        end_id,
        start,
        end,
    )
}

fn projection_checksum(pool: &ComputePool, points: &[[f64; 3]]) -> u64 {
    pool.map("bench.viewport", points, |index, point| {
        let x = point[0].mul_add(0.866_025_403_784, point[2] * 0.5);
        let y = point[1].mul_add(
            std::f64::consts::FRAC_1_SQRT_2,
            point[2] * -std::f64::consts::FRAC_1_SQRT_2,
        );
        let depth = point[0].mul_add(-0.353_553_390_593, point[1] * 0.612_372_435_696)
            + point[2] * std::f64::consts::FRAC_1_SQRT_2;
        (x.mul_add(1_000.0, y * 10.0) + depth + index as f64 * f64::EPSILON).to_bits()
    })
    .into_iter()
    .fold(0_u64, u64::wrapping_add)
}

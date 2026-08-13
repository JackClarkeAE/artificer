//! What a hovered face costs on a body that has fallen back to faceting.
//!
//! Two crossing round cuts take a 40 mm cuboid from 6 faces to nearly three
//! thousand, and the viewport asks for the hovered face's tangent group on
//! every frame. This bench is that question, on that body.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

fn cuboid(size: f64) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("bench-cuboid"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: size,
            size_y: size,
            size_z: size,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("the bench cuboid should build")
        .snapshot
}

fn face_where(snapshot: &Snapshot, pick: fn(Point3) -> bool) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    for triangle in &scene.triangles {
        let [a, b, c] = triangle.vertices;
        let centre = Point3::new(
            (a.x + b.x + c.x) / 3.0,
            (a.y + b.y + c.y) / 3.0,
            (a.z + b.z + c.z) / 3.0,
        );
        if pick(centre) {
            return triangle.source_face;
        }
    }
    panic!("the bench fixture should expose the requested face");
}

fn through_cut(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    radius: f64,
    label: &str,
) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(0.0, 0.0),
                            radius,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: vec![],
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .expect("the bench cut should build")
        .snapshot
}

fn hovered_face(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("tangent_face_group");

    let plain = cuboid(40.0);
    let plain_scene = NativeKernel::debug_scene(&plain);
    let plain_seed = plain_scene.triangles[0].source_face;
    group.bench_function("cuboid", |bencher| {
        bencher.iter(|| {
            black_box(artificer_viewport::tangent_face_group(
                &plain_scene,
                plain_seed,
            ))
        });
    });

    let top = face_where(&plain, |centre| (centre.z - 40.0).abs() < 1.0e-6);
    let once = through_cut(
        &plain,
        top,
        PlanarFrame3::new(
            Point3::new(20.0, 20.0, 40.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ),
        8.0,
        "bench-cut-1",
    );
    let side = face_where(&once, |centre| (centre.x - 40.0).abs() < 1.0e-6);
    let crossed = through_cut(
        &once,
        side,
        PlanarFrame3::new(
            Point3::new(40.0, 20.0, 20.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        ),
        8.0,
        "bench-cut-2",
    );
    let crossed_scene = NativeKernel::debug_scene(&crossed);
    let crossed_seed = crossed_scene.triangles[0].source_face;
    eprintln!(
        "crossed-cut fixture: {} faces, {} scene edges, {} triangles",
        crossed.counts().faces,
        crossed_scene.edges.len(),
        crossed_scene.triangles.len()
    );
    group.bench_function("two_crossing_cuts", |bencher| {
        bencher.iter(|| {
            black_box(artificer_viewport::tangent_face_group(
                &crossed_scene,
                crossed_seed,
            ))
        });
    });
    group.finish();
}

criterion_group!(benches, hovered_face);
criterion_main!(benches);

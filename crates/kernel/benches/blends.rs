//! Rim fillets and chamfers against rim complexity (ADR 0026, V1).

mod common;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, KernelCommand, Point3,
    PrecisionPolicy, RequestId,
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

const HEIGHT: f64 = 10.0;

/// Every edge lying in the top cap plane — the rim loop.
fn top_rim(snapshot: &Snapshot) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut rim = Vec::new();
    for edge in &scene.edges {
        let [first, second] = edge.endpoints;
        if (first.z - HEIGHT).abs() < 1.0e-9
            && (second.z - HEIGHT).abs() < 1.0e-9
            && !rim.contains(&edge.source_edge)
        {
            rim.push(edge.source_edge);
        }
    }
    rim
}

fn finish_request(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
) -> ExecuteRequest {
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("bench-rim-finish"),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: targets,
            kind,
            distance: 0.75,
        },
    }
}

fn blends(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("rim_blend");
    for sides in [4_usize, 16, 64] {
        let base = common::extrude(
            common::regular_polygon((0.0, 0.0), 25.0, sides),
            Point3::new(0.0, 0.0, 0.0),
            HEIGHT,
        );
        let rim = top_rim(&base);
        assert_eq!(rim.len(), sides, "the rim loop should be the whole cap");
        for (label, kind) in [
            ("fillet", EdgeFinishKind::Fillet),
            ("chamfer", EdgeFinishKind::Chamfer),
        ] {
            let request = finish_request(&base, rim.clone(), kind);
            group.bench_with_input(
                BenchmarkId::new(label, sides),
                &request,
                |bencher, request| {
                    bencher.iter(|| {
                        black_box(NativeKernel::execute(
                            &base,
                            request,
                            &CancellationToken::new(),
                        ))
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, blends);
criterion_main!(benches);

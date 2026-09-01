//! A fillet's transition rails are tangent: real, selectable topology that
//! the presentation must not draw as creases. A chamfer's rails are creases.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, KernelCommand, Point3,
    PrecisionPolicy, RequestId,
};

const SIZE: f64 = 20.0;

fn execute(snapshot: &Snapshot, label: &str, command: KernelCommand) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
        .snapshot
}

fn block() -> Snapshot {
    execute(
        &NativeKernel::empty(),
        "block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
    )
}

/// The top edge along +X at y = 0.
fn front_top_edge(snapshot: &Snapshot) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .edges
        .iter()
        .find(|edge| {
            edge.endpoints
                .iter()
                .all(|point| (point.z - SIZE).abs() < 1.0e-9 && point.y.abs() < 1.0e-9)
        })
        .expect("the block exposes its front top edge")
        .source_edge
}

fn finish(snapshot: &Snapshot, kind: EdgeFinishKind) -> Snapshot {
    execute(
        snapshot,
        "finish",
        KernelCommand::FinishEdges {
            target_edges: vec![front_top_edge(snapshot)],
            kind,
            distance: 3.0,
        },
    )
}

#[test]
fn a_fillet_presents_its_two_rails_as_tangent_and_its_end_arcs_as_creases() {
    let filleted = finish(&block(), EdgeFinishKind::Fillet);
    let scene = NativeKernel::debug_scene(&filleted);
    let tangent = scene
        .edges
        .iter()
        .filter(|edge| edge.is_tangent)
        .collect::<Vec<_>>();
    assert!(!tangent.is_empty(), "the fillet rails must be tangent");
    // Both rails are straight lines along X, one on the top cap and one on
    // the front wall; every tangent chord lies on one of them.
    for edge in &tangent {
        assert!(!edge.is_smooth, "a tangent rail is still real topology");
        let [a, b] = edge.endpoints;
        assert!((a.y - b.y).abs() < 1.0e-9 && (a.z - b.z).abs() < 1.0e-9);
        let on_top = (a.z - SIZE).abs() < 1.0e-9 && (a.y - 3.0).abs() < 1.0e-9;
        let on_front = a.y.abs() < 1.0e-9 && (a.z - (SIZE - 3.0)).abs() < 1.0e-9;
        assert!(on_top || on_front, "unexpected tangent chord at {a:?}");
    }
    let rail_sources = tangent
        .iter()
        .map(|edge| edge.source_edge)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(rail_sources.len(), 2, "one rail per side of the fillet");
    // The quarter-circle ends of the fillet are creases.
    let end_arcs = scene
        .edges
        .iter()
        .filter(|edge| {
            !edge.is_smooth
                && !edge.is_tangent
                && edge.endpoints.iter().all(|point| point.x.abs() < 1.0e-9)
                && edge
                    .endpoints
                    .iter()
                    .any(|point| point.y > 1.0e-6 && point.y < 3.0 - 1.0e-6)
        })
        .count();
    assert!(end_arcs > 0, "the fillet end arcs stay visible");
}

#[test]
fn a_chamfer_keeps_both_rails_as_creases() {
    let chamfered = finish(&block(), EdgeFinishKind::Chamfer);
    let scene = NativeKernel::debug_scene(&chamfered);
    assert!(
        scene.edges.iter().all(|edge| !edge.is_tangent),
        "a chamfer meets its neighbours at 45 degrees; nothing is tangent"
    );
}

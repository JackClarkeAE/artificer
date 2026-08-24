//! The rails where chamfer slants meet each other.
//!
//! Three chamfers meeting at a cube corner leave three mitres between them.
//! Each is a real rail: a visible line, and a selectable target a later finish
//! can be stacked on. They are published by the same presentation rule that
//! hides a curved surface's parameterization seams, and two 45-degree slants
//! meet at exactly 60 degrees — which is where that rule's fan allowance used
//! to sit, so whether a mitre survived was decided by rounding.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, KernelCommand, Point3,
    PrecisionPolicy, RequestId,
};

const SIZE: f64 = 20.0;
const DISTANCE: f64 = 3.0;

fn cuboid() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("corner-cuboid"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("the cuboid should build")
        .snapshot
}

/// The three edges meeting at the far top corner.
fn corner_edges(snapshot: &Snapshot) -> Vec<EntityRef> {
    let corner = Point3::new(SIZE, SIZE, SIZE);
    let at_corner = |point: &artificer_protocol::Point3| {
        (point.x - corner.x).abs() < 1.0e-9
            && (point.y - corner.y).abs() < 1.0e-9
            && (point.z - corner.z).abs() < 1.0e-9
    };
    let scene = NativeKernel::debug_scene(snapshot);
    let mut found = Vec::new();
    for edge in &scene.edges {
        if edge.endpoints.iter().any(at_corner) && !found.contains(&edge.source_edge) {
            found.push(edge.source_edge);
        }
    }
    assert_eq!(found.len(), 3, "a cube corner joins exactly three edges");
    found
}

#[test]
fn every_mitre_between_three_corner_chamfers_presents_as_a_rail() {
    let cuboid = cuboid();
    let targets = corner_edges(&cuboid);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("corner-chamfers"),
        expected_snapshot: cuboid.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: targets,
            kind: EdgeFinishKind::Chamfer,
            distance: DISTANCE,
        },
    };
    let chamfered = NativeKernel::execute(&cuboid, &request, &CancellationToken::new())
        .expect("three corner edges should chamfer as one feature")
        .snapshot;

    // A slant is the only face of this body whose normal is off-axis.
    let scene = NativeKernel::debug_scene(&chamfered);
    let mut slants = Vec::new();
    for triangle in &scene.triangles {
        let [first, second, third] = triangle.vertices;
        let edge_a = [
            second.x - first.x,
            second.y - first.y,
            second.z - first.z,
        ];
        let edge_b = [third.x - first.x, third.y - first.y, third.z - first.z];
        let normal = [
            edge_a[1].mul_add(edge_b[2], -(edge_a[2] * edge_b[1])),
            edge_a[2].mul_add(edge_b[0], -(edge_a[0] * edge_b[2])),
            edge_a[0].mul_add(edge_b[1], -(edge_a[1] * edge_b[0])),
        ];
        let length = normal[0]
            .mul_add(normal[0], normal[1].mul_add(normal[1], normal[2] * normal[2]))
            .sqrt();
        if length <= 1.0e-12 {
            continue;
        }
        let axis_aligned = normal
            .iter()
            .any(|component| (component.abs() / length) > 1.0 - 1.0e-6);
        if !axis_aligned && !slants.contains(&triangle.source_face) {
            slants.push(triangle.source_face);
        }
    }
    assert_eq!(
        slants.len(),
        3,
        "three chamfered edges should leave three slant faces, found {slants:?}"
    );

    // Each pair of slants meets along one mitre, and every one of them must
    // present as a hard edge. Hiding a mitre is what made a corner look like
    // it had lost a line, and it would also take the rail out of reach of any
    // later finish that wanted to stack on it.
    for (index, first) in slants.iter().enumerate() {
        for second in slants.iter().skip(index + 1) {
            let shared = scene
                .edges
                .iter()
                .filter(|edge| {
                    let faces = edge.incident_faces.iter().flatten().collect::<Vec<_>>();
                    faces.contains(&first) && faces.contains(&second)
                })
                .collect::<Vec<_>>();
            assert!(
                !shared.is_empty(),
                "slants {first:?} and {second:?} should meet along a mitre"
            );
            assert!(
                shared.iter().all(|edge| !edge.is_smooth),
                "the mitre between slants {first:?} and {second:?} should draw as a rail"
            );
        }
    }
}

//! A finish on two edges meeting at a corner that is not square.
//!
//! A ridge is the everyday case: the roof of a prism bends, so at each gable
//! the two slope edges meet at the apex with the ridge crease as the third
//! edge. No face there is square to another. The owned corner blend derives
//! its seam for three mutually square faces and refused this; the answer
//! that is general is the one ADR 0044 already made for a finish standing
//! apart — each edge's removal cut from the body as it is, the two bands
//! meeting along whatever seam the general engine finds — and the ladder
//! now takes it before the faceted tier.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const WIDTH: f64 = 20.0;
const EAVE: f64 = 10.0;
const APEX: f64 = 16.0;
const LENGTH: f64 = 30.0;

fn run(input: &Snapshot, command: KernelCommand, label: &str) -> Result<Snapshot, String> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
        .map_err(|error| format!("{error:?}"))
}

/// A house-shaped prism: a rectangle with a peaked roof, extruded along z.
fn house() -> Snapshot {
    let corners = [
        (0.0, 0.0),
        (WIDTH, 0.0),
        (WIDTH, EAVE),
        (WIDTH / 2.0, APEX),
        (0.0, EAVE),
    ];
    let curves = (0..corners.len())
        .map(|index| {
            let (from, to) = (corners[index], corners[(index + 1) % corners.len()]);
            PlanarCurve2::Line {
                start: Point2::new(from.0, from.1),
                end: Point2::new(to.0, to.1),
            }
        })
        .collect();
    run(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 { curves },
                    holes: vec![],
                }],
            },
            distance: LENGTH,
        },
        "house",
    )
    .expect("a house-shaped prism")
}

fn edge_between(snapshot: &Snapshot, a: Point3, b: Point3) -> EntityRef {
    let near = |p: Point3, q: Point3| {
        (p.x - q.x).abs() < 1.0e-9 && (p.y - q.y).abs() < 1.0e-9 && (p.z - q.z).abs() < 1.0e-9
    };
    NativeKernel::debug_scene(snapshot)
        .edges
        .iter()
        .filter(|edge| !edge.is_smooth)
        .find(|edge| {
            let [p, q] = edge.endpoints;
            (near(p, a) && near(q, b)) || (near(p, b) && near(q, a))
        })
        .map(|edge| edge.source_edge)
        .unwrap_or_else(|| panic!("an edge from {a:?} to {b:?}"))
}

fn tessellated_volume(snapshot: &Snapshot) -> f64 {
    NativeKernel::debug_scene(snapshot)
        .triangles
        .iter()
        .map(|triangle| {
            let [a, b, c] = triangle.vertices;
            (a.x * (b.y * c.z - b.z * c.y) - a.y * (b.x * c.z - b.z * c.x)
                + a.z * (b.x * c.y - b.y * c.x))
                / 6.0
        })
        .sum()
}

/// The two slope edges of one gable, finished together, with the ridge left
/// sharp: a two-of-three corner whose faces lean.
fn finish_gable_slopes(kind: EdgeFinishKind, distance: f64) {
    let house = house();
    let before = house.measures().volume;
    let apex = Point3::new(WIDTH / 2.0, APEX, 0.0);
    let left = edge_between(&house, Point3::new(0.0, EAVE, 0.0), apex);
    let right = edge_between(&house, Point3::new(WIDTH, EAVE, 0.0), apex);
    let finished = run(
        &house,
        KernelCommand::FinishEdges {
            target_edges: vec![left, right],
            kind,
            distance,
            standing_apart: false,
        },
        "gable finish",
    )
    .unwrap_or_else(|error| panic!("a leaning corner is the everyday one: {error}"));
    let validation = NativeKernel::validate(&finished, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
    let volume = finished.measures().volume;
    assert!(
        volume < before,
        "the finish removes material: {volume} vs {before}"
    );
    let tessellated = tessellated_volume(&finished);
    assert!(
        ((volume - tessellated) / tessellated).abs() < 1.0e-3,
        "the exact volume {volume} agrees with the tessellated {tessellated}"
    );
}

#[test]
fn a_chamfer_across_a_leaning_gable_corner_is_built() {
    finish_gable_slopes(EdgeFinishKind::Chamfer, 2.0);
}

#[test]
fn a_fillet_across_a_leaning_gable_corner_is_built() {
    finish_gable_slopes(EdgeFinishKind::Fillet, 2.0);
}

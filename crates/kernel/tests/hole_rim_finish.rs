//! A fillet or chamfer around the rim of a hole through a side wall.
//!
//! The rim of a drilled hole is the edge a user rounds most, and until now
//! only a hole through a prism *cap* had an exact route. A hole through a
//! side wall — any face the body is not a prism about — fell through every
//! exact rung to the faceted tier, which could not weld the torus. The band
//! is now built in place: a quarter torus (or a cone) between the grown hole
//! on the wall and the sunk ring on the bore, one face per rim arc.
//!
//! Both volumes are closed forms by Pappus: the removed ring is the corner
//! region — a square of side `d` less its inscribed quarter disc for a
//! fillet, a right triangle of leg `d` for a chamfer — swept round the axis
//! at the radius of its own centroid.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIZE: f64 = 40.0;
const RADIUS: f64 = 8.0;
const FINISH: f64 = 2.0;

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
    panic!("the fixture should expose the requested face");
}

/// A cube, extruded along z so its `x = SIZE` face is a side wall, bored
/// through that wall along x.
fn bored_block() -> Snapshot {
    let square = [(0.0, 0.0), (SIZE, 0.0), (SIZE, SIZE), (0.0, SIZE)];
    let curves = (0..4)
        .map(|index| PlanarCurve2::Line {
            start: Point2::new(square[index].0, square[index].1),
            end: Point2::new(square[(index + 1) % 4].0, square[(index + 1) % 4].1),
        })
        .collect();
    let block = run(
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
            distance: SIZE,
        },
        "block",
    )
    .expect("a block");
    let side = face_where(&block, |centre| (centre.x - SIZE).abs() < 1.0e-6);
    run(
        &block,
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: side,
            frame: PlanarFrame3::new(
                Point3::new(SIZE, SIZE / 2.0, SIZE / 2.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(0.0, 0.0),
                            radius: RADIUS,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: vec![],
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
        "side bore",
    )
    .expect("a bore through the side wall")
}

/// Every edge of the hole's rim on the `x = SIZE` wall.
fn rim_edges(snapshot: &Snapshot) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut rim: Vec<EntityRef> = Vec::new();
    for edge in &scene.edges {
        let [a, b] = edge.endpoints;
        let on_wall = (a.x - SIZE).abs() < 1.0e-6 && (b.x - SIZE).abs() < 1.0e-6;
        let on_circle =
            |p: Point3| ((p.y - SIZE / 2.0).hypot(p.z - SIZE / 2.0) - RADIUS).abs() < 1.0e-6;
        if on_wall && on_circle(a) && on_circle(b) && !rim.contains(&edge.source_edge) {
            rim.push(edge.source_edge);
        }
    }
    assert!(!rim.is_empty(), "the rim is on the wall");
    rim
}

fn finish_rim(kind: EdgeFinishKind) -> (f64, f64) {
    let bored = bored_block();
    let before = bored.measures().volume;
    let finished = run(
        &bored,
        KernelCommand::FinishEdges {
            target_edges: rim_edges(&bored),
            kind,
            distance: FINISH,
            standing_apart: false,
        },
        "rim finish",
    )
    .unwrap_or_else(|error| panic!("a hole rim through a wall is the everyday finish: {error}"));
    let validation = NativeKernel::validate(&finished, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
    (before, finished.measures().volume)
}

#[test]
fn a_fillet_round_a_hole_through_a_side_wall_removes_exactly_its_ring() {
    let (before, after) = finish_rim(EdgeFinishKind::Fillet);
    // The corner region less its quarter disc, at the radius of its centroid.
    let area = FINISH * FINISH * (1.0 - PI / 4.0);
    let centroid = FINISH * (5.0 / 6.0 - PI / 4.0) / (1.0 - PI / 4.0);
    let ring = 2.0 * PI * (RADIUS + centroid) * area;
    let expected = before - ring;
    assert!(
        ((after - expected) / expected).abs() < 1.0e-9,
        "exactly the ring: {after} vs {expected}"
    );
}

#[test]
fn a_chamfer_round_a_hole_through_a_side_wall_removes_exactly_its_ring() {
    let (before, after) = finish_rim(EdgeFinishKind::Chamfer);
    // A right triangle of leg `d`, at the radius of its centroid.
    let ring = PI * FINISH * FINISH * (RADIUS + FINISH / 3.0);
    let expected = before - ring;
    assert!(
        ((after - expected) / expected).abs() < 1.0e-9,
        "exactly the ring: {after} vs {expected}"
    );
}

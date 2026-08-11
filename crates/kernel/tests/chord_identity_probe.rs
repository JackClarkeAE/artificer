//! A face's tessellated boundary must be built from the very same points as
//! the edge tessellation's chords.
//!
//! The viewport matches chords to triangle edges by exact identity to decide
//! which are front-facing and which a face may occlude. Nothing in that
//! contract is tolerant, so a boundary vertex that drifted by a unit in the
//! last place would silently unmatch its chord and the edge would simply not
//! be drawn — a circular rim would come out dashed.
//!
//! The two paths compute their samples separately: `sampled_edge_segments`
//! walks an edge forward, while `sampled_loop_polygon` walks a reverse-wound
//! coedge by flipping the interval. They agree today, including on the
//! reverse-wound inner loop of a pierced plate, but nothing in the types says
//! they must. This pins the agreement so a future change to either sampler
//! cannot quietly break the identity the viewport depends on.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, Vector3,
};

fn extrude(profile: PlanarProfile2, distance: f64, tag: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(tag),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{tag} should build: {error:?}"))
        .snapshot
}

fn circle(radius: f64, direction: ArcDirection) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(0.0, 0.0),
            radius,
            direction,
        }],
    }
}

fn bits(point: Point3) -> [u64; 3] {
    // Normalize only signed zero, exactly as the display identity does.
    let canonical = |value: f64| if value == 0.0 { 0.0_f64 } else { value };
    [
        canonical(point.x).to_bits(),
        canonical(point.y).to_bits(),
        canonical(point.z).to_bits(),
    ]
}

/// Every chord endpoint of every curved edge must appear, bit for bit, among
/// the tessellation's triangle vertices.
fn assert_chords_are_triangle_vertices(snapshot: &Snapshot, label: &str) {
    // The display budget is the one that matters: it is what the viewport
    // draws, and its coarser sampling is where the two paths disagreed.
    for scale in [1.0, 4.0, 12.0] {
        check_scene(
            &NativeKernel::display_scene_scaled(snapshot, scale),
            &format!("{label} @ display x{scale}"),
        );
    }
    check_scene(&NativeKernel::authoritative_scene(snapshot), label);
}

fn check_scene(scene: &artificer_kernel::DebugScene, label: &str) {
    assert!(!scene.triangles.is_empty(), "{label} should tessellate");
    let vertices: std::collections::HashSet<[u64; 3]> = scene
        .triangles
        .iter()
        .flat_map(|triangle| triangle.vertices.into_iter().map(bits))
        .collect();

    // Pairs, not points. Checking only that each endpoint is *a* vertex
    // somewhere is far too weak: a cap's boundary can drift while the wall
    // still supplies the same endpoints, so the chord stops being an edge of
    // the cap without any endpoint going missing. That is exactly the defect
    // that painted rims as dashed arcs.
    let mut edges: std::collections::HashSet<[[u64; 3]; 2]> = std::collections::HashSet::new();
    for triangle in &scene.triangles {
        let [a, b, c] = triangle.vertices;
        for pair in [[a, b], [b, c], [c, a]] {
            let mut key = pair.map(bits);
            key.sort_unstable();
            edges.insert(key);
        }
    }

    let mut orphaned = 0_usize;
    let mut total = 0_usize;
    for edge in scene.edges.iter().filter(|edge| !edge.is_smooth) {
        total += 1;
        let mut key = edge.endpoints.map(bits);
        key.sort_unstable();
        if !edges.contains(&key) {
            orphaned += 1;
        }
        for endpoint in edge.endpoints {
            assert!(
                vertices.contains(&bits(endpoint)),
                "{label}: a chord endpoint is not a triangle vertex"
            );
        }
    }
    assert!(total > 0, "{label} should publish model edges");
    assert_eq!(
        orphaned, 0,
        "{label}: {orphaned} of {total} chords are not triangle edges"
    );
}

#[test]
fn a_cylinder_rim_agrees_with_its_cap_triangulation() {
    // Both caps matter: one loop winds forward, the other reverse, and only
    // the reverse-wound one exercised the drifting arithmetic.
    let solid = extrude(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: circle(25.0, ArcDirection::CounterClockwise),
                holes: vec![],
            }],
        },
        100.0,
        "chord-cylinder",
    );
    assert_chords_are_triangle_vertices(&solid, "cylinder");
}

#[test]
fn a_hole_rim_agrees_with_its_annular_triangulation() {
    // Inner loops are reverse-wound by construction, so a pierced plate is
    // the worst case for boundary/chord agreement.
    let solid = extrude(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![
                        PlanarCurve2::Line {
                            start: Point2::new(-20.0, -20.0),
                            end: Point2::new(20.0, -20.0),
                        },
                        PlanarCurve2::Line {
                            start: Point2::new(20.0, -20.0),
                            end: Point2::new(20.0, 20.0),
                        },
                        PlanarCurve2::Line {
                            start: Point2::new(20.0, 20.0),
                            end: Point2::new(-20.0, 20.0),
                        },
                        PlanarCurve2::Line {
                            start: Point2::new(-20.0, 20.0),
                            end: Point2::new(-20.0, -20.0),
                        },
                    ],
                },
                holes: vec![circle(8.0, ArcDirection::Clockwise)],
            }],
        },
        20.0,
        "chord-plate",
    );
    assert_chords_are_triangle_vertices(&solid, "pierced plate");
}

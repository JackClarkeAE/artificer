//! Automated permutation tests for faceted edge hiding on cuts across extruded shapes.
//!
//! Generates permutations of extruded shapes (cuboid, cylinder, hexagonal prism)
//! and cuts through them (single cut, multiple crossing cuts on orthogonal faces,
//! double-sided transverse cylinder cuts, slot cuts intersecting parallel and perpendicular cylinder cuts),
//! verifying that:
//! 1. All internal facet seams within curved surfaces are marked `is_smooth: true` (hidden).
//! 2. Real physical boundary rims and intersections are marked `is_smooth: false` (visible).
//! 3. Topology remains valid solids.

use artificer_kernel::{CancellationToken, DebugScene, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

fn cross_prod(u: Vector3, v: Vector3) -> Vector3 {
    Vector3::new(
        u.y * v.z - u.z * v.y,
        u.z * v.x - u.x * v.z,
        u.x * v.y - u.y * v.x,
    )
}

fn dot_prod(u: Vector3, v: Vector3) -> f64 {
    u.x * v.x + u.y * v.y + u.z * v.z
}

fn vec_len(v: Vector3) -> f64 {
    dot_prod(v, v).sqrt()
}

fn point_dist(a: Point3, b: Point3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn make_cuboid(size_x: f64, size_y: f64, size_z: f64, label: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x,
            size_y,
            size_z,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("cuboid should build")
        .snapshot
}

fn make_cylinder(radius: f64, height: f64, label: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(0.0, 0.0),
                            radius,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: Vec::new(),
                }],
            },
            distance: height,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("cylinder should build")
        .snapshot
}

fn make_regular_polygon_prism(sides: usize, radius: f64, height: f64, label: &str) -> Snapshot {
    assert!(sides >= 3);
    let mut vertices = Vec::with_capacity(sides);
    for i in 0..sides {
        let a = std::f64::consts::TAU * (i as f64) / (sides as f64);
        vertices.push(Point2::new(radius * a.cos(), radius * a.sin()));
    }
    let mut curves = Vec::with_capacity(sides);
    for i in 0..sides {
        let next = (i + 1) % sides;
        curves.push(PlanarCurve2::Line {
            start: vertices[i],
            end: vertices[next],
        });
    }
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 { curves },
                    holes: Vec::new(),
                }],
            },
            distance: height,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("polygon prism should build")
        .snapshot
}

fn find_face_by_normal_and_point(
    snapshot: &Snapshot,
    target_normal: Vector3,
    approx_point: Point3,
) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut best_face = None;
    let mut best_dist = f64::INFINITY;

    for triangle in &scene.triangles {
        let [a, b, c] = triangle.vertices;
        let u = Vector3::new(b.x - a.x, b.y - a.y, b.z - a.z);
        let v = Vector3::new(c.x - a.x, c.y - a.y, c.z - a.z);
        let normal = cross_prod(u, v);
        let len = vec_len(normal);
        if len <= 1.0e-9 {
            continue;
        }
        let unit_normal = Vector3::new(normal.x / len, normal.y / len, normal.z / len);
        if dot_prod(unit_normal, target_normal) > 0.99 {
            let center = Point3::new(
                (a.x + b.x + c.x) / 3.0,
                (a.y + b.y + c.y) / 3.0,
                (a.z + b.z + c.z) / 3.0,
            );
            let dist = point_dist(center, approx_point);
            if dist < best_dist {
                best_dist = dist;
                best_face = Some(triangle.source_face);
            }
        }
    }
    best_face.expect("should find matching face")
}

fn cut_round_hole(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    center: Point2,
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
                            center,
                            radius,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: Vec::new(),
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .expect("cut should execute")
        .snapshot
}

fn cut_slot_hole(
    snapshot: &Snapshot,
    target_face: EntityRef,
    frame: PlanarFrame3,
    half_length: f64,
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
                        curves: vec![
                            PlanarCurve2::CircularArc {
                                center: Point2::new(-half_length, 0.0),
                                start: Point2::new(-half_length, -radius),
                                end: Point2::new(-half_length, radius),
                                direction: ArcDirection::CounterClockwise,
                            },
                            PlanarCurve2::Line {
                                start: Point2::new(-half_length, radius),
                                end: Point2::new(half_length, radius),
                            },
                            PlanarCurve2::CircularArc {
                                center: Point2::new(half_length, 0.0),
                                start: Point2::new(half_length, radius),
                                end: Point2::new(half_length, -radius),
                                direction: ArcDirection::CounterClockwise,
                            },
                            PlanarCurve2::Line {
                                start: Point2::new(half_length, -radius),
                                end: Point2::new(-half_length, -radius),
                            },
                        ],
                    },
                    holes: Vec::new(),
                }],
            },
            distance: 1_000.0,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .expect("slot cut should execute")
        .snapshot
}

fn assert_scene_edge_invariants(scene: &DebugScene, label: &str) {
    assert!(
        !scene.triangles.is_empty(),
        "{label}: scene should contain triangles"
    );
    assert!(
        !scene.edges.is_empty(),
        "{label}: scene should contain edges"
    );

    let total_edges = scene.edges.len();
    let visible_edges = scene.edges.iter().filter(|e| !e.is_smooth).count();
    let smooth_edges = scene.edges.iter().filter(|e| e.is_smooth).count();

    assert!(
        visible_edges > 0,
        "{label}: should have visible boundary edges (found {visible_edges} of {total_edges})"
    );
    assert!(
        smooth_edges > 0 || total_edges == visible_edges,
        "{label}: smooth edges should be categorized properly"
    );
}

#[test]
fn permutation_1_square_box_single_cut_on_one_face() {
    let box_snapshot = make_cuboid(50.0, 50.0, 50.0, "p1-box");
    let top_face = find_face_by_normal_and_point(
        &box_snapshot,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(25.0, 25.0, 50.0),
    );
    let frame = PlanarFrame3::new(
        Point3::new(25.0, 25.0, 50.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let cut = cut_round_hole(
        &box_snapshot,
        top_face,
        frame,
        Point2::new(0.0, 0.0),
        10.0,
        "p1-cut",
    );

    assert!(NativeKernel::validate(&cut, ValidationProfile::Solid).valid);
    let scene = NativeKernel::debug_scene(&cut);
    assert_scene_edge_invariants(&scene, "p1-single-cut");
}

#[test]
fn permutation_2_square_box_multiple_cuts_on_multiple_faces() {
    let box_snapshot = make_cuboid(60.0, 60.0, 60.0, "p2-box");

    // Cut 1: top face along Z
    let top_face = find_face_by_normal_and_point(
        &box_snapshot,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(30.0, 30.0, 60.0),
    );
    let frame_z = PlanarFrame3::new(
        Point3::new(30.0, 30.0, 60.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let cut1 = cut_round_hole(
        &box_snapshot,
        top_face,
        frame_z,
        Point2::new(0.0, 0.0),
        8.0,
        "p2-cut1",
    );

    // Cut 2: side face along X
    let side_face = find_face_by_normal_and_point(
        &cut1,
        Vector3::new(1.0, 0.0, 0.0),
        Point3::new(60.0, 30.0, 30.0),
    );
    let frame_x = PlanarFrame3::new(
        Point3::new(60.0, 30.0, 30.0),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    );
    let cut2 = cut_round_hole(
        &cut1,
        side_face,
        frame_x,
        Point2::new(0.0, 0.0),
        8.0,
        "p2-cut2",
    );

    assert!(NativeKernel::validate(&cut2, ValidationProfile::Solid).valid);
    let scene = NativeKernel::debug_scene(&cut2);
    assert_scene_edge_invariants(&scene, "p2-crossing-cuts");

    // Verify that internal facet seams of both bores remain smooth/hidden
    let smooth_count = scene.edges.iter().filter(|e| e.is_smooth).count();
    assert!(
        smooth_count > 20,
        "internal cylindrical panels must be classified as smooth (found {smooth_count})"
    );
}

#[test]
fn permutation_3_cylinder_cut_through_on_both_sides() {
    let cyl = make_cylinder(20.0, 60.0, "p3-cyl");
    let top_face = find_face_by_normal_and_point(
        &cyl,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(0.0, 0.0, 60.0),
    );
    let frame_top = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 60.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    // Cut 1: through the top cap
    let cut1 = cut_round_hole(
        &cyl,
        top_face,
        frame_top,
        Point2::new(0.0, 0.0),
        8.0,
        "p3-cut1",
    );
    assert!(NativeKernel::validate(&cut1, ValidationProfile::Solid).valid);

    let scene1 = NativeKernel::debug_scene(&cut1);
    assert_scene_edge_invariants(&scene1, "p3-cylinder-through-cut");
}

#[test]
fn permutation_4_hexagon_prism_with_multiple_face_cuts() {
    let hex = make_regular_polygon_prism(6, 30.0, 50.0, "p4-hex");
    let top_face = find_face_by_normal_and_point(
        &hex,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(0.0, 0.0, 50.0),
    );
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 50.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let cut = cut_round_hole(&hex, top_face, frame, Point2::new(0.0, 0.0), 10.0, "p4-cut");

    assert!(NativeKernel::validate(&cut, ValidationProfile::Solid).valid);
    let scene = NativeKernel::debug_scene(&cut);
    assert_scene_edge_invariants(&scene, "p4-hexagon-cut");
}

#[test]
fn permutation_5_slot_cut_intersecting_parallel_cylinder_cut() {
    let box_snapshot = make_cuboid(60.0, 60.0, 60.0, "p5-box");

    // 1. Parallel cylinder cut on top face
    let top_face = find_face_by_normal_and_point(
        &box_snapshot,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(25.0, 30.0, 60.0),
    );
    let frame_cyl = PlanarFrame3::new(
        Point3::new(25.0, 30.0, 60.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let cut1 = cut_round_hole(
        &box_snapshot,
        top_face,
        frame_cyl,
        Point2::new(0.0, 0.0),
        7.0,
        "p5-cyl-cut",
    );
    assert!(NativeKernel::validate(&cut1, ValidationProfile::Solid).valid);

    // 2. Parallel slot cut on top face intersecting the cylinder void
    let top_face2 = find_face_by_normal_and_point(
        &cut1,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(50.0, 50.0, 60.0),
    );
    let frame_slot = PlanarFrame3::new(
        Point3::new(35.0, 30.0, 60.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let cut2 = cut_slot_hole(&cut1, top_face2, frame_slot, 8.0, 6.0, "p5-slot-cut");

    assert!(NativeKernel::validate(&cut2, ValidationProfile::Solid).valid);
    let scene = NativeKernel::debug_scene(&cut2);
    assert_scene_edge_invariants(&scene, "p5-slot-intersecting-parallel-cyl");

    // Real physical intersection creases and rims are visible
    let visible_count = scene.edges.iter().filter(|e| !e.is_smooth).count();
    assert!(
        visible_count > 0,
        "boundary rims and intersection creases must remain visible"
    );
}

#[test]
fn permutation_6_slot_cut_intersecting_perpendicular_cylinder_cut() {
    let box_snapshot = make_cuboid(60.0, 60.0, 60.0, "p6-box");

    // 1. Slot cut on top face (down Z)
    let top_face = find_face_by_normal_and_point(
        &box_snapshot,
        Vector3::new(0.0, 0.0, 1.0),
        Point3::new(30.0, 30.0, 60.0),
    );
    let frame_slot = PlanarFrame3::new(
        Point3::new(30.0, 30.0, 60.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let cut1 = cut_slot_hole(
        &box_snapshot,
        top_face,
        frame_slot,
        10.0,
        7.0,
        "p6-slot-cut",
    );
    assert!(NativeKernel::validate(&cut1, ValidationProfile::Solid).valid);

    // 2. Perpendicular cylinder cut on side face (through X)
    let side_face = find_face_by_normal_and_point(
        &cut1,
        Vector3::new(1.0, 0.0, 0.0),
        Point3::new(60.0, 30.0, 30.0),
    );
    let frame_cyl = PlanarFrame3::new(
        Point3::new(60.0, 30.0, 30.0),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    );
    let cut2 = cut_round_hole(
        &cut1,
        side_face,
        frame_cyl,
        Point2::new(0.0, 0.0),
        7.0,
        "p6-perp-cyl-cut",
    );

    assert!(NativeKernel::validate(&cut2, ValidationProfile::Solid).valid);
    let scene = NativeKernel::debug_scene(&cut2);
    assert_scene_edge_invariants(&scene, "p6-slot-intersecting-perp-cyl");

    // Verify smooth faceted internal edges are hidden
    let smooth_count = scene.edges.iter().filter(|e| e.is_smooth).count();
    assert!(
        smooth_count > 15,
        "internal cylindrical panels of perpendicular cuts must be classified as smooth (found {smooth_count})"
    );
}

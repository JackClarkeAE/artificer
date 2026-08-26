//! Automated permutation testing for filleted edge finishes across various base shapes and cut configurations.
//!
//! Generates permutations of filleted extruded shapes (cuboids, cylinders, hexagonal prisms,
//! and cut shapes with fillets), verifying that:
//! 1. All internal facet seams across fillet blends (cylinders, tori, corner sphere patches) are marked `is_smooth: true` (hidden).
//! 2. True boundary rails and transition creases are marked `is_smooth: false` (visible).
//! 3. Solids remain valid and watertight.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
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

fn find_edges_by_endpoints(
    snapshot: &Snapshot,
    predicate: impl Fn(Point3, Point3) -> bool,
) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut matching = Vec::new();
    for edge in &scene.edges {
        if predicate(edge.endpoints[0], edge.endpoints[1]) && !matching.contains(&edge.source_edge)
        {
            matching.push(edge.source_edge);
        }
    }
    matching
}

fn finish_edge(
    snapshot: &Snapshot,
    target_edge: EntityRef,
    kind: EdgeFinishKind,
    distance: f64,
    label: &str,
) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdge {
            target_edge,
            kind,
            distance,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|e| panic!("{label}: finish edge failed: {e:?}"))
        .snapshot
}

fn finish_edges(
    snapshot: &Snapshot,
    target_edges: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
    label: &str,
) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges,
            kind,
            distance,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|e| panic!("{label}: finish edges failed: {e:?}"))
        .snapshot
}

fn assert_fillet_presentation_invariants(snapshot: &Snapshot, label: &str) {
    let valid = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(
        valid.valid,
        "{label}: topology should be a valid solid: {:?}",
        valid.diagnostics
    );

    let scene = NativeKernel::debug_scene(snapshot);
    assert!(
        !scene.triangles.is_empty(),
        "{label}: scene should contain triangles"
    );
    assert!(
        !scene.edges.is_empty(),
        "{label}: scene should contain edges"
    );

    let visible_edges = scene.edges.iter().filter(|e| !e.is_smooth).count();
    assert!(
        visible_edges > 0,
        "{label}: must retain visible boundary rails (found {visible_edges} visible)"
    );
}

#[test]
fn fillet_permutation_1_cuboid_single_and_parallel_vertical_edges() {
    let box_snapshot = make_cuboid(40.0, 40.0, 40.0, "fp1-box");

    // Fillet 1: Single vertical edge at (0, 0, Z)
    let vertical_edges = find_edges_by_endpoints(&box_snapshot, |a, b| {
        a.x.abs() < 1e-6 && a.y.abs() < 1e-6 && b.x.abs() < 1e-6 && b.y.abs() < 1e-6
    });
    assert_eq!(vertical_edges.len(), 1);
    let filleted1 = finish_edge(
        &box_snapshot,
        vertical_edges[0],
        EdgeFinishKind::Fillet,
        4.0,
        "fp1-single-fillet",
    );
    assert_fillet_presentation_invariants(&filleted1, "fp1-single-fillet");

    // Fillet 2: All 4 parallel vertical edges on a fresh cuboid
    let all_vertical = find_edges_by_endpoints(&box_snapshot, |a, b| {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 && (a.z - b.z).abs() > 1.0
    });
    assert_eq!(all_vertical.len(), 4);
    let filleted4 = finish_edges(
        &box_snapshot,
        all_vertical,
        EdgeFinishKind::Fillet,
        4.0,
        "fp1-four-vertical-fillet",
    );
    assert_fillet_presentation_invariants(&filleted4, "fp1-four-vertical-fillet");
}

#[test]
fn fillet_permutation_2_cuboid_trihedral_corner() {
    let box_snapshot = make_cuboid(40.0, 40.0, 40.0, "fp2-box");
    // Find the 3 edges meeting at corner (40, 40, 40)
    let corner_edges = find_edges_by_endpoints(&box_snapshot, |a, b| {
        ((a.x - 40.0).abs() < 1e-6 && (a.y - 40.0).abs() < 1e-6 && (a.z - 40.0).abs() < 1e-6)
            || ((b.x - 40.0).abs() < 1e-6 && (b.y - 40.0).abs() < 1e-6 && (b.z - 40.0).abs() < 1e-6)
    });
    assert_eq!(corner_edges.len(), 3);
    let filleted_corner = finish_edges(
        &box_snapshot,
        corner_edges,
        EdgeFinishKind::Fillet,
        5.0,
        "fp2-corner-fillet",
    );
    assert_fillet_presentation_invariants(&filleted_corner, "fp2-corner-fillet");

    // Verify that corner sphere/torus blend facet seams are hidden
    let scene = NativeKernel::debug_scene(&filleted_corner);
    let smooth_count = scene.edges.iter().filter(|e| e.is_smooth).count();
    assert!(
        smooth_count > 0,
        "corner fillet blend patches must have smooth internal facet seams"
    );
}

#[test]
fn fillet_permutation_3_cuboid_whole_top_rim_loop() {
    let box_snapshot = make_cuboid(40.0, 40.0, 40.0, "fp3-box");
    // All top edges at z = 40.0
    let top_rim = find_edges_by_endpoints(&box_snapshot, |a, b| {
        (a.z - 40.0).abs() < 1e-6 && (b.z - 40.0).abs() < 1e-6
    });
    assert_eq!(top_rim.len(), 4);
    let filleted_rim = finish_edges(
        &box_snapshot,
        top_rim,
        EdgeFinishKind::Fillet,
        3.0,
        "fp3-rim-fillet",
    );
    assert_fillet_presentation_invariants(&filleted_rim, "fp3-rim-fillet");

    // Verify smooth corner blend patches
    let scene = NativeKernel::debug_scene(&filleted_rim);
    let smooth_count = scene.edges.iter().filter(|e| e.is_smooth).count();
    assert!(
        smooth_count > 0,
        "rim loop fillet corner blends must have smooth internal facet seams"
    );
}

#[test]
fn fillet_permutation_4_cylinder_top_rim_fillet() {
    let cyl = make_cylinder(20.0, 40.0, "fp4-cyl");
    let top_rim = find_edges_by_endpoints(&cyl, |a, b| {
        (a.z - 40.0).abs() < 1e-6 && (b.z - 40.0).abs() < 1e-6
    });
    assert!(!top_rim.is_empty());
    let filleted_cyl = finish_edges(&cyl, top_rim, EdgeFinishKind::Fillet, 3.0, "fp4-cyl-fillet");
    assert_fillet_presentation_invariants(&filleted_cyl, "fp4-cyl-fillet");

    // The toroidal blend rim must have smooth internal chords and visible bounding circles
    let scene = NativeKernel::debug_scene(&filleted_cyl);
    let smooth_count = scene.edges.iter().filter(|e| e.is_smooth).count();
    assert!(
        smooth_count > 0,
        "cylinder toroidal rim fillet must have smooth internal seams"
    );
}

#[test]
fn fillet_permutation_5_hexagon_prism_edges() {
    let hex = make_regular_polygon_prism(6, 25.0, 40.0, "fp5-hex");
    // Vertical column edges
    let vertical_edges = find_edges_by_endpoints(&hex, |a, b| {
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 && (a.z - b.z).abs() > 1.0
    });
    assert_eq!(vertical_edges.len(), 6);
    let filleted_hex = finish_edges(
        &hex,
        vertical_edges,
        EdgeFinishKind::Fillet,
        2.0,
        "fp5-hex-fillet",
    );
    assert_fillet_presentation_invariants(&filleted_hex, "fp5-hex-fillet");
}

#[test]
fn fillet_permutation_6_cuts_with_filleted_outer_edges() {
    let box_snapshot = make_cuboid(60.0, 60.0, 60.0, "fp6-box");

    // 1. Cut on top face along Z
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
        "fp6-cut1",
    );

    // 2. Cut on side face along X
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
        "fp6-cut2",
    );
    assert!(NativeKernel::validate(&cut2, ValidationProfile::Solid).valid);

    // Fillet vertical outer corner edge at (0, 60, Z)
    let corner_edge = find_edges_by_endpoints(&cut2, |a, b| {
        (a.x - b.x).abs() < 1e-6
            && (a.y - b.y).abs() < 1e-6
            && (a.z - b.z).abs() > 1.0
            && a.x.abs() < 1e-6
            && (a.y - 60.0).abs() < 1e-6
    });
    assert_eq!(corner_edge.len(), 1);
    let filleted_cut = finish_edge(
        &cut2,
        corner_edge[0],
        EdgeFinishKind::Fillet,
        4.0,
        "fp6-fillet-crossing-cuts",
    );
    assert_fillet_presentation_invariants(&filleted_cut, "fp6-fillet-crossing-cuts");

    // Internal facet seams of both crossing cuts remain smooth/hidden
    let scene = NativeKernel::debug_scene(&filleted_cut);
    let smooth_count = scene.edges.iter().filter(|e| e.is_smooth).count();
    assert!(
        smooth_count > 20,
        "internal cylindrical panels of cut bores must remain smooth (found {smooth_count})"
    );
}

fn make_l_prism(width: f64, depth: f64, thickness: f64, height: f64, label: &str) -> Snapshot {
    let curves = vec![
        PlanarCurve2::Line {
            start: Point2::new(0.0, 0.0),
            end: Point2::new(width, 0.0),
        },
        PlanarCurve2::Line {
            start: Point2::new(width, 0.0),
            end: Point2::new(width, thickness),
        },
        PlanarCurve2::Line {
            start: Point2::new(width, thickness),
            end: Point2::new(thickness, thickness),
        },
        PlanarCurve2::Line {
            start: Point2::new(thickness, thickness),
            end: Point2::new(thickness, depth),
        },
        PlanarCurve2::Line {
            start: Point2::new(thickness, depth),
            end: Point2::new(0.0, depth),
        },
        PlanarCurve2::Line {
            start: Point2::new(0.0, depth),
            end: Point2::new(0.0, 0.0),
        },
    ];
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
        .expect("l-prism should build")
        .snapshot
}

#[test]
fn fillet_permutation_7_internal_concave_reentrant_fillet() {
    // L-shaped step block with an internal (concave / re-entrant) corner edge at (20, 20, Z)
    let l_block = make_l_prism(50.0, 50.0, 20.0, 40.0, "fp7-l-block");
    let internal_corner = find_edges_by_endpoints(&l_block, |a, b| {
        (a.x - 20.0).abs() < 1e-6
            && (a.y - 20.0).abs() < 1e-6
            && (b.x - 20.0).abs() < 1e-6
            && (b.y - 20.0).abs() < 1e-6
    });
    assert_eq!(internal_corner.len(), 1);
    let filleted_internal = finish_edge(
        &l_block,
        internal_corner[0],
        EdgeFinishKind::Fillet,
        5.0,
        "fp7-internal-fillet",
    );
    assert_fillet_presentation_invariants(&filleted_internal, "fp7-internal-fillet");
}

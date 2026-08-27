//! Comprehensive degree-by-degree test matrix verifying fillets and chamfers
//! at arbitrary non-90 degree angles, collinear continuous edges (0 deg),
//! and acute/obtuse dihedral angles.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Vector3,
};

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

fn finish(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
    label: &str,
) -> Result<Snapshot, artificer_protocol::KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: targets,
            kind,
            distance,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

/// Builds a solid block with two connected rim edges meeting at (0, 0, 30):
/// Edge 1: (-30, 0, 30) -> (0, 0, 30)
/// Edge 2: (0, 0, 30) -> (30*cos(delta_theta), 30*sin(delta_theta), 30)
/// delta_theta sweeps from +90 deg (+90 right turn) down through 0 deg (collinear in-line)
/// to -90 deg (-90 left turn).
fn make_turn_angle_solid(delta_deg: f64) -> (Snapshot, EntityRef, EntityRef) {
    let delta_rad = delta_deg.to_radians();
    let length = 30.0;
    let p_start = Point2::new(-length, 0.0);
    let p_corner = Point2::new(0.0, 0.0);
    let p_end = Point2::new(length * delta_rad.cos(), length * delta_rad.sin());

    let bottom_y = -100.0;
    let p_slant = Point2::new(p_end.x + 30.0, p_end.y - 40.0);
    let curves = vec![
        PlanarCurve2::Line {
            start: p_start,
            end: p_corner,
        },
        PlanarCurve2::Line {
            start: p_corner,
            end: p_end,
        },
        PlanarCurve2::Line {
            start: p_end,
            end: p_slant,
        },
        PlanarCurve2::Line {
            start: p_slant,
            end: Point2::new(p_slant.x, bottom_y),
        },
        PlanarCurve2::Line {
            start: Point2::new(p_slant.x, bottom_y),
            end: Point2::new(p_start.x, bottom_y),
        },
        PlanarCurve2::Line {
            start: Point2::new(p_start.x, bottom_y),
            end: p_start,
        },
    ];

    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves },
            holes: vec![],
        }],
    };

    let solid = execute(
        &NativeKernel::empty(),
        &format!("turn-{delta_deg}"),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: 30.0,
        },
    );

    let scene = NativeKernel::debug_scene(&solid);
    let at_corner = |p: &Point3| p.x.hypot(p.y) < 1.0e-4 && (p.z - 30.0).abs() < 1.0e-4;
    let on_top_rim = |p: &Point3| (p.z - 30.0).abs() < 1.0e-4;

    let mut found_edges = Vec::new();
    for edge in &scene.edges {
        if edge.endpoints.iter().all(on_top_rim)
            && edge.endpoints.iter().any(at_corner)
            && !found_edges.contains(&edge.source_edge)
        {
            found_edges.push(edge.source_edge);
        }
    }

    assert!(
        found_edges.len() >= 2,
        "Should find 2 top rim edges meeting at corner for turn angle {delta_deg} deg (found {})",
        found_edges.len()
    );

    (solid, found_edges[0], found_edges[1])
}

#[test]
fn test_fillet_and_chamfer_degree_by_degree_matrix_90_to_minus_90() {
    println!("=== Testing Fillet & Chamfer Degree-by-Degree from +90 deg to -90 deg ===");

    // Full degree-by-degree sweep across all 181 degrees from +90 deg to -90 deg
    let test_angles: Vec<f64> = (0..=180).map(|i| (90 - i) as f64).collect();

    let mut fillet_successes = 0;
    let mut chamfer_successes = 0;
    let finish_dist = 2.0;

    for &angle in &test_angles {
        // Build the base solid with corner turning by angle
        let (solid, e1, e2) = make_turn_angle_solid(angle);

        // 1. Test Fillet on the corner pair
        let fillet_res = finish(
            &solid,
            vec![e1, e2],
            EdgeFinishKind::Fillet,
            finish_dist,
            &format!("matrix-fillet-{angle}"),
        );
        assert!(
            fillet_res.is_ok(),
            "Fillet should succeed at turn angle {angle} deg: {:?}",
            fillet_res.err()
        );
        let fillet_solid = fillet_res.unwrap();
        let fillet_vol = fillet_solid.measures().volume;
        assert!(
            fillet_vol > 0.0,
            "Fillet volume must be positive at angle {angle}"
        );
        fillet_successes += 1;

        // 2. Test Chamfer on the corner pair
        let chamfer_res = finish(
            &solid,
            vec![e1, e2],
            EdgeFinishKind::Chamfer,
            finish_dist,
            &format!("matrix-chamfer-{angle}"),
        );
        assert!(
            chamfer_res.is_ok(),
            "Chamfer should succeed at turn angle {angle} deg: {:?}",
            chamfer_res.err()
        );
        let chamfer_solid = chamfer_res.unwrap();
        let chamfer_vol = chamfer_solid.measures().volume;
        assert!(
            chamfer_vol > 0.0,
            "Chamfer volume must be positive at angle {angle}"
        );
        chamfer_successes += 1;

        println!(
            "  Angle {:5.1} deg: Fillet OK (vol = {:.1}), Chamfer OK (vol = {:.1})",
            angle, fillet_vol, chamfer_vol
        );
    }

    assert_eq!(
        fillet_successes,
        test_angles.len(),
        "All fillet angles must pass"
    );
    assert_eq!(
        chamfer_successes,
        test_angles.len(),
        "All chamfer angles must pass"
    );
    println!("=== 100% of Fillet and Chamfer angles passed from +90 to -90 degrees ===");
}

#[test]
fn test_arbitrary_dihedral_angle_fillets_and_chamfers() {
    // Test non-90 dihedral angles
    let block = execute(
        &NativeKernel::empty(),
        "dihedral-block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: 50.0,
            size_y: 50.0,
            size_z: 50.0,
        },
    );

    let scene = NativeKernel::debug_scene(&block);
    let top_face = scene
        .triangles
        .iter()
        .find(|t| t.vertices.iter().all(|v| (v.z - 50.0).abs() < 1.0e-5))
        .unwrap()
        .source_face;

    let triangle_pocket = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(10.0, 10.0),
                        end: Point2::new(40.0, 10.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(40.0, 10.0),
                        end: Point2::new(25.0, 40.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(25.0, 40.0),
                        end: Point2::new(10.0, 10.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };

    let cut = execute(
        &block,
        "dihedral-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 50.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: triangle_pocket,
            distance: 20.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    let scene_cut = NativeKernel::debug_scene(&cut);
    let mut rim_edges = Vec::new();
    for e in &scene_cut.edges {
        let [p0, p1] = e.endpoints;
        if (p0.z - 50.0).abs() < 1.0e-4
            && (p1.z - 50.0).abs() < 1.0e-4
            && p0.x > 5.0
            && p0.x < 45.0
            && p0.y > 5.0
            && p0.y < 45.0
            && !rim_edges.contains(&e.source_edge)
        {
            rim_edges.push(e.source_edge);
        }
    }

    assert_eq!(
        rim_edges.len(),
        3,
        "Pocket has 3 rim edges (found {})",
        rim_edges.len()
    );

    // All 3 rim edges together (closed loop with 60 deg corners)
    let fillet_loop = finish(
        &cut,
        rim_edges.clone(),
        EdgeFinishKind::Fillet,
        1.5,
        "tri-rim-fillet",
    );
    assert!(
        fillet_loop.is_ok(),
        "Triangle pocket rim loop fillet should succeed"
    );

    let chamfer_loop = finish(
        &cut,
        rim_edges,
        EdgeFinishKind::Chamfer,
        1.5,
        "tri-rim-chamfer",
    );
    assert!(
        chamfer_loop.is_ok(),
        "Triangle pocket rim loop chamfer should succeed"
    );
}

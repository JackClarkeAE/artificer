//! Regression probe verifying that when a cylindrical cut (such as a circle with
//! a tangential slot) is crossed perpendicularly by another cut (e.g. an arbitrary
//! triangle or polygon), all internal facet seams of the cylindrical bore remain
//! marked `is_smooth: true` (hidden) in the presentation model, while physical
//! intersection boundaries and crease rails remain visible.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, Vector3,
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

#[test]
fn test_circle_tangent_slot_and_perpendicular_triangle_cut() {
    let size = 60.0;
    let block = execute(
        &NativeKernel::empty(),
        "block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: size,
            size_y: size,
            size_z: size,
        },
    );

    let scene_block = NativeKernel::debug_scene(&block);
    let top_face = scene_block
        .triangles
        .iter()
        .find(|t| t.vertices.iter().all(|v| (v.z - size).abs() < 1.0e-5))
        .unwrap()
        .source_face;

    // 1. Circle with tangential slot profile on top face (Z = size)
    // Circle centered at (30, 20) with radius 8.
    // Slot extends tangentially from (22, 20) to (22, 45), (38, 45), (38, 20).
    let center = Point2::new(30.0, 20.0);
    let circle_and_tangential_slot = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center,
                        start: Point2::new(38.0, 20.0),
                        end: Point2::new(22.0, 20.0),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(22.0, 20.0),
                        end: Point2::new(22.0, 45.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(22.0, 45.0),
                        end: Point2::new(38.0, 45.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(38.0, 45.0),
                        end: Point2::new(38.0, 20.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };

    let cut1 = execute(
        &block,
        "circle-slot-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, size),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: circle_and_tangential_slot,
            distance: size,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    let scene1 = NativeKernel::debug_scene(&cut1);
    let visible1 = scene1.edges.iter().filter(|e| !e.is_smooth).count();
    assert!(visible1 > 0, "Cut 1 should have visible boundary edges");

    // 2. Perpendicular cut through front face (Y = 0) with an arbitrary triangle profile
    let front_face = scene1
        .triangles
        .iter()
        .find(|t| t.vertices.iter().all(|v| v.y.abs() < 1.0e-5))
        .expect("should find front face")
        .source_face;

    let triangle_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(15.0, 15.0),
                        end: Point2::new(45.0, 15.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(45.0, 15.0),
                        end: Point2::new(30.0, 45.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(30.0, 45.0),
                        end: Point2::new(15.0, 15.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };

    let cut2 = execute(
        &cut1,
        "perpendicular-triangle-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: front_face,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: triangle_profile,
            distance: size,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    let scene2 = NativeKernel::debug_scene(&cut2);

    // Verify: No internal axial facet seams of the cylindrical bore are marked visible
    let mut cylinder_facet_visible_seams = 0;
    for edge in &scene2.edges {
        let [p0, p1] = edge.endpoints;
        let is_axial = (p0.x - p1.x).abs() < 1.0e-4 && (p0.y - p1.y).abs() < 1.0e-4;
        let r0 = (p0.x - 30.0).hypot(p0.y - 20.0);
        let r1 = (p1.x - 30.0).hypot(p1.y - 20.0);
        let on_cyl = (r0 - 8.0).abs() < 1.0e-3
            && (r1 - 8.0).abs() < 1.0e-3
            && p0.y <= 20.001
            && p1.y <= 20.001;

        if on_cyl && is_axial && !edge.is_smooth {
            cylinder_facet_visible_seams += 1;
        }
    }

    assert_eq!(
        cylinder_facet_visible_seams, 0,
        "All internal axial facet seams of the cylindrical bore must remain smooth/hidden"
    );
}

#[test]
fn test_circle_tangent_slot_and_perpendicular_pentagon_cut() {
    let size = 60.0;
    let block = execute(
        &NativeKernel::empty(),
        "block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: size,
            size_y: size,
            size_z: size,
        },
    );

    let scene_block = NativeKernel::debug_scene(&block);
    let top_face = scene_block
        .triangles
        .iter()
        .find(|t| t.vertices.iter().all(|v| (v.z - size).abs() < 1.0e-5))
        .unwrap()
        .source_face;

    let center = Point2::new(30.0, 20.0);
    let circle_and_tangential_slot = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center,
                        start: Point2::new(38.0, 20.0),
                        end: Point2::new(22.0, 20.0),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(22.0, 20.0),
                        end: Point2::new(22.0, 45.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(22.0, 45.0),
                        end: Point2::new(38.0, 45.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(38.0, 45.0),
                        end: Point2::new(38.0, 20.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };

    let cut1 = execute(
        &block,
        "circle-slot-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, size),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: circle_and_tangential_slot,
            distance: size,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    let scene1 = NativeKernel::debug_scene(&cut1);
    let front_face = scene1
        .triangles
        .iter()
        .find(|t| t.vertices.iter().all(|v| v.y.abs() < 1.0e-5))
        .expect("should find front face")
        .source_face;

    // Perpendicular pentagon cut
    let pentagon_corners = [
        Point2::new(15.0, 20.0),
        Point2::new(25.0, 10.0),
        Point2::new(45.0, 15.0),
        Point2::new(40.0, 45.0),
        Point2::new(20.0, 40.0),
    ];
    let pentagon_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: (0..5)
                    .map(|i| PlanarCurve2::Line {
                        start: pentagon_corners[i],
                        end: pentagon_corners[(i + 1) % 5],
                    })
                    .collect(),
            },
            holes: vec![],
        }],
    };

    let cut2 = execute(
        &cut1,
        "perpendicular-pentagon-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: front_face,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: pentagon_profile,
            distance: size,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    let scene2 = NativeKernel::debug_scene(&cut2);

    let mut cylinder_facet_visible_seams = 0;
    for edge in &scene2.edges {
        let [p0, p1] = edge.endpoints;
        let is_axial = (p0.x - p1.x).abs() < 1.0e-4 && (p0.y - p1.y).abs() < 1.0e-4;
        let r0 = (p0.x - 30.0).hypot(p0.y - 20.0);
        let r1 = (p1.x - 30.0).hypot(p1.y - 20.0);
        let on_cyl = (r0 - 8.0).abs() < 1.0e-3
            && (r1 - 8.0).abs() < 1.0e-3
            && p0.y <= 20.001
            && p1.y <= 20.001;

        if on_cyl && is_axial && !edge.is_smooth {
            cylinder_facet_visible_seams += 1;
        }
    }

    assert_eq!(
        cylinder_facet_visible_seams, 0,
        "All internal axial facet seams of the cylindrical bore must remain smooth/hidden under pentagon cut"
    );
}

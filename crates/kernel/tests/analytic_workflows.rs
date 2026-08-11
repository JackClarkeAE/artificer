use std::collections::BTreeMap;

use artificer_kernel::{CancellationToken, FaceRole, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityKind, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, KernelErrorCode, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, SimilarityTransform3,
    ValidationProfile, Vector3,
};

const EPSILON: f64 = 1.0e-9;

fn exact_circle(center: Point2, radius: f64, direction: ArcDirection) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center,
            radius,
            direction,
        }],
    }
}

fn planar_extrusion_request(profile: PlanarProfile2, distance: f64) -> ExecuteRequest {
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("analytic-workflow-extrusion"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::default(),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance,
        },
    }
}

fn disk(radius: f64, distance: f64) -> artificer_kernel::ExecutionOutcome {
    let input = NativeKernel::empty();
    NativeKernel::execute(
        &input,
        &planar_extrusion_request(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: exact_circle(Point2::default(), radius, ArcDirection::CounterClockwise),
                    holes: Vec::new(),
                }],
            },
            distance,
        ),
        &CancellationToken::new(),
    )
    .expect("exact disk extrusion")
}

fn point_in_or_on_xy_triangle(point: Point2, triangle: [Point3; 3]) -> bool {
    let orient =
        |a: Point3, b: Point3| (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
    let signs = [
        orient(triangle[0], triangle[1]),
        orient(triangle[1], triangle[2]),
        orient(triangle[2], triangle[0]),
    ];
    signs.iter().all(|value| *value >= -EPSILON) || signs.iter().all(|value| *value <= EPSILON)
}

#[test]
fn committed_exact_annulus_has_source_mapped_curves_cylinders_and_an_open_debug_hole() {
    let input = NativeKernel::empty();
    let outcome = NativeKernel::execute(
        &input,
        &planar_extrusion_request(
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: exact_circle(Point2::default(), 2.0, ArcDirection::CounterClockwise),
                    holes: vec![exact_circle(
                        Point2::default(),
                        1.0,
                        ArcDirection::Clockwise,
                    )],
                }],
            },
            1.0,
        ),
        &CancellationToken::new(),
    )
    .expect("exact annulus extrusion");
    assert!(outcome.report.validation.valid);
    assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);

    let scene = NativeKernel::debug_scene(&outcome.snapshot);
    assert!(!scene.triangles.is_empty());
    assert!(!scene.edges.is_empty());
    assert!(scene.triangles.iter().all(|triangle| {
        triangle.source_face.snapshot == outcome.snapshot.id()
            && triangle.source_face.kind == EntityKind::Face
    }));
    assert!(scene.edges.iter().all(|edge| {
        edge.source_edge.snapshot == outcome.snapshot.id()
            && edge.source_edge.kind == EntityKind::Edge
    }));

    let cap_triangles = scene
        .triangles
        .iter()
        .filter(|triangle| {
            matches!(
                triangle.role,
                FaceRole::ExtrusionBottom | FaceRole::ExtrusionTop
            )
        })
        .collect::<Vec<_>>();
    assert!(!cap_triangles.is_empty());
    assert!(cap_triangles.iter().all(|triangle| {
        !point_in_or_on_xy_triangle(
            Point2::default(),
            triangle
                .vertices
                .map(|point| Point3::new(point.x, point.y, point.z)),
        )
    }));

    let cylindrical_triangles = scene
        .triangles
        .iter()
        .filter(|triangle| matches!(triangle.role, FaceRole::ExtrusionSide(_)))
        .count();
    assert!(cylindrical_triangles > 8);

    let sampled_edge_counts = scene
        .edges
        .iter()
        .fold(BTreeMap::new(), |mut counts, edge| {
            *counts.entry(edge.source_edge).or_insert(0_usize) += 1;
            counts
        });
    assert_eq!(
        sampled_edge_counts
            .values()
            .filter(|count| **count > 1)
            .count(),
        8,
        "the eight exact cap semicircles must each retain their source edge id"
    );
}

#[test]
fn exact_disk_top_face_accepts_a_second_exact_circle_add_and_cut() {
    let base = disk(2.0, 1.0).snapshot;
    assert!((base.measures().volume - 4.0 * std::f64::consts::PI).abs() <= EPSILON);

    let top_face = NativeKernel::debug_scene(&base)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::ExtrusionTop)
        .expect("extrusion top debug face")
        .source_face;
    let support = NativeKernel::planar_face_support(&base, top_face)
        .expect("analytic cap remains exact sketch support");
    assert!(support.boundary.len() > 8);
    assert!(support.inner_boundaries.is_empty());

    let feature_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: exact_circle(Point2::default(), 0.5, ArcDirection::CounterClockwise),
            holes: Vec::new(),
        }],
    };
    for (operation, expected_volume) in [
        (
            FaceExtrusionOperation::Add,
            4.0 * std::f64::consts::PI + 0.125 * std::f64::consts::PI,
        ),
        (
            FaceExtrusionOperation::Cut,
            4.0 * std::f64::consts::PI - 0.125 * std::f64::consts::PI,
        ),
    ] {
        let outcome = NativeKernel::execute(
            &base,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("analytic-cap-circle-feature"),
                expected_snapshot: base.id(),
                precision: base.precision_policy().unwrap(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: top_face,
                    frame: support.frame,
                    profile: feature_profile.clone(),
                    distance: 0.5,
                    operation,
                },
            },
            &CancellationToken::new(),
        )
        .unwrap_or_else(|error| panic!("{operation:?}: {error:#?}"));
        assert!((outcome.snapshot.measures().volume - expected_volume).abs() <= EPSILON);
        assert!(outcome.report.validation.valid);
        assert_eq!(
            outcome.report.history.len(),
            outcome.snapshot.counts().total() as usize
        );
        assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
    }
}

#[test]
fn exact_disk_caps_accept_strict_inset_linear_add_and_cut_without_losing_the_cylinder() {
    let base = disk(2.0, 4.0).snapshot;
    let base_counts = base.counts();
    let base_measures = base.measures();
    assert!((base_measures.volume - 16.0 * std::f64::consts::PI).abs() <= EPSILON);
    assert!((base_measures.surface_area - 24.0 * std::f64::consts::PI).abs() <= EPSILON);

    let rectangle = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&[
                Point2::new(-0.5, -0.5),
                Point2::new(0.5, -0.5),
                Point2::new(0.5, 0.5),
                Point2::new(-0.5, 0.5),
            ]),
            holes: Vec::new(),
        }],
    };

    for role in [FaceRole::ExtrusionTop, FaceRole::ExtrusionBottom] {
        let target = NativeKernel::debug_scene(&base)
            .triangles
            .iter()
            .find(|triangle| triangle.role == role)
            .expect("analytic disk cap")
            .source_face;
        let support = NativeKernel::planar_face_support(&base, target)
            .expect("analytic disk cap remains exact planar support");

        for operation in [FaceExtrusionOperation::Add, FaceExtrusionOperation::Cut] {
            let outcome = NativeKernel::execute(
                &base,
                &ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!(
                        "analytic-cap-linear-{role:?}-{operation:?}"
                    )),
                    expected_snapshot: base.id(),
                    precision: base.precision_policy().unwrap(),
                    command: KernelCommand::ExtrudeFacePlanarProfile {
                        target_face: target,
                        frame: support.frame,
                        profile: rectangle.clone(),
                        distance: 1.0,
                        operation,
                    },
                },
                &CancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("{role:?} {operation:?}: {error:#?}"));

            let sign = if operation == FaceExtrusionOperation::Add {
                1.0
            } else {
                -1.0
            };
            let measures = outcome.snapshot.measures();
            assert!(
                (measures.volume - (base_measures.volume + sign)).abs() <= EPSILON,
                "{role:?} {operation:?}: expected a one-cubic-unit feature, got {measures:#?}"
            );
            assert!(
                (measures.surface_area - (base_measures.surface_area + 4.0)).abs() <= EPSILON,
                "{role:?} {operation:?}: expected four square units of sidewall"
            );
            let bounds = measures.bounds.expect("feature bounds");
            let (expected_min_z, expected_max_z, feature_centroid_z) = match (role, operation) {
                (FaceRole::ExtrusionTop, FaceExtrusionOperation::Add) => (0.0, 5.0, 4.5),
                (FaceRole::ExtrusionBottom, FaceExtrusionOperation::Add) => (-1.0, 4.0, -0.5),
                (FaceRole::ExtrusionTop, FaceExtrusionOperation::Cut) => (0.0, 4.0, 3.5),
                (FaceRole::ExtrusionBottom, FaceExtrusionOperation::Cut) => (0.0, 4.0, 0.5),
                _ => unreachable!("the fixture covers only disk caps"),
            };
            assert!((bounds.min.z - expected_min_z).abs() <= EPSILON);
            assert!((bounds.max.z - expected_max_z).abs() <= EPSILON);
            let expected_centroid_z = (base_measures.volume * 2.0 + sign * feature_centroid_z)
                / (base_measures.volume + sign);
            let centroid = measures.centroid.expect("feature centroid");
            assert!(centroid.x.abs() <= EPSILON);
            assert!(centroid.y.abs() <= EPSILON);
            assert!((centroid.z - expected_centroid_z).abs() <= EPSILON);

            let counts = outcome.snapshot.counts();
            assert_eq!(counts.vertices, base_counts.vertices + 8);
            assert_eq!(counts.edges, base_counts.edges + 12);
            assert_eq!(counts.coedges, base_counts.coedges + 24);
            assert_eq!(counts.loops, base_counts.loops + 6);
            assert_eq!(counts.faces, base_counts.faces + 5);
            assert_eq!(counts.shells, base_counts.shells);
            assert_eq!(counts.solids, base_counts.solids);
            assert!(outcome.report.validation.valid);
            assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
            assert_eq!(
                outcome.report.history.len(),
                outcome.snapshot.counts().total() as usize
            );

            let scene = NativeKernel::debug_scene(&outcome.snapshot);
            assert!(
                scene
                    .triangles
                    .iter()
                    .any(|triangle| { matches!(triangle.role, FaceRole::ExtrusionSide(_)) })
            );
            assert!(
                scene.triangles.iter().any(|triangle| {
                    triangle.role == role
                        && point_in_or_on_xy_triangle(Point2::new(1.5, 0.0), triangle.vertices)
                }),
                "{role:?} {operation:?}: the circular support shoulder disappeared"
            );
        }
    }
}

#[test]
fn standalone_analytic_extrusion_keeps_its_original_measure_path() {
    let outcome = disk(2.0, 4.0);
    let measures = outcome.snapshot.measures();
    assert!((measures.volume - 16.0 * std::f64::consts::PI).abs() <= EPSILON);
    assert!((measures.surface_area - 24.0 * std::f64::consts::PI).abs() <= EPSILON);
    assert!(
        NativeKernel::debug_scene(&outcome.snapshot)
            .triangles
            .iter()
            .all(|triangle| !matches!(triangle.role, FaceRole::FeatureSide(_)))
    );
    assert!(outcome.report.validation.valid);
    assert_eq!(
        outcome.report.history.len(),
        outcome.snapshot.counts().total() as usize
    );
}

#[test]
fn precision_perturbed_concentric_through_hole_crosses_an_earlier_boss_shoulder_void() {
    let empty = NativeKernel::empty();
    let base = NativeKernel::execute(
        &empty,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("analytic-concentric-cuboid"),
            expected_snapshot: empty.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::default(),
                size_x: 4.0,
                size_y: 4.0,
                size_z: 4.0,
            },
        },
        &CancellationToken::new(),
    )
    .unwrap()
    .snapshot;
    let top_face = NativeKernel::debug_scene(&base)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::PositiveZ)
        .expect("base top face")
        .source_face;
    let top_support = NativeKernel::planar_face_support(&base, top_face).unwrap();
    let circle_profile = |center, radius| PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: exact_circle(center, radius, ArcDirection::CounterClockwise),
            holes: Vec::new(),
        }],
    };
    let boss = NativeKernel::execute(
        &base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("analytic-concentric-boss"),
            expected_snapshot: base.id(),
            precision: base.precision_policy().unwrap(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: top_face,
                frame: top_support.frame,
                profile: circle_profile(Point2::default(), 1.0),
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            },
        },
        &CancellationToken::new(),
    )
    .expect("concentric exact boss");
    assert!((boss.snapshot.measures().volume - (64.0 + std::f64::consts::PI)).abs() <= EPSILON);

    let boss_end = NativeKernel::debug_scene(&boss.snapshot)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::FeatureEnd)
        .expect("boss end face")
        .source_face;
    let end_support = NativeKernel::planar_face_support(&boss.snapshot, boss_end).unwrap();
    let through = NativeKernel::execute(
        &boss.snapshot,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("analytic-concentric-through-hole"),
            expected_snapshot: boss.snapshot.id(),
            precision: boss.snapshot.precision_policy().unwrap(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: boss_end,
                frame: end_support.frame,
                // This is the real workbench support-bounds residue. It must
                // remain inside the earlier circular shoulder void under the
                // snapshot's linear-agreement policy.
                profile: circle_profile(Point2::new(0.0, -1.7763568394002505e-15), 0.5),
                distance: 10.0,
                operation: FaceExtrusionOperation::Cut,
            },
        },
        &CancellationToken::new(),
    )
    .expect("the shoulder void contains the smaller through-hole sweep");
    assert!(
        (through.snapshot.measures().volume - (64.0 - 0.25 * std::f64::consts::PI)).abs()
            <= EPSILON
    );
    assert!(
        (through.snapshot.measures().surface_area - (96.0 + 6.5 * std::f64::consts::PI)).abs()
            <= EPSILON
    );
    assert!(through.report.validation.valid);
    assert!(NativeKernel::validate(&through.snapshot, ValidationProfile::Solid).valid);
    assert_eq!(
        through.report.history.len(),
        through.snapshot.counts().total() as usize
    );
    let scene = NativeKernel::debug_scene(&through.snapshot);
    assert!(scene.triangles.iter().all(|triangle| {
        triangle.source_face.snapshot == through.snapshot.id()
            && triangle.source_face.kind == EntityKind::Face
    }));
    assert!(scene.edges.iter().all(|edge| {
        edge.source_edge.snapshot == through.snapshot.id()
            && edge.source_edge.kind == EntityKind::Edge
    }));
}

#[test]
fn exact_circle_feature_on_a_linear_rectangular_boss_retains_base_measures() {
    let empty = NativeKernel::empty();
    let base = NativeKernel::execute(
        &empty,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("mixed-feature-cuboid"),
            expected_snapshot: empty.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::default(),
                size_x: 4.0,
                size_y: 4.0,
                size_z: 4.0,
            },
        },
        &CancellationToken::new(),
    )
    .unwrap()
    .snapshot;
    let top_face = NativeKernel::debug_scene(&base)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::PositiveZ)
        .unwrap()
        .source_face;
    let top_support = NativeKernel::planar_face_support(&base, top_face).unwrap();
    let rectangle = [
        Point2::new(-1.0, -0.5),
        Point2::new(1.0, -0.5),
        Point2::new(1.0, 0.5),
        Point2::new(-1.0, 0.5),
    ];
    let linear_boss = NativeKernel::execute(
        &base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("mixed-feature-linear-boss"),
            expected_snapshot: base.id(),
            precision: base.precision_policy().unwrap(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: top_face,
                frame: top_support.frame,
                profile: PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: PlanarLoop2::from_polygon(&rectangle),
                        holes: Vec::new(),
                    }],
                },
                distance: 1.0,
                operation: FaceExtrusionOperation::Add,
            },
        },
        &CancellationToken::new(),
    )
    .expect("linear rectangular boss");
    assert!((linear_boss.snapshot.measures().volume - 66.0).abs() <= EPSILON);
    assert!((linear_boss.snapshot.measures().surface_area - 102.0).abs() <= EPSILON);

    let boss_end = NativeKernel::debug_scene(&linear_boss.snapshot)
        .triangles
        .iter()
        .find(|triangle| triangle.role == FaceRole::FeatureEnd)
        .unwrap()
        .source_face;
    let support = NativeKernel::planar_face_support(&linear_boss.snapshot, boss_end).unwrap();
    let circle = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: exact_circle(Point2::default(), 0.25, ArcDirection::CounterClockwise),
            holes: Vec::new(),
        }],
    };
    for (operation, signed_delta) in [
        (FaceExtrusionOperation::Add, 1.0),
        (FaceExtrusionOperation::Cut, -1.0),
    ] {
        let outcome = NativeKernel::execute(
            &linear_boss.snapshot,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("mixed-feature-exact-circle"),
                expected_snapshot: linear_boss.snapshot.id(),
                precision: linear_boss.snapshot.precision_policy().unwrap(),
                command: KernelCommand::ExtrudeFacePlanarProfile {
                    target_face: boss_end,
                    frame: support.frame,
                    profile: circle.clone(),
                    distance: 0.5,
                    operation,
                },
            },
            &CancellationToken::new(),
        )
        .unwrap_or_else(|error| panic!("{operation:?}: {error:#?}"));
        assert!(
            (outcome.snapshot.measures().volume
                - (66.0 + signed_delta * std::f64::consts::PI / 32.0))
                .abs()
                <= EPSILON
        );
        assert!(
            (outcome.snapshot.measures().surface_area - (102.0 + std::f64::consts::PI / 4.0)).abs()
                <= EPSILON
        );
        assert!(outcome.report.validation.valid);
        assert_eq!(
            outcome.report.history.len(),
            outcome.snapshot.counts().total() as usize
        );
    }
}

#[test]
fn circular_add_rejects_contact_with_a_sibling_body_without_mutating_input() {
    let rectangle = |x_min, x_max| {
        PlanarRegion2::from_polygon(&[
            Point2::new(x_min, 0.0),
            Point2::new(x_max, 0.0),
            Point2::new(x_max, 4.0),
            Point2::new(x_min, 4.0),
        ])
    };
    let input = NativeKernel::empty();
    let compound = NativeKernel::execute(
        &input,
        &planar_extrusion_request(
            PlanarProfile2 {
                regions: vec![rectangle(0.0, 2.0), rectangle(4.0, 6.0)],
            },
            4.0,
        ),
        &CancellationToken::new(),
    )
    .expect("two disjoint rectangular solids")
    .snapshot;
    let target = NativeKernel::debug_scene(&compound)
        .triangles
        .iter()
        .find(|triangle| {
            triangle
                .vertices
                .iter()
                .all(|point| (point.x - 2.0).abs() <= EPSILON)
        })
        .expect("right face of the left solid")
        .source_face;
    let support = NativeKernel::planar_face_support(&compound, target).unwrap();
    let before_digest = compound.semantic_digest();
    let before_measures = compound.measures();
    let before_scene = NativeKernel::debug_scene(&compound);
    let error = NativeKernel::execute(
        &compound,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("analytic-sibling-body-contact"),
            expected_snapshot: compound.id(),
            precision: compound.precision_policy().unwrap(),
            command: KernelCommand::ExtrudeFacePlanarProfile {
                target_face: target,
                frame: support.frame,
                profile: PlanarProfile2 {
                    regions: vec![PlanarRegion2 {
                        outer: exact_circle(Point2::default(), 0.5, ArcDirection::CounterClockwise),
                        holes: Vec::new(),
                    }],
                },
                distance: 3.0,
                operation: FaceExtrusionOperation::Add,
            },
        },
        &CancellationToken::new(),
    )
    .expect_err("the boss would enter the sibling solid");
    assert_eq!(error.code, KernelErrorCode::Unsupported);
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "FACE_FEATURE_SWEEP_COLLISION")
    );
    assert_eq!(compound.semantic_digest(), before_digest);
    assert_eq!(compound.measures(), before_measures);
    assert_eq!(NativeKernel::debug_scene(&compound), before_scene);
}

#[test]
fn transform_rejects_a_circle_whose_true_extremum_exceeds_the_coordinate_limit() {
    let base = disk(2.0, 1.0).snapshot;
    let original_digest = base.semantic_digest();
    let precision = base.precision_policy().unwrap();
    let error = NativeKernel::execute(
        &base,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("analytic-transform-extremum-limit"),
            expected_snapshot: base.id(),
            precision,
            command: KernelCommand::TransformSnapshot {
                transform: SimilarityTransform3 {
                    translation: Vector3::new(0.0, precision.max_abs_coordinate - 1.0, 0.0),
                    ..SimilarityTransform3::identity()
                },
            },
        },
        &CancellationToken::new(),
    )
    .expect_err("the circle extremum crosses the coordinate envelope");
    assert_eq!(error.code, KernelErrorCode::ResourceLimitExceeded);
    assert_eq!(
        error.diagnostics[0].code.as_str(),
        "TRANSFORM_COORDINATE_LIMIT_EXCEEDED"
    );
    assert_eq!(base.semantic_digest(), original_digest);
}

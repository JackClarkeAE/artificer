use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, Vector3,
};

const SIZE: f64 = 40.0;

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

fn top_face(snapshot: &Snapshot, height: f64) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .triangles
        .iter()
        .find(|triangle| {
            triangle
                .vertices
                .iter()
                .all(|vertex| (vertex.z - height).abs() < 1.0e-6)
        })
        .expect("the block should expose its top face")
        .source_face
}

fn outer_top_edge(snapshot: &Snapshot) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .edges
        .iter()
        .find(|edge| {
            edge.endpoints
                .iter()
                .all(|point| (point.z - SIZE).abs() < 1.0e-6)
                && edge.endpoints.iter().any(|point| point.x.abs() < 1.0e-6)
        })
        .expect("outer top edge should exist")
        .source_edge
}

#[test]
fn chamfer_cube_with_circle_and_slot_cuts() {
    let block = execute(
        &NativeKernel::empty(),
        "block",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
    );

    // 1. Circle cut on top face
    let circle_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 6.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };

    let circle_cut = execute(
        &block,
        "circle-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face(&block, SIZE),
            frame: PlanarFrame3::new(
                Point3::new(20.0, 20.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: circle_profile,
            distance: 20.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    // 2. Slot cut on top face partially intersecting the circle
    let slot_profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center: Point2::new(-5.0, 0.0),
                        start: Point2::new(-5.0, -3.0),
                        end: Point2::new(-5.0, 3.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(-5.0, 3.0),
                        end: Point2::new(5.0, 3.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(5.0, 0.0),
                        start: Point2::new(5.0, 3.0),
                        end: Point2::new(5.0, -3.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(5.0, -3.0),
                        end: Point2::new(-5.0, -3.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };

    let slot_cut = execute(
        &circle_cut,
        "slot-cut",
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top_face(&circle_cut, SIZE),
            frame: PlanarFrame3::new(
                Point3::new(25.0, 20.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: slot_profile,
            distance: 20.0,
            operation: FaceExtrusionOperation::Cut,
        },
    );

    // The slot is a 10 by 6 rectangle with both ends scooped inward by discs
    // of radius 3, the left one on the pocket's own axis; where it runs into
    // the pocket, the pocket has already taken the half-annulus strip
    // `|y − 20| ≤ 3, 3 ≤ r ≤ 6` around that axis. Both are 20 deep, so the
    // slot removes 20 × (its area less the strip), exactly: this cut used to
    // reach the faceted tier, and is the analytic Boolean's now. The strip is
    // `∫ 2r·asin(3/r) dr` over `[3, 6]`, walked through a map whose rate
    // vanishes at the ends so the square-root cusp at `r = 3` is smooth.
    let strip = {
        let panels = 20_000;
        let step = 1.0 / f64::from(panels);
        (0..=panels)
            .map(|index| {
                let t = f64::from(index) * step;
                let r = 3.0f64.mul_add(t * t * 2.0f64.mul_add(-t, 3.0), 3.0);
                let rate = 18.0 * t * (1.0 - t);
                let weight = if index == 0 || index == panels {
                    1.0
                } else if index % 2 == 1 {
                    4.0
                } else {
                    2.0
                };
                weight * 2.0 * r * (3.0 / r).min(1.0).asin() * rate
            })
            .sum::<f64>()
            * step
            / 3.0
    };
    let slot_area = 9.0f64.mul_add(-std::f64::consts::PI, 60.0);
    let expected = 20.0f64.mul_add(-(slot_area - strip), circle_cut.measures().volume);
    let slot_volume = slot_cut.measures().volume;
    assert!(
        ((slot_volume - expected) / expected).abs() < 1.0e-9,
        "the slot cut is exact: {slot_volume} vs {expected}"
    );

    // 3. Chamfer one outer top edge of the cube, for a valid solid whose
    //    volume lost at most the full 45-degree wedge along the edge.
    //
    //    This used to require the faceted tier and its approximation label.
    //    The premise was that the pocket walls are curved, so no exact rung
    //    owned a lone edge of this body — but what actually disqualified the
    //    edge was the cube's own top and side faces arriving as fans of
    //    panels, not the pockets. The coplanar merge (ADR 0039) puts those
    //    faces back together, the edge between two of them is an ordinary
    //    prism edge again, and the exact rung answers. So the label must now
    //    be absent, and the solid and its volume are checked as before.
    let edge = outer_top_edge(&slot_cut);
    let distance = 2.0;
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("chamfer"),
        expected_snapshot: slot_cut.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: vec![edge],
            kind: EdgeFinishKind::Chamfer,
            distance,
            standing_apart: false,
        },
    };
    let outcome = NativeKernel::execute(&slot_cut, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("the chamfer must build: {error:?}"));
    assert!(
        !outcome
            .report
            .warnings
            .iter()
            .any(|warning| warning.code.as_str() == "EDGE_FINISH_FACETED_APPROXIMATION"),
        "an edge between two whole planar faces is finished exactly, not \
         approximated: {:?}",
        outcome.report.warnings
    );
    assert!(
        NativeKernel::validate(
            &outcome.snapshot,
            artificer_protocol::ValidationProfile::Solid
        )
        .valid
    );
    let before = slot_cut.measures().volume;
    let after = outcome.snapshot.measures().volume;
    let wedge = 0.5 * distance * distance * SIZE;
    assert!(
        after < before && after >= before - wedge - 1.0e-6,
        "volume {after} must lie within the chamfer wedge below {before}"
    );
}

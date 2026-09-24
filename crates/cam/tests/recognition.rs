//! ADR 0057 gate 1: every fixture body classified as the table says.

mod common;

use artificer_cam::{Setup, recognise};
use artificer_kernel::{NativeKernel, Snapshot};
use artificer_protocol::{BooleanOperation, PlanarRegion2, Point3, PrecisionPolicy, Vector3};

use common::*;

#[test]
fn the_classification_table() {
    let table: Vec<(&str, Snapshot, &str)> = vec![
        ("cuboid", cuboid(), "Milled"),
        ("stepped shaft", stepped_shaft(), "Turned"),
        ("tube", tube(), "Turned"),
        ("ball", ball(), "Turned"),
        ("pocketed plate", pocketed_plate(), "Milled"),
        (
            "square-to-circle loft",
            square_to_circle_loft(),
            "Unsupported",
        ),
    ];
    for (name, snapshot, expected) in &table {
        let setup = recognise(snapshot);
        assert_eq!(setup.kind(), *expected, "{name}: {setup:?}");
    }
}

#[test]
fn a_cuboid_is_milled_from_above_with_one_level() {
    let Setup::Milled(milled) = recognise(&cuboid()) else {
        panic!("a cuboid is milled");
    };
    assert_eq!(milled.axis_label, "+Z");
    assert_eq!(milled.levels.len(), 1);
    assert!((milled.top - 4.0).abs() < 1.0e-12);
    assert!((milled.bottom - 0.0).abs() < 1.0e-12);
    assert!((milled.stock.min.x + 2.0).abs() < 1.0e-12);
    assert!((milled.stock.max.z - 5.0).abs() < 1.0e-12);
    let footprint = milled.footprint().unwrap();
    assert_eq!(footprint.len(), 1);
    assert!((artificer_cam::geom::region_area(&footprint[0]) - 6.0).abs() < 1.0e-9);
    assert_eq!(milled.origin(), Point3::new(-2.0, -2.0, 5.0));
}

#[test]
fn the_pocketed_plate_reads_three_levels_and_a_through_hole() {
    let Setup::Milled(milled) = recognise(&pocketed_plate()) else {
        panic!("the plate is milled");
    };
    assert_eq!(milled.axis_label, "+Z");
    let heights = milled
        .levels
        .iter()
        .map(|level| level.height)
        .collect::<Vec<_>>();
    assert_eq!(heights, vec![12.0, 8.0, 6.0]);
    // The top face carries the two pockets and the hole as holes.
    assert_eq!(milled.levels[0].regions.len(), 1);
    assert_eq!(milled.levels[0].regions[0].holes.len(), 3);
    // The footprint is the rounded outline with only the through hole left.
    let footprint = milled.footprint().unwrap();
    assert_eq!(footprint.len(), 1, "{footprint:?}");
    assert_eq!(footprint[0].holes.len(), 1);
    let (width, height, _) = PLATE;
    let outline_area = width * height - (4.0 - std::f64::consts::PI) * 64.0;
    let hole_area = std::f64::consts::PI * 9.0;
    assert!(
        (artificer_cam::geom::region_area(&footprint[0]) - (outline_area - hole_area)).abs()
            < 1.0e-9
    );
    // Just above the deep pocket's floor, the shallow pocket is already
    // material again and the deep pocket is still a hole.
    let above_deep = milled.section_above(6.0).unwrap();
    assert_eq!(above_deep.len(), 1);
    assert_eq!(above_deep[0].holes.len(), 2);
}

#[test]
fn the_stepped_shaft_is_turned_from_its_small_end() {
    let Setup::Turned(turned) = recognise(&stepped_shaft()) else {
        panic!("the shaft is turned");
    };
    assert!((turned.length - 75.0).abs() < 1.0e-12);
    assert!((turned.max_radius - 20.0).abs() < 1.0e-12);
    assert!((turned.stock.radius - 22.0).abs() < 1.0e-12);
    assert!(
        !turned.flipped,
        "the chamfered Ø20 end is already the high end"
    );
    assert!(!turned.through_bore);
    // The section is a closed counter-clockwise loop through the axis, with
    // z = 0 at the front face and the chuck end at z = -75.
    let area = artificer_cam::geom::signed_area(&turned.section);
    let expected = 20.0 * 30.0 + 15.0 * 25.0 - 2.0 * 4.0 + 10.0 * 18.0 + 18.0;
    assert!((area - expected).abs() < 1.0e-9, "{area} vs {expected}");
    let (min, max) = artificer_cam::geom::bounds(&turned.section).unwrap();
    assert!((max.y - 0.0).abs() < 1.0e-12 && (min.y + 75.0).abs() < 1.0e-12);
    assert!((min.x - 0.0).abs() < 1.0e-12 && (max.x - 20.0).abs() < 1.0e-12);
    assert_eq!(turned.axis.direction, Vector3::new(0.0, 0.0, 1.0));
    assert_eq!(turned.axis.origin, Point3::new(0.0, 0.0, 75.0));
}

#[test]
fn a_shaft_drawn_the_other_way_round_is_flipped_to_face_the_tailstock() {
    let reversed = revolve(
        polygon(&[
            (0.0, 0.0),
            (8.0, 0.0),
            (10.0, 2.0),
            (10.0, 20.0),
            (20.0, 20.0),
            (20.0, 50.0),
            (0.0, 50.0),
        ]),
        "reversed-shaft",
    );
    let Setup::Turned(turned) = recognise(&reversed) else {
        panic!("turned");
    };
    assert!(turned.flipped);
    assert_eq!(turned.axis.direction, Vector3::new(0.0, 0.0, -1.0));
    assert_eq!(turned.axis.origin, Point3::new(0.0, 0.0, 0.0));
    assert!(artificer_cam::geom::signed_area(&turned.section) > 0.0);
}

#[test]
fn the_tube_is_bored_through() {
    let Setup::Turned(turned) = recognise(&tube()) else {
        panic!("turned");
    };
    assert!(turned.through_bore);
    let (min, _) = artificer_cam::geom::bounds(&turned.section).unwrap();
    assert!((min.x - 15.0).abs() < 1.0e-12);
}

#[test]
fn the_loft_is_refused_with_its_ruled_faces_named() {
    let Setup::Unsupported { faces } = recognise(&square_to_circle_loft()) else {
        panic!("unsupported");
    };
    assert!(!faces.is_empty());
    assert!(
        faces.iter().all(|face| face.surface == "ruled"),
        "{faces:?}"
    );
}

/// A flat on a cylinder is reachable from above, so the part is a 2.5D
/// prism seen along its own axis: one setup on a mill beats two on a lathe
/// and a mill, and the table's first fit wins.
#[test]
fn a_flatted_cylinder_is_milled_along_its_axis() {
    let Some(flatted) = flatted_cylinder() else {
        eprintln!("the Boolean ladder cannot flat a cylinder yet; untested");
        return;
    };
    let setup = recognise(&flatted);
    assert_eq!(setup.kind(), "Milled", "{setup:?}");
}

/// A radial hole is reachable from neither the lathe nor any one mill axis:
/// the part is turned, then drilled.
#[test]
fn a_cross_drilled_cylinder_is_mill_turn() {
    let Some(drilled) = cross_drilled_cylinder() else {
        eprintln!("the Boolean ladder cannot cross-drill a cylinder yet; untested");
        return;
    };
    let setup = recognise(&drilled);
    let Setup::MillTurn(mill_turn) = setup else {
        panic!("{setup:?}");
    };
    assert_eq!(mill_turn.axis, Vector3::new(0.0, 0.0, 1.0));
    assert!(
        mill_turn
            .milled_faces
            .iter()
            .all(|face| face.reason.contains("radial hole")),
        "{:?}",
        mill_turn.milled_faces
    );
}

#[test]
fn a_cuboid_with_a_filleted_top_edge_is_milled_from_the_side() {
    use artificer_protocol::{EdgeFinishKind, KernelCommand};
    let block = cuboid();
    // The edge along X at the top front: its two ends.
    let scene = NativeKernel::debug_scene(&block);
    let edge = scene
        .edges
        .iter()
        .find(|edge| {
            edge.endpoints
                .iter()
                .all(|p| (p.z - 4.0).abs() < 1.0e-9 && p.y.abs() < 1.0e-9)
        })
        .expect("the top front edge")
        .source_edge;
    let filleted = execute(
        &block,
        "fillet",
        KernelCommand::FinishEdge {
            target_edge: edge,
            kind: EdgeFinishKind::Fillet,
            distance: 0.5,
        },
    );
    let Setup::Milled(milled) = recognise(&filleted) else {
        panic!(
            "a fillet along X is a wall seen from X: {:?}",
            recognise(&filleted)
        );
    };
    assert!(
        milled.axis_label == "+X" || milled.axis_label == "-X",
        "{}",
        milled.axis_label
    );
}

#[test]
fn the_kernel_queries_answer_in_closed_form() {
    // A square offset inward by one is a square of side eight.
    let square = artificer_cam::geom::rectangle(
        artificer_protocol::Point2::new(0.0, 0.0),
        artificer_protocol::Point2::new(10.0, 10.0),
    );
    let inner = NativeKernel::offset_loop(&square, 1.0).unwrap();
    assert_eq!(inner.len(), 1);
    assert!((artificer_cam::geom::signed_area(&inner[0]) - 64.0).abs() < 1.0e-9);
    let outer = NativeKernel::offset_loop(&square, -1.0).unwrap();
    assert!((artificer_cam::geom::signed_area(&outer[0]) - 144.0).abs() < 1.0e-9);
    // Offsetting past the middle leaves nothing rather than an inverted loop.
    assert!(NativeKernel::offset_loop(&square, 6.0).unwrap().is_empty());
    // Ten by ten minus a four by four bite out of one corner.
    let bite = artificer_cam::geom::rectangle(
        artificer_protocol::Point2::new(-1.0, -1.0),
        artificer_protocol::Point2::new(4.0, 4.0),
    );
    let difference = NativeKernel::profile_boolean(
        &[PlanarRegion2 {
            outer: square.clone(),
            holes: vec![],
        }],
        &[PlanarRegion2 {
            outer: bite,
            holes: vec![],
        }],
        BooleanOperation::Difference,
        PrecisionPolicy::default(),
    )
    .unwrap();
    assert_eq!(difference.len(), 1);
    assert!((artificer_cam::geom::region_area(&difference[0]) - 84.0).abs() < 1.0e-9);
    // A point in a cuboid.
    let block = cuboid();
    assert_eq!(
        NativeKernel::point_in_solid(&block, Point3::new(1.0, 1.5, 2.0)),
        Some(true)
    );
    assert_eq!(
        NativeKernel::point_in_solid(&block, Point3::new(3.0, 1.5, 2.0)),
        Some(false)
    );
    // The cuboid is a prism along Z with a 2 × 3 outer loop.
    let prism = NativeKernel::prism_profile(&block, Vector3::new(0.0, 0.0, 1.0)).unwrap();
    assert!((prism.height - 4.0).abs() < 1.0e-12);
    assert!((artificer_cam::geom::signed_area(&prism.outer).abs() - 6.0).abs() < 1.0e-9);
    assert!(NativeKernel::prism_profile(&block, Vector3::new(1.0, 0.0, 0.0)).is_none());
}

/// A cylinder drilled through is a tube, and a lathe part, before any rim
/// is chamfered: its section used to fail to chain and it was milled.
#[test]
fn a_drilled_cylinder_is_turned_before_it_is_chamfered() {
    use std::collections::BTreeMap;

    use artificer_kernel::api::scripting::NoModules;
    use artificer_kernel::api::session::Session;
    let mut session = Session::new();
    let outcome = session.run_script_with(
        "let s = sketch(on: \"XY\", entities: [circle(center: [0, 0], radius: 20)], label: \"s\");\nlet cyl = extrude(sketch: s, distance: 60, label: \"cyl\");\ndrill(face: faces(\">Z\"), center: [0, 0], diameter: 10, depth: 60, label: \"bore\");",
        &BTreeMap::new(),
        &NoModules,
        &artificer_kernel::CancellationToken::default(),
    );
    assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
    let Setup::Turned(turned) = recognise(&session.snapshot) else {
        panic!(
            "a bored cylinder is turned: {:?}",
            recognise(&session.snapshot).kind()
        );
    };
    assert!(turned.through_bore);
    assert!((turned.max_radius - 20.0).abs() < 1.0e-9);
    let plan = artificer_cam::plan_setup(
        &Setup::Turned(turned),
        &artificer_cam::ToolLibrary::builtin(),
        artificer_cam::Material::Aluminium,
    )
    .expect("a tube plans");
    assert!(
        plan.operations.iter().any(|operation| {
            matches!(operation.kind, artificer_cam::plan::OperationKind::Drill)
        })
    );
}

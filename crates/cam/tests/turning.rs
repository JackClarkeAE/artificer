//! ADR 0057 gate 2: the stepped shaft with a groove and a chamfer roughs,
//! finishes and parts off, and the final section equals the part's to 1e-9.

mod common;

use artificer_cam::plan::OperationKind;
use artificer_cam::simulate::{LatheSimulation, motions_from_plan};
use artificer_cam::stock::{LatheStock, loops_agree};
use artificer_cam::turning::{FINISH_ALLOWANCE_AXIAL, FINISH_ALLOWANCE_RADIAL, read_section};
use artificer_cam::{Material, Setup, ToolKind, ToolLibrary, geom, plan_setup, recognise};
use artificer_protocol::{PlanarCurve2, Point2};

use common::*;

fn turned(snapshot: &artificer_kernel::Snapshot) -> artificer_cam::TurnedSetup {
    match recognise(snapshot) {
        Setup::Turned(turned) => turned,
        other => panic!("expected a turned setup, got {other:?}"),
    }
}

#[test]
fn the_section_reads_its_groove_and_chamfer() {
    let setup = turned(&stepped_shaft());
    let reading = read_section(&setup).unwrap();
    assert_eq!(reading.grooves.len(), 1);
    let groove = reading.grooves[0];
    assert!((groove.front_z + 28.0).abs() < 1.0e-9, "{groove:?}");
    assert!((groove.back_z + 32.0).abs() < 1.0e-9);
    assert!((groove.floor_radius - 13.0).abs() < 1.0e-9);
    assert!((groove.rim_radius - 15.0).abs() < 1.0e-9);
    assert!(reading.bore.is_none());
    assert!((reading.front_outer_radius - 8.0).abs() < 1.0e-9);
    assert!((reading.back_outer_radius - 20.0).abs() < 1.0e-9);
    // The envelope runs front to back, chamfer first, with the groove gone.
    let first = geom::curve_start(&reading.envelope[0]);
    assert!((first.x - 8.0).abs() < 1.0e-9 && first.y.abs() < 1.0e-9);
    let last = geom::curve_end(reading.envelope.last().unwrap());
    assert!((last.x - 20.0).abs() < 1.0e-9 && (last.y + 75.0).abs() < 1.0e-9);
    assert_eq!(reading.envelope.len(), 6, "{:?}", reading.envelope);
    // Chamfer, Ø20, shoulder, Ø30, three groove faces, Ø30, shoulder, Ø40.
    assert_eq!(reading.outside.len(), 10);
}

#[test]
fn the_stepped_shaft_is_turned_to_its_exact_section() {
    let snapshot = stepped_shaft();
    let setup = turned(&snapshot);
    let library = ToolLibrary::builtin();
    let plan = plan_setup(&Setup::Turned(setup.clone()), &library, Material::Aluminium).unwrap();
    let kinds = plan
        .operations
        .iter()
        .map(|operation| operation.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            OperationKind::Face,
            OperationKind::Rough,
            OperationKind::Finish,
            OperationKind::Groove,
            OperationKind::PartOff,
        ]
    );
    let sequence = plan
        .tool_sequence()
        .into_iter()
        .map(|number| plan.tool(number).unwrap().kind)
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        vec![
            ToolKind::TurningInsertRough,
            ToolKind::TurningInsertFinish,
            ToolKind::PartingBlade,
        ]
    );
    assert_eq!(plan.tool_changes(), 3);

    let stock = LatheStock::bar(setup.stock.radius, setup.stock.back, setup.stock.front);
    let bar_volume = stock.volume();
    let motions = motions_from_plan(&plan);
    let simulation = LatheSimulation::run(&plan, motions, stock).unwrap();
    assert!(
        simulation.collisions.is_empty(),
        "{:?}",
        simulation.collisions
    );

    // Gate: the final section equals the part's to 1e-9 in area and in
    // every vertex.
    let final_stock = simulation.final_stock();
    assert_eq!(
        final_stock.regions.len(),
        2,
        "the part and the chuck remnant"
    );
    let part = final_stock.part_region().unwrap();
    assert!(part.holes.is_empty());
    loops_agree(&setup.section, &part.outer, 1.0e-9).unwrap();
    let part_volume = artificer_cam::stock::loop_swept_volume(&part.outer);
    assert!(
        (part_volume - stepped_shaft_volume()).abs() < 1.0e-6,
        "{part_volume} vs {}",
        stepped_shaft_volume()
    );
    assert!((part_volume - snapshot.measures().volume).abs() < 1.0e-6);
    eprintln!(
        "stepped shaft: bar {bar_volume:.1} mm³, part {part_volume:.1} mm³, {} motions, {:.1} s, {} tool changes",
        simulation.motions.len(),
        simulation.total_seconds,
        plan.tool_changes()
    );
    assert!(simulation.total_seconds > 60.0 && simulation.total_seconds < 3600.0);

    // Gate: roughing never cuts inside the finish allowance. After the rough
    // operation, every point of the envelope pushed out by a little less
    // than the allowance is still material.
    let last_rough = simulation
        .motions
        .iter()
        .rposition(|motion| motion.operation == 1)
        .unwrap();
    let after_rough = simulation.stock_after(last_rough);
    let reading = read_section(&setup).unwrap();
    for curve in &reading.envelope {
        let PlanarCurve2::Line { start, end } = curve else {
            continue;
        };
        let axial = (start.x - end.x).abs() < 1.0e-9;
        for t in [0.1, 0.5, 0.9] {
            let p = Point2::new(
                (end.x - start.x).mul_add(t, start.x),
                (end.y - start.y).mul_add(t, start.y),
            );
            let probe = if axial {
                Point2::new(p.x + FINISH_ALLOWANCE_RADIAL * 0.9, p.y)
            } else {
                Point2::new(p.x, p.y + FINISH_ALLOWANCE_AXIAL * 0.75)
            };
            assert!(
                after_rough.contains(probe),
                "roughing cut inside the allowance at ({}, {})",
                probe.x,
                probe.y
            );
            // And the part itself is untouched.
            let inside = if axial {
                Point2::new(p.x - 1.0e-3, p.y)
            } else {
                Point2::new(p.x, p.y - 1.0e-3)
            };
            assert!(after_rough.contains(inside));
        }
    }
    // The roughing did take most of the stock: what remains is the part
    // plus a thin skin plus the remnant in the chuck.
    let remnant = std::f64::consts::PI
        * setup.stock.radius.powi(2)
        * (artificer_cam::recognise::PART_OFF_MARGIN);
    let skin_bound = 2.0 * std::f64::consts::PI * 20.0 * 0.6 * 80.0;
    assert!(
        after_rough.volume() < stepped_shaft_volume() + remnant + skin_bound,
        "{}",
        after_rough.volume()
    );
}

#[test]
fn the_tube_is_drilled_bored_and_parted() {
    let snapshot = tube();
    let setup = turned(&snapshot);
    let library = ToolLibrary::builtin();
    let plan = plan_setup(&Setup::Turned(setup.clone()), &library, Material::Brass).unwrap();
    let kinds = plan
        .operations
        .iter()
        .map(|operation| operation.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            OperationKind::Face,
            OperationKind::Rough,
            OperationKind::CentreDrill,
            OperationKind::Drill,
            OperationKind::BoreRough,
            OperationKind::BoreFinish,
            OperationKind::Finish,
            OperationKind::PartOff,
        ]
    );
    let stock = LatheStock::bar(setup.stock.radius, setup.stock.back, setup.stock.front);
    let simulation = LatheSimulation::run(&plan, motions_from_plan(&plan), stock).unwrap();
    assert!(
        simulation.collisions.is_empty(),
        "{:?}",
        simulation.collisions
    );
    let part = simulation.final_stock().part_region().unwrap();
    loops_agree(&setup.section, &part.outer, 1.0e-9).unwrap();
    let volume = artificer_cam::stock::loop_swept_volume(&part.outer);
    let expected = std::f64::consts::PI * (25.0_f64.powi(2) - 15.0_f64.powi(2)) * 40.0;
    assert!((volume - expected).abs() < 1.0e-6, "{volume} vs {expected}");
}

#[test]
fn an_undercut_is_refused_by_name() {
    // A shaft fatter in the middle than at either end faces the chuck on
    // one shoulder whichever end is in front.
    let dumbbell = revolve(
        polygon(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (20.0, 10.0),
            (20.0, 30.0),
            (10.0, 30.0),
            (10.0, 40.0),
            (0.0, 40.0),
        ]),
        "dumbbell",
    );
    let setup = turned(&dumbbell);
    let refusal = plan_setup(
        &Setup::Turned(setup),
        &ToolLibrary::builtin(),
        Material::Aluminium,
    )
    .unwrap_err();
    assert!(
        matches!(refusal, artificer_cam::CamRefusal::TurnedUndercut { .. }),
        "{refusal}"
    );
}

#[test]
fn a_groove_narrower_than_the_blade_is_refused_by_name() {
    let shaft = revolve(
        polygon(&[
            (0.0, 0.0),
            (15.0, 0.0),
            (15.0, 10.0),
            (13.0, 10.0),
            (13.0, 12.0),
            (15.0, 12.0),
            (15.0, 30.0),
            (0.0, 30.0),
        ]),
        "narrow-groove",
    );
    let setup = turned(&shaft);
    let refusal = plan_setup(
        &Setup::Turned(setup),
        &ToolLibrary::builtin(),
        Material::Aluminium,
    )
    .unwrap_err();
    assert!(
        matches!(refusal, artificer_cam::CamRefusal::GrooveUnsupported { .. }),
        "{refusal}"
    );
}

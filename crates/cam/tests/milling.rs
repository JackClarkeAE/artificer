//! ADR 0057 gate 3: the pocketed plate is faced, pocketed with two tools,
//! profiled and drilled; the heightmap's volume matches the part's; no tool
//! position enters the part; no rapid passes through stock.

mod common;

use artificer_cam::plan::OperationKind;
use artificer_cam::simulate::{MillSimulation, MotionKind, motions_from_plan, position_along};
use artificer_cam::stock::MillStock;
use artificer_cam::{Machine, Material, Setup, ToolKind, ToolLibrary, plan_setup, recognise};
use artificer_kernel::{NativeKernel, Snapshot};
use artificer_protocol::Point3;

use common::*;

fn milled(snapshot: &Snapshot) -> artificer_cam::MilledSetup {
    match recognise(snapshot) {
        Setup::Milled(milled) => milled,
        other => panic!("expected a milled setup, got {other:?}"),
    }
}

/// The stock in work coordinates, gridded from the smallest tool.
fn stock_for(setup: &artificer_cam::MilledSetup, plan: &artificer_cam::Plan) -> MillStock {
    let origin = setup.origin();
    let smallest = plan
        .tools
        .iter()
        .map(|tool| tool.diameter)
        .fold(f64::INFINITY, f64::min);
    MillStock::for_tools(
        Point3::new(
            setup.stock.min.x - origin.x,
            setup.stock.min.y - origin.y,
            setup.stock.min.z - origin.z,
        ),
        Point3::new(
            setup.stock.max.x - origin.x,
            setup.stock.max.y - origin.y,
            setup.stock.max.z - origin.z,
        ),
        smallest,
    )
}

#[test]
fn the_pocketed_plate_is_machined_with_the_right_tools_in_the_right_order() {
    let snapshot = pocketed_plate();
    let setup = milled(&snapshot);
    let library = ToolLibrary::builtin();
    let plan = plan_setup(&Setup::Milled(setup.clone()), &library, Material::Aluminium).unwrap();
    assert_eq!(plan.machine, Machine::Mill);
    let summary = plan
        .operations
        .iter()
        .map(|operation| (operation.kind, plan.tool(operation.tool).unwrap().diameter))
        .collect::<Vec<_>>();
    eprintln!("plate operations: {summary:?}");
    // Face and profile with the 12, the wide pocket with the 10, the tight
    // pocket with the 4, the hole with the Ø6 drill.
    assert_eq!(summary[0], (OperationKind::Face, 12.0));
    let profiles = summary
        .iter()
        .filter(|(kind, _)| *kind == OperationKind::Profile)
        .collect::<Vec<_>>();
    assert_eq!(profiles.len(), 3, "one ring between each pair of levels");
    assert!(profiles.iter().all(|(_, diameter)| *diameter == 12.0));
    let pockets = summary
        .iter()
        .filter(|(kind, _)| *kind == OperationKind::Pocket)
        .map(|(_, diameter)| *diameter)
        .collect::<Vec<_>>();
    assert_eq!(pockets, vec![10.0, 4.0], "{summary:?}");
    let drills = summary
        .iter()
        .filter(|(kind, _)| *kind == OperationKind::Drill)
        .collect::<Vec<_>>();
    assert_eq!(drills.len(), 1);
    assert_eq!(drills[0].1, 6.0);
    let sequence = plan
        .tool_sequence()
        .into_iter()
        .map(|number| {
            let tool = plan.tool(number).unwrap();
            (tool.kind, tool.diameter)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        vec![
            (ToolKind::FlatEndMill, 12.0),
            (ToolKind::FlatEndMill, 10.0),
            (ToolKind::FlatEndMill, 4.0),
            (ToolKind::Drill, 6.0),
        ]
    );
    assert_eq!(plan.tool_changes(), 4);
    // The drill goes in last and pecks.
    let last = plan.operations.last().unwrap();
    assert_eq!(last.kind, OperationKind::Drill);
    assert!(matches!(
        last.moves[1],
        artificer_cam::Move::Drill { peck: Some(_), .. }
    ));
}

#[test]
fn the_heightmap_ends_within_the_gate_of_the_part_volume() {
    let snapshot = pocketed_plate();
    let setup = milled(&snapshot);
    let library = ToolLibrary::builtin();
    let plan = plan_setup(&Setup::Milled(setup.clone()), &library, Material::Aluminium).unwrap();
    let stock = stock_for(&setup, &plan);
    let stock_volume = stock.volume();
    let motions = motions_from_plan(&plan);
    let simulation = MillSimulation::run(&plan, motions, stock).unwrap();
    assert!(
        simulation.collisions.is_empty(),
        "{:?}",
        simulation.collisions
    );
    let remaining = simulation.final_stock.volume();
    let part = snapshot.measures().volume;
    let boundary = simulation.final_stock.boundary_cells();
    let gate = simulation.final_stock.cell_volume() * boundary as f64;
    eprintln!(
        "plate: stock {stock_volume:.1} mm³, remaining {remaining:.1} mm³, part {part:.1} mm³, {} motions, {:.1} s, cell {:.3} mm, {boundary} boundary cells, gate ±{gate:.1} mm³",
        simulation.motions.len(),
        simulation.total_seconds,
        simulation.final_stock.cell
    );
    assert!(
        (remaining - part).abs() <= gate,
        "{remaining} vs {part} beyond {gate}"
    );
    // And much tighter than the gate: within two percent.
    assert!(
        (remaining - part).abs() <= 0.02 * part,
        "{remaining} vs {part}"
    );
    assert!(simulation.total_seconds > 30.0 && simulation.total_seconds < 7200.0);
    // Scrubbing replays to the same state the run kept.
    let midway = simulation.motions.len() / 2;
    let replayed = simulation.stock_after(midway);
    let direct = {
        let mut stock = simulation.initial.clone();
        for (index, motion) in simulation.motions.iter().enumerate().take(midway + 1) {
            if motion.is_cutting() {
                let radius = plan.tool(motion.tool).unwrap().radius();
                artificer_cam::simulate::apply_mill_motion(&mut stock, motion, radius);
                let _ = index;
            }
        }
        stock
    };
    assert_eq!(replayed.heights, direct.heights);
    assert_eq!(
        simulation.stock_after(simulation.motions.len() - 1).heights,
        simulation.final_stock.heights
    );
}

#[test]
fn no_tool_position_enters_the_part() {
    let snapshot = pocketed_plate();
    let setup = milled(&snapshot);
    let library = ToolLibrary::builtin();
    let plan = plan_setup(&Setup::Milled(setup.clone()), &library, Material::Aluminium).unwrap();
    let origin = setup.origin();
    let motions = motions_from_plan(&plan);
    let mut samples = 0_usize;
    for motion in motions.iter().filter(|motion| motion.is_cutting()) {
        let tool = plan.tool(motion.tool).unwrap();
        let radius = tool.radius() - 1.0e-6;
        let steps = (motion.length(Machine::Mill) / 0.5).ceil().max(1.0) as usize;
        for step in 0..=steps {
            let position = position_along(motion, Machine::Mill, step as f64 / steps as f64);
            // The tool's bottom disc, just above the tip: its centre and
            // eight rim points.
            let mut probes = vec![(0.0, 0.0)];
            for eighth in 0..8 {
                let angle = std::f64::consts::TAU * eighth as f64 / 8.0;
                probes.push((radius * angle.cos(), radius * angle.sin()));
            }
            // A drill's point is a cone: only its centre reaches the tip.
            let probes = if tool.kind == ToolKind::Drill {
                vec![(0.0, 0.0)]
            } else {
                probes
            };
            for (dx, dy) in probes {
                let machine = Point3::new(
                    position.x + dx + origin.x,
                    position.y + dy + origin.y,
                    position.z + 1.0e-6 + origin.z,
                );
                let world = setup.frame.to_world(machine);
                samples += 1;
                assert_ne!(
                    NativeKernel::point_in_solid(&snapshot, world),
                    Some(true),
                    "tool T{} at ({:.3}, {:.3}, {:.3}) gouges the part (line {}, {:?})",
                    motion.tool,
                    position.x,
                    position.y,
                    position.z,
                    motion.line,
                    motion.kind
                );
            }
        }
    }
    assert!(samples > 1000, "{samples} samples");
}

#[test]
fn a_plain_cuboid_is_faced_and_profiled_only() {
    let snapshot = cuboid();
    let setup = milled(&snapshot);
    let library = ToolLibrary::builtin();
    let plan = plan_setup(&Setup::Milled(setup.clone()), &library, Material::Abs).unwrap();
    let kinds = plan
        .operations
        .iter()
        .map(|operation| operation.kind)
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec![OperationKind::Face, OperationKind::Profile]);
    assert_eq!(plan.tool_changes(), 1);
    let stock = stock_for(&setup, &plan);
    let simulation = MillSimulation::run(&plan, motions_from_plan(&plan), stock).unwrap();
    assert!(
        simulation.collisions.is_empty(),
        "{:?}",
        simulation.collisions
    );
    let remaining = simulation.final_stock.volume();
    assert!((remaining - 24.0).abs() < 0.02 * 24.0, "{remaining}");
    assert!(
        simulation
            .motions
            .iter()
            .any(|motion| matches!(motion.kind, MotionKind::Rapid))
    );
}

#[test]
fn a_hole_with_no_drill_of_its_size_is_bored_helically() {
    use artificer_protocol::{KernelCommand, Point2};
    let (width, height, thickness) = PLATE;
    let plate = execute(
        &NativeKernel::empty(),
        "bore-plate",
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: width,
            size_y: height,
            size_z: thickness,
        },
    );
    let top = top_face(&plate, thickness);
    let bored = execute(
        &plate,
        "odd-hole",
        KernelCommand::DrillHole {
            target_face: top,
            frame: xy_frame(Point3::new(0.0, 0.0, thickness)),
            center: Point2::new(40.0, 25.0),
            diameter: 15.25,
            depth: 5.0,
        },
    );
    let setup = milled(&bored);
    let plan = plan_setup(
        &Setup::Milled(setup.clone()),
        &ToolLibrary::builtin(),
        Material::Brass,
    )
    .unwrap();
    let helical = plan
        .operations
        .iter()
        .find(|operation| operation.kind == OperationKind::HelicalBore)
        .expect("a blind Ø15.25 hole is bored with an end mill");
    assert!(plan.tool(helical.tool).unwrap().diameter <= 14.75);
    assert!(
        helical
            .moves
            .iter()
            .filter(|m| matches!(m, artificer_cam::Move::Arc { .. }))
            .count()
            >= 4
    );
    let stock = stock_for(&setup, &plan);
    let simulation = MillSimulation::run(&plan, motions_from_plan(&plan), stock).unwrap();
    assert!(
        simulation.collisions.is_empty(),
        "{:?}",
        simulation.collisions
    );
    let remaining = simulation.final_stock.volume();
    let part = bored.measures().volume;
    assert!(
        (remaining - part).abs() <= 0.02 * part,
        "{remaining} vs {part}"
    );
}

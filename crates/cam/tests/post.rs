//! ADR 0057 gate 4: the posted G-code re-simulates identically through the
//! interpreter, and an arc, a drilling cycle and a G96 block each read.

mod common;

use artificer_cam::interpreter::{arc_centre_from_radius, interpret};
use artificer_cam::post::post;
use artificer_cam::simulate::{
    LatheSimulation, MillSimulation, Motion, MotionKind, motion_seconds, motions_from_plan,
    start_position,
};
use artificer_cam::stock::{LatheStock, MillStock, loops_agree};
use artificer_cam::{
    FeedRate, Machine, Material, Setup, Spindle, ToolLibrary, plan_setup, recognise,
};
use artificer_protocol::{Point2, Point3};

use common::*;

fn same_motions(planned: &[Motion], read: &[Motion]) {
    assert_eq!(planned.len(), read.len(), "motion counts differ");
    for (index, (a, b)) in planned.iter().zip(read).enumerate() {
        assert_eq!(a.from, b.from, "motion {index} start (line {})", b.line);
        assert_eq!(a.to, b.to, "motion {index} end (line {})", b.line);
        assert_eq!(a.tool, b.tool, "motion {index} tool");
        assert_eq!(a.feed, b.feed, "motion {index} feed");
        assert_eq!(a.spindle, b.spindle, "motion {index} spindle");
        assert_eq!(a.operation, b.operation, "motion {index} operation");
        match (a.kind, b.kind) {
            (MotionKind::Rapid, MotionKind::Rapid) | (MotionKind::Feed, MotionKind::Feed) => {}
            (
                MotionKind::Arc {
                    center: ca,
                    clockwise: wa,
                },
                MotionKind::Arc {
                    center: cb,
                    clockwise: wb,
                },
            ) => {
                assert_eq!(wa, wb, "motion {index} arc sense");
                assert!(
                    (ca.x - cb.x).abs() < 1.0e-9 && (ca.y - cb.y).abs() < 1.0e-9,
                    "motion {index} arc centre {ca:?} vs {cb:?}"
                );
            }
            (a, b) => panic!("motion {index}: {a:?} vs {b:?}"),
        }
    }
}

#[test]
fn the_lathe_program_re_simulates_to_the_same_section() {
    let snapshot = stepped_shaft();
    let Setup::Turned(setup) = recognise(&snapshot) else {
        panic!("turned");
    };
    let plan = plan_setup(
        &Setup::Turned(setup.clone()),
        &ToolLibrary::builtin(),
        Material::Aluminium,
    )
    .unwrap();
    let text = post(&plan);
    assert!(text.starts_with("%\n(Artificer CAM: "));
    assert!(text.trim_end().ends_with("M30\n%"));
    assert_eq!(text.matches(" M6").count(), plan.tool_changes());
    assert!(
        text.contains("G96 D3000 S300 M3"),
        "constant surface speed for aluminium"
    );
    assert!(text.contains("G95"), "feed per revolution");
    assert!(text.contains("G7"), "diameter programming");
    let read = interpret(&text, Machine::Lathe, start_position(&plan)).unwrap();
    let planned = motions_from_plan(&plan);
    same_motions(&planned, &read);
    let stock = || LatheStock::bar(setup.stock.radius, setup.stock.back, setup.stock.front);
    let from_plan = LatheSimulation::run(&plan, planned, stock()).unwrap();
    let from_gcode = LatheSimulation::run(&plan, read, stock()).unwrap();
    assert!(from_gcode.collisions.is_empty());
    assert_eq!(from_plan.states.len(), from_gcode.states.len());
    assert!((from_plan.total_seconds - from_gcode.total_seconds).abs() < 1.0e-9);
    let a = from_plan.final_stock().part_region().unwrap();
    let b = from_gcode.final_stock().part_region().unwrap();
    assert_eq!(
        a, b,
        "the section after the G-code is the section after the plan, bit for bit"
    );
    loops_agree(&setup.section, &b.outer, 1.0e-9).unwrap();
    eprintln!(
        "lathe program: {} lines, {:.1} s",
        text.lines().count(),
        from_gcode.total_seconds
    );
}

#[test]
fn the_mill_program_re_simulates_to_the_same_heightmap() {
    let snapshot = pocketed_plate();
    let Setup::Milled(setup) = recognise(&snapshot) else {
        panic!("milled");
    };
    let plan = plan_setup(
        &Setup::Milled(setup.clone()),
        &ToolLibrary::builtin(),
        Material::Aluminium,
    )
    .unwrap();
    let text = post(&plan);
    assert!(text.contains("G90.1"), "absolute arc centres");
    assert!(text.contains("G43 H"), "tool length compensation");
    assert!(text.contains("G99 G83 "), "a peck cycle for the hole");
    assert!(text.contains("G80"));
    assert_eq!(text.matches(" M6").count(), 4);
    let read = interpret(&text, Machine::Mill, start_position(&plan)).unwrap();
    let planned = motions_from_plan(&plan);
    same_motions(&planned, &read);
    let origin = setup.origin();
    let stock = || {
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
            4.0,
        )
    };
    let from_plan = MillSimulation::run(&plan, planned, stock()).unwrap();
    let from_gcode = MillSimulation::run(&plan, read, stock()).unwrap();
    assert!(
        from_gcode.collisions.is_empty(),
        "{:?}",
        from_gcode.collisions
    );
    assert_eq!(
        from_plan.final_stock.heights,
        from_gcode.final_stock.heights
    );
    assert!((from_plan.total_seconds - from_gcode.total_seconds).abs() < 1.0e-9);
    eprintln!(
        "mill program: {} lines, {:.1} s",
        text.lines().count(),
        from_gcode.total_seconds
    );
}

#[test]
fn an_arc_is_read_by_centre_and_by_radius() {
    let program = "G21 G90 G17 G94\nT1 M6\nS1000 M3\nG0 X0 Y0 Z1\nG1 Z0 F100\nG2 X10 Y10 I10 J0\nG3 X0 Y0 R10\nM30\n";
    let motions = interpret(program, Machine::Mill, Point3::new(0.0, 0.0, 10.0)).unwrap();
    let arcs = motions
        .iter()
        .filter(|motion| matches!(motion.kind, MotionKind::Arc { .. }))
        .collect::<Vec<_>>();
    assert_eq!(arcs.len(), 2);
    let MotionKind::Arc { center, clockwise } = arcs[0].kind else {
        unreachable!()
    };
    assert!(clockwise);
    assert_eq!(
        center,
        Point2::new(10.0, 0.0),
        "incremental offsets by default"
    );
    let quarter = std::f64::consts::PI * 10.0 / 2.0;
    assert!((arcs[0].length(Machine::Mill) - quarter).abs() < 1.0e-9);
    assert_eq!(arcs[0].feed, FeedRate::PerMinute(100.0));
    assert_eq!(arcs[0].spindle, Spindle::Rpm(1000.0));
    // Back by radius: the short counter-clockwise quarter from (10, 10) to
    // (0, 0) of radius 10 retraces the same circle about (10, 0).
    let MotionKind::Arc { center, clockwise } = arcs[1].kind else {
        unreachable!()
    };
    assert!(!clockwise);
    assert!(
        (center.x - 10.0).abs() < 1.0e-9 && center.y.abs() < 1.0e-9,
        "{center:?}"
    );
    assert!((arcs[1].length(Machine::Mill) - quarter).abs() < 1.0e-9);
    assert_eq!(arcs[0].line, 6);
    // The helper by itself.
    let c =
        arc_centre_from_radius(Point2::new(0.0, 0.0), Point2::new(10.0, 10.0), 10.0, true).unwrap();
    assert!((c.x - 10.0).abs() < 1.0e-9 && c.y.abs() < 1.0e-9);
    assert!(
        arc_centre_from_radius(Point2::new(0.0, 0.0), Point2::new(30.0, 0.0), 10.0, true).is_none()
    );
}

#[test]
fn a_peck_cycle_expands_into_its_pecks() {
    let program =
        "G21 G90 G17 G94\nT2 M6\nS2000 M3\nG0 X5 Y5 Z10\nG99 G83 X5 Y5 Z-6 R2 Q2 F150\nG80\nM30\n";
    let motions = interpret(program, Machine::Mill, Point3::new(0.0, 0.0, 10.0)).unwrap();
    let feeds = motions
        .iter()
        .filter(|motion| motion.kind == MotionKind::Feed)
        .map(|motion| (motion.from.z, motion.to.z))
        .collect::<Vec<_>>();
    // Three pecks of two from the retract plane at 2: to 0, -2, -4, then -6.
    assert_eq!(
        feeds,
        vec![(2.0, 0.0), (0.5, -2.0), (-1.5, -4.0), (-3.5, -6.0)]
    );
    // Every peck rapids back to the retract plane.
    let retracts = motions
        .iter()
        .filter(|motion| {
            motion.kind == MotionKind::Rapid && motion.to.z == 2.0 && motion.from.z < 2.0
        })
        .count();
    assert_eq!(retracts, 4);
    assert!(motions.iter().all(|motion| motion.tool == 2));
    let last = motions.last().unwrap();
    assert_eq!(last.to, Point3::new(5.0, 5.0, 2.0));
}

#[test]
fn constant_surface_speed_sets_the_clock_by_radius() {
    let program = "G21 G90 G18 G7 G95\nT31 M6\nG96 D3000 S300 M3\nG0 X50 Z2\nG1 X50 Z-20 F0.25\nG1 X10 Z-20\nM30\n";
    let motions = interpret(program, Machine::Lathe, Point3::new(30.0, 0.0, 30.0)).unwrap();
    let cuts = motions
        .iter()
        .filter(|motion| motion.is_cutting())
        .collect::<Vec<_>>();
    assert_eq!(cuts.len(), 2);
    assert_eq!(cuts[0].from, Point3::new(25.0, 0.0, 2.0), "X is a diameter");
    assert_eq!(cuts[0].to, Point3::new(25.0, 0.0, -20.0));
    assert_eq!(
        cuts[0].spindle,
        Spindle::SurfaceSpeed {
            metres_per_minute: 300.0,
            max_rpm: 3000.0
        }
    );
    assert_eq!(cuts[0].feed, FeedRate::PerRevolution(0.25));
    // At Ø50: 300 m/min is 1910 rpm; 22 mm at 0.25 mm/rev takes 22 / 477.5 min.
    let rpm = 300.0 * 1000.0 / (std::f64::consts::PI * 50.0);
    let expected = 22.0 / (0.25 * rpm) * 60.0;
    let seconds = motion_seconds(cuts[0], Machine::Lathe, 5000.0);
    assert!(
        (seconds - expected).abs() < 1.0e-9,
        "{seconds} vs {expected}"
    );
    // Facing inward from Ø50 to Ø10 turns at the mean diameter, Ø30, under
    // the D3000 cap, and says so in the docs: the true time integrates the
    // rising speed.
    let mean_rpm = (300.0 * 1000.0 / (std::f64::consts::PI * 30.0)).min(3000.0);
    let facing = motion_seconds(cuts[1], Machine::Lathe, 5000.0);
    assert!((facing - 20.0 / (0.25 * mean_rpm) * 60.0).abs() < 1.0e-9);
}

#[test]
fn unsupported_words_are_refused_by_line() {
    let program = "G21 G90\nT1 M6\nG0 X0 Y0 Z1\nG91\nG1 X5\n";
    let error = interpret(program, Machine::Mill, Point3::new(0.0, 0.0, 10.0)).unwrap_err();
    assert_eq!(error.line, 4);
    assert!(error.message.contains("incremental"));
    let program = "G21 G90\nG1 X5 F10\n";
    let error = interpret(program, Machine::Mill, Point3::new(0.0, 0.0, 10.0)).unwrap_err();
    assert_eq!(error.line, 2);
}

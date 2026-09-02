//! Sketch relations from the canvas, from the user's report: "we need to
//! get constraints working within the sketch window".
//!
//! Picking a relation tile and clicking geometry did nothing: the click went
//! to plain selection. These drive the tools the way the user does — tile,
//! then geometry — and check that the solver's answer reaches the canvas.

use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{SketchGeometry, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }
    harness.run();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    let position = harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
    click_at(harness, position);
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn draw_line(harness: &mut Harness<'static, KernelLabApp>, start: SketchPoint, end: SketchPoint) {
    click_button(harness, "Single line");
    click_sketch_point(harness, start);
    click_sketch_point(harness, end);
    harness.key_press(egui::Key::Escape);
    harness.run();
}

/// Confirms a staged relation through the rail if the canvas is holding one.
fn confirm_if_staged(harness: &mut Harness<'static, KernelLabApp>) {
    if harness.state().sketch_pending_label().is_some() {
        click_button(harness, "Confirm operation");
    }
    assert_eq!(harness.state().sketch_pending_label(), None);
}

fn segments(harness: &Harness<'static, KernelLabApp>) -> Vec<(SketchPoint, SketchPoint)> {
    harness
        .state()
        .sketch_entity_geometries()
        .into_iter()
        .filter_map(|geometry| match geometry {
            SketchGeometry::Segment { start, end } => Some((start, end)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_horizontal_relation_picked_from_the_tile_levels_the_clicked_line() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_line(
        &mut harness,
        SketchPoint::new(-3.0, -1.0),
        SketchPoint::new(3.0, 1.0),
    );
    let before = segments(&harness);
    assert_eq!(before.len(), 1, "one line drawn");
    assert!((before[0].0.v - before[0].1.v).abs() > 1.0);

    click_button(&mut harness, "Horizontal relation");
    assert_eq!(
        harness.state().active_sketch_tool_label(),
        "Horizontal relation"
    );
    // Mid-span, clear of both endpoints, so the line itself is the operand.
    // A relation applies the moment it is complete, like a drawn stroke,
    // with undo as the safety net.
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    confirm_if_staged(&mut harness);

    let after = segments(&harness);
    assert_eq!(after.len(), 1);
    assert!(
        (after[0].0.v - after[0].1.v).abs() <= 1.0e-9,
        "the line should be level after the relation: {after:?}"
    );
}

#[test]
fn a_perpendicular_relation_squares_two_lines_picked_in_turn() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_line(
        &mut harness,
        SketchPoint::new(-3.0, 0.0),
        SketchPoint::new(3.0, 0.0),
    );
    draw_line(
        &mut harness,
        SketchPoint::new(0.0, 1.0),
        SketchPoint::new(2.0, 3.0),
    );
    assert_eq!(segments(&harness).len(), 2);

    click_button(&mut harness, "Horizontal relation");
    // The chooser reaches the rest of the family; the accessible names are
    // the variants' own.
    click_button(
        &mut harness,
        "Choose relation; current default: Horizontal.",
    );
    click_button(&mut harness, "Perpendicular relation");
    click_sketch_point(&mut harness, SketchPoint::new(-1.5, 0.0));
    let untouched = segments(&harness);
    assert_eq!(
        untouched[0],
        (SketchPoint::new(-3.0, 0.0), SketchPoint::new(3.0, 0.0)),
        "one operand is not yet a relation"
    );
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 2.0));
    confirm_if_staged(&mut harness);

    let after = segments(&harness);
    let direction = |(start, end): (SketchPoint, SketchPoint)| (end.u - start.u, end.v - start.v);
    let first = direction(after[0]);
    let second = direction(after[1]);
    let dot = first.0 * second.0 + first.1 * second.1;
    assert!(
        dot.abs() <= 1.0e-7,
        "the lines should be square, dot = {dot}"
    );
}

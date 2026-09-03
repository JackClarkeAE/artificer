//! Sketch relations from the canvas, from the user's report: "we need to
//! get constraints working within the sketch window", and then "we can't add
//! constraints, none of the relation buttons actually work or show a right
//! panel".
//!
//! Picking a relation tile and clicking geometry did nothing: the click went
//! to plain selection. These drive the tools the way the user does — tile,
//! then geometry — and check that the solver's answer reaches the canvas, that
//! a relation which cannot be made says so, and that the relations a sketch is
//! holding are on screen and can be released.

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
    click_button(harness, "Create sketch");
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

    // Every relation is its own cell beyond the divider: no chooser to open
    // and nothing hidden under the name of the last one used.
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

/// Every relation is on the ribbon in its own right. The set used to be one
/// tile showing whichever was picked last, with nine more behind a chevron,
/// which is why a sketcher looking for "perpendicular" concluded the workbench
/// had no constraints.
#[test]
fn every_relation_is_its_own_button_beside_the_drawing_tools() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    for label in [
        "Horizontal relation",
        "Vertical relation",
        "Coincident relation",
        "Collinear relation",
        "Parallel relation",
        "Perpendicular relation",
        "Tangent relation",
        "Equal-length relation",
        "Distance relation",
        "Fixed relation",
        "Sketch dimension",
    ] {
        assert!(
            harness
                .query_by_role_and_label(Role::Button, label)
                .is_some(),
            "{label} must be reachable without opening a menu"
        );
    }
    assert!(
        harness
            .query_by_role_and_label(
                Role::Button,
                "Choose relation; current default: Horizontal."
            )
            .is_none(),
        "the relation chooser is gone: the whole set is on the ribbon"
    );
}

/// Arming a relation raises the panel that says what it wants, and a pick that
/// names nothing says so rather than looking like a dead button.
#[test]
fn arming_a_relation_shows_the_panel_and_a_missed_pick_explains_itself() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_line(
        &mut harness,
        SketchPoint::new(-3.0, 0.0),
        SketchPoint::new(3.0, 0.0),
    );

    click_button(&mut harness, "Coincident relation");
    harness.run();
    assert!(
        harness.query_by_label("RELATION").is_some(),
        "the relation panel is up while a relation is armed"
    );
    assert!(
        harness
            .query_by_label("Click the first endpoint to make coincident.")
            .is_some(),
        "the panel says what to pick first"
    );

    // Empty canvas, well clear of the drawn line and well inside the viewport.
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 2.0));
    harness.run();
    assert_eq!(
        harness.state().sketch_relation_diagnostic(),
        Some("Nothing to relate here. Click a line or one of its endpoints."),
        "a pick that names nothing explains itself instead of doing nothing"
    );
    assert!(
        harness
            .query_by_label("Nothing to relate here. Click a line or one of its endpoints.")
            .is_some(),
        "and the explanation is on screen, in the panel"
    );
    assert_eq!(
        harness.state().sketch_relation_operand_count(),
        0,
        "a missed pick stages nothing"
    );
}

/// A relation the sketch is holding is listed, and can be released again.
#[test]
fn held_relations_are_listed_and_can_be_released() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_line(
        &mut harness,
        SketchPoint::new(-3.0, -1.0),
        SketchPoint::new(3.0, 1.0),
    );
    click_button(&mut harness, "Horizontal relation");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    confirm_if_staged(&mut harness);
    assert_eq!(harness.state().sketch_constraint_count(), 1);

    harness.run();
    assert!(
        harness.query_by_label("RELATIONS HELD · 1").is_some(),
        "the sketch says what it is holding"
    );
    assert!(
        harness.query_by_label("Horizontal").is_some(),
        "the held relation is named"
    );

    click_button(&mut harness, "Remove relation 1");
    confirm_if_staged(&mut harness);
    assert_eq!(
        harness.state().sketch_constraint_count(),
        0,
        "releasing a relation takes the equation out of the sketch"
    );
    // A relation is a projection over the recipe, not an edit to it, so the
    // line returns to the shape it was drawn with once nothing holds it.
    let after = segments(&harness);
    assert_eq!(
        after[0],
        (SketchPoint::new(-3.0, -1.0), SketchPoint::new(3.0, 1.0)),
        "released geometry goes back to what the recipe says: {after:?}"
    );
}

/// An operand is picked for the relation that was armed when it was clicked.
/// Switching relations mid-pick used to keep it, so the next click completed
/// the new relation from an operand chosen for the old one.
#[test]
fn switching_relations_forgets_the_operands_picked_for_the_last_one() {
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

    click_button(&mut harness, "Perpendicular relation");
    click_sketch_point(&mut harness, SketchPoint::new(-1.5, 0.0));
    assert_eq!(harness.state().sketch_relation_operand_count(), 1);

    click_button(&mut harness, "Parallel relation");
    assert_eq!(
        harness.state().sketch_relation_operand_count(),
        0,
        "the new relation starts from nothing"
    );
    assert_eq!(
        harness.state().sketch_relation_diagnostic(),
        None,
        "and carries no complaint from the last one"
    );

    // One click is one operand, so the parallel relation is still incomplete
    // and nothing has been staged from the abandoned pick.
    click_sketch_point(&mut harness, SketchPoint::new(-1.5, 0.0));
    assert_eq!(harness.state().sketch_constraint_count(), 0);
    assert_eq!(harness.state().sketch_relation_operand_count(), 1);
}

/// The first sketch most people draw is a rectangle, and a rectangle is a
/// recipe: its edges belong to the recipe, so a relation on one is refused by
/// design (ADR 0026's recipe boundary). What was missing was the sentence
/// saying so — the click simply did nothing, which is what "the relation
/// buttons don't work" looked like from the outside.
#[test]
fn a_relation_on_recipe_owned_geometry_says_what_to_pick_instead() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, -1.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 1.0));
    harness.key_press(egui::Key::Escape);
    harness.run();

    click_button(&mut harness, "Horizontal relation");
    // Mid-span of the lower edge, clear of both corners.
    click_sketch_point(&mut harness, SketchPoint::new(0.0, -1.0));
    harness.run();

    let diagnostic = harness
        .state()
        .sketch_relation_diagnostic()
        .expect("a refused relation must say why")
        .to_owned();
    assert!(
        diagnostic.contains("anchor points"),
        "the refusal should point at the route that works: {diagnostic}"
    );
    assert!(
        harness.query_by_label(diagnostic.as_str()).is_some(),
        "and it belongs on screen, not only in the state"
    );
    assert_eq!(harness.state().sketch_constraint_count(), 0);
}

/// Types a new value into the nth listed relation's field.
fn set_relation_value(harness: &mut Harness<'static, KernelLabApp>, row: usize, value: f64) {
    let name = format!("Relation {row} value");
    let field = harness.get_by_role_and_label(Role::TextInput, name.as_str());
    field.click();
    harness.run();
    let field = harness.get_by_role_and_label(Role::TextInput, name.as_str());
    field.type_text(&format!("{value}"));
    harness.key_press(egui::Key::Enter);
    harness.run();
}

/// Returns the centre of the one circle in the sketch.
fn circle_centre(harness: &Harness<'static, KernelLabApp>) -> SketchPoint {
    harness
        .state()
        .sketch_entity_geometries()
        .into_iter()
        .find_map(|geometry| match geometry {
            SketchGeometry::Circle { center, .. } => Some(center),
            _ => None,
        })
        .expect("one circle")
}

/// The dimension a drawer actually reaches for: a hole placed in a plate by
/// measuring from two of its edges, from the user's report — "someone might
/// ensure a circle is centred but offset vertically in a rectangle by using a
/// sketch dimension from the sides of the rectangle to the centre of the
/// circle".
///
/// The rectangle is a recipe, so the solver moves it as a body: the circle
/// travels to meet the dimension and the plate stays a plate.
#[test]
fn a_circle_is_placed_in_a_rectangle_by_dimensioning_from_its_edges() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-4.0, -3.0));
    click_sketch_point(&mut harness, SketchPoint::new(4.0, 3.0));
    harness.key_press(egui::Key::Escape);
    harness.run();

    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(0.5, 0.5));
    click_sketch_point(&mut harness, SketchPoint::new(1.5, 0.5));
    harness.key_press(egui::Key::Escape);
    harness.run();
    let drawn = circle_centre(&harness);
    assert!((drawn.u - 0.5).abs() <= 1.0e-6 && (drawn.v - 0.5).abs() <= 1.0e-6);

    // Dimension: the circle's centre first, then the plate's left edge.
    click_button(&mut harness, "Sketch dimension");
    click_sketch_point(&mut harness, drawn);
    assert_eq!(
        harness.state().sketch_dimension_operand_count(),
        1,
        "the centre is the first operand: {:?}",
        harness.state().sketch_relation_diagnostic()
    );
    // Mid-height of the left edge, clear of both corners.
    click_sketch_point(&mut harness, SketchPoint::new(-4.0, 0.0));
    confirm_if_staged(&mut harness);
    assert_eq!(
        harness.state().sketch_constraint_count(),
        1,
        "the dimension should be held: {:?}",
        harness.state().sketch_relation_diagnostic()
    );

    // It arrives holding what the sketch already showed.
    let held = harness
        .state()
        .sketch_constraint_values()
        .first()
        .copied()
        .expect("the dimension holds a value");
    assert!(
        (held - 4.5).abs() <= 1.0e-6,
        "the dimension measures the offset it was taken at, got {held}"
    );

    // Retyping it drives the circle, and leaves the plate alone.
    let plate_before = segments(&harness);
    click_button(&mut harness, "Select sketch geometry");
    harness.run();
    set_relation_value(&mut harness, 1, 2.0);
    confirm_if_staged(&mut harness);

    let placed = circle_centre(&harness);
    assert!(
        (placed.u - (-2.0)).abs() <= 1.0e-6,
        "the hole should sit 2 from the left edge, got {placed:?}"
    );
    assert!(
        (placed.v - 0.5).abs() <= 1.0e-6,
        "and should not have drifted vertically: {placed:?}"
    );
    assert_eq!(
        segments(&harness),
        plate_before,
        "the plate is the reference and does not move"
    );
}

/// Drags the pointer from `from` to `to` on the canvas.
fn drag_between(harness: &mut Harness<'static, KernelLabApp>, from: egui::Pos2, to: egui::Pos2) {
    harness.hover_at(from);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    for step in 1..=4_u8 {
        let fraction = f32::from(step) / 4.0;
        harness.event(egui::Event::PointerMoved(from + (to - from) * fraction));
        harness.step();
    }
    harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.run();
}

/// A dimension's value is a handle: dragging it moves the label clear of the
/// geometry, and does not pick whatever it was sitting on.
#[test]
fn a_dimension_label_is_dragged_clear_of_the_geometry_it_measures() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_line(
        &mut harness,
        SketchPoint::new(-3.0, 0.0),
        SketchPoint::new(3.0, 0.0),
    );
    draw_line(
        &mut harness,
        SketchPoint::new(-3.0, 2.0),
        SketchPoint::new(3.0, 2.0),
    );

    // Dimension between the two left endpoints, so its label lands mid-way.
    click_button(&mut harness, "Sketch dimension");
    click_sketch_point(&mut harness, SketchPoint::new(-3.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(-3.0, 2.0));
    confirm_if_staged(&mut harness);
    assert_eq!(harness.state().sketch_constraint_count(), 1);

    let placed = *harness
        .state()
        .sketch_dimension_label_positions()
        .first()
        .expect("the dimension has a label");
    assert!(
        (placed.u - (-3.0)).abs() <= 1.0e-6 && (placed.v - 1.0).abs() <= 1.0e-6,
        "the label starts in the middle of what it measures: {placed:?}"
    );

    let geometry_before = segments(&harness);
    let viewport = harness.get_by_label("Sketch viewport").rect();
    let grab = harness
        .state()
        .sketch_point_screen_position(viewport, placed);
    let release = harness
        .state()
        .sketch_point_screen_position(viewport, SketchPoint::new(-5.0, 1.0));
    drag_between(&mut harness, grab, release);

    let moved = *harness
        .state()
        .sketch_dimension_label_positions()
        .first()
        .expect("the dimension still has a label");
    assert!(
        (moved.u - (-5.0)).abs() <= 0.05 && (moved.v - 1.0).abs() <= 0.05,
        "the label should follow the pointer, got {moved:?}"
    );
    assert_eq!(
        segments(&harness),
        geometry_before,
        "dragging a label moves the label, not the drawing"
    );
    assert_eq!(
        harness.state().sketch_constraint_count(),
        1,
        "and stages nothing"
    );
    assert_eq!(
        harness.state().sketch_dimension_operand_count(),
        0,
        "the grab must not read as the first pick of another dimension"
    );
}

/// Where a dimension's value was dragged to is part of the drawing, so it is
/// in the document: a sketch saved, reopened in a fresh process and edited
/// again shows its labels where they were left.
#[test]
fn a_dragged_dimension_label_survives_a_save_and_reopen() {
    let mut source = harness();
    enter_xy_sketch(&mut source);
    draw_line(
        &mut source,
        SketchPoint::new(-3.0, 0.0),
        SketchPoint::new(3.0, 0.0),
    );
    draw_line(
        &mut source,
        SketchPoint::new(-3.0, 2.0),
        SketchPoint::new(3.0, 2.0),
    );
    click_button(&mut source, "Sketch dimension");
    click_sketch_point(&mut source, SketchPoint::new(-3.0, 0.0));
    click_sketch_point(&mut source, SketchPoint::new(-3.0, 2.0));
    confirm_if_staged(&mut source);
    assert_eq!(source.state().sketch_constraint_count(), 1);

    let placed = *source
        .state()
        .sketch_dimension_label_positions()
        .first()
        .expect("the dimension has a label");
    let viewport = source.get_by_label("Sketch viewport").rect();
    let grab = source
        .state()
        .sketch_point_screen_position(viewport, placed);
    let release = source
        .state()
        .sketch_point_screen_position(viewport, SketchPoint::new(-5.5, 1.0));
    drag_between(&mut source, grab, release);
    let moved = *source
        .state()
        .sketch_dimension_label_positions()
        .first()
        .expect("the label is still there");
    assert!((moved.u - (-5.5)).abs() <= 0.05, "dragged: {moved:?}");

    click_button(&mut source, "Finish sketch");
    assert_eq!(source.state().workbench_mode(), WorkbenchMode::Model);
    let saved = source.state().native_document_json().expect("save");

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("the sketch should hydrate in a fresh process");
    restored.run();
    click_button(&mut restored, "Sketch 1 feature");
    assert_eq!(restored.state().workbench_mode(), WorkbenchMode::Sketch);

    assert_eq!(
        restored.state().sketch_constraint_count(),
        1,
        "the dimension came back"
    );
    let reopened = *restored
        .state()
        .sketch_dimension_label_positions()
        .first()
        .expect("and so did its label");
    assert!(
        (reopened.u - moved.u).abs() <= 1.0e-6 && (reopened.v - moved.v).abs() <= 1.0e-6,
        "the label should reopen where it was left: {reopened:?} against {moved:?}"
    );
}

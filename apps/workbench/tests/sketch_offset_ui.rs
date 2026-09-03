//! The Offset tool, from the tile to committed geometry.

use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{SketchGeometry, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

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
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    click_at(harness, canvas_sketch_point(harness, point));
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Create sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

/// An 8 × 6 rectangle centred on the origin, committed. One presentation
/// entity; four curves in the sketch behind it, which is what the chain walks.
fn draw_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Two-point rectangle");
    click_sketch_point(harness, SketchPoint::new(-4.0, -3.0));
    click_sketch_point(harness, SketchPoint::new(4.0, 3.0));
    assert_eq!(harness.state().sketch_entity_count(), 1);
}

/// The lowest point any offset line reaches. The rectangle itself is one
/// composite entity, so every segment here came from the offset.
fn lowest_offset_edge(harness: &Harness<'static, KernelLabApp>) -> f64 {
    segments(harness)
        .iter()
        .flat_map(|(start, end)| [start.v, end.v])
        .fold(f64::INFINITY, f64::min)
}

/// How far down the whole drawing reaches, whatever kind of curve gets there.
///
/// Counting entities does not survive an undo: replaying a composite like a
/// rectangle explodes it into the exact curves it was always made of, so the
/// tally changes without the drawing changing. Where the geometry ends does
/// not move for that reason.
fn lowest_point(harness: &Harness<'static, KernelLabApp>) -> f64 {
    harness
        .state()
        .sketch_entity_geometries()
        .into_iter()
        .flat_map(|geometry| match geometry {
            SketchGeometry::Point(point) => vec![point.v],
            SketchGeometry::Segment { start, end } => vec![start.v, end.v],
            SketchGeometry::Rectangle { first, opposite } => vec![first.v, opposite.v],
            SketchGeometry::Circle { center, rim } => {
                vec![center.v - center.distance_squared(rim).sqrt()]
            }
            SketchGeometry::Arc { start, end, .. } => vec![start.v, end.v],
        })
        .fold(f64::INFINITY, f64::min)
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
fn one_click_offsets_the_whole_outline_to_the_side_it_was_clicked_from() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);

    click_button(&mut harness, "Offset chain");
    assert_eq!(harness.state().active_sketch_tool_label(), "Offset chain");

    // Click the bottom side from outside the rectangle. One click takes the
    // whole connected outline and offsets it away from the pointer's side.
    let below = canvas_sketch_point(&harness, SketchPoint::new(0.0, -3.0)) + egui::vec2(0.0, 6.0);
    click_at(&mut harness, below);
    // A sketch stroke commits on acceptance (ADR 0027), and an offset is one.
    assert!(!harness.state().operation_confirmation_pending());

    // Four sides, beside the rectangle they came from. A rectangle offsets to
    // a rectangle: every corner is where the two offset sides meet, so nothing
    // is added to the topology of the parent.
    assert_eq!(harness.state().sketch_entity_count(), 5);
    assert_eq!(segments(&harness).len(), 4, "four sides and no corner arcs");
    let lowest = lowest_offset_edge(&harness);
    assert!(
        (lowest + 4.0).abs() < 1.0e-6,
        "an outward offset of 1 mm puts the lowest side at v = -4, not {lowest}"
    );
}

#[test]
fn clicking_from_inside_offsets_inward_instead() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);

    click_button(&mut harness, "Offset chain");
    let above = canvas_sketch_point(&harness, SketchPoint::new(0.0, -3.0)) + egui::vec2(0.0, -6.0);
    click_at(&mut harness, above);

    // Inward, every corner is concave and trims to a sharp meeting, so the
    // result is four sides and no join arcs at all.
    assert_eq!(harness.state().sketch_entity_count(), 5);
    assert_eq!(segments(&harness).len(), 4);
    let lowest = lowest_offset_edge(&harness);
    assert!(
        (lowest + 2.0).abs() < 1.0e-6,
        "an inward offset of 1 mm puts the lowest side at v = -2, not {lowest}"
    );
}

#[test]
fn the_offset_distance_is_a_typed_field_that_drives_the_result() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);
    click_button(&mut harness, "Offset chain");

    harness
        .get_by_role_and_label(Role::TextInput, "Offset distance")
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Offset distance")
        .type_text("3");
    harness.run();

    let below = canvas_sketch_point(&harness, SketchPoint::new(0.0, -3.0)) + egui::vec2(0.0, 6.0);
    click_at(&mut harness, below);

    let lowest = lowest_offset_edge(&harness);
    assert!(
        (lowest + 6.0).abs() < 1.0e-6,
        "a typed 3 mm offset puts the lowest side at v = -6, not {lowest}"
    );
}

#[test]
fn a_click_on_empty_canvas_stages_nothing_at_all() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);
    let before = harness.state().sketch_entity_count();

    click_button(&mut harness, "Offset chain");
    click_sketch_point(&mut harness, SketchPoint::new(12.0, 12.0));

    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_entity_count(), before);
    assert_eq!(
        harness.state().active_sketch_tool_label(),
        "Offset chain",
        "a miss leaves the tool armed for the next try"
    );
}

#[test]
fn an_offset_undoes_as_one_thing_and_can_itself_be_offset() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    // Smaller than the shared fixture, so two offsets still fit the canvas.
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-3.0, -2.0));
    click_sketch_point(&mut harness, SketchPoint::new(3.0, 2.0));
    let before = harness.state().sketch_entity_count();

    click_button(&mut harness, "Offset chain");
    let below = canvas_sketch_point(&harness, SketchPoint::new(0.0, -2.0)) + egui::vec2(0.0, 6.0);
    click_at(&mut harness, below);
    assert_eq!(harness.state().sketch_entity_count(), before + 4);

    // The offset's own curves are profile geometry, so the chain walk finds
    // them and the tool works on its own output — the case that catches an
    // offset whose result cannot be offset again. Take its top edge, clear of
    // the dimension chips the last stroke's selection drew below.
    let above = canvas_sketch_point(&harness, SketchPoint::new(0.0, 3.0)) + egui::vec2(0.0, -6.0);
    click_at(&mut harness, above);
    assert_eq!(harness.state().sketch_entity_count(), before + 8);

    // Each is one edit, undone whole. Escape first closes the live dimension
    // readout the stroke left open, which owns the keyboard while it is up.
    // Where the drawing ends is the measure, not how many entities it holds:
    // an undo replays a composite rectangle as the four curves it always was.
    assert!((lowest_point(&harness) + 4.0).abs() < 1.0e-6);
    press_key(&mut harness, egui::Key::Escape);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert!(
        (lowest_point(&harness) + 3.0).abs() < 1.0e-6,
        "one undo takes back the second offset and nothing else"
    );
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert!(
        (lowest_point(&harness) + 2.0).abs() < 1.0e-6,
        "the second takes back the first, leaving the rectangle"
    );
}

#[test]
fn hovering_a_side_lights_the_whole_chain_a_click_would_take() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);
    click_button(&mut harness, "Offset chain");

    // Nothing under the pointer, nothing lit.
    harness.hover_at(canvas_sketch_point(&harness, SketchPoint::new(12.0, 12.0)));
    harness.step();
    assert_eq!(harness.state().sketch_offset_hover_count(), 0);

    // One side under it, and the whole outline lights: what "the connected
    // chain" means is a claim about geometry the user cannot check by looking
    // at one curve, so the tool shows it before the click.
    harness.hover_at(canvas_sketch_point(&harness, SketchPoint::new(0.0, -3.0)));
    harness.step();
    assert_eq!(harness.state().sketch_offset_hover_count(), 4);

    // And moving off it again puts the highlight away.
    harness.hover_at(canvas_sketch_point(&harness, SketchPoint::new(12.0, 12.0)));
    harness.step();
    assert_eq!(harness.state().sketch_offset_hover_count(), 0);
}

#[test]
fn the_offset_tile_is_live_and_says_what_it_does() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    let tile = harness.get_by_role_and_label(Role::Button, "Offset chain");
    assert!(!tile.accesskit_node().is_disabled());
}

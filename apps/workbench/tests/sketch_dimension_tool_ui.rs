//! The Dimension tool, from the user's report: "sketch dimension does not seem
//! to work as a tool or if it does it gives no visual indication of it".
//!
//! With the tool active, clicking a curve arms its driving dimensions as real
//! fields on the canvas, seeded with the exact literal the recipe replays. The
//! hard part is that a rectangle authors one recipe but does not stay one
//! presentation entity — so these tests pin the tool across the first edit and
//! across a document round trip, where it used to go silent.

use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const WIDTH_BOX: &str = "Rectangle width";
const HEIGHT_BOX: &str = "Rectangle height";
const DIAMETER_BOX: &str = "Circle diameter";
const LENGTH_BOX: &str = "Line length";

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

fn create_two_point_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Two-point rectangle");
    click_sketch_point(harness, SketchPoint::new(-2.0, -1.0));
    click_sketch_point(harness, SketchPoint::new(2.0, 1.0));
    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed { .. } | CertifiedProfileStatus::ClosedRegions { .. }
    ));
}

fn arm_dimension_tool(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Sketch dimension");
}

/// Type into whichever dimension box currently holds the caret.
fn type_into_armed_box(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .is_focused(),
        "{label} should hold the caret"
    );
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn rectangle_width(harness: &Harness<'static, KernelLabApp>) -> String {
    harness
        .state()
        .selected_sketch_recipe_editor()
        .expect("a rectangle side is selected")
        .parameters[0]
        .text
        .clone()
}

/// The reported gesture, end to end: press D, click the rectangle, type, Enter.
#[test]
fn dimension_pick_arms_the_caret_on_the_first_driving_box() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    arm_dimension_tool(&mut harness);

    let revision = harness.state().sketch_revision();
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 1.0));
    // Both driving dimensions are real fields, and the first one is armed.
    harness.get_by_role_and_label(Role::TextInput, HEIGHT_BOX);
    type_into_armed_box(&mut harness, WIDTH_BOX, "6");
    assert_eq!(harness.state().sketch_pending_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), revision);

    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(harness.state().sketch_revision(), revision + 1);
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(rectangle_width(&harness), "6");
}

/// The picked side names the dimension: clicking a vertical wall of the
/// rectangle arms Height, not whichever field happened to come first. This is
/// the reported confusion — dimensioning one side used to bring the whole
/// recipe back up with Width always holding the caret.
#[test]
fn clicking_a_vertical_side_arms_height() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    arm_dimension_tool(&mut harness);

    click_sketch_point(&mut harness, SketchPoint::new(2.0, 0.0));
    type_into_armed_box(&mut harness, HEIGHT_BOX, "3");
    harness.key_press(egui::Key::Enter);
    harness.run();
    let height = harness
        .state()
        .selected_sketch_recipe_editor()
        .expect("a rectangle side is selected")
        .parameters[1]
        .text
        .clone();
    assert_eq!(height, "3");
}

/// The chip registered by `semantic_selection_targets` sits over the canvas and
/// takes the click outright. It has to arm the tool too, or a pick that lands
/// on it selects and does nothing else.
#[test]
fn dimension_pick_on_the_semantic_chip_also_arms() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    arm_dimension_tool(&mut harness);

    // The rectangle chip sits at the midpoint of its min_v edge.
    click_sketch_point(&mut harness, SketchPoint::new(0.0, -1.0));
    type_into_armed_box(&mut harness, WIDTH_BOX, "5");
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(rectangle_width(&harness), "5");
}

/// The regression that made the tool useless in practice: committing an edit
/// explodes the rectangle's presentation into four segments, and measuring the
/// picked segment would offer Line length where the recipe says Width.
#[test]
fn rectangle_stays_dimensionable_after_its_first_canvas_edit() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    arm_dimension_tool(&mut harness);

    click_sketch_point(&mut harness, SketchPoint::new(0.0, 1.0));
    type_into_armed_box(&mut harness, WIDTH_BOX, "6");
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(harness.state().sketch_entity_count(), 4);

    click_sketch_point(&mut harness, SketchPoint::new(0.0, 1.0));
    type_into_armed_box(&mut harness, WIDTH_BOX, "7");
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(rectangle_width(&harness), "7");
}

/// Opening a saved part and dimensioning it is the commoner path, and it hits
/// the same explode: hydration builds one entity per exact curve.
#[test]
fn reloaded_rectangle_is_dimensionable() {
    let mut source = harness();
    enter_xy_sketch(&mut source);
    create_two_point_rectangle(&mut source);
    click_button(&mut source, "Finish sketch");
    let saved = source.state().native_document_json().unwrap();

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("the saved sketch should hydrate");
    restored.run();
    click_button(&mut restored, "Sketch 1 feature");
    assert_eq!(restored.state().workbench_mode(), WorkbenchMode::Sketch);
    assert_eq!(restored.state().sketch_entity_count(), 4);

    arm_dimension_tool(&mut restored);
    click_sketch_point(&mut restored, SketchPoint::new(0.0, 1.0));
    type_into_armed_box(&mut restored, WIDTH_BOX, "6");
    restored.key_press(egui::Key::Enter);
    restored.run();
    assert_eq!(rectangle_width(&restored), "6");
}

/// Escape reverts the typed value and leaves the sketch exactly as it was.
#[test]
fn escape_reverts_an_on_canvas_dimension() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    arm_dimension_tool(&mut harness);

    let revision = harness.state().sketch_revision();
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 1.0));
    type_into_armed_box(&mut harness, WIDTH_BOX, "6");
    assert_eq!(harness.state().sketch_pending_entity_count(), 4);

    harness.key_press(egui::Key::Escape);
    harness.run();
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), revision);
    assert_eq!(rectangle_width(&harness), "4");
}

/// A circle drives one literal, and the box must survive its own keystroke:
/// its candidate is a single curve, which used to win the layout race and
/// replace the focused field with a read-only label.
#[test]
fn circle_diameter_edits_on_the_canvas_without_losing_the_caret() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 0.0));
    arm_dimension_tool(&mut harness);

    click_sketch_point(&mut harness, SketchPoint::new(2.0, 0.0));
    type_into_armed_box(&mut harness, DIAMETER_BOX, "6");
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, DIAMETER_BOX)
            .is_focused(),
        "the diameter box keeps the caret while its candidate previews"
    );

    harness.key_press(egui::Key::Enter);
    harness.run();
    assert_eq!(
        harness
            .state()
            .selected_sketch_recipe_editor()
            .expect("the circle stays selected")
            .parameters[0]
            .text,
        "6"
    );
}

/// A line stores two points, so its length and angle are derived on the way out
/// and turned back into an end point on the way in. Driving the length has to
/// move the end and leave the start where it was.
#[test]
fn line_length_is_driven_and_moves_only_the_end_point() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Single line");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 0.0));
    arm_dimension_tool(&mut harness);

    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, LENGTH_BOX)
            .is_some(),
        "clicking a line with the dimension tool must offer its length to type into"
    );
    let editor = harness
        .state()
        .selected_sketch_recipe_editor()
        .expect("the line stays selected");
    let keys = editor
        .parameters
        .iter()
        .map(|parameter| parameter.stable_key)
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec!["length", "angle"],
        "a line is drivable by exactly the two numbers that define it"
    );
}

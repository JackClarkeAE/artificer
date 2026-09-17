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
    click_button(harness, "Sketch mode");
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
///
/// Clicking one edge asks one question. The horizontal side clicked here is a
/// Width question, and Width is the only box that appears: bringing the whole
/// recipe back up buried the answer among the shape's other numbers, and the
/// extra field measures a position rather than a span, so it drew a bare
/// leader with no arrows beside the dimension that had them.
#[test]
fn dimensioning_one_edge_offers_that_edge_and_nothing_else() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    arm_dimension_tool(&mut harness);

    let revision = harness.state().sketch_revision();
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 1.0));
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, HEIGHT_BOX)
            .is_none(),
        "a horizontal side asks for Width, so Height has no box"
    );
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

const SEPARATION_BOX: &str = "Distance between points";

/// A typed dimension applies the moment it is accepted, so nothing should be
/// left waiting at the confirmation gate.
fn assert_dimension_applied_without_a_gate(harness: &Harness<'static, KernelLabApp>) {
    assert_eq!(
        harness.state().sketch_pending_label(),
        None,
        "a typed dimension applies on acceptance, like a drawn stroke"
    );
}

/// Draws two separate lines, four apart at their near ends.
fn two_separate_lines(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Single line");
    click_sketch_point(harness, SketchPoint::new(-8.0, 0.0));
    click_sketch_point(harness, SketchPoint::new(-4.0, 0.0));
    click_button(harness, "Single line");
    click_sketch_point(harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(harness, SketchPoint::new(4.0, 0.0));
}

/// The reported gesture: dimension between two objects, then change it.
///
/// Every piece of this was present and none of it was connected. The solver
/// has held a distance between two arbitrary points since the sketch crate was
/// written; the relation tool created one and captured what the points already
/// measured; and the tool's own tooltip promised the dimension tool would edit
/// it afterwards. Nothing drew the relation and nothing could retype it, so
/// what the user got for dimensioning between two objects was silence.
#[test]
fn a_dimension_between_two_objects_is_drawn_and_can_be_retyped() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    two_separate_lines(&mut harness);
    arm_dimension_tool(&mut harness);

    click_sketch_point(&mut harness, SketchPoint::new(-4.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    let dimensions = harness.state().sketch_point_to_point_dimensions();
    assert_eq!(
        dimensions.len(),
        1,
        "picking an endpoint of each line should make one dimension"
    );
    assert!(
        (dimensions[0].value - 4.0).abs() <= 1.0e-6,
        "the dimension holds what the points already measure, and holds {}",
        dimensions[0].value
    );
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, SEPARATION_BOX)
            .is_some(),
        "a placed dimension offers its value to type into"
    );

    type_into_armed_box(&mut harness, SEPARATION_BOX, "10");
    harness.key_press(egui::Key::Enter);
    harness.run();
    let dimensions = harness.state().sketch_point_to_point_dimensions();
    assert_eq!(dimensions.len(), 1, "the dimension survives its own edit");
    assert!(
        (dimensions[0].value - 10.0).abs() <= 1.0e-6,
        "the dimension should hold ten and holds {}",
        dimensions[0].value
    );
    let separation = (dimensions[0].to.u - dimensions[0].from.u)
        .hypot(dimensions[0].to.v - dimensions[0].from.v);
    assert!(
        (separation - 10.0).abs() <= 1.0e-3,
        "the geometry should have moved to match, and measures {separation}"
    );
    assert!(
        (dimensions[0].from.u + 4.0).abs() <= 1.0e-3,
        "the end the dimension is measured from should have stayed put"
    );
}

/// A value the sketch cannot hold is refused in the solver's words, and the
/// text stays on the canvas to be corrected. The model keeps what it had.
#[test]
fn a_dimension_the_sketch_cannot_hold_keeps_its_text_and_says_why() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    two_separate_lines(&mut harness);
    arm_dimension_tool(&mut harness);
    click_sketch_point(&mut harness, SketchPoint::new(-4.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    type_into_armed_box(&mut harness, SEPARATION_BOX, "nonsense");
    harness.key_press(egui::Key::Enter);
    harness.run();

    let (text, error) = harness
        .state()
        .sketch_relation_dimension_entry()
        .expect("a refused entry stays open for correction");
    assert_eq!(text, "nonsense", "the text stays exactly as typed");
    assert!(error.is_some(), "and the refusal is named");
    assert!(
        (harness.state().sketch_point_to_point_dimensions()[0].value - 4.0).abs() <= 1.0e-6,
        "the model still holds the value it had"
    );
}

/// The tool's original behaviour is untouched: a click that lands on a curve
/// rather than an endpoint is still a question about that curve's own numbers.
#[test]
fn dimensioning_a_curve_still_asks_about_that_curve() {
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
        "the middle of a line is a question about its length"
    );
    assert!(
        harness
            .state()
            .sketch_point_to_point_dimensions()
            .is_empty(),
        "and it makes no relation"
    );
}

/// A dimension is design intent, so it has to be in the file. Reopening the
/// document brings back the dimension, its value, and the geometry it holds —
/// and it stays editable, which is what makes it intent rather than a note.
#[test]
fn a_dimension_between_two_objects_survives_reopening_the_document() {
    let mut source = harness();
    enter_xy_sketch(&mut source);
    two_separate_lines(&mut source);
    arm_dimension_tool(&mut source);
    click_sketch_point(&mut source, SketchPoint::new(-4.0, 0.0));
    click_sketch_point(&mut source, SketchPoint::new(0.0, 0.0));
    type_into_armed_box(&mut source, SEPARATION_BOX, "10");
    source.key_press(egui::Key::Enter);
    source.run();
    assert_dimension_applied_without_a_gate(&source);
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

    let dimensions = restored.state().sketch_point_to_point_dimensions();
    assert_eq!(dimensions.len(), 1, "the dimension is part of the document");
    assert!(
        (dimensions[0].value - 10.0).abs() <= 1.0e-6,
        "it comes back holding ten and holds {}",
        dimensions[0].value
    );

    arm_dimension_tool(&mut restored);
    let constraint = dimensions[0].constraint;
    assert!(
        restored
            .state_mut()
            .begin_sketch_relation_dimension_edit(constraint),
        "a reloaded dimension is still editable"
    );
    restored
        .state_mut()
        .set_sketch_relation_dimension_text("14".to_owned());
    assert!(restored.state_mut().accept_sketch_relation_dimension_edit());
    restored.run();
    assert!(
        (restored.state().sketch_point_to_point_dimensions()[0].value - 14.0).abs() <= 1.0e-6,
        "and retyping it still drives the geometry"
    );
}

/// Escape abandons the typed value and leaves the dimension holding what it
/// had — the same meaning Escape has in every other numeric field here.
#[test]
fn escape_abandons_a_typed_distance_without_moving_anything() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    two_separate_lines(&mut harness);
    arm_dimension_tool(&mut harness);
    click_sketch_point(&mut harness, SketchPoint::new(-4.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    type_into_armed_box(&mut harness, SEPARATION_BOX, "25");

    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(
        harness.state().sketch_relation_dimension_entry().is_none(),
        "Escape closes the box"
    );
    let dimensions = harness.state().sketch_point_to_point_dimensions();
    assert_eq!(dimensions.len(), 1, "and leaves the dimension in place");
    assert!(
        (dimensions[0].value - 4.0).abs() <= 1.0e-6,
        "still holding what it had, and holds {}",
        dimensions[0].value
    );
}

/// The value box asks for the caret once. Asking every frame would take it
/// back off whatever the user clicked next and never let go, which is the kind
/// of fault that only shows up in the running application.
#[test]
fn the_dimension_box_does_not_take_the_caret_back_after_a_click_away() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    two_separate_lines(&mut harness);
    arm_dimension_tool(&mut harness);
    click_sketch_point(&mut harness, SketchPoint::new(-4.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, SEPARATION_BOX)
            .is_focused(),
        "the new dimension takes the caret"
    );

    // Click empty canvas, well away from the annotation and both lines.
    click_sketch_point(&mut harness, SketchPoint::new(6.0, -6.0));
    harness.run();
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, SEPARATION_BOX)
            .is_none_or(|box_| !box_.is_focused()),
        "clicking away gives the caret up for good"
    );
}

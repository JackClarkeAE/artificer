//! The Parametric Design tab, from the user's report: the variables system
//! was "built in early but didn't really work on again". These tests pin the
//! full story — create a variable from the ribbon, rename it, give it a value
//! or an expression in the Variables panel, and drive a sketch dimension with
//! it by name.

use artificer_workbench::{KernelLabApp, WorkbenchMode, sketch::SketchPoint};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const CONFIRM_OPERATION: &str = "Confirm operation";

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
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

fn replace_text_input(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .click();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();
}

fn create_length_variable(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Parametric ribbon tab");
    click_button(harness, "New length variable");
    click_button(harness, CONFIRM_OPERATION);
    assert_eq!(
        harness
            .state()
            .evaluated_variable_values()
            .get("Length1")
            .map(|value| value.canonical),
        Some(10.0),
        "a confirmed new length starts at its 10 mm default"
    );
}

/// Ribbon → variable → value: the panel's value field accepts a number and
/// an expression over another variable, each staged through the universal
/// confirmation gate.
#[test]
fn variables_are_created_valued_and_derived_through_the_panel() {
    let mut harness = harness();
    harness.run();
    create_length_variable(&mut harness);

    // The panel opened with the creation; retype its value.
    replace_text_input(&mut harness, "Variable value Length1", "25");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness
            .state()
            .evaluated_variable_values()
            .get("Length1")
            .map(|value| value.canonical),
        Some(25.0)
    );

    // A second variable derived from the first, with units in the entry.
    click_button(&mut harness, "New length variable");
    click_button(&mut harness, CONFIRM_OPERATION);
    replace_text_input(&mut harness, "Variable value Length2", "Length1 * 2 + 5mm");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness
            .state()
            .evaluated_variable_values()
            .get("Length2")
            .map(|value| value.canonical),
        Some(55.0),
        "expressions evaluate through the typed parameter table"
    );

    // Renaming the source re-renders the derived expression by its new name
    // and keeps evaluating: references are by identity, not by text.
    replace_text_input(&mut harness, "Variable name Length1", "depth");
    let values = harness.state().evaluated_variable_values();
    assert_eq!(values.get("depth").map(|value| value.canonical), Some(25.0));
    assert_eq!(
        values.get("Length2").map(|value| value.canonical),
        Some(55.0)
    );
}

/// The point of the whole feature: a sketch dimension driven by a variable's
/// name. Draw a rectangle, arm the Dimension tool on a side, and type
/// arithmetic over the document variable into the box.
#[test]
fn a_sketch_dimension_accepts_a_variable_expression() {
    let mut harness = harness();
    harness.run();
    create_length_variable(&mut harness);
    replace_text_input(&mut harness, "Variable name Length1", "depth");

    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(&mut harness, "Two-point rectangle");
    for point in [SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)] {
        let position = harness
            .state()
            .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
        click_at(&mut harness, position);
    }
    click_button(&mut harness, "Sketch dimension");
    let top = harness.state().sketch_point_screen_position(
        harness.get_by_label("Sketch viewport").rect(),
        SketchPoint::new(0.0, 1.0),
    );
    click_at(&mut harness, top);

    let width_box = harness.get_by_role_and_label(Role::TextInput, "Rectangle width");
    assert!(width_box.is_focused(), "the pick arms the width box");
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Rectangle width")
        .type_text("depth * 2");
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();

    let width = harness
        .state()
        .selected_sketch_recipe_editor()
        .expect("the rectangle stays selected")
        .parameters[0]
        .text
        .clone();
    assert_eq!(
        width, "20",
        "depth * 2 with depth = 10 mm must commit 20 mm"
    );
}

/// An angle variable is an angle wherever it is typed. A new angle variable
/// is 45°; typed by name into a line's angle box it must give 45°, not the
/// 0.785 radians it is stored as.
#[test]
fn an_angle_variable_fills_a_sketch_angle_box_in_degrees() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "Parametric ribbon tab");
    click_button(&mut harness, "New angle variable");
    click_button(&mut harness, CONFIRM_OPERATION);
    replace_text_input(&mut harness, "Variable name Angle1", "tilt");
    let tilt = harness.state().evaluated_variable_values()["tilt"];
    assert!((tilt.canonical - 45.0_f64.to_radians()).abs() < 1.0e-12);

    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(&mut harness, "Single line");
    assert_eq!(harness.state().active_sketch_tool_label(), "Single line");
    let viewport = harness.get_by_label("Sketch viewport").rect();
    let start = harness
        .state()
        .sketch_point_screen_position(viewport, SketchPoint::new(0.0, 0.0));
    click_at(&mut harness, start);
    let towards = harness
        .state()
        .sketch_point_screen_position(viewport, SketchPoint::new(2.0, 0.5));
    harness.hover_at(towards);
    harness.step();
    harness.step();

    // Tab walks the draft's boxes: the length first, then the angle.
    for (label, value) in [("Line length", "3"), ("Line angle", "tilt")] {
        harness.key_press(egui::Key::Tab);
        harness.run();
        let field = harness.get_by_role_and_label(Role::TextInput, label);
        assert!(field.is_focused(), "{label} should hold the caret");
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness
            .get_by_role_and_label(Role::TextInput, label)
            .type_text(value);
        harness.run();
    }
    assert_eq!(harness.state().sketch_dimension_error(), None);
    let angle = harness
        .state()
        .sketch_dimension_readouts()
        .into_iter()
        .find(|readout| {
            readout.kind == artificer_workbench::sketch::SketchDimensionKind::AngleDegrees
        })
        .expect("the line draft shows its angle");
    assert!(
        (angle.value - 45.0).abs() < 1.0e-9,
        "tilt must read as 45°, got {}",
        angle.value
    );
}

/// An extrusion's distance typed as a variable stays linked to it: the body
/// follows the variable when it changes, and the link is in the file.
#[test]
fn an_extrusion_typed_as_a_variable_follows_it() {
    let mut harness = harness();
    harness.run();
    create_length_variable(&mut harness);
    replace_text_input(&mut harness, "Variable name Length1", "length");
    replace_text_input(&mut harness, "Variable value length", "25");
    click_button(&mut harness, CONFIRM_OPERATION);

    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Sketch mode");
    click_button(&mut harness, "Two-point rectangle");
    for point in [SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)] {
        let position = harness
            .state()
            .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
        click_at(&mut harness, position);
    }
    click_button(&mut harness, "Extrude");
    replace_text_input(&mut harness, "Extrusion distance expression", "length");
    // Tab leaves the field, which reads it; Enter would also confirm.
    harness.key_press(egui::Key::Tab);
    harness.run();
    assert_eq!(
        harness.state().extrusion_distance_follows(),
        Some("length"),
        "status: {:?}",
        harness.state().document_status_text()
    );
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let area = 4.0 * 2.0;
    let volume = |harness: &Harness<'static, KernelLabApp>| {
        harness
            .state()
            .displayed_measures()
            .expect("the extrusion measures")
            .volume
    };
    assert!(
        (volume(&harness) - area * 25.0).abs() < 1.0e-6,
        "{}",
        volume(&harness)
    );

    // The variable changes; the extrusion follows.
    click_button(&mut harness, "Parametric ribbon tab");
    if harness
        .query_by_role_and_label(Role::TextInput, "Variable value length")
        .is_none()
    {
        click_button(&mut harness, "Variables");
    }
    replace_text_input(&mut harness, "Variable value length", "40");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(
        (volume(&harness) - area * 40.0).abs() < 1.0e-6,
        "{}",
        volume(&harness)
    );

    // The file keeps the link: reopened, it still follows.
    let saved = harness.state().native_document_json().unwrap();
    assert!(saved.contains("distance_expression"));
    let mut reopened = self::harness();
    reopened.run();
    reopened
        .state_mut()
        .load_native_document_json(&saved)
        .expect("the linked document opens");
    reopened.run();
    assert!(reopened.state().features_suppressed_on_open().is_empty());
    assert!(
        (volume(&reopened) - area * 40.0).abs() < 1.0e-6,
        "{}",
        volume(&reopened)
    );
}

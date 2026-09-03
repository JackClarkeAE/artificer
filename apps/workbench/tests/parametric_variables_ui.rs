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
            .copied(),
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
            .copied(),
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
            .copied(),
        Some(55.0),
        "expressions evaluate through the typed parameter table"
    );

    // Renaming the source re-renders the derived expression by its new name
    // and keeps evaluating: references are by identity, not by text.
    replace_text_input(&mut harness, "Variable name Length1", "depth");
    let values = harness.state().evaluated_variable_values();
    assert_eq!(values.get("depth").copied(), Some(25.0));
    assert_eq!(values.get("Length2").copied(), Some(55.0));
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
    // Creating the variable left the ribbon on its own tab; the sketch command
    // lives on the Model one.
    click_button(&mut harness, "Model ribbon tab");
    click_button(&mut harness, "Create sketch");
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

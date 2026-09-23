//! The Parametric Design tab, from the user's report: the variables system
//! was "built in early but didn't really work on again". These tests pin the
//! full story — create a variable from the ribbon, rename it, give it a value
//! or an expression in the Variables panel, and drive a sketch dimension with
//! it by name.

use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{SketchGeometry, SketchPoint},
};
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
/// arithmetic over the document variable into the box. The dimension keeps
/// the entry and follows the variable (ADR 0054): extruded, the body is
/// rebuilt when the variable changes, and one undo takes both back.
#[test]
fn a_sketch_dimension_follows_the_variable_it_is_typed_over() {
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
        .clone();
    assert_eq!(width.text, "depth * 2", "the field shows what it follows");
    assert!(width.follows_variables);
    // The sketch's extent: a rectangle is shown whole when drawn and side by
    // side once it has been rebuilt.
    let spans = |harness: &Harness<'static, KernelLabApp>| {
        let points = harness
            .state()
            .sketch_entity_geometries()
            .into_iter()
            .flat_map(|geometry| match geometry {
                SketchGeometry::Rectangle { first, opposite } => vec![first, opposite],
                SketchGeometry::Segment { start, end } => vec![start, end],
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();
        assert!(!points.is_empty(), "the rectangle");
        let span = |coordinate: fn(&SketchPoint) -> f64| {
            let values = points.iter().map(coordinate);
            values.clone().fold(f64::MIN, f64::max) - values.fold(f64::MAX, f64::min)
        };
        (span(|point| point.u), span(|point| point.v))
    };
    let (u, v) = spans(&harness);
    assert!(
        (u - 20.0).abs() < 1.0e-9 && (v - 2.0).abs() < 1.0e-9,
        "depth * 2 with depth = 10 mm is 20 mm: {u} × {v}"
    );

    // Extruded, the body is the rectangle's size.
    click_button(&mut harness, "Extrude");
    let extrusion = harness
        .get_by_role_and_label(Role::TextInput, "Extrusion distance expression")
        .rect()
        .center();
    click_at(&mut harness, extrusion);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Extrusion distance expression")
        .type_text("5");
    harness.key_press(egui::Key::Tab);
    harness.run();
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let volume =
        |harness: &Harness<'static, KernelLabApp>| harness.state().mass_properties().volume;
    assert!(
        (volume(&harness) - 20.0 * 2.0 * 5.0).abs() < 1.0e-6,
        "{}",
        volume(&harness)
    );

    // Changing the variable rebuilds the sketch and the body from it.
    click_button(&mut harness, "Parametric ribbon tab");
    replace_text_input(&mut harness, "Variable value depth", "15");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(
        (volume(&harness) - 30.0 * 2.0 * 5.0).abs() < 1.0e-6,
        "{} · {:?}",
        volume(&harness),
        harness.state().document_status_text()
    );
    let (u, _) = spans(&harness);
    assert!(
        (u - 30.0).abs() < 1.0e-9,
        "the open sketch follows too: {u}"
    );

    // A value the rectangle cannot take is refused, and nothing changes.
    replace_text_input(&mut harness, "Variable value depth", "-3");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(
        harness
            .state()
            .document_status_text()
            .is_some_and(|status| status.contains("rejected")),
        "{:?}",
        harness.state().document_status_text()
    );
    assert_eq!(
        harness.state().evaluated_variable_values()["depth"].canonical,
        15.0
    );
    assert!((volume(&harness) - 30.0 * 2.0 * 5.0).abs() < 1.0e-6);
    click_button(&mut harness, "Cancel operation");

    // One undo takes the variable and the sketch that followed it back.
    click_button(&mut harness, "Undo history change");
    assert_eq!(
        harness.state().evaluated_variable_values()["depth"].canonical,
        10.0
    );
    assert!(
        (volume(&harness) - 20.0 * 2.0 * 5.0).abs() < 1.0e-6,
        "{}",
        volume(&harness)
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

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    let position = harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
    click_at(harness, position);
}

/// A dimension drawn between two objects follows a variable too (ADR 0054):
/// typed as `gap / 2`, it keeps the entry and moves the far object, from
/// the end it is measured from, when `gap` changes.
#[test]
fn a_distance_between_two_objects_follows_the_variable_it_is_typed_over() {
    let mut harness = harness();
    harness.run();
    create_length_variable(&mut harness);
    replace_text_input(&mut harness, "Variable name Length1", "gap");

    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Sketch mode");
    for (start, end) in [((-8.0, 0.0), (-4.0, 0.0)), ((0.0, 0.0), (4.0, 0.0))] {
        click_button(&mut harness, "Single line");
        click_sketch_point(&mut harness, SketchPoint::new(start.0, start.1));
        click_sketch_point(&mut harness, SketchPoint::new(end.0, end.1));
    }
    click_button(&mut harness, "Sketch dimension");
    click_sketch_point(&mut harness, SketchPoint::new(-4.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    let separation_box = "Distance between points";
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, separation_box)
            .is_focused()
    );
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, separation_box)
        .type_text("gap / 2");
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();

    let distance = |harness: &Harness<'static, KernelLabApp>| {
        let dimensions = harness.state().sketch_point_to_point_dimensions();
        assert_eq!(dimensions.len(), 1, "one dimension throughout");
        (dimensions[0].to.u - dimensions[0].from.u).hypot(dimensions[0].to.v - dimensions[0].from.v)
    };
    assert!(
        (distance(&harness) - 5.0).abs() < 1.0e-6,
        "{}",
        distance(&harness)
    );
    let constraint = harness.state().sketch_point_to_point_dimensions()[0].constraint;
    assert_eq!(
        harness
            .state()
            .sketch_relation_dimension_follows(constraint),
        Some("gap / 2".to_owned())
    );

    // The far line is three long, so a gap that moves its start by more than
    // that would fold it; 12 moves it by one.
    replace_text_input(&mut harness, "Variable value gap", "12");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(
        (distance(&harness) - 6.0).abs() < 1.0e-6,
        "{} · {:?}",
        distance(&harness),
        harness.state().document_status_text()
    );
    let from = harness.state().sketch_point_to_point_dimensions()[0].from;
    assert!(
        (from.u + 4.0).abs() < 1.0e-6,
        "the end it is measured from stays put"
    );

    assert_eq!(
        harness
            .state()
            .sketch_relation_dimension_follows(constraint),
        Some("gap / 2".to_owned()),
        "it still follows the variable after following it"
    );
}

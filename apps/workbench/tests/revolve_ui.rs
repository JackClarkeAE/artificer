//! The Revolve command through the real widgets (ADR 0055): a rectangle
//! sketched on XZ with its width dimensioned over a variable, the Revolve
//! button picking up the sketch's only profile, the card's axis buttons
//! choosing what it turns about, confirming putting a Revolve chip in the
//! history, the variable reshaping it, and the chip's own right-click menu
//! reopening it.

use artificer_workbench::{KernelLabApp, WorkbenchMode, sketch::SketchPoint};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

const CONFIRM_OPERATION: &str = "Confirm operation";
const PI: f64 = std::f64::consts::PI;

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn press(
    harness: &mut Harness<'static, KernelLabApp>,
    position: egui::Pos2,
    button: egui::PointerButton,
) {
    harness.hover_at(position);
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: position,
            button,
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
    press(harness, center, egui::PointerButton::Primary);
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    let position = harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
    press(harness, position, egui::PointerButton::Primary);
}

fn replace_text(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
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

fn volume(harness: &Harness<'static, KernelLabApp>) -> f64 {
    harness
        .state()
        .displayed_measures()
        .expect("a body is on screen")
        .volume
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < 1.0e-9,
        "{what}: {actual} should be {expected}"
    );
}

#[test]
fn a_sketched_profile_is_revolved_follows_its_variable_and_reopens_from_its_chip() {
    let mut harness = harness();
    harness.run();

    // A `width` variable, 10 mm.
    click_button(&mut harness, "Parametric ribbon tab");
    click_button(&mut harness, "New length variable");
    click_button(&mut harness, CONFIRM_OPERATION);
    replace_text(&mut harness, "Variable name Length1", "width");
    // Put the panel away so it does not stand over the sketch.
    click_button(&mut harness, "Variables");

    // A rectangle beside the sketch's vertical axis, 3 tall and starting at
    // r = 1, its width dimensioned as the variable.
    click_button(&mut harness, "XZ Plane");
    click_button(&mut harness, "Sketch mode");
    // The camera turns to face the plane before anything is drawn on it.
    for _ in 0..24 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 3.0));
    click_button(&mut harness, "Sketch dimension");
    click_sketch_point(&mut harness, SketchPoint::new(1.5, 3.0));
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, "Rectangle width")
            .is_focused(),
        "the top side asks for the width"
    );
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Rectangle width")
        .type_text("width");
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);

    // Revolve takes the sketch's only profile and asks for an axis.
    click_button(&mut harness, "Revolve");
    assert!(!harness.state().staged_revolve_has_preview());
    assert!(
        harness
            .state()
            .staged_revolve_issue()
            .is_some_and(|issue| issue.contains("axis")),
        "{:?}",
        harness.state().staged_revolve_issue()
    );
    // The world Y axis stands out of the XZ plane, so it is offered but
    // cannot be chosen.
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Revolve axis Origin Y axis")
            .accesskit_node()
            .is_disabled()
    );
    click_button(&mut harness, "Revolve axis Sketch vertical axis");
    assert!(
        harness.state().staged_revolve_has_preview(),
        "{:?}",
        harness.state().staged_revolve_issue()
    );
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    // r in [1, 11], 3 tall.
    assert_close(volume(&harness), PI * (121.0 - 1.0) * 3.0, "revolved");

    // The variable reshapes the sketch, and the revolve with it.
    click_button(&mut harness, "Parametric ribbon tab");
    click_button(&mut harness, "Variables");
    replace_text(&mut harness, "Variable value width", "5");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_close(volume(&harness), PI * (36.0 - 1.0) * 3.0, "followed");

    // Its chip reopens it; the world Z axis is the same line.
    let chip = harness
        .get_by_role_and_label(Role::Button, "Revolve 1 feature")
        .rect()
        .center();
    press(&mut harness, chip, egui::PointerButton::Secondary);
    click_button(&mut harness, "Edit this revolve");
    assert!(harness.state().staged_revolve_has_preview());
    click_button(&mut harness, "Revolve axis Origin Z axis");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_close(volume(&harness), PI * (36.0 - 1.0) * 3.0, "edited");
}

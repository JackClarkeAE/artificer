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

/// Scrolls the operation pane until a control in it clears the pane's
/// confirm bar, as a user would to reach the lower rows of a long card.
fn reveal_in_card(harness: &mut Harness<'static, KernelLabApp>, role: Role, label: &str) {
    for _ in 0..24 {
        let bar = harness
            .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .rect()
            .top();
        let target = harness.get_by_role_and_label(role, label).rect();
        if target.bottom() < bar - 4.0 {
            return;
        }
        harness.hover_at(egui::pos2(target.center().x, bar - 80.0));
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -40.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        // The pane scrolls smoothly, so it is stepped rather than settled.
        for _ in 0..12 {
            harness.step();
        }
    }
    panic!("{label} stays behind the operation pane's confirm bar");
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

/// An angle typed over a variable: the revolve turns through it, follows it
/// when it changes, and reopens still following it.
#[test]
fn a_revolve_angle_typed_over_a_variable_follows_it() {
    let mut harness = harness();
    harness.run();

    // A `sweep` variable; new angle variables start at 45 degrees.
    click_button(&mut harness, "Parametric ribbon tab");
    click_button(&mut harness, "New angle variable");
    click_button(&mut harness, CONFIRM_OPERATION);
    replace_text(&mut harness, "Variable name Angle1", "sweep");
    click_button(&mut harness, "Variables");

    // A rectangle r in [1, 2], 3 tall.
    click_button(&mut harness, "XZ Plane");
    click_button(&mut harness, "Sketch mode");
    for _ in 0..24 {
        harness.step();
    }
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 3.0));
    click_button(&mut harness, "Finish sketch");

    click_button(&mut harness, "Revolve");
    click_button(&mut harness, "Revolve axis Sketch vertical axis");
    reveal_in_card(&mut harness, Role::Button, "Revolve extent Angle");
    click_button(&mut harness, "Revolve extent Angle");
    reveal_in_card(&mut harness, Role::TextInput, "Revolve angle");
    // Enter takes the angle and confirms the revolve.
    replace_text(&mut harness, "Revolve angle", "sweep * 2");
    assert_eq!(harness.state().last_error_code(), None);
    let tube = PI * (4.0 - 1.0) * 3.0;
    assert_close(volume(&harness), tube / 4.0, "a quarter turn");

    click_button(&mut harness, "Parametric ribbon tab");
    click_button(&mut harness, "Variables");
    replace_text(&mut harness, "Variable value sweep", "60");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_close(volume(&harness), tube / 3.0, "a third of a turn");
    click_button(&mut harness, "Variables");

    let chip = harness
        .get_by_role_and_label(Role::Button, "Revolve 1 feature")
        .rect()
        .center();
    press(&mut harness, chip, egui::PointerButton::Secondary);
    click_button(&mut harness, "Edit this revolve");
    assert_eq!(
        harness.state().staged_revolve_angle_follows().as_deref(),
        Some("sweep * 2")
    );
    reveal_in_card(&mut harness, Role::Button, "Revolve direction Symmetric");
    click_button(&mut harness, "Revolve direction Symmetric");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_close(volume(&harness), tube / 3.0, "symmetric");
}

/// The card's "Pick in view" arms the axis pick, and a second press puts it
/// away; while it is armed the model view offers the sketch's lines.
#[test]
fn the_axis_pick_is_armed_from_the_card() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "XZ Plane");
    click_button(&mut harness, "Sketch mode");
    for _ in 0..24 {
        harness.step();
    }
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 3.0));
    click_button(&mut harness, "Finish sketch");

    click_button(&mut harness, "Revolve");
    assert!(!harness.state().revolve_axis_pick_armed());
    reveal_in_card(&mut harness, Role::Button, "Pick the revolve axis in the view");
    click_button(&mut harness, "Pick the revolve axis in the view");
    assert!(harness.state().revolve_axis_pick_armed());
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Pick the revolve axis in the view")
            .accesskit_node()
            .toggled()
            == Some(egui::accesskit::Toggled::True),
        "the button shows the pick is armed"
    );
    click_button(&mut harness, "Pick the revolve axis in the view");
    assert!(!harness.state().revolve_axis_pick_armed());
}

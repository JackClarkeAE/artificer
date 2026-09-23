//! The Sweep command through the real widgets (ADR 0055): a path drawn on
//! XZ, a disc drawn on XY, the Sweep button taking the disc as the profile
//! with the path already chosen on the card, confirming putting a Sweep chip
//! in the history, and the chip's own right-click menu reopening it to run
//! the path the other way.

use artificer_workbench::{KernelLabApp, WorkbenchMode, sketch::SketchPoint};
use egui::accesskit::{Role, Toggled};
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

/// Opens a sketch on an origin plane and waits for the camera to face it.
fn sketch_on(harness: &mut Harness<'static, KernelLabApp>, plane: &str) {
    click_button(harness, plane);
    click_button(harness, "Sketch mode");
    for _ in 0..24 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn swept_bounds(harness: &Harness<'static, KernelLabApp>) -> (f64, f64, f64) {
    let measures = harness
        .state()
        .displayed_measures()
        .expect("a body is on screen");
    let bounds = measures.bounds.expect("the body has bounds");
    (measures.volume, bounds.min.z, bounds.max.z)
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= 1.0e-9 * expected.abs().max(1.0),
        "{what}: {actual} should be {expected}"
    );
}

#[test]
fn a_disc_is_swept_along_a_path_reversed_and_reopened_from_its_chip() {
    let mut harness = harness();
    harness.run();

    // The path: a line 3 up the XZ plane from the origin.
    sketch_on(&mut harness, "XZ Plane");
    click_button(&mut harness, "Single line");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 3.0));
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);

    // The profile: a disc of radius 1 on XY.
    sketch_on(&mut harness, "XY Plane");
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 0.0));
    click_button(&mut harness, "Finish sketch");

    // Sweep takes the sketch just finished as the profile, its only region,
    // and starts on the one path the other sketch offers.
    click_button(&mut harness, "Sweep");
    assert!(
        harness.state().staged_sweep_has_preview(),
        "{:?}",
        harness.state().staged_sweep_issue()
    );
    let pressed = |harness: &Harness<'static, KernelLabApp>, label: &str| {
        harness
            .get_by_role_and_label(Role::Button, label)
            .accesskit_node()
            .toggled()
            == Some(Toggled::True)
    };
    assert!(pressed(&harness, "Sweep path Sketch 1 path 1 · 1 curve"));
    assert!(pressed(&harness, "Sweep orientation Follow path"));
    assert!(!pressed(&harness, "Sweep reverse path"));
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let (volume, bottom, top) = swept_bounds(&harness);
    assert_close(volume, PI * 3.0, "swept");
    assert_close(bottom, 0.0, "from the disc");
    assert_close(top, 3.0, "up the path");

    // Its chip reopens it; run from the path's top, the disc is carried
    // downwards from where it lies.
    let chip = harness
        .get_by_role_and_label(Role::Button, "Sweep 1 feature")
        .rect()
        .center();
    press(&mut harness, chip, egui::PointerButton::Secondary);
    click_button(&mut harness, "Edit this sweep");
    assert!(harness.state().staged_sweep_has_preview());
    click_button(&mut harness, "Sweep reverse path");
    click_button(&mut harness, "Sweep orientation Keep orientation");
    assert!(pressed(&harness, "Sweep reverse path"));
    assert!(pressed(&harness, "Sweep orientation Keep orientation"));
    assert!(
        harness.state().staged_sweep_has_preview(),
        "{:?}",
        harness.state().staged_sweep_issue()
    );
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let (volume, bottom, top) = swept_bounds(&harness);
    assert_close(volume, PI * 3.0, "reversed");
    assert_close(bottom, -3.0, "down the path");
    assert_close(top, 0.0, "from the disc");
}

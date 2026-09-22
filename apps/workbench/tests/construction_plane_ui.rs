//! Construction planes through the real widgets (ADR 0048): the Plane
//! command stages a card on the picked face, its arrow drags it off the face,
//! the card's field reads the same value, confirming puts a Plane chip in the
//! history, and that chip's own right-click menu edits the plane.

use artificer_kernel::FaceRole;
use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn activate_face(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness
        .get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    harness.run();
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
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    press(harness, center, egui::PointerButton::Primary);
}

fn drag(harness: &mut Harness<'static, KernelLabApp>, start: egui::Pos2, end: egui::Pos2) {
    harness.hover_at(start);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    for step in 1..=6 {
        let t = step as f32 / 6.0;
        harness.hover_at(start + (end - start) * t);
        harness.step();
    }
    harness.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.step();
}

#[test]
fn a_plane_is_dragged_off_its_face_confirmed_and_reopened_from_its_chip() {
    let mut harness = harness();
    harness.run();
    activate_face(&mut harness, "Positive Z face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::PositiveZ)
    );

    click_button(&mut harness, "Plane");
    harness.run();
    assert_eq!(harness.state().staged_plane_offset(), Some(0.0));
    // The card carries the typed half of the editor.
    harness.get_by_label("Plane offset");

    // The arrow stands on the face pointing up the screen; dragging it up
    // lifts the plane off the face.
    let handle = harness.get_by_label("Plane offset handle").rect().center();
    drag(&mut harness, handle, handle - egui::vec2(0.0, 80.0));
    let offset = harness
        .state()
        .staged_plane_offset()
        .expect("the plane is still staged");
    assert!(
        offset > 0.1,
        "dragging the arrow up lifts the plane: {offset}"
    );

    click_button(&mut harness, "Confirm operation");
    harness.run();
    assert_eq!(harness.state().staged_plane_offset(), None);
    assert_eq!(harness.state().construction_plane_names(), vec!["Plane 1"]);

    // The plane has a chip of its own in the history, and the chip's own
    // right-click menu reopens the plane's editor where it was left.
    let chip = harness
        .get_by_role_and_label(Role::Button, "Plane 1 feature")
        .rect()
        .center();
    press(&mut harness, chip, egui::PointerButton::Secondary);
    harness.run();
    click_button(&mut harness, "Edit this plane");
    harness.run();
    let reopened = harness
        .state()
        .staged_plane_offset()
        .expect("the editor reopened");
    assert!((reopened - offset).abs() < 1.0e-9);
}

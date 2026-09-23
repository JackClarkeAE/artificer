//! The Loft command through the real widgets (ADR 0051): a square sketched
//! on XY, a circle sketched on a construction plane above it, the Loft button
//! opening its card, one profile picked in each sketch, confirming putting a
//! Loft chip in the history, and that chip's own right-click menu reopening
//! the loft on the sections it was built from.

use artificer_workbench::{KernelLabApp, WorkbenchMode, sketch::SketchPoint};
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

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    let position = harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
    press(harness, position, egui::PointerButton::Primary);
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

/// The one region a committed sketch offers the model view.
fn only_region(harness: &Harness<'static, KernelLabApp>, sketch: usize) -> [f64; 2] {
    let anchors = harness.state().model_sketch_region_anchors(sketch);
    assert_eq!(anchors.len(), 1, "{anchors:?}");
    anchors[0]
}

#[test]
fn a_loft_is_built_from_two_sketches_and_reopened_from_its_chip() {
    let mut harness = harness();
    harness.run();

    // A square on XY.
    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, -2.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 2.0));
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);

    // A plane dragged up off XY.
    click_button(&mut harness, "Plane");
    harness.run();
    let handle = harness.get_by_label("Plane offset handle").rect().center();
    drag(&mut harness, handle, handle - egui::vec2(0.0, 90.0));
    let offset = harness
        .state()
        .staged_plane_offset()
        .expect("the plane is staged");
    assert!(offset > 0.5, "the plane left XY: {offset}");
    click_button(&mut harness, "Confirm operation");
    harness.run();

    // A circle on the plane.
    click_button(&mut harness, "Sketch on selected plane");
    for _ in 0..24 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 0.0));
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);

    // Loft, one profile in each sketch, in order.
    click_button(&mut harness, "Loft");
    harness.run();
    assert_eq!(harness.state().staged_loft_section_count(), Some(0));
    let square = only_region(&harness, 0);
    let circle = only_region(&harness, 1);
    assert!(harness.state_mut().pick_loft_region(0, square, false));
    assert!(harness.state_mut().pick_loft_region(1, circle, false));
    harness.run();
    assert!(
        harness.state().staged_loft_has_preview(),
        "{:?}",
        harness.state().staged_loft_issue()
    );
    // The card lists both sections, each with its own control.
    harness.get_by_label("Remove loft section 1");
    harness.get_by_label("Move loft section 2 earlier");
    click_button(&mut harness, "Confirm operation");
    harness.run();
    assert_eq!(harness.state().staged_loft_section_count(), None);
    let volume = harness
        .state()
        .displayed_measures()
        .expect("the loft is on screen")
        .volume;
    assert!(volume > 0.0 && volume < 16.0 * offset, "volume {volume}");

    // Its chip's right-click menu reopens it on the same two sections.
    let chip = harness
        .get_by_role_and_label(Role::Button, "Loft 1 feature")
        .rect()
        .center();
    press(&mut harness, chip, egui::PointerButton::Secondary);
    harness.run();
    click_button(&mut harness, "Edit this loft");
    harness.run();
    assert_eq!(harness.state().staged_loft_section_count(), Some(2));
    assert!(harness.state().staged_loft_has_preview());
    harness.get_by_label("Remove loft section 2");
}

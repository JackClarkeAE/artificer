//! A spline profile through the real widgets (ADR 0050): the Fit-point spline
//! tool draws a loop by clicking its points and then its first point again,
//! the profile card reads the loop as a closed exact region, and Extrude
//! turns it into a solid whose walls are B-spline surfaces.

use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchPoint},
};
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
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    let position = harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
    click_at(harness, position);
}

#[test]
fn a_closed_fit_point_spline_is_drawn_and_extruded() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(
        &mut harness,
        "Choose line or spline type; current default: Single line.",
    );
    click_button(&mut harness, "Fit-point spline");
    assert_eq!(
        harness.state().active_sketch_tool_label(),
        "Fit-point spline"
    );

    let points = [
        SketchPoint::new(-3.0, -2.0),
        SketchPoint::new(2.5, -2.5),
        SketchPoint::new(3.5, 1.5),
        SketchPoint::new(0.0, 3.0),
        SketchPoint::new(-3.5, 1.0),
    ];
    for point in points {
        click_sketch_point(&mut harness, point);
    }
    // The first point again closes the loop.
    click_sketch_point(&mut harness, points[0]);
    harness.run();
    assert!(
        matches!(
            harness.state().sketch_profile_status(),
            CertifiedProfileStatus::ClosedRegions { regions: 1, .. }
        ),
        "{:?}",
        harness.state().sketch_profile_status()
    );

    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, "Confirm operation");
    harness.run();
    assert_eq!(harness.state().last_error_code(), None);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("the spline extruded into a body");
    // The loop encloses roughly a 6 × 5 patch; the default depth is 4.
    assert!(
        measures.volume > 4.0 * 15.0 && measures.volume < 4.0 * 45.0,
        "volume {}",
        measures.volume
    );
}

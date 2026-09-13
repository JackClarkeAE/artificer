//! The view cube's corners and edges: each is a click target that turns the
//! camera to look from it, with world Z kept upright, and none of them
//! interferes with dragging the cube to orbit.

use artificer_protocol::Vector3;
use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const EPSILON: f64 = 1.0e-9;

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn assert_direction(actual: Vector3, expected: Vector3, what: &str) {
    assert!(
        (actual.x - expected.x).abs() <= EPSILON
            && (actual.y - expected.y).abs() <= EPSILON
            && (actual.z - expected.z).abs() <= EPSILON,
        "{what}: {actual:?} != {expected:?}"
    );
}

fn unit(x: f64, y: f64, z: f64) -> Vector3 {
    let length = (x * x + y * y + z * z).sqrt();
    Vector3::new(x / length, y / length, z / length)
}

fn click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
}

#[test]
fn clicking_a_cube_corner_looks_from_that_corner_with_z_up() {
    let mut harness = harness();
    harness.run();
    let framing_before = harness.state().view_frame();
    harness
        .get_by_role_and_label(Role::Button, "View cube corner front-top-right")
        .click_accesskit();
    harness.run();

    let direction = harness.state().view_direction();
    assert_direction(direction, unit(1.0, -1.0, 1.0), "front-top-right");
    // Screen-up is world +Z with its component along the view removed.
    assert_direction(
        harness.state().screen_up_direction(),
        unit(-1.0, 1.0, 2.0),
        "screen up from the corner",
    );
    assert_eq!(
        harness.state().view_frame(),
        framing_before,
        "a corner click turns the camera without reframing"
    );
}

#[test]
fn clicking_a_cube_edge_looks_from_that_edge() {
    let mut harness = harness();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "View cube edge front-top")
        .click_accesskit();
    harness.run();
    assert_direction(
        harness.state().view_direction(),
        unit(0.0, -1.0, 1.0),
        "front-top",
    );
    assert_direction(
        harness.state().screen_up_direction(),
        unit(0.0, 1.0, 1.0),
        "screen up from the edge",
    );

    // A vertical edge keeps Z exactly up. From the front-top view the
    // front-right edge is on show; back-right is not.
    harness
        .get_by_role_and_label(Role::Button, "View cube edge front-right")
        .click_accesskit();
    harness.run();
    assert_direction(
        harness.state().view_direction(),
        unit(1.0, -1.0, 0.0),
        "front-right",
    );
    assert_direction(
        harness.state().screen_up_direction(),
        Vector3::new(0.0, 0.0, 1.0),
        "screen up from a vertical edge",
    );
}

#[test]
fn only_handles_on_a_visible_face_are_offered() {
    let mut harness = harness();
    harness.run();
    // The paused workbench opens on an isometric from the +X, +Y, +Z side,
    // so the far corner touches no visible face and is not a target.
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "View cube corner front-bottom-left")
            .is_none(),
        "a corner behind the cube must not be clickable"
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "View cube corner back-top-right")
            .is_some()
    );
    // The front-top edge touches the visible top face, so it is offered
    // even though the front face is hidden; turning to it brings the front
    // handles out and puts the back-right edge, which touches neither the
    // front nor the top, out of reach.
    harness
        .get_by_role_and_label(Role::Button, "View cube edge front-top")
        .click_accesskit();
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "View cube corner front-bottom-left")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "View cube edge back-right")
            .is_none()
    );
}

#[test]
fn a_pointer_click_on_a_corner_is_not_taken_by_the_face_behind_it() {
    let mut harness = harness();
    harness.run();
    let corner = harness
        .get_by_role_and_label(Role::Button, "View cube corner back-top-right")
        .rect()
        .center();
    click_at(&mut harness, corner);
    harness.run();
    assert_direction(
        harness.state().view_direction(),
        unit(1.0, 1.0, 1.0),
        "a pointer click on the back-top-right corner",
    );
}

#[test]
fn dragging_the_cube_still_orbits() {
    let mut harness = harness();
    harness.run();
    let before = harness.state().view_parameters();
    let start = harness
        .get_by_role_and_label(Role::Button, "View cube top")
        .rect()
        .center();
    let end = start + egui::vec2(30.0, 18.0);
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.run();
    let after = harness.state().view_parameters();
    assert!(
        (after.0 - before.0).abs() > 0.01 || (after.1 - before.1).abs() > 0.01,
        "dragging the cube from {before:?} left it at {after:?}"
    );
}

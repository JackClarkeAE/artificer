//! The model viewport's right-click menu, from the user's report: "we really
//! really need an on right click menu relative to the current selection".
//!
//! The menu offers only what applies to what was right-clicked, acts on that
//! entity the way a left-click would, and never steals the right-drag orbit.

use artificer_kernel::FaceRole;
use artificer_workbench::{KernelLabApp, WorkbenchMode, sketch::SketchPoint};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const NORMAL_TO_FACE: &str = "Normal to face";
const SKETCH_ON_FACE: &str = "Sketch on this face";
const ZOOM_TO_SELECTION: &str = "Zoom to selection";
const HIDE_BODY: &str = "Hide this body";
const ZOOM_TO_FIT: &str = "Zoom to fit";

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

/// Extrude lives on the Model tab alone now, and a ribbon tab no longer
/// changes the workspace: a sketch reaches the model commands without leaving.
fn show_model_commands(harness: &mut Harness<'static, KernelLabApp>) {
    if harness
        .query_by_role_and_label(Role::Button, "Extrude")
        .is_none()
    {
        click_button(harness, "Model ribbon tab");
    }
}

fn click_extrude(harness: &mut Harness<'static, KernelLabApp>) {
    show_model_commands(harness);
    click_button(harness, "Extrude");
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn right_click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    // Press and release inside one frame: egui only reports a click once it has
    // ruled out a drag, which is what keeps right-drag orbit working.
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    harness.step();
    harness.step();
}

fn right_click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    right_click_at(harness, center);
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn commit_centered_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    width: f64,
    height: f64,
) {
    click_button(harness, "Two-point rectangle");
    click_at(
        harness,
        canvas_sketch_point(harness, SketchPoint::new(-width * 0.5, -height * 0.5)),
    );
    click_at(
        harness,
        canvas_sketch_point(harness, SketchPoint::new(width * 0.5, height * 0.5)),
    );
    assert_eq!(harness.state().sketch_entity_count(), 1);
}

fn create_extruded_body(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Create sketch");
    commit_centered_rectangle(harness, 4.0, 2.0);
    click_button(harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    click_extrude(harness);
    click_button(harness, "Confirm operation");
    assert!(harness.state().displayed_snapshot_id().is_some());
    // Committing keeps the camera where the sketch left it, which frames
    // the whole datum plane; the picks below want the body filling the view.
    press_key(harness, egui::Key::F);
    harness.run();
}

#[test]
fn right_clicking_a_face_selects_it_and_offers_only_what_applies() {
    let mut harness = harness();
    create_extruded_body(&mut harness);

    right_click_button(&mut harness, "Extrusion top face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::ExtrusionTop),
        "a right-click selects what it lands on, the way a left-click does"
    );
    let labels = harness.state().model_context_menu_labels();
    assert_eq!(
        labels,
        vec![
            NORMAL_TO_FACE,
            SKETCH_ON_FACE,
            ZOOM_TO_SELECTION,
            HIDE_BODY,
            "Clear face, edge and vertex selection",
            ZOOM_TO_FIT,
        ]
    );
    // "Isolate this body" needs another visible body to hide, and "Show all
    // bodies" needs a hidden one. Neither exists here, so neither is offered.
    harness.get_by_role_and_label(Role::Button, NORMAL_TO_FACE);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Isolate this body")
            .is_none()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show all bodies")
            .is_none()
    );
}

#[test]
fn normal_to_face_flies_the_camera_square_to_it() {
    let mut harness = harness();
    create_extruded_body(&mut harness);

    right_click_button(&mut harness, "Extrusion top face");
    click_button(&mut harness, NORMAL_TO_FACE);
    for _ in 0..90 {
        harness.step();
    }
    assert!(harness.state().model_context_menu_labels().is_empty());
    // The top face's normal is +Z, so the camera ends up looking straight down.
    let (_, pitch, _) = harness.state().view_parameters();
    assert!(
        (pitch.to_degrees().abs() - 90.0).abs() < 1.0,
        "pitch was {}",
        pitch.to_degrees()
    );
}

#[test]
fn sketch_on_this_face_opens_a_face_sketch() {
    let mut harness = harness();
    create_extruded_body(&mut harness);

    right_click_button(&mut harness, "Extrusion top face");
    click_button(&mut harness, SKETCH_ON_FACE);
    for _ in 0..120 {
        harness.step();
        if harness.state().workbench_mode() == WorkbenchMode::Sketch {
            break;
        }
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(harness.state().sketch_is_face_supported());
}

#[test]
fn hide_this_body_hides_it_and_then_offers_to_show_it_again() {
    let mut harness = harness();
    create_extruded_body(&mut harness);

    let centre = harness.get_by_label("Model viewport").rect().center();
    right_click_button(&mut harness, "Extrusion top face");
    click_button(&mut harness, HIDE_BODY);
    assert!(!harness.state().body_visible(0));

    // With the body hidden there is nothing under the pointer, so the menu
    // shrinks to what still applies — including putting it back.
    right_click_at(&mut harness, centre);
    let labels = harness.state().model_context_menu_labels();
    assert!(labels.contains(&"Show all bodies"), "{labels:?}");
    assert!(!labels.contains(&HIDE_BODY), "{labels:?}");
    click_button(&mut harness, "Show all bodies");
    assert!(harness.state().body_visible(0));
}

#[test]
fn escape_dismisses_the_menu_without_touching_the_model() {
    let mut harness = harness();
    create_extruded_body(&mut harness);
    let snapshot = harness.state().displayed_snapshot_id();

    right_click_button(&mut harness, "Extrusion top face");
    assert!(!harness.state().model_context_menu_labels().is_empty());
    press_key(&mut harness, egui::Key::Escape);
    assert!(harness.state().model_context_menu_labels().is_empty());
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::ExtrusionTop),
        "dismissing the menu leaves the selection it acted on"
    );
}

#[test]
fn a_staged_operation_owns_the_canvas_and_suppresses_the_menu() {
    let mut harness = harness();
    create_extruded_body(&mut harness);

    // With a face selected, Extrude stages a push/pull on it.
    click_button(&mut harness, "Extrusion top face");
    click_extrude(&mut harness);
    assert!(harness.state().operation_confirmation_pending());
    right_click_button(&mut harness, "Extrusion top face");
    assert!(
        harness.state().model_context_menu_labels().is_empty(),
        "a staged operation owns the canvas until its rail resolves"
    );
}

/// The menu as pixels: anchored where the click landed, listing only what
/// applies to the face it was raised on.
#[test]
fn model_context_menu_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .with_options(
            egui_kittest::SnapshotOptions::new()
                .output_path(snapshot_directory)
                .failed_pixel_count_threshold(
                    egui_kittest::OsThreshold::new(0).linux(400).windows(400),
                ),
        )
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    create_extruded_body(&mut harness);
    right_click_button(&mut harness, "Extrusion top face");
    harness.remove_cursor();
    for _ in 0..3 {
        harness.step();
    }
    harness.snapshot("workbench_model_context_menu_1280");
}

/// Each ribbon tab, so the whole command surface is under pixel review rather
/// than only the tab that happens to open first.
#[test]
fn ribbon_tab_snapshots() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .with_options(
            egui_kittest::SnapshotOptions::new()
                .output_path(snapshot_directory)
                .failed_pixel_count_threshold(
                    egui_kittest::OsThreshold::new(0).linux(400).windows(400),
                ),
        )
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    create_extruded_body(&mut harness);
    click_button(&mut harness, "Extrusion top face");
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_ribbon_model_tab_1280");

    click_button(&mut harness, "View ribbon tab");
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_ribbon_view_tab_1280");

    click_button(&mut harness, "Switch theme");
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_ribbon_dark_theme_1280");
    click_button(&mut harness, "Switch theme");

    click_button(&mut harness, "Model ribbon tab");
    click_button(&mut harness, "Sketch on selected face");
    for _ in 0..120 {
        harness.step();
        if harness.state().workbench_mode() == WorkbenchMode::Sketch {
            break;
        }
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_ribbon_sketch_tab_1280");
}

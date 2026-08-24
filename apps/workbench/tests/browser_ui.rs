//! The Browser tree's selection and right-click menu, from the user's report:
//! the explorer needs its own right-click menu, selectable rows that other
//! commands can act on — several bodies at once included — and an eye that
//! reads as an eye.
//!
//! Everything here drives accessible names, so the eye buttons keep their
//! "Hide Body 1"-style labels no matter how they are painted.

use artificer_workbench::{KernelLabApp, WorkbenchMode, sketch::SketchPoint};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const BODY_ONE_ROW: &str = "Body 1 · native cuboid";
const COMPONENT_ROW: &str = "20 × 20 Aluminium Extrusion · component 1";
const HIDE_SELECTED: &str = "Hide selected bodies";
const SHOW_SELECTED: &str = "Show selected bodies";
const UNHIDE_ALL: &str = "Unhide all bodies";
const MIRROR_SELECTED: &str = "Mirror across selected plane";
const ASSIGN_MATERIAL: &str = "Assign material…";
const EXPORT_BODY_STL: &str = "Export this body as STL";
const EXPORT_BODY_STEP: &str = "Export this body as STEP";
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

fn click_at(
    harness: &mut Harness<'static, KernelLabApp>,
    position: egui::Pos2,
    modifiers: egui::Modifiers,
) {
    harness.hover_at(position);
    harness.step();
    for pressed in [true, false] {
        // `event_modifiers` holds the modifiers in the frame's global input,
        // which is where the Browser's row-click handler reads them.
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            },
            modifiers,
        );
        harness.step();
    }
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center, egui::Modifiers::NONE);
}

fn double_click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    // Outrun the double-click window first, so an earlier click in the test
    // cannot chain with this pair into a triple click.
    for _ in 0..30 {
        harness.step();
    }
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    harness.hover_at(center);
    harness.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
        }
    }
    harness.step();
}

fn command_click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center, egui::Modifiers::COMMAND);
}

fn right_click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    // Press and release inside one frame: egui only reports a click once it
    // has ruled out a drag.
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

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

/// Commits one 20×20 aluminium extrusion from the part library, giving the
/// document a second body ("Body 2") inside component 1.
fn insert_library_component(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Library");
    let input = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    input.click();
    input.type_text("455");
    harness.run();
    click_button(harness, "Add to current workspace");
    click_button(harness, CONFIRM_OPERATION);
    // Toggle the library window shut again: left open it floats over the
    // Browser panel and would swallow the row clicks this file is about.
    click_button(harness, "Library");
    harness.run();
    assert_eq!(harness.state().body_count(), 2);
}

fn commit_rectangle_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(harness, "Two-point rectangle");
    for point in [SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)] {
        let position = harness
            .state()
            .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
        click_at(harness, position, egui::Modifiers::NONE);
    }
    click_button(harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn scratch_document_root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("artificer-browser-{tag}-{}", std::process::id()))
}

/// The reported gap: a sketch row's left-click used to be an invisible
/// selection no-op. It is now the contextually correct action — open the
/// sketch for editing.
#[test]
fn left_clicking_a_sketch_row_selects_it_and_double_click_edits() {
    let mut harness = harness();
    commit_rectangle_sketch(&mut harness);

    // A single click is orientation, not a mode jump: the row highlights and
    // the sketch becomes the active profile source, while the workspace
    // stays where the user was.
    click_button(&mut harness, "Select Sketch 1");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().browser_selected_sketch_index(), Some(0));

    double_click_button(&mut harness, "Select Sketch 1");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(
        harness.state().sketch_entity_count() >= 1,
        "the sketch opened with its committed geometry on the canvas"
    );
}

/// A sketch is already a drawing in its own plane, and its row's menu can
/// hand it to anything that reads DXF.
#[test]
fn the_sketch_row_menu_exports_a_dxf() {
    let mut harness = harness();
    commit_rectangle_sketch(&mut harness);
    let root = scratch_document_root("dxf");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    harness
        .state_mut()
        .set_document_path(root.join("part.artificer"));

    right_click_button(&mut harness, "Select Sketch 1");
    click_button(&mut harness, "Export this sketch as DXF");

    let exported = root.join("part.sketch-1.dxf");
    let text = std::fs::read_to_string(&exported).expect("the DXF should exist");
    assert!(text.contains("LINE"), "{text}");
    assert!(text.trim_end().ends_with("EOF"));
    let _ = std::fs::remove_dir_all(root);
}

/// Exporting one body from its row writes that body alone, without touching
/// the visibility of anything else.
#[test]
fn the_body_row_menu_exports_that_body_alone() {
    let mut harness = harness();
    harness.run();
    let root = scratch_document_root("body-stl");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    harness
        .state_mut()
        .set_document_path(root.join("part.artificer"));

    right_click_button(&mut harness, BODY_ONE_ROW);
    click_button(&mut harness, EXPORT_BODY_STL);

    let exported = root.join("part.body-1.stl");
    let text = std::fs::read_to_string(&exported).expect("the STL should exist");
    assert!(text.starts_with("solid Artificer"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn right_clicking_a_body_row_offers_commands_and_escape_dismisses() {
    let mut harness = harness();
    harness.run();
    assert!(harness.state().browser_context_menu_labels().is_empty());

    right_click_button(&mut harness, BODY_ONE_ROW);
    // The right-click selected the row before opening the menu.
    assert_eq!(harness.state().browser_selected_body_ordinals(), vec![1]);
    assert_eq!(
        harness.state().browser_context_menu_labels(),
        vec![
            HIDE_SELECTED,
            MIRROR_SELECTED,
            ASSIGN_MATERIAL,
            EXPORT_BODY_STL,
            EXPORT_BODY_STEP,
        ],
    );

    press_key(&mut harness, egui::Key::Escape);
    assert!(harness.state().browser_context_menu_labels().is_empty());
}

#[test]
fn browser_menu_hides_and_unhides_the_selected_body() {
    let mut harness = harness();
    harness.run();

    right_click_button(&mut harness, BODY_ONE_ROW);
    click_button(&mut harness, HIDE_SELECTED);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Body 1")
            .is_some(),
        "hiding must flip the eye to the closed state's label"
    );
    assert!(!harness.state().body_visible(0));

    right_click_button(&mut harness, BODY_ONE_ROW);
    let labels = harness.state().browser_context_menu_labels();
    assert!(labels.contains(&SHOW_SELECTED));
    assert!(labels.contains(&UNHIDE_ALL));
    click_button(&mut harness, UNHIDE_ALL);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Body 1")
            .is_some()
    );
    assert!(harness.state().body_visible(0));
}

#[test]
fn command_click_builds_a_multi_selection_and_batch_hides() {
    let mut harness = harness();
    harness.run();
    insert_library_component(&mut harness);

    click_button(&mut harness, BODY_ONE_ROW);
    assert_eq!(harness.state().browser_selected_body_ordinals(), vec![1]);
    command_click_button(&mut harness, COMPONENT_ROW);
    assert_eq!(
        harness.state().browser_selected_body_ordinals(),
        vec![1, 2],
        "Cmd/Ctrl-click must add the second body to the selection"
    );

    right_click_button(&mut harness, COMPONENT_ROW);
    let labels = harness.state().browser_context_menu_labels();
    assert!(labels.contains(&HIDE_SELECTED));
    assert!(labels.contains(&MIRROR_SELECTED));
    click_button(&mut harness, HIDE_SELECTED);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Body 1")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Body 2")
            .is_some()
    );

    right_click_button(&mut harness, COMPONENT_ROW);
    click_button(&mut harness, SHOW_SELECTED);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Body 1")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Body 2")
            .is_some()
    );
}

#[test]
fn selecting_a_plane_in_the_browser_aims_the_mirror() {
    let mut harness = harness();
    harness.run();
    let before = harness
        .state()
        .displayed_measures()
        .expect("the default document displays a body")
        .centroid
        .expect("a solid has a centroid");

    right_click_button(&mut harness, "YZ Plane");
    assert_eq!(
        harness.state().browser_context_menu_labels(),
        vec!["Select this plane"],
    );
    click_button(&mut harness, "Select this plane");
    assert!(harness.state().browser_context_menu_labels().is_empty());

    right_click_button(&mut harness, BODY_ONE_ROW);
    click_button(&mut harness, MIRROR_SELECTED);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Mirror body")
    );
    click_button(&mut harness, CONFIRM_OPERATION);
    harness.run();

    assert_eq!(harness.state().last_error_code(), None);
    let after = harness
        .state()
        .displayed_measures()
        .expect("the mirrored document displays a body")
        .centroid
        .expect("a solid has a centroid");
    assert!(
        (after.x + before.x).abs() <= 1.0e-9,
        "mirroring across YZ must negate the centroid's x: {} vs {}",
        before.x,
        after.x,
    );
}

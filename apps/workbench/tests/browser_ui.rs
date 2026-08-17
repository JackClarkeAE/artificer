//! The Browser tree's selection and right-click menu, from the user's report:
//! the explorer needs its own right-click menu, selectable rows that other
//! commands can act on — several bodies at once included — and an eye that
//! reads as an eye.
//!
//! Everything here drives accessible names, so the eye buttons keep their
//! "Hide Body 1"-style labels no matter how they are painted.

use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const BODY_ONE_ROW: &str = "Body 1 · native cuboid";
const COMPONENT_ROW: &str = "20 × 20 Aluminium Extrusion · component 1";
const HIDE_SELECTED: &str = "Hide selected bodies";
const SHOW_SELECTED: &str = "Show selected bodies";
const UNHIDE_ALL: &str = "Unhide all bodies";
const MIRROR_SELECTED: &str = "Mirror across selected plane";
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
        vec![HIDE_SELECTED, MIRROR_SELECTED],
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

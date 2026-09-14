//! The document tab strip: opening, switching, and closing documents from
//! the top of the window, each keeping its own workbench — and the dot that
//! marks a document with unsaved changes, with the prompt that stands
//! between such a document and anything that would lose them.

use std::path::PathBuf;

use artificer_workbench::documents::WorkbenchShell;
use egui::accesskit::Role;
use egui::{ViewportCommand, ViewportId};
use egui_kittest::{Harness, kittest::Queryable as _};

fn harness() -> Harness<'static, WorkbenchShell> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| WorkbenchShell::new_paused(creation_context))
}

fn click_button(harness: &mut Harness<'static, WorkbenchShell>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

/// An edit to the document in front: a library part added to the
/// workspace and confirmed, which is the shortest committed change the
/// workbench offers.
fn edit_the_active_document(harness: &mut Harness<'static, WorkbenchShell>) {
    click_button(harness, "Library");
    let input = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    input.click();
    input.type_text("310");
    harness.run();
    click_button(harness, "Add to current workspace");
    click_button(harness, "Confirm operation");
    assert!(harness.state().active_document().is_document_dirty());
}

/// A scratch folder of its own for each test, so a save lands nowhere shared.
fn scratch_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "artificer-document-tabs-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Whether the last frame asked the window to do `command`.
fn root_viewport_sent(
    harness: &Harness<'static, WorkbenchShell>,
    command: &ViewportCommand,
) -> bool {
    harness
        .output()
        .viewport_output
        .get(&ViewportId::ROOT)
        .is_some_and(|output| output.commands.contains(command))
}

/// The window's close button, as the platform reports it.
fn request_window_close(harness: &mut Harness<'static, WorkbenchShell>) {
    harness
        .input_mut()
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
}

/// Steps up to `frames` frames and reports whether any of them asked the
/// window to do `command`. A click's answer lands a frame or two after the
/// click, and a settled run has already moved past that frame.
fn sent_within_frames(
    harness: &mut Harness<'static, WorkbenchShell>,
    command: &ViewportCommand,
    frames: usize,
) -> bool {
    (0..frames).any(|_| {
        harness.step();
        root_viewport_sent(harness, command)
    })
}

#[test]
fn a_document_wears_a_dot_from_its_first_edit_until_it_is_saved() {
    let root = scratch_root("dirty-marker");
    let mut harness = harness();
    harness
        .state_mut()
        .active_document_mut()
        .set_document_path(root.join("marked.artificer"));
    harness.run();
    assert!(!harness.state().active_document().is_document_dirty());
    assert!(
        harness
            .query_by_label("Unsaved changes in Document 1")
            .is_none()
    );

    edit_the_active_document(&mut harness);
    assert!(
        harness
            .query_by_label("Unsaved changes in Document 1")
            .is_some(),
        "the header marks the document dirty"
    );
    // The tab keeps its name; the dot is display, not identity.
    assert!(harness.query_by_label("Show Document 1").is_some());

    click_button(&mut harness, "File menu");
    click_button(&mut harness, "Save document");
    assert!(root.join("marked.artificer").is_file());
    assert!(!harness.state().active_document().is_document_dirty());
    assert!(
        harness
            .query_by_label("Unsaved changes in Document 1")
            .is_none()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn closing_a_dirty_tab_asks_first() {
    let mut harness = harness();
    harness.run();
    edit_the_active_document(&mut harness);
    // A second, clean tab so the first can be closed at all.
    harness.get_by_label("New document tab").click();
    harness.run();
    assert_eq!(harness.state().document_count(), 2);

    harness.get_by_label("Close Document 1").click();
    harness.run();
    assert!(harness.state().close_prompt_open());
    assert_eq!(harness.state().document_count(), 2, "nothing closed yet");

    click_button(&mut harness, "Cancel");
    assert!(!harness.state().close_prompt_open());
    assert_eq!(harness.state().document_count(), 2);

    harness.get_by_label("Close Document 1").click();
    harness.run();
    click_button(&mut harness, "Don't save");
    assert!(!harness.state().close_prompt_open());
    assert_eq!(harness.state().document_count(), 1);
    assert_eq!(
        harness.state().active_document().document_title(),
        "Document 2"
    );
}

#[test]
fn closing_the_window_with_unsaved_changes_asks_first() {
    let mut harness = harness();
    harness.run();

    // A clean window closes without a word.
    request_window_close(&mut harness);
    harness.step();
    assert!(!harness.state().close_prompt_open());
    assert!(!root_viewport_sent(&harness, &ViewportCommand::CancelClose));

    edit_the_active_document(&mut harness);
    request_window_close(&mut harness);
    harness.step();
    assert!(harness.state().close_prompt_open());
    assert!(root_viewport_sent(&harness, &ViewportCommand::CancelClose));
    assert!(!harness.state().window_close_confirmed());
    harness.run();

    // Cancel keeps the window and the changes.
    click_button(&mut harness, "Cancel");
    assert!(!harness.state().close_prompt_open());
    assert!(!harness.state().window_close_confirmed());
    assert!(harness.state().active_document().is_document_dirty());

    // Don't save lets the window go: the close is sent, and the window's
    // next close request goes through unchallenged.
    request_window_close(&mut harness);
    harness.step();
    assert!(harness.state().close_prompt_open());
    harness
        .get_by_role_and_label(Role::Button, "Don't save")
        .click();
    assert!(sent_within_frames(&mut harness, &ViewportCommand::Close, 4));
    assert!(!harness.state().close_prompt_open());
    assert!(harness.state().window_close_confirmed());
    request_window_close(&mut harness);
    harness.step();
    assert!(!harness.state().close_prompt_open());
    assert!(!root_viewport_sent(&harness, &ViewportCommand::CancelClose));
}

#[test]
fn opening_over_a_dirty_document_asks_first() {
    let root = scratch_root("open-over-dirty");
    let saved = root.join("saved.artificer");
    let mut harness = harness();
    harness
        .state_mut()
        .active_document_mut()
        .set_document_path(&saved);
    harness.run();
    click_button(&mut harness, "File menu");
    click_button(&mut harness, "Save document");
    assert!(saved.is_file());

    edit_the_active_document(&mut harness);
    // Open by path is the flow a headless test can drive.
    click_button(&mut harness, "File menu");
    click_button(&mut harness, "Open document by path");
    assert!(
        harness
            .state()
            .active_document()
            .document_path_prompt_open()
    );
    let field = harness.get_by_role_and_label(Role::TextInput, "Document path");
    field.click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Document path")
        .type_text(&saved.display().to_string());
    harness.run();
    click_button(&mut harness, "Open");
    assert!(harness.state().active_document().unsaved_prompt_open());
    assert_eq!(
        harness.state().active_document().pending_operation_label(),
        None,
        "nothing is staged until the user has answered"
    );

    click_button(&mut harness, "Don't save");
    assert!(!harness.state().active_document().unsaved_prompt_open());
    assert_eq!(
        harness.state().active_document().pending_operation_label(),
        Some("Open saved document"),
        "the open then goes to the same confirmation gate as every operation"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_window_opens_with_one_document_tab_and_a_way_to_add_more() {
    let mut harness = harness();
    harness.run();
    assert_eq!(harness.state().document_count(), 1);
    assert!(harness.query_by_label("Show Document 1").is_some());
    assert!(harness.query_by_label("New document tab").is_some());
    // The one document keeps its header and its workbench below the strip.
    assert!(harness.query_by_label("Artificer Workbench").is_some());
    assert!(harness.query_by_label("Document 1").is_some());
    // A lone document cannot be closed, so it offers no close glyph.
    assert!(harness.query_by_label("Close Document 1").is_none());
}

#[test]
fn adding_switching_and_closing_tabs_keeps_each_document_in_place() {
    let mut harness = harness();
    harness.run();

    harness.get_by_label("New document tab").click();
    harness.run();
    assert_eq!(harness.state().document_count(), 2);
    assert_eq!(harness.state().active_index(), 1);
    assert_eq!(
        harness.state().active_document().document_title(),
        "Document 2"
    );
    assert!(harness.query_by_label("Show Document 1").is_some());
    assert!(harness.query_by_label("Show Document 2").is_some());

    harness.get_by_label("Show Document 1").click();
    harness.run();
    assert_eq!(harness.state().active_index(), 0);
    assert_eq!(
        harness.state().active_document().document_title(),
        "Document 1"
    );

    harness.get_by_label("Close Document 2").click();
    harness.run();
    assert_eq!(harness.state().document_count(), 1);
    assert_eq!(harness.state().active_index(), 0);
    assert!(harness.query_by_label("Show Document 2").is_none());
}

#[test]
fn the_file_menu_opens_a_new_document_in_its_own_tab() {
    let mut harness = harness();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "File menu")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "New document")
        .click();
    harness.step();
    // The request is answered on the next frame's logic pass.
    harness.run();
    assert_eq!(harness.state().document_count(), 2);
    assert_eq!(
        harness.state().active_document().document_title(),
        "Document 2"
    );
}

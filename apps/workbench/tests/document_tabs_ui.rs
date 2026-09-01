//! The document tab strip: opening, switching, and closing documents from
//! the top of the window, each keeping its own workbench.

use artificer_workbench::documents::WorkbenchShell;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

fn harness() -> Harness<'static, WorkbenchShell> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| WorkbenchShell::new_paused(creation_context))
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

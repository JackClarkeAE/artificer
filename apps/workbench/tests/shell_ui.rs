use artificer_workbench::{
    DisplayLengthUnit, KernelLabApp, WorkbenchMode, shell::WorkbenchShellVisibility,
};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const COLLAPSE_COMMAND_RIBBON: &str = "Collapse command ribbon";

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

fn click_scrolled_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    // `scroll_to_me` animates the scroll offset, and `run()` can return while
    // that animation is still in flight. A click aimed at a rect sampled from
    // one of those frames lands wherever the row has moved on to by the time
    // the press is processed, which is a real one-in-a-dozen flake rather than
    // a product fault. Settle until the row stops moving, then aim.
    let mut previous = None;
    for _ in 0..60 {
        harness
            .get_by_role_and_label(Role::Button, label)
            .scroll_to_me();
        harness.run();
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        if previous == Some(rect) {
            break;
        }
        previous = Some(rect);
    }
    click_button(harness, label);
}

fn open_lab_diagnostics(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Properties");
    click_scrolled_button(harness, "LAB / DIAGNOSTICS");
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
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.step();
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn model_viewport(harness: &Harness<'static, KernelLabApp>) -> egui::Rect {
    harness.get_by_label("Model viewport").rect()
}

fn collapse_all_shell_regions(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Collapse browser panel");
    click_button(harness, COLLAPSE_COMMAND_RIBBON);
    click_button(harness, "Collapse design-history preview");
}

fn restore_all_shell_regions(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "History");
    click_button(harness, "Expand command ribbon");
    click_button(harness, "Expand model browser");
}

#[test]
fn document_properties_popout_changes_units_and_exposes_real_file_actions() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "Properties");

    harness
        .get_by_role_and_label(Role::ComboBox, "Length unit")
        .click_accesskit();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Inches (in)")
        .click_accesskit();
    harness.run();

    assert_eq!(
        harness.state().document_settings().length_unit,
        DisplayLengthUnit::Inch
    );
    for action in [
        "Save .ARTIFICER",
        "Open .ARTIFICER",
        "Export STL",
        "Export STEP (exact B-rep)",
        "Export STEP (faceted)",
    ] {
        assert!(
            harness
                .get_by_role_and_label(Role::Button, action)
                .rect()
                .is_positive(),
            "{action} must be visible in the document properties popout"
        );
    }
}

#[test]
fn shell_regions_collapse_independently_and_restore_without_model_mutation() {
    let mut harness = harness();
    harness.run();

    let initial_viewport = model_viewport(&harness);
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();
    let transform = harness.state().displayed_transform();
    let view = harness.state().view_parameters();
    let frame = harness.state().view_frame();
    let timeline = harness.state().feature_timeline_entries();
    let mut expected = WorkbenchShellVisibility::default();
    assert_eq!(harness.state().shell_visibility(), expected);

    click_button(&mut harness, "Collapse browser panel");
    expected.model_browser = false;
    assert_eq!(harness.state().shell_visibility(), expected);
    let browser_collapsed = model_viewport(&harness);
    assert!(browser_collapsed.min.x < initial_viewport.min.x);
    assert!(browser_collapsed.width() > initial_viewport.width());

    click_button(&mut harness, COLLAPSE_COMMAND_RIBBON);
    expected.command_ribbon = false;
    assert_eq!(harness.state().shell_visibility(), expected);
    let ribbon_collapsed = model_viewport(&harness);
    assert!(ribbon_collapsed.min.y < browser_collapsed.min.y);
    assert!(ribbon_collapsed.height() > browser_collapsed.height());

    click_button(&mut harness, "Collapse design-history preview");
    expected.feature_timeline = false;
    assert_eq!(harness.state().shell_visibility(), expected);
    let all_collapsed = model_viewport(&harness);
    assert!(all_collapsed.max.y > ribbon_collapsed.max.y);
    assert!(all_collapsed.height() > ribbon_collapsed.height());

    restore_all_shell_regions(&mut harness);
    assert_eq!(
        harness.state().shell_visibility(),
        WorkbenchShellVisibility::default()
    );
    assert_eq!(model_viewport(&harness), initial_viewport);
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().displayed_transform(), transform);
    assert_eq!(harness.state().view_parameters(), view);
    assert_eq!(harness.state().view_frame(), frame);
    assert_eq!(harness.state().feature_timeline_entries(), timeline);
}

#[test]
fn shell_collapse_and_restore_preserve_a_pending_operation() {
    let mut harness = harness();
    harness.run();
    open_lab_diagnostics(&mut harness);
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();
    let timeline = harness.state().feature_timeline_entries();

    click_scrolled_button(&mut harness, "Zero width");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().transaction_attempt_count(), attempts);

    collapse_all_shell_regions(&mut harness);
    assert_eq!(
        harness.state().shell_visibility(),
        WorkbenchShellVisibility {
            command_ribbon: false,
            model_browser: false,
            feature_timeline: false,
        }
    );
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().feature_timeline_entries(), timeline);

    restore_all_shell_regions(&mut harness);
    assert_eq!(
        harness.state().shell_visibility(),
        WorkbenchShellVisibility::default()
    );
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().feature_timeline_entries(), timeline);

    press_key(&mut harness, egui::Key::Escape);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
}

#[test]
fn bare_enter_activates_shell_focus_only_when_no_operation_is_pending() {
    let mut harness = harness();
    harness.run();

    harness
        .get_by_role_and_label(Role::Button, "Collapse browser panel")
        .focus();
    harness.run();
    press_key(&mut harness, egui::Key::Enter);
    assert!(!harness.state().shell_visibility().model_browser);
    assert!(!harness.state().operation_confirmation_pending());

    click_button(&mut harness, "Expand model browser");
    assert!(harness.state().shell_visibility().model_browser);
    let attempts = harness.state().transaction_attempt_count();
    open_lab_diagnostics(&mut harness);
    click_scrolled_button(&mut harness, "Zero width");
    assert!(harness.state().operation_confirmation_pending());

    harness
        .get_by_role_and_label(Role::Button, "Collapse browser panel")
        .focus();
    harness.run();
    press_key(&mut harness, egui::Key::Enter);

    assert!(
        harness.state().shell_visibility().model_browser,
        "bare Enter must confirm the pending operation without also toggling focused shell chrome"
    );
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
}

#[test]
fn feature_timeline_changes_only_after_committed_sketch_and_extrusion_edits() {
    let mut harness = harness();
    harness.run();
    let initial_snapshot = harness.state().displayed_snapshot_id();
    let initial_attempts = harness.state().transaction_attempt_count();
    let base_timeline = vec!["Origin".to_owned(), "Base body".to_owned()];

    assert_eq!(harness.state().feature_timeline_entries(), base_timeline);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Base cuboid feature")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Sketch 1 feature")
            .is_none()
    );

    click_button(&mut harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    press_key(&mut harness, egui::Key::R);
    assert_eq!(harness.state().sketch_tool_label(), "Rectangle");

    let center = harness.get_by_label("Sketch viewport").rect().center();
    let first = center + egui::vec2(-70.0, 45.0);
    let opposite = center + egui::vec2(70.0, -45.0);
    // A half-finished draft is timeline-neutral: nothing has committed.
    click_at(&mut harness, first);
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().feature_timeline_entries(), base_timeline);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Sketch 1 feature")
            .is_none()
    );

    press_key(&mut harness, egui::Key::Escape);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().feature_timeline_entries(), base_timeline);

    // The finished stroke commits itself; the timeline follows immediately.
    click_at(&mut harness, first);
    click_at(&mut harness, opposite);
    assert_eq!(harness.state().pending_operation_label(), None);

    let sketch_timeline = vec![
        "Origin".to_owned(),
        "Base body".to_owned(),
        "Sketch 1 · r1".to_owned(),
    ];
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert_eq!(harness.state().feature_timeline_entries(), sketch_timeline);
    assert_eq!(harness.state().displayed_snapshot_id(), initial_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Sketch 1 feature")
            .is_some()
    );

    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().feature_timeline_entries(), sketch_timeline);

    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().feature_timeline_entries(), sketch_timeline);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts
    );

    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch")
    );
    assert_eq!(harness.state().feature_timeline_entries(), sketch_timeline);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Extrude 1 feature")
            .is_none()
    );

    press_key(&mut harness, egui::Key::Escape);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().feature_timeline_entries(), sketch_timeline);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts
    );

    click_button(&mut harness, "Extrude");
    assert_eq!(harness.state().feature_timeline_entries(), sketch_timeline);
    press_key(&mut harness, egui::Key::Enter);

    let extruded_timeline = vec![
        "Origin".to_owned(),
        "Base body".to_owned(),
        "Sketch 1 · r1".to_owned(),
        "Extrude 1".to_owned(),
    ];
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().extruded_sketch_revision(), Some(1));
    assert_eq!(
        harness.state().feature_timeline_entries(),
        extruded_timeline
    );
    assert_ne!(harness.state().displayed_snapshot_id(), initial_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts + 1
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Extrude 1 feature")
            .is_some()
    );
}

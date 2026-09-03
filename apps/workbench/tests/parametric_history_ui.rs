use artificer_kernel::FaceRole;
use artificer_model::{
    CURRENT_DOCUMENT_VERSION, ModelDocument, NATIVE_DOCUMENT_FORMAT, RebuildState,
};
use artificer_workbench::{
    ExtrusionMode, KernelLabApp, SketchExtrusionEligibility, WorkbenchMode, sketch::SketchPoint,
};
use egui::accesskit::{Action, ActionData, ActionRequest, Role};
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

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

fn activate_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    {
        let node = harness.get_by_role_and_label(Role::Button, label);
        assert!(
            !node.accesskit_node().is_disabled(),
            "accessible control {label:?} should be enabled"
        );
        node.click_accesskit();
    }
    harness.run();
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn set_history_slider(harness: &mut Harness<'static, KernelLabApp>, position: usize) {
    let expected_maximum = harness.state().document_feature_count() as f64;
    let (target_node, target_tree) = {
        let slider = harness
            .query_all_by_role(Role::Slider)
            .find(|node| {
                node.accesskit_node().min_numeric_value() == Some(0.0)
                    && node.accesskit_node().max_numeric_value() == Some(expected_maximum)
            })
            .expect("Fusion-style history rollback slider");
        slider.accesskit_node().locate()
    };
    harness.event(egui::Event::AccessKitActionRequest(ActionRequest {
        target_node,
        target_tree,
        action: Action::SetValue,
        data: Some(ActionData::NumericValue(position as f64)),
    }));
    harness.run();
}

fn history_slider_value(harness: &Harness<'static, KernelLabApp>) -> f64 {
    let expected_maximum = harness.state().document_feature_count() as f64;
    harness
        .query_all_by_role(Role::Slider)
        .find(|node| {
            node.accesskit_node().min_numeric_value() == Some(0.0)
                && node.accesskit_node().max_numeric_value() == Some(expected_maximum)
        })
        .and_then(|node| node.accesskit_node().numeric_value())
        .expect("numeric history rollback slider value")
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

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn type_active_dimension(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn commit_centered_rectangle_with_dimensions(
    harness: &mut Harness<'static, KernelLabApp>,
    width: f64,
    height: f64,
) {
    click_button(harness, "Two-point rectangle");
    click_at(
        harness,
        canvas_sketch_point(harness, SketchPoint::new(-width * 0.5, -height * 0.5)),
    );
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle width", &width.to_string());
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle height", &height.to_string());
    press_key(harness, egui::Key::Enter);
    // Strokes commit as they are drawn; nothing waits behind a tick.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
}

fn commit_centered_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    commit_centered_rectangle_with_dimensions(harness, 1.0, 1.0);
}

fn finish_active_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Finish sketch");
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

/// Whether an accessibility node is the extrusion distance control.
fn is_extrusion_distance(node: &egui_kittest::Node<'_>) -> bool {
    node.value()
        .as_deref()
        .is_some_and(|value| value.starts_with("Distance "))
        || node.accesskit_node().min_numeric_value() == Some(-1_000.0)
}

fn set_extrusion_distance(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    {
        // Exactly one control must match. Two would mean the inspector and a
        // command editor are both offering the distance, and picking the first
        // silently drives whichever the accessibility tree happened to order
        // first — which reads as a wrong value rather than a missing widget.
        let matches = harness
            .query_all_by_role(Role::SpinButton)
            .filter(is_extrusion_distance)
            .count();
        assert_eq!(
            matches, 1,
            "expected exactly one extrusion distance control, found {matches}"
        );
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(is_extrusion_distance)
            .expect("extrusion distance control");
        distance.scroll_to_me();
    }
    harness.run();
    {
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(is_extrusion_distance)
            .expect("visible extrusion distance control");
        distance.click();
    }
    harness.run();
    harness.event(egui::Event::Text(value.to_owned()));
    harness.run();
    press_key(harness, egui::Key::Enter);
}

#[derive(Clone, Copy, Debug)]
enum FeatureScenario {
    Extrude,
    Add,
    Cut,
}

impl FeatureScenario {
    const fn mode(self) -> ExtrusionMode {
        match self {
            Self::Extrude => ExtrusionMode::NewBody,
            Self::Add => ExtrusionMode::Add,
            Self::Cut => ExtrusionMode::Cut,
        }
    }

    const fn history_label(self) -> &'static str {
        match self {
            Self::Extrude => "Extrude 1",
            Self::Add => "Add 1",
            Self::Cut => "Cut 1",
        }
    }

    fn feature_button_label(self) -> String {
        format!("{} feature", self.history_label())
    }
}

fn prepare_finished_sketch(harness: &mut Harness<'static, KernelLabApp>, case: FeatureScenario) {
    harness.run();
    match case {
        FeatureScenario::Extrude => {
            click_button(harness, "XY Plane");
            click_button(harness, "Create sketch");
            assert!(!harness.state().sketch_is_face_supported());
        }
        FeatureScenario::Add | FeatureScenario::Cut => {
            click_button(harness, "Positive Z face");
            click_button(harness, "Sketch on selected face");
            assert!(harness.state().sketch_is_face_supported());
        }
    }
    commit_centered_rectangle(harness);
    finish_active_sketch(harness);
    if matches!(case, FeatureScenario::Cut) {
        activate_button(harness, "Cut");
        set_extrusion_distance(harness, "-1");
    }
    assert_eq!(harness.state().extrusion_mode(), case.mode());
}

fn commit_prepared_feature(harness: &mut Harness<'static, KernelLabApp>, case: FeatureScenario) {
    click_extrude(harness);
    activate_button(harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    assert!(
        harness
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == case.history_label())
    );
}

fn assert_extrude_enabled(harness: &mut Harness<'static, KernelLabApp>) {
    show_model_commands(harness);
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a committed, active closed sketch should enable Extrude"
    );
}

fn assert_extrude_disabled(harness: &mut Harness<'static, KernelLabApp>) {
    show_model_commands(harness);
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "an inactive history sketch must not remain extrudable"
    );
}

fn select_latest_feature_end(harness: &mut Harness<'static, KernelLabApp>) {
    {
        let face = harness
            .query_all_by_role_and_label(Role::Button, "Feature end face")
            .last()
            .expect("latest generated end face should be selectable");
        face.click_accesskit();
    }
    harness.run();
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::FeatureEnd)
    );
}

#[test]
fn committed_history_undo_and_redo_restore_exact_model_and_visibility() {
    let mut harness = harness();
    harness.run();
    let initial_feature_count = harness.state().document_feature_count();
    let initial_revision = harness.state().document_revision();
    assert_eq!(initial_feature_count, 2);
    assert_eq!(harness.state().document_dirty_feature_count(), 0);

    prepare_finished_sketch(&mut harness, FeatureScenario::Add);
    let sketch_revision = harness.state().document_revision();
    let before_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("base body snapshot");
    let before_digest = harness
        .state()
        .displayed_semantic_digest()
        .expect("base body semantic digest");
    let before_body_count = harness.state().body_count();
    assert_eq!(
        harness.state().document_feature_count(),
        initial_feature_count + 1
    );
    assert!(sketch_revision > initial_revision);
    assert_eq!(harness.state().sketch_count(), 1);
    assert!(harness.state().sketch_visible(0));

    commit_prepared_feature(&mut harness, FeatureScenario::Add);
    let committed_revision = harness.state().document_revision();
    let committed_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("added body snapshot");
    let committed_digest = harness
        .state()
        .displayed_semantic_digest()
        .expect("added body semantic digest");
    let committed_body_count = harness.state().body_count();
    assert_ne!(committed_snapshot, before_snapshot);
    assert_ne!(committed_digest, before_digest);
    assert_eq!(
        harness.state().document_feature_count(),
        initial_feature_count + 2
    );
    assert!(committed_revision > sketch_revision);
    assert!(!harness.state().sketch_visible(0));
    assert!(harness.state().document_can_undo());
    assert!(!harness.state().document_can_redo());
    assert_eq!(harness.state().document_dirty_feature_count(), 0);

    activate_button(&mut harness, "Undo history change");
    assert_eq!(
        harness.state().document_feature_count(),
        initial_feature_count + 1
    );
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(before_snapshot)
    );
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        Some(before_digest)
    );
    assert_eq!(harness.state().body_count(), before_body_count);
    assert!(harness.state().sketch_visible(0));
    assert!(harness.state().document_can_redo());
    assert_eq!(harness.state().document_dirty_feature_count(), 0);

    activate_button(&mut harness, "Redo history change");
    assert_eq!(
        harness.state().document_feature_count(),
        initial_feature_count + 2
    );
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(committed_snapshot)
    );
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        Some(committed_digest)
    );
    assert_eq!(harness.state().body_count(), committed_body_count);
    assert!(!harness.state().sketch_visible(0));
    assert_eq!(harness.state().document_dirty_feature_count(), 0);
    assert!(harness.state().document_revision() > committed_revision);
}

#[test]
fn rollback_marker_retains_future_timeline_and_restores_the_exact_branch_head() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Add);
    let base_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("base body snapshot before Add");
    let base_digest = harness
        .state()
        .displayed_semantic_digest()
        .expect("base body semantic digest before Add");
    let base_body_count = harness.state().body_count();
    commit_prepared_feature(&mut harness, FeatureScenario::Add);

    let committed_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("committed Add snapshot");
    let committed_digest = harness
        .state()
        .displayed_semantic_digest()
        .expect("committed Add semantic digest");
    let retained_timeline = harness.state().feature_timeline_entries();
    let history_end = harness.state().document_feature_count();
    assert_eq!(history_end, 4);
    assert_eq!(harness.state().history_position(), history_end);
    assert_eq!(harness.state().history_position_count(), history_end + 1);
    assert_eq!(history_slider_value(&harness), history_end as f64);
    assert_ne!(committed_snapshot, base_snapshot);
    assert!(!harness.state().sketch_visible(0));

    activate_button(&mut harness, "Step history backward");
    assert_eq!(harness.state().history_position(), history_end - 1);
    assert_eq!(history_slider_value(&harness), (history_end - 1) as f64);
    assert_eq!(
        harness.state().feature_timeline_entries(),
        retained_timeline,
        "rolling back must retain the future feature branch in the timeline"
    );
    assert_eq!(harness.state().displayed_snapshot_id(), Some(base_snapshot));
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        Some(base_digest)
    );
    assert_eq!(harness.state().body_count(), base_body_count);
    assert!(harness.state().body_visible(0));
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    show_model_commands(&mut harness);
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "new solid features must remain unavailable until the marker returns to the end"
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "New sketch")
            .accesskit_node()
            .is_disabled(),
        "new sketches must remain unavailable while viewing an earlier history state"
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "M  Move")
            .accesskit_node()
            .is_disabled(),
        "body mutation tools must remain unavailable while viewing an earlier history state"
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Add 1 feature")
            .accesskit_node()
            .is_disabled(),
        "future timeline chips should be visible but inactive"
    );

    set_history_slider(&mut harness, 2);
    assert_eq!(harness.state().history_position(), 2);
    assert_eq!(history_slider_value(&harness), 2.0);
    assert_eq!(harness.state().displayed_snapshot_id(), Some(base_snapshot));
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        Some(base_digest)
    );
    assert!(!harness.state().sketch_visible(0));
    let rolled_before_sketch = harness.state().feature_timeline_entries();
    assert_eq!(rolled_before_sketch.len(), retained_timeline.len());
    assert_eq!(rolled_before_sketch[0], "Origin");
    assert_eq!(rolled_before_sketch[1], "Base body");
    assert_eq!(rolled_before_sketch[3], "Add 1");

    activate_button(&mut harness, "Step history forward");
    assert_eq!(harness.state().history_position(), 3);
    assert!(harness.state().sketch_visible(0));
    activate_button(&mut harness, "Step history forward");
    assert_eq!(harness.state().history_position(), history_end);
    assert_eq!(history_slider_value(&harness), history_end as f64);
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(committed_snapshot)
    );
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        Some(committed_digest)
    );
    assert_eq!(harness.state().body_count(), base_body_count);
    assert!(harness.state().body_visible(0));
    assert!(!harness.state().sketch_visible(0));
    assert_eq!(
        harness.state().feature_timeline_entries(),
        retained_timeline
    );
}

#[test]
fn face_operation_override_preserves_direction_and_auto_restores_sign_inference() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Add);

    let initial_magnitude = harness.state().extrusion_distance().abs();
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
    assert!(initial_magnitude > 0.0);

    activate_button(&mut harness, "Cut");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Cut);
    assert_eq!(harness.state().extrusion_distance(), initial_magnitude);
    assert!(!harness.state().extrusion_mode_is_automatic());

    set_extrusion_distance(&mut harness, "-0.5");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Cut);
    assert!((harness.state().extrusion_distance() + 0.5).abs() <= 1.0e-12);

    activate_button(&mut harness, "Add");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
    assert!((harness.state().extrusion_distance() + 0.5).abs() <= 1.0e-12);
    assert!(!harness.state().extrusion_mode_is_automatic());

    set_extrusion_distance(&mut harness, "0.5");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
    assert!((harness.state().extrusion_distance() - 0.5).abs() <= 1.0e-12);

    activate_button(&mut harness, "Auto");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
    assert!(harness.state().extrusion_mode_is_automatic());

    set_extrusion_distance(&mut harness, "-0.5");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Cut);
    click_extrude(&mut harness);
    assert!(harness.state().operation_confirmation_pending());
    activate_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().last_error_code(), None);
    assert!(
        harness
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == "Cut 1"),
        "the signed negative intent should publish as a Cut feature"
    );
}

#[test]
fn selected_solid_features_suppress_and_restore_with_atomic_clean_rebuilds() {
    for case in [
        FeatureScenario::Extrude,
        FeatureScenario::Add,
        FeatureScenario::Cut,
    ] {
        let mut harness = harness();
        prepare_finished_sketch(&mut harness, case);
        let before_snapshot = harness
            .state()
            .displayed_snapshot_id()
            .expect("pre-feature body snapshot");
        let before_digest = harness
            .state()
            .displayed_semantic_digest()
            .expect("pre-feature semantic digest");
        let before_body_count = harness.state().body_count();

        commit_prepared_feature(&mut harness, case);
        let feature_count = harness.state().document_feature_count();
        let committed_snapshot = harness
            .state()
            .displayed_snapshot_id()
            .expect("committed solid feature snapshot");
        let committed_digest = harness
            .state()
            .displayed_semantic_digest()
            .expect("committed solid feature digest");
        let committed_body_count = harness.state().body_count();
        assert_ne!(committed_snapshot, before_snapshot, "case: {case:?}");

        activate_button(&mut harness, &case.feature_button_label());
        activate_button(&mut harness, "Suppress selected feature");
        assert_eq!(harness.state().document_feature_count(), feature_count);
        assert_eq!(harness.state().document_dirty_feature_count(), 0);
        assert_eq!(
            harness.state().displayed_snapshot_id(),
            Some(before_snapshot),
            "suppression did not atomically restore the branch base for {case:?}"
        );
        assert_eq!(
            harness.state().displayed_semantic_digest(),
            Some(before_digest),
            "case: {case:?}"
        );
        assert_eq!(harness.state().body_count(), before_body_count);
        assert!(harness.state().sketch_visible(0));
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Restore selected feature")
                .is_some(),
            "suppression state must be visible and reversible for {case:?}"
        );

        activate_button(&mut harness, "Restore selected feature");
        assert_eq!(harness.state().document_feature_count(), feature_count);
        assert_eq!(harness.state().document_dirty_feature_count(), 0);
        assert_eq!(
            harness.state().displayed_snapshot_id(),
            Some(committed_snapshot),
            "restore did not deterministically replay {case:?}"
        );
        assert_eq!(
            harness.state().displayed_semantic_digest(),
            Some(committed_digest),
            "case: {case:?}"
        );
        assert_eq!(harness.state().body_count(), committed_body_count);
        assert!(!harness.state().sketch_visible(0));
        assert!(harness.state().document_can_undo());
    }
}

#[test]
fn identical_new_bodies_keep_independent_history_branches() {
    let mut harness = harness();
    harness.run();

    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "Create sketch");
    commit_centered_rectangle_with_dimensions(&mut harness, 4.0, 2.0);
    finish_active_sketch(&mut harness);
    assert_extrude_enabled(&mut harness);
    commit_prepared_feature(&mut harness, FeatureScenario::Extrude);
    let shared_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("first extrusion snapshot");
    assert_eq!(harness.state().body_count(), 1);

    click_button(&mut harness, "New sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    commit_centered_rectangle_with_dimensions(&mut harness, 4.0, 2.0);
    finish_active_sketch(&mut harness);
    assert_extrude_enabled(&mut harness);
    click_extrude(&mut harness);
    activate_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().body_count(), 2);
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(shared_snapshot),
        "geometrically identical bodies should be allowed to share a content snapshot"
    );
    click_button(&mut harness, "Browser");
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

    // Make the overlapping face target unambiguous, then mutate only Body 2.
    click_button(&mut harness, "Hide Body 1");
    click_button(&mut harness, "Extrusion top face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::ExtrusionTop)
    );
    click_button(&mut harness, "Sketch on selected face");
    commit_centered_rectangle(&mut harness);
    finish_active_sketch(&mut harness);
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
    assert_extrude_enabled(&mut harness);
    commit_prepared_feature(&mut harness, FeatureScenario::Add);
    let modified_second_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("second body Add snapshot");
    assert_ne!(modified_second_snapshot, shared_snapshot);

    click_button(&mut harness, "Browser");
    activate_button(&mut harness, "Body 1 · native sketch extrusion");
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(shared_snapshot),
        "editing Body 2 must not move Body 1's branch cursor"
    );
    activate_button(&mut harness, "Body 2 · native added boss");
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(modified_second_snapshot),
        "Body 2 must retain its independently rebuilt branch head"
    );
    assert!(!harness.state().body_visible(0));
    assert!(harness.state().body_visible(1));
}

#[test]
fn undoing_add_suppression_restores_a_generated_face_that_can_be_sketch_extruded() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Add);
    commit_prepared_feature(&mut harness, FeatureScenario::Add);
    let added_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("committed Add snapshot");

    activate_button(&mut harness, "Add 1 feature");
    activate_button(&mut harness, "Suppress selected feature");
    assert_ne!(
        harness.state().displayed_snapshot_id(),
        Some(added_snapshot)
    );

    activate_button(&mut harness, "Undo history change");
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(added_snapshot),
        "undo must restore the exact generated-face snapshot"
    );
    assert_eq!(harness.state().document_dirty_feature_count(), 0);

    select_latest_feature_end(&mut harness);
    click_button(&mut harness, "Sketch on selected face");
    commit_centered_rectangle_with_dimensions(&mut harness, 0.5, 0.5);
    finish_active_sketch(&mut harness);
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
    assert_extrude_enabled(&mut harness);
    click_extrude(&mut harness);
    activate_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().last_error_code(), None);
    assert!(
        harness
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == "Add 2"),
        "the face restored by undo must remain a valid support for a subsequent Add"
    );
}

#[test]
fn undoing_extrusion_restores_its_sketch_then_undoing_sketch_removes_the_artifact() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Extrude);
    commit_prepared_feature(&mut harness, FeatureScenario::Extrude);
    assert!(!harness.state().sketch_visible(0));

    activate_button(&mut harness, "Undo history change");
    assert_eq!(harness.state().document_feature_count(), 3);
    assert_eq!(harness.state().sketch_count(), 1);
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some(),
        "undoing the consumer must visibly restore its source sketch"
    );
    assert_extrude_enabled(&mut harness);

    activate_button(&mut harness, "Undo history change");
    assert_eq!(harness.state().document_feature_count(), 2);
    let restored: ModelDocument = serde_json::from_str(
        &harness
            .state()
            .native_document_json()
            .expect("document after undoing Sketch should serialize"),
    )
    .expect("document after undoing Sketch should deserialize");
    assert!(restored.sketches().is_empty());
    let runtime_sketch_count = harness.state().sketch_count();
    let overlay_count = harness.state().visible_model_sketch_overlay_count();
    let eligibility = harness.state().sketch_extrusion_eligibility();
    show_model_commands(&mut harness);
    let extrude_disabled = harness
        .get_by_role_and_label(Role::Button, "Extrude")
        .accesskit_node()
        .is_disabled();
    let has_sketch_visibility_control = harness
        .query_by_role_and_label(Role::Button, "Hide Sketch 1")
        .is_some()
        || harness
            .query_by_role_and_label(Role::Button, "Show Sketch 1")
            .is_some();
    assert!(
        runtime_sketch_count == 0
            && overlay_count == 0
            && eligibility != SketchExtrusionEligibility::Ready
            && extrude_disabled
            && !has_sketch_visibility_control,
        "undoing past Sketch must remove its stale runtime identity and geometry; \
         runtime sketches={runtime_sketch_count}, overlays={overlay_count}, \
         eligibility={eligibility:?}, Extrude disabled={extrude_disabled}, \
         visibility control present={has_sketch_visibility_control}"
    );

    // The hidden runtime artifact is an undo/redo cache, not live model state.
    // Redo must recover the exact closed profile before restoring its consumer.
    activate_button(&mut harness, "Redo history change");
    assert_eq!(harness.state().sketch_count(), 1);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert!(harness.state().sketch_visible(0));
    assert_extrude_enabled(&mut harness);

    activate_button(&mut harness, "Redo history change");
    assert_eq!(harness.state().sketch_count(), 1);
    assert!(!harness.state().sketch_visible(0));
    assert_extrude_disabled(&mut harness);
}

#[test]
fn suppressing_and_restoring_sketch_disables_and_reenables_extrude() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Extrude);
    let feature_count = harness.state().document_feature_count();
    assert_extrude_enabled(&mut harness);

    activate_button(&mut harness, "Sketch 1 feature");
    activate_button(&mut harness, "Suppress selected feature");
    assert_eq!(harness.state().document_feature_count(), feature_count);
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::InactiveHistorySketch
    );
    assert!(!harness.state().sketch_visible(0));
    assert_extrude_disabled(&mut harness);

    activate_button(&mut harness, "Restore selected feature");
    assert_eq!(harness.state().document_feature_count(), feature_count);
    assert!(harness.state().sketch_visible(0));
    assert_extrude_enabled(&mut harness);
}

#[test]
fn explicitly_shown_consumed_sketch_survives_rebuild_undo_and_redo() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Extrude);
    commit_prepared_feature(&mut harness, FeatureScenario::Extrude);
    assert!(!harness.state().sketch_visible(0));
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Sketch 1")
            .is_some()
    );

    click_button(&mut harness, "Show Sketch 1");
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some()
    );

    activate_button(&mut harness, "Extrude 1 feature");
    activate_button(&mut harness, "Suppress selected feature");
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);

    activate_button(&mut harness, "Restore selected feature");
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some(),
        "rebuild must not reinstate auto-hide after an explicit Show"
    );

    activate_button(&mut harness, "Undo history change");
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    activate_button(&mut harness, "Redo history change");
    assert!(harness.state().sketch_visible(0));
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some()
    );
}

#[test]
fn native_document_json_roundtrip_preserves_public_document_invariants() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Add);
    commit_prepared_feature(&mut harness, FeatureScenario::Add);

    let json = harness
        .state()
        .native_document_json()
        .expect("native document should serialize");
    let encoded: serde_json::Value =
        serde_json::from_str(&json).expect("native document should be valid JSON");
    assert_eq!(encoded["format"], NATIVE_DOCUMENT_FORMAT);
    assert_eq!(encoded["version"], CURRENT_DOCUMENT_VERSION);
    assert_eq!(encoded["revision"], harness.state().document_revision());

    let restored: ModelDocument =
        serde_json::from_str(&json).expect("current native document should deserialize");
    assert_eq!(restored.revision(), harness.state().document_revision());
    assert_eq!(
        restored.features().len(),
        harness.state().document_feature_count()
    );
    assert_eq!(restored.bodies().len(), harness.state().body_count());
    assert_eq!(restored.sketches().len(), harness.state().sketch_count());
    assert_eq!(
        restored.head_snapshot(),
        harness.state().displayed_snapshot_id()
    );
    assert_eq!(
        restored.bodies()[0].visible,
        harness.state().body_visible(0)
    );
    assert_eq!(
        restored
            .features()
            .last()
            .and_then(|feature| feature.committed)
            .map(|commit| commit.semantic_digest),
        harness.state().displayed_semantic_digest()
    );
    assert!(
        restored
            .features()
            .iter()
            .all(|feature| feature.state.rebuild == RebuildState::Clean)
    );
    assert!(!restored.can_undo());
    assert!(!restored.can_redo());

    let reencoded = serde_json::to_value(&restored).expect("restored document should serialize");
    assert_eq!(reencoded, encoded);
}

/// The reported gap: editing a sketch that a feature already consumed used to
/// leave the branch dirty until a manual Rebuild press, and until then the
/// document refused further work on it. An accepted sketch edit now replays
/// its dependents on its own, exactly as a feature-scalar edit does.
#[test]
fn editing_a_consumed_sketch_rebuilds_its_extrusion_automatically() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Extrude);
    commit_prepared_feature(&mut harness, FeatureScenario::Extrude);
    let before = harness
        .state()
        .displayed_measures()
        .expect("committed extrusion")
        .volume;
    assert!((before - 4.0).abs() <= 1.0e-9, "1 x 1 x 4: {before}");

    // The Browser row's double-click is the explicit edit action for a
    // committed sketch; a single click only selects it.
    double_click_button(&mut harness, "Select Sketch 1");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    // Resize the rectangle through its own recipe.
    click_button(&mut harness, "Sketch dimension");
    let top_edge = canvas_sketch_point(&harness, SketchPoint::new(0.0, 0.5));
    click_at(&mut harness, top_edge);
    {
        let input = harness.get_by_role_and_label(Role::TextInput, "Rectangle width");
        input.scroll_to_me();
    }
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, "Rectangle width")
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, "Rectangle width")
        .type_text("2");
    harness.run();
    press_key(&mut harness, egui::Key::Enter);
    click_button(&mut harness, "Finish sketch");

    assert_eq!(
        harness.state().document_dirty_feature_count(),
        0,
        "finishing the edit replays the dependent extrusion by itself"
    );
    let after = harness
        .state()
        .displayed_measures()
        .expect("rebuilt extrusion")
        .volume;
    assert!(
        (after - 8.0).abs() <= 1.0e-9,
        "the extrusion follows the edited sketch: {after}"
    );
}

/// Editing a sketch from a rolled-back history cursor used to do nothing at
/// all. The edit action now returns the cursor to the end of history and
/// opens the sketch, so the Browser row works no matter where the scrubber
/// stands.
#[test]
fn a_rolled_back_history_cursor_still_opens_a_sketch_for_editing() {
    let mut harness = harness();
    prepare_finished_sketch(&mut harness, FeatureScenario::Extrude);
    commit_prepared_feature(&mut harness, FeatureScenario::Extrude);
    let end = harness.state().history_position();
    set_history_slider(&mut harness, 2);
    assert_eq!(harness.state().history_position(), 2, "rolled back");

    double_click_button(&mut harness, "Select Sketch 1");
    assert_eq!(
        harness.state().workbench_mode(),
        WorkbenchMode::Sketch,
        "the edit opens instead of silently refusing"
    );
    assert_eq!(
        harness.state().history_position(),
        end,
        "editing returned the cursor to the end of history"
    );
    assert!(
        harness.state().sketch_entity_count() >= 1,
        "the committed geometry is on the canvas"
    );
}

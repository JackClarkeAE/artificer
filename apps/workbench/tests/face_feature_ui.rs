use artificer_kernel::FaceRole;
use artificer_protocol::{Aabb3, FaceExtrusionOperation, KernelCommand, Point3, TopologyCounts};
use artificer_workbench::{
    ExtrusionMode, KernelLabApp, SketchExtrusionEligibility, WorkbenchMode, sketch::SketchPoint,
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

const CONFIRM_OPERATION: &str = "Confirm operation";
const CANCEL_OPERATION: &str = "Cancel operation";

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

fn drag_at(harness: &mut Harness<'static, KernelLabApp>, start: egui::Pos2, end: egui::Pos2) {
    harness.hover_at(start);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn type_active_dimension(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn prepare_active_one_by_one_face_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "Positive Z face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::PositiveZ)
    );

    click_button(harness, "Sketch on selected face");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(harness.state().sketch_is_face_supported());
    assert!(harness.state().sketch_support_label().starts_with("Face #"));
    let (triangles, edges) = harness
        .state()
        .face_sketch_context_counts()
        .expect("face sketch should retain its projected body context");
    assert!(triangles >= 2);
    assert!(edges >= 4);
    click_button(harness, "Properties");
    assert!(
        harness
            .query_by_label("Authoritative face-local frame · reference boundary")
            .is_some()
    );

    click_button(harness, "Two-point rectangle");
    click_at(
        harness,
        canvas_sketch_point(harness, SketchPoint::new(-0.5, -0.5)),
    );
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle width", "1");
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle height", "1");

    press_key(harness, egui::Key::Enter);
    // Strokes commit as they are drawn: accepting the dimensions publishes
    // the rectangle immediately, with nothing pending behind a tick.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert!(!harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn prepare_one_by_one_face_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    prepare_active_one_by_one_face_rectangle(harness);
    // Finishing is one action now; no confirmation step follows.
    click_button(harness, "Finish sketch command");
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn finish_centered_face_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    width: &str,
    height: &str,
) {
    let half_width = width.parse::<f64>().expect("numeric rectangle width") * 0.5;
    let half_height = height.parse::<f64>().expect("numeric rectangle height") * 0.5;
    click_button(harness, "Two-point rectangle");
    click_at(
        harness,
        canvas_sketch_point(harness, SketchPoint::new(-half_width, -half_height)),
    );
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle width", width);
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle height", height);
    press_key(harness, egui::Key::Enter);
    press_key(harness, egui::Key::Enter);
    assert_eq!(harness.state().sketch_entity_count(), 1);

    click_button(harness, "Finish sketch command");
    press_key(harness, egui::Key::Enter);
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn select_latest_feature_end(harness: &mut Harness<'static, KernelLabApp>) {
    {
        let node = harness
            .query_all_by_role_and_label(Role::Button, "Feature end face")
            .last()
            .expect("latest generated rectangular end or floor face");
        // The bound semantic target names the exact B-rep face even when its
        // projected label overlaps a nearer wall in the isometric camera.
        node.click_accesskit();
    }
    harness.run();
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::FeatureEnd)
    );
}

fn set_extrusion_distance(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    {
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(|node| {
                node.value()
                    .as_deref()
                    .is_some_and(|value| value.starts_with("Distance "))
            })
            .expect("extrusion distance control");
        distance.scroll_to_me();
    }
    harness.run();
    {
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(|node| {
                node.value()
                    .as_deref()
                    .is_some_and(|value| value.starts_with("Distance "))
            })
            .expect("visible extrusion distance control");
        distance.click();
    }
    harness.run();
    harness.event(egui::Event::Text(value.to_owned()));
    harness.run();
}

fn assert_topology(harness: &Harness<'static, KernelLabApp>, expected: TopologyCounts) {
    assert_eq!(harness.state().displayed_topology_counts(), Some(expected));
}

fn assert_measures(
    harness: &Harness<'static, KernelLabApp>,
    volume: f64,
    surface_area: f64,
    centroid_z: f64,
    maximum_z: f64,
) {
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed feature-chain measures");
    assert!((measures.volume - volume).abs() <= 1.0e-9);
    assert!((measures.surface_area - surface_area).abs() <= 1.0e-9);
    let centroid = measures.centroid.expect("committed solid centroid");
    assert!((centroid.x - 1.0).abs() <= 1.0e-9);
    assert!((centroid.y - 1.5).abs() <= 1.0e-9);
    assert!((centroid.z - centroid_z).abs() <= 1.0e-9);
    assert_eq!(
        measures.bounds,
        Some(Aabb3::new(
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(2.0, 3.0, maximum_z),
        ))
    );
}

#[test]
fn active_face_sketch_can_extrude_directly_with_lossless_cancel() {
    let mut harness = harness();
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();
    prepare_active_one_by_one_face_rectangle(&mut harness);

    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a certified active face profile should enable Extrude"
    );

    click_button(&mut harness, "Extrude");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    assert!(!harness.state().sketch_finished());
    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts
    );

    click_button(&mut harness, CANCEL_OPERATION);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(!harness.state().sketch_finished());
    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts
    );

    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 1
    );
    assert!((harness.state().displayed_measures().unwrap().volume - 25.0).abs() <= 1.0e-9);
    assert!(
        harness
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == "Add 1")
    );
}

#[test]
fn selected_face_add_and_cut_preview_then_publish_only_through_global_confirmation() {
    for (mode, expected_volume, expected_feature) in [
        (ExtrusionMode::Add, 25.0, "Add 1"),
        (ExtrusionMode::Cut, 23.0, "Cut 1"),
    ] {
        let mut harness = harness();
        let original_snapshot = harness.state().displayed_snapshot_id();
        let original_attempts = harness.state().transaction_attempt_count();
        prepare_one_by_one_face_rectangle(&mut harness);

        click_button(&mut harness, "Extrude");
        assert!(harness.state().operation_confirmation_pending());
        assert_eq!(
            harness.state().pending_operation_label(),
            Some("Extrude finished sketch")
        );
        assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
        assert_eq!(
            harness.state().transaction_attempt_count(),
            original_attempts
        );
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Preview Add")
                .is_none(),
            "the ribbon command is the sole face-feature staging action"
        );

        assert!(
            !harness
                .get_by_role_and_label(Role::Button, mode.label_for_test())
                .accesskit_node()
                .is_disabled(),
            "face-feature mode stays editable during the live preview"
        );

        click_button(&mut harness, CANCEL_OPERATION);
        assert!(!harness.state().operation_confirmation_pending());
        assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
        assert_eq!(
            harness.state().transaction_attempt_count(),
            original_attempts
        );

        click_button(&mut harness, "Extrude");
        click_button(&mut harness, mode.label_for_test());
        set_extrusion_distance(
            &mut harness,
            if mode == ExtrusionMode::Cut {
                "-1"
            } else {
                "1"
            },
        );
        assert!(harness.state().operation_confirmation_pending());
        assert_eq!(harness.state().extrusion_mode(), mode);
        assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
        assert_eq!(
            harness.state().transaction_attempt_count(),
            original_attempts
        );

        click_button(&mut harness, CONFIRM_OPERATION);
        assert!(!harness.state().operation_confirmation_pending());
        assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
        assert_eq!(
            harness.state().transaction_attempt_count(),
            original_attempts + 1
        );
        assert_eq!(harness.state().last_error_code(), None);
        let measures = harness
            .state()
            .displayed_measures()
            .expect("feature measures");
        assert!((measures.volume - expected_volume).abs() <= 1.0e-9);
        assert!((measures.surface_area - 56.0).abs() <= 1.0e-9);
        assert!(
            harness
                .state()
                .feature_timeline_entries()
                .iter()
                .any(|entry| entry == expected_feature)
        );
        let committed_history = vec![
            "Origin".to_owned(),
            "Base body".to_owned(),
            "Sketch 1 · r1".to_owned(),
            expected_feature.to_owned(),
        ];
        assert_eq!(
            harness.state().feature_timeline_entries(),
            committed_history
        );

        let committed_sketch_support = harness.state().sketch_support_label();
        let committed_sketch_entities = harness.state().sketch_entity_count();
        click_button(&mut harness, "Feature end face");
        click_button(&mut harness, "Sketch 1 feature");
        assert_eq!(
            harness.state().workbench_mode(),
            WorkbenchMode::Model,
            "a face sketch from the prior body state must remain read-only until feature replay exists"
        );
        assert_eq!(harness.state().face_sketch_context_counts(), None);
        assert_eq!(
            harness.state().sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::StaleFaceSupport
        );
        assert_eq!(
            harness.state().sketch_support_label(),
            committed_sketch_support,
            "read-only history navigation must not retarget the committed sketch to the selected face"
        );
        assert_eq!(
            harness.state().sketch_entity_count(),
            committed_sketch_entities,
            "read-only history navigation must not replace committed sketch geometry"
        );
        assert_eq!(
            harness.state().feature_timeline_entries(),
            committed_history
        );

        click_button(&mut harness, "Model mode");
        click_button(&mut harness, "Sketch on selected face");
        assert!(harness.state().sketch_is_face_supported());
        let browser_sketch = format!(
            "└  Sketch 2 · {} · empty",
            harness.state().sketch_support_label()
        );
        click_button(&mut harness, "Browser");
        assert!(
            harness.query_by_label(&browser_sketch).is_some(),
            "the Browser and History must agree on the next sketch ordinal"
        );
        assert_eq!(
            harness.state().feature_timeline_entries(),
            committed_history,
            "starting the next face sketch must preserve prior committed history"
        );
    }
}

#[test]
fn selected_face_extrudes_directly_and_signed_distance_switches_to_cut() {
    let mut harness = harness();
    harness.run();
    let original = harness
        .state()
        .displayed_snapshot_id()
        .expect("bootstrap cuboid snapshot");
    let attempts = harness.state().transaction_attempt_count();

    click_button(&mut harness, "Positive Z face");
    let extrude = harness.get_by_role_and_label(Role::Button, "Extrude");
    assert!(
        !extrude.accesskit_node().is_disabled(),
        "a supported selected face should enable direct Extrude without a surrogate sketch"
    );
    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Push/pull selected face")
    );
    assert_eq!(harness.state().displayed_snapshot_id(), Some(original));
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert!(harness.query_by_label("PUSH/PULL PREVIEW").is_some());

    set_extrusion_distance(&mut harness, "-1");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Cut);
    assert_eq!(harness.state().displayed_snapshot_id(), Some(original));
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_ne!(harness.state().displayed_snapshot_id(), Some(original));
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_measures(&harness, 18.0, 42.0, 1.5, 3.0);
    assert_eq!(
        harness.state().feature_timeline_entries(),
        ["Origin", "Base body", "Cut 1"]
    );
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_label("◆  Body 1 · native pushed/pulled solid")
            .is_some()
    );
}

#[test]
fn repeated_face_add_cut_add_chain_uses_the_ribbon_and_global_confirmation() {
    let mut harness = harness();
    harness.run();
    let base_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("bootstrap cuboid snapshot");
    let base_attempts = harness.state().transaction_attempt_count();
    assert_topology(
        &harness,
        TopologyCounts {
            vertices: 8,
            edges: 12,
            coedges: 24,
            loops: 6,
            faces: 6,
            shells: 1,
            solids: 1,
        },
    );
    assert_measures(&harness, 24.0, 52.0, 2.0, 4.0);

    click_button(&mut harness, "Positive Z face");
    click_button(&mut harness, "Sketch on selected face");
    finish_centered_face_rectangle(&mut harness, "1", "1");

    // The ribbon action stages only presentation intent. Both the committed
    // B-rep and transaction counter stay neutral when that intent is declined.
    click_button(&mut harness, "M  Move");
    assert_eq!(harness.state().active_tool_label(), "Move");
    click_button(&mut harness, "Extrude");
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().active_tool_label(), "Select");
    assert!(!harness.state().transform_preview_pending());
    let transform_before_drag = harness.state().displayed_transform();
    let viewport_center = harness.get_by_label("Model viewport").rect().center();
    drag_at(
        &mut harness,
        viewport_center,
        viewport_center + egui::vec2(32.0, 18.0),
    );
    assert_eq!(harness.state().displayed_transform(), transform_before_drag);
    assert!(!harness.state().transform_preview_pending());
    assert_eq!(harness.state().displayed_snapshot_id(), Some(base_snapshot));
    assert_eq!(harness.state().transaction_attempt_count(), base_attempts);
    click_button(&mut harness, CANCEL_OPERATION);
    assert_eq!(harness.state().displayed_snapshot_id(), Some(base_snapshot));
    assert_eq!(harness.state().transaction_attempt_count(), base_attempts);

    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);
    let add_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("first Add snapshot");
    assert_ne!(add_snapshot, base_snapshot);
    assert_topology(
        &harness,
        TopologyCounts {
            vertices: 16,
            edges: 24,
            coedges: 48,
            loops: 12,
            faces: 11,
            shells: 1,
            solids: 1,
        },
    );
    assert_measures(&harness, 25.0, 56.0, 2.1, 5.0);

    select_latest_feature_end(&mut harness);
    click_button(&mut harness, "Sketch on selected face");
    finish_centered_face_rectangle(&mut harness, "0.5", "0.5");
    click_button(&mut harness, "Cut");
    set_extrusion_distance(&mut harness, "-0.5");
    click_button(&mut harness, "Extrude");
    assert_eq!(harness.state().displayed_snapshot_id(), Some(add_snapshot));
    click_button(&mut harness, CONFIRM_OPERATION);
    let cut_snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("Cut snapshot");
    assert_ne!(cut_snapshot, add_snapshot);
    assert_topology(
        &harness,
        TopologyCounts {
            vertices: 24,
            edges: 36,
            coedges: 72,
            loops: 18,
            faces: 16,
            shells: 1,
            solids: 1,
        },
    );
    assert_measures(&harness, 24.875, 57.0, 51.90625 / 24.875, 5.0);

    select_latest_feature_end(&mut harness);
    click_button(&mut harness, "Sketch on selected face");
    click_button(&mut harness, "Snap");
    finish_centered_face_rectangle(&mut harness, "0.25", "0.25");
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready,
        "the newest generated floor must remain a supported rectangular face"
    );
    set_extrusion_distance(&mut harness, "0.25");
    click_button(&mut harness, "Extrude");
    assert_eq!(harness.state().displayed_snapshot_id(), Some(cut_snapshot));
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_ne!(harness.state().displayed_snapshot_id(), Some(cut_snapshot));
    assert_topology(
        &harness,
        TopologyCounts {
            vertices: 32,
            edges: 48,
            coedges: 96,
            loops: 24,
            faces: 21,
            shells: 1,
            solids: 1,
        },
    );
    assert_measures(&harness, 24.890625, 57.25, 51.978515625 / 24.890625, 5.0);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        base_attempts + 3
    );
    assert_eq!(harness.state().last_error_code(), None);
    assert!(harness.query_by_label("EXTRUSION COMMITTED").is_some());
    assert!(harness.query_by_label("Solid · valid").is_some());

    let history = vec![
        "Origin".to_owned(),
        "Base body".to_owned(),
        "Sketch 1 · r1".to_owned(),
        "Add 1".to_owned(),
        "Sketch 2 · r1".to_owned(),
        "Cut 1".to_owned(),
        "Sketch 3 · r1".to_owned(),
        "Add 2".to_owned(),
    ];
    assert_eq!(harness.state().feature_timeline_entries(), history);
    for label in [
        "Sketch 1 feature",
        "Add 1 feature",
        "Sketch 2 feature",
        "Cut 1 feature",
        "Sketch 3 feature",
        "Add 2 feature",
    ] {
        assert!(
            harness
                .query_by_role_and_label(Role::Button, label)
                .is_some(),
            "History is missing {label}"
        );
    }
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_label("◆  Body 1 · native added boss")
            .is_some()
    );
    assert_eq!(harness.state().sketch_count(), 3);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Select Sketch 3")
            .is_some(),
        "Browser and History disagree on the final sketch"
    );
}

#[test]
fn generated_annular_shoulder_rejects_a_profile_drawn_inside_its_void() {
    let mut harness = harness();
    prepare_one_by_one_face_rectangle(&mut harness);
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);
    let committed = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    {
        let shoulder = harness
            .query_all_by_role_and_label(Role::Button, "Positive Z face")
            .next()
            .expect("generated Positive Z shoulder patch");
        shoulder.click_accesskit();
    }
    harness.run();
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::PositiveZ),
        "the semantic face target must select its bound shoulder entity"
    );
    click_button(&mut harness, "Sketch on selected face");
    click_button(&mut harness, "Snap");
    finish_centered_face_rectangle(&mut harness, "0.1", "0.1");
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::ProfileOutsideSupport
    );
    let extrude = harness.get_by_role_and_label(Role::Button, "Extrude");
    assert!(extrude.accesskit_node().is_disabled());
    extrude.click_accesskit();
    harness.run();
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().displayed_snapshot_id(), committed);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
}

#[test]
fn rotated_face_support_commits_an_exact_add_through_the_unified_kernel() {
    let mut harness = harness();
    harness.run();
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();

    click_button(&mut harness, "R  Rotate");
    let viewport = harness.get_by_label("Model viewport").rect();
    drag_at(
        &mut harness,
        viewport.center() + egui::vec2(-80.0, 40.0),
        viewport.center() + egui::vec2(-38.0, 17.0),
    );
    assert!(harness.state().transform_preview_pending());
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 1
    );
    let transformed_snapshot = harness.state().displayed_snapshot_id();

    click_button(&mut harness, "V  Select");
    {
        let face = harness
            .query_all_by_role_and_label(Role::Button, "Positive Z face")
            .next()
            .expect("rotated rectangular face");
        face.click_accesskit();
    }
    harness.run();
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::PositiveZ)
    );
    click_button(&mut harness, "Sketch on selected face");
    finish_centered_face_rectangle(&mut harness, "1", "1");
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    let extrude = harness.get_by_role_and_label(Role::Button, "Extrude");
    assert!(!extrude.accesskit_node().is_disabled());
    extrude.click_accesskit();
    harness.run();
    assert!(harness.state().operation_confirmation_pending());
    let Some(KernelCommand::ExtrudeFacePlanarProfile {
        distance,
        operation,
        profile,
        ..
    }) = harness.state().pending_sketch_extrusion_command()
    else {
        panic!("rotated face Add must stage the unified exact profile command")
    };
    assert_eq!(operation, FaceExtrusionOperation::Add);
    assert!((distance - 1.0).abs() <= 1.0e-9);
    assert_eq!(profile.regions.len(), 1);
    assert_eq!(profile.regions[0].outer.curves.len(), 4);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(!harness.state().operation_confirmation_pending());
    assert_ne!(
        harness.state().displayed_snapshot_id(),
        transformed_snapshot
    );
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 2
    );
    assert_eq!(harness.state().last_error_code(), None);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("rotated exact Add measures");
    assert!((measures.volume - 25.0).abs() <= 1.0e-9);
    assert!((measures.surface_area - 56.0).abs() <= 1.0e-9);
    assert_topology(
        &harness,
        TopologyCounts {
            vertices: 16,
            edges: 24,
            coedges: 48,
            loops: 12,
            faces: 11,
            shells: 1,
            solids: 1,
        },
    );
    assert_eq!(
        harness.state().feature_timeline_entries(),
        [
            "Origin",
            "Base body",
            "Transform 1",
            "Sketch 1 · r1",
            "Add 1",
        ]
        .map(str::to_owned)
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Add 1 feature")
            .is_some()
    );
}

trait ExtrusionModeTestLabel {
    fn label_for_test(self) -> &'static str;
}

impl ExtrusionModeTestLabel for ExtrusionMode {
    fn label_for_test(self) -> &'static str {
        match self {
            ExtrusionMode::NewBody => "New body",
            ExtrusionMode::Add => "Add",
            ExtrusionMode::Cut => "Cut",
        }
    }
}

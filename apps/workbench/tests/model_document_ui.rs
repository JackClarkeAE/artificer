use artificer_kernel::FaceRole;
use artificer_protocol::SnapshotId;
use artificer_workbench::{
    ExtrusionMode, KernelLabApp, SketchExtrusionEligibility, WorkbenchMode, sketch::SketchPoint,
};
use egui::accesskit::Role;
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

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
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

fn commit_centered_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    width: f64,
    height: f64,
) {
    commit_rectangle(harness, SketchPoint::new(0.0, 0.0), width, height);
}

fn commit_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    center: SketchPoint,
    width: f64,
    height: f64,
) {
    click_button(harness, "Two-point rectangle");
    click_at(
        harness,
        canvas_sketch_point(
            harness,
            SketchPoint::new(center.u - width * 0.5, center.v - height * 0.5),
        ),
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

fn finish_active_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Finish sketch");
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn assert_extrude_enabled(harness: &Harness<'static, KernelLabApp>) {
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a confirmed closed sketch on the current support must enable Extrude"
    );
}

fn create_origin_extruded_body(harness: &mut Harness<'static, KernelLabApp>) -> (u64, SnapshotId) {
    harness.run();
    let initial_attempts = harness.state().transaction_attempt_count();

    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    commit_centered_rectangle(harness, 4.0, 2.0);
    finish_active_sketch(harness);
    assert_extrude_enabled(harness);
    click_button(harness, "Extrude");
    click_button(harness, CONFIRM_OPERATION);
    let snapshot = harness
        .state()
        .displayed_snapshot_id()
        .expect("origin sketch extrusion snapshot");
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts + 1
    );
    (initial_attempts, snapshot)
}

fn begin_top_face_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Extrusion top face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::ExtrusionTop)
    );
    click_button(harness, "Sketch on selected face");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(harness.state().sketch_is_face_supported());
    commit_centered_rectangle(harness, 1.0, 1.0);
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Add);
}

#[test]
fn staged_face_extrusion_arrow_drags_through_the_complete_workbench() {
    let mut harness = harness();
    create_origin_extruded_body(&mut harness);
    begin_top_face_sketch(&mut harness);
    finish_active_sketch(&mut harness);
    assert_extrude_enabled(&harness);
    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch")
    );

    let initial = harness.state().extrusion_distance();
    let handle = harness
        .get_by_role_and_label(Role::Slider, "Extrusion distance handle")
        .rect()
        .center();
    let first = handle + egui::vec2(24.0, -14.0);
    let second = handle + egui::vec2(49.0, -28.0);
    let finish = handle + egui::vec2(73.0, -41.0);
    harness.event(egui::Event::PointerMoved(handle));
    harness.event(egui::Event::PointerButton {
        pos: handle,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.event(egui::Event::PointerMoved(first));
    harness.step();
    let after_first_frame = harness.state().extrusion_distance();

    // Deliberately leave the pointer down across an idle frame. Previously
    // the async preview cache was cleared here, the arrow disappeared for one
    // frame, and capture was cancelled after this first tiny movement.
    harness.step();
    assert!(
        harness
            .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .accesskit_node()
            .is_disabled(),
        "confirmation must remain gated while the live handle owns the pointer"
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Cancel operation")
            .accesskit_node()
            .is_disabled(),
        "cancellation must also remain gated until the live handle is released"
    );
    harness.event(egui::Event::PointerMoved(second));
    harness.step();
    let after_second_frame = harness.state().extrusion_distance();
    harness.event(egui::Event::PointerMoved(finish));
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: finish,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    // The confirmation panel precedes the viewport in the UI pass, so one
    // repaint publishes its newly released state to accessibility as well.
    harness.step();

    assert!(
        (harness.state().extrusion_distance() - initial).abs() > 0.05,
        "the production workbench must route the captured arrow drag into extrusion intent"
    );
    assert!(
        (after_second_frame - after_first_frame).abs() > 0.05,
        "one continuous hold must keep changing the distance after the async replacement frame"
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .accesskit_node()
            .is_disabled(),
        "confirmation becomes available only after the handle is released"
    );
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch"),
        "dragging updates the staged preview without bypassing confirmation"
    );
}

#[test]
fn browser_body_visibility_control_removes_hidden_body_from_selection() {
    let mut harness = harness();
    harness.run();
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    click_button(&mut harness, "Positive Z face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::PositiveZ)
    );

    click_button(&mut harness, "Hide Body 1");
    assert_eq!(
        harness.state().selected_face(),
        None,
        "hiding a body must clear selection owned by that body"
    );
    assert!(
        harness
            .query_all_by_role_and_label(Role::Button, "Positive Z face")
            .next()
            .is_none(),
        "hidden body faces must not remain selectable in the viewport"
    );
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);

    click_button(&mut harness, "Show Body 1");
    assert!(
        harness
            .query_all_by_role_and_label(Role::Button, "Positive Z face")
            .next()
            .is_some(),
        "showing a body must restore its viewport selection targets"
    );
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
}

#[test]
fn finished_face_sketch_stays_visible_until_hidden_or_extruded() {
    let mut harness = harness();
    harness.run();
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    click_button(&mut harness, "Positive Z face");
    click_button(&mut harness, "Sketch on selected face");
    commit_centered_rectangle(&mut harness, 1.0, 1.0);
    finish_active_sketch(&mut harness);
    assert_extrude_enabled(&harness);

    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some(),
        "a finished, unconsumed sketch should default to visible in Model mode"
    );
    click_button(&mut harness, "Hide Sketch 1");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Sketch 1")
            .is_some()
    );
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);

    click_button(&mut harness, "Show Sketch 1");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some()
    );
    assert_extrude_enabled(&harness);
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Sketch 1")
            .is_some(),
        "a consumed sketch should auto-hide after its extrusion commits"
    );
}

#[test]
fn fresh_process_load_keeps_origin_sketch_visible_and_separately_extrudable() {
    let mut source = harness();
    source.run();
    click_button(&mut source, "XY Plane");
    click_button(&mut source, "Sketch mode");
    commit_centered_rectangle(&mut source, 4.0, 2.0);
    finish_active_sketch(&mut source);
    let saved = source.state().native_document_json().unwrap();

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("portable origin sketch should replay in a fresh app");
    restored.run();

    assert_eq!(restored.state().native_document_json().unwrap(), saved);
    assert_eq!(restored.state().sketch_count(), 1);
    assert_eq!(restored.state().visible_model_sketch_overlay_count(), 1);
    assert_extrude_enabled(&restored);
    click_button(&mut restored, "Extrude");
    click_button(&mut restored, CONFIRM_OPERATION);

    assert!((restored.state().displayed_measures().unwrap().volume - 32.0).abs() <= 1.0e-9);
    assert_eq!(restored.state().component_instance_count(), 0);
    click_button(&mut restored, "Browser");
    assert!(
        restored
            .query_by_role_and_label(Role::Button, "Show Sketch 1")
            .is_some(),
        "the loaded sketch should auto-hide only after its later extrusion commits"
    );
}

#[test]
fn fresh_process_load_resolves_face_support_and_extrudes_loaded_sketch() {
    let mut source = harness();
    source.run();
    click_button(&mut source, "Positive Z face");
    click_button(&mut source, "Sketch on selected face");
    commit_centered_rectangle(&mut source, 1.0, 1.0);
    finish_active_sketch(&mut source);
    let saved = source.state().native_document_json().unwrap();

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("persistent face support should resolve from regenerated reports");
    restored.run();

    assert!(restored.state().sketch_is_face_supported());
    assert_eq!(restored.state().visible_model_sketch_overlay_count(), 1);
    assert_extrude_enabled(&restored);
    click_button(&mut restored, "Extrude");
    click_button(&mut restored, CONFIRM_OPERATION);

    assert!((restored.state().displayed_measures().unwrap().volume - 25.0).abs() <= 1.0e-9);
    assert_eq!(restored.state().last_error_code(), None);
    assert_eq!(
        restored.state().feature_timeline_entries(),
        ["Origin", "Base body", "Sketch 1 · r1", "Add 1"].map(str::to_owned)
    );
}

#[test]
fn two_new_body_extrusions_coexist_and_have_independent_browser_visibility() {
    let mut harness = harness();
    let (_, first_body_snapshot) = create_origin_extruded_body(&mut harness);

    click_button(&mut harness, "New sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(!harness.state().sketch_is_face_supported());
    commit_rectangle(&mut harness, SketchPoint::new(4.0, 0.0), 2.0, 1.0);
    finish_active_sketch(&mut harness);
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::NewBody);
    assert_extrude_enabled(&harness);
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_ne!(
        harness.state().displayed_snapshot_id(),
        Some(first_body_snapshot)
    );
    let (document_center, document_radius) = harness.state().view_frame();
    assert!((document_center.x - 1.5).abs() <= 1.0e-12);
    assert!(document_center.y.abs() <= 1.0e-12);
    assert!((document_center.z - 2.0).abs() <= 1.0e-12);
    assert!((document_radius - 17.25_f64.sqrt()).abs() <= 1.0e-12);
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
    assert_eq!(
        harness.state().feature_timeline_entries(),
        [
            "Origin",
            "Base body",
            "Sketch 1 · r1",
            "Extrude 1",
            "Sketch 2 · r1",
            "Extrude 2",
        ]
        .map(str::to_owned)
    );

    let attempts = harness.state().transaction_attempt_count();
    click_button(&mut harness, "Hide Body 1");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Body 1")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Body 2")
            .is_some()
    );

    click_button(&mut harness, "Hide Body 2");
    assert!(
        harness
            .query_all_by_role_and_label(Role::Button, "Extrusion top face")
            .next()
            .is_none(),
        "no face-selection target may survive when both bodies are hidden"
    );

    click_button(&mut harness, "Show Body 2");
    assert!(
        harness
            .query_all_by_role_and_label(Role::Button, "Extrusion top face")
            .next()
            .is_some(),
        "showing Body 2 alone must restore Body 2's selection targets"
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show Body 1")
            .is_some(),
        "Body 1 must remain independently hidden"
    );
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
}

#[test]
fn confirmed_active_sketch_on_origin_extruded_body_top_face_can_extrude_directly() {
    let mut harness = harness();
    let (initial_attempts, first_extrusion) = create_origin_extruded_body(&mut harness);
    begin_top_face_sketch(&mut harness);

    assert!(!harness.state().sketch_finished());
    assert_extrude_enabled(&harness);
    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    click_button(&mut harness, CONFIRM_OPERATION);

    assert!(harness.state().sketch_finished());
    assert_ne!(
        harness.state().displayed_snapshot_id(),
        Some(first_extrusion)
    );
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts + 2
    );
    assert_eq!(harness.state().last_error_code(), None);
}

/// Regression for the exact user path that differs from the older
/// cuboid-face tests: first create a body from an origin sketch, then sketch on
/// that extrusion's generated top face, finish the second sketch, and extrude
/// it as an Add feature.
#[test]
fn finished_sketch_on_origin_extruded_body_top_face_can_extrude() {
    let mut harness = harness();
    let (initial_attempts, first_extrusion) = create_origin_extruded_body(&mut harness);
    begin_top_face_sketch(&mut harness);
    finish_active_sketch(&mut harness);

    assert_extrude_enabled(&harness);
    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch")
    );
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_ne!(
        harness.state().displayed_snapshot_id(),
        Some(first_extrusion)
    );
    assert_eq!(
        harness.state().transaction_attempt_count(),
        initial_attempts + 2
    );
    assert_eq!(harness.state().last_error_code(), None);
    assert_eq!(
        harness.state().feature_timeline_entries(),
        [
            "Origin",
            "Base body",
            "Sketch 1 · r1",
            "Extrude 1",
            "Sketch 2 · r1",
            "Add 1",
        ]
        .map(str::to_owned)
    );
}

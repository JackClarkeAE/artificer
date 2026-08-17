use artificer_geometry::ProfileWinding;
use artificer_kernel::FaceRole;
use artificer_workbench::{
    KernelLabApp, SketchExtrusionEligibility, WorkbenchMode,
    sketch::{
        CertifiedProfileStatus, DimensionInputError, SketchDimensionKind, SketchPlane, SketchPoint,
    },
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

const CONFIRM_OPERATION: &str = "Confirm operation";
const CANCEL_OPERATION: &str = "Cancel operation";

fn harness(size: [f32; 2]) -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size(size)
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
    // Panels are rendered before the central canvas. A canvas release can
    // therefore stage an operation after the bottom rail rendered; one clean
    // frame makes the new confirmation controls observable.
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

fn click_at_with_modifiers(
    harness: &mut Harness<'static, KernelLabApp>,
    position: egui::Pos2,
    modifiers: egui::Modifiers,
) {
    harness.input_mut().modifiers = modifiers;
    harness.event(egui::Event::PointerMoved(position));
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
        harness.step();
    }
    harness.input_mut().modifiers = egui::Modifiers::NONE;
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

fn enter_sketch(harness: &mut Harness<'static, KernelLabApp>, plane: &str) {
    click_button(harness, plane);
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn enter_positive_z_face_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "Positive Z face");
    click_button(harness, "Sketch on selected face");
    for _ in 0..18 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(harness.state().sketch_is_face_supported());
}

fn choose_sketch_tool(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    click_button(harness, label);
    let controller = match label {
        "Select sketch geometry" => "Select",
        "Sketch point" => "Point",
        "Single line" | "Chained polyline" => "Line",
        "Centreline" => "Centre line",
        "Two-point rectangle" | "Centre-point rectangle" => "Rectangle",
        "Centre-point circle" | "Two-point diameter circle" => "Circle",
        "Centre-start-end arc" | "Three-point arc" => "Arc",
        _ => return,
    };
    assert_eq!(harness.state().sketch_tool_label(), controller);
}

fn canvas_point(harness: &Harness<'static, KernelLabApp>, offset: egui::Vec2) -> egui::Pos2 {
    harness.get_by_label("Sketch viewport").rect().center() + offset
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn assert_kernel_unchanged(
    harness: &Harness<'static, KernelLabApp>,
    snapshot: Option<artificer_protocol::SnapshotId>,
    attempts: u64,
) {
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
}

fn dimension_value(harness: &Harness<'static, KernelLabApp>, kind: SketchDimensionKind) -> f64 {
    harness
        .state()
        .sketch_dimension_readouts()
        .into_iter()
        .find(|readout| readout.kind == kind)
        .unwrap_or_else(|| panic!("missing dimension readout {kind:?}"))
        .value
}

fn type_active_dimension(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn begin_first_dimension(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    press_key(harness, egui::Key::Tab);
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, label)
            .is_some(),
        "Tab did not activate {label}"
    );
}

fn stage_exact_xy_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    choose_sketch_tool(harness, "Two-point rectangle");
    let origin = canvas_point(harness, egui::Vec2::ZERO);
    click_at(harness, origin);

    begin_first_dimension(harness, "Rectangle width");
    type_active_dimension(harness, "Rectangle width", "4");
    assert!((dimension_value(harness, SketchDimensionKind::Width) - 4.0).abs() <= 1.0e-12);

    press_key(harness, egui::Key::Tab);
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle height")
            .is_some()
    );
    type_active_dimension(harness, "Rectangle height", "2");
    assert!((dimension_value(harness, SketchDimensionKind::Height) - 2.0).abs() <= 1.0e-12);

    // This Enter belongs to the active dimension; accepting it completes
    // the stroke, which commits itself immediately.
    press_key(harness, egui::Key::Enter);
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
}

fn commit_exact_xy_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    stage_exact_xy_rectangle(harness);
    assert!(!harness.state().operation_confirmation_pending());
}

fn finish_exact_xy_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    commit_exact_xy_rectangle(harness);
    click_button(harness, "Finish sketch");

    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
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

#[test]
fn startup_shell_selects_each_origin_plane_without_touching_the_kernel() {
    let mut harness = harness([1280.0, 800.0]);
    harness.run();
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().selected_origin_plane(), SketchPlane::XY);
    assert!(harness.query_by_label("Artificer Workbench").is_some());
    assert!(harness.query_by_label("Document 1").is_some());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Origin")
            .is_some()
    );
    assert!(harness.query_by_label("Body 1 · native cuboid").is_some());

    for (label, plane) in [
        ("YZ Plane", SketchPlane::YZ),
        ("XZ Plane", SketchPlane::XZ),
        ("XY Plane", SketchPlane::XY),
    ] {
        click_button(&mut harness, label);
        assert_eq!(harness.state().selected_origin_plane(), plane);
        assert_eq!(harness.state().sketch_plane(), plane);
        assert_kernel_unchanged(&harness, snapshot, attempts);
        assert!(!harness.state().operation_confirmation_pending());
    }
}

#[test]
fn entering_sketch_exposes_the_selected_orthographic_plane_and_all_creation_tools() {
    let mut harness = harness([1280.0, 800.0]);
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    enter_sketch(&mut harness, "YZ Plane");

    assert_eq!(harness.state().selected_origin_plane(), SketchPlane::YZ);
    assert_eq!(harness.state().sketch_plane(), SketchPlane::YZ);
    assert!(
        harness
            .query_by_label("YZ Plane · orthographic sketch")
            .is_some()
    );
    assert!(harness.query_by_label("YZ · ORTHOGRAPHIC").is_some());
    assert!(harness.query_by_label("Sketch viewport").is_some());
    for tool in [
        "Select sketch geometry",
        "Sketch point",
        "Single line",
        "Two-point rectangle",
        "Centre-point circle",
        "Centre-start-end arc",
    ] {
        assert!(
            harness
                .query_by_role_and_label(Role::Button, tool)
                .is_some(),
            "missing sketch tool {tool}"
        );
    }
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn mode_roundtrip_cancels_incomplete_rectangle_and_arc_drafts() {
    struct DraftGesture {
        tool: &'static str,
        pending: &'static str,
        clicks_before_roundtrip: Vec<egui::Vec2>,
        fresh_clicks: Vec<egui::Vec2>,
    }

    let gestures = [
        DraftGesture {
            tool: "Two-point rectangle",
            pending: "Add sketch rectangle",
            clicks_before_roundtrip: vec![egui::vec2(-84.0, 56.0)],
            fresh_clicks: vec![egui::vec2(-56.0, 28.0), egui::vec2(84.0, -56.0)],
        },
        DraftGesture {
            tool: "Centre-start-end arc",
            pending: "Add sketch arc",
            clicks_before_roundtrip: vec![egui::vec2(-84.0, 0.0), egui::vec2(0.0, 56.0)],
            fresh_clicks: vec![
                egui::vec2(-28.0, 0.0),
                egui::vec2(56.0, 0.0),
                egui::vec2(0.0, -56.0),
            ],
        },
    ];

    for gesture in gestures {
        let mut harness = harness([1280.0, 800.0]);
        enter_sketch(&mut harness, "XY Plane");
        choose_sketch_tool(&mut harness, gesture.tool);
        let snapshot = harness.state().displayed_snapshot_id();
        let attempts = harness.state().transaction_attempt_count();
        let revision = harness.state().sketch_revision();

        for offset in &gesture.clicks_before_roundtrip {
            let point = canvas_point(&harness, *offset);
            click_at(&mut harness, point);
        }
        assert!(harness.state().sketch_creation_draft_active());
        assert!(!harness.state().operation_confirmation_pending());

        click_button(&mut harness, "Model mode");
        assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
        assert!(!harness.state().sketch_creation_draft_active());
        assert_kernel_unchanged(&harness, snapshot, attempts);

        click_button(&mut harness, "Sketch mode");
        assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
        assert!(!harness.state().sketch_creation_draft_active());

        for (index, offset) in gesture.fresh_clicks.iter().enumerate() {
            let point = canvas_point(&harness, *offset);
            click_at(&mut harness, point);
            if index + 1 < gesture.fresh_clicks.len() {
                assert!(harness.state().sketch_creation_draft_active());
                assert!(!harness.state().operation_confirmation_pending());
                assert_eq!(harness.state().sketch_entity_count(), 0);
                assert_eq!(harness.state().sketch_revision(), revision);
            }
        }

        // Completing the fresh gesture commits it; the roundtrip only ever
        // discarded presentation-only draft clicks.
        assert!(!harness.state().sketch_creation_draft_active());
        assert_eq!(harness.state().pending_operation_label(), None);
        let _ = gesture.pending;
        assert_eq!(harness.state().sketch_entity_count(), 1);
        assert_eq!(harness.state().sketch_revision(), revision + 1);
        assert_kernel_unchanged(&harness, snapshot, attempts);
    }
}

#[test]
fn point_rectangle_circle_and_arc_gestures_commit_as_they_complete() {
    struct Gesture {
        tool: &'static str,
        offsets: Vec<egui::Vec2>,
    }

    let gestures = [
        Gesture {
            tool: "Sketch point",
            offsets: vec![egui::vec2(0.0, 0.0)],
        },
        Gesture {
            tool: "Two-point rectangle",
            offsets: vec![egui::vec2(-84.0, 56.0), egui::vec2(84.0, -56.0)],
        },
        Gesture {
            tool: "Centre-point circle",
            offsets: vec![egui::vec2(-28.0, 0.0), egui::vec2(84.0, 0.0)],
        },
        Gesture {
            tool: "Centre-start-end arc",
            offsets: vec![
                egui::vec2(0.0, 0.0),
                egui::vec2(84.0, 0.0),
                egui::vec2(0.0, -84.0),
            ],
        },
    ];

    for gesture in gestures {
        let mut harness = harness([1280.0, 800.0]);
        enter_sketch(&mut harness, "XY Plane");
        choose_sketch_tool(&mut harness, gesture.tool);
        let snapshot = harness.state().displayed_snapshot_id();
        let attempts = harness.state().transaction_attempt_count();
        let revision = harness.state().sketch_revision();

        for offset in gesture.offsets {
            let point = canvas_point(&harness, offset);
            click_at(&mut harness, point);
        }

        // The completed gesture commits itself: no pending operation, one
        // new entity, and no kernel transaction — the sketch journal is the
        // undo path.
        assert!(!harness.state().operation_confirmation_pending());
        assert_eq!(harness.state().pending_operation_label(), None);
        assert_eq!(harness.state().sketch_entity_count(), 1);
        assert_eq!(harness.state().sketch_revision(), revision + 1);
        assert_kernel_unchanged(&harness, snapshot, attempts);
    }
}

#[test]
fn rectangle_dimensions_stage_once_then_require_a_separate_global_confirmation() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    stage_exact_xy_rectangle(&mut harness);
    assert_kernel_unchanged(&harness, snapshot, attempts);

    // A distinct Enter confirms the already-staged edit. This proves the
    // dimension Enter above cannot leak into the global confirmation gate.
    press_key(&mut harness, egui::Key::Enter);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert!(!harness.state().operation_confirmation_pending());
    assert!((dimension_value(&harness, SketchDimensionKind::Width) - 4.0).abs() <= 1.0e-12);
    assert!((dimension_value(&harness, SketchDimensionKind::Height) - 2.0).abs() <= 1.0e-12);
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn circle_diameter_dimension_is_exact_before_and_after_confirmation() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Centre-point circle");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    let center = canvas_point(&harness, egui::Vec2::ZERO);
    click_at(&mut harness, center);
    begin_first_dimension(&mut harness, "Circle diameter");
    type_active_dimension(&mut harness, "Circle diameter", "5.5");
    assert!((dimension_value(&harness, SketchDimensionKind::Diameter) - 5.5).abs() <= 1.0e-12);

    press_key(&mut harness, egui::Key::Enter);
    // Accepting the diameter completes the stroke, which commits itself
    // with the typed value intact.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert!((dimension_value(&harness, SketchDimensionKind::Diameter) - 5.5).abs() <= 1.0e-12);
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn invalid_active_dimension_blocks_both_enter_and_the_green_tick() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    // Draft a rectangle whose typed width is invalid: nothing may commit
    // while the error stands, however many times Enter is pressed.
    choose_sketch_tool(&mut harness, "Two-point rectangle");
    let first_corner = canvas_point(&harness, egui::Vec2::ZERO);
    click_at(&mut harness, first_corner);
    begin_first_dimension(&mut harness, "Rectangle width");
    type_active_dimension(&mut harness, "Rectangle width", "0");
    harness.run_steps(2);
    assert_eq!(
        harness.state().sketch_dimension_error(),
        Some(DimensionInputError::NonPositive)
    );

    press_key(&mut harness, egui::Key::Enter);
    harness.run_steps(2);
    assert_eq!(
        harness.state().sketch_dimension_error(),
        Some(DimensionInputError::NonPositive)
    );
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 0);
    assert_kernel_unchanged(&harness, snapshot, attempts);

    // Correcting the value recovers: the completed stroke commits itself.
    type_active_dimension(&mut harness, "Rectangle width", "4");
    press_key(&mut harness, egui::Key::Tab);
    type_active_dimension(&mut harness, "Rectangle height", "2");
    press_key(&mut harness, egui::Key::Enter);
    harness.run_steps(2);
    assert_eq!(harness.state().sketch_dimension_error(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn unfocused_dimension_editor_does_not_steal_global_confirm_or_cancel() {
    for key in [egui::Key::Enter, egui::Key::Escape] {
        let mut harness = harness([1280.0, 800.0]);
        enter_sketch(&mut harness, "XY Plane");
        choose_sketch_tool(&mut harness, "Two-point rectangle");
        let first = canvas_point(&harness, egui::vec2(-84.0, 56.0));
        let opposite = canvas_point(&harness, egui::vec2(84.0, -56.0));
        click_at(&mut harness, first);
        let _ = opposite;
        // Open the draft's width editor, then move focus elsewhere.
        press_key(&mut harness, egui::Key::Tab);
        assert!(
            harness
                .query_by_role_and_label(Role::TextInput, "Rectangle width")
                .is_some()
        );

        // Sketch mode has no side panel; the ribbon's Snap toggle is the
        // control outside the canvas that can hold focus.
        harness.get_by_role_and_label(Role::Button, "Snap").focus();
        harness.run();
        press_key(&mut harness, key);

        // A stray Enter or Escape from an unfocused control neither commits
        // the half-drawn draft nor invents an operation to confirm.
        assert!(!harness.state().operation_confirmation_pending());
        assert_eq!(harness.state().sketch_entity_count(), 0);
        assert_eq!(harness.state().sketch_revision(), 0);
    }
}

#[test]
fn tab_from_an_inspector_control_does_not_activate_a_draft_dimension() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Two-point rectangle");
    let first = canvas_point(&harness, egui::Vec2::ZERO);
    click_at(&mut harness, first);
    assert!(harness.state().sketch_creation_draft_active());
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle width")
            .is_none()
    );

    harness.get_by_role_and_label(Role::Button, "Snap").focus();
    harness.run();
    press_key(&mut harness, egui::Key::Tab);

    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle width")
            .is_none()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle height")
            .is_none()
    );
    assert!(harness.state().sketch_creation_draft_active());
    assert!(!harness.state().operation_confirmation_pending());
}

#[test]
fn read_only_selected_dimensions_do_not_block_the_next_creation_click() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Centre-point circle");
    let center = canvas_point(&harness, egui::Vec2::ZERO);
    let rim = canvas_point(&harness, egui::vec2(84.0, 0.0));
    click_at(&mut harness, center);
    click_at(&mut harness, rim);
    assert_eq!(harness.state().sketch_entity_count(), 1);

    choose_sketch_tool(&mut harness, "Two-point rectangle");
    let viewport = harness.get_by_label("Sketch viewport").rect();
    let read_only_dimension = harness
        .query_all_by_label("Circle diameter")
        .find(|node| viewport.contains(node.rect().center()))
        .expect("read-only circle dimension inside the sketch viewport")
        .rect()
        .center();
    click_at(&mut harness, read_only_dimension);

    assert!(harness.state().sketch_creation_draft_active());
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert!(!harness.state().operation_confirmation_pending());

    let opposite = canvas_point(&harness, egui::vec2(-84.0, -56.0));
    click_at(&mut harness, opposite);
    assert_eq!(harness.state().pending_operation_label(), None);
}

#[test]
fn active_closed_sketch_extrudes_directly_and_cancel_returns_to_sketch() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();
    commit_exact_xy_rectangle(&mut harness);

    assert!(!harness.state().sketch_finished());
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a confirmed closed profile should enable Extrude without a separate Finish step"
    );

    click_button(&mut harness, "Extrude");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    assert!(!harness.state().sketch_finished());
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    click_button(&mut harness, CANCEL_OPERATION);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(!harness.state().sketch_finished());
    assert!(!harness.state().operation_confirmation_pending());
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    click_button(&mut harness, "Model mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CANCEL_OPERATION);
    assert_eq!(
        harness.state().workbench_mode(),
        WorkbenchMode::Sketch,
        "cancelling an unfinished sketch extrusion must reopen its sketch"
    );
    assert!(!harness.state().sketch_finished());
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    harness
        .get_by_role_and_label(Role::Button, "Extrude")
        .focus();
    harness.run();
    press_key(&mut harness, egui::Key::Enter);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch"),
        "the Enter that activates Extrude must stage only, never also publish"
    );
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 1
    );
    assert_eq!(
        harness.state().feature_timeline_entries(),
        vec![
            "Origin".to_owned(),
            "Base body".to_owned(),
            "Sketch 1 · r1".to_owned(),
            "Extrude 1".to_owned(),
        ]
    );
}

#[test]
fn crossing_cells_require_selection_and_extrude_the_exact_selected_union() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");

    choose_sketch_tool(&mut harness, "Two-point rectangle");
    for (first, opposite) in [
        (SketchPoint::new(-4.0, -2.0), SketchPoint::new(1.0, 1.0)),
        (SketchPoint::new(-1.0, -1.0), SketchPoint::new(4.0, 2.0)),
    ] {
        let first = canvas_sketch_point(&harness, first);
        let opposite = canvas_sketch_point(&harness, opposite);
        click_at(&mut harness, first);
        click_at(&mut harness, opposite);
        assert_eq!(harness.state().pending_operation_label(), None);
    }

    choose_sketch_tool(&mut harness, "Select sketch geometry");
    let blank = canvas_sketch_point(&harness, SketchPoint::new(0.0, -4.0));
    click_at(&mut harness, blank);
    assert_eq!(harness.state().available_sketch_region_count(), 3);
    assert_eq!(harness.state().selected_sketch_region_count(), 0);
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::RegionSelectionRequired { available: 3 }
    );
    assert!(harness.state().sketch_planar_profile_payload().is_none());

    let left = canvas_sketch_point(&harness, SketchPoint::new(-3.0, 0.0));
    click_at(&mut harness, left);
    assert_eq!(harness.state().selected_sketch_region_count(), 1);
    assert_eq!(
        harness
            .state()
            .sketch_planar_profile_payload()
            .expect("selected left cell")
            .regions
            .len(),
        1
    );

    let overlap = canvas_sketch_point(&harness, SketchPoint::new(0.0, 0.0));
    click_at_with_modifiers(&mut harness, overlap, egui::Modifiers::SHIFT);
    assert_eq!(harness.state().selected_sketch_region_count(), 2);
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert_eq!(
        harness
            .state()
            .sketch_planar_profile_payload()
            .expect("selected cell union")
            .regions
            .len(),
        1
    );
    assert_eq!(harness.state().available_sketch_region_count(), 3);

    let attempts = harness.state().transaction_attempt_count();
    click_button(&mut harness, "Extrude");
    let command = harness
        .state()
        .pending_sketch_extrusion_command()
        .expect("selected union should stage exact extrusion");
    let artificer_protocol::KernelCommand::ExtrudePlanarProfile { profile, .. } = command else {
        panic!("expected an exact planar-profile extrusion, got {command:?}");
    };
    assert_eq!(profile.regions.len(), 1);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().last_error_code(), None);
}

#[test]
fn finished_four_by_two_xy_rectangle_extrudes_transactionally_to_exact_native_solid() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();
    finish_exact_xy_rectangle(&mut harness);
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a finished eligible sketch enables the persistent Extrude command"
    );
    click_button(&mut harness, "Collapse browser panel");
    assert!(!harness.state().shell_visibility().model_browser);
    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().workbench_mode(),
        WorkbenchMode::Model,
        "Extrude must enter the model preview workspace"
    );
    // Starting a command no longer reopens the tree. Properties moved to
    // their own dock, so there is nothing an operation needs from the left
    // panel, and a panel the user collapsed should stay collapsed.
    assert!(!harness.state().shell_visibility().model_browser);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch")
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Stage native extrusion")
            .is_none(),
        "the ribbon command is the sole extrusion staging action"
    );
    assert!(
        harness
            .query_by_label("Extrusion staged · confirm with Enter or the green tick")
            .is_some()
    );
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    set_extrusion_distance(&mut harness, "2");
    assert!((harness.state().extrusion_distance() - 2.0).abs() <= 1.0e-12);
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);
    assert_eq!(harness.state().extruded_sketch_revision(), None);

    click_button(&mut harness, CANCEL_OPERATION);
    assert!(!harness.state().operation_confirmation_pending());
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch")
    );
    set_extrusion_distance(&mut harness, "3");
    assert!((harness.state().extrusion_distance() - 3.0).abs() <= 1.0e-12);
    assert!(
        harness
            .query_by_label("Extrusion staged · confirm with Enter or the green tick")
            .is_some()
    );
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);
    assert_eq!(harness.state().extruded_sketch_revision(), None);

    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(!harness.state().operation_confirmation_pending());
    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 1
    );
    assert_eq!(
        harness.state().extruded_sketch_revision(),
        Some(harness.state().sketch_revision())
    );
    assert_eq!(harness.state().extruded_sketch_revision(), Some(1));
    assert_eq!(harness.state().last_error_code(), None);
    assert!(
        harness
            .query_by_label("Sketch extrusion committed")
            .is_some()
    );
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_label("Body 1 · native sketch extrusion")
            .is_some()
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "the applied sketch revision cannot be extruded twice"
    );

    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed extrusion measures");
    assert!((measures.volume - 24.0).abs() <= 1.0e-9);
    assert!((measures.surface_area - 52.0).abs() <= 1.0e-9);
    let centroid = measures.centroid.expect("solid centroid");
    assert!((centroid.x - 2.0).abs() <= 1.0e-9);
    assert!((centroid.y - 1.0).abs() <= 1.0e-9);
    assert!((centroid.z - 1.5).abs() <= 1.0e-9);

    click_button(&mut harness, "Extrusion top face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::ExtrusionTop)
    );
}

#[test]
fn concave_finished_loop_extrudes_as_an_exact_native_linear_profile() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Single line");
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();

    // A simple arrow-notch loop: closed and non-self-intersecting, but with
    // one turn opposite to the certified winding.
    let points = [
        SketchPoint::new(-2.0, -1.25),
        SketchPoint::new(2.0, -1.25),
        SketchPoint::new(0.5, 0.0),
        SketchPoint::new(2.0, 1.25),
        SketchPoint::new(-2.0, 1.25),
        SketchPoint::new(-2.0, -1.25),
    ];
    for edge in points.windows(2) {
        let start = canvas_sketch_point(&harness, edge[0]);
        let end = canvas_sketch_point(&harness, edge[1]);
        click_at(&mut harness, start);
        click_at(&mut harness, end);
        press_key(&mut harness, egui::Key::Enter);
    }
    let profile = harness.state().sketch_profile_status();
    assert!(
        matches!(profile, CertifiedProfileStatus::Closed { .. }),
        "expected a closed concave loop, got {profile:?}; entities {}, revision {}, pending {:?}",
        harness.state().sketch_entity_count(),
        harness.state().sketch_revision(),
        harness.state().pending_operation_label(),
    );

    choose_sketch_tool(&mut harness, "Select sketch geometry");
    click_button(&mut harness, "Finish sketch");
    press_key(&mut harness, egui::Key::Enter);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );

    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled()
    );
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);

    set_extrusion_distance(&mut harness, "2");
    click_button(&mut harness, "Extrude");
    assert!(harness.state().operation_confirmation_pending());
    assert_kernel_unchanged(&harness, original_snapshot, original_attempts);
    click_button(&mut harness, "Confirm operation");

    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 1
    );
    let counts = harness
        .state()
        .displayed_topology_counts()
        .expect("concave prism topology");
    assert_eq!((counts.vertices, counts.edges, counts.faces), (10, 15, 7));
    let measures = harness
        .state()
        .displayed_measures()
        .expect("concave prism measures");
    assert!((measures.volume - 16.25).abs() <= 1.0e-9);
}

#[test]
fn separately_drawn_reversed_square_is_visibly_certified_and_extrude_ready() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let a = SketchPoint::new(-2.0, -1.0);
    let b = SketchPoint::new(2.0, -1.0);
    let c = SketchPoint::new(2.0, 1.0);
    let d = SketchPoint::new(-2.0, 1.0);

    // Deliberately neither entity order nor entity direction follows the
    // perimeter. Each segment is a separate Line-tool gesture.
    for (start, end) in [(c, b), (a, d), (c, d), (a, b)] {
        choose_sketch_tool(&mut harness, "Single line");
        let start = canvas_sketch_point(&harness, start);
        let end = canvas_sketch_point(&harness, end);
        click_at(&mut harness, start);
        click_at(&mut harness, end);
        press_key(&mut harness, egui::Key::Enter);
        choose_sketch_tool(&mut harness, "Select sketch geometry");
    }

    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed { .. }
    ));
    let payload = harness
        .state()
        .sketch_planar_profile_payload()
        .expect("a certified closed square extracts one region");
    assert_eq!(payload.regions.len(), 1);
    assert!(payload.regions[0].holes.is_empty());
    click_button(&mut harness, "Finish sketch");
    press_key(&mut harness, egui::Key::Enter);
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a loop should not depend on its entity insertion order"
    );
}

#[test]
fn every_sketch_entity_kind_has_an_activatable_semantic_selection_target() {
    struct EntityGesture {
        tool: &'static str,
        semantic_kind: &'static str,
        offsets: Vec<egui::Vec2>,
    }

    let gestures = [
        EntityGesture {
            tool: "Sketch point",
            semantic_kind: "point",
            offsets: vec![egui::vec2(0.0, 0.0)],
        },
        EntityGesture {
            tool: "Single line",
            semantic_kind: "segment",
            offsets: vec![egui::vec2(-84.0, 0.0), egui::vec2(84.0, 0.0)],
        },
        EntityGesture {
            tool: "Two-point rectangle",
            semantic_kind: "rectangle",
            offsets: vec![egui::vec2(-84.0, 56.0), egui::vec2(84.0, -56.0)],
        },
        EntityGesture {
            tool: "Centre-point circle",
            semantic_kind: "circle",
            offsets: vec![egui::vec2(-28.0, 0.0), egui::vec2(84.0, 0.0)],
        },
        EntityGesture {
            tool: "Centre-start-end arc",
            semantic_kind: "arc",
            offsets: vec![
                egui::vec2(0.0, 0.0),
                egui::vec2(84.0, 0.0),
                egui::vec2(0.0, -56.0),
            ],
        },
    ];

    for gesture in gestures {
        let mut harness = harness([1280.0, 800.0]);
        enter_sketch(&mut harness, "XY Plane");
        choose_sketch_tool(&mut harness, gesture.tool);
        for offset in gesture.offsets {
            let point = canvas_point(&harness, offset);
            click_at(&mut harness, point);
        }
        choose_sketch_tool(&mut harness, "Select sketch geometry");

        let semantic_label = format!("Sketch {} 1", gesture.semantic_kind);
        assert!(
            harness
                .query_by_role_and_label(Role::Button, &semantic_label)
                .is_some(),
            "missing semantic selection target {semantic_label}"
        );
        click_button(&mut harness, &semantic_label);
        assert!(
            harness.query_by_label("Sketch entity #1").is_some(),
            "activating {semantic_label} did not select the entity"
        );
    }
}

#[test]
fn rectangle_is_certified_counter_clockwise_and_finish_is_itself_confirmed() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XZ Plane");
    choose_sketch_tool(&mut harness, "Two-point rectangle");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    let first = canvas_point(&harness, egui::vec2(-84.0, 56.0));
    let opposite = canvas_point(&harness, egui::vec2(84.0, -56.0));
    click_at(&mut harness, first);
    click_at(&mut harness, opposite);

    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed {
            winding: ProfileWinding::CounterClockwise,
        }
    );

    click_button(&mut harness, "Finish sketch");
    // Finishing is one action: the sketch saves and the mode returns.
    assert!(harness.state().sketch_finished());
    assert_kernel_unchanged(&harness, snapshot, attempts);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert!(!harness.state().operation_confirmation_pending());
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn connected_line_loop_is_certified_and_finishes_through_the_global_gate() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Single line");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    let offsets = [
        egui::vec2(-84.0, 56.0),
        egui::vec2(84.0, 56.0),
        egui::vec2(84.0, -56.0),
        egui::vec2(-84.0, -56.0),
        egui::vec2(-84.0, 56.0),
    ];
    for edge in offsets.windows(2) {
        let start = canvas_point(&harness, edge[0]);
        let end = canvas_point(&harness, edge[1]);
        click_at(&mut harness, start);
        click_at(&mut harness, end);
        assert_eq!(harness.state().pending_operation_label(), None);
    }

    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 4);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed {
            winding: ProfileWinding::CounterClockwise,
        }
    );
    assert_kernel_unchanged(&harness, snapshot, attempts);

    choose_sketch_tool(&mut harness, "Select sketch geometry");
    click_button(&mut harness, "Finish sketch");

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert!(!harness.state().operation_confirmation_pending());
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn connected_line_loop_extrudes_without_leaving_the_line_tool() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Single line");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();
    let points = [
        SketchPoint::new(-2.0, -1.0),
        SketchPoint::new(2.0, -1.0),
        SketchPoint::new(2.0, 1.0),
        SketchPoint::new(-2.0, 1.0),
        SketchPoint::new(-2.0, -1.0),
    ];
    for edge in points.windows(2) {
        let start = canvas_sketch_point(&harness, edge[0]);
        let end = canvas_sketch_point(&harness, edge[1]);
        click_at(&mut harness, start);
        click_at(&mut harness, end);
        press_key(&mut harness, egui::Key::Enter);
    }

    assert_eq!(harness.state().sketch_tool_label(), "Line");
    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed { .. }
    ));
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "the automatic next-line anchor must not masquerade as unfinished geometry"
    );

    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    assert_kernel_unchanged(&harness, snapshot, attempts);
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed multiline extrusion");
    assert!((measures.volume - 32.0).abs() <= 1.0e-9);
}

#[test]
fn circles_and_nested_profile_holes_extrude_from_the_visible_ui() {
    for (radii, expected_volume) in [
        (vec![2.0], 16.0 * std::f64::consts::PI),
        (vec![4.0, 2.0], 48.0 * std::f64::consts::PI),
    ] {
        let mut harness = harness([1280.0, 800.0]);
        enter_sketch(&mut harness, "XY Plane");
        choose_sketch_tool(&mut harness, "Centre-point circle");
        let snapshot = harness.state().displayed_snapshot_id();
        let attempts = harness.state().transaction_attempt_count();

        for radius in radii {
            let center = canvas_sketch_point(&harness, SketchPoint::new(0.0, 0.0));
            let rim = canvas_sketch_point(&harness, SketchPoint::new(radius, 0.0));
            click_at(&mut harness, center);
            click_at(&mut harness, rim);
            assert_eq!(harness.state().pending_operation_label(), None);
        }

        assert!(matches!(
            harness.state().sketch_profile_status(),
            CertifiedProfileStatus::ClosedAnalyticCircle
                | CertifiedProfileStatus::ClosedRegions { analytic: true, .. }
        ));
        assert!(
            !harness
                .get_by_role_and_label(Role::Button, "Extrude")
                .accesskit_node()
                .is_disabled(),
            "an exact circular profile must expose the same Extrude action as a polygon"
        );

        click_button(&mut harness, "Extrude");
        assert_kernel_unchanged(&harness, snapshot, attempts);
        click_button(&mut harness, CONFIRM_OPERATION);

        assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
        assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
        let measures = harness
            .state()
            .displayed_measures()
            .expect("committed analytic extrusion");
        assert!((measures.volume - expected_volume).abs() <= 1.0e-8);
    }
}

#[test]
fn a_visible_circle_on_a_face_commits_as_an_exact_boss() {
    let mut harness = harness([1280.0, 800.0]);
    enter_positive_z_face_sketch(&mut harness);
    choose_sketch_tool(&mut harness, "Centre-point circle");
    let center = canvas_sketch_point(&harness, SketchPoint::new(0.0, 0.0));
    let rim = canvas_sketch_point(&harness, SketchPoint::new(0.5, 0.0));
    click_at(&mut harness, center);
    click_at(&mut harness, rim);
    press_key(&mut harness, egui::Key::Enter);

    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled()
    );
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().last_error_code(), None);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed circular boss");
    assert!((measures.volume - (24.0 + 0.25 * std::f64::consts::PI)).abs() <= 1.0e-8);
    assert!(
        harness
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == "Add 1")
    );
}

#[test]
fn a_visible_line_and_semicircular_arc_extrude_as_one_exact_profile() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let left = SketchPoint::new(-2.0, 0.0);
    let center = SketchPoint::new(0.0, 0.0);
    let right = SketchPoint::new(2.0, 0.0);

    choose_sketch_tool(&mut harness, "Single line");
    let left_canvas = canvas_sketch_point(&harness, left);
    let right_canvas = canvas_sketch_point(&harness, right);
    click_at(&mut harness, left_canvas);
    click_at(&mut harness, right_canvas);
    press_key(&mut harness, egui::Key::Enter);

    choose_sketch_tool(&mut harness, "Centre-start-end arc");
    let center_canvas = canvas_sketch_point(&harness, center);
    let right_canvas = canvas_sketch_point(&harness, right);
    let left_canvas = canvas_sketch_point(&harness, left);
    click_at(&mut harness, center_canvas);
    click_at(&mut harness, right_canvas);
    click_at(&mut harness, left_canvas);
    // The completed arc commits itself with exact geometry.
    assert_eq!(harness.state().pending_operation_label(), None);
    let _ = (center, right, left);

    let status = harness.state().sketch_profile_status();
    assert!(
        matches!(
            status,
            CertifiedProfileStatus::ClosedAnalyticCurves
                | CertifiedProfileStatus::ClosedRegions { analytic: true, .. }
        ),
        "unexpected line/arc profile status: {status:?}"
    );
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().last_error_code(), None);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed line-and-arc extrusion");
    assert!((measures.volume - 8.0 * std::f64::consts::PI).abs() <= 1.0e-8);
}

#[test]
fn finish_saves_an_open_line_but_keeps_extrude_unavailable() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Single line");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    let start = canvas_point(&harness, egui::vec2(-84.0, 0.0));
    let end = canvas_point(&harness, egui::vec2(84.0, 0.0));
    click_at(&mut harness, start);
    click_at(&mut harness, end);
    press_key(&mut harness, egui::Key::Enter);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Open
    );

    // Open authoring geometry is a valid saved sketch even though it cannot
    // define material until later edits close it.
    choose_sketch_tool(&mut harness, "Select sketch geometry");
    click_button(&mut harness, "Finish sketch");
    press_key(&mut harness, egui::Key::Enter);

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert!(!harness.state().operation_confirmation_pending());
    assert_ne!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled()
    );
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn finish_saves_self_intersecting_authoring_and_exposes_the_selected_bounded_cell() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    choose_sketch_tool(&mut harness, "Single line");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();

    // A -> B -> C -> D -> A is a closed bow tie. Each exact single-line
    // command stages one connected segment behind the same Enter gate.
    let offsets = [
        egui::vec2(-84.0, -56.0),
        egui::vec2(84.0, 56.0),
        egui::vec2(-84.0, 56.0),
        egui::vec2(84.0, -56.0),
        egui::vec2(-84.0, -56.0),
    ];
    for edge in offsets.windows(2) {
        let start = canvas_point(&harness, edge[0]);
        let end = canvas_point(&harness, edge[1]);
        click_at(&mut harness, start);
        click_at(&mut harness, end);
        assert_eq!(harness.state().pending_operation_label(), None);
    }

    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 4);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::SelfIntersecting
    );

    choose_sketch_tool(&mut harness, "Select sketch geometry");
    click_button(&mut harness, "Finish sketch");
    press_key(&mut harness, egui::Key::Enter);

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(harness.state().sketch_finished());
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().available_sketch_region_count(), 2);
    assert_eq!(
        harness.state().selected_sketch_region_count(),
        1,
        "the first certified lobe remains explicitly selected when the closing edge creates the second lobe"
    );
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled()
    );
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn sketch_pan_and_zoom_are_immediate_presentation_changes_only() {
    let mut harness = harness([1280.0, 800.0]);
    enter_sketch(&mut harness, "XY Plane");
    let snapshot = harness.state().displayed_snapshot_id();
    let attempts = harness.state().transaction_attempt_count();
    let before = harness.state().sketch_view_parameters();
    let start = canvas_point(&harness, egui::vec2(0.0, 0.0));
    let end = start + egui::vec2(56.0, -28.0);

    harness.hover_at(start);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Middle,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Middle,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();

    let after_pan = harness.state().sketch_view_parameters();
    assert_ne!((after_pan.0, after_pan.1), (before.0, before.1));
    assert_eq!(after_pan.2, before.2);

    harness.hover_at(end);
    harness.step();
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 80.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    let after_zoom = harness.state().sketch_view_parameters();
    assert_ne!(after_zoom.2, after_pan.2);

    assert_eq!(harness.state().sketch_revision(), 0);
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert!(!harness.state().operation_confirmation_pending());
    assert_kernel_unchanged(&harness, snapshot, attempts);
}

#[test]
fn minimum_window_keeps_critical_sketch_controls_visible_and_canvas_fixed() {
    let mut harness = harness([1040.0, 700.0]);
    enter_sketch(&mut harness, "XY Plane");
    let ribbon_bottom = harness.get_by_label("Sketch viewport").rect().top();

    for label in [
        "Sketch point",
        "Single line",
        "Two-point rectangle",
        "Centre-point circle",
        "Centre-start-end arc",
        "Extrude",
        "Frame sketch",
    ] {
        let node = harness.get_by_role_and_label(Role::Button, label);
        let rect = node.rect();
        assert!(rect.is_positive(), "{label} must have a visible hit target");
        assert!(
            rect.height() >= 24.0,
            "{label} is vertically clipped: {rect:?}"
        );
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0 && rect.min.y >= 0.0 && rect.max.y <= 700.0,
            "{label} escaped the supported window: {rect:?}"
        );
        assert!(
            rect.max.y <= ribbon_bottom,
            "{label} overlaps the sketch canvas below the command ribbon: {rect:?}"
        );
    }
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "persistent Extrude stays visible but unavailable until a closed profile is confirmed"
    );

    let clean_canvas = harness.get_by_label("Sketch viewport").rect();
    choose_sketch_tool(&mut harness, "Sketch point");
    let point = canvas_point(&harness, egui::vec2(0.0, 0.0));
    click_at(&mut harness, point);
    // The point committed itself; the idle rail keeps the sketch actions
    // reachable at the minimum window.
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.get_by_label("Sketch viewport").rect(), clean_canvas);

    for label in ["Finish sketch", "Exit sketch"] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a visible hit target");
        assert_eq!(rect.width(), rect.height(), "{label} must be square");
        assert!(rect.width() <= 30.0, "{label} is too bulky: {rect:?}");
        assert!(rect.width() >= 24.0, "{label} is too small: {rect:?}");
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0 && rect.min.y >= 0.0 && rect.max.y <= 700.0,
            "{label} escaped the supported window: {rect:?}"
        );
    }

    // Exiting the sketch from the rail keeps the canvas geometry intact.
    click_button(&mut harness, "Exit sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().sketch_entity_count(), 1);
}

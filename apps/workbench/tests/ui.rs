use artificer_protocol::KernelErrorCode;
use artificer_workbench::{KernelLabApp, WorkbenchMode};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const EPSILON: f64 = 1.0e-10;
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

fn minimum_window_harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn open_collapsible(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness
        .get_by_role_and_label(Role::Button, label)
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    harness.run();
}

fn diagnostic_harness() -> Harness<'static, KernelLabApp> {
    let mut harness = harness();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Properties")
        .click_accesskit();
    harness.run();
    open_collapsible(&mut harness, "LAB / DIAGNOSTICS");
    harness
}

/// The settings dialog is anchored over the centre of the window, so the model
/// beneath it cannot be picked or dragged while it is up. Tests that do both
/// put it away for the part that touches the model, exactly as a user would.
fn with_model_reachable(
    harness: &mut Harness<'static, KernelLabApp>,
    body: impl FnOnce(&mut Harness<'static, KernelLabApp>),
) {
    harness.state_mut().close_document_properties();
    harness.run();
    body(harness);
    harness.state_mut().open_document_properties();
    harness.run();
}

fn minimum_diagnostic_harness() -> Harness<'static, KernelLabApp> {
    let mut harness = minimum_window_harness();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Properties")
        .click_accesskit();
    harness.run();
    open_collapsible(&mut harness, "LAB / DIAGNOSTICS");
    harness
}

/// The ribbon is tabbed, so a test that wants a View-tab command has to open
/// that tab first — the same trip the user makes.
fn open_ribbon_tab(harness: &mut Harness<'static, KernelLabApp>, tab: &str) {
    harness
        .get_by_role_and_label(Role::Button, tab)
        .click_accesskit();
    harness.run();
}

fn click_tool(harness: &mut Harness<'static, KernelLabApp>, shortcut: &str, label: &str) {
    harness
        .get_by_role_and_label(Role::Button, &format!("{shortcut}  {label}"))
        .click_accesskit();
    harness.run();
    assert_eq!(harness.state().active_tool_label(), label);
}

/// A grab point inside the model viewport that is not its dead centre.
///
/// Centred dialogs land on the middle of the screen, so a drag that starts
/// there is grabbing whatever floats above the model rather than the model.
/// Quarter-width in is still comfortably over the body and owned by nothing.
fn viewport_grab_point(harness: &Harness<'static, KernelLabApp>) -> egui::Pos2 {
    let viewport = harness.get_by_label("Model viewport").rect();
    egui::pos2(
        viewport.left() + viewport.width() * 0.25,
        viewport.center().y,
    )
}

fn drag_viewport(harness: &mut Harness<'static, KernelLabApp>, delta: egui::Vec2) {
    let start = viewport_grab_point(harness);
    let end = start + delta;
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
}

fn secondary_drag_viewport(harness: &mut Harness<'static, KernelLabApp>, delta: egui::Vec2) {
    let start = viewport_grab_point(harness);
    let end = start + delta;
    harness.hover_at(start);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.event(egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
}

fn click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run();
}

fn confirm_with_tick(harness: &mut Harness<'static, KernelLabApp>) {
    harness
        .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
        .click_accesskit();
    harness.run();
}

fn cancel_with_red_button(harness: &mut Harness<'static, KernelLabApp>) {
    harness
        .get_by_role_and_label(Role::Button, CANCEL_OPERATION)
        .click_accesskit();
    harness.run();
}

fn assert_compact_confirmation_controls(harness: &Harness<'static, KernelLabApp>) {
    for label in [CONFIRM_OPERATION, CANCEL_OPERATION] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a visible hit target");
        assert!(
            (rect.width() - rect.height()).abs() <= f32::EPSILON,
            "{label} must be square, got {rect:?}"
        );
        assert!(
            rect.width() <= 30.0 + f32::EPSILON,
            "{label} must remain compact, got {rect:?}"
        );
        assert!(
            rect.width() >= 24.0,
            "{label} must retain an accessible hit target, got {rect:?}"
        );
    }
}

#[test]
fn native_constructor_starts_at_the_authored_static_pose() {
    let harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new(creation_context));

    assert!(!harness.state().animation_playing());
    assert!(harness.state().animation_phase().abs() <= EPSILON);
}

#[test]
fn face_sketch_stops_motion_and_focuses_the_authored_face_pose() {
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));
    harness.state_mut().set_face_camera_animation(true);
    harness.state_mut().set_animation_playing(false);
    harness
        .state_mut()
        .set_animation_phase(std::f64::consts::FRAC_PI_2);
    harness.run();

    let model_viewport = harness.get_by_label("Model viewport").rect();
    harness
        .get_by_role_and_label(Role::Button, "Positive X face")
        .click_accesskit();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Sketch on selected face")
        .click_accesskit();
    harness.step();

    assert!(harness.state().face_camera_transition_active());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    let mut transition_frames = 0;
    while harness.state().face_camera_transition_active() {
        harness.step();
        transition_frames += 1;
        assert!(
            transition_frames < 30,
            "the 340 ms camera move did not settle"
        );
    }
    assert!(
        (20..=23).contains(&transition_frames),
        "the 340 ms camera move used {transition_frames} 60 Hz frames"
    );
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert_eq!(
        harness.state().sketch_screen_axis_labels()[1],
        "Z",
        "the face's world-Z sketch axis should remain upright on screen"
    );
    let sketch_viewport = harness.get_by_label("Sketch viewport").rect();
    let cube_face = harness
        .get_by_role_and_label(Role::Button, "View cube right")
        .rect();
    assert!(
        sketch_viewport.contains_rect(cube_face),
        "the face cube must be an overlay inside the sketch viewport: {cube_face:?}"
    );

    // The turntable phase is presentation-only. Entering a face sketch stops
    // it and focuses the bootstrap cuboid's authored +X-face center.
    let (focus, fit_radius) = harness.state().view_frame();
    assert!((focus.x - 2.0).abs() <= EPSILON, "focus x was {}", focus.x);
    assert!((focus.y - 1.5).abs() <= EPSILON, "focus y was {}", focus.y);
    assert!((focus.z - 2.0).abs() <= EPSILON, "focus z was {}", focus.z);
    assert!(harness.state().animation_phase().abs() <= EPSILON);

    // The last 3D frame and first 2D sketch frame use the same orthographic
    // scale. This guards the mode-boundary zoom snap independently of pixels or
    // face color changes between the two workspaces.
    let (_, _, model_zoom) = harness.state().view_parameters();
    let (_, _, sketch_points_per_unit) = harness.state().sketch_view_parameters();
    let model_points_per_unit =
        f64::from(model_viewport.width().min(model_viewport.height())) * 0.34 * model_zoom
            / fit_radius;
    assert!(
        (model_points_per_unit - sketch_points_per_unit).abs() <= EPSILON,
        "last model scale {model_points_per_unit} differed from first sketch scale {sketch_points_per_unit}"
    );
}

#[test]
fn canonical_case_exposes_native_result() {
    let mut harness = harness();
    harness.run();

    assert!(harness.state().displayed_snapshot_id().is_some());
    assert_eq!(harness.state().last_error_code(), None);
    assert!(harness.query_by_label("NATIVE RUST ONLY").is_some());
    assert!(harness.query_by_label("6 faces").is_some());
    assert!(harness.query_by_label("Solid · valid").is_some());
    assert!(harness.query_by_label("Model viewport").is_some());
    assert!(harness.query_by_label("60 FPS GOAL").is_some());
    assert!(harness.query_by_label("ANIMATION STOPPED").is_some());
    assert_eq!(harness.state().reported_fps(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .is_none()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CANCEL_OPERATION)
            .is_none()
    );
}

#[test]
fn case_is_staged_without_kernel_work_and_tick_rejection_stays_pending() {
    let mut harness = diagnostic_harness();
    let committed = harness
        .state()
        .displayed_snapshot_id()
        .expect("startup case should commit a cuboid");
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();

    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().last_error_code(), None);
    assert_eq!(harness.state().displayed_snapshot_id(), Some(committed));
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CANCEL_OPERATION)
            .is_some()
    );
    assert_compact_confirmation_controls(&harness);

    confirm_with_tick(&mut harness);

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::InvalidInput)
    );
    assert_eq!(harness.state().displayed_snapshot_id(), Some(committed));
    assert!(
        harness
            .query_all_by_label("Rejected · invalid_input")
            .next()
            .is_some()
    );
    assert!(
        harness
            .query_all_by_label("Last valid snapshot retained")
            .next()
            .is_some()
    );
    assert!(harness.query_by_label("6 faces").is_some());
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .is_some()
    );

    cancel_with_red_button(&mut harness);
    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().displayed_snapshot_id(), Some(committed));
}

#[test]
fn bare_enter_confirms_a_staged_case_and_rejection_keeps_it_pending() {
    let mut harness = diagnostic_harness();
    let committed = harness
        .state()
        .displayed_snapshot_id()
        .expect("startup case should commit a cuboid");
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, "Stale snapshot")
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Stale snapshot")
        .click_accesskit();
    harness.run();

    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Stale snapshot")
    );

    harness.key_press(egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::StaleSnapshot)
    );
    assert_eq!(harness.state().displayed_snapshot_id(), Some(committed));
    assert!(
        harness
            .query_all_by_label("Rejected · stale_snapshot")
            .next()
            .is_some()
    );
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Stale snapshot")
    );
}

#[test]
fn focused_case_button_needs_one_enter_to_stage_and_another_to_execute() {
    let mut harness = diagnostic_harness();
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .focus();
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
    assert_eq!(harness.state().last_error_code(), None);

    harness.key_press(egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::InvalidInput)
    );
    assert!(harness.state().operation_confirmation_pending());
}

#[test]
fn focused_tick_dispatches_a_rejected_operation_exactly_once() {
    let mut harness = diagnostic_harness();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
        .focus();
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::InvalidInput)
    );
    assert!(harness.state().operation_confirmation_pending());
}

#[test]
fn bare_enter_confirms_even_when_the_cancel_button_has_focus() {
    let mut harness = diagnostic_harness();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, CANCEL_OPERATION)
        .focus();
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::InvalidInput)
    );
    assert!(harness.state().operation_confirmation_pending());
}

#[test]
fn space_activates_the_focused_icon_only_confirmation_button() {
    let mut harness = diagnostic_harness();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();
    let attempts = harness.state().transaction_attempt_count();
    let animation_playing = harness.state().animation_playing();

    harness
        .get_by_role_and_label(Role::Button, CANCEL_OPERATION)
        .focus();
    harness.run();
    harness.key_press(egui::Key::Space);
    harness.step();

    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().animation_playing(), animation_playing);

    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
        .focus();
    harness.run();
    harness.key_press(egui::Key::Space);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert!(harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().animation_playing(), animation_playing);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::InvalidInput)
    );
}

#[test]
fn modified_enter_cannot_bypass_the_bare_enter_contract() {
    let mut harness = diagnostic_harness();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
        .focus();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().last_error_code(), None);
    assert!(harness.state().operation_confirmation_pending());
}

#[test]
fn successful_case_confirmation_clears_the_pending_operation() {
    let mut harness = diagnostic_harness();
    with_model_reachable(&mut harness, |harness| {
        click_tool(harness, "M", "Move");
        drag_viewport(harness, egui::vec2(24.0, -10.0));
        confirm_with_tick(harness);
    });
    assert_eq!(
        harness.state().feature_timeline_entries(),
        vec![
            "Origin".to_owned(),
            "Base body".to_owned(),
            "Transform 1".to_owned(),
        ]
    );
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, "Valid 2 × 3 × 4")
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Valid 2 × 3 × 4")
        .click_accesskit();
    harness.run();
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert!(harness.state().operation_confirmation_pending());

    confirm_with_tick(&mut harness);

    assert_eq!(harness.state().transaction_attempt_count(), attempts + 1);
    assert_eq!(harness.state().last_error_code(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(
        harness.state().feature_timeline_entries(),
        vec!["Origin".to_owned(), "Base body".to_owned()],
        "a diagnostic body replacement resets the presentation-owned history atomically"
    );
    assert!(
        harness
            .query_all_by_label("Cuboid committed")
            .next()
            .is_some()
    );
}

#[test]
fn escape_cancels_a_staged_case_without_calling_the_kernel() {
    let mut harness = diagnostic_harness();
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();

    harness
        .get_by_role_and_label(Role::Button, "Non-finite depth")
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Non-finite depth")
        .click_accesskit();
    harness.run();
    assert!(harness.state().operation_confirmation_pending());

    harness.key_press(egui::Key::Escape);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().last_error_code(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .is_none()
    );
}

#[test]
fn enter_without_a_pending_operation_is_a_complete_noop() {
    let mut harness = harness();
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let transform = harness.state().displayed_transform();
    let selection = harness.state().selected_face();
    let view = harness.state().view_parameters();
    let frame = harness.state().view_frame();
    let attempts = harness.state().transaction_attempt_count();

    harness.key_press(egui::Key::Enter);
    harness.step();

    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().displayed_transform(), transform);
    assert_eq!(harness.state().selected_face(), selection);
    assert_eq!(harness.state().view_parameters(), view);
    assert_eq!(harness.state().view_frame(), frame);
    assert!(!harness.state().operation_confirmation_pending());
}

#[test]
fn edge_overlay_toggles() {
    let mut harness = diagnostic_harness();
    open_collapsible(&mut harness, "DISPLAY");
    assert!(harness.state().edge_overlay_enabled());

    harness
        .get_by_role_and_label(Role::CheckBox, "Source edge overlay")
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::CheckBox, "Source edge overlay")
        .click_accesskit();
    harness.run();

    assert!(!harness.state().edge_overlay_enabled());
}

#[test]
fn source_face_click_selects_kernel_entity() {
    let mut harness = harness();
    assert_eq!(harness.state().selected_face(), None);

    harness
        .get_by_role_and_label(Role::Button, "Positive Z face")
        .click_accesskit();
    harness.run();

    let selected = harness
        .state()
        .selected_face()
        .expect("face click should select a source entity");
    assert_eq!(selected.kind, artificer_protocol::EntityKind::Face);
    assert_eq!(
        harness.state().selected_face_role(),
        Some(artificer_kernel::FaceRole::PositiveZ)
    );
}

#[test]
fn background_click_does_not_select_a_face() {
    let mut harness = harness();
    let viewport = harness.get_by_label("Model viewport").rect();
    click_at(&mut harness, viewport.left_top() + egui::vec2(45.0, 75.0));
    assert_eq!(harness.state().selected_face(), None);
}

#[test]
fn toolbar_exposes_each_interaction_mode() {
    let mut harness = harness();

    for (shortcut, label) in [
        ("O", "Orbit"),
        ("M", "Move"),
        ("R", "Rotate"),
        ("S", "Scale"),
        ("I", "Measure"),
        ("V", "Select"),
    ] {
        click_tool(&mut harness, shortcut, label);
    }

    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Positive Z face")
            .is_some(),
        "source-face hit regions should return in Select mode"
    );
}

#[test]
fn confirmation_slot_preserves_viewport_geometry_at_the_supported_minimum_window() {
    let mut harness = minimum_diagnostic_harness();
    harness.run();
    let ribbon_bottom = harness.get_by_label("Model viewport").rect().top();

    // Every tab, not just the one that opens first: a ribbon that fits at the
    // minimum window on one tab and overflows on another is still broken.
    for (tab, label) in [
        (None, "Create sketch"),
        (None, "Extrude"),
        (None, "Revolve"),
        (None, "Hole"),
        (None, "Fillet"),
        (None, "Combine"),
        (None, "V  Select"),
        (None, "I  Measure"),
        (None, "O  Orbit"),
        (None, "M  Move"),
        (None, "R  Rotate"),
        (None, "S  Scale"),
        (Some("View ribbon tab"), "Frame"),
        (Some("View ribbon tab"), "Home"),
        (Some("View ribbon tab"), "Edges"),
        (Some("View ribbon tab"), "Shaded"),
        (Some("View ribbon tab"), "Play motion"),
        (Some("View ribbon tab"), "Show browser panel"),
    ] {
        if let Some(tab) = tab {
            open_ribbon_tab(&mut harness, tab);
        }
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a visible hit region");
        assert!(
            rect.height() >= 24.0,
            "{label} is vertically clipped: {rect:?}"
        );
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0,
            "{label}: {rect:?}"
        );
        assert!(
            rect.min.y >= 0.0 && rect.max.y <= 700.0,
            "{label}: {rect:?}"
        );
        assert!(
            rect.max.y <= ribbon_bottom,
            "{label} overlaps the canvas below the command ribbon: {rect:?}"
        );
    }
    // Leave the ribbon where the rest of this test expects to find it.
    open_ribbon_tab(&mut harness, "Model mode");

    let clean_viewport = harness.get_by_label("Model viewport").rect();
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .is_none()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CANCEL_OPERATION)
            .is_none()
    );

    with_model_reachable(&mut harness, |harness| {
        click_tool(harness, "M", "Move");
        drag_viewport(harness, egui::vec2(45.0, -18.0));
    });

    assert!(harness.state().operation_confirmation_pending());
    assert_compact_confirmation_controls(&harness);
    let pending_viewport = harness.get_by_label("Model viewport").rect();
    assert_eq!(
        pending_viewport, clean_viewport,
        "the fixed confirmation slot must not move or resize the viewport"
    );
    for label in [CONFIRM_OPERATION, CANCEL_OPERATION] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a visible hit region");
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0,
            "{label}: {rect:?}"
        );
        assert!(
            rect.min.y >= 0.0 && rect.max.y <= 700.0,
            "{label}: {rect:?}"
        );
    }

    cancel_with_red_button(&mut harness);
    assert_eq!(
        harness.get_by_label("Model viewport").rect(),
        clean_viewport,
        "cancelling must preserve the fixed viewport geometry"
    );

    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();
    confirm_with_tick(&mut harness);
    assert_eq!(
        harness.state().last_error_code(),
        Some(KernelErrorCode::InvalidInput)
    );
    assert_eq!(
        harness.get_by_label("Model viewport").rect(),
        clean_viewport,
        "a rejected operation must keep the confirmation slot and viewport fixed"
    );
}

#[test]
fn viewport_gestures_change_only_presentation_state() {
    let mut harness = harness();
    let committed_snapshot = harness.state().displayed_snapshot_id();
    let committed_digest = harness.state().displayed_semantic_digest();

    click_tool(&mut harness, "M", "Move");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Positive Z face")
            .is_none(),
        "face hit regions must not steal a transform drag"
    );
    drag_viewport(&mut harness, egui::vec2(90.0, -45.0));
    let (translation, rotation, scale) = harness.state().displayed_transform();
    assert!(translation[0] > EPSILON);
    assert!(translation[2] > EPSILON);
    assert_eq!(rotation, [0.0; 3]);
    assert!((scale - 1.0).abs() <= EPSILON);

    click_tool(&mut harness, "R", "Rotate");
    drag_viewport(&mut harness, egui::vec2(70.0, 35.0));
    let (_, rotation, _) = harness.state().displayed_transform();
    assert!(rotation[0].abs() > EPSILON);
    assert!(rotation[2].abs() > EPSILON);

    click_tool(&mut harness, "S", "Scale");
    drag_viewport(&mut harness, egui::vec2(0.0, -70.0));
    let (_, _, scale) = harness.state().displayed_transform();
    assert!(scale > 1.0);

    let view_before = harness.state().view_parameters();
    click_tool(&mut harness, "O", "Orbit");
    drag_viewport(&mut harness, egui::vec2(55.0, -30.0));
    let view_after = harness.state().view_parameters();
    assert!((view_after.0 - view_before.0).abs() > EPSILON);
    assert!((view_after.1 - view_before.1).abs() > EPSILON);

    assert_eq!(harness.state().displayed_snapshot_id(), committed_snapshot);
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        committed_digest
    );
    assert!(harness.state().transform_preview_pending());
    assert_eq!(harness.state().transform_preview_base(), committed_snapshot);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Transform whole body/group")
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .is_some()
    );
}

#[test]
fn tick_confirms_preview_through_kernel_and_preserves_view_motion_and_selection() {
    let mut harness = harness();
    harness
        .get_by_role_and_label(Role::Button, "Positive Z face")
        .click_accesskit();
    harness.run();
    let selected_before = harness.state().selected_face().unwrap();
    let snapshot_before = harness.state().displayed_snapshot_id().unwrap();
    let digest_before = harness.state().displayed_semantic_digest().unwrap();
    let view_before = harness.state().view_parameters();
    let frame_before = harness.state().view_frame();
    harness.state_mut().set_animation_phase(0.52);

    click_tool(&mut harness, "M", "Move");
    drag_viewport(&mut harness, egui::vec2(72.0, -31.0));
    assert!(harness.state().transform_preview_pending());
    assert_eq!(
        harness.state().displayed_snapshot_id(),
        Some(snapshot_before)
    );
    assert_eq!(
        harness.state().displayed_semantic_digest(),
        Some(digest_before)
    );

    confirm_with_tick(&mut harness);

    let snapshot_after = harness.state().displayed_snapshot_id().unwrap();
    assert_ne!(snapshot_after, snapshot_before);
    assert_ne!(
        harness.state().displayed_semantic_digest(),
        Some(digest_before)
    );
    assert!(!harness.state().transform_preview_pending());
    assert_eq!(
        harness.state().displayed_transform(),
        ([0.0; 3], [0.0; 3], 1.0)
    );
    assert_eq!(harness.state().view_parameters(), view_before);
    assert_eq!(harness.state().view_frame(), frame_before);
    assert!((harness.state().animation_phase() - 0.52).abs() <= EPSILON);
    let selected_after = harness.state().selected_face().unwrap();
    assert_eq!(selected_after.kind, selected_before.kind);
    assert_eq!(selected_after.entity, selected_before.entity);
    assert_eq!(selected_after.snapshot, snapshot_after);
    assert_eq!(
        harness.state().selected_face_role(),
        Some(artificer_kernel::FaceRole::PositiveZ)
    );
    assert_eq!(harness.state().last_error_code(), None);
}

#[test]
fn history_records_each_successful_transform_but_not_staging_or_cancellation() {
    let mut harness = harness();
    harness.run();
    let base_history = vec!["Origin".to_owned(), "Base body".to_owned()];
    assert_eq!(harness.state().feature_timeline_entries(), base_history);

    click_tool(&mut harness, "M", "Move");
    drag_viewport(&mut harness, egui::vec2(36.0, -14.0));
    assert_eq!(harness.state().feature_timeline_entries(), base_history);
    cancel_with_red_button(&mut harness);
    assert_eq!(harness.state().feature_timeline_entries(), base_history);

    drag_viewport(&mut harness, egui::vec2(36.0, -14.0));
    assert_eq!(harness.state().feature_timeline_entries(), base_history);
    confirm_with_tick(&mut harness);
    let once = vec![
        "Origin".to_owned(),
        "Base body".to_owned(),
        "Transform 1".to_owned(),
    ];
    assert_eq!(harness.state().feature_timeline_entries(), once);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Transform 1 feature")
            .is_some()
    );

    drag_viewport(&mut harness, egui::vec2(-18.0, 9.0));
    assert_eq!(harness.state().feature_timeline_entries(), once);
    confirm_with_tick(&mut harness);
    assert_eq!(
        harness.state().feature_timeline_entries(),
        vec![
            "Origin".to_owned(),
            "Base body".to_owned(),
            "Transform 1".to_owned(),
            "Transform 2".to_owned(),
        ]
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Transform 2 feature")
            .is_some()
    );
}

#[test]
fn red_cancel_changes_no_committed_camera_motion_or_selection_state() {
    let mut harness = harness();
    harness
        .get_by_role_and_label(Role::Button, "Positive Z face")
        .click_accesskit();
    harness.run();
    let selected = harness.state().selected_face();
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let view = harness.state().view_parameters();
    let frame = harness.state().view_frame();
    harness.state_mut().set_animation_phase(0.73);

    click_tool(&mut harness, "S", "Scale");
    drag_viewport(&mut harness, egui::vec2(0.0, -55.0));
    assert!(harness.state().transform_preview_pending());
    assert!(harness.query_by_label("PREVIEW — NOT COMMITTED").is_some());
    assert_compact_confirmation_controls(&harness);

    cancel_with_red_button(&mut harness);

    assert!(!harness.state().transform_preview_pending());
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().selected_face(), selected);
    assert_eq!(harness.state().view_parameters(), view);
    assert_eq!(harness.state().view_frame(), frame);
    assert!((harness.state().animation_phase() - 0.73).abs() <= EPSILON);
    assert_eq!(harness.state().last_error_code(), None);
}

#[test]
fn rejected_transform_retains_preview_model_selection_camera_and_motion() {
    let mut harness = harness();
    harness
        .get_by_role_and_label(Role::Button, "Positive Z face")
        .click_accesskit();
    harness.run();
    harness.state_mut().set_animation_phase(0.41);
    click_tool(&mut harness, "S", "Scale");

    let mut observed_rejection = false;
    for _ in 0..10 {
        drag_viewport(&mut harness, egui::vec2(0.0, 220.0));
        let snapshot = harness.state().displayed_snapshot_id();
        let digest = harness.state().displayed_semantic_digest();
        let preview = harness.state().displayed_transform();
        let preview_base = harness.state().transform_preview_base();
        let selected = harness.state().selected_face();
        let view = harness.state().view_parameters();
        let frame = harness.state().view_frame();
        let phase = harness.state().animation_phase();

        confirm_with_tick(&mut harness);

        if harness.state().last_error_code() == Some(KernelErrorCode::InvalidInput) {
            assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
            assert_eq!(harness.state().displayed_semantic_digest(), digest);
            assert_eq!(harness.state().displayed_transform(), preview);
            assert_eq!(harness.state().transform_preview_base(), preview_base);
            assert_eq!(harness.state().selected_face(), selected);
            assert_eq!(harness.state().view_parameters(), view);
            assert_eq!(harness.state().view_frame(), frame);
            assert!((harness.state().animation_phase() - phase).abs() <= EPSILON);
            assert!(harness.state().transform_preview_pending());
            assert!(harness.state().operation_confirmation_pending());
            assert!(
                harness
                    .query_by_role_and_label(Role::Button, CONFIRM_OPERATION)
                    .is_some()
            );
            assert!(
                harness
                    .query_by_role_and_label(Role::Button, CANCEL_OPERATION)
                    .is_some()
            );
            assert_eq!(
                harness.state().last_error_code(),
                Some(KernelErrorCode::InvalidInput)
            );
            observed_rejection = true;
            break;
        }

        assert_eq!(harness.state().last_error_code(), None);
        assert!(!harness.state().transform_preview_pending());
    }

    assert!(
        observed_rejection,
        "repeated valid scale commits must eventually hit minimum feature policy"
    );
}

#[test]
fn case_commands_are_blocked_while_a_preview_is_dirty() {
    let mut harness = diagnostic_harness();
    let snapshot = harness.state().displayed_snapshot_id();
    with_model_reachable(&mut harness, |harness| {
        click_tool(harness, "M", "Move");
        drag_viewport(harness, egui::vec2(60.0, 20.0));
    });

    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .scroll_to_me();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();

    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().last_error_code(), None);
    assert!(harness.state().transform_preview_pending());
}

#[test]
fn transform_model_tools_are_blocked_while_a_case_is_pending() {
    let mut harness = diagnostic_harness();
    let attempts = harness.state().transaction_attempt_count();
    harness
        .get_by_role_and_label(Role::Button, "Zero width")
        .click_accesskit();
    harness.run();

    harness
        .get_by_role_and_label(Role::Button, "M  Move")
        .click_accesskit();
    harness.run();
    harness.key_press(egui::Key::R);
    harness.step();

    assert_eq!(harness.state().active_tool_label(), "Select");
    assert_eq!(
        harness.state().displayed_transform(),
        ([0.0; 3], [0.0; 3], 1.0)
    );
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Zero width")
    );
}

#[test]
fn universal_orbit_and_zoom_work_without_leaving_select() {
    let mut harness = harness();
    assert_eq!(harness.state().active_tool_label(), "Select");
    let transform_before = harness.state().displayed_transform();
    let view_before = harness.state().view_parameters();

    secondary_drag_viewport(&mut harness, egui::vec2(48.0, -26.0));
    let view_after_orbit = harness.state().view_parameters();
    assert!((view_after_orbit.0 - view_before.0).abs() > EPSILON);
    assert!((view_after_orbit.1 - view_before.1).abs() > EPSILON);

    let viewport_center = harness.get_by_label("Model viewport").rect().center();
    harness.hover_at(viewport_center);
    harness.step();
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 80.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();

    let view_after_zoom = harness.state().view_parameters();
    assert!((view_after_zoom.2 - view_after_orbit.2).abs() > EPSILON);
    assert_eq!(harness.state().displayed_transform(), transform_before);
}

#[test]
fn shaded_display_and_view_cube_controls_are_live_toolbar_actions() {
    let mut harness = harness();
    open_ribbon_tab(&mut harness, "View ribbon tab");
    assert!(harness.state().shaded_display_enabled());
    harness
        .get_by_role_and_label(Role::Button, "Shaded")
        .click_accesskit();
    harness.run();
    assert!(!harness.state().shaded_display_enabled());
    harness
        .get_by_role_and_label(Role::Button, "Shaded")
        .click_accesskit();
    harness.run();
    assert!(harness.state().shaded_display_enabled());

    let initial_view = harness.state().view_parameters();
    harness
        .get_by_role_and_label(Role::Button, "View cube top")
        .click_accesskit();
    harness.run();
    let top_view = harness.state().view_parameters();
    assert_ne!(top_view, initial_view);

    harness
        .get_by_role_and_label(Role::Button, "Rotate view clockwise")
        .click_accesskit();
    harness.run();
    assert_ne!(harness.state().view_parameters(), top_view);

    harness
        .get_by_role_and_label(Role::Button, "Reset to isometric view")
        .click_accesskit();
    harness.run();
    assert_eq!(harness.state().view_parameters(), initial_view);
}

#[test]
fn shift_mouse_motion_spring_loads_orbit_and_restores_the_selected_tool() {
    let mut harness = harness();
    click_tool(&mut harness, "M", "Move");

    let viewport_center = harness.get_by_label("Model viewport").rect().center();
    harness.hover_at(viewport_center);
    harness.step();
    let view_before = harness.state().view_parameters();
    let transform_before = harness.state().displayed_transform();

    harness.input_mut().modifiers = egui::Modifiers::SHIFT;
    harness.hover_at(viewport_center + egui::vec2(42.0, -23.0));
    harness.step();

    let view_after = harness.state().view_parameters();
    assert!((view_after.0 - view_before.0).abs() > EPSILON);
    assert!((view_after.1 - view_before.1).abs() > EPSILON);
    assert_eq!(harness.state().displayed_transform(), transform_before);
    assert_eq!(harness.state().active_tool_label(), "Move");

    // An idle frame and releasing Shift perform no extra camera movement and
    // leave the exact prior modeling tool selected.
    harness.step();
    assert_eq!(harness.state().view_parameters(), view_after);
    harness.input_mut().modifiers = egui::Modifiers::NONE;
    harness.step();
    assert_eq!(harness.state().active_tool_label(), "Move");

    drag_viewport(&mut harness, egui::vec2(30.0, 18.0));
    assert_ne!(harness.state().displayed_transform(), transform_before);
}

#[test]
fn orbit_returns_a_face_focused_camera_to_the_visible_document_centre() {
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));
    harness.state_mut().set_face_camera_animation(true);
    harness.state_mut().set_animation_playing(false);
    harness.run();

    let document_centre = harness.state().view_frame().0;
    harness
        .get_by_role_and_label(Role::Button, "Positive X face")
        .click_accesskit();
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Sketch on selected face")
        .click_accesskit();
    harness.step();
    while harness.state().face_camera_transition_active() {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert_ne!(harness.state().view_frame().0, document_centre);

    harness
        .get_by_role_and_label(Role::Button, "Model mode")
        .click_accesskit();
    harness.run();
    secondary_drag_viewport(&mut harness, egui::vec2(36.0, -19.0));

    assert_eq!(harness.state().view_frame().0, document_centre);
}

#[test]
fn animation_advances_at_fixed_test_time_and_pauses_cleanly() {
    let mut harness = diagnostic_harness();
    open_collapsible(&mut harness, "MOTION");
    open_ribbon_tab(&mut harness, "View ribbon tab");
    assert!(!harness.state().animation_playing());

    harness
        .get_by_role_and_label(Role::Button, "Play motion")
        .click_accesskit();
    harness.step();
    assert!(harness.state().animation_playing());

    let start_phase = harness.state().animation_phase();
    harness.run_steps(61);
    let expected_phase =
        (start_phase + std::f64::consts::TAU * 0.1).rem_euclid(std::f64::consts::TAU);
    assert!((harness.state().animation_phase() - expected_phase).abs() <= 1.0e-5);
    assert!((harness.state().reported_fps().unwrap() - 60.0).abs() <= 0.01);
    assert!(harness.query_all_by_label("60 FPS UI").count() >= 2);

    harness
        .get_by_role_and_label(Role::Button, "Stop motion")
        .click_accesskit();
    harness.step();
    assert!(!harness.state().animation_playing());
    harness.run();
    assert!(harness.query_by_label("ANIMATION STOPPED").is_some());

    let paused_phase = harness.state().animation_phase();
    assert!(paused_phase.abs() <= EPSILON);
    harness.run_steps(12);
    assert!((harness.state().animation_phase() - paused_phase).abs() <= EPSILON);
}

#[test]
fn keyboard_shortcuts_separate_view_reset_pending_cancel_and_confirm() {
    let mut harness = harness();
    let initial_view = harness.state().view_parameters();

    harness.key_press(egui::Key::M);
    harness.step();
    assert_eq!(harness.state().active_tool_label(), "Move");
    drag_viewport(&mut harness, egui::vec2(65.0, -20.0));
    assert_ne!(harness.state().displayed_transform().0, [0.0; 3]);
    secondary_drag_viewport(&mut harness, egui::vec2(35.0, 20.0));
    assert_ne!(harness.state().view_parameters(), initial_view);

    harness.key_press(egui::Key::Space);
    harness.step();
    assert!(harness.state().animation_playing());
    harness.run_steps(10);

    harness.key_press(egui::Key::Space);
    harness.step();
    assert!(!harness.state().animation_playing());

    harness.key_press(egui::Key::Home);
    harness.step();
    assert_ne!(harness.state().displayed_transform().0, [0.0; 3]);
    let reset_view = harness.state().view_parameters();
    assert!((reset_view.0 - initial_view.0).abs() <= EPSILON);
    assert!((reset_view.1 - initial_view.1).abs() <= EPSILON);
    assert!((reset_view.2 - initial_view.2).abs() <= EPSILON);
    let phase = harness.state().animation_phase();
    assert!(phase.abs() <= EPSILON);

    harness.key_press(egui::Key::Escape);
    harness.step();
    assert_eq!(
        harness.state().displayed_transform(),
        ([0.0; 3], [0.0; 3], 1.0)
    );
    assert_eq!(harness.state().view_parameters(), reset_view);
    assert!((harness.state().animation_phase() - phase).abs() <= EPSILON);

    click_tool(&mut harness, "M", "Move");
    drag_viewport(&mut harness, egui::vec2(50.0, 10.0));
    let snapshot = harness.state().displayed_snapshot_id();
    harness.key_press(egui::Key::Enter);
    harness.step();
    assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
    assert!(!harness.state().transform_preview_pending());
}

#[test]
fn confirm_and_cancel_shortcuts_remain_global_while_numeric_controls_have_focus() {
    let mut harness = diagnostic_harness();
    open_collapsible(&mut harness, "TRANSFORM PREVIEW");
    harness
        .get_by_role_and_label(Role::SpinButton, "Display scale")
        .focus();
    harness.run();
    harness.key_press(egui::Key::M);
    harness.step();
    assert_eq!(harness.state().active_tool_label(), "Select");

    let snapshot = harness.state().displayed_snapshot_id();
    harness
        .get_by_role_and_label(Role::SpinButton, "Display scale")
        .focus();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::SpinButton, "Display scale")
        .type_text("1.25");
    harness.run();
    assert!(harness.state().operation_confirmation_pending());
    harness.key_press(egui::Key::Enter);
    harness.step();
    assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
    assert!(!harness.state().transform_preview_pending());
    harness
        .get_by_role_and_label(Role::Button, "Reset view")
        .click_accesskit();
    harness.run();
    assert_eq!(
        harness.state().displayed_transform(),
        ([0.0; 3], [0.0; 3], 1.0),
        "losing numeric focus after Confirm must not resurrect its edit buffer"
    );

    harness
        .get_by_role_and_label(Role::SpinButton, "Display scale")
        .focus();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::SpinButton, "Display scale")
        .type_text("1.15");
    harness.run();
    harness.key_press(egui::Key::Escape);
    harness.step();
    assert!(
        !harness.state().transform_preview_pending(),
        "cancel left preview {:?}",
        harness.state().displayed_transform()
    );
    harness
        .get_by_role_and_label(Role::Button, "Reset view")
        .click_accesskit();
    harness.run();
    assert_eq!(
        harness.state().displayed_transform(),
        ([0.0; 3], [0.0; 3], 1.0),
        "losing numeric focus after Cancel must not resurrect its edit buffer"
    );
}

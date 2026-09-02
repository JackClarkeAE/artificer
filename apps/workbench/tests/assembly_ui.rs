use artificer_model::CURRENT_DOCUMENT_VERSION;
use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

fn new_harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

/// The component card scrolls when its reference facts are longer than the
/// card, so a control below the fold has to be brought into view first — the
/// same thing the user does.
fn click_scrolled_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
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

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

fn enter_length(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    let input = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    input.click();
    input.type_text(value);
    harness.run();
}

fn insert_component(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Add to current workspace");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Insert library component")
    );
    click_button(harness, "Confirm operation");
}

fn drag_viewport(harness: &mut Harness<'static, KernelLabApp>, delta: egui::Vec2) {
    let start = harness.get_by_label("Model viewport").rect().center();
    let end = start + delta;
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
}

fn undo(harness: &mut Harness<'static, KernelLabApp>) {
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
}

fn redo(harness: &mut Harness<'static, KernelLabApp>) {
    harness.key_press_modifiers(
        egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::NONE
        },
        egui::Key::Z,
    );
    harness.run();
}

fn two_component_assembly() -> Harness<'static, KernelLabApp> {
    let mut harness = new_harness();
    harness.run();
    click_button(&mut harness, "Library");
    enter_length(&mut harness, "80");
    insert_component(&mut harness);
    insert_component(&mut harness);
    click_button(&mut harness, "Library");
    harness
}

#[test]
fn repeated_parts_insert_as_distinct_non_overlapping_occurrences() {
    let harness = two_component_assembly();
    let poses = harness.state().component_poses();
    assert_eq!(poses.len(), 2);
    assert_ne!(poses[0].0, poses[1].0);
    assert_eq!(poses[0].2, [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(poses[1].2, [1.0, 0.0, 0.0, 0.0]);
    assert!(
        poses[1].1[0] - poses[0].1[0] >= 30.0,
        "20 mm parts must retain the configured 10 mm assembly clearance: {poses:?}"
    );
    assert_eq!(harness.state().active_component_instance_id(), Some(2));
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "S  Scale")
            .accesskit_node()
            .is_disabled(),
        "component occurrences must remain rigid and scale-free"
    );
}

#[test]
fn placement_preview_cancel_confirm_and_undo_never_rewrite_component_geometry() {
    let mut harness = two_component_assembly();
    let original_poses = harness.state().component_poses();
    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    let attempts = harness.state().transaction_attempt_count();
    let features = harness.state().document_feature_count();

    click_button(&mut harness, "M  Move");
    drag_viewport(&mut harness, egui::vec2(85.0, -30.0));
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Place component")
    );
    assert_eq!(harness.state().component_poses(), original_poses);
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    click_button(&mut harness, "Cancel operation");
    assert_eq!(harness.state().component_poses(), original_poses);

    click_button(&mut harness, "M  Move");
    drag_viewport(&mut harness, egui::vec2(85.0, -30.0));
    click_button(&mut harness, "Confirm operation");
    let moved_poses = harness.state().component_poses();
    assert_ne!(moved_poses, original_poses);
    assert_eq!(moved_poses[0], original_poses[0]);
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
    assert_eq!(harness.state().transaction_attempt_count(), attempts);
    assert_eq!(harness.state().document_feature_count(), features);

    undo(&mut harness);
    assert_eq!(harness.state().component_poses(), original_poses);
    redo(&mut harness);
    assert_eq!(harness.state().component_poses(), moved_poses);
}

#[test]
fn grounding_and_named_revolute_joint_share_the_confirmation_gate_and_persist() {
    let mut harness = two_component_assembly();
    let poses = harness.state().component_poses();

    click_scrolled_button(&mut harness, "Ground component");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Ground component")
    );
    click_button(&mut harness, "Cancel operation");
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "M  Move")
            .accesskit_node()
            .is_disabled()
    );

    click_scrolled_button(&mut harness, "Ground component");
    click_button(&mut harness, "Confirm operation");
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "M  Move")
            .accesskit_node()
            .is_disabled()
    );
    assert_eq!(harness.state().component_poses(), poses);

    click_scrolled_button(&mut harness, "Release component");
    click_button(&mut harness, "Confirm operation");
    click_scrolled_button(&mut harness, "Add revolute joint");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Create revolute joint")
    );
    assert_eq!(harness.state().assembly_joint_count(), 0);
    click_button(&mut harness, "Cancel operation");
    assert_eq!(harness.state().assembly_joint_count(), 0);

    click_scrolled_button(&mut harness, "Add revolute joint");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().assembly_joint_count(), 1);
    let summaries = harness.state().assembly_joint_summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].1, "20 × 20 Aluminium Extrusion Rotation");
    assert_eq!(summaries[0].2, 2);
    assert_eq!(summaries[0].3, "Revolute");
    assert!(summaries[0].4);
    click_button(&mut harness, "Browser");
    assert!(harness.query_by_label("Joints (1)").is_some());

    let saved = harness.state().native_document_json().unwrap();
    let native: serde_json::Value =
        serde_json::from_str(&saved).expect("saved assembly should be valid JSON");
    assert_eq!(native["version"], CURRENT_DOCUMENT_VERSION);
    let mut restored = new_harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("assembly document should hydrate in a fresh workbench");
    restored.run();
    assert_eq!(restored.state().component_poses(), poses);
    assert_eq!(restored.state().assembly_joint_summaries(), summaries);
}

#[test]
fn a_revolute_joint_poses_its_component_and_stands_the_turntable_down() {
    let mut harness = two_component_assembly();
    // No mechanism yet: the animation is the turntable, and every
    // component stands where the document assembled it.
    assert!(!harness.state().animation_drives_joints());
    assert_eq!(
        harness.state().solved_component_poses(),
        harness.state().component_poses()
    );

    click_scrolled_button(&mut harness, "Add revolute joint");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().assembly_joint_count(), 1);

    let joints = harness.state().drivable_joints();
    assert_eq!(joints.len(), 1, "one revolute joint to drive");
    let hinge = joints[0].id;
    assert!(
        joints[0].limits.is_none(),
        "the workbench's joint is unlimited, so its slider shows one turn"
    );

    // At rest the mechanism is the assembled document, exactly.
    assert_eq!(harness.state().joint_angle(hinge), 0.0);
    assert_eq!(
        harness.state().solved_component_poses(),
        harness.state().component_poses()
    );

    // Driving it moves the jointed component and nothing else. The joint
    // sits on the component's own pivot, so a half turn carries it to the
    // far side of that pivot.
    let assembled = harness.state().component_poses();
    let jointed = harness
        .state()
        .active_component_instance_id()
        .expect("an active component");
    harness
        .state_mut()
        .set_joint_angle(hinge, std::f64::consts::PI);
    harness.run();

    let posed = harness.state().solved_component_poses();
    assert_eq!(
        posed.len(),
        assembled.len(),
        "posing adds and removes no components"
    );
    for (solved, rest) in posed.iter().zip(&assembled) {
        assert_eq!(solved.0, rest.0);
        if solved.0 == jointed {
            assert_ne!(solved.1, rest.1, "the jointed component moved");
        } else {
            assert_eq!(solved.1, rest.1, "an unjointed component did not");
        }
    }

    // The document itself never moved: a joint coordinate is a pose, not
    // an edit.
    assert_eq!(harness.state().component_poses(), assembled);

    // And the animation now belongs to the mechanism.
    assert!(harness.state().animation_drives_joints());
}

#[test]
fn a_sweep_measures_the_travel_the_animation_plays() {
    let mut harness = two_component_assembly();

    // Without a joint there is no motion to sweep, and the workbench says
    // so rather than sweeping one pose and calling it a travel.
    harness.state_mut().run_interference_sweep();
    assert!(harness.state().sweep_report().is_none());
    assert!(
        harness
            .state()
            .document_status_text()
            .is_some_and(|status| status.contains("joint")),
        "{:?}",
        harness.state().document_status_text()
    );

    click_scrolled_button(&mut harness, "Add revolute joint");
    click_button(&mut harness, "Confirm operation");
    assert!(harness.state().animation_drives_joints());

    harness.state_mut().run_interference_sweep();
    // The sweep runs off the UI thread, so the frames keep coming while it
    // works — which is the point of running it there.
    for _ in 0..600 {
        harness.run();
        if !harness.state().sweep_is_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!harness.state().sweep_is_running(), "the sweep finished");
    let report = harness
        .state()
        .sweep_report()
        .unwrap_or_else(|| panic!("a sweep: {:?}", harness.state().document_status_text()))
        .clone();
    assert!(
        report.subjects.len() >= 2,
        "every visible body takes part: {:?}",
        report.subjects
    );
    assert!(report.steps_offered > 1, "a travel, not a pose");
    assert!(!report.cancelled);
    assert!(report.steps_measured > 0);
    // Either the mechanism cleared its whole travel, or it stopped where
    // it did not; both are answers, and neither is silence.
    if let Some(collision) = report.collision.as_ref() {
        assert_eq!(report.steps_measured, collision.step + 1);
    } else {
        assert_eq!(report.steps_measured, report.steps_offered);
    }

    // The sweep leaves the picture of the whole motion behind, bound to
    // the facets it was measured on.
    let heat_map = harness.state().heat_map_sample_counts();
    assert_eq!(
        heat_map.len(),
        report.subjects.len(),
        "one field per swept body"
    );
    for (body, samples) in heat_map {
        assert!(samples > 0, "body {body} has no readings");
    }
}

#[test]
fn a_part_can_be_inserted_into_the_open_design_from_the_ribbon() {
    let mut harness = new_harness();
    harness.run();
    assert!(
        !harness.state().part_library_open(),
        "the library starts closed"
    );

    // The Model tab's Create group offers it, alongside the sketch and the
    // construction plane, because inserting a part is making something in
    // this design rather than a view of it.
    click_button(&mut harness, "Insert a part into this design");
    assert!(
        harness.state().part_library_open(),
        "the command opens the library"
    );
    assert!(
        harness
            .state()
            .document_status_text()
            .is_some_and(|status| status.contains("insert into this design")),
        "{:?}",
        harness.state().document_status_text()
    );

    // And the part lands in the design that was already open, as a second
    // occurrence beside the one there rather than a new document.
    enter_length(&mut harness, "80");
    insert_component(&mut harness);
    let first = harness.state().component_poses().len();
    assert_eq!(first, 1, "one occurrence so far");

    insert_component(&mut harness);
    let poses = harness.state().component_poses();
    assert_eq!(poses.len(), 2, "the second part joins the same design");
    assert_ne!(poses[0].0, poses[1].0, "distinct occurrences");
}

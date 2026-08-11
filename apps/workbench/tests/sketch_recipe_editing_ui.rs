use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchPoint},
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
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    click_at(harness, canvas_sketch_point(harness, point));
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn confirm(harness: &mut Harness<'static, KernelLabApp>) {
    assert!(harness.state().operation_confirmation_pending());
    click_button(harness, CONFIRM_OPERATION);
    assert!(!harness.state().operation_confirmation_pending());
}

fn replace_selected_parameter(
    harness: &mut Harness<'static, KernelLabApp>,
    label: &str,
    value: &str,
) {
    {
        let input = harness.get_by_role_and_label(Role::TextInput, label);
        input.scroll_to_me();
    }
    harness.run();
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn replace_active_tool_parameter(
    harness: &mut Harness<'static, KernelLabApp>,
    label: &str,
    value: &str,
) {
    replace_selected_parameter(harness, label, value);
}

fn create_two_point_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Two-point rectangle");
    click_sketch_point(harness, SketchPoint::new(-1.0, -1.0));
    click_sketch_point(harness, SketchPoint::new(1.0, 1.0));
    assert!(!harness.state().operation_confirmation_pending());
    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed { .. } | CertifiedProfileStatus::ClosedRegions { .. }
    ));
}

fn select_rectangle_top(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Select sketch geometry");
    click_sketch_point(harness, SketchPoint::new(0.0, 1.0));
    harness.get_by_label("SELECTED FEATURE");
}

fn create_right_angle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Single line");
    for (start, end) in [
        (SketchPoint::new(-2.0, 0.0), SketchPoint::new(0.0, 0.0)),
        (SketchPoint::new(0.0, 0.0), SketchPoint::new(0.0, 2.0)),
    ] {
        click_sketch_point(harness, start);
        click_sketch_point(harness, end);
        assert!(!harness.state().operation_confirmation_pending());
    }
}

fn pick_right_angle_sources(harness: &mut Harness<'static, KernelLabApp>) {
    click_sketch_point(harness, SketchPoint::new(-1.0, 0.0));
    click_sketch_point(harness, SketchPoint::new(0.0, 1.0));
    assert!(!harness.state().operation_confirmation_pending());
    click_button(harness, "Select sketch geometry");
}

#[test]
fn rectangle_recipe_edit_previews_commits_and_drives_exact_new_body_volume() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    select_rectangle_top(&mut harness);

    let revision = harness.state().sketch_revision();
    replace_selected_parameter(&mut harness, "Width", "3");
    replace_selected_parameter(&mut harness, "Height", "2");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Edit sketch parameters")
    );
    assert_eq!(harness.state().sketch_revision(), revision);
    assert_eq!(harness.state().sketch_pending_entity_count(), 4);
    confirm(&mut harness);
    assert_eq!(harness.state().sketch_revision(), revision + 1);
    let editor = harness
        .state()
        .selected_sketch_recipe_editor()
        .expect("edited rectangle stays selected");
    assert_eq!(editor.title, "Two-point rectangle");
    assert_eq!(editor.parameters[0].text, "3");
    assert_eq!(editor.parameters[1].text, "2");

    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    confirm(&mut harness);
    let volume = harness.state().displayed_measures().unwrap().volume;
    assert!((volume - 24.0).abs() <= 1.0e-9);
}

#[test]
fn invalid_selected_parameter_keeps_last_preview_and_cross_cancels_neutrally() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    select_rectangle_top(&mut harness);

    let revision = harness.state().sketch_revision();
    replace_selected_parameter(&mut harness, "Width", "3");
    assert!(harness.state().operation_confirmation_pending());
    replace_selected_parameter(&mut harness, "Width", "not-a-number");
    let confirm = harness.get_by_role_and_label(Role::Button, CONFIRM_OPERATION);
    assert!(confirm.accesskit_node().is_disabled());
    assert_eq!(harness.state().sketch_pending_entity_count(), 4);

    click_button(&mut harness, CANCEL_OPERATION);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_revision(), revision);
    let editor = harness
        .state()
        .selected_sketch_recipe_editor()
        .expect("cancel retains selection");
    assert_eq!(editor.parameters[0].text, "2");
}

#[test]
fn polygon_and_slot_literals_replay_as_exact_compound_features() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Outer-diameter polygon");
    replace_active_tool_parameter(&mut harness, "Sides", "4");
    replace_active_tool_parameter(&mut harness, "Outer diameter", "2");
    replace_active_tool_parameter(&mut harness, "Rotation", "45");
    click_sketch_point(&mut harness, SketchPoint::new(-3.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 0.0));
    assert!(!harness.state().operation_confirmation_pending());
    click_button(&mut harness, "Select sketch geometry");
    let old_polygon_ids = harness.state().selected_sketch_recipe_output_ids();
    assert_eq!(old_polygon_ids.len(), 4);

    replace_selected_parameter(&mut harness, "Sides", "6");
    replace_selected_parameter(&mut harness, "Outer diameter", "4");
    assert_eq!(harness.state().sketch_pending_entity_count(), 6);
    confirm(&mut harness);
    let new_polygon_ids = harness.state().selected_sketch_recipe_output_ids();
    assert_eq!(new_polygon_ids.len(), 6);
    assert_eq!(&new_polygon_ids[..4], old_polygon_ids.as_slice());
    let polygon = harness.state().selected_sketch_recipe_editor().unwrap();
    assert_eq!(polygon.parameters[0].text, "6");
    assert_eq!(polygon.parameters[1].text, "4");

    click_button(&mut harness, "Two-point centre-to-centre slot");
    replace_active_tool_parameter(&mut harness, "Centre distance", "2");
    replace_active_tool_parameter(&mut harness, "Width", "1");
    replace_active_tool_parameter(&mut harness, "Angle", "0");
    for point in [
        SketchPoint::new(1.0, 0.0),
        SketchPoint::new(3.0, 0.0),
        SketchPoint::new(2.0, 0.5),
    ] {
        click_sketch_point(&mut harness, point);
    }
    assert!(!harness.state().operation_confirmation_pending());
    click_button(&mut harness, "Select sketch geometry");
    assert_eq!(
        harness
            .state()
            .selected_sketch_recipe_editor()
            .unwrap()
            .title,
        "Two-point slot"
    );
    let old_slot_ids = harness.state().selected_sketch_recipe_output_ids();
    replace_selected_parameter(&mut harness, "Width", "1.5");
    confirm(&mut harness);
    assert_eq!(
        harness.state().selected_sketch_recipe_output_ids(),
        old_slot_ids
    );
    assert_eq!(
        harness
            .state()
            .selected_sketch_recipe_editor()
            .unwrap()
            .parameters[0]
            .text,
        "1.5"
    );
}

#[test]
fn both_pattern_counts_replay_with_stable_existing_output_ids() {
    let mut rectangular = harness();
    enter_xy_sketch(&mut rectangular);
    click_button(&mut rectangular, "Single line");
    click_sketch_point(&mut rectangular, SketchPoint::new(-2.0, 0.0));
    click_sketch_point(&mut rectangular, SketchPoint::new(-1.0, 0.0));
    assert!(!rectangular.state().operation_confirmation_pending());
    click_button(&mut rectangular, "Rectangular sketch pattern");
    replace_active_tool_parameter(&mut rectangular, "First count", "3");
    replace_active_tool_parameter(&mut rectangular, "First spacing", "1");
    click_sketch_point(&mut rectangular, SketchPoint::new(-0.5, 0.0));
    assert!(!rectangular.state().operation_confirmation_pending());
    click_button(&mut rectangular, "Select sketch geometry");
    let before = rectangular.state().selected_sketch_recipe_output_ids();
    assert_eq!(before.len(), 2);
    replace_selected_parameter(&mut rectangular, "Columns", "4");
    confirm(&mut rectangular);
    let after = rectangular.state().selected_sketch_recipe_output_ids();
    assert_eq!(after.len(), 3);
    assert_eq!(&after[..2], before.as_slice());

    let mut circular = harness();
    enter_xy_sketch(&mut circular);
    click_button(&mut circular, "Single line");
    click_sketch_point(&mut circular, SketchPoint::new(-2.0, 0.0));
    click_sketch_point(&mut circular, SketchPoint::new(-1.0, 0.0));
    assert!(!circular.state().operation_confirmation_pending());
    click_button(
        &mut circular,
        "Choose pattern type; current default: Rectangular sketch pattern.",
    );
    click_button(&mut circular, "Circular sketch pattern");
    replace_active_tool_parameter(&mut circular, "Count", "3");
    click_sketch_point(&mut circular, SketchPoint::new(0.0, -2.0));
    assert!(!circular.state().operation_confirmation_pending());
    click_button(&mut circular, "Select sketch geometry");
    let before = circular.state().selected_sketch_recipe_output_ids();
    assert_eq!(before.len(), 2);
    replace_selected_parameter(&mut circular, "Count", "5");
    confirm(&mut circular);
    let after = circular.state().selected_sketch_recipe_output_ids();
    assert_eq!(after.len(), 4);
    assert_eq!(&after[..2], before.as_slice());
}

#[test]
fn fillet_and_both_chamfer_parameter_forms_replay_exactly() {
    let mut fillet = harness();
    enter_xy_sketch(&mut fillet);
    create_right_angle(&mut fillet);
    click_button(&mut fillet, "2D fillet");
    replace_active_tool_parameter(&mut fillet, "Fillet radius", "0.75");
    pick_right_angle_sources(&mut fillet);
    assert_eq!(
        fillet
            .state()
            .selected_sketch_recipe_editor()
            .unwrap()
            .title,
        "2D fillet"
    );
    let fillet_ids = fillet.state().selected_sketch_recipe_output_ids();
    replace_selected_parameter(&mut fillet, "Radius", "0.5");
    confirm(&mut fillet);
    assert_eq!(
        fillet.state().selected_sketch_recipe_output_ids(),
        fillet_ids
    );

    let mut equal = harness();
    enter_xy_sketch(&mut equal);
    create_right_angle(&mut equal);
    click_button(&mut equal, "Equal-distance chamfer");
    replace_active_tool_parameter(&mut equal, "Chamfer distance", "0.75");
    pick_right_angle_sources(&mut equal);
    let editor = equal.state().selected_sketch_recipe_editor().unwrap();
    assert_eq!(editor.parameters.len(), 1);
    assert_eq!(editor.parameters[0].label, "Distance");
    replace_selected_parameter(&mut equal, "Distance", "0.5");
    confirm(&mut equal);
    let editor = equal.state().selected_sketch_recipe_editor().unwrap();
    assert_eq!(editor.parameters.len(), 1);
    assert_eq!(editor.parameters[0].text, "0.5");

    let mut unequal = harness();
    enter_xy_sketch(&mut unequal);
    create_right_angle(&mut unequal);
    click_button(
        &mut unequal,
        "Choose chamfer type; current default: Equal-distance chamfer.",
    );
    click_button(&mut unequal, "Two-distance chamfer");
    replace_active_tool_parameter(&mut unequal, "First distance", "0.5");
    replace_active_tool_parameter(&mut unequal, "Second distance", "0.75");
    pick_right_angle_sources(&mut unequal);
    let editor = unequal.state().selected_sketch_recipe_editor().unwrap();
    assert_eq!(editor.parameters.len(), 2);
    replace_selected_parameter(&mut unequal, "Distance 1", "0.4");
    replace_selected_parameter(&mut unequal, "Distance 2", "0.6");
    confirm(&mut unequal);
    let editor = unequal.state().selected_sketch_recipe_editor().unwrap();
    assert_eq!(editor.parameters[0].text, "0.4");
    assert_eq!(editor.parameters[1].text, "0.6");
}

#[test]
fn saved_v6_recipe_reopens_editable_and_persists_one_logical_revision() {
    let mut source = harness();
    enter_xy_sketch(&mut source);
    create_two_point_rectangle(&mut source);
    click_button(&mut source, "Finish sketch command");
    assert_eq!(source.state().workbench_mode(), WorkbenchMode::Model);
    let saved = source.state().native_document_json().unwrap();

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("v6 authoring should hydrate in a fresh process");
    restored.run();
    let feature_count = restored.state().document_feature_count();
    click_button(&mut restored, "Sketch 1 feature");
    assert_eq!(restored.state().workbench_mode(), WorkbenchMode::Sketch);
    select_rectangle_top(&mut restored);
    replace_selected_parameter(&mut restored, "Width", "3");
    confirm(&mut restored);
    click_button(&mut restored, "Finish sketch command");
    assert_eq!(restored.state().document_feature_count(), feature_count);
    assert_eq!(restored.state().document_dirty_feature_count(), 1);
    click_button(&mut restored, "Rebuild selected branch");
    assert_eq!(restored.state().document_dirty_feature_count(), 0);
    assert!(
        restored
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == "Sketch 1 · r2")
    );

    let modified = restored.state().native_document_json().unwrap();
    let mut round_trip = harness();
    round_trip.run();
    round_trip
        .state_mut()
        .load_native_document_json(&modified)
        .expect("modified v6 recipe should replay");
    round_trip.run();
    assert_eq!(round_trip.state().native_document_json().unwrap(), modified);
    assert_eq!(round_trip.state().sketch_entity_count(), 4);
}

#[test]
fn edited_extruded_sketch_rebuilds_in_place_and_cancel_stays_neutral() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    create_two_point_rectangle(&mut harness);
    click_button(&mut harness, "Extrude");
    confirm(&mut harness);
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_feature_count = harness.state().document_feature_count();
    assert!((harness.state().displayed_measures().unwrap().volume - 16.0).abs() <= 1.0e-9);

    click_button(&mut harness, "Sketch 1 feature");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    select_rectangle_top(&mut harness);
    replace_selected_parameter(&mut harness, "Width", "3");
    replace_selected_parameter(&mut harness, "Width", "invalid");
    click_button(&mut harness, CANCEL_OPERATION);
    assert_eq!(harness.state().document_dirty_feature_count(), 0);
    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().document_feature_count(),
        original_feature_count
    );

    replace_selected_parameter(&mut harness, "Width", "3");
    confirm(&mut harness);
    click_button(&mut harness, "Finish sketch command");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(
        harness.state().document_feature_count(),
        original_feature_count
    );
    assert!(harness.state().document_dirty_feature_count() >= 2);
    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert!((harness.state().displayed_measures().unwrap().volume - 16.0).abs() <= 1.0e-9);

    let rebuild = harness.get_by_role_and_label(Role::Button, "Rebuild selected branch");
    assert!(!rebuild.accesskit_node().is_disabled());
    click_button(&mut harness, "Rebuild selected branch");
    assert_eq!(harness.state().document_dirty_feature_count(), 0);
    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert!((harness.state().displayed_measures().unwrap().volume - 24.0).abs() <= 1.0e-9);
    assert_eq!(harness.state().sketch_revision(), 2);
}

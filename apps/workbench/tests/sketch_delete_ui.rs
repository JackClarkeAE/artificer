use artificer_workbench::{KernelLabApp, WorkbenchMode};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|context| KernelLabApp::new_paused(context))
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
    let position = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, position);
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
    harness.step();
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Create sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn draw_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Two-point rectangle");
    let center = harness.get_by_label("Sketch viewport").rect().center();
    click_at(harness, center + egui::vec2(-80.0, 50.0));
    click_at(harness, center + egui::vec2(80.0, -50.0));
    assert_eq!(harness.state().pending_operation_label(), None);
}

fn select_left_rectangle_edge(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Select sketch geometry");
    let center = harness.get_by_label("Sketch viewport").rect().center();
    click_at(harness, center + egui::vec2(-80.0, 0.0));
}

#[test]
fn delete_uses_tick_cross_enter_escape_and_local_undo_redo() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);

    // Delete commits immediately; the freshly drawn stroke is still the
    // selection, and the local journal is the safety net. Escape first
    // closes the live dimension readout the drawing gesture left open.
    press_key(&mut harness, egui::Key::Escape);
    press_key(&mut harness, egui::Key::Delete);
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 2);

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 3);

    harness.key_press_modifiers(
        egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::NONE
        },
        egui::Key::Z,
    );
    harness.run();
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 4);

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    select_left_rectangle_edge(&mut harness);
    press_key(&mut harness, egui::Key::Delete);
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 6);
}

#[test]
fn focused_dimension_editor_owns_delete_and_cannot_retire_selection() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle(&mut harness);

    click_button(&mut harness, "Two-point rectangle");
    let center = harness.get_by_label("Sketch viewport").rect().center();
    click_at(&mut harness, center + egui::vec2(180.0, 90.0));
    press_key(&mut harness, egui::Key::Tab);
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle width")
            .is_some()
    );
    press_key(&mut harness, egui::Key::Delete);

    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert!(harness.state().sketch_creation_draft_active());
}

#[test]
fn saved_v6_sketch_delete_replaces_one_logical_feature_and_rebuilds() {
    let mut source = harness();
    enter_xy_sketch(&mut source);
    draw_rectangle(&mut source);
    click_button(&mut source, "Finish sketch");
    assert_eq!(source.state().workbench_mode(), WorkbenchMode::Model);
    let saved = source
        .state()
        .native_document_json()
        .expect("serialize v6 sketch");

    let mut restored = harness();
    restored.run();
    restored
        .state_mut()
        .load_native_document_json(&saved)
        .expect("hydrate v6 sketch");
    restored.run();
    assert_eq!(restored.state().sketch_entity_count(), 4);
    assert_eq!(restored.state().sketch_revision(), 1);
    let feature_count = restored.state().document_feature_count();

    click_button(&mut restored, "Sketch 1 feature");
    assert_eq!(restored.state().workbench_mode(), WorkbenchMode::Sketch);
    select_left_rectangle_edge(&mut restored);
    press_key(&mut restored, egui::Key::Delete);
    assert_eq!(restored.state().sketch_entity_count(), 0);
    assert_eq!(restored.state().sketch_revision(), 2);
    assert_eq!(restored.state().visible_model_sketch_overlay_count(), 0);

    restored.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    restored.run();
    assert_eq!(restored.state().sketch_entity_count(), 4);
    assert_eq!(restored.state().sketch_revision(), 3);
    assert_eq!(restored.state().visible_model_sketch_overlay_count(), 1);
    restored.key_press_modifiers(
        egui::Modifiers {
            command: true,
            shift: true,
            ..egui::Modifiers::NONE
        },
        egui::Key::Z,
    );
    restored.run();
    assert_eq!(restored.state().sketch_entity_count(), 0);
    assert_eq!(restored.state().sketch_revision(), 4);
    assert_eq!(restored.state().visible_model_sketch_overlay_count(), 0);

    click_button(&mut restored, "Finish sketch");
    assert_eq!(restored.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(restored.state().document_feature_count(), feature_count);
    assert_eq!(restored.state().sketch_revision(), 2);
    // Finishing the edit replays the dirtied branch on its own; the manual
    // Rebuild press is no longer part of the flow.
    assert_eq!(restored.state().document_dirty_feature_count(), 0);
    assert_eq!(restored.state().document_feature_count(), feature_count);

    let edited = restored
        .state()
        .native_document_json()
        .expect("serialize tombstoned edit");
    let mut rebuilt = harness();
    rebuilt.run();
    rebuilt
        .state_mut()
        .load_native_document_json(&edited)
        .expect("rebuild tombstoned v6 sketch");
    rebuilt.run();
    assert_eq!(rebuilt.state().sketch_entity_count(), 0);
    assert_eq!(rebuilt.state().sketch_revision(), 2);
    assert_eq!(rebuilt.state().visible_model_sketch_overlay_count(), 0);
}

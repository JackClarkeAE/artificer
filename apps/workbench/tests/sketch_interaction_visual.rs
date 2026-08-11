use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{DimensionInputError, SketchDimensionKind, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness, SnapshotOptions,
    kittest::{NodeT as _, Queryable as _},
};

fn harness() -> Harness<'static, KernelLabApp> {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .with_options(SnapshotOptions::new().output_path(snapshot_directory))
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn pointer_button(
    harness: &mut Harness<'static, KernelLabApp>,
    position: egui::Pos2,
    pressed: bool,
) {
    harness.event(egui::Event::PointerMoved(position));
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
}

fn click_at(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
    harness.hover_at(position);
    harness.step();
    pointer_button(harness, position, true);
    pointer_button(harness, position, false);
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    for _ in 0..18 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    harness.get_by_label("Sketch viewport");
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    click_at(harness, canvas_sketch_point(harness, point));
}

fn hover_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    harness.hover_at(canvas_sketch_point(harness, point));
    for _ in 0..3 {
        harness.step();
    }
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn replace_tool_input(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .click();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn commit_line(harness: &mut Harness<'static, KernelLabApp>, start: SketchPoint, end: SketchPoint) {
    click_sketch_point(harness, start);
    click_sketch_point(harness, end);
    assert_eq!(harness.state().pending_operation_label(), None);
}

fn settle_hover_snapshot(harness: &mut Harness<'static, KernelLabApp>, name: &str) {
    // Retain the pointer: the exact hover span and an in-progress manipulator
    // are intentionally pointer-owned presentation states.
    for _ in 0..3 {
        harness.step();
    }
    harness.snapshot(name);
}

fn settle_snapshot(harness: &mut Harness<'static, KernelLabApp>, name: &str) {
    harness.remove_cursor();
    for _ in 0..3 {
        harness.step();
    }
    harness.snapshot(name);
}

#[test]
fn exact_trim_hover_and_staged_span_snapshots() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Single line");

    commit_line(
        &mut harness,
        SketchPoint::new(-4.0, 0.75),
        SketchPoint::new(4.0, 0.75),
    );
    commit_line(
        &mut harness,
        SketchPoint::new(-2.0, -2.5),
        SketchPoint::new(-2.0, 2.5),
    );
    commit_line(
        &mut harness,
        SketchPoint::new(2.0, -2.5),
        SketchPoint::new(2.0, 2.5),
    );
    assert_eq!(harness.state().sketch_entity_count(), 3);
    assert_eq!(harness.state().sketch_revision(), 3);

    // A committed creation remains selected by design. Clear it so the Trim
    // baseline contains exactly one orange semantic highlight: the removable
    // middle span, with no unrelated selected-curve dimensions over it.
    click_button(&mut harness, "Select sketch geometry");
    click_sketch_point(&mut harness, SketchPoint::new(3.0, -3.0));
    click_button(&mut harness, "Trim curve span");
    let middle_span_pick = SketchPoint::new(0.25, 0.75);
    hover_sketch_point(&mut harness, middle_span_pick);

    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_entity_count(), 3);
    assert_eq!(harness.state().sketch_revision(), 3);
    settle_hover_snapshot(&mut harness, "workbench_exact_trim_middle_span_hover_1040");

    click_sketch_point(&mut harness, middle_span_pick);
    // The trim commits as it is picked, like every other stroke.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_revision(), 4);
    settle_snapshot(
        &mut harness,
        "workbench_exact_trim_middle_span_committed_1040",
    );
}

#[test]
fn rectangular_pattern_direction_handle_drag_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Single line");
    commit_line(
        &mut harness,
        SketchPoint::new(-3.5, 0.0),
        SketchPoint::new(-2.5, 0.0),
    );
    assert_eq!(harness.state().sketch_revision(), 1);

    click_button(&mut harness, "Rectangular sketch pattern");
    replace_tool_input(&mut harness, "First spacing", "2");
    let anchor = SketchPoint::new(-3.0, 0.0);
    let initial_handle = canvas_sketch_point(&harness, SketchPoint::new(anchor.u + 2.0, anchor.v));
    let dragged_handle = canvas_sketch_point(&harness, SketchPoint::new(anchor.u, anchor.v + 3.0));

    pointer_button(&mut harness, initial_handle, true);
    harness.event(egui::Event::PointerMoved(dragged_handle));
    harness.step();
    harness.step();

    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "First spacing")
            .value()
            .as_deref(),
        Some("3")
    );
    settle_hover_snapshot(
        &mut harness,
        "workbench_rectangular_pattern_direction_handle_drag_1040",
    );

    pointer_button(&mut harness, dragged_handle, false);
    // Releasing the handle completes the pattern stroke, which commits.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_revision(), 2);
}

#[test]
fn three_point_arc_tab_sweep_validation_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(
        &mut harness,
        "Choose arc type; current default: Centre-start-end arc.",
    );
    click_button(&mut harness, "Three-point arc");

    click_sketch_point(&mut harness, SketchPoint::new(-2.25, -0.75));
    click_sketch_point(&mut harness, SketchPoint::new(2.25, -0.75));
    hover_sketch_point(&mut harness, SketchPoint::new(0.0, 1.75));
    press_key(&mut harness, egui::Key::Tab);

    let sweep = harness.get_by_role_and_label(Role::TextInput, "Arc sweep");
    assert!(sweep.accesskit_node().is_focused());
    sweep.type_text("400");
    harness.run();

    assert_eq!(
        harness.state().sketch_dimension_error(),
        Some(DimensionInputError::SweepOutOfRange)
    );
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 0);
    let readouts = harness.state().sketch_dimension_readouts();
    assert_eq!(readouts.len(), 2);
    assert_eq!(readouts[0].kind, SketchDimensionKind::Radius);
    assert!(!readouts[0].editable);
    assert!(readouts[0].value.is_finite() && readouts[0].value > 0.0);
    assert_eq!(readouts[1].kind, SketchDimensionKind::SweepDegrees);
    assert!(readouts[1].editable);
    assert!(
        harness
            .query_all_by_label("Sweep must be greater than 0 and less than 360 degrees")
            .count()
            >= 1
    );
    settle_hover_snapshot(
        &mut harness,
        "workbench_three_point_arc_tab_sweep_validation_1040",
    );
}

use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchDimensionKind, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness, OsThreshold, SnapshotOptions,
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
        .with_options(
            SnapshotOptions::new()
                .output_path(snapshot_directory)
                // Baselines are recorded on one machine but compared on
                // several. Software rasterisers disagree with a GPU on a
                // handful of antialiased pixels; the measured worst case
                // across the whole suite is 52 of ~1,024,000. Allow a few
                // hundred, which is orders of magnitude below any real
                // layout change and still catches one.
                .failed_pixel_count_threshold(OsThreshold::new(0).linux(400).windows(400)),
        )
        .wgpu()
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

fn choose_variant(harness: &mut Harness<'static, KernelLabApp>, chooser: &str, variant: &str) {
    click_button(harness, chooser);
    click_button(harness, variant);
    assert!(harness.query_all_by_label(variant).count() >= 2);
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
    harness.step();
    harness.step();
}

fn replace_tool_input(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
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

fn assert_idle_sketch_rail(harness: &Harness<'static, KernelLabApp>) {
    assert!(!harness.state().operation_confirmation_pending());
    for label in ["Finish sketch", "Exit sketch"] {
        let node = harness.get_by_role_and_label(Role::Button, label);
        assert!(!node.accesskit_node().is_disabled());
        let rect = node.rect();
        assert_eq!(rect.width(), rect.height(), "{label} should remain square");
        assert!((24.0..=30.0).contains(&rect.width()), "{label}: {rect:?}");
    }
}

fn settle_snapshot(harness: &mut Harness<'static, KernelLabApp>, name: &str) {
    harness.remove_cursor();
    for _ in 0..3 {
        harness.step();
    }
    harness.snapshot(name);
}

fn settle_live_hover_snapshot(harness: &mut Harness<'static, KernelLabApp>, name: &str) {
    // The third pointer position is one of the three-point arc inputs. Keep it
    // present while settling so the exact derived R/SWEEP overlay remains live.
    for _ in 0..3 {
        harness.step();
    }
    harness.snapshot(name);
}

#[test]
fn polygon_variants_committed_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Outer-diameter polygon");
    replace_tool_input(&mut harness, "Sides", "6");
    replace_tool_input(&mut harness, "Outer diameter", "2.75");
    replace_tool_input(&mut harness, "Rotation", "30");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 0.25));
    click_sketch_point(&mut harness, SketchPoint::new(-0.625, 0.25));
    // The polygon commits as its radius click lands.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 6);
    assert_eq!(harness.state().sketch_revision(), 1);

    // Clear the last committed side selection so its line readouts do not
    // overlap the next compound primitive's staged across-flats controls.
    click_button(&mut harness, "Select sketch geometry");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, -3.0));

    choose_variant(
        &mut harness,
        "Choose polygon type; current default: Outer-diameter polygon.",
        "Inner-diameter polygon",
    );
    replace_tool_input(&mut harness, "Sides", "5");
    replace_tool_input(&mut harness, "Inner diameter", "2.25");
    replace_tool_input(&mut harness, "Rotation", "18");
    click_sketch_point(&mut harness, SketchPoint::new(1.75, 0.25));
    click_sketch_point(&mut harness, SketchPoint::new(2.875, 0.25));

    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 11);
    assert_eq!(harness.state().sketch_revision(), 2);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::ClosedRegions {
            regions: 2,
            loops: 2,
            holes: 0,
            analytic: false,
        }
    );
    for (label, value) in [
        ("Sides", "5"),
        ("Inner diameter", "2.25"),
        ("Rotation", "18"),
    ] {
        let input = harness.get_by_role_and_label(Role::TextInput, label);
        assert_eq!(input.value().as_deref(), Some(value));
        assert!(!input.accesskit_node().is_disabled());
    }
    assert_idle_sketch_rail(&harness);
    settle_snapshot(&mut harness, "workbench_polygon_variants_committed_1040");
}

#[test]
fn analytic_slot_committed_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Two-point centre-to-centre slot");
    replace_tool_input(&mut harness, "Centre distance", "3.25");
    replace_tool_input(&mut harness, "Width", "1.25");
    replace_tool_input(&mut harness, "Angle", "20");
    for point in [
        SketchPoint::new(-1.625, 0.0),
        SketchPoint::new(1.625, 0.0),
        SketchPoint::new(0.0, 0.625),
    ] {
        click_sketch_point(&mut harness, point);
    }

    // The slot commits as its width click lands.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 1);
    for label in ["Centre distance", "Width", "Angle"] {
        assert!(
            !harness
                .get_by_role_and_label(Role::TextInput, label)
                .accesskit_node()
                .is_disabled()
        );
    }
    assert_idle_sketch_rail(&harness);
    settle_snapshot(&mut harness, "workbench_analytic_slot_committed_1040");
}

#[test]
fn three_point_arc_live_measurement_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    choose_variant(
        &mut harness,
        "Choose arc type; current default: Centre-start-end arc.",
        "Three-point arc",
    );

    for point in [
        SketchPoint::new(-2.25, -0.75),
        SketchPoint::new(2.25, -0.75),
    ] {
        click_sketch_point(&mut harness, point);
    }
    hover_sketch_point(&mut harness, SketchPoint::new(0.0, 1.75));

    assert!(harness.state().pending_operation_label().is_none());
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 0);
    let readouts = harness.state().sketch_dimension_readouts();
    assert_eq!(readouts.len(), 2);
    assert_eq!(readouts[0].kind, SketchDimensionKind::Radius);
    assert_eq!(readouts[1].kind, SketchDimensionKind::SweepDegrees);
    assert!(
        readouts
            .iter()
            .all(|readout| readout.value.is_finite() && readout.value > 0.0)
    );
    assert!(!readouts[0].editable, "derived radius stays read-only");
    assert!(readouts[1].editable, "Tab owns the arc sweep field");
    assert!(harness.query_all_by_label("Arc radius").count() >= 2);
    assert!(harness.query_all_by_label("Arc sweep").count() >= 2);
    settle_live_hover_snapshot(&mut harness, "workbench_three_point_arc_live_1040");
}

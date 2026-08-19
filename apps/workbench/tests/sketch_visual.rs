use artificer_geometry::ProfileWinding;
use artificer_kernel::FaceRole;
use artificer_workbench::{
    ExtrusionMode, KernelLabApp, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchDimensionKind, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness, OsThreshold, SnapshotOptions,
    kittest::{NodeT as _, Queryable as _},
};

const CONFIRM_OPERATION: &str = "Confirm operation";
const IMAGE_WIDTH: usize = 1280;

fn harness() -> Harness<'static, KernelLabApp> {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    Harness::builder()
        .with_size([1280.0, 800.0])
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

fn minimum_harness() -> Harness<'static, KernelLabApp> {
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
    harness.event(egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
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

fn press_enter(harness: &mut Harness<'static, KernelLabApp>) {
    harness.key_down(egui::Key::Enter);
    harness.step();
    harness.key_up(egui::Key::Enter);
    harness.step();
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    // Let egui's deterministic panel transition reach its fixed workbench
    // geometry before measuring layout or pixels.
    for _ in 0..18 {
        harness.step();
    }
    assert!(harness.query_by_label("XY · ORTHOGRAPHIC").is_some());
}

fn enter_positive_z_face_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "Positive Z face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::PositiveZ)
    );
    click_button(harness, "Sketch on selected face");
    for _ in 0..18 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(harness.state().sketch_is_face_supported());
    assert!(harness.state().sketch_support_label().starts_with("Face #"));
    let (triangles, edges) = harness
        .state()
        .face_sketch_context_counts()
        .expect("face sketch should retain a projected committed-body context");
    assert!(triangles >= 2);
    assert!(edges >= 4);
    assert!(
        harness
            .query_by_label("Authoritative face-local frame · reference boundary")
            .is_some()
    );
}

fn canvas_point(harness: &Harness<'static, KernelLabApp>, offset: egui::Vec2) -> egui::Pos2 {
    harness.get_by_label("Sketch viewport").rect().center() + offset
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
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

fn draw_live_exact_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Two-point rectangle");
    let origin = canvas_point(harness, egui::Vec2::ZERO);
    click_at(harness, origin);
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle width", "4");
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle height", "2");
    assert!((dimension_value(harness, SketchDimensionKind::Width) - 4.0).abs() <= 1.0e-12);
    assert!((dimension_value(harness, SketchDimensionKind::Height) - 2.0).abs() <= 1.0e-12);
}

fn finish_exact_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    draw_live_exact_rectangle(harness);
    press_enter(harness);
    // Strokes commit as they are drawn, and finishing is one action.
    assert_eq!(harness.state().pending_operation_label(), None);
    click_button(harness, "Finish sketch");
    assert!(harness.state().sketch_finished());
}

fn commit_circle(
    harness: &mut Harness<'static, KernelLabApp>,
    center: SketchPoint,
    rim: SketchPoint,
) {
    click_button(harness, "Centre-point circle");
    let center = canvas_sketch_point(harness, center);
    let rim = canvas_sketch_point(harness, rim);
    click_at(harness, center);
    click_at(harness, rim);
    // The completed circle commits itself.
    assert_eq!(harness.state().pending_operation_label(), None);
}

fn finish_one_by_one_face_rectangle(harness: &mut Harness<'static, KernelLabApp>) {
    finish_centered_face_rectangle(harness, "1", "1");
}

fn commit_centered_face_rectangle(
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

    press_enter(harness);
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert!(!harness.state().sketch_finished());
}

fn finish_centered_face_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    width: &str,
    height: &str,
) {
    commit_centered_face_rectangle(harness, width, height);
    click_button(harness, "Finish sketch");
    press_enter(harness);
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn finish_offset_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    lower_left: SketchPoint,
    width: &str,
    height: &str,
) {
    click_button(harness, "Two-point rectangle");
    click_at(harness, canvas_sketch_point(harness, lower_left));
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle width", width);
    press_key(harness, egui::Key::Tab);
    type_active_dimension(harness, "Rectangle height", height);
    press_enter(harness);
    assert_eq!(harness.state().pending_operation_label(), None);
    click_button(harness, "Finish sketch");
    press_enter(harness);
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn select_latest_feature_end(harness: &mut Harness<'static, KernelLabApp>) {
    {
        let node = harness
            .query_all_by_role_and_label(Role::Button, "Feature end face")
            .last()
            .expect("latest generated rectangular end or floor face");
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

fn differing_rgba_pixels_in_rect(
    left: &[u8],
    right: &[u8],
    image_width: usize,
    rect: egui::Rect,
) -> usize {
    assert_eq!(left.len(), right.len());
    let image_height = left.len() / 4 / image_width;
    let min_x = rect.min.x.floor().max(0.0) as usize;
    let min_y = rect.min.y.floor().max(0.0) as usize;
    let max_x = rect.max.x.ceil().min(image_width as f32) as usize;
    let max_y = rect.max.y.ceil().min(image_height as f32) as usize;
    let mut differing = 0;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let index = (y * image_width + x) * 4;
            if left[index..index + 4] != right[index..index + 4] {
                differing += 1;
            }
        }
    }
    differing
}

#[test]
fn workbench_live_rectangle_dimensions_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_live_exact_rectangle(&mut harness);

    assert!(!harness.state().operation_confirmation_pending());
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle height")
            .is_some()
    );
    // The live readouts have one home, the dimension widgets on the canvas;
    // the palette that restated them in millimetres is gone.
    let readouts = harness.state().sketch_dimension_readouts();
    let value_of = |kind: SketchDimensionKind| {
        readouts
            .iter()
            .find(|readout| readout.kind == kind)
            .unwrap_or_else(|| panic!("missing live readout {kind:?}"))
            .value
    };
    assert!((value_of(SketchDimensionKind::Width) - 4.0).abs() <= 1.0e-12);
    assert!((value_of(SketchDimensionKind::Height) - 2.0).abs() <= 1.0e-12);

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_live_rectangle_dimensions");
}

#[test]
fn workbench_compact_sketch_toolbar_at_minimum_size_snapshot() {
    let mut harness = minimum_harness();
    enter_xy_sketch(&mut harness);

    for label in [
        "Single line",
        "Two-point rectangle",
        "Centre-point circle",
        "Outer-diameter polygon",
        "Two-point centre-to-centre slot",
        "Trim curve span",
        "2D fillet",
        "Equal-distance chamfer",
        "Rectangular sketch pattern",
    ] {
        // Every family keeps a whole, unclipped tile at the minimum window;
        // the tile geometry itself is held by `sketch_compact_toolbar_ui`.
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a visible hit target");
        assert!(
            rect.height() >= 24.0,
            "{label} lost its accessible hit target: {rect:?}"
        );
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0,
            "{label} escaped the supported window: {rect:?}"
        );
    }

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_compact_sketch_toolbar_1040");
}

#[test]
fn workbench_two_distance_chamfer_palette_snapshot() {
    let mut harness = minimum_harness();
    enter_xy_sketch(&mut harness);
    click_button(
        &mut harness,
        "Choose chamfer type; current default: Equal-distance chamfer.",
    );
    click_button(&mut harness, "Two-distance chamfer");
    replace_tool_input(&mut harness, "First distance", "0.5");
    replace_tool_input(&mut harness, "Second distance", "1.25");

    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "First distance")
            .value()
            .as_deref(),
        Some("0.5")
    );
    assert_eq!(
        harness
            .get_by_role_and_label(Role::TextInput, "Second distance")
            .value()
            .as_deref(),
        Some("1.25")
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_two_distance_chamfer_palette_1040");
}

#[test]
fn workbench_maximum_rectangular_pattern_committed_snapshot() {
    let mut harness = minimum_harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Single line");
    let center = canvas_point(&harness, egui::Vec2::ZERO);
    click_at(&mut harness, center + egui::vec2(-24.0, 0.0));
    click_at(&mut harness, center + egui::vec2(24.0, 0.0));
    assert_eq!(harness.state().pending_operation_label(), None);

    click_button(&mut harness, "Rectangular sketch pattern");
    replace_tool_input(&mut harness, "First count", "16");
    replace_tool_input(&mut harness, "First spacing", "0.25");
    harness
        .get_by_role_and_label(Role::CheckBox, "Second direction")
        .click();
    harness.run();
    replace_tool_input(&mut harness, "Second count", "16");
    replace_tool_input(&mut harness, "Second spacing", "0.25");
    click_at(&mut harness, center + egui::vec2(80.0, 0.0));

    // The bounded 16x16 placement is a complete stroke, which commits.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_entity_count(), 256);
    assert_eq!(harness.state().sketch_revision(), 2);

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_maximum_rectangular_pattern_committed_1040");
}

#[test]
fn workbench_analytic_annulus_extrusion_preview_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    commit_circle(
        &mut harness,
        SketchPoint::new(0.0, 0.0),
        SketchPoint::new(3.0, 0.0),
    );
    commit_circle(
        &mut harness,
        SketchPoint::new(0.0, 0.0),
        SketchPoint::new(1.25, 0.0),
    );
    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::ClosedRegions {
            regions: 1,
            loops: 2,
            holes: 1,
            analytic: true,
        }
    ));

    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    assert!(harness.query_by_label("EXTRUSION PREVIEW").is_some());
    assert!(harness.state().operation_confirmation_pending());

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_analytic_annulus_extrusion_preview");

    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed analytic annulus");
    assert!((measures.volume - 29.75 * std::f64::consts::PI).abs() <= 1.0e-8);
    assert!(
        harness
            .state()
            .displayed_topology_counts()
            .is_some_and(|counts| { counts.solids == 1 && counts.faces >= 4 && counts.edges >= 8 })
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_analytic_annulus_extrusion_committed");

    // The banding tripwire (ADR 0026, P1). Flat per-triangle shading shows as
    // vertical bands across a cylinder wall, and at this magnification each
    // band is tens of pixels wide — far above the portability threshold, so a
    // regression to per-facet shading cannot pass this baseline quietly.
    let viewport_center = harness.get_by_label("Model viewport").rect().center();
    harness.hover_at(viewport_center);
    harness.step();
    for _ in 0..3 {
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 120.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }
    harness.remove_cursor();
    // Hovering the viewport leaves a tooltip animating, so settle a fixed
    // number of frames rather than waiting for a quiescent one.
    harness.run_steps(6);
    harness.snapshot("workbench_curved_wall_close_up");
}

#[test]
fn workbench_selected_face_sketch_support_snapshot() {
    let mut harness = harness();
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();
    enter_positive_z_face_sketch(&mut harness);

    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts
    );
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_selected_face_sketch_support");
}

#[test]
fn workbench_active_face_sketch_extrude_ready_snapshot() {
    let mut harness = harness();
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();
    enter_positive_z_face_sketch(&mut harness);
    commit_centered_face_rectangle(&mut harness, "1", "1");

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(!harness.state().sketch_finished());
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "a certified active face profile should render an enabled Extrude command"
    );
    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_active_face_sketch_extrude_ready");
}

#[test]
fn workbench_committed_face_sketch_overlay_snapshot() {
    let mut harness = harness();
    enter_positive_z_face_sketch(&mut harness);
    finish_centered_face_rectangle(&mut harness, "1", "1");

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    click_button(&mut harness, "Browser");
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Hide Sketch 1")
            .is_some(),
        "the committed sketch must remain visible and hideable in the Browser"
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "the committed closed face sketch must remain separately extrudable"
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_committed_face_sketch_overlay");
}

struct FaceFeatureSnapshotCase<'a> {
    mode: ExtrusionMode,
    mode_label: &'a str,
    staged_label: &'a str,
    committed_label: &'a str,
    expected_volume: f64,
    expected_feature: &'a str,
    preview_snapshot: &'a str,
    committed_snapshot: &'a str,
}

fn snapshot_selected_face_feature(case: FaceFeatureSnapshotCase<'_>) {
    let FaceFeatureSnapshotCase {
        mode,
        mode_label,
        staged_label,
        committed_label,
        expected_volume,
        expected_feature,
        preview_snapshot,
        committed_snapshot,
    } = case;
    let mut harness = harness();
    let original_snapshot = harness.state().displayed_snapshot_id();
    let original_attempts = harness.state().transaction_attempt_count();
    enter_positive_z_face_sketch(&mut harness);
    finish_one_by_one_face_rectangle(&mut harness);

    click_button(&mut harness, mode_label);
    assert_eq!(harness.state().extrusion_mode(), mode);
    if mode == ExtrusionMode::Cut {
        set_extrusion_distance(&mut harness, "-1");
    }
    harness.remove_cursor();
    harness.run();
    let viewport_rect = harness.get_by_label("Model viewport").rect();
    let before_preview = harness
        .render()
        .expect("base body before face-feature preview should render");

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
    assert!(harness.query_by_label(staged_label).is_some());

    harness.remove_cursor();
    harness.run();
    assert_eq!(
        harness.get_by_label("Model viewport").rect(),
        viewport_rect,
        "staging a face feature must preserve the exact model viewport"
    );
    let preview = harness
        .render()
        .expect("selected-face feature preview should render");
    let changed_in_viewport = differing_rgba_pixels_in_rect(
        before_preview.as_raw(),
        preview.as_raw(),
        IMAGE_WIDTH,
        viewport_rect,
    );
    assert!(
        changed_in_viewport > 500,
        "the {mode_label} preview changed only {changed_in_viewport} viewport pixels"
    );
    harness.snapshot(preview_snapshot);

    click_button(&mut harness, CONFIRM_OPERATION);
    assert!(!harness.state().operation_confirmation_pending());
    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(
        harness.state().transaction_attempt_count(),
        original_attempts + 1
    );
    assert_eq!(harness.state().last_error_code(), None);
    assert!(harness.query_by_label(committed_label).is_some());
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

    harness.remove_cursor();
    harness.run();
    harness.snapshot(committed_snapshot);
}

#[test]
fn workbench_selected_face_add_snapshots() {
    snapshot_selected_face_feature(FaceFeatureSnapshotCase {
        mode: ExtrusionMode::Add,
        mode_label: "Add",
        staged_label: "Add preview staged · confirm with Enter or the green tick",
        committed_label: "Added extrusion committed",
        expected_volume: 25.0,
        expected_feature: "Add 1",
        preview_snapshot: "workbench_selected_face_add_preview",
        committed_snapshot: "workbench_selected_face_add_committed",
    });
}

#[test]
fn workbench_selected_face_cut_snapshots() {
    snapshot_selected_face_feature(FaceFeatureSnapshotCase {
        mode: ExtrusionMode::Cut,
        mode_label: "Cut",
        staged_label: "Cut preview staged · confirm with Enter or the green tick",
        committed_label: "Cut extrusion committed",
        expected_volume: 23.0,
        expected_feature: "Cut 1",
        preview_snapshot: "workbench_selected_face_cut_preview",
        committed_snapshot: "workbench_selected_face_cut_committed",
    });
}

#[test]
fn workbench_selected_face_push_pull_preview_snapshot() {
    let mut harness = harness();
    harness.run();
    click_button(&mut harness, "Positive Z face");
    click_button(&mut harness, "Extrude");
    set_extrusion_distance(&mut harness, "-1");

    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Push/pull selected face")
    );
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Cut);
    assert!(harness.query_by_label("PUSH/PULL PREVIEW").is_some());
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_selected_face_push_pull_preview");
}

#[test]
fn workbench_committed_xy_extrusion_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    let original_snapshot = harness.state().displayed_snapshot_id();
    finish_exact_rectangle(&mut harness);
    set_extrusion_distance(&mut harness, "3");
    click_button(&mut harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude finished sketch")
    );
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_ne!(harness.state().displayed_snapshot_id(), original_snapshot);
    assert_eq!(harness.state().extruded_sketch_revision(), Some(1));
    let measures = harness
        .state()
        .displayed_measures()
        .expect("extrusion measures");
    assert!((measures.volume - 24.0).abs() <= 1.0e-9);
    assert!((measures.surface_area - 52.0).abs() <= 1.0e-9);
    let centroid = measures.centroid.expect("extrusion centroid");
    assert!((centroid.x - 2.0).abs() <= 1.0e-9);
    assert!((centroid.y - 1.0).abs() <= 1.0e-9);
    assert!((centroid.z - 1.5).abs() <= 1.0e-9);

    click_button(&mut harness, "Extrusion top face");
    assert_eq!(
        harness.state().selected_face_role(),
        Some(FaceRole::ExtrusionTop)
    );
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_committed_xy_extrusion");
}

#[test]
fn workbench_two_visible_bodies_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    finish_exact_rectangle(&mut harness);
    set_extrusion_distance(&mut harness, "2");
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    click_button(&mut harness, "New sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    finish_offset_rectangle(&mut harness, SketchPoint::new(3.0, -0.5), "2", "1");
    set_extrusion_distance(&mut harness, "1");
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().body_count(), 2);
    assert!(harness.state().body_visible(0));
    assert!(harness.state().body_visible(1));
    let (document_center, document_radius) = harness.state().view_frame();
    assert!((document_center.x - 2.5).abs() <= 1.0e-12);
    assert!((document_center.y - 0.75).abs() <= 1.0e-12);
    assert!((document_center.z - 1.0).abs() <= 1.0e-12);
    assert!((document_radius - 8.8125_f64.sqrt()).abs() <= 1.0e-12);
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

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_two_visible_bodies");
}

#[test]
fn workbench_repeated_face_add_cut_add_committed_snapshot() {
    let mut harness = harness();
    enter_positive_z_face_sketch(&mut harness);
    finish_one_by_one_face_rectangle(&mut harness);
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    select_latest_feature_end(&mut harness);
    click_button(&mut harness, "Sketch on selected face");
    finish_centered_face_rectangle(&mut harness, "0.5", "0.5");
    click_button(&mut harness, "Cut");
    set_extrusion_distance(&mut harness, "-0.5");
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    select_latest_feature_end(&mut harness);
    click_button(&mut harness, "Sketch on selected face");
    click_button(&mut harness, "Snap");
    finish_centered_face_rectangle(&mut harness, "0.25", "0.25");
    set_extrusion_distance(&mut harness, "0.25");
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, CONFIRM_OPERATION);

    let counts = harness
        .state()
        .displayed_topology_counts()
        .expect("committed repeated-feature topology");
    assert_eq!((counts.vertices, counts.edges, counts.faces), (32, 48, 21));
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed repeated-feature measures");
    assert!((measures.volume - 24.890625).abs() <= 1.0e-9);
    assert!((measures.surface_area - 57.25).abs() <= 1.0e-9);
    assert_eq!(
        harness.state().feature_timeline_entries(),
        [
            "Origin",
            "Base body",
            "Sketch 1 · r1",
            "Add 1",
            "Sketch 2 · r1",
            "Cut 1",
            "Sketch 3 · r1",
            "Add 2",
        ]
        .map(str::to_owned)
    );

    select_latest_feature_end(&mut harness);
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_repeated_face_add_cut_add_committed");
}

#[test]
fn workbench_sketch_xy_rectangle_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    // The Finish/Exit rail is always present while sketching, so switching
    // tools and committing strokes must never reflow the canvas.
    click_button(&mut harness, "Two-point rectangle");
    harness.remove_cursor();
    harness.run();
    let empty = harness.render().expect("empty sketch frame should render");
    let stable_viewport = harness.get_by_label("Sketch viewport").rect();

    let first = canvas_point(&harness, egui::vec2(-112.0, 70.0));
    let opposite = canvas_point(&harness, egui::vec2(112.0, -70.0));
    let profile_interior = egui::Rect::from_two_pos(first, opposite).shrink(24.0);
    click_at(&mut harness, first);
    click_at(&mut harness, opposite);
    let committed_viewport = harness.get_by_label("Sketch viewport").rect();

    assert_eq!(
        committed_viewport, stable_viewport,
        "committing a rectangle must preserve the exact sketch viewport"
    );
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed {
            winding: ProfileWinding::CounterClockwise,
        }
    );
    assert_eq!(
        harness.get_by_label("Sketch viewport").rect(),
        committed_viewport,
        "the certified readout must preserve the exact sketch viewport"
    );

    harness.remove_cursor();
    harness.run();
    let closed = harness
        .render()
        .expect("certified rectangle frame should render");
    let changed = differing_rgba_pixels_in_rect(
        empty.as_raw(),
        closed.as_raw(),
        IMAGE_WIDTH,
        profile_interior,
    );
    assert!(
        changed > 8_000,
        "the certified profile fill changed only {changed} interior pixels"
    );
    harness.snapshot("workbench_sketch_xy_rectangle");
}

#[test]
fn workbench_sketch_self_intersection_snapshot() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    let stable_viewport = harness.get_by_label("Sketch viewport").rect();
    click_button(&mut harness, "Single line");

    let offsets = [
        egui::vec2(-112.0, -70.0),
        egui::vec2(112.0, 70.0),
        egui::vec2(-112.0, 70.0),
        egui::vec2(112.0, -70.0),
        egui::vec2(-112.0, -70.0),
    ];
    for edge in offsets.windows(2) {
        let start = canvas_point(&harness, edge[0]);
        let end = canvas_point(&harness, edge[1]);
        click_at(&mut harness, start);
        click_at(&mut harness, end);
        press_enter(&mut harness);
    }

    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::SelfIntersecting
    );
    assert_eq!(
        harness.get_by_label("Sketch viewport").rect(),
        stable_viewport
    );
    click_button(&mut harness, "Select sketch geometry");
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::SelfIntersecting
    );
    assert_eq!(
        harness.get_by_label("Sketch viewport").rect(),
        stable_viewport,
        "profile diagnostics must preserve the exact sketch viewport"
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_sketch_self_intersection");
}

fn blank_harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused_blank(creation_context))
}

/// Pixels in `rect` within a small distance of `target`.
fn pixels_near_colour(
    image: &[u8],
    image_width: usize,
    rect: egui::Rect,
    target: [u8; 3],
) -> usize {
    let image_height = image.len() / 4 / image_width;
    let min_x = rect.min.x.floor().max(0.0) as usize;
    let min_y = rect.min.y.floor().max(0.0) as usize;
    let max_x = rect.max.x.ceil().min(image_width as f32) as usize;
    let max_y = rect.max.y.ceil().min(image_height as f32) as usize;
    let mut count = 0;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let index = (y * image_width + x) * 4;
            let close = (0..3).all(|channel| {
                (i32::from(image[index + channel]) - i32::from(target[channel])).abs() <= 24
            });
            if close {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn workbench_blank_document_keeps_its_first_committed_sketch_visible() {
    // Production starts blank: reference planes, no solid. Committing the
    // first sketch retires the planes, and with no body either the renderer
    // used to decide there was nothing to look at and paint a placeholder
    // over the sketch it had just been handed.
    let mut harness = blank_harness();
    enter_xy_sketch(&mut harness);
    commit_circle(
        &mut harness,
        SketchPoint::new(0.0, 0.0),
        SketchPoint::new(6.0, 0.0),
    );
    click_button(&mut harness, "Finish sketch");
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().displayed_snapshot_id(), None);
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    // The sketch is what there is; Frame must find it, not fall back to a
    // camera radius sized for planes that are no longer shown.
    click_button(&mut harness, "Frame all visible bodies");
    harness.remove_cursor();
    harness.run();

    let image = harness.render().expect("model frame should render");
    let viewport = harness.get_by_label("Model viewport").rect();
    // The unconsumed committed-sketch stroke colour.
    let amber = pixels_near_colour(image.as_raw(), IMAGE_WIDTH, viewport, [206, 128, 16]);
    assert!(
        amber > 200,
        "the committed circle must be drawn in the model viewport ({amber} amber pixels)"
    );
}

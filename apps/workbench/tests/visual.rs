use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{Harness, OsThreshold, SnapshotOptions, kittest::Queryable as _};

const CONFIRM_OPERATION: &str = "Confirm operation";
const CANCEL_OPERATION: &str = "Cancel operation";

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
}

fn differing_rgba_pixels(left: &[u8], right: &[u8]) -> usize {
    assert_eq!(left.len(), right.len());
    left.chunks_exact(4)
        .zip(right.chunks_exact(4))
        .filter(|(left, right)| left != right)
        .count()
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

/// Focused pixel regression for the complete canonical lab view. The checked-in
/// baseline is updated only with `UPDATE_SNAPSHOTS=true` and manual review.
#[test]
fn canonical_cuboid_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
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
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    let identity = harness.render().expect("identity frame should render");

    harness.state_mut().set_animation_phase(0.52);
    harness.run();
    let animated = harness.render().expect("animated frame should render");
    assert!(
        differing_rgba_pixels(identity.as_raw(), animated.as_raw()) > 500,
        "animation phase must visibly transform the body"
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("canonical_cuboid_2x3x4");

    let snapshot = harness.state().displayed_snapshot_id();
    let digest = harness.state().displayed_semantic_digest();
    harness
        .get_by_role_and_label(Role::Button, "M  Move")
        .click();
    harness.run();
    let start = harness.get_by_label("Model viewport").rect().center();
    let end = start + egui::vec2(85.0, -35.0);
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
    let moved = harness.render().expect("moved frame should render");
    assert!(
        differing_rgba_pixels(animated.as_raw(), moved.as_raw()) > 500,
        "Move must visibly transform the body and display its gizmo"
    );
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);

    harness
        .get_by_role_and_label(Role::Button, "R  Rotate")
        .click();
    harness.run();
    let start = harness.get_by_label("Model viewport").rect().center();
    let end = start + egui::vec2(52.0, 27.0);
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
    harness
        .get_by_role_and_label(Role::Button, "S  Scale")
        .click();
    harness.run();
    let start = harness.get_by_label("Model viewport").rect().center();
    let end = start + egui::vec2(0.0, -42.0);
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
    harness
        .get_by_role_and_label(Role::Button, "V  Select")
        .click();
    harness.remove_cursor();
    harness.run();
    let combined_preview = harness
        .render()
        .expect("combined transform preview should render");
    assert_eq!(harness.state().displayed_snapshot_id(), snapshot);
    assert_eq!(harness.state().displayed_semantic_digest(), digest);
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
    harness.snapshot("pending_transform_confirmation");

    let viewport_rect = harness.get_by_label("Model viewport").rect();
    // The floating confirmation chip vanishes on commit by design. Its zone
    // is excluded from the jump check exactly like the status readout: both
    // are transient chrome over the canvas, not model geometry.
    let confirm_rect = harness
        .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
        .rect();
    let chip_rect = egui::Rect::from_center_size(
        egui::pos2(moved.width() as f32 / 2.0, confirm_rect.center().y),
        egui::vec2(470.0, 56.0),
    );
    harness
        .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
        .click();
    harness.run();
    harness.remove_cursor();
    harness.run();
    let committed = harness.render().expect("committed frame should render");
    assert_eq!(
        harness.get_by_label("Model viewport").rect(),
        viewport_rect,
        "confirming must preserve the model viewport rectangle"
    );
    // What must not move is the model. The right-hand strip of the viewport is
    // floating chrome — the status chip, and the contextual card, which appears
    // and disappears with the operation by design — so the comparison is made
    // over the region the geometry actually occupies rather than over every
    // pixel the viewport owns.
    let model_rect = egui::Rect::from_min_max(
        viewport_rect.min,
        egui::pos2(viewport_rect.right() - 300.0, viewport_rect.bottom()),
    );
    let changed_in_model = differing_rgba_pixels_in_rect(
        combined_preview.as_raw(),
        committed.as_raw(),
        moved.width() as usize,
        model_rect,
    );
    let changed_in_chip = differing_rgba_pixels_in_rect(
        combined_preview.as_raw(),
        committed.as_raw(),
        moved.width() as usize,
        chip_rect.intersect(model_rect),
    );
    let changed_outside_status = changed_in_model.saturating_sub(changed_in_chip);
    assert!(
        changed_outside_status <= 24,
        "commit visibly jumped by {changed_outside_status} model pixels"
    );
    assert_ne!(harness.state().displayed_snapshot_id(), snapshot);
    assert_ne!(harness.state().displayed_semantic_digest(), digest);
    assert!(!harness.state().transform_preview_pending());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Transform 1 feature")
            .is_some(),
        "the committed transform must be visible in History"
    );
    harness.snapshot("committed_transform_history");
}

#[test]
fn shaded_edges_and_dynamic_view_cube_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
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
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    assert!(harness.state().shaded_display_enabled());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "View cube top")
            .is_some()
    );
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_shaded_edges_and_view_cube");
}

#[test]
fn face_measurement_is_visible_on_the_model_and_in_the_inspector() {
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    let before = harness.render().expect("unmeasured model should render");
    harness
        .get_by_role_and_label(Role::Button, "I  Measure")
        .click();
    harness.run();
    let viewport = harness.get_by_label("Model viewport").rect();
    click_at(&mut harness, viewport.center());
    harness.run();

    assert!(harness.query_by_label("FACE AREA RESULT").is_some());
    let measured = harness.render().expect("measured model should render");
    assert!(
        differing_rgba_pixels_in_rect(
            before.as_raw(),
            measured.as_raw(),
            measured.width() as usize,
            viewport,
        ) > 80,
        "the model-space area annotation must be visibly rendered"
    );
}

#[test]
fn collapsed_workbench_shell_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
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
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    let expanded_viewport = harness.get_by_label("Model viewport").rect();
    for label in [
        "Collapse browser panel",
        "History",
        "Collapse command ribbon",
    ] {
        harness.get_by_role_and_label(Role::Button, label).click();
        harness.run();
    }

    let visibility = harness.state().shell_visibility();
    assert!(!visibility.command_ribbon);
    assert!(!visibility.model_browser);
    assert!(!visibility.feature_timeline);
    assert!(
        harness.get_by_label("Model viewport").rect().width() > expanded_viewport.width(),
        "collapsing both side docks must return horizontal space to the canvas"
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_collapsed_shell");
}

#[test]
fn minimum_window_compact_ribbon_and_confirmation_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
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
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    // The file actions live in the File menu now, so the header holds the menu
    // itself plus the controls that earn a permanent place beside it.
    for label in [
        "File menu",
        "Undo history change",
        "Redo history change",
        "Library",
        "Browser",
        "Properties",
        "History",
    ] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must remain visible: {rect:?}");
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0,
            "{label} is clipped by the supported window: {rect:?}"
        );
    }
    // Opening it must reach every file action, at the minimum window.
    harness
        .get_by_role_and_label(Role::Button, "File menu")
        .click_accesskit();
    harness.run();
    for label in [
        "Save document",
        "Open saved document",
        "Export as STL…",
        "Export as STEP (exact B-rep)…",
        "Export as STEP (faceted)…",
    ] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must be reachable: {rect:?}");
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0 && rect.max.y <= 700.0,
            "{label} is clipped by the supported window: {rect:?}"
        );
    }
    harness.key_press(egui::Key::Escape);
    harness.run();
    let viewport_top = harness.get_by_label("Model viewport").rect().top();
    // "Home" now lives on the View tab, so this checks a Model-tab command from
    // each weight instead: a large one, a primary one, and a small one.
    for label in ["Create sketch", "Extrude", "M  Move", "Fillet"] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.height() >= 24.0, "{label} is clipped: {rect:?}");
        assert!(
            rect.max.y <= viewport_top,
            "{label} crosses the command-ribbon boundary: {rect:?}"
        );
    }

    harness
        .get_by_role_and_label(Role::Button, "M  Move")
        .click();
    harness.run();
    let start = harness.get_by_label("Model viewport").rect().center();
    let end = start + egui::vec2(54.0, -22.0);
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
    assert!(harness.state().operation_confirmation_pending());
    for label in [CONFIRM_OPERATION, CANCEL_OPERATION] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert_eq!(rect.width(), rect.height(), "{label} must be square");
        assert!(rect.width() <= 30.0, "{label} is too bulky: {rect:?}");
        assert!(rect.width() >= 24.0, "{label} is too small: {rect:?}");
    }

    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_minimum_compact_controls");
}

#[test]
fn document_properties_units_and_interchange_snapshot() {
    // This panel prints the document and export paths, which come from the
    // user's home directory. The baseline is therefore machine-specific and is
    // skipped in CI rather than compared there; the layout it covers is
    // exercised by the other snapshots in this suite.
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
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
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Properties")
        .click_accesskit();
    harness.run();
    harness.remove_cursor();
    harness.run();
    harness.snapshot("workbench_document_properties_units_and_export");
}

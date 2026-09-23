use artificer_workbench::{KernelLabApp, part_library::PartInsertionEligibility};
use egui::accesskit::Role;
use egui_kittest::{Harness, OsThreshold, SnapshotOptions, kittest::Queryable as _};

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let position = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
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

#[test]
fn staged_parametric_part_library_snapshot() {
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
    click_button(&mut harness, "Library");
    let length = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    length.click();
    length.type_text("455");
    harness.run();
    assert_eq!(
        harness.state().part_library_eligibility(),
        PartInsertionEligibility::Ready
    );
    assert_eq!(harness.state().part_library_length_mm(), Some(455.0));
    click_button(&mut harness, "Add to current workspace");

    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Insert library component")
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Confirm operation")
            .is_some()
    );
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Cancel operation")
            .is_some()
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("part_library_staged_parametric_extrusion");
}

#[test]
fn committed_parametric_component_snapshot() {
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
    click_button(&mut harness, "Library");
    let length = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    length.click();
    length.type_text("80");
    harness.run();
    click_button(&mut harness, "Add to current workspace");
    click_button(&mut harness, "Confirm operation");
    click_button(&mut harness, "Library");

    assert_eq!(harness.state().component_instance_count(), 1);
    assert_eq!(harness.state().body_count(), 2);
    assert!((harness.state().displayed_measures().unwrap().volume - 32_000.0).abs() <= 1.0e-8);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "20 × 20 Aluminium Extrusion · component 1")
            .is_some()
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("part_library_committed_parametric_component");
}

/// The library as a person with a local library sees it: the part's picture
/// on the left of its row, drawn when the part was saved into the library,
/// with its version and rough size beside it.
#[test]
fn library_list_shows_picture_version_and_size_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let root = std::env::temp_dir().join(format!(
        "artificer-library-visual-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let catalog_root = root.clone();
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .with_options(
            SnapshotOptions::new()
                .output_path(snapshot_directory)
                .failed_pixel_count_threshold(OsThreshold::new(0).linux(400).windows(400)),
        )
        .wgpu()
        .build_eframe(move |creation_context| {
            KernelLabApp::new_paused_with_catalog_root(creation_context, catalog_root)
        });

    harness.run();
    assert!(harness.state().persistent_catalog_active());
    click_button(&mut harness, "Library");
    harness.get_by_role_and_label(Role::Image, "Picture of 20 × 20 Aluminium Extrusion");
    harness.remove_cursor();
    harness.run();
    harness.snapshot("part_library_list_picture_version_size");
    let _ = std::fs::remove_dir_all(&root);
}

fn click_point(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
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
    harness.run();
}

fn retype(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .click();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

/// A `length` variable at 50 mm, and a 4 × 2 rectangle extruded by it.
fn draw_a_bar_that_follows_length(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Parametric ribbon tab");
    click_button(harness, "New length variable");
    click_button(harness, "Confirm operation");
    retype(harness, "Variable name Length1", "length");
    harness.key_press(egui::Key::Enter);
    harness.run();
    retype(harness, "Variable value length", "50");
    harness.key_press(egui::Key::Enter);
    harness.run();
    click_button(harness, "Confirm operation");
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    click_button(harness, "Two-point rectangle");
    for point in [
        artificer_workbench::sketch::SketchPoint::new(-2.0, -1.0),
        artificer_workbench::sketch::SketchPoint::new(2.0, 1.0),
    ] {
        let position = harness
            .state()
            .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
        click_point(harness, position);
    }
    click_button(harness, "Extrude");
    retype(harness, "Extrusion distance expression", "length");
    harness.key_press(egui::Key::Tab);
    harness.run();
    click_button(harness, "Confirm operation");
}

/// The save window, and the library with a saved part under MY PARTS beside
/// the built-in one.
#[test]
fn saving_a_part_into_the_library_snapshots() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let root = std::env::temp_dir().join(format!(
        "artificer-library-save-visual-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let catalog_root = root.clone();
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .with_options(
            SnapshotOptions::new()
                .output_path(snapshot_directory)
                .failed_pixel_count_threshold(OsThreshold::new(0).linux(400).windows(400)),
        )
        .wgpu()
        .build_eframe(move |creation_context| {
            KernelLabApp::new_paused_with_catalog_root(creation_context, catalog_root)
        });
    harness.run();
    draw_a_bar_that_follows_length(&mut harness);

    click_button(&mut harness, "File menu");
    click_button(&mut harness, "Save to Part Library…");
    retype(&mut harness, "Part name", "Bar");
    retype(
        &mut harness,
        "Part description",
        "A 4 × 2 bar cut to length",
    );
    harness.remove_cursor();
    harness.run();
    harness.snapshot("part_library_save_window");

    click_button(&mut harness, "Save to library");
    assert!(harness.state().part_library_open());
    harness.remove_cursor();
    harness.run();
    harness.snapshot("part_library_with_a_saved_part");
    let _ = std::fs::remove_dir_all(&root);
}

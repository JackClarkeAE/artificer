use artificer_workbench::KernelLabApp;
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

fn insert_component(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Add to current workspace");
    click_button(harness, "Confirm operation");
}

#[test]
fn assembly_placement_and_joint_snapshots() {
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
    insert_component(&mut harness);
    insert_component(&mut harness);
    click_button(&mut harness, "Library");

    assert_eq!(harness.state().component_instance_count(), 2);
    assert_eq!(harness.state().component_poses().len(), 2);
    harness.remove_cursor();
    harness.run();
    harness.snapshot("assembly_two_placed_components");

    click_button(&mut harness, "M  Move");
    let start = harness.get_by_label("Model viewport").rect().center();
    let end = start + egui::vec2(90.0, -32.0);
    harness.drag_at(start);
    harness.step();
    harness.hover_at(end);
    harness.step();
    harness.drop_at(end);
    harness.step();
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Place component")
    );
    harness.remove_cursor();
    harness.run();
    harness.snapshot("assembly_component_placement_preview");

    click_button(&mut harness, "Cancel operation");
    click_button(&mut harness, "Properties");
    click_button(&mut harness, "Add revolute joint");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().assembly_joint_count(), 1);
    let at_rest = harness.render().expect("joint rest pose should render");
    harness.state_mut().set_animation_phase(0.52);
    harness.run();
    let animated = harness
        .render()
        .expect("joint animation phase should render");
    let differing_pixels = at_rest
        .as_raw()
        .chunks_exact(4)
        .zip(animated.as_raw().chunks_exact(4))
        .filter(|(left, right)| left != right)
        .count();
    assert!(
        differing_pixels > 500,
        "named revolute playback must visibly rotate the active occurrence"
    );
    harness.remove_cursor();
    harness.run();
    harness.snapshot("assembly_revolute_joint_committed");
}

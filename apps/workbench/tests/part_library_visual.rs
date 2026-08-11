use artificer_workbench::{KernelLabApp, part_library::PartInsertionEligibility};
use egui::accesskit::Role;
use egui_kittest::{Harness, SnapshotOptions, kittest::Queryable as _};

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
        .with_options(SnapshotOptions::new().output_path(snapshot_directory))
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    click_button(&mut harness, "Library");
    let length = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    length.click();
    length.type_text("455");
    harness.run();
    assert!(matches!(
        harness.state().part_library_eligibility(),
        PartInsertionEligibility::Ready {
            length_mm: 455.0,
            ..
        }
    ));
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
        .with_options(SnapshotOptions::new().output_path(snapshot_directory))
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
            .query_by_role_and_label(Role::Button, "◇  20 × 20 Aluminium Extrusion · component 1")
            .is_some()
    );

    harness.remove_cursor();
    harness.run();
    harness.snapshot("part_library_committed_parametric_component");
}

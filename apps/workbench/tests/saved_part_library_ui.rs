//! Saving a part into the Part Library and placing it, end to end, through
//! the real widgets: a part whose extrusion follows a `length` variable is
//! saved from the File menu, then placed several times at different lengths,
//! each its own component; the built-in extrusion is placed at several
//! lengths beside them; and the document keeps every placement when it is
//! opened again in an app that has no library at all.

use artificer_workbench::{KernelLabApp, sketch::SketchPoint};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};
use std::path::PathBuf;

const CONFIRM_OPERATION: &str = "Confirm operation";

struct TemporaryLibrary {
    path: PathBuf,
}

impl TemporaryLibrary {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "artificer-saved-part-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Self { path }
    }
}

impl Drop for TemporaryLibrary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn harness_with_library(root: PathBuf) -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(move |creation_context| {
            KernelLabApp::new_paused_with_catalog_root(creation_context, root)
        })
}

fn harness_without_library() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
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
    harness.run();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn replace_text(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
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
    click_button(harness, CONFIRM_OPERATION);
    replace_text(harness, "Variable name Length1", "length");
    harness.key_press(egui::Key::Enter);
    harness.run();
    replace_text(harness, "Variable value length", "50");
    harness.key_press(egui::Key::Enter);
    harness.run();
    click_button(harness, CONFIRM_OPERATION);

    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    click_button(harness, "Two-point rectangle");
    for point in [SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)] {
        let position = harness
            .state()
            .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point);
        click_at(harness, position);
    }
    click_button(harness, "Extrude");
    replace_text(harness, "Extrusion distance expression", "length");
    harness.key_press(egui::Key::Tab);
    harness.run();
    assert_eq!(harness.state().extrusion_distance_follows(), Some("length"));
    click_button(harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
}

fn place(harness: &mut Harness<'static, KernelLabApp>, field: &str, value: &str) -> f64 {
    let components = harness.state().component_instance_count();
    replace_text(harness, field, value);
    click_button(harness, "Add to current workspace");
    click_button(harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().component_instance_count(),
        components + 1,
        "status: {:?}",
        harness.state().document_status_text()
    );
    harness
        .state()
        .displayed_measures()
        .expect("the placed part measures")
        .volume
}

#[test]
fn a_saved_part_is_placed_many_times_at_different_lengths() {
    let library = TemporaryLibrary::new("lengths");
    let mut harness = harness_with_library(library.path.clone());
    harness.run();
    draw_a_bar_that_follows_length(&mut harness);

    // Save it from the File menu, as a person would.
    click_button(&mut harness, "File menu");
    click_button(&mut harness, "Save to Part Library…");
    assert!(harness.state().save_part_dialog_open());
    replace_text(&mut harness, "Part name", "Bar");
    click_button(&mut harness, "Save to library");
    assert!(
        !harness.state().save_part_dialog_open(),
        "status: {:?}",
        harness.state().document_status_text()
    );
    assert_eq!(
        harness.state().document_status_text(),
        Some("Saved Bar v1.0.0 into the Part Library")
    );

    // The library lists it, selected, with its picture, its version, and a
    // size that names the variable its length follows.
    let part = harness
        .state()
        .part_library()
        .selected_part()
        .expect("the saved part is selected")
        .clone();
    assert_eq!(part.name, "Bar");
    assert_eq!(part.key, "user.bar");
    assert_eq!(part.revision, [1, 0, 0]);
    assert_eq!(part.parameters.len(), 1);
    assert_eq!(part.parameters[0].default, Some(50.0));
    assert_eq!(
        harness.state().part_library().preview_image_size(),
        Some([96, 96])
    );
    assert_eq!(
        harness
            .state()
            .part_library()
            .rough_dimensions_text()
            .as_deref(),
        Some("4 × 2 mm × length")
    );
    harness.get_by_role_and_label(Role::Image, "Picture of Bar");
    harness.get_by_label("v1.0.0");

    // Placed three times, each at its own length.
    let area = 4.0 * 2.0;
    for length in [30.0, 75.0, 120.0] {
        let volume = place(&mut harness, "length (mm)", &format!("{length}"));
        assert!(
            (volume - area * length).abs() < 1.0e-6,
            "{length} mm placed as {volume} mm³"
        );
    }

    // The built-in extrusion, at two lengths of its own, beside them.
    click_button(&mut harness, "20 × 20 Aluminium Extrusion");
    for length in [100.0, 250.0] {
        let volume = place(&mut harness, "Length (mm)", &format!("{length}"));
        assert!((volume - 400.0 * length).abs() < 1.0e-6);
    }
    assert_eq!(harness.state().component_instance_count(), 5);

    // Saving again under the same name is a new version of the same part.
    let before = harness.state().part_library().parts().len();
    harness
        .state_mut()
        .save_current_part_to_library("Bar", None, Vec::new())
        .expect("a second save");
    assert_eq!(harness.state().part_library().parts().len(), before);
    let part = harness.state().part_library().selected_part().unwrap();
    assert_eq!((part.key.as_str(), part.revision), ("user.bar", [2, 0, 0]));
    assert!(part.parameters.is_empty(), "this save offered no variables");

    // The document keeps every placement, and replays them with no library.
    let total = harness.state().mass_properties().volume;
    let saved = harness.state().native_document_json().unwrap();
    let mut reopened = harness_without_library();
    reopened.run();
    reopened
        .state_mut()
        .load_native_document_json(&saved)
        .expect("the document opens without the library");
    reopened.run();
    assert!(reopened.state().features_suppressed_on_open().is_empty());
    assert_eq!(reopened.state().component_instance_count(), 5);
    assert!(
        (reopened.state().mass_properties().volume - total).abs() < 1.0e-6,
        "{} against {total}",
        reopened.state().mass_properties().volume
    );
}

//! The Theme ribbon tab: choosing a theme, editing its colours, resetting.
//!
//! The theme is process-global, so this suite is its own binary: nothing
//! else renders in this process while it switches palettes.

use artificer_workbench::{KernelLabApp, WorkbenchMode, theme};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

fn harness() -> Harness<'static, KernelLabApp> {
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
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn button_disabled(harness: &Harness<'static, KernelLabApp>, label: &str) -> bool {
    harness
        .get_by_role_and_label(Role::Button, label)
        .accesskit_node()
        .is_disabled()
}

#[test]
fn the_theme_tab_chooses_a_theme_edits_its_colours_and_resets_them() {
    theme::set_active_theme(theme::WorkbenchTheme::Dark);
    theme::reset_palette(theme::WorkbenchTheme::Dark);
    theme::reset_palette(theme::WorkbenchTheme::Light);
    let mut harness = harness();
    harness.run();

    // A ribbon-only tab: it never changes the workspace.
    click_button(&mut harness, "Theme ribbon tab");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Light theme")
            .is_some()
    );

    click_button(&mut harness, "Light theme");
    assert_eq!(theme::active_theme(), theme::WorkbenchTheme::Light);
    assert!(!theme::palette().dark);
    // The sketch canvas follows: a near-white ground for the light chrome.
    assert!(theme::sketch().background.r() > 200);
    click_button(&mut harness, "Dark theme");
    assert_eq!(theme::active_theme(), theme::WorkbenchTheme::Dark);
    assert!(theme::sketch().background.r() < 80);

    // Reset has nothing to do until a colour is edited.
    assert!(button_disabled(&harness, "Reset theme colours"));

    click_button(&mut harness, "Edit theme colours");
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::ColorWell, "background colour")
            .is_some(),
        "the editor offers the sketch background"
    );
    // Editing goes through the palette the window writes back; drive it the
    // way the picker does rather than through the popup's sliders.
    let mut edited = theme::palette_for(theme::WorkbenchTheme::Dark);
    edited.sketch.background = egui::Color32::from_rgb(10, 20, 30);
    theme::set_palette(theme::WorkbenchTheme::Dark, edited);
    harness.run();
    assert!(!button_disabled(&harness, "Reset theme colours"));
    assert_eq!(
        theme::sketch().background,
        egui::Color32::from_rgb(10, 20, 30)
    );

    click_button(&mut harness, "Reset theme colours");
    assert!(!theme::palette_is_customised(theme::WorkbenchTheme::Dark));
    assert_eq!(
        theme::sketch().background,
        theme::WorkbenchTheme::Dark
            .default_palette()
            .sketch
            .background
    );
    assert!(button_disabled(&harness, "Reset theme colours"));
}

//! The Simulation tab (ADR 0058), driven headlessly: a structural study
//! set up by picking faces, solved, drawn as stress and deformation on the
//! part, refused by name when ill-posed, and rerun finer.

use artificer_protocol::Tier;
use artificer_sim::Resolution;
use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

fn new_harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

/// The study card scrolls once its conditions and results are longer than
/// the card, so a control below the fold is brought into view first — the
/// same thing the user does.
fn click_card_control(harness: &mut Harness<'static, KernelLabApp>, role: Role, label: &str) {
    let mut previous = None;
    for _ in 0..60 {
        harness.get_by_role_and_label(role, label).scroll_to_me();
        harness.run();
        let rect = harness.get_by_role_and_label(role, label).rect();
        if previous == Some(rect) {
            break;
        }
        previous = Some(rect);
    }
    harness.get_by_role_and_label(role, label).click();
    harness.run();
}

fn click_card_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    click_card_control(harness, Role::Button, label);
}

/// Faces are accessible buttons named by their role; selecting one is a
/// click on it.
fn select_face(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness
        .get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    harness.run();
}

/// The faces the viewport offers from where the camera stands: only a face
/// turned towards the camera is a target, so the test picks from what is
/// on screen rather than naming a face the view hides.
fn visible_faces(harness: &Harness<'static, KernelLabApp>) -> Vec<String> {
    let mut faces = harness
        .get_all_by_role(Role::Button)
        .filter_map(|node| node.accesskit_node().label())
        .filter(|label| label.ends_with(" face"))
        .collect::<Vec<_>>();
    faces.sort();
    faces.dedup();
    faces
}

/// Opens the tab and the structural study on the canonical 2 × 3 × 4 body.
fn open_structural_study(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "Simulation ribbon tab");
    click_button(harness, "Structural study");
    assert!(
        harness
            .state()
            .document_status_text()
            .is_some_and(|status| status.contains("Structural study")),
        "{:?}",
        harness.state().document_status_text()
    );
}

/// Clamps the first face the camera shows and pushes on the second.
fn clamp_and_load(harness: &mut Harness<'static, KernelLabApp>) {
    let faces = visible_faces(harness);
    assert!(faces.len() >= 2, "two faces to pick from: {faces:?}");
    select_face(harness, &faces[0]);
    click_card_button(harness, "Fix selected faces");
    select_face(harness, &faces[1]);
    click_card_button(harness, "Load selected faces");
    let conditions = harness.state().simulation_face_conditions();
    assert_eq!(conditions.len(), 2, "{conditions:?}");
    assert!(conditions.iter().any(|(_, condition)| condition == "fixed"));
    assert!(
        conditions
            .iter()
            .any(|(_, condition)| condition.contains("N"))
    );
}

#[test]
fn the_simulation_tab_opens_a_structural_study_that_solves_the_active_body() {
    let mut harness = new_harness();
    open_structural_study(&mut harness);
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Solve structural study")
            .accesskit_node()
            .is_disabled(),
        "nothing is held or loaded yet"
    );
    clamp_and_load(&mut harness);
    click_card_button(&mut harness, "Solve structural study");
    harness.run();

    let summary = harness
        .state()
        .structural_summary()
        .expect("a solved study leaves a summary");
    assert_eq!(summary.resolution, Resolution::Coarse);
    assert_eq!(summary.tier, Tier::Approximate);
    assert!(summary.converged, "{summary:?}");
    // 24 cells along the 4 mm side of a 2 × 3 × 4 block.
    assert_eq!(summary.voxels, 12 * 18 * 24, "{summary:?}");
    assert!(summary.max_deflection > 0.0, "{summary:?}");
    assert!(summary.max_von_mises > 0.0, "{summary:?}");
    assert!(
        summary.safety_factor.is_finite() && summary.safety_factor > 0.0,
        "{summary:?}"
    );
    assert!(summary.residual <= 1.0e-6, "{summary:?}");
    assert!(
        harness
            .state()
            .document_status_text()
            .is_some_and(|status| status.contains("approximate") && status.contains("voxels")),
        "{:?}",
        harness.state().document_status_text()
    );

    // The picture: every facet corner of the cuboid's twelve facets carries
    // a stress, and the default exaggeration moves the drawn vertices.
    let (samples, moved) = harness
        .state()
        .simulation_display_extent()
        .expect("the study is drawn");
    assert_eq!(samples, 12 * 3);
    assert!(moved > 0.0, "the deformation is drawn exaggerated");
    assert!(
        moved <= 10.0 * summary.max_deflection + 1.0e-9,
        "no vertex moves further than the exaggerated maximum: {moved} vs {}",
        summary.max_deflection
    );
}

#[test]
fn a_study_with_no_fixed_faces_is_refused_by_name() {
    let mut harness = new_harness();
    open_structural_study(&mut harness);
    let faces = visible_faces(&harness);
    select_face(&mut harness, &faces[0]);
    click_card_button(&mut harness, "Load selected faces");
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Solve structural study")
            .accesskit_node()
            .is_disabled(),
        "a study with no fixed face cannot be solved from the card"
    );
    // Forced past the card, the solver refuses by name.
    harness.state_mut().solve_structural_study();
    harness.run();
    assert!(harness.state().structural_summary().is_none());
    let message = harness
        .state()
        .simulation_message()
        .expect("a refusal is named");
    assert!(
        message.contains("no fixed faces") && message.contains("rigid body"),
        "{message}"
    );
}

#[test]
fn the_deformation_slider_and_stress_toggle_change_what_is_drawn() {
    let mut harness = new_harness();
    open_structural_study(&mut harness);
    clamp_and_load(&mut harness);
    click_card_button(&mut harness, "Solve structural study");
    harness.run();
    let (samples, moved_at_ten) = harness.state().simulation_display_extent().unwrap();
    assert_eq!(samples, 36);
    assert!(moved_at_ten > 0.0);

    harness.state_mut().set_simulation_exaggeration(50.0);
    harness.run();
    let (_, moved_at_fifty) = harness.state().simulation_display_extent().unwrap();
    assert!(
        (moved_at_fifty - 5.0 * moved_at_ten).abs() <= 1.0e-6 * moved_at_fifty.max(1.0e-12),
        "{moved_at_fifty} against 5 × {moved_at_ten}"
    );

    harness.state_mut().set_simulation_exaggeration(0.0);
    harness.run();
    let (samples, moved) = harness.state().simulation_display_extent().unwrap();
    assert_eq!(samples, 36, "the colours stay when the deformation goes");
    assert_eq!(moved, 0.0, "zero draws the part as modelled");

    click_card_control(&mut harness, Role::CheckBox, "Stress colours");
    let (samples, _) = harness.state().simulation_display_extent().unwrap();
    assert_eq!(samples, 0, "the toggle takes the colours off");

    click_card_button(&mut harness, "Dismiss study");
    assert!(harness.state().structural_summary().is_none());
    assert!(harness.state().simulation_display_extent().is_none());
}

#[test]
fn rerun_finer_reports_the_change_between_resolutions() {
    let mut harness = new_harness();
    open_structural_study(&mut harness);
    clamp_and_load(&mut harness);
    click_card_button(&mut harness, "Solve structural study");
    harness.run();
    let coarse = harness.state().structural_summary().unwrap();
    assert!(harness.state().simulation_convergence_hint().is_none());

    click_card_button(&mut harness, "Rerun finer");
    harness.run();
    let medium = harness.state().structural_summary().unwrap();
    assert_eq!(medium.resolution, Resolution::Medium);
    assert!(medium.voxels > coarse.voxels, "{medium:?} after {coarse:?}");
    assert!(medium.cell < coarse.cell);
    // A voxel mesh is stiffer than the part; the finer one gives a little.
    assert!(
        medium.max_deflection > coarse.max_deflection,
        "{} after {}",
        medium.max_deflection,
        coarse.max_deflection
    );
    let hint = harness
        .state()
        .simulation_convergence_hint()
        .expect("a rerun reports its change");
    assert!(
        hint.contains("Coarse → Medium") && hint.contains('%'),
        "{hint}"
    );
}

#[test]
fn every_simulation_tab_button_fits_the_supported_minimum_window() {
    let mut harness = Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));
    harness.run();
    click_button(&mut harness, "Simulation ribbon tab");
    for label in [
        "Structural study",
        "Thermal study",
        "Topology optimisation (experimental)",
        "Motion timeline",
        "V  Select",
    ] {
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        assert!(rect.is_positive(), "{label} must have a visible hit region");
        assert!(rect.height() >= 24.0, "{label} is clipped: {rect:?}");
        assert!(
            rect.min.x >= 0.0 && rect.max.x <= 1040.0,
            "{label}: {rect:?}"
        );
    }
    // The tab strip itself still fits with the seventh tab in it.
    let theme_tab = harness
        .get_by_role_and_label(Role::Button, "Theme ribbon tab")
        .rect();
    assert!(theme_tab.max.x <= 1040.0, "{theme_tab:?}");
}

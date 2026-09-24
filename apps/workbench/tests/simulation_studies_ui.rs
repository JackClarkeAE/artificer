//! The other three studies of the Simulation tab (ADR 0058), driven
//! headlessly: the motion timeline over an assembly with a joint, the
//! thermal study on the canonical block, and the experimental topology
//! optimisation carving it.

use artificer_protocol::Tier;
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

/// A control inside the card is scrolled into view before it is clicked.
fn click_card_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let mut previous = None;
    for _ in 0..60 {
        harness
            .get_by_role_and_label(Role::Button, label)
            .scroll_to_me();
        harness.run();
        let rect = harness.get_by_role_and_label(Role::Button, label).rect();
        if previous == Some(rect) {
            break;
        }
        previous = Some(rect);
    }
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.run();
}

fn select_face(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness
        .get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    harness.run();
}

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

fn open_tab(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "Simulation ribbon tab");
}

#[test]
fn a_thermal_study_paints_the_temperature_between_two_held_faces() {
    let mut harness = new_harness();
    open_tab(&mut harness);
    click_button(&mut harness, "Thermal study");
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Solve thermal study")
            .accesskit_node()
            .is_disabled(),
        "nothing is held yet"
    );
    let faces = visible_faces(&harness);
    assert!(faces.len() >= 2, "{faces:?}");
    // The card's temperature is 100 °C until it is changed.
    select_face(&mut harness, &faces[0]);
    click_card_button(&mut harness, "Hold selected faces");
    harness.state_mut().set_simulation_hold_temperature(0.0);
    select_face(&mut harness, &faces[1]);
    click_card_button(&mut harness, "Hold selected faces");
    click_card_button(&mut harness, "Solve thermal study");
    harness.run();

    let summary = harness
        .state()
        .thermal_summary()
        .expect("a solved study leaves a summary");
    assert_eq!(summary.tier, Tier::Approximate);
    assert!(summary.converged, "{summary:?}");
    assert_eq!(summary.voxels, 12 * 18 * 24, "{summary:?}");
    assert!((summary.max_c - 100.0).abs() < 1.0e-6, "{summary:?}");
    assert!(summary.min_c.abs() < 1.0e-6, "{summary:?}");
    assert!(summary.held_nodes > 0);
    assert!(
        summary.convecting_sides > 0,
        "the rest of the skin loses heat"
    );
    // The picture: every facet corner carries a temperature, nothing moves.
    let triangles = harness
        .state()
        .simulation_display_triangles()
        .expect("the study is drawn");
    assert_eq!(triangles, 12);
    let (samples, moved) = harness.state().simulation_display_extent().unwrap();
    assert_eq!(samples, 36);
    assert_eq!(moved, 0.0);
    click_card_button(&mut harness, "Dismiss study");
    assert!(harness.state().thermal_summary().is_none());
}

#[test]
fn a_thermal_study_with_nothing_held_is_refused_by_name() {
    let mut harness = new_harness();
    open_tab(&mut harness);
    click_button(&mut harness, "Thermal study");
    // Convection alone is a study of the air; forced past the card with
    // convection off, the solver refuses by name.
    harness.state_mut().set_thermal_convection(false);
    harness.state_mut().solve_thermal_study();
    harness.run();
    assert!(harness.state().thermal_summary().is_none());
    let message = harness.state().simulation_message().expect("a refusal");
    assert!(message.contains("no face is held"), "{message}");
}

/// Two library parts side by side, the second on a revolute joint about
/// world Z at its own pivot, as the assembly tests build it.
fn jointed_assembly() -> Harness<'static, KernelLabApp> {
    let mut harness = new_harness();
    harness.run();
    click_button(&mut harness, "Library");
    let input = harness.get_by_role_and_label(Role::TextInput, "Length (mm)");
    input.click();
    input.type_text("80");
    harness.run();
    for _ in 0..2 {
        click_button(&mut harness, "Add to current workspace");
        click_button(&mut harness, "Confirm operation");
    }
    click_button(&mut harness, "Library");
    click_card_button(&mut harness, "Add revolute joint");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().assembly_joint_count(), 1);
    harness
}

#[test]
fn the_motion_timeline_scrubs_the_mechanism_and_measures_every_frame() {
    let mut harness = jointed_assembly();
    open_tab(&mut harness);
    click_button(&mut harness, "Motion timeline");
    assert_eq!(harness.state().motion_frame(), 0);

    harness.state_mut().set_motion_frame(16);
    harness.run();
    assert_eq!(
        harness.state().motion_frame(),
        16,
        "the scrubber holds a frame"
    );
    assert!(
        harness.state().animation_holds_joints(),
        "a scrubbed frame poses the joints"
    );
    let posed = harness.state().posed_component_poses();
    let assembled = harness.state().component_poses();
    harness.state_mut().set_motion_frame(32);
    harness.run();
    assert_ne!(
        harness.state().posed_component_poses(),
        posed,
        "another frame is another pose"
    );
    assert_eq!(
        harness.state().component_poses(),
        assembled,
        "the document's assembled poses never move: the timeline is a view"
    );

    click_card_button(&mut harness, "Measure clearance");
    harness.run();
    let summary = harness
        .state()
        .motion_timeline_summary()
        .expect("a measured timeline");
    assert_eq!(summary.frames, 64);
    assert!(!summary.cancelled);
    // The flag on the parts follows the frame: red at a colliding frame,
    // nothing at a clear one.
    match summary.first_collision {
        Some(frame) => {
            harness.state_mut().set_motion_frame(frame);
            harness.run();
            assert_eq!(harness.state().simulation_flagged_bodies().len(), 2);
        }
        None => {
            let tightest = summary
                .tightest_frame
                .expect("a clear motion has a tightest frame");
            harness.state_mut().set_motion_frame(tightest);
            harness.run();
            assert!(harness.state().simulation_flagged_bodies().is_empty());
            assert!(summary.tightest_distance.unwrap() > 0.0);
        }
    }
    // Playing repaints every frame, so the harness is stepped rather than
    // run to quiescence while the motion is going.
    harness
        .get_by_role_and_label(Role::Button, "Play timeline")
        .click();
    harness.step();
    harness.step();
    assert!(harness.state().motion_is_playing());
    harness
        .get_by_role_and_label(Role::Button, "Pause timeline")
        .click();
    harness.step();
    harness.step();
    harness.run();
    assert!(!harness.state().motion_is_playing());
    click_card_button(&mut harness, "Dismiss timeline");
    assert!(harness.state().motion_timeline_summary().is_none());
    assert!(harness.state().simulation_flagged_bodies().is_empty());
}

#[test]
fn the_motion_timeline_needs_a_joint() {
    let mut harness = new_harness();
    open_tab(&mut harness);
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Motion timeline")
            .accesskit_node()
            .is_disabled(),
        "a block with no joints has no motion to time"
    );
}

#[test]
fn topology_optimisation_is_experimental_and_carves_the_block() {
    let mut harness = new_harness();
    open_tab(&mut harness);
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Topology optimisation (experimental)")
            .accesskit_node()
            .is_disabled(),
        "the optimisation carries a solved structural study's supports and loads"
    );
    click_button(&mut harness, "Structural study");
    let faces = visible_faces(&harness);
    select_face(&mut harness, &faces[0]);
    click_card_button(&mut harness, "Fix selected faces");
    select_face(&mut harness, &faces[1]);
    click_card_button(&mut harness, "Load selected faces");
    click_card_button(&mut harness, "Solve structural study");
    harness.run();
    assert!(harness.state().structural_summary().is_some());

    click_button(&mut harness, "Topology optimisation (experimental)");
    harness.state_mut().set_topology_iterations(3);
    harness.state_mut().set_topology_volume_fraction(0.4);
    click_card_button(&mut harness, "Optimise topology");
    harness.run();
    let summary = harness
        .state()
        .topology_summary()
        .expect("an optimisation leaves a summary");
    assert_eq!(summary.tier, Tier::Approximate);
    assert_eq!(summary.iterations, 3);
    assert_eq!(summary.voxels, 12 * 18 * 24);
    assert!((summary.volume_fraction - 0.4).abs() < 0.02, "{summary:?}");
    assert!(summary.last_compliance > 0.0);
    // The picture is the density surface: a voxel skin, not the body's
    // twelve facets, and no field over it.
    let triangles = harness
        .state()
        .simulation_display_triangles()
        .expect("the field is drawn");
    assert!(triangles > 12, "{triangles} triangles of voxel skin");
    assert!(
        harness
            .state()
            .document_status_text()
            .is_some_and(|status| status.contains("experimental")),
        "{:?}",
        harness.state().document_status_text()
    );
    click_card_button(&mut harness, "Dismiss study");
    assert!(harness.state().topology_summary().is_none());
    assert!(harness.state().simulation_display_triangles().is_none());
}

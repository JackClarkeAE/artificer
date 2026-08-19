use artificer_workbench::{
    KernelLabApp, SketchExtrusionEligibility, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};

const CONFIRM_OPERATION: &str = "Confirm operation";

fn harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1040.0, 700.0])
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

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    click_at(harness, canvas_sketch_point(harness, point));
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
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

fn set_extrusion_distance(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    {
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(|node| {
                node.value()
                    .as_deref()
                    .is_some_and(|current| current.starts_with("Distance "))
            })
            .expect("extrusion distance control");
        distance.scroll_to_me();
    }
    harness.run();
    let distance = harness
        .query_all_by_role(Role::SpinButton)
        .find(|node| {
            node.value()
                .as_deref()
                .is_some_and(|current| current.starts_with("Distance "))
        })
        .expect("visible extrusion distance control");
    distance.click();
    harness.run();
    harness.event(egui::Event::Text(value.to_owned()));
    harness.run();
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
}

fn enter_positive_z_face_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "Positive Z face");
    click_button(harness, "Sketch on selected face");
    for _ in 0..18 {
        harness.step();
    }
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    assert!(harness.state().sketch_is_face_supported());
}

fn extrude_and_measure(harness: &mut Harness<'static, KernelLabApp>) -> f64 {
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "every certified first-pass region must expose the same Extrude action"
    );
    click_button(harness, "Extrude");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Extrude active sketch")
    );
    click_button(harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion")
        .volume
}

#[test]
fn outer_diameter_polygon_extrudes_through_the_visible_unified_action() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Outer-diameter polygon");
    replace_tool_input(&mut harness, "Sides", "4");
    replace_tool_input(&mut harness, "Outer diameter", "2");
    replace_tool_input(&mut harness, "Rotation", "45");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 0.0));

    // The polygon stroke commits as its second click lands.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Closed { .. }
            | CertifiedProfileStatus::ClosedRegions {
                analytic: false,
                ..
            }
    ));

    let volume = extrude_and_measure(&mut harness);
    assert!((volume - 8.0).abs() <= 1.0e-9);
}

#[test]
fn analytic_slot_extrudes_without_a_rectangle_specific_fallback() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point centre-to-centre slot");
    replace_tool_input(&mut harness, "Centre distance", "2");
    replace_tool_input(&mut harness, "Width", "1");
    replace_tool_input(&mut harness, "Angle", "0");
    for point in [
        SketchPoint::new(-1.0, 0.0),
        SketchPoint::new(1.0, 0.0),
        SketchPoint::new(0.0, 0.5),
    ] {
        click_sketch_point(&mut harness, point);
    }

    // The slot stroke commits as its third click lands.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_entity_count(), 4);
    assert_eq!(harness.state().sketch_revision(), 1);
    assert!(matches!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::ClosedAnalyticCurves
            | CertifiedProfileStatus::ClosedRegions { analytic: true, .. }
    ));

    let volume = extrude_and_measure(&mut harness);
    assert!((volume - (8.0 + std::f64::consts::PI)).abs() <= 1.0e-8);
}

#[test]
fn analytic_slot_on_a_face_cuts_the_existing_solid() {
    let mut harness = harness();
    enter_positive_z_face_sketch(&mut harness);
    click_button(&mut harness, "Two-point centre-to-centre slot");
    replace_tool_input(&mut harness, "Centre distance", "0.5");
    replace_tool_input(&mut harness, "Width", "0.5");
    replace_tool_input(&mut harness, "Angle", "0");
    for point in [
        SketchPoint::new(-0.25, 0.0),
        SketchPoint::new(0.25, 0.0),
        SketchPoint::new(0.0, 0.25),
    ] {
        click_sketch_point(&mut harness, point);
    }
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::Ready
    );

    let original_snapshot = harness.state().displayed_snapshot_id();
    click_button(&mut harness, "Extrude");
    click_button(&mut harness, "Cut");
    set_extrusion_distance(&mut harness, "-1");
    assert_eq!(harness.state().displayed_snapshot_id(), original_snapshot);
    click_button(&mut harness, CONFIRM_OPERATION);

    assert_eq!(harness.state().last_error_code(), None);
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed analytic face cut");
    let removed = 0.25 + std::f64::consts::PI / 16.0;
    assert!((measures.volume - (24.0 - removed)).abs() <= 1.0e-8);
    assert!(
        harness
            .state()
            .feature_timeline_entries()
            .iter()
            .any(|entry| entry == "Cut 1")
    );
}

#[test]
fn explicit_finish_chain_commits_one_atomic_polyline() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(
        &mut harness,
        "Choose line type; current default: Single line.",
    );
    click_button(&mut harness, "Chained polyline");

    click_sketch_point(&mut harness, SketchPoint::new(-1.0, -1.0));
    // One vertex is not a chain: an explicit finish here has nothing to
    // stage, and the draft stays open for the next vertex.
    press_key(&mut harness, egui::Key::Enter);
    assert!(harness.state().sketch_creation_draft_active());
    assert_eq!(harness.state().sketch_entity_count(), 0);
    click_sketch_point(&mut harness, SketchPoint::new(1.0, -1.0));
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 1.0));
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_entity_count(), 0);
    assert_eq!(harness.state().sketch_revision(), 0);

    // Enter is the explicit finish: the same `finish_polyline_draft` the
    // palette's button used to reach, now that the palette is gone.
    press_key(&mut harness, egui::Key::Enter);
    // Finishing the chain commits the whole polyline as one atomic stroke.
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_entity_count(), 2);
    assert_eq!(harness.state().sketch_revision(), 1);
}

#[test]
fn a_circle_resting_on_a_square_side_still_offers_the_disc_to_extrude() {
    // With grid snap on, "a circle inside a square" lands tangent to a side
    // more often than not. That used to void every region: nothing filled,
    // Extrude greyed out. Now it is two profiles, and Extrude hands the
    // canvas to Select so the click that picks one cannot start a new stroke.
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, -2.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 2.0));
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(-1.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 0.0));

    assert_eq!(harness.state().sketch_entity_count(), 2);
    assert_eq!(harness.state().available_sketch_region_count(), 2);
    // The square's selection survives as the surround, which the tangent
    // point pinches into a loop no solid can be built from — and the tool is
    // still Circle, so Extrude must not simply refuse.
    assert_eq!(harness.state().sketch_tool_label(), "Circle");
    assert_eq!(
        harness.state().sketch_extrusion_eligibility(),
        SketchExtrusionEligibility::PinchedRegion
    );
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "Extrude stays live so it can ask for the profile"
    );
    click_button(&mut harness, "Extrude");
    assert_eq!(harness.state().pending_operation_label(), None);
    assert_eq!(harness.state().sketch_tool_label(), "Select");

    click_sketch_point(&mut harness, SketchPoint::new(-1.0, 0.0));
    assert_eq!(harness.state().selected_sketch_region_count(), 1);
    let volume = extrude_and_measure(&mut harness);
    assert!(
        (volume - 4.0 * std::f64::consts::PI).abs() <= 1.0e-9,
        "{volume}"
    );
}

#[test]
fn an_inscribed_circle_leaves_four_corner_profiles_and_the_disc() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, -2.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 2.0));
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 0.0));

    assert_eq!(harness.state().available_sketch_region_count(), 5);
    click_button(&mut harness, "Select sketch geometry");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 0.0));
    assert_eq!(harness.state().selected_sketch_region_count(), 1);
    let volume = extrude_and_measure(&mut harness);
    assert!(
        (volume - 16.0 * std::f64::consts::PI).abs() <= 1.0e-9,
        "{volume}"
    );
}

use artificer_workbench::{
    ExtrusionMode, KernelLabApp, SketchExtrusionEligibility, WorkbenchMode,
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

/// Extrude lives on the Model tab alone now, and a ribbon tab no longer
/// changes the workspace: a sketch reaches the model commands without leaving.
fn show_model_commands(harness: &mut Harness<'static, KernelLabApp>) {
    if harness
        .query_by_role_and_label(Role::Button, "Extrude")
        .is_none()
    {
        click_button(harness, "Model ribbon tab");
    }
}

fn click_extrude(harness: &mut Harness<'static, KernelLabApp>) {
    show_model_commands(harness);
    click_button(harness, "Extrude");
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
    click_button(harness, "Create sketch");
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
    show_model_commands(harness);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "every certified first-pass region must expose the same Extrude action"
    );
    click_extrude(harness);
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
    click_extrude(&mut harness);
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
    show_model_commands(&mut harness);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Extrude")
            .accesskit_node()
            .is_disabled(),
        "Extrude stays live so it can ask for the profile"
    );
    click_extrude(&mut harness);
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

#[test]
fn an_off_centre_circle_inside_a_rectangle_extrudes_the_ring_around_it() {
    // Rectangle, circle dropped inside it off centre, pick the surround
    // (rectangle minus disc), extrude it.
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, -2.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 2.0));
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(0.5, 0.5));
    click_sketch_point(&mut harness, SketchPoint::new(1.5, 0.5));

    assert_eq!(harness.state().sketch_entity_count(), 2);
    assert_eq!(harness.state().available_sketch_region_count(), 2);
    click_button(&mut harness, "Select sketch geometry");
    click_sketch_point(&mut harness, SketchPoint::new(-1.5, -1.5));
    assert_eq!(harness.state().selected_sketch_region_count(), 1);

    let volume = extrude_and_measure(&mut harness);
    assert!(
        (volume - 4.0 * (16.0 - std::f64::consts::PI)).abs() <= 1.0e-8,
        "{volume}"
    );
}

fn draw_rectangle_around_an_off_centre_circle(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Two-point rectangle");
    click_sketch_point(harness, SketchPoint::new(-2.0, -2.0));
    click_sketch_point(harness, SketchPoint::new(2.0, 2.0));
    click_button(harness, "Centre-point circle");
    click_sketch_point(harness, SketchPoint::new(0.5, 0.5));
    click_sketch_point(harness, SketchPoint::new(1.5, 0.5));
    assert_eq!(harness.state().available_sketch_region_count(), 2);
}

#[test]
fn a_committed_multi_region_sketch_offers_every_region_to_the_model_viewport() {
    // The regions the model viewport can hover and click used to come from
    // the payload's compiled profile cache, which holds only whatever was
    // selected when the sketch was committed. Finish a two-cell sketch
    // without selecting anything and the cache is empty, so outside Sketch
    // mode the sketch drew but nothing under the pointer ever lit up.
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle_around_an_off_centre_circle(&mut harness);
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);

    let anchors = harness.state().model_sketch_region_anchors(0);
    assert_eq!(
        anchors.len(),
        2,
        "both the surround and the disc must be selectable in 3D: {anchors:?}"
    );

    // Every offered anchor selects its own region, and no two name the same
    // one — an anchor that drifted onto a boundary or into a hole would
    // resolve to a neighbouring cell.
    let mut signatures = Vec::new();
    for anchor in anchors {
        assert!(
            harness
                .state_mut()
                .select_committed_sketch_region(0, anchor),
            "anchor {anchor:?} selected no region"
        );
        assert_eq!(harness.state().selected_sketch_region_count(), 1);
        signatures.push(harness.state().selected_sketch_region_signatures());
    }
    assert_ne!(signatures[0], signatures[1]);
}

#[test]
fn a_region_picked_in_the_model_viewport_extrudes_that_region() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle_around_an_off_centre_circle(&mut harness);
    click_button(&mut harness, "Finish sketch");

    // The surround is the region whose anchor is not inside the circle.
    let ring = harness
        .state()
        .model_sketch_region_anchors(0)
        .into_iter()
        .find(|anchor| (anchor[0] - 0.5).hypot(anchor[1] - 0.5) > 1.0)
        .expect("the rectangle minus the disc");
    assert!(harness.state_mut().select_committed_sketch_region(0, ring));
    assert_eq!(harness.state().selected_sketch_region_count(), 1);

    click_extrude(&mut harness);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let volume = harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion")
        .volume;
    assert!(
        (volume - 4.0 * (16.0 - std::f64::consts::PI)).abs() <= 1.0e-8,
        "{volume}"
    );
}

#[test]
fn a_picked_region_is_highlighted_and_a_background_click_releases_it() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle_around_an_off_centre_circle(&mut harness);
    click_button(&mut harness, "Finish sketch");

    // Finishing alone highlights nothing: the lone-cell convenience
    // selection is for Extrude, not a pick the user made.
    assert!(
        harness
            .state_mut()
            .selected_sketch_region_selections()
            .is_empty()
    );

    let anchor = harness.state().model_sketch_region_anchors(0)[0];
    assert!(
        harness
            .state_mut()
            .select_committed_sketch_region(0, anchor)
    );
    let highlighted = harness.state_mut().selected_sketch_region_selections();
    assert_eq!(
        highlighted.len(),
        1,
        "the picked region wears a selection fill"
    );
    assert_eq!(highlighted[0].sketch_index, 0);
    // The pick selected the region, not the geometry beneath it.
    assert_eq!(harness.state().selected_face(), None);

    // Clicking empty space releases the region highlight.
    let viewport = harness.get_by_label("Model viewport").rect();
    click_at(&mut harness, viewport.left_top() + egui::vec2(40.0, 60.0));
    assert!(
        harness
            .state_mut()
            .selected_sketch_region_selections()
            .is_empty()
    );
}

#[test]
fn a_drafted_sketch_offers_its_regions_to_the_model_viewport() {
    // Leaving a sketch as a draft still draws it in 3D. Its regions have to
    // come with it, or the pointer finds nothing to pick for the next feature.
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle_around_an_off_centre_circle(&mut harness);
    click_button(&mut harness, "Exit sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    assert_eq!(harness.state().visible_model_sketch_overlay_count(), 1);
    let anchors = harness.state().model_sketch_region_anchors(0);
    assert_eq!(anchors.len(), 2, "{anchors:?}");
    for anchor in anchors {
        assert!(
            harness
                .state_mut()
                .select_committed_sketch_region(0, anchor),
            "anchor {anchor:?} selected no region"
        );
        assert_eq!(harness.state().selected_sketch_region_count(), 1);
    }
}

fn blank_harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused_blank(creation_context))
}

#[test]
fn the_pointer_picks_a_committed_sketch_region_in_the_model_viewport() {
    // The whole point of showing a sketch in 3D is being able to act on it
    // there: hover a region, click it, extrude it, without going back into
    // Sketch mode. The rectangle is placed off the origin and the circle at
    // its centre so that framing the document puts the disc under the middle
    // of the viewport whatever camera distance the framing chooses.
    let mut harness = blank_harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-1.0, -1.0));
    click_sketch_point(&mut harness, SketchPoint::new(3.0, 3.0));
    click_button(&mut harness, "Centre-point circle");
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 1.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 1.0));
    assert_eq!(harness.state().available_sketch_region_count(), 2);
    click_button(&mut harness, "Finish sketch");
    click_button(&mut harness, "Frame all visible bodies");
    for _ in 0..30 {
        harness.step();
    }

    // Finishing carries the surround over as the live selection, so a click
    // that lands on the disc has to visibly change it.
    let surround = harness.state().selected_sketch_region_signatures();
    assert_eq!(surround.len(), 1);
    let after_finish = harness.state().feature_timeline_entries();
    let viewport = harness.get_by_label("Model viewport").rect();
    click_at(&mut harness, viewport.center());
    let disc = harness.state().selected_sketch_region_signatures();
    assert_eq!(disc.len(), 1);
    assert_ne!(
        disc, surround,
        "clicking the disc in the model viewport must select the disc"
    );

    click_extrude(&mut harness);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().last_error_code(),
        None,
        "{:?}",
        harness.state().last_error_detail()
    );
    let volume = harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion")
        .volume;
    assert!(
        (volume - 4.0 * std::f64::consts::PI).abs() <= 1.0e-8,
        "the disc, not the surround: {volume}"
    );
    // Picking a different region is not an edit to the sketch, so history
    // gains the extrusion and nothing else. Rewriting the sketch here is what
    // marked it dirty and had the extrusion refused for depending on it.
    let mut expected = after_finish;
    expected.push("Extrude 1".to_owned());
    assert_eq!(harness.state().feature_timeline_entries(), expected);
}

#[test]
fn picking_the_surround_of_a_committed_sketch_extrudes_it() {
    // The reported flow, in the order it was hit: the sketch is committed
    // carrying the disc, and the surround is picked afterwards. Picking is not
    // an edit, so the sketch must not be rewritten and marked dirty under the
    // extrusion that immediately depends on it.
    let mut harness = blank_harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle_around_an_off_centre_circle(&mut harness);
    click_button(&mut harness, "Select sketch geometry");
    click_sketch_point(&mut harness, SketchPoint::new(0.5, 0.5));
    assert_eq!(harness.state().selected_sketch_region_count(), 1);
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    let committed = harness.state().feature_timeline_entries();

    // The surround is the region whose anchor is not inside the circle.
    let surround = harness
        .state()
        .model_sketch_region_anchors(0)
        .into_iter()
        .find(|anchor| (anchor[0] - 0.5).hypot(anchor[1] - 0.5) > 1.0)
        .expect("the rectangle minus the disc");
    assert!(
        harness
            .state_mut()
            .select_committed_sketch_region(0, surround)
    );

    click_extrude(&mut harness);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().last_error_code(),
        None,
        "{:?}",
        harness.state().last_error_detail()
    );
    let volume = harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion")
        .volume;
    assert!(
        (volume - 4.0 * (16.0 - std::f64::consts::PI)).abs() <= 1.0e-8,
        "the surround, not the disc: {volume}"
    );
    let mut expected = committed;
    expected.push("Extrude 1".to_owned());
    assert_eq!(harness.state().feature_timeline_entries(), expected);
}

#[test]
fn a_negative_distance_builds_the_new_body_below_the_sketch_plane() {
    // A minus sign used to be swallowed by the distance box's own range, which
    // clamped it to the 0.01 mm minimum and built a sliver. New body reads its
    // sign the way Add and Cut always have: which side of the plane to grow on.
    // The rectangle is deliberately off the u axis, because reversing the frame
    // reflects the profile to match and a symmetric one would hide a mistake.
    let mut harness = blank_harness();
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 1.0));
    click_sketch_point(&mut harness, SketchPoint::new(2.0, 3.0));
    click_extrude(&mut harness);
    set_extrusion_distance(&mut harness, "-4");
    assert!((harness.state().extrusion_distance() + 4.0).abs() <= 1.0e-9);
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, CONFIRM_OPERATION)
            .accesskit_node()
            .is_disabled(),
        "a negative distance is a direction, not an invalid magnitude"
    );

    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().last_error_code(),
        None,
        "{:?}",
        harness.state().last_error_detail()
    );
    let built = harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion");
    let centroid = built.centroid.expect("a solid has a centroid");
    assert!((built.volume - 32.0).abs() <= 1.0e-9, "{}", built.volume);
    assert!(
        (centroid.z + 2.0).abs() <= 1.0e-9,
        "below the plane: {centroid:?}"
    );
    assert!(
        (centroid.y - 2.0).abs() <= 1.0e-9,
        "the profile stays where it was drawn rather than mirroring: {centroid:?}"
    );

    // Replay has to reach the same solid. The kernel takes only positive
    // depths, so the direction survives as a reversed frame or not at all.
    click_button(&mut harness, "Suppress selected feature");
    assert_eq!(harness.state().displayed_measures().map(|m| m.volume), None);
    click_button(&mut harness, "Restore selected feature");
    let rebuilt = harness
        .state()
        .displayed_measures()
        .expect("the restored feature rebuilds");
    let rebuilt_centroid = rebuilt.centroid.expect("a solid has a centroid");
    assert!((rebuilt.volume - built.volume).abs() <= 1.0e-9);
    assert!(
        (rebuilt_centroid.z - centroid.z).abs() <= 1.0e-9,
        "rebuilt on the wrong side of the plane: {rebuilt_centroid:?}"
    );
    assert!((rebuilt_centroid.y - centroid.y).abs() <= 1.0e-9);
}

#[test]
fn a_minus_sign_still_chooses_cut_on_a_face() {
    // Opening New body to negative distances must not disturb what the sign
    // has always meant on a face: in Auto it picks the operation, and Cut is
    // what a negative distance asks for.
    let mut harness = harness();
    enter_positive_z_face_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-0.4, -0.4));
    click_sketch_point(&mut harness, SketchPoint::new(0.4, 0.4));
    // The distance control belongs to the staged operation's panel.
    click_extrude(&mut harness);

    set_extrusion_distance(&mut harness, "-0.5");
    assert_eq!(harness.state().extrusion_mode(), ExtrusionMode::Cut);
    assert!((harness.state().extrusion_distance() + 0.5).abs() <= 1.0e-12);

    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().last_error_code(),
        None,
        "{:?}",
        harness.state().last_error_detail()
    );
    let measures = harness
        .state()
        .displayed_measures()
        .expect("committed face cut");
    // The canonical cuboid is 24 mm³, and the corners snap to a 1 × 1 square,
    // so a 0.5 deep pocket removes exactly 0.5.
    assert!(
        (measures.volume - 23.5).abs() <= 1.0e-9,
        "a negative distance must remove material, not add it: {}",
        measures.volume
    );
}

#[test]
fn a_second_sketch_is_its_own_feature_and_the_first_still_extrudes() {
    // The reported flow: two sketches, extrude the second, then extrude the
    // first without undoing anything. The Sketch command used to resume the
    // committed first sketch silently, so the "second sketch" rewrote the
    // first one in place and the rewrite left its feature dirty and
    // unextrudable.
    let mut harness = blank_harness();
    // Sketch 1: a 2 x 2 square left of the origin.
    enter_xy_sketch(&mut harness);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(-3.0, -1.0));
    click_sketch_point(&mut harness, SketchPoint::new(-1.0, 1.0));
    click_button(&mut harness, "Finish sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
    // Sketch 2: a 3 x 3 square right of the origin, its own feature. With the
    // first sketch committed the Create command names itself "New sketch",
    // which is the whole point of this test.
    click_button(&mut harness, "XY Plane");
    click_button(&mut harness, "New sketch");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    click_button(&mut harness, "Two-point rectangle");
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 1.0));
    click_sketch_point(&mut harness, SketchPoint::new(4.0, 4.0));
    click_button(&mut harness, "Finish sketch");
    assert_eq!(
        harness.state().feature_timeline_entries(),
        ["Origin", "Sketch 1 · r1", "Sketch 2 · r1"],
        "the second sketch must be its own history feature"
    );
    // Extrude sketch 2 (the active one) at the default 4 mm.
    click_extrude(&mut harness);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().last_error_code(),
        None,
        "{:?}",
        harness.state().last_error_detail()
    );
    let volume = harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion")
        .volume;
    assert!((volume - 36.0).abs() <= 1.0e-8, "sketch 2 alone: {volume}");

    // Now extrude sketch 1 without undoing anything: pick its region the way
    // the viewport click does, then run the same Extrude action.
    let anchor = harness
        .state()
        .model_sketch_region_anchors(0)
        .first()
        .copied()
        .expect("sketch 1 keeps its region");
    assert!(
        harness
            .state_mut()
            .select_committed_sketch_region(0, anchor),
        "sketch 1's region must be selectable after sketch 2 was consumed"
    );
    click_extrude(&mut harness);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(
        harness.state().last_error_code(),
        None,
        "{:?}",
        harness.state().last_error_detail()
    );
    let second = harness
        .state()
        .displayed_measures()
        .expect("both extrusions committed")
        .volume;
    assert!(
        (second - 16.0).abs() <= 1.0e-8,
        "sketch 1's own 16 mm3 body: {second}"
    );
    let timeline = harness.state().feature_timeline_entries();
    assert_eq!(
        timeline.len(),
        5,
        "both extrusions live in history with both sketches: {timeline:?}"
    );
    assert!(
        timeline[3].starts_with("Extrude") && timeline[4].starts_with("Extrude"),
        "{timeline:?}"
    );
}

#[test]
fn shift_select_multiple_sketch_regions_in_viewport() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    draw_rectangle_around_an_off_centre_circle(&mut harness);
    click_button(&mut harness, "Finish sketch");

    let anchors = harness.state().model_sketch_region_anchors(0);
    assert_eq!(anchors.len(), 2);

    // Select first region
    assert!(
        harness
            .state_mut()
            .select_committed_sketch_region_additive(0, anchors[0], false)
    );
    assert_eq!(harness.state().selected_sketch_region_count(), 1);

    // Shift-select second region
    assert!(
        harness
            .state_mut()
            .select_committed_sketch_region_additive(0, anchors[1], true)
    );
    assert_eq!(harness.state().selected_sketch_region_count(), 2);

    // Both regions should now extrude together (full rectangle without hole)
    click_extrude(&mut harness);
    click_button(&mut harness, CONFIRM_OPERATION);
    assert_eq!(harness.state().last_error_code(), None);
    let volume = harness
        .state()
        .displayed_measures()
        .expect("committed exact extrusion")
        .volume;
    // 8.0 * 4.0 * 2.0 (extrusion distance 2.0) = 64.0
    assert!((volume - 64.0).abs() <= 1.0e-8, "volume: {volume}");
}

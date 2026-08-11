use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{CertifiedProfileStatus, SketchDimensionKind, SketchPoint},
};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT as _, Queryable as _},
};
use serde_json::Value;

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
    // The canvas is painted after the fixed confirmation rail. Give the next
    // frame a chance to expose a newly staged operation to accessibility.
    harness.step();
}

fn click_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    let center = harness
        .get_by_role_and_label(Role::Button, label)
        .rect()
        .center();
    click_at(harness, center);
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn enter_xy_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    click_button(harness, "XY Plane");
    click_button(harness, "Sketch mode");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    harness.get_by_label("Sketch viewport");
}

fn choose_variant(harness: &mut Harness<'static, KernelLabApp>, chooser: &str, variant: &str) {
    click_button(harness, chooser);
    click_button(harness, variant);
    assert!(
        harness.query_all_by_label(variant).count() >= 2,
        "{variant} should be both the compact primary action and ACTIVE TOOL"
    );
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn click_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    click_at(harness, canvas_sketch_point(harness, point));
}

fn hover_sketch_point(harness: &mut Harness<'static, KernelLabApp>, point: SketchPoint) {
    harness.hover_at(canvas_sketch_point(harness, point));
    harness.step();
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

fn type_canvas_dimension(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn assert_committed(harness: &Harness<'static, KernelLabApp>, entities: usize, revision: u64) {
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(!harness.state().operation_confirmation_pending());
    assert_eq!(harness.state().sketch_pending_entity_count(), 0);
    assert_eq!(harness.state().sketch_entity_count(), entities);
    assert_eq!(harness.state().sketch_revision(), revision);
}

fn finish_sketch(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "Finish sketch command");
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(harness.state().sketch_finished());
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn assert_closed_profile(status: CertifiedProfileStatus) {
    assert!(
        matches!(
            status,
            CertifiedProfileStatus::Closed { .. }
                | CertifiedProfileStatus::ClosedAnalyticCircle
                | CertifiedProfileStatus::ClosedAnalyticCurves
                | CertifiedProfileStatus::ClosedRegions { .. }
        ),
        "expected an exact closed profile, got {status:?}"
    );
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PersistedAuthoringCounts {
    operations: usize,
    profile: usize,
    construction: usize,
    reference: usize,
}

/// Reads the committed v6 authoring graph through the app's public native-file
/// seam. This verifies roles from persisted model truth rather than paint.
fn persisted_authoring_counts(app: &KernelLabApp) -> PersistedAuthoringCounts {
    let document: Value = serde_json::from_str(
        &app.native_document_json()
            .expect("committed native document should serialize"),
    )
    .expect("native document should be valid JSON");
    let features = document["state"]["features"]
        .as_array()
        .expect("native feature list");
    let authoring = features
        .iter()
        .rev()
        .find_map(|feature| feature.get("sketch_payload"))
        .and_then(|payload| payload.get("authoring"))
        .expect("finished sketch should persist editable v6 authoring");
    let operations = authoring["operations"]
        .as_array()
        .expect("persisted sketch operations")
        .iter()
        .filter(|operation| operation["active"].as_bool() == Some(true))
        .count();
    let mut counts = PersistedAuthoringCounts {
        operations,
        ..PersistedAuthoringCounts::default()
    };
    for entity in authoring["entities"]
        .as_object()
        .expect("persisted sketch entities")
        .values()
        .filter(|entity| entity["active"].as_bool() == Some(true))
    {
        match entity["role"].as_str().expect("persisted entity role") {
            "profile" => counts.profile += 1,
            "construction" => counts.construction += 1,
            "reference" => counts.reference += 1,
            role => panic!("unexpected persisted sketch role {role}"),
        }
    }
    counts
}

#[test]
fn centreline_is_construction_and_escape_is_revision_and_id_neutral() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    choose_variant(
        &mut harness,
        "Choose line type; current default: Single line.",
        "Centreline",
    );

    // Abandoning a half-finished draft with Escape is revision- and
    // id-neutral: nothing was committed, so nothing needs undoing.
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 0.0));
    assert!(harness.state().sketch_creation_draft_active());
    press_key(&mut harness, egui::Key::Escape);
    assert!(!harness.state().sketch_creation_draft_active());
    assert_committed(&harness, 0, 0);

    for point in [SketchPoint::new(-2.0, 0.0), SketchPoint::new(2.0, 0.0)] {
        click_sketch_point(&mut harness, point);
    }
    // The centreline commits as its second click lands.
    assert_committed(&harness, 1, 1);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Empty
    );

    finish_sketch(&mut harness);
    assert_eq!(
        persisted_authoring_counts(harness.state()),
        PersistedAuthoringCounts {
            operations: 1,
            profile: 0,
            construction: 1,
            reference: 0,
        }
    );
}

#[test]
fn centre_rectangle_and_two_point_circle_use_tab_dimensions_and_exact_roles() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);
    choose_variant(
        &mut harness,
        "Choose rectangle type; current default: Two-point rectangle.",
        "Centre-point rectangle",
    );

    click_sketch_point(&mut harness, SketchPoint::new(-2.25, 0.0));
    hover_sketch_point(&mut harness, SketchPoint::new(-1.25, 0.75));
    press_key(&mut harness, egui::Key::Tab);
    type_canvas_dimension(&mut harness, "Rectangle width", "2");
    press_key(&mut harness, egui::Key::Tab);
    type_canvas_dimension(&mut harness, "Rectangle height", "1.5");
    press_key(&mut harness, egui::Key::Enter);
    // Enter completes the dimensioned rectangle, which commits.
    assert_committed(&harness, 4, 1);
    assert_closed_profile(harness.state().sketch_profile_status());

    choose_variant(
        &mut harness,
        "Choose circle type; current default: Centre-point circle.",
        "Two-point diameter circle",
    );
    click_sketch_point(&mut harness, SketchPoint::new(1.25, 0.0));
    hover_sketch_point(&mut harness, SketchPoint::new(3.25, 0.0));
    press_key(&mut harness, egui::Key::Tab);
    type_canvas_dimension(&mut harness, "Circle diameter", "2");
    assert_eq!(
        harness.state().sketch_dimension_readouts()[0].kind,
        SketchDimensionKind::Diameter
    );
    press_key(&mut harness, egui::Key::Enter);
    assert_committed(&harness, 5, 2);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::ClosedRegions {
            regions: 2,
            loops: 2,
            holes: 0,
            analytic: true,
        }
    );

    finish_sketch(&mut harness);
    assert_eq!(
        persisted_authoring_counts(harness.state()),
        PersistedAuthoringCounts {
            operations: 2,
            profile: 5,
            construction: 0,
            reference: 0,
        }
    );
}

#[test]
fn both_polygon_and_both_slot_variants_commit_atomic_closed_profiles() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    choose_variant(
        &mut harness,
        "Choose polygon type; current default: Outer-diameter polygon.",
        "Inner-diameter polygon",
    );
    replace_tool_input(&mut harness, "Sides", "5");
    // Descriptor ordering is the keyboard contract for the compact palette.
    harness
        .get_by_role_and_label(Role::TextInput, "Sides")
        .focus();
    harness.run();
    press_key(&mut harness, egui::Key::Tab);
    assert!(
        harness
            .get_by_role_and_label(Role::TextInput, "Inner diameter")
            .is_focused()
    );
    replace_tool_input(&mut harness, "Inner diameter", "2");
    replace_tool_input(&mut harness, "Rotation", "18");
    click_sketch_point(&mut harness, SketchPoint::new(-3.25, 1.75));
    click_sketch_point(&mut harness, SketchPoint::new(-2.25, 1.75));
    // Each atomic compound stroke commits as its final click lands.
    assert_committed(&harness, 5, 1);

    choose_variant(
        &mut harness,
        "Choose polygon type; current default: Inner-diameter polygon.",
        "Outer-diameter polygon",
    );
    replace_tool_input(&mut harness, "Sides", "7");
    replace_tool_input(&mut harness, "Outer diameter", "2");
    replace_tool_input(&mut harness, "Rotation", "0");
    click_sketch_point(&mut harness, SketchPoint::new(0.0, 1.75));
    click_sketch_point(&mut harness, SketchPoint::new(1.0, 1.75));
    assert_committed(&harness, 12, 2);

    click_button(&mut harness, "Two-point centre-to-centre slot");
    replace_tool_input(&mut harness, "Centre distance", "2");
    replace_tool_input(&mut harness, "Width", "0.75");
    replace_tool_input(&mut harness, "Angle", "0");
    for point in [
        SketchPoint::new(-3.0, -1.75),
        SketchPoint::new(-1.0, -1.75),
        SketchPoint::new(-2.0, -1.375),
    ] {
        click_sketch_point(&mut harness, point);
    }
    assert_committed(&harness, 16, 3);

    choose_variant(
        &mut harness,
        "Choose slot type; current default: Two-point centre-to-centre slot.",
        "Centre-to-outer-point slot",
    );
    replace_tool_input(&mut harness, "Overall length", "2.5");
    replace_tool_input(&mut harness, "Width", "0.75");
    replace_tool_input(&mut harness, "Angle", "0");
    for point in [
        SketchPoint::new(1.75, -1.75),
        SketchPoint::new(3.0, -1.75),
        SketchPoint::new(1.75, -1.375),
    ] {
        click_sketch_point(&mut harness, point);
    }
    assert_committed(&harness, 20, 4);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::ClosedRegions {
            regions: 4,
            loops: 4,
            holes: 0,
            analytic: true,
        }
    );

    finish_sketch(&mut harness);
    assert_eq!(
        persisted_authoring_counts(harness.state()),
        PersistedAuthoringCounts {
            operations: 4,
            profile: 20,
            construction: 0,
            reference: 0,
        }
    );
}

#[test]
fn both_arc_variants_remain_exact_open_profile_curves() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Centre-start-end arc");
    for point in [
        SketchPoint::new(-2.0, 0.0),
        SketchPoint::new(-1.0, 0.0),
        SketchPoint::new(-2.0, 1.0),
    ] {
        click_sketch_point(&mut harness, point);
    }
    // The arc commits as its third click lands.
    assert_committed(&harness, 1, 1);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Open
    );

    choose_variant(
        &mut harness,
        "Choose arc type; current default: Centre-start-end arc.",
        "Three-point arc",
    );
    for point in [
        SketchPoint::new(1.0, 0.0),
        SketchPoint::new(3.0, 0.0),
        SketchPoint::new(2.0, 1.0),
    ] {
        click_sketch_point(&mut harness, point);
    }
    assert_committed(&harness, 2, 2);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Open
    );

    finish_sketch(&mut harness);
    assert_eq!(
        persisted_authoring_counts(harness.state()),
        PersistedAuthoringCounts {
            operations: 2,
            profile: 2,
            construction: 0,
            reference: 0,
        }
    );
}

#[test]
fn pattern_mode_controls_block_invalid_text_and_commit_bounded_instances() {
    let mut harness = harness();
    enter_xy_sketch(&mut harness);

    click_button(&mut harness, "Single line");
    click_sketch_point(&mut harness, SketchPoint::new(-2.0, 0.0));
    click_sketch_point(&mut harness, SketchPoint::new(-1.0, 0.0));
    assert_committed(&harness, 1, 1);

    click_button(&mut harness, "Rectangular sketch pattern");
    replace_tool_input(&mut harness, "First count", "invalid");
    assert!(harness.query_all_by_label("Enter a number").count() >= 1);
    click_sketch_point(&mut harness, SketchPoint::new(0.5, 0.0));
    assert!(harness.state().pending_operation_label().is_none());
    assert_eq!(harness.state().sketch_entity_count(), 1);
    assert_eq!(harness.state().sketch_revision(), 1);

    replace_tool_input(&mut harness, "First count", "3");
    replace_tool_input(&mut harness, "First spacing", "1.5");
    click_sketch_point(&mut harness, SketchPoint::new(0.5, 0.0));
    // A bounded pattern placement is a complete stroke, which commits.
    assert_committed(&harness, 3, 2);

    click_button(&mut harness, "Select sketch geometry");
    click_sketch_point(&mut harness, SketchPoint::new(-1.5, 0.0));
    choose_variant(
        &mut harness,
        "Choose pattern type; current default: Rectangular sketch pattern.",
        "Circular sketch pattern",
    );
    replace_tool_input(&mut harness, "Count", "4");
    let extent = harness.get_by_role_and_label(Role::TextInput, "Angular extent");
    assert!(
        extent.accesskit_node().is_disabled(),
        "angular extent is irrelevant in full-circle mode"
    );
    harness
        .get_by_role_and_label(Role::CheckBox, "Full circle")
        .click();
    harness.run();
    replace_tool_input(&mut harness, "Angular extent", "180");
    harness
        .get_by_role_and_label(Role::CheckBox, "Rotate instances")
        .click();
    harness.run();
    click_sketch_point(&mut harness, SketchPoint::new(-1.5, -2.0));
    assert_committed(&harness, 6, 3);
    assert_eq!(
        harness.state().sketch_profile_status(),
        CertifiedProfileStatus::Open
    );

    finish_sketch(&mut harness);
    assert_eq!(
        persisted_authoring_counts(harness.state()),
        PersistedAuthoringCounts {
            operations: 3,
            profile: 6,
            construction: 0,
            reference: 0,
        }
    );
}

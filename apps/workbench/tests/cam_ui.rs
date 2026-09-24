//! The CAM tab (ADR 0057), driven headlessly: Auto-CAM stages a plan for
//! the canonical cuboid behind the confirmation gate, the card scrubs and
//! plays it, Confirm keeps it, and the G-code exports.

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
    harness
        .get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    harness.run();
}

/// A click that takes one frame rather than running to quiescence: while
/// the simulation plays the viewport asks for a repaint every frame, and
/// `Harness::run` treats that as a page that never settles.
fn click_button_once(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    harness
        .get_by_role_and_label(Role::Button, label)
        .click_accesskit();
    harness.step();
}

fn open_cam_tab(harness: &mut Harness<'static, KernelLabApp>) {
    click_button(harness, "CAM ribbon tab");
}

fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "artificer-cam-ui-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn auto_cam_stages_a_plan_for_the_cuboid_behind_the_gate() {
    let mut harness = new_harness();
    harness.run();
    open_cam_tab(&mut harness);
    // The plan-dependent commands wait for a plan.
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Export G-code")
            .accesskit_node()
            .is_disabled()
    );
    click_button(&mut harness, "Auto-CAM");
    assert_eq!(
        harness.state().pending_operation_label(),
        Some("Auto-CAM plan")
    );
    let summary = harness.state().cam_plan_summary().expect("a plan");
    assert_eq!(summary.machine, "mill");
    assert_eq!(summary.setup, "Milled");
    assert_eq!(summary.operations, 2, "face, then profile");
    assert_eq!(summary.tool_changes, 1);
    assert_eq!(summary.collisions, 0);
    assert!(summary.total_seconds > 1.0, "{summary:?}");
    let names = harness.state().cam_operation_names();
    assert!(names[0].starts_with("Face"), "{names:?}");
    assert!(names[1].starts_with("Profile"), "{names:?}");
    // Staging executed nothing: the body count is unchanged and the plan is
    // not yet kept.
    assert!(!harness.state().cam_is_committed());
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Confirm operation")
            .is_some()
    );
    // The card carries the timeline and the transport.
    harness.get_by_label("Timeline");
    harness.get_by_role_and_label(Role::Button, "Play simulation");
    // Export waits for the tick.
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Export G-code file")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn the_timeline_scrubs_and_plays_and_the_tool_moves() {
    let mut harness = new_harness();
    harness.run();
    open_cam_tab(&mut harness);
    click_button(&mut harness, "Auto-CAM");
    let total = harness.state().cam_plan_summary().unwrap().total_seconds;
    let start = harness.state().cam_tool_position().unwrap();
    harness.state_mut().set_cam_time(total / 2.0);
    harness.run();
    assert!((harness.state().cam_time() - total / 2.0).abs() < 1.0e-9);
    let midway = harness.state().cam_tool_position().unwrap();
    assert_ne!(start, midway, "the tool is somewhere else half way through");
    // The overlay has a stock ghost, remaining stock and a tool to draw.
    assert!(harness.state_mut().cam_overlay_triangle_count() > 100);
    // Playing advances the clock frame by frame.
    harness.state_mut().set_cam_time(0.0);
    harness.state_mut().set_cam_speed(64.0);
    click_button_once(&mut harness, "Play simulation");
    assert!(harness.state().cam_playing());
    for _ in 0..30 {
        harness.step();
    }
    assert!(harness.state().cam_time() > 0.0, "the clock ran");
    // Rewind puts it back.
    click_button_once(&mut harness, "Rewind simulation");
    harness.run();
    assert_eq!(harness.state().cam_time(), 0.0);
    assert!(!harness.state().cam_playing());
    // Playing to the end stops by itself.
    harness.state_mut().set_cam_time(total - 0.01);
    harness.state_mut().set_cam_playing(true);
    for _ in 0..10 {
        harness.step();
    }
    assert!(!harness.state().cam_playing());
    assert!((harness.state().cam_time() - total).abs() < 1.0e-9);
}

#[test]
fn confirm_keeps_the_plan_and_the_gcode_exports() {
    let mut harness = new_harness();
    harness.run();
    open_cam_tab(&mut harness);
    click_button(&mut harness, "Auto-CAM");
    click_button(&mut harness, "Confirm operation");
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(harness.state().cam_is_committed());
    assert!(
        !harness
            .get_by_role_and_label(Role::Button, "Export G-code")
            .accesskit_node()
            .is_disabled()
    );
    let directory = scratch("export");
    let path = directory.join("cuboid.ngc");
    harness.state().export_cam_gcode_to(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("%\n(Artificer CAM: "));
    assert!(text.contains("T"), "a tool change");
    assert!(
        text.contains("G83") || text.contains("G1 "),
        "cutting moves"
    );
    assert!(text.trim_end().ends_with("M30\n%"));
    assert_eq!(harness.state().cam_gcode().as_deref(), Some(text.as_str()));
    // The card stays up on the CAM tab, describing what was kept.
    assert!(
        harness
            .query_by_role_and_label(Role::Button, "Show CAM Operations")
            .is_some()
    );
    std::fs::remove_dir_all(&directory).ok();
}

#[test]
fn cancel_drops_the_staged_plan() {
    let mut harness = new_harness();
    harness.run();
    open_cam_tab(&mut harness);
    click_button(&mut harness, "Auto-CAM");
    click_button(&mut harness, "Cancel operation");
    assert_eq!(harness.state().pending_operation_label(), None);
    assert!(harness.state().cam_plan_summary().is_none());
    assert!(!harness.state().cam_is_committed());
    assert!(
        harness
            .get_by_role_and_label(Role::Button, "Export G-code")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn operations_reorder_from_the_card_and_re_simulate() {
    let mut harness = new_harness();
    harness.run();
    open_cam_tab(&mut harness);
    click_button(&mut harness, "Auto-CAM");
    let before = harness.state().cam_operation_names();
    click_button(&mut harness, "Show CAM Operations");
    click_button(&mut harness, "Move operation 2 earlier");
    let after = harness.state().cam_operation_names();
    assert_eq!(after, vec![before[1].clone(), before[0].clone()]);
    let summary = harness.state().cam_plan_summary().unwrap();
    assert_eq!(summary.operations, 2);
    assert!(summary.total_seconds > 0.0);
}

#[test]
fn the_tool_library_is_seeded_on_first_use_and_read_back() {
    let directory = scratch("tools");
    let path = directory.join("tools.json");
    let mut harness = new_harness();
    harness.run();
    harness.state_mut().set_cam_tools_path(Some(path.clone()));
    open_cam_tab(&mut harness);
    click_button(&mut harness, "Auto-CAM");
    let text = std::fs::read_to_string(&path).expect("tools.json seeded on first use");
    let library = artificer_cam::ToolLibrary::from_json(&text).unwrap();
    assert_eq!(library, artificer_cam::ToolLibrary::builtin());
    click_button(&mut harness, "Cancel operation");

    // A hand-edited library, with every end mill but the 6 removed, is what
    // the next plan uses.
    let mut edited = library.clone();
    edited.tools.retain(|tool| {
        tool.kind != artificer_cam::ToolKind::FlatEndMill || (tool.diameter - 6.0).abs() < 1.0e-9
    });
    std::fs::write(&path, edited.to_json().unwrap()).unwrap();
    let mut harness = new_harness();
    harness.run();
    harness.state_mut().set_cam_tools_path(Some(path.clone()));
    open_cam_tab(&mut harness);
    click_button(&mut harness, "Auto-CAM");
    assert_eq!(harness.state().cam_tool_count(), edited.tools.len());
    let gcode = harness.state().cam_gcode().unwrap();
    assert!(gcode.contains("6 mm 4-flute end mill"), "{gcode}");
    assert!(!gcode.contains("12 mm 4-flute end mill"));
    std::fs::remove_dir_all(&directory).ok();
}

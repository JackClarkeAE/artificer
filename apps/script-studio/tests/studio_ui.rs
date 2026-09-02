//! The studio opens, runs the welcome script, shows the run in the console,
//! and re-runs when the text or a customizer value changes.

use std::time::{Duration, Instant};

use artificer_script_studio::{ScriptStudio, WELCOME_SCRIPT};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

fn harness(source: &str) -> Harness<'static, ScriptStudio> {
    let source = source.to_owned();
    Harness::builder()
        .with_size([1360.0, 840.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Light)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(move |creation_context| ScriptStudio::with_source(creation_context, &source))
}

/// Steps frames until a run newer than `generation` has answered. The
/// kernel runs on another thread and an edit waits out the debounce first,
/// so the frames are paced rather than spun.
fn settle_past(harness: &mut Harness<'static, ScriptStudio>, generation: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        harness.step();
        let state = harness.state();
        if !state.is_running()
            && let Some(outcome) = state.last_outcome()
            && outcome.generation > generation
        {
            let generation = outcome.generation;
            // One more frame so the console and status reflect the outcome.
            harness.step();
            return generation;
        }
        assert!(Instant::now() < deadline, "the script run did not finish");
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn the_welcome_script_runs_and_reports_in_the_console() {
    let mut harness = harness(WELCOME_SCRIPT);
    settle_past(&mut harness, 0);

    let outcome = harness.state().last_outcome().expect("a run");
    assert!(outcome.succeeded(), "{:?}", outcome.error);
    assert!(outcome.scene.is_some());
    // The header's run status carries its text in its accessible name.
    harness.get_by_label_contains("Run status: ●");

    // The welcome script's parameters populate the customizer.
    assert!(
        harness
            .state()
            .customizer_rows()
            .iter()
            .any(|row| row.parameter.name == "hub_radius")
    );
}

#[test]
fn an_error_is_located_and_the_previous_model_stays() {
    let mut harness = harness(WELCOME_SCRIPT);
    let first = settle_past(&mut harness, 0);

    harness.state_mut().set_source(
        "let a = box(size: [10, 10, 10], label: \"a\");\n\nlet b = cylinder(radius: 2);\n",
    );
    settle_past(&mut harness, first);

    let outcome = harness.state().last_outcome().expect("a second run");
    let error = outcome.error.as_ref().expect("the cylinder lacks a height");
    assert_eq!(error.location, Some((3, 9)), "{error:?}");
    assert!(
        outcome.scene.is_some(),
        "the last good model stays visible through a parse error"
    );

    harness.get_by_label_contains("Run status: ✕");
    harness.get_by_label_contains("line 3");
    harness.get_by_role_and_label(Role::Button, "Script error");
}

#[test]
fn a_customizer_change_reruns_the_script() {
    let mut harness =
        harness("param w: f64 = 10.0;\nlet b = box(size: [w, 10, 10], label: \"b\");\n");
    let first = settle_past(&mut harness, 0);
    let volume = |harness: &Harness<'static, ScriptStudio>| {
        harness
            .state()
            .last_outcome()
            .and_then(|outcome| outcome.snapshot.as_ref())
            .map(|snapshot| snapshot.measures().volume)
            .expect("a body")
    };
    assert!((volume(&harness) - 1000.0).abs() < 1.0e-9);

    harness.get_by_role_and_label(Role::SpinButton, "Parameter w");
    harness.state_mut().set_parameter("w", 25.0);
    settle_past(&mut harness, first);
    assert!((volume(&harness) - 2500.0).abs() < 1.0e-9);
}

#[test]
fn the_section_plane_clips_the_model_and_shows_its_controls() {
    use artificer_script_studio::{SectionAxis, SectionPlane};
    let mut harness = harness(WELCOME_SCRIPT);
    settle_past(&mut harness, 0);
    // The toggle lives in the View menu; the panel appears once it is on.
    harness.get_by_role_and_label(Role::Button, "View").click();
    harness.step();
    harness.get_by_role_and_label(Role::CheckBox, "Section analysis");
    harness.state_mut().set_section(SectionPlane {
        active: true,
        axis: SectionAxis::Z,
        offset: 4.0,
        flipped: true,
    });
    harness.step();
    harness.step();
    // The plane reaches the renderer as a clipping plane, kept side below.
    let plane = harness
        .state()
        .section()
        .cut_plane()
        .expect("an active section has a plane");
    assert!(plane.distance_to_point(artificer_protocol::Point3::new(0.0, 0.0, 0.0)) > 0.0);
    assert!(plane.distance_to_point(artificer_protocol::Point3::new(0.0, 0.0, 20.0)) < 0.0);
    harness.get_by_role_and_label(Role::SpinButton, "Section offset");
}

#[test]
fn the_run_button_and_menus_are_reachable() {
    let mut harness = harness(WELCOME_SCRIPT);
    let first = settle_past(&mut harness, 0);
    harness
        .get_by_role_and_label(Role::Button, "Run script")
        .click();
    settle_past(&mut harness, first);
    harness.get_by_role_and_label(Role::TextInput, "Script");
    harness.get_by_role_and_label(Role::CheckBox, "Auto-run");
}

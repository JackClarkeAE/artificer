//! The studio opens, runs the welcome script, shows the run in the console,
//! and re-runs when the text or a customizer value changes.

use std::time::{Duration, Instant};

use artificer_script_studio::{ScriptStudio, WELCOME_SCRIPT};
use egui::accesskit::Role;
use egui::{ViewportCommand, ViewportId};
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

/// Whether the last frame asked the window to do `command`.
fn root_viewport_sent(harness: &Harness<'static, ScriptStudio>, command: &ViewportCommand) -> bool {
    harness
        .output()
        .viewport_output
        .get(&ViewportId::ROOT)
        .is_some_and(|output| output.commands.contains(command))
}

/// The window's close button, as the platform reports it.
fn request_window_close(harness: &mut Harness<'static, ScriptStudio>) {
    harness
        .input_mut()
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
}

/// Steps up to `frames` frames and reports whether any of them asked the
/// window to do `command`. A click's answer lands a frame or two after the
/// click, so the check has to span more than the frame of the click.
fn sent_within_frames(
    harness: &mut Harness<'static, ScriptStudio>,
    command: &ViewportCommand,
    frames: usize,
) -> bool {
    (0..frames).any(|_| {
        harness.step();
        root_viewport_sent(harness, command)
    })
}

#[test]
fn closing_a_dirty_script_asks_first_and_does_not_close() {
    let mut harness = harness(WELCOME_SCRIPT);
    harness.state_mut().set_native_file_dialogs(false);
    settle_past(&mut harness, 0);

    // A clean script closes without a word.
    assert!(!harness.state().is_dirty());
    request_window_close(&mut harness);
    harness.step();
    assert!(!harness.state().unsaved_prompt_open());
    assert!(!root_viewport_sent(&harness, &ViewportCommand::CancelClose));

    // A dirty one is asked about, and the close is cancelled meanwhile.
    harness
        .state_mut()
        .set_source("let a = box(size: [1, 1, 1], label: \"a\");\n");
    assert!(harness.state().is_dirty());
    request_window_close(&mut harness);
    harness.step();
    assert!(harness.state().unsaved_prompt_open());
    assert!(root_viewport_sent(&harness, &ViewportCommand::CancelClose));
    harness.step();
    harness.get_by_role_and_label(Role::Button, "Save");
    harness.get_by_role_and_label(Role::Button, "Don't save");

    // Cancel keeps editing; nothing is sent to the window.
    harness
        .get_by_role_and_label(Role::Button, "Cancel")
        .click();
    harness.step();
    harness.step();
    assert!(!harness.state().unsaved_prompt_open());
    assert!(!root_viewport_sent(&harness, &ViewportCommand::Close));
    assert!(harness.state().is_dirty(), "cancelling saves nothing");

    // Don't save lets the window go: the close is sent, and the window's
    // next close request goes through unchallenged.
    request_window_close(&mut harness);
    harness.step();
    harness.step();
    assert!(harness.state().unsaved_prompt_open());
    assert!(!harness.state().window_close_confirmed());
    harness
        .get_by_role_and_label(Role::Button, "Don't save")
        .click();
    assert!(sent_within_frames(&mut harness, &ViewportCommand::Close, 4));
    assert!(!harness.state().unsaved_prompt_open());
    assert!(harness.state().window_close_confirmed());
    request_window_close(&mut harness);
    harness.step();
    assert!(!harness.state().unsaved_prompt_open());
    assert!(!root_viewport_sent(&harness, &ViewportCommand::CancelClose));
}

#[test]
fn replacing_a_dirty_script_with_an_example_asks_first() {
    let mut harness = harness(WELCOME_SCRIPT);
    harness.state_mut().set_native_file_dialogs(false);
    settle_past(&mut harness, 0);
    let edited = "let a = box(size: [1, 1, 1], label: \"a\");\n";
    harness.state_mut().set_source(edited);

    harness
        .get_by_role_and_label(Role::Button, "Examples")
        .click();
    harness.step();
    harness
        .get_by_role_and_label(Role::Button, "Filleted cube")
        .click();
    harness.step();
    harness.step();
    assert!(harness.state().unsaved_prompt_open());
    assert_eq!(harness.state().source(), edited, "nothing replaced yet");

    harness
        .get_by_role_and_label(Role::Button, "Don't save")
        .click();
    harness.step();
    harness.step();
    assert!(!harness.state().unsaved_prompt_open());
    assert_ne!(harness.state().source(), edited, "the example took over");
    assert!(!harness.state().is_dirty());
}

#[test]
fn clicking_a_named_face_selects_it_and_the_console_names_it() {
    let mut harness = harness(WELCOME_SCRIPT);
    settle_past(&mut harness, 0);
    harness
        .get_by_role_and_label(Role::Button, "Face flange_top")
        .click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("Selected face: flange_top · planar, facing up");
}

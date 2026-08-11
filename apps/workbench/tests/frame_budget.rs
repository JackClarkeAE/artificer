use std::time::{Duration, Instant};

use artificer_geometry::ProfileWinding;
use artificer_sketch::{
    Angle, Integer, PointInput, SignedLength, SketchPoint2, SketchRecipe, SketchValue,
};
use artificer_workbench::{
    KernelLabApp, WorkbenchMode,
    sketch::{
        CertifiedProfileStatus, SketchCanvasState, SketchGeometry, SketchPoint, show as show_sketch,
    },
};
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};

const CONFIRM_OPERATION: &str = "Confirm operation";

fn workbench_harness() -> Harness<'static, KernelLabApp> {
    Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context))
}

fn activate_button(harness: &mut Harness<'static, KernelLabApp>, label: &str) {
    {
        let button = harness.get_by_role_and_label(Role::Button, label);
        button.click_accesskit();
    }
    harness.run();
}

fn press_key(harness: &mut Harness<'static, KernelLabApp>, key: egui::Key) {
    harness.key_down(key);
    harness.step();
    harness.key_up(key);
    harness.step();
}

fn canvas_sketch_point(harness: &Harness<'static, KernelLabApp>, point: SketchPoint) -> egui::Pos2 {
    harness
        .state()
        .sketch_point_screen_position(harness.get_by_label("Sketch viewport").rect(), point)
}

fn click_canvas(harness: &mut Harness<'static, KernelLabApp>, position: egui::Pos2) {
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

fn type_dimension(harness: &mut Harness<'static, KernelLabApp>, label: &str, value: &str) {
    harness
        .get_by_role_and_label(Role::TextInput, label)
        .type_text(value);
    harness.run();
}

fn finish_centered_face_rectangle(
    harness: &mut Harness<'static, KernelLabApp>,
    width: &str,
    height: &str,
) {
    let half_width = width.parse::<f64>().expect("numeric rectangle width") * 0.5;
    let half_height = height.parse::<f64>().expect("numeric rectangle height") * 0.5;
    activate_button(harness, "Two-point rectangle");
    click_canvas(
        harness,
        canvas_sketch_point(harness, SketchPoint::new(-half_width, -half_height)),
    );
    press_key(harness, egui::Key::Tab);
    type_dimension(harness, "Rectangle width", width);
    press_key(harness, egui::Key::Tab);
    type_dimension(harness, "Rectangle height", height);
    press_key(harness, egui::Key::Enter);
    activate_button(harness, "Finish sketch command");
    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Model);
}

fn select_latest_feature_end(harness: &mut Harness<'static, KernelLabApp>) {
    {
        let face = harness
            .query_all_by_role_and_label(Role::Button, "Feature end face")
            .last()
            .expect("latest generated feature end");
        face.click_accesskit();
    }
    harness.run();
}

fn set_extrusion_distance(harness: &mut Harness<'static, KernelLabApp>, value: &str) {
    {
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(|node| {
                node.value()
                    .as_deref()
                    .is_some_and(|value| value.starts_with("Distance "))
            })
            .expect("extrusion distance control");
        distance.scroll_to_me();
    }
    harness.run();
    {
        let distance = harness
            .query_all_by_role(Role::SpinButton)
            .find(|node| {
                node.value()
                    .as_deref()
                    .is_some_and(|value| value.starts_with("Distance "))
            })
            .expect("visible extrusion distance control");
        distance.click_accesskit();
    }
    harness.run();
    harness.event(egui::Event::Text(value.to_owned()));
    harness.run();
}

fn commit_repeated_face_feature_chain(harness: &mut Harness<'static, KernelLabApp>) {
    harness.run();
    activate_button(harness, "Positive Z face");
    activate_button(harness, "Sketch on selected face");
    finish_centered_face_rectangle(harness, "1", "1");
    activate_button(harness, "Extrude");
    activate_button(harness, CONFIRM_OPERATION);

    select_latest_feature_end(harness);
    activate_button(harness, "Sketch on selected face");
    finish_centered_face_rectangle(harness, "0.5", "0.5");
    activate_button(harness, "Cut");
    set_extrusion_distance(harness, "-0.5");
    activate_button(harness, "Extrude");
    activate_button(harness, CONFIRM_OPERATION);

    select_latest_feature_end(harness);
    activate_button(harness, "Sketch on selected face");
    activate_button(harness, "Snap");
    finish_centered_face_rectangle(harness, "0.25", "0.25");
    set_extrusion_distance(harness, "0.25");
    activate_button(harness, "Extrude");
    activate_button(harness, CONFIRM_OPERATION);

    let counts = harness
        .state()
        .displayed_topology_counts()
        .expect("committed repeated-feature topology");
    assert_eq!((counts.vertices, counts.edges, counts.faces), (32, 48, 21));
}

fn assert_60hz_budget<State>(harness: &mut Harness<'_, State>, label: &str) {
    harness.run_steps(100);
    let frame_count = 500;
    let mut samples = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        let start = Instant::now();
        harness.step();
        samples.push(start.elapsed());
    }
    samples.sort_unstable();
    let total = samples.iter().copied().sum::<Duration>();
    let average = total / frame_count as u32;
    let median = samples[frame_count / 2];
    let p95 = samples[(frame_count * 95).div_ceil(100) - 1];
    let maximum = samples[frame_count - 1];
    let budget = Duration::from_secs_f64(1.0 / 60.0);

    if std::env::var_os("ARTIFICER_PERF_REPORT").is_some() {
        eprintln!(
            "ARTIFICER_PERF fixture={label:?} frames={frame_count} average_ns={} median_ns={} p95_ns={} max_ns={}",
            average.as_nanos(),
            median.as_nanos(),
            p95.as_nanos(),
            maximum.as_nanos(),
        );
    }

    assert!(
        average < budget,
        "{label} averaged {average:?} per frame (median {median:?}, p95 {p95:?}, max {maximum:?})"
    );
    assert!(
        p95 < budget,
        "{label} p95 was {p95:?} per frame (median {median:?}, average {average:?}, max {maximum:?})"
    );
}

fn dense_square_vertices() -> Vec<SketchPoint> {
    let mut vertices = Vec::new();
    for x in 0..64 {
        vertices.push(SketchPoint::new(f64::from(x), 0.0));
    }
    for y in 0..64 {
        vertices.push(SketchPoint::new(64.0, f64::from(y)));
    }
    for x in (1..=64).rev() {
        vertices.push(SketchPoint::new(f64::from(x), 64.0));
    }
    for y in (1..=64).rev() {
        vertices.push(SketchPoint::new(0.0, f64::from(y)));
    }
    vertices.push(vertices[0]);
    vertices
}

/// A quick CPU-side guard for the immediate-mode UI and debug-scene renderer.
///
/// This is deliberately not presented as a GPU or end-to-end frame pacing
/// guarantee. The native app reports repaint-start cadence on the actual
/// machine; this catches interaction/render preparation regressions in every
/// test run.
#[test]
fn headless_ui_generation_average_fits_60hz_budget() {
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));
    harness.state_mut().set_animation_playing(true);

    assert_60hz_budget(&mut harness, "model workbench UI generation");
}

#[test]
fn collapsed_workbench_generation_fits_60hz_budget() {
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));

    harness.run();
    for label in [
        "Collapse browser panel",
        "History",
        "Collapse command ribbon",
    ] {
        harness.get_by_role_and_label(Role::Button, label).click();
        harness.run();
    }
    harness.state_mut().set_animation_playing(true);

    assert_60hz_budget(&mut harness, "collapsed model workbench UI generation");
}

#[test]
fn repeated_face_feature_workbench_generation_fits_60hz_budget() {
    let mut harness = workbench_harness();
    commit_repeated_face_feature_chain(&mut harness);
    harness.state_mut().set_animation_playing(true);

    assert_60hz_budget(&mut harness, "21-face Add/Cut/Add workbench UI generation");
}

#[test]
fn dense_face_sketch_body_context_generation_fits_60hz_budget() {
    let mut harness = workbench_harness();
    commit_repeated_face_feature_chain(&mut harness);
    select_latest_feature_end(&mut harness);
    activate_button(&mut harness, "Sketch on selected face");

    assert_eq!(harness.state().workbench_mode(), WorkbenchMode::Sketch);
    let (triangles, edges) = harness
        .state()
        .face_sketch_context_counts()
        .expect("generated face sketch should cache its committed-body projection");
    assert!(triangles >= 2);
    assert_eq!(
        edges, 28,
        "the face-normal sketch context must omit the 20 rear/hidden source edges"
    );
    harness.state_mut().set_animation_playing(true);

    assert_60hz_budget(
        &mut harness,
        "21-face body-context sketch workbench UI generation",
    );
}

#[test]
fn dense_certified_sketch_generation_fits_60hz_budget() {
    let vertices = dense_square_vertices();

    let mut sketch = SketchCanvasState::default();
    for edge in vertices.windows(2) {
        sketch
            .stage_geometry(SketchGeometry::segment(edge[0], edge[1]))
            .expect("dense fixture edge should stage");
        sketch
            .commit_pending()
            .expect("dense fixture edge should commit");
    }
    assert_eq!(
        sketch.certified_profile_status(),
        CertifiedProfileStatus::Closed {
            winding: ProfileWinding::CounterClockwise,
        }
    );

    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_ui_state(
            |ui, state| {
                let _ = show_sketch(ui, state);
            },
            sketch,
        );

    assert_60hz_budget(&mut harness, "256-edge certified sketch UI generation");
}

#[test]
fn active_rectangle_dimension_overlay_fits_60hz_budget() {
    let mut sketch = SketchCanvasState::default();
    sketch
        .stage_geometry(SketchGeometry::rectangle(
            SketchPoint::new(0.0, 0.0),
            SketchPoint::new(4.0, 2.0),
        ))
        .expect("rectangle fixture should stage");

    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_ui_state(
            |ui, state| {
                let _ = show_sketch(ui, state);
            },
            sketch,
        );

    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "Rectangle width")
        .click();
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::TextInput, "Rectangle width")
            .is_some()
    );

    assert_60hz_budget(&mut harness, "active rectangle dimension overlay");
}

/// Fixed `sketch.pattern.256` fixture from the 2D-tooling performance contract.
///
/// The seed is committed and all 255 generated outputs remain provisional, so
/// this measures the maximum visible pattern preview without publishing a
/// second sketch revision or substituting paint-only copies for authored curves.
#[test]
fn maximum_rectangular_pattern_preview_fits_60hz_budget() {
    let mut sketch = SketchCanvasState::default();
    let _seed = sketch
        .stage_recipe(
            SketchRecipe::Line {
                start: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
                end: PointInput::Position(SketchPoint2::new(0.2, 0.0)),
            },
            "Add pattern seed",
        )
        .expect("pattern seed should stage");
    sketch.commit_pending().expect("pattern seed should commit");
    let source = sketch
        .authoring()
        .active_entities()
        .next()
        .expect("committed seed curve")
        .id;
    sketch
        .stage_recipe(
            SketchRecipe::RectangularPattern {
                sources: vec![source],
                columns: SketchValue::Literal(Integer::new(16)),
                rows: SketchValue::Literal(Integer::new(16)),
                column_spacing: SketchValue::Literal(
                    SignedLength::new(0.4).expect("finite column spacing"),
                ),
                row_spacing: SketchValue::Literal(
                    SignedLength::new(0.4).expect("finite row spacing"),
                ),
                direction: SketchValue::Literal(Angle::radians(0.0).expect("finite pattern angle")),
            },
            "Preview maximum pattern",
        )
        .expect("maximum bounded pattern should stage");

    assert_eq!(
        sketch.pending().expect("pattern preview").entities().len(),
        255,
        "the existing seed is not duplicated by a 16 by 16 pattern"
    );
    assert_eq!(sketch.authoring().revision().get(), 1);

    let mut harness = Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_ui_state(
            |ui, state| {
                let _ = show_sketch(ui, state);
            },
            sketch,
        );

    assert_60hz_budget(&mut harness, "sketch.pattern.256 pending preview");
}

/// Rendering ceiling fixture for the declared 1,024 active-curve sketch limit.
#[test]
fn maximum_visible_curve_preview_fits_60hz_budget() {
    let vertices = (0..=1_024)
        .map(|index| {
            let row = index / 33;
            let column = index % 33;
            let snake_column = if row % 2 == 0 { column } else { 32 - column };
            PointInput::Position(SketchPoint2::new(
                (snake_column as f64).mul_add(0.2, -3.2),
                (row as f64).mul_add(0.2, -3.1),
            ))
        })
        .collect();
    let mut sketch = SketchCanvasState::default();
    sketch
        .stage_recipe(
            SketchRecipe::Polyline {
                vertices,
                closed: false,
                construction: true,
            },
            "Preview maximum visible curves",
        )
        .expect("the declared 1,024-curve ceiling should stage");
    assert_eq!(
        sketch
            .pending()
            .expect("curve ceiling preview")
            .entities()
            .len(),
        1_024
    );

    let mut harness = Harness::builder()
        .with_size([1040.0, 700.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .build_ui_state(
            |ui, state| {
                let _ = show_sketch(ui, state);
            },
            sketch,
        );

    assert_60hz_budget(&mut harness, "sketch.visible_curves.1024 pending preview");
}

/// CPU-side development guard for the latency of a geometry-changing click.
///
/// This is not an end-to-end presentation guarantee, but the median CPU work
/// for a representative geometry-changing click must fit one 60 Hz frame.
#[test]
fn staging_the_closing_edge_of_a_dense_sketch_stays_interactive() {
    const SAMPLE_COUNT: usize = 5;
    const MEDIAN_BUDGET: Duration = Duration::from_micros(16_667);

    let vertices = dense_square_vertices();
    let closing_edge = &vertices[vertices.len() - 2..];
    let mut samples = Vec::with_capacity(SAMPLE_COUNT);

    for _ in 0..SAMPLE_COUNT {
        let mut sketch = SketchCanvasState::default();
        for edge in vertices.windows(2).take(vertices.len() - 2) {
            sketch
                .stage_geometry(SketchGeometry::segment(edge[0], edge[1]))
                .expect("dense fixture edge should stage");
            sketch
                .commit_pending()
                .expect("dense fixture edge should commit");
        }
        assert_eq!(
            sketch.certified_profile_status(),
            CertifiedProfileStatus::Open
        );

        let start = Instant::now();
        sketch
            .stage_geometry(SketchGeometry::segment(closing_edge[0], closing_edge[1]))
            .expect("closing edge should stage");
        samples.push(start.elapsed());

        assert_eq!(
            sketch.certified_profile_status(),
            CertifiedProfileStatus::Closed {
                winding: ProfileWinding::CounterClockwise,
            }
        );
    }

    samples.sort_unstable();
    let median = samples[SAMPLE_COUNT / 2];
    assert!(
        median < MEDIAN_BUDGET,
        "dense closing-edge certification median was {median:?}; samples: {samples:?}"
    );
}

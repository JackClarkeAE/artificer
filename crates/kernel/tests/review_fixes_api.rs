//! Regressions from a review of the public API and the `.art` language:
//! each test reproduces one finding and holds its fix.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::{Duration, Instant};

use artificer_kernel::api::analysis::MAX_STUDY_SUBJECTS;
use artificer_kernel::api::commands::{ApiCommand, SketchEntity, SketchPlane};
use artificer_kernel::api::debug::ApiErrorCode;
use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::journal::JournalEntry;
use artificer_kernel::api::scripting::parser::MAX_EXPRESSION_DEPTH;
use artificer_kernel::api::scripting::{
    MAX_ARRAY_DEPTH, MAX_EVALUATION_DEPTH, MAX_EVALUATION_STEPS, ScriptError, compile_program,
    compile_script,
};
use artificer_kernel::api::selectors::{
    EntitySelector, GeometricSelector, NormalMatch, resolve_selector,
};
use artificer_kernel::api::server::{
    INVALID_REQUEST, JsonRpcResponse, MAX_REQUEST_BYTES, SharedSession, serve_lines,
};
use artificer_kernel::api::session::Session;
use artificer_kernel::api::sweep::{MAX_SWEEP_MEASUREMENTS, MAX_SWEEP_STEPS};
use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{Point2, Point3, Vector3};

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

fn compile_error(source: &str) -> ScriptError {
    compile_script(source, &BTreeMap::new()).expect_err("the script is refused")
}

fn labels(source: &str) -> Vec<String> {
    compile_program(source, &BTreeMap::new())
        .unwrap_or_else(|error| panic!("{error}"))
        .commands
        .iter()
        .map(|command| command.label().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. `return <feature> with faces` builds the feature
// ---------------------------------------------------------------------------

#[test]
fn a_feature_returned_with_faces_is_built_and_later_steps_reach_it() {
    let source = r#"
let base = box(size: [50, 50, 10], label: "base");
fn boss() -> body {
    return cylinder(center: [100, 100, 0], radius: 5, height: 20, label: "c") with faces { top: faces(">Z") };
}
let b = boss();
drill(face: b.top, center: [0, 0], diameter: 4, depth: 5, label: "d");
"#;
    assert_eq!(labels(source), ["base", "boss_1/c", "d"]);
    // The drill goes into the cylinder the function returned, not into the
    // base: the body left is the cylinder less the hole.
    let session = run(source);
    let expected = std::f64::consts::PI * (5.0 * 5.0 * 20.0 - 2.0 * 2.0 * 5.0);
    let volume = session.snapshot.measures().volume;
    assert!(
        (volume - expected).abs() < 1.0e-6 * expected,
        "{volume} against {expected}"
    );
}

// ---------------------------------------------------------------------------
// 2. Nesting limits that multiply are one budget, not a stack overflow
// ---------------------------------------------------------------------------

/// Thirty-two functions, each calling the next inside sixty nested calls:
/// every construct is inside its own limit, and together they are two
/// thousand levels deep.
fn chained_nesting() -> String {
    let functions = 32;
    let mut source = format!("fn f{functions}() {{ return 1; }}\n");
    for index in (1..functions).rev() {
        source.push_str(&format!(
            "fn f{index}() {{ return {}f{}(){}; }}\n",
            "nearest(point: ".repeat(60),
            index + 1,
            ")".repeat(60)
        ));
    }
    source.push_str("let x = f1();\n");
    source
}

#[test]
fn nesting_that_multiplies_across_functions_is_an_error_on_a_default_stack() {
    let source = chained_nesting();
    // A spawned thread's default stack, which is what Script Studio's worker
    // and every test thread get; the evaluator used to overflow it.
    let outcomes = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || [(); 2].map(|()| compile_script(&source, &BTreeMap::new()).map(|_| ())))
        .unwrap()
        .join()
        .expect("the compilation returns rather than overflowing");
    let error = outcomes[0].clone().expect_err("the nesting is refused");
    assert!(
        error
            .message()
            .contains(&format!("more than {MAX_EVALUATION_DEPTH} levels deep")),
        "{error}"
    );
    assert!(error.location().is_some(), "{error}");
    // The same script is refused the same way every time.
    assert_eq!(outcomes[0], outcomes[1]);
}

#[test]
fn an_operator_chain_counts_toward_the_expression_depth() {
    // A chain is parsed in a loop but builds a tree as deep as it is long;
    // a hundred thousand links would overflow evaluating or dropping it.
    let long = format!("let x = {}1;", "1 + ".repeat(100_000));
    let error = compile_error(&long);
    assert!(error.to_string().contains("nested deeper"), "{error}");
    let long = format!(
        "let b = box(size: [1, 1, 1], label: \"b\");\nlet f = b{};",
        ".x".repeat(100_000)
    );
    let error = compile_error(&long);
    assert!(error.to_string().contains("nested deeper"), "{error}");
    // A chain of ordinary length is untouched.
    let usual = format!("let x = {}1;", "1 + ".repeat(MAX_EXPRESSION_DEPTH / 2));
    compile_script(&usual, &BTreeMap::new()).expect("a sum of thirty terms is fine");
}

#[test]
fn arrays_wrapped_in_arrays_through_a_variable_are_bounded() {
    let source = "let a = [1];\nfor i in 0..1000 { let a = [a]; }\n";
    let error = compile_error(source);
    assert!(
        error
            .message()
            .contains(&format!("nest at most {MAX_ARRAY_DEPTH} deep")),
        "{error}"
    );
    compile_script(
        "let a = [1];\nfor i in 0..8 { let a = [a]; }\n",
        &BTreeMap::new(),
    )
    .expect("a few levels are fine");
}

// ---------------------------------------------------------------------------
// 3. Work that multiplies is bounded, and values are shared, not copied
// ---------------------------------------------------------------------------

fn steps_refusal(error: &ScriptError) -> bool {
    error
        .message()
        .contains(&format!("more than {MAX_EVALUATION_STEPS} steps"))
}

#[test]
fn a_call_tree_that_multiplies_is_refused_by_the_step_budget() {
    // Each function calls the next ten times, eight levels down: ten million
    // calls of the last, with no loop and nothing deep.
    let levels = 8;
    let mut source = format!("fn f{levels}() {{ }}\n");
    for level in (1..levels).rev() {
        source.push_str(&format!(
            "fn f{level}() {{ {} }}\n",
            format!("f{}(); ", level + 1).repeat(10)
        ));
    }
    source.push_str("f1();\n");
    let error = compile_error(&source);
    assert!(steps_refusal(&error), "{error}");
}

#[test]
fn text_built_again_and_again_is_refused_by_the_step_budget() {
    // Half a megabyte of text, within what one string may hold, joined to
    // a number in every iteration of a loop: each join is new text.
    let source = "let s = \"ab\";\nfor i in 0..18 { let s = s + s; }\nfor i in 0..10000 { let t = s + i; }\n";
    let error = compile_error(source);
    assert!(steps_refusal(&error), "{error}");
}

#[test]
fn a_shared_array_is_cheap_to_hold_and_paid_for_when_a_call_receives_it() {
    // A thousand arrays of a thousand arrays share their items: holding
    // them costs next to nothing.
    let row = vec!["0"; 1000].join(", ");
    let rows = vec!["a"; 1000].join(", ");
    let planes = ["b"; 10].join(", ");
    let build = format!("let a = [{row}];\nlet b = [{rows}];\nlet c = [{planes}];\n");
    compile_script(&build, &BTreeMap::new()).expect("sharing costs nothing");
    // Handing ten million items to a call is paid for item by item.
    let call = format!("{build}fn f(x: any) {{ }}\nf(c);\n");
    let error = compile_error(&call);
    assert!(steps_refusal(&error), "{error}");
    // As is handing a megabyte string to a builtin ten thousand times over.
    let labels = "let s = \"ab\";\nfor i in 0..19 { let s = s + s; }\nlet names = [s, s, s, s, s, s, s, s, s, s];\nfor i in 0..10000 { let v = sketch(on: \"XY\", entities: [], label: names[0]); }\n";
    let error = compile_error(labels);
    assert!(steps_refusal(&error), "{error}");
}

#[test]
fn many_top_level_statements_and_calls_cost_what_they_bind() {
    // Forty thousand names, then ten thousand calls. Every statement used
    // to copy every name bound before it, and every call every name: over
    // a billion copies. Now each costs what it binds, and the whole takes a
    // fraction of a second.
    let mut source = String::new();
    for index in 0..40_000 {
        source.push_str(&format!("let v{index} = {index};\n"));
    }
    source.push_str("fn f(x: f64) -> f64 { return x + v39999; }\n");
    source.push_str("for i in 0..10000 { let y = f(x: i); }\n");
    let started = Instant::now();
    let program = compile_program(&source, &BTreeMap::new()).unwrap_or_else(|e| panic!("{e}"));
    assert!(program.commands.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "compiling took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_rebinding_still_replaces_the_name_a_host_is_shown() {
    let source = "let b = box(size: [1, 1, 1], label: \"b\");\nlet top = faces(\">Z\");\nlet side = faces(\">X\");\nlet top = faces(\"<Z\");\n";
    let program = compile_program(source, &BTreeMap::new()).unwrap();
    let names: Vec<&str> = program
        .names
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(names, ["side", "top"]);
    assert_eq!(
        program.names[1].1,
        EntitySelector::ByGeometry {
            selector: GeometricSelector::FaceByNormal {
                direction: Vector3::new(0.0, 0.0, -1.0),
                match_kind: NormalMatch::Closest,
            },
        }
    );
}

// ---------------------------------------------------------------------------
// 4. An oversized request line is discarded without being gathered
// ---------------------------------------------------------------------------

fn responses(output: &[u8]) -> Vec<JsonRpcResponse> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn an_oversized_line_is_refused_and_the_next_request_is_still_answered() {
    let mut input = vec![b'x'; MAX_REQUEST_BYTES + 4096];
    input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"report\"}\n");
    // A request of exactly the limit is not too long, and must not take the
    // request after it down with it.
    let mut exact = br#"{"jsonrpc":"2.0","id":3,"method":"report"}"#.to_vec();
    exact.resize(MAX_REQUEST_BYTES, b' ');
    input.extend_from_slice(&exact);
    input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"report\"}\n");
    // And one past it, with no newline before the end of the stream.
    input.extend(std::iter::repeat_n(b'y', MAX_REQUEST_BYTES + 1));

    let mut output = Vec::new();
    serve_lines(Cursor::new(input), &mut output).unwrap();
    let answers = responses(&output);
    assert_eq!(answers.len(), 5, "{answers:?}");
    assert_eq!(
        answers[0].error.as_ref().map(|e| e.code),
        Some(INVALID_REQUEST)
    );
    for (answer, id) in answers[1..4].iter().zip([2, 3, 4]) {
        assert_eq!(answer.id, Some(serde_json::json!(id)));
        assert_eq!(answer.error, None, "{answer:?}");
    }
    assert_eq!(
        answers[4].error.as_ref().map(|e| e.code),
        Some(INVALID_REQUEST)
    );
}

// ---------------------------------------------------------------------------
// 5. Redo redoes every step undone
// ---------------------------------------------------------------------------

fn make_box(label: &str) -> ApiCommand {
    ApiCommand::MakeBox {
        label: label.to_owned(),
        origin: Point3::new(0.0, 0.0, 0.0),
        size: [10.0, 10.0, 10.0],
    }
}

#[test]
fn two_undos_are_redone_by_two_redos() {
    let token = CancellationToken::default();
    let mut session = Session::new();
    session.execute(make_box("a"), &token).unwrap();
    session.execute(make_box("b"), &token).unwrap();
    let digest = session.snapshot.semantic_digest();
    session.undo().unwrap();
    session.undo().unwrap();
    assert!(session.journal.is_empty());
    session.redo().expect("the first redo");
    session
        .redo()
        .expect("the second redo, which the first used to clear");
    assert_eq!(session.step_order, ["a", "b"]);
    assert_eq!(session.snapshot.semantic_digest(), digest);
    assert!(session.redo().is_err(), "nothing is left to redo");

    // A new edit after an undo still clears what was undone.
    session.undo().unwrap();
    session.execute(make_box("c"), &token).unwrap();
    assert!(session.redo().is_err());
}

#[test]
fn a_redo_that_fails_keeps_its_step() {
    let token = CancellationToken::default();
    let mut session = Session::new();
    session.execute(make_box("a"), &token).unwrap();
    let doomed = ApiCommand::BooleanUnion {
        label: "u".to_owned(),
        target: artificer_kernel::api::commands::StepLabel("a".to_owned()),
        tool: artificer_kernel::api::commands::StepLabel("missing".to_owned()),
    };
    session.redo_stack.push(JournalEntry::new(doomed));
    assert!(session.redo().is_err());
    assert_eq!(
        session.redo_stack.len(),
        1,
        "the failed step is still there"
    );
}

// ---------------------------------------------------------------------------
// 6. Numbers stay finite, and axes have a direction
// ---------------------------------------------------------------------------

#[test]
fn numbers_that_are_not_finite_are_refused() {
    let error = compile_error("let x = 1e400;");
    assert!(error.to_string().contains("too large"), "{error}");
    assert_eq!(error.location(), Some((1, 9)));
    for source in [
        "let x = 1e300 * 1e300;",
        "let x = 1e308 + 1e308;",
        "let x = -1e308 - 1e308;",
        "let x = 1e300 / 1e-10;",
    ] {
        let error = compile_error(source);
        assert!(error.message().contains("overflows"), "{source}: {error}");
    }
    // `f64::clamp` panics on bounds the wrong way round; a script says so.
    let error = compile_error("let x = clamp(5, 10, 1);");
    assert!(error.message().contains("above the high"), "{error}");
}

#[test]
fn a_parameter_override_that_is_not_finite_is_refused() {
    let source = "param w: f64 [mm] in 20..200 = 60;\nparam h: f64 = 10;\nlet b = box(size: [w, h, 1], label: \"b\");\n";
    for (name, value) in [
        ("w", f64::NAN),
        ("w", f64::INFINITY),
        ("h", f64::NAN),
        ("h", f64::NEG_INFINITY),
    ] {
        let overrides = BTreeMap::from([(name.to_owned(), value)]);
        let error = compile_script(source, &overrides).expect_err("refused");
        assert!(
            error.message().contains("not a finite number"),
            "{name} = {value}: {error}"
        );
    }
    let overrides = BTreeMap::from([("w".to_owned(), 250.0)]);
    let error = compile_script(source, &overrides).expect_err("out of range");
    assert!(error.message().contains("outside its range"), "{error}");
}

#[test]
fn a_cylinder_axis_without_a_direction_is_refused() {
    let token = CancellationToken::default();
    for axis in [
        Vector3::new(0.0, 0.0, 0.0),
        Vector3::new(f64::NAN, 0.0, 1.0),
        Vector3::new(0.0, f64::INFINITY, 0.0),
    ] {
        let mut session = Session::new();
        let error = session
            .execute(
                ApiCommand::MakeCylinder {
                    label: "c".to_owned(),
                    center: Point3::new(0.0, 0.0, 0.0),
                    axis,
                    radius: 5.0,
                    height: 10.0,
                },
                &token,
            )
            .expect_err("no direction, no cylinder");
        assert_eq!(error.code, ApiErrorCode::InvalidInput, "{error}");
        assert!(error.message.contains("axis"), "{error}");
        assert!(session.journal.is_empty());
    }
    let mut session = Session::new();
    let outcome = session.run_script(
        "cylinder(axis: [0, 0, 0], radius: 5, height: 10, label: \"c\");",
        &BTreeMap::new(),
        &token,
    );
    assert!(!outcome.succeeded());
}

// ---------------------------------------------------------------------------
// 7. Decompiled arcs rebuild the same arc, to the bit
// ---------------------------------------------------------------------------

/// Whether the decompiler's old way with an angle — the degrees of the
/// radians, written shortest, read back and converted — loses it.
fn naive_trip_loses(degrees: f64) -> bool {
    let radians = degrees.to_radians();
    radians.to_degrees().to_radians().to_bits() != radians.to_bits()
}

#[test]
fn an_arc_in_degrees_decompiles_to_the_same_radians_and_the_same_body() {
    // The first angle in thousandths of a degree from 43 that the old
    // decompiler brought back one bit off; about one in twenty is.
    let degrees = (43_000..44_000)
        .map(|thousandths| f64::from(thousandths) / 1000.0)
        .find(|degrees| naive_trip_loses(*degrees))
        .expect("an angle the naive trip loses");
    let source = format!(
        "let s = sketch(on: \"XY\", entities: [\
line(start: [0, 0], end: [20, 0]), \
arc(center: [0, 0], radius: 20, start_angle: 0, end_angle: {degrees}), \
line(start: [20 * cos({degrees}), 20 * sin({degrees})], end: [0, 0])], label: \"s\");\n\
let e = extrude(sketch: s, distance: 5, label: \"e\");\n"
    );
    let session = run(&source);
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(
        script.contains(&format!("end_angle: {degrees}")),
        "{script}"
    );
    let rebuilt = run(&script);
    assert_eq!(rebuilt.journal.entries[0], session.journal.entries[0]);
    assert_eq!(
        rebuilt.snapshot.semantic_digest(),
        session.snapshot.semantic_digest()
    );
}

#[test]
fn every_arc_angle_survives_decompiling_whether_or_not_it_is_whole_degrees() {
    // Angles in radians from outside any script: most are some number of
    // degrees converted, some are not, and both must come back exactly.
    let entities = (1..=400)
        .map(|index| {
            let angle = f64::from(index) * 0.013_7 - 2.5;
            SketchEntity::Arc {
                center: Point2::new(0.0, 0.0),
                radius: 1.0 + f64::from(index),
                start_angle: angle,
                end_angle: angle + 0.5,
            }
        })
        .collect::<Vec<_>>();
    let command = ApiCommand::Sketch {
        label: "arcs".to_owned(),
        on: SketchPlane::XY,
        entities,
        constraints: Vec::new(),
    };
    let mut session = Session::new();
    session
        .execute(command.clone(), &CancellationToken::default())
        .unwrap();
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(script.contains("_angle: "), "most angles are degrees");
    assert!(script.contains("_radians: "), "some angles are not");
    let program = compile_program(&script, &BTreeMap::new()).unwrap();
    assert_eq!(program.commands, [command]);
}

#[test]
fn negative_zero_survives_decompiling() {
    let command = ApiCommand::MakeBox {
        label: "b".to_owned(),
        origin: Point3::new(-0.0, 0.0, -0.0),
        size: [10.0, 10.0, 10.0],
    };
    let mut session = Session::new();
    session
        .execute(command.clone(), &CancellationToken::default())
        .unwrap();
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    let program = compile_program(&script, &BTreeMap::new()).unwrap();
    let ApiCommand::MakeBox { origin, .. } = &program.commands[0] else {
        panic!("{:?}", program.commands);
    };
    assert!(origin.x.is_sign_negative() && origin.z.is_sign_negative());
    assert!(origin.y.is_sign_positive());
}

// ---------------------------------------------------------------------------
// 8. A label that merely starts with the call's label is still scoped
// ---------------------------------------------------------------------------

#[test]
fn labels_are_scoped_by_segment_not_by_letters() {
    let source = r#"
fn plate(label: str) { box(size: [1, 1, 1], label: "base"); }
plate(label: "b");
plate(label: "ba");
fn deep(label: str) { box(size: [1, 1, 1], label: label + "/inner"); box(size: [1, 1, 1], label: label + "_x"); }
deep(label: "d");
"#;
    assert_eq!(
        labels(source),
        ["b/base", "ba/base", "d/inner", "d/d_x"],
        "a label already under the call's is left alone; one that only shares its letters is not"
    );
}

// ---------------------------------------------------------------------------
// 9. Selectors mean what they say
// ---------------------------------------------------------------------------

#[test]
fn the_nearest_edge_is_the_nearest_along_its_length_not_by_its_middle() {
    let session = run("let b = box(size: [100, 10, 10], label: \"b\");\n");
    let bounds = session.snapshot.measures().bounds.unwrap();
    let (min, max) = (bounds.min, bounds.max);
    // Beside the end of the long top-front edge, a millimetre in front of it
    // and half above: the short edges' middles are closer than the long
    // edge's middle, but the long edge itself is nearest.
    let point = Point3::new(min.x + 5.0, min.y - 1.0, max.z + 0.5);
    let selector = EntitySelector::ByGeometry {
        selector: GeometricSelector::NearestTo {
            point,
            kind: artificer_protocol::EntityKind::Edge,
        },
    };
    let edge = resolve_selector(
        &selector,
        &session.snapshot,
        &session.step_order,
        &session.step_reports,
    )
    .unwrap();
    let description = NativeKernel::describe_edge(&session.snapshot, edge).unwrap();
    assert!(
        (description.length - 100.0).abs() < 1.0e-9,
        "{}",
        description.summary
    );
    assert!((description.midpoint.y - min.y).abs() < 1.0e-9);
    assert!((description.midpoint.z - max.z).abs() < 1.0e-9);
}

#[test]
fn an_edge_between_a_face_and_itself_is_an_error() {
    let session = run("let b = box(size: [10, 10, 10], label: \"b\");\n");
    let top = EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(0.0, 0.0, 1.0),
            match_kind: NormalMatch::Closest,
        },
    };
    let selector = EntitySelector::ByGeometry {
        selector: GeometricSelector::EdgeBetween {
            face_a: Box::new(top.clone()),
            face_b: Box::new(top),
        },
    };
    let error = resolve_selector(
        &selector,
        &session.snapshot,
        &session.step_order,
        &session.step_reports,
    )
    .expect_err("one face twice names no edge");
    assert_eq!(error.code, ApiErrorCode::InvalidInput, "{error}");
}

#[test]
fn ordinals_and_region_indices_are_whole_numbers_from_zero() {
    let prelude = "let b = box(size: [10, 10, 10], label: \"b\");\nlet s = sketch(on: \"XY\", entities: [circle(radius: 1)], label: \"s\");\n";
    for tail in [
        "let f = b.face(\"top_face\", ordinal: -1);",
        "let f = b.face(\"top_face\", ordinal: 1.9);",
        "let f = b.edge(\"edge\", ordinal: 0.5);",
        "let e = extrude(sketch: s, regions: [-1, 0], distance: 1, label: \"e\");",
        "let e = extrude(sketch: s, regions: [0, 0.7], distance: 1, label: \"e\");",
        "let e = extrude(sketch: s, regions: 1.5, distance: 1, label: \"e\");",
    ] {
        let error = compile_error(&format!("{prelude}{tail}"));
        assert!(
            error.message().contains("whole number from 0 up"),
            "{tail}: {error}"
        );
    }
    compile_script(
        &format!(
            "{prelude}let f = b.face(\"top_face\", ordinal: 1);\nlet e = extrude(sketch: s, regions: [0], distance: 1, label: \"e\");"
        ),
        &BTreeMap::new(),
    )
    .expect("whole numbers are fine");
}

// ---------------------------------------------------------------------------
// 10. A study or a sweep is sized before it runs
// ---------------------------------------------------------------------------

#[test]
fn a_study_or_a_sweep_past_its_size_is_refused_before_it_runs() {
    let server = SharedSession::new();
    let subjects = (0..=MAX_STUDY_SUBJECTS)
        .map(|index| format!("s{index}"))
        .collect::<Vec<_>>();
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "analysis.interference",
        "params": {"subjects": subjects}
    });
    let error = server
        .handle_request(&request.to_string())
        .error
        .expect("too many subjects");
    assert!(
        error
            .message
            .contains(&format!("at most {MAX_STUDY_SUBJECTS} subjects")),
        "{}",
        error.message
    );

    let steps = vec![serde_json::json!({}); MAX_SWEEP_STEPS + 1];
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "analysis.sweep",
        "params": {"subjects": ["a", "b"], "steps": steps}
    });
    let error = server
        .handle_request(&request.to_string())
        .error
        .expect("too many positions");
    assert!(
        error
            .message
            .contains(&format!("at most {MAX_SWEEP_STEPS} positions")),
        "{}",
        error.message
    );

    let subjects = (0..40).map(|index| format!("s{index}")).collect::<Vec<_>>();
    let steps = vec![serde_json::json!({}); 500];
    let request = serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "analysis.sweep",
        "params": {"subjects": subjects, "steps": steps}
    });
    let error = server
        .handle_request(&request.to_string())
        .error
        .expect("too many measurements");
    assert!(
        error.message.contains(&format!(
            "at most {MAX_SWEEP_MEASUREMENTS} pair measurements"
        )),
        "{}",
        error.message
    );
}

// ---------------------------------------------------------------------------
// 11. Every face of a step decompiles to a script that compiles
// ---------------------------------------------------------------------------

#[test]
fn every_face_of_a_step_decompiles_to_a_selector_a_script_can_read() {
    let token = CancellationToken::default();
    let mut session = Session::new();
    session.execute(make_box("b"), &token).unwrap();
    let sketch = ApiCommand::Sketch {
        label: "s".to_owned(),
        on: SketchPlane::OnFace {
            face: EntitySelector::history_faces("b"),
        },
        entities: vec![SketchEntity::Circle {
            center: Point2::new(0.0, 0.0),
            radius: 1.0,
        }],
        constraints: Vec::new(),
    };
    session.execute(sketch.clone(), &token).unwrap();
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(script.contains("b.faces()"), "{script}");
    let program = compile_program(&script, &BTreeMap::new()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(program.commands[1], sketch);
    // `.faces()` names a set; a role or an ordinal belongs to `.face(...)`.
    let error = compile_error(
        "let b = box(size: [1, 1, 1], label: \"b\");\nlet f = b.faces(\"top_face\");",
    );
    assert!(error.message().contains("takes no arguments"), "{error}");
}

// ---------------------------------------------------------------------------
// The lexer: strings that never close, and strings over several lines
// ---------------------------------------------------------------------------

#[test]
fn a_string_that_never_closes_is_an_error() {
    let error = compile_error("let b = box(size: [1, 1, 1], label: \"b);\n");
    assert!(error.message().contains("never closes"), "{error}");
    assert_eq!(error.location().map(|(line, _)| line), Some(1), "{error}");
    let error = compile_error("let s = \"trailing escape\\");
    assert!(error.message().contains("never closes"), "{error}");
}

#[test]
fn a_newline_inside_a_string_counts_as_a_line() {
    let error = compile_error("let s = \"one\ntwo\";\nlet x = y;\n");
    assert_eq!(error.location(), Some((3, 9)), "{error}");
}

//! Functions, modules and typed parameters in `.art`: the gates from the
//! feature request, held as tests.

use std::collections::BTreeMap;

use artificer_kernel::api::scripting::{
    InlineModules, ScriptError, compile_program, compile_program_with, compile_script,
    script_parameters,
};
use artificer_kernel::api::selectors::{
    EntitySelector, GeometricSelector, NormalMatch, resolve_selector_set,
};
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{EntityKind, EntityRef, SemanticDigest, Vector3};

const STANDOFF_PLATE: &str = include_str!("../examples/standoff_plate.art");

fn digest_of(source: &str, overrides: &BTreeMap<String, f64>) -> SemanticDigest {
    let commands = compile_script(source, overrides).unwrap_or_else(|error| panic!("{error}"));
    let mut session = Session::new();
    for command in commands {
        session
            .execute(command, &CancellationToken::default())
            .unwrap_or_else(|error| panic!("{error}"));
    }
    session.snapshot.semantic_digest()
}

fn location_of(error: &ScriptError) -> (usize, usize) {
    error
        .location()
        .unwrap_or_else(|| panic!("no location on: {error}"))
}

#[test]
fn a_function_builds_the_same_body_as_its_inlined_steps() {
    let with_function = "\
fn post(on: face, at: [f64; 2], d: f64, h: f64, label: str) -> body {
    let s = sketch(on: on, entities: [circle(center: at, diameter: d)], label: \"s\");
    let p = extrude(sketch: s, distance: h, operation: \"add\", label: label);
    return p with faces { top: p.face(\"end_face\") };
}
let plate = box(size: [60, 40, 5], label: \"plate\");
let a = post(on: plate.face(\"top_face\"), at: [15, 0], d: 8, h: 6, label: \"a\");
let b = post(on: plate.face(\"top_face\"), at: [-15, 0], d: 8, h: 6, label: \"b\");
drill(face: a.top, center: [0, 0], diameter: 3, depth: 6, label: \"hole\");
";
    let inlined = "\
let plate = box(size: [60, 40, 5], label: \"plate\");
let sa = sketch(on: plate.face(\"top_face\"), entities: [circle(center: [15, 0], diameter: 8)], label: \"sa\");
let a = extrude(sketch: sa, distance: 6, operation: \"add\", label: \"a\");
let sb = sketch(on: plate.face(\"top_face\"), entities: [circle(center: [-15, 0], diameter: 8)], label: \"sb\");
let b = extrude(sketch: sb, distance: 6, operation: \"add\", label: \"b\");
drill(face: a.face(\"end_face\"), center: [0, 0], diameter: 3, depth: 6, label: \"hole\");
";
    assert_eq!(
        digest_of(with_function, &BTreeMap::new()),
        digest_of(inlined, &BTreeMap::new())
    );

    // The steps a function builds are labelled under the call's label,
    // and the step carrying the call's own label is the call's step.
    let program = compile_program(with_function, &BTreeMap::new()).unwrap();
    let labels: Vec<&str> = program
        .commands
        .iter()
        .map(|command| command.label())
        .collect();
    assert_eq!(labels, ["plate", "a/s", "a", "b/s", "b", "hole"]);
    // The exported face is recorded under the binding's name.
    assert!(program.names.iter().any(|(name, _)| name == "a.top"));
}

#[test]
fn exported_faces_resolve_after_later_steps_modify_the_body() {
    let mut session = Session::new();
    let outcome = session.run_script(
        STANDOFF_PLATE,
        &BTreeMap::new(),
        &CancellationToken::default(),
    );
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let labels: Vec<&str> = session.step_order.iter().map(String::as_str).collect();
    assert_eq!(
        &labels[..4],
        [
            "plate",
            "standoff_0/boss/profile",
            "standoff_0/boss",
            "standoff_0/hole"
        ]
    );
    // Add a step after every standoff, then bind one and read its face:
    // the exported selector is a history selector, so it still finds the
    // boss top through everything drilled since.
    let source = format!(
        "{STANDOFF_PLATE}
let s = standoff(on: plate_top, at: [0, 0], height: standoff_height, hole: screw, label: \"centre\");
drill(face: plate_top, center: [0, 20], diameter: 2, depth: plate_thickness, label: \"extra\");
"
    );
    let mut session = Session::new();
    let outcome = session.run_script(&source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let report = session.report();
    let top = report
        .names
        .iter()
        .find(|named| named.name == "s.top")
        .expect("the exported face is a name of the body");
    assert_eq!(top.kind, EntityKind::Face);
    assert!(
        top.summary
            .starts_with("planar, facing up, one hole, centre (40.0, 30.0, 15.0)"),
        "{}",
        top.summary
    );
}

#[test]
fn parameters_list_with_units_ranges_and_descriptions_and_round_trip() {
    let parameters = script_parameters(STANDOFF_PLATE).unwrap();
    let screw = parameters
        .iter()
        .find(|parameter| parameter.name == "screw")
        .unwrap();
    assert_eq!(screw.param_type, "f64");
    assert_eq!(screw.unit.as_deref(), Some("mm"));
    assert_eq!((screw.min, screw.max), (Some(2.0), Some(8.0)));
    assert_eq!(screw.default, Some(3.0));
    assert_eq!(screw.default_text, "3");
    assert_eq!(
        screw.description.as_deref(),
        Some("screw clearance hole diameter")
    );
    assert_eq!(screw.line, 13);

    // Overriding every parameter with its own default reproduces the body.
    let defaults: BTreeMap<String, f64> = parameters
        .iter()
        .filter_map(|parameter| {
            parameter
                .default
                .map(|value| (parameter.name.clone(), value))
        })
        .collect();
    assert_eq!(
        digest_of(STANDOFF_PLATE, &defaults),
        digest_of(STANDOFF_PLATE, &BTreeMap::new())
    );
    let program = compile_program(STANDOFF_PLATE, &BTreeMap::new()).unwrap();
    assert_eq!(program.parameters, defaults);

    // An override outside the range is refused, naming the range.
    let error = compile_script(
        STANDOFF_PLATE,
        &BTreeMap::from([("screw".to_owned(), 20.0)]),
    )
    .expect_err("out of range");
    assert!(
        error.message().contains("outside its range 2..8"),
        "{error}"
    );
    assert_eq!(location_of(&error).0, 13);

    // Typed parameters: an int refuses a fraction, a bool takes 0 or 1, a
    // string cannot be overridden.
    let typed = "\
param count: int in 1..8 = 4 \"how many\";
param mirrored: bool = false;
param name: str = \"plate\";
let b = box(size: [count * 10, 10, 10], label: name + \"_body\");
";
    let listed = script_parameters(typed).unwrap();
    assert_eq!(listed[0].param_type, "int");
    assert_eq!(listed[1].param_type, "bool");
    assert_eq!(listed[1].default_text, "false");
    assert_eq!(listed[2].param_type, "str");
    assert_eq!(listed[2].default, None);
    assert_eq!(listed[2].default_text, "plate");
    let error =
        compile_script(typed, &BTreeMap::from([("count".to_owned(), 2.5)])).expect_err("not whole");
    assert!(error.message().contains("not a whole number"), "{error}");
    let error =
        compile_script(typed, &BTreeMap::from([("name".to_owned(), 1.0)])).expect_err("a string");
    assert!(error.message().contains("set it in the script"), "{error}");
    let program = compile_program(typed, &BTreeMap::from([("mirrored".to_owned(), 1.0)])).unwrap();
    assert_eq!(program.parameters["mirrored"], 1.0);
    assert_eq!(program.commands[0].label(), "plate_body");
}

#[test]
fn unbound_names_arity_types_recursion_and_cycles_refuse_with_locations() {
    let unbound = "let b = box(size: [10, 10, 10], label: \"b\");\nlet c = widthh * 2;\n";
    let error = compile_script(unbound, &BTreeMap::new()).expect_err("unbound");
    assert!(
        error.message().contains("Undefined identifier `widthh`"),
        "{error}"
    );
    assert_eq!(location_of(&error), (2, 9));

    let arity = "fn f(a: f64, b: f64) -> f64 { return a + b; }\nlet x = f(1, 2, 3);\n";
    let error = compile_script(arity, &BTreeMap::new()).expect_err("too many");
    assert!(
        error.message().contains("takes 2 arguments, got 3"),
        "{error}"
    );
    assert_eq!(location_of(&error), (2, 9));

    let unknown_argument = "fn f(a: f64) -> f64 { return a; }\nlet x = f(b: 1);\n";
    let error = compile_script(unknown_argument, &BTreeMap::new()).expect_err("no such argument");
    assert!(error.message().contains("has no argument `b`"), "{error}");

    let missing = "fn f(a: f64, b: f64) -> f64 { return a + b; }\nlet x = f(a: 1);\n";
    let error = compile_script(missing, &BTreeMap::new()).expect_err("missing");
    assert!(error.message().contains("requires `b`"), "{error}");

    let wrong_type = "fn f(on: face) -> face { return on; }\nlet x = f(on: 3);\n";
    let error = compile_script(wrong_type, &BTreeMap::new()).expect_err("wrong type");
    assert!(
        error
            .message()
            .contains("`on` expects face, got the number 3"),
        "{error}"
    );

    let wrong_return = "fn f() -> body { return 3; }\nlet x = f();\n";
    let error = compile_script(wrong_return, &BTreeMap::new()).expect_err("wrong return");
    assert!(
        error
            .message()
            .contains("declared to return body, but returned the number 3"),
        "{error}"
    );

    let recursion = "fn f(n: f64) -> f64 { return f(n: n - 1); }\nlet x = f(n: 3);\n";
    let error = compile_script(recursion, &BTreeMap::new()).expect_err("recursion");
    assert!(
        error
            .message()
            .contains("Recursion is not supported: f -> f"),
        "{error}"
    );
    assert_eq!(location_of(&error), (1, 30));

    let mutual = "fn f() -> f64 { return g(); }\nfn g() -> f64 { return f(); }\nlet x = f();\n";
    let error = compile_script(mutual, &BTreeMap::new()).expect_err("mutual recursion");
    assert!(error.message().contains("f -> g -> f"), "{error}");

    let builtin = "fn box(size: [f64; 3]) -> body { return 1; }\n";
    let error = compile_script(builtin, &BTreeMap::new()).expect_err("builtin");
    assert!(error.message().contains("built-in"), "{error}");

    let modules = InlineModules::new(BTreeMap::from([
        (
            "a.art".to_owned(),
            "use \"b.art\";\nfn fa() -> f64 { return 1; }\n".to_owned(),
        ),
        (
            "b.art".to_owned(),
            "use \"a.art\";\nfn fb() -> f64 { return 2; }\n".to_owned(),
        ),
    ]));
    let error =
        compile_program_with("use \"a.art\";\n", &BTreeMap::new(), &modules).expect_err("cycle");
    assert!(
        error
            .message()
            .contains("Import cycle: a.art -> b.art -> a.art"),
        "{error}"
    );
    assert_eq!(location_of(&error), (1, 1));

    let error = compile_program("use \"missing.art\";\n", &BTreeMap::new()).expect_err("no host");
    assert!(error.message().contains("does not load modules"), "{error}");
}

#[test]
fn modules_share_functions_and_constants_and_build_nothing_themselves() {
    let library = "\
param wall: f64 = 3;
let bore = 12;
fn pillar(h: f64, label: str) -> body {
    let p = cylinder(diameter: bore + wall * 2, height: h, label: label);
    return p with faces { top: p.face(\"top\") };
}
";
    let modules = InlineModules::new(BTreeMap::from([(
        "lib/pillar.art".to_owned(),
        library.to_owned(),
    )]));
    let script = "\
use \"lib/pillar.art\";
use \"lib/pillar.art\";
let p = pillar(h: bore * 2, label: \"p\");
let q = box(size: [wall, wall, wall], label: \"q\");
";
    let program = compile_program_with(script, &BTreeMap::new(), &modules).unwrap();
    assert_eq!(program.commands.len(), 2);
    assert_eq!(program.commands[0].label(), "p");
    assert!(program.names.iter().any(|(name, _)| name == "p.top"));
    // The module's parameter takes an override like the script's own.
    let program = compile_program_with(
        script,
        &BTreeMap::from([("wall".to_owned(), 5.0)]),
        &modules,
    )
    .unwrap();
    assert_eq!(program.parameters["wall"], 5.0);

    let building = InlineModules::new(BTreeMap::from([(
        "bad.art".to_owned(),
        "let b = box(size: [1, 1, 1], label: \"b\");\n".to_owned(),
    )]));
    let error = compile_program_with("use \"bad.art\";\n", &BTreeMap::new(), &building)
        .expect_err("a module that builds");
    assert!(
        error
            .message()
            .contains("A module builds nothing at its top level"),
        "{error}"
    );
    assert!(error.message().contains("In module bad.art"), "{error}");
}

#[test]
fn arrays_index_and_functions_without_a_label_scope_by_call_count() {
    let script = "\
let sizes = [[10, 10, 10], [20, 20, 20]];
fn block(size: [f64; 3]) -> body {
    return box(size: size, label: \"block\");
}
block(size: sizes[0]);
block(size: sizes[1]);
";
    let program = compile_program(script, &BTreeMap::new()).unwrap();
    let labels: Vec<&str> = program
        .commands
        .iter()
        .map(|command| command.label())
        .collect();
    assert_eq!(labels, ["block_1/block", "block_2/block"]);

    let error = compile_script("let a = [1, 2];\nlet b = a[2];\n", &BTreeMap::new())
        .expect_err("out of range");
    assert!(
        error
            .message()
            .contains("Index 2 is outside the array of 2 items"),
        "{error}"
    );
    assert_eq!(location_of(&error), (2, 10));
}

#[test]
fn numbers_take_an_exponent_and_a_huge_one_is_the_kernels_to_refuse() {
    let parameters = script_parameters(
        "param wall: f64 = 1e-3;\nparam big: f64 = 2.5E+4;\nparam e = 2;\nparam x = 3 * e + 1e1;\nparam y = 2.5E+4 / 1e3;\n",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let defaults: Vec<(&str, f64)> = parameters
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter.default.unwrap()))
        .collect();
    assert_eq!(
        defaults,
        [
            ("wall", 0.001),
            ("big", 25_000.0),
            ("e", 2.0),
            ("x", 16.0),
            ("y", 25.0)
        ]
    );

    // A coordinate the size of a light-year parses; it is the kernel, not
    // the parser, that refuses it.
    let commands = compile_script(
        "let b = box(size: [1e12, 1, 1], label: \"b\");\n",
        &BTreeMap::new(),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let mut session = Session::new();
    let error = session
        .execute(
            commands.into_iter().next().unwrap(),
            &CancellationToken::default(),
        )
        .expect_err("the kernel bounds coordinates");
    assert!(error.to_string().contains("coordinate envelope"), "{error}");
}

#[test]
fn parse_errors_name_tokens_as_they_are_written() {
    let error = |source: &str| compile_script(source, &BTreeMap::new()).expect_err(source);

    let unclosed = error("let x = (1 + 2;\n");
    assert_eq!(unclosed.message(), "expected `)` but found `;` at 1:15");
    assert_eq!(location_of(&unclosed), (1, 15));

    let eof = error("let a = [1, 2\n\n");
    assert_eq!(eof.message(), "expected `]` but found end of file at 3:1");
    assert_eq!(location_of(&eof), (3, 1));

    let name = error("let a = [1 x];\n");
    assert_eq!(name.message(), "expected `]` but found `x` at 1:12");

    let stray = error("let a = ];\n");
    assert_eq!(stray.message(), "unexpected `]` at 1:9");

    // A string holding " at " does not move the location.
    let text = error("let a = [1 \"cut at 3:4\"];\n");
    assert_eq!(
        text.message(),
        "expected `]` but found `\"cut at 3:4\"` at 1:12"
    );
    assert_eq!(location_of(&text), (1, 12));
}

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

fn edges_of(session: &Session, selector: &EntitySelector) -> Vec<EntityRef> {
    resolve_selector_set(
        selector,
        &session.snapshot,
        &session.step_order,
        &session.step_reports,
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

fn edge_summary(session: &Session, edge: EntityRef) -> String {
    NativeKernel::describe_edge(&session.snapshot, edge)
        .unwrap()
        .summary
}

fn top_face() -> EntitySelector {
    EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(0.0, 0.0, 1.0),
            match_kind: NormalMatch::Closest,
        },
    }
}

#[test]
fn a_face_selector_names_its_edges_and_the_blend_is_the_rim_blend() {
    // The top's edges from the face, and the same four named one by one.
    let by_face = "\
let b = box(size: [40, 30, 20], label: \"b\");
fillet(edges: faces(\">Z\").edges(), radius: 2, label: \"soften\");
";
    let by_point = "\
let b = box(size: [40, 30, 20], label: \"b\");
fillet(edges: [nearest(point: [20, 0, 20], kind: \"edge\"), nearest(point: [20, 30, 20], kind: \"edge\"),
               nearest(point: [0, 15, 20], kind: \"edge\"), nearest(point: [40, 15, 20], kind: \"edge\")],
       radius: 2, label: \"soften\");
";
    let face = run(by_face);
    let point = run(by_point);
    assert_eq!(
        face.snapshot.semantic_digest(),
        point.snapshot.semantic_digest()
    );
    assert_eq!(face.snapshot.counts(), point.snapshot.counts());
    let box_volume = 40.0 * 30.0 * 20.0;
    let volume = face.snapshot.measures().volume;
    assert!(volume < box_volume && volume > 0.9 * box_volume, "{volume}");
    assert_eq!(
        face.step_reports["soften"].rung.as_deref(),
        Some("edge-finish/rim-loop-blend")
    );

    // The selector is bound to the face, not to a step, so it works inline
    // and through a `let`, and the `let` form is a named edge set.
    let through_let = "\
let b = box(size: [40, 30, 20], label: \"b\");
let top = faces(\">Z\");
fillet(edges: top.edges(), radius: 2, label: \"soften\");
";
    assert_eq!(
        run(through_let).snapshot.semantic_digest(),
        face.snapshot.semantic_digest()
    );
    // Mixed with another set selector in one array.
    let mixed = "\
let b = box(size: [40, 30, 20], label: \"b\");
let top = faces(\">Z\");
chamfer(edges: [top.edges(), edges(\"|Z\")], distance: 1, label: \"break\");
";
    let program = compile_program(mixed, &BTreeMap::new()).unwrap();
    assert!(matches!(
        &program.commands[1],
        artificer_kernel::api::commands::ApiCommand::Chamfer { edges, .. } if edges.len() == 2
    ));
}

#[test]
fn the_rim_of_a_drilled_face_leaves_the_hole_out() {
    let session = run("\
let b = box(size: [40, 30, 20], label: \"b\");
drill(face: faces(\">Z\"), center: [0, 0], diameter: 10, depth: 20, label: \"hole\");
");
    let rim = edges_of(&session, &EntitySelector::rim_of_face(top_face()));
    let all = edges_of(&session, &EntitySelector::edges_of_face(top_face()));
    assert_eq!(rim.len(), 4, "{rim:?}");
    assert_eq!(all.len(), 6, "{all:?}");
    for edge in &rim {
        assert!(all.contains(edge));
        assert!(edge_summary(&session, *edge).starts_with("straight edge"));
    }
    let hole: Vec<_> = all.iter().filter(|edge| !rim.contains(edge)).collect();
    assert_eq!(hole.len(), 2);
    for edge in hole {
        assert!(edge_summary(&session, *edge).starts_with("circular arc"));
    }

    // And the rim blends exactly while the hole stays sharp.
    let session = run("\
let b = box(size: [40, 30, 20], label: \"b\");
drill(face: faces(\">Z\"), center: [0, 0], diameter: 10, depth: 20, label: \"hole\");
fillet(edges: faces(\">Z\").rim(), radius: 2, label: \"soften\");
");
    assert_eq!(
        session.step_reports["soften"].rung.as_deref(),
        Some("edge-finish/rim-loop-blend")
    );
}

#[test]
fn an_edge_spelling_of_a_face_is_that_faces_edges() {
    let script = |selector: &str| {
        format!("let b = box(size: [40, 30, 20], label: \"b\");\nlet rim = {selector};\n")
    };
    let sugar = compile_program(&script("edges(\">Z\")"), &BTreeMap::new()).unwrap();
    let explicit = compile_program(&script("faces(\">Z\").edges()"), &BTreeMap::new()).unwrap();
    assert_eq!(sugar.names, explicit.names);
    assert_eq!(
        sugar.names[0].1,
        EntitySelector::edges_of_face(top_face()),
        "{:?}",
        sugar.names
    );
    // The axis forms keep their meaning.
    let parallel = compile_program(&script("edges(\"|Z\")"), &BTreeMap::new()).unwrap();
    assert_eq!(
        parallel.names[0].1,
        EntitySelector::ByGeometry {
            selector: GeometricSelector::EdgesParallelTo {
                direction: Vector3::new(0.0, 0.0, 1.0),
            },
        }
    );
    let unknown = compile_program(&script("edges(\"sideways\")"), &BTreeMap::new())
        .expect_err("not a selector");
    assert!(
        unknown
            .message()
            .starts_with("Unknown edge selector `sideways`"),
        "{unknown}"
    );
    // Only a face selector has `.edges()` and `.rim()`.
    let number = compile_program("let e = 3;\nlet r = e.rim();\n", &BTreeMap::new())
        .expect_err("a number has no rim");
    assert!(number.message().contains("face selector"), "{number}");
}

#[test]
fn a_step_without_a_role_names_every_crease_edge_it_made() {
    let cylinder = run("let cyl = cylinder(radius: 10, height: 20, label: \"cyl\");\n");
    let rims = edges_of(&cylinder, &EntitySelector::history_edges("cyl"));
    assert_eq!(rims.len(), 4, "{rims:?}");
    for edge in &rims {
        let summary = edge_summary(&cylinder, *edge);
        assert!(summary.starts_with("circular arc"), "{summary}");
    }
    // The seams between the wall's halves are not creases and stay out, so
    // the fillet is the exact rim blend of both rims.
    let rounded = run("\
let cyl = cylinder(radius: 10, height: 20, label: \"cyl\");
fillet(edges: cyl.edges(), radius: 1, label: \"round\");
");
    assert_eq!(
        rounded.step_reports["round"].rung.as_deref(),
        Some("edge-finish/rim-blend")
    );
    // A box still has all twelve, and the role form still counts by ordinal.
    let block = run("let b = box(size: [40, 30, 20], label: \"b\");\n");
    assert_eq!(
        edges_of(&block, &EntitySelector::history_edges("b")).len(),
        12
    );
    let program = compile_program(
        "let b = box(size: [40, 30, 20], label: \"b\");\nlet four = b.edges(\"edge\", count: 4);\n",
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(program.names.is_empty());
    let program = compile_program(
        "let b = box(size: [40, 30, 20], label: \"b\");\nlet all = b.edges();\n",
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(program.names[0].1, EntitySelector::history_edges("b"));
}

#[test]
fn a_revolve_angle_short_of_a_turn_is_a_partial_revolve_either_way() {
    let ring = |angle: f64| {
        run(&format!(
            "let section = sketch(on: \"XZ\", label: \"section\", entities: [
    rect(origin: [10, 0], width: 5, height: 4),
]);
let ring = revolve(sketch: section, axis: [0, 0, 1], angle: {angle}, label: \"ring\");
"
        ))
    };
    let quarter = std::f64::consts::PI * (15.0 * 15.0 - 10.0 * 10.0) * 4.0 / 4.0;
    for (angle, side) in [(90.0, 1.0), (-90.0, -1.0)] {
        let session = ring(angle);
        assert_eq!(
            session.step_reports["ring"].rung.as_deref(),
            Some("revolve/partial-turn")
        );
        let measures = session.snapshot.measures();
        assert!(
            ((measures.volume - quarter) / quarter).abs() < 1.0e-9,
            "{angle}: {} should be {quarter}",
            measures.volume
        );
        let centroid = measures.centroid.expect("a centroid");
        assert!(
            centroid.y * side > 1.0,
            "{angle} degrees about +Z turns towards {side} Y: {centroid:?}"
        );
    }
    let full = ring(360.0);
    assert_eq!(
        full.step_reports["ring"].rung.as_deref(),
        Some("revolve/full-turn")
    );
}

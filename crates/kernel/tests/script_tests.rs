//! Functions, modules and typed parameters in `.art`: the gates from the
//! feature request, held as tests.

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::scripting::{
    InlineModules, ScriptError, compile_program, compile_program_with, compile_script,
    script_parameters,
};
use artificer_kernel::api::session::Session;
use artificer_protocol::{EntityKind, SemanticDigest};

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

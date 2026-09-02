//! Journal to `.art` and back, and the semantic diff: the gates from the
//! feature request, held as tests.

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::decompile::{DecompileOptions, ParamPolicy, decompile_journal};
use artificer_kernel::api::diff::{DiffEntry, ScriptDiff};
use artificer_kernel::api::journal::Journal;
use artificer_kernel::api::scripting::{compile_program, script_parameters};
use artificer_kernel::api::selectors::EntitySelector;
use artificer_kernel::api::server::SharedSession;
use artificer_kernel::api::session::Session;
use artificer_protocol::{Point2, Point3, Tier, Vector3};

const EXAMPLES: &[(&str, &str)] = &[
    (
        "bearing_mount",
        include_str!("../examples/bearing_mount.art"),
    ),
    (
        "filleted_cube",
        include_str!("../examples/filleted_cube.art"),
    ),
    (
        "three_holes_and_cut",
        include_str!("../examples/three_holes_and_cut.art"),
    ),
    ("flanged_hub", include_str!("../examples/flanged_hub.art")),
    (
        "filleted_flange",
        include_str!("../examples/filleted_flange.art"),
    ),
    (
        "standoff_plate",
        include_str!("../examples/standoff_plate.art"),
    ),
];

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

#[test]
fn every_example_decompiles_to_a_script_that_rebuilds_the_same_digest() {
    for (name, source) in EXAMPLES {
        let session = run(source);
        for policy in [ParamPolicy::Dimensions, ParamPolicy::None] {
            let options = DecompileOptions {
                params: policy,
                header: true,
            };
            let script = session
                .to_art(&options)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let rebuilt = run(&script);
            assert_eq!(
                rebuilt.snapshot.semantic_digest(),
                session.snapshot.semantic_digest(),
                "{name} with {policy:?}:\n{script}"
            );
            assert_eq!(rebuilt.step_order, session.step_order, "{name}");
            assert_eq!(rebuilt.tier(), session.tier(), "{name}");
            // Every script name survives the round trip.
            for (script_name, _) in &session.names {
                assert!(
                    rebuilt
                        .names
                        .iter()
                        .any(|(rebuilt_name, _)| rebuilt_name == script_name),
                    "{name}: {script_name} lost"
                );
            }
        }
        // The dimensions became parameters a customizer can list.
        let script = session.to_art(&DecompileOptions::default()).unwrap();
        let parameters = script_parameters(&script).unwrap();
        assert!(!parameters.is_empty(), "{name} has dimensions");
        // Decompiling the journal file itself gives the same steps; only
        // the script's own names are not in a journal.
        let json = session.export_journal().unwrap();
        let journal = Journal::from_json(&json).unwrap();
        let from_journal = decompile_journal(&journal, &DecompileOptions::default()).unwrap();
        assert_eq!(
            from_journal,
            Session::from_journal(&json)
                .unwrap()
                .to_art(&DecompileOptions::default())
                .unwrap()
        );
        assert_eq!(
            run(&from_journal).snapshot.semantic_digest(),
            session.snapshot.semantic_digest(),
            "{name} via journal"
        );
    }
}

#[test]
fn snapshot_bound_references_become_history_selectors() {
    // A session driven through the API with direct references, as a
    // client that resolved selectors itself would build.
    let mut session = Session::new();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::MakeBox {
                label: "block".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [40.0, 30.0, 20.0],
            },
            &token,
        )
        .unwrap();
    let top = session
        .query()
        .entity_info(&EntitySelector::history_face("block", "top_face"))
        .unwrap()
        .entity_ref;
    session
        .execute(
            ApiCommand::DrillHole {
                label: "hole".to_owned(),
                face: EntitySelector::Direct { entity_ref: top },
                center: Point2::new(5.0, 5.0),
                diameter: 6.0,
                depth: 20.0,
            },
            &token,
        )
        .unwrap();
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    // The box reports its top as `face[1]`; the direct reference becomes
    // that history selector.
    assert!(
        script.contains("drill(face: block.face(\"face\", ordinal: 1)"),
        "{script}"
    );
    assert!(script.contains("param hole_diameter: f64 = 6;"), "{script}");
    let rebuilt = run(&script);
    assert_eq!(
        rebuilt.snapshot.semantic_digest(),
        session.snapshot.semantic_digest()
    );
}

#[test]
fn an_approximate_step_is_annotated_and_rebuilds_on_the_same_tier() {
    let session = run(include_str!("../examples/three_holes_and_cut.art"));
    assert_eq!(session.tier(), Tier::Approximate);
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(
        script.contains("// approximate: the faceted tier built this step"),
        "{script}"
    );
    assert!(script.contains("fell to the faceted tier"), "{script}");
    let rebuilt = run(&script);
    assert_eq!(rebuilt.tier(), Tier::Approximate);
    assert_eq!(
        rebuilt.snapshot.semantic_digest(),
        session.snapshot.semantic_digest()
    );
}

#[test]
fn the_diff_of_a_script_against_itself_is_empty() {
    for (name, source) in EXAMPLES {
        let program = compile_program(source, &BTreeMap::new()).unwrap();
        let diff = ScriptDiff::between(&program, &program);
        assert!(diff.is_empty(), "{name}: {:?}", diff.entries);
    }
}

#[test]
fn a_changed_default_and_a_renamed_face_are_two_distinct_entries() {
    let before = "\
param width: f64 = 40;
let b = box(size: [width, 30, 20], label: \"b\");
let lid = faces(\">Z\");
";
    let after = "\
param width: f64 = 50;
let b = box(size: [width, 30, 20], label: \"b\");
let top = faces(\">Z\");
";
    let old = compile_program(before, &BTreeMap::new()).unwrap();
    let new = compile_program(after, &BTreeMap::new()).unwrap();
    let diff = ScriptDiff::between(&old, &new);
    assert_eq!(diff.entries.len(), 3, "{:?}", diff.entries);
    assert!(matches!(
        &diff.entries[0],
        DiffEntry::ParameterChanged { name, old, new } if name == "width" && *old == 40.0 && *new == 50.0
    ));
    // The width flows into the box, which is a changed step.
    assert!(matches!(
        &diff.entries[1],
        DiffEntry::StepChanged { label, fields, .. } if label == "b" && fields[0].field == "size"
    ));
    assert!(matches!(
        &diff.entries[2],
        DiffEntry::NameRenamed { old, new } if old == "lid" && new == "top"
    ));

    // Added, removed and moved steps, and a name that now selects
    // differently.
    let before = "\
let a = box(size: [1, 1, 1], label: \"a\");
let b = box(size: [2, 2, 2], label: \"b\");
let c = box(size: [3, 3, 3], label: \"c\");
let top = faces(\">Z\");
";
    let after = "\
let c = box(size: [3, 3, 3], label: \"c\");
let a = box(size: [1, 1, 1], label: \"a\");
let d = box(size: [4, 4, 4], label: \"d\");
let top = faces(\"<Z\");
";
    let diff = ScriptDiff::between(
        &compile_program(before, &BTreeMap::new()).unwrap(),
        &compile_program(after, &BTreeMap::new()).unwrap(),
    );
    let lines = diff.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("step \"b\" (make_box) removed")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("step \"d\" (make_box) added")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line.contains("moved")), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line == "name top now selects differently"),
        "{lines:?}"
    );
    // And the diff round-trips through JSON for the wire.
    let json = serde_json::to_string(&diff).unwrap();
    assert_eq!(serde_json::from_str::<ScriptDiff>(&json).unwrap(), diff);
}

#[test]
fn the_server_decompiles_and_diffs_over_json_rpc() {
    let server = SharedSession::new();
    let run = server.handle_request(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"script.run","params":{{"source":{}}}}}"#,
        serde_json::to_string(EXAMPLES[1].1).unwrap()
    ));
    assert_eq!(run.error, None);
    let script = server.handle_request(r#"{"jsonrpc":"2.0","id":2,"method":"journal.art"}"#);
    assert_eq!(script.error, None);
    let script = script.result.unwrap().as_str().unwrap().to_owned();
    assert!(script.starts_with("// Decompiled from an Artificer session journal."));
    let rebuilt = run_script_source(&script);
    assert_eq!(
        rebuilt.snapshot.semantic_digest(),
        run_script_source(EXAMPLES[1].1).snapshot.semantic_digest()
    );

    let diff = server.handle_request(&format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"script.diff","params":{{"a":{},"b":{}}}}}"#,
        serde_json::to_string(EXAMPLES[1].1).unwrap(),
        serde_json::to_string(&script).unwrap()
    ));
    assert_eq!(diff.error, None);
    let diff: ScriptDiff = serde_json::from_value(diff.result.unwrap()).unwrap();
    // The decompiled script names its parameters after the dimensions;
    // the steps and what they build are the same.
    assert!(!diff.is_empty());
    assert!(
        diff.entries.iter().all(|entry| matches!(
            entry,
            DiffEntry::ParameterAdded { .. } | DiffEntry::ParameterRemoved { .. }
        )),
        "{:?}",
        diff.lines()
    );
}

fn run_script_source(source: &str) -> Session {
    run(source)
}

#[test]
fn the_named_selector_forms_reach_every_geometric_selector() {
    // A block with a boss on top, so the bottom is the one largest face.
    let script = "\
let b = box(size: [40, 30, 20], label: \"b\");
let lean = faces(direction: [1, 0, 1], match: \"closest\");
let big = faces(metric: \"area\", extremum: \"max\");
let seam = edge_between(a: faces(\"<Z\"), b: faces(\">X\"));
let s = sketch(on: faces(\">Z\"), entities: [circle(center: [0, 0], radius: 5)], label: \"s\");
let boss = extrude(sketch: s, distance: 10, operation: \"add\", label: \"boss\");
";
    let program = compile_program(script, &BTreeMap::new()).unwrap();
    assert_eq!(program.names.len(), 3);
    let session = run(script);
    let report = session.report();
    let named: Vec<&str> = report
        .names
        .iter()
        .map(|named| named.name.as_str())
        .collect();
    for name in ["lean", "big", "seam"] {
        assert!(named.contains(&name), "{name} did not resolve: {named:?}");
    }
    let big = report
        .names
        .iter()
        .find(|named| named.name == "big")
        .unwrap();
    assert!(
        big.summary.starts_with("planar, facing down"),
        "{}",
        big.summary
    );
    // And they survive a round trip through the decompiler.
    let script = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(
        script.contains("faces(direction: [1, 0, 1], match: \"closest\")"),
        "{script}"
    );
    assert!(
        script.contains("faces(metric: \"area\", extremum: \"max\")"),
        "{script}"
    );
    assert!(
        script.contains("edge_between(a: faces(\"<Z\"), b: faces(\">X\"))"),
        "{script}"
    );
    let rebuilt = run(&script);
    assert_eq!(rebuilt.names.len(), 3);
    assert_eq!(
        rebuilt.snapshot.semantic_digest(),
        session.snapshot.semantic_digest()
    );
    let _ = Vector3::new(0.0, 0.0, 1.0);
}

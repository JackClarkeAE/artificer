//! The session report and the probes: what a verification-driven caller
//! reads, and the promise that reading changes nothing.

use std::collections::{BTreeMap, BTreeSet};

use artificer_kernel::CancellationToken;
use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::probe::{ProbeRequest, probe};
use artificer_kernel::api::query::MeasureTarget;
use artificer_kernel::api::report::{
    FailurePhase, NameSource, REPORT_SCHEMA_VERSION, RunStatus, SessionReport,
};
use artificer_kernel::api::selectors::{EntitySelector, GeometricSelector, NormalMatch};
use artificer_kernel::api::server::SharedSession;
use artificer_kernel::api::session::Session;
use artificer_protocol::{EntityKind, Point3, Tier, Vector3};

const FLANGED_HUB: &str = include_str!("../examples/flanged_hub.art");
const SCHEMA: &str = include_str!("../../../docs/report-schema.json");

fn face_toward(x: f64, y: f64, z: f64) -> EntitySelector {
    EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(x, y, z),
            match_kind: NormalMatch::Closest,
        },
    }
}

fn box_session() -> Session {
    let mut session = Session::new();
    session
        .execute(
            ApiCommand::MakeBox {
                label: "block".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [40.0, 30.0, 20.0],
            },
            &CancellationToken::default(),
        )
        .expect("box");
    session
}

#[test]
fn every_built_step_names_its_rung_and_tier() {
    let mut session = Session::new();
    let outcome = session.run_script(FLANGED_HUB, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let report = session.report();
    assert_eq!(report.schema_version, REPORT_SCHEMA_VERSION);
    assert_eq!(report.status, RunStatus::Ok);
    assert_eq!(report.tier, Tier::Exact);
    assert_eq!(report.parameters["bolt_count"], 4.0);

    let rungs: BTreeMap<&str, Option<&str>> = report
        .steps
        .iter()
        .map(|step| (step.label.as_str(), step.rung.as_deref()))
        .collect();
    assert_eq!(rungs["section"], None, "a sketch builds nothing");
    assert_eq!(rungs["hub"], Some("revolve/full-turn"));
    assert_eq!(rungs["hub_rim"], Some("edge-finish/rim-blend"));
    assert_eq!(rungs["bolt_0"], Some("face-feature/exact-prism"));
    assert!(report.steps.iter().all(|step| step.tier == Tier::Exact));
    // Every step's volume is the exact measure of the body it left.
    let hub = report
        .steps
        .iter()
        .find(|step| step.label == "hub")
        .unwrap();
    assert!(
        hub.volume > 80_000.0 && hub.volume < 90_000.0,
        "{}",
        hub.volume
    );
    assert!(
        hub.entities
            .iter()
            .all(|entity| { matches!(entity.kind, EntityKind::Face | EntityKind::Edge) })
    );

    let body = report.body.as_ref().expect("a body");
    assert_eq!(body.approximate_feature_count, 0);
    assert_eq!(body.surfaces.planes, 3);
    assert_eq!(body.surfaces.cylinders, 14);
    assert_eq!(body.surfaces.tori, 4);
    assert_eq!(body.faces.len() as u64, body.surfaces.total());
    assert_eq!(body.faces.len() as u64, body.topology.faces);
    assert_eq!(body.edges.len() as u64, body.topology.edges);

    let flange_top = report
        .names
        .iter()
        .find(|named| named.name == "flange_top")
        .expect("the script's name");
    assert_eq!(flange_top.source, NameSource::Script);
    assert_eq!(flange_top.kind, EntityKind::Face);
    assert!(
        flange_top.summary.starts_with("planar, facing up, 5 holes"),
        "{}",
        flange_top.summary
    );
    let top_face = body
        .faces
        .iter()
        .find(|face| face.description.face == flange_top.entity)
        .unwrap();
    assert!(top_face.names.contains(&"flange_top".to_owned()));
    assert_eq!(top_face.description.loops, 6);
    assert!((top_face.description.centre.z - 8.0).abs() < 1.0e-9);
    // Script names come first; every history name reaches an entity.
    assert_eq!(report.names[0].source, NameSource::Script);
    assert!(
        report
            .names
            .iter()
            .any(|named| named.source == NameSource::History && named.name.starts_with("bolt_0."))
    );

    // The report round-trips through JSON unchanged.
    let json = serde_json::to_string(&report).unwrap();
    let back: SessionReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, report);
}

#[test]
fn a_faceted_step_marks_the_body_approximate() {
    // A cut that crosses earlier holes leaves the exact ladder; the report
    // says so on the step and on the body.
    let source = include_str!("../examples/three_holes_and_cut.art");
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let report = session.report();
    let approximate: Vec<&str> = report
        .steps
        .iter()
        .filter(|step| step.tier == Tier::Approximate)
        .map(|step| step.label.as_str())
        .collect();
    assert!(!approximate.is_empty(), "the crossing cut is faceted");
    for step in report
        .steps
        .iter()
        .filter(|step| step.tier == Tier::Approximate)
    {
        assert!(
            step.rung
                .as_deref()
                .is_some_and(|rung| rung.ends_with("/faceted")),
            "{:?}",
            step.rung
        );
        assert!(
            step.warnings
                .iter()
                .any(|warning| warning.code.ends_with("_FACETED_APPROXIMATION"))
        );
    }
    assert_eq!(report.tier, Tier::Approximate);
    let body = report.body.as_ref().unwrap();
    assert_eq!(body.tier, Tier::Approximate);
    assert_eq!(body.approximate_feature_count as usize, approximate.len());
    assert_eq!(
        body.surfaces.total(),
        body.surfaces.planes,
        "facets are planes"
    );
}

#[test]
fn a_failed_script_reports_the_step_its_line_and_the_kernel_codes() {
    let source = "\
let b = box(size: [10, 10, 10], label: \"b\");
drill(face: faces(\">Z\"), center: [0, 0], diameter: 0.000001, depth: 5, label: \"tiny\");
let never = box(size: [1, 1, 1], label: \"never\");
";
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    let failure = outcome.failure.as_ref().expect("the drill fails");
    assert_eq!(failure.phase, FailurePhase::Execute);
    assert_eq!(failure.label, "tiny");
    assert_eq!(failure.command, "drill_hole");
    assert_eq!(failure.line, Some(2));
    assert_eq!(outcome.results.len(), 1, "the box before it committed");
    let report = session.report_with(outcome.failure);
    assert_eq!(report.status, RunStatus::Failed);
    assert_eq!(report.steps.len(), 1);
    assert!(
        report.body.is_some(),
        "the body built so far is still reported"
    );

    // A script that does not parse fails in the compile phase, with a
    // location and no steps.
    let mut session = Session::new();
    let outcome = session.run_script(
        "let b = box(size: [10, 10, 10], label: \"b\"\n",
        &BTreeMap::new(),
        &CancellationToken::default(),
    );
    let failure = outcome.failure.unwrap();
    assert_eq!(failure.phase, FailurePhase::Compile);
    assert_eq!(failure.command, "script");
    assert!(failure.line.is_some());
}

#[test]
fn probes_answer_exactly_and_leave_the_session_untouched() {
    let mut session = box_session();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::MakeBox {
                label: "tool".to_owned(),
                origin: Point3::new(30.0, 20.0, 10.0),
                size: [40.0, 30.0, 20.0],
            },
            &token,
        )
        .expect("tool box");
    let before = (
        session.snapshot.semantic_digest(),
        session.journal.len(),
        session.step_order.clone(),
        session.undo_stack.len(),
    );

    let volume = probe(
        &session,
        &ProbeRequest::Volume {
            step: Some("block".to_owned()),
        },
    )
    .unwrap();
    assert!((volume.value - 24_000.0).abs() < 1.0e-9);
    assert_eq!(volume.tier, Tier::Exact);
    assert_eq!(volume.unit, "mm^3");

    // The overlap of the two boxes is a 10 × 10 × 10 corner; the probe
    // agrees with a committed intersection to the kernel's agreement.
    let overlap = probe(
        &session,
        &ProbeRequest::IntersectionVolume {
            a: "block".to_owned(),
            b: "tool".to_owned(),
        },
    )
    .unwrap();
    assert!((overlap.value - 1_000.0).abs() < 1.0e-9, "{overlap:?}");
    assert_eq!(overlap.tier, Tier::Exact);
    let mut committed = Session::new();
    for command in [
        ApiCommand::MakeBox {
            label: "block".to_owned(),
            origin: Point3::new(0.0, 0.0, 0.0),
            size: [40.0, 30.0, 20.0],
        },
        ApiCommand::MakeBox {
            label: "tool".to_owned(),
            origin: Point3::new(30.0, 20.0, 10.0),
            size: [40.0, 30.0, 20.0],
        },
        ApiCommand::BooleanIntersection {
            label: "overlap".to_owned(),
            target: "block".into(),
            tool: "tool".into(),
        },
    ] {
        committed
            .execute(command, &token)
            .expect("committed boolean");
    }
    assert!((committed.snapshot.measures().volume - overlap.value).abs() < 1.0e-9);

    let far = probe(
        &session,
        &ProbeRequest::IntersectionVolume {
            a: "block".to_owned(),
            b: "block".to_owned(),
        },
    )
    .unwrap();
    assert!(
        (far.value - 24_000.0).abs() < 1.0e-9,
        "a body overlaps itself"
    );

    let wall = probe(
        &session,
        &ProbeRequest::MinWall {
            step: Some("block".to_owned()),
        },
    )
    .unwrap();
    assert!((wall.value - 20.0).abs() < 1.0e-9, "{wall:?}");
    assert_eq!(wall.tier, Tier::Approximate);

    let inside = probe(
        &session,
        &ProbeRequest::Contains {
            point: Point3::new(35.0, 25.0, 15.0),
            step: None,
        },
    )
    .unwrap();
    assert_eq!(inside.value, 1.0);
    assert_eq!(
        inside.tier,
        Tier::Exact,
        "a polyhedron is contained exactly"
    );
    let outside = probe(
        &session,
        &ProbeRequest::Contains {
            point: Point3::new(-1.0, 25.0, 15.0),
            step: None,
        },
    )
    .unwrap();
    assert_eq!(outside.value, 0.0);

    let area = probe(
        &session,
        &ProbeRequest::Area {
            face: face_toward(0.0, 0.0, 1.0),
        },
    )
    .unwrap();
    assert!((area.value - 1_200.0).abs() < 1.0e-9, "{area:?}");
    assert_eq!(area.tier, Tier::Exact);

    let distance = probe(
        &session,
        &ProbeRequest::Distance {
            from: MeasureTarget::Entity(face_toward(0.0, 0.0, 1.0)),
            to: MeasureTarget::Entity(face_toward(0.0, 0.0, -1.0)),
        },
    )
    .unwrap();
    assert!((distance.value - 20.0).abs() < 1.0e-9, "{distance:?}");
    assert_eq!(distance.tier, Tier::Exact);
    let touching = probe(
        &session,
        &ProbeRequest::Distance {
            from: MeasureTarget::Entity(face_toward(0.0, 0.0, 1.0)),
            to: MeasureTarget::Point(Point3::new(50.0, 35.0, 30.0)),
        },
    )
    .unwrap();
    assert_eq!(touching.value, 0.0, "a point on the face");
    let above = probe(
        &session,
        &ProbeRequest::Distance {
            from: MeasureTarget::Point(Point3::new(50.0, 35.0, 37.0)),
            to: MeasureTarget::Entity(face_toward(0.0, 0.0, 1.0)),
        },
    )
    .unwrap();
    assert!((above.value - 7.0).abs() < 1.0e-9, "{above:?}");

    let after = (
        session.snapshot.semantic_digest(),
        session.journal.len(),
        session.step_order.clone(),
        session.undo_stack.len(),
    );
    assert_eq!(before, after, "probes never move the session");
    assert_eq!(session.report().steps.len(), 2);
}

#[test]
fn probes_on_curved_bodies_say_they_are_approximate() {
    let mut session = Session::new();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::MakeCylinder {
                label: "post".to_owned(),
                center: Point3::new(0.0, 0.0, 0.0),
                axis: Vector3::new(0.0, 0.0, 1.0),
                radius: 10.0,
                height: 30.0,
            },
            &token,
        )
        .expect("cylinder");
    let volume = probe(&session, &ProbeRequest::Volume { step: None }).unwrap();
    assert!((volume.value - std::f64::consts::PI * 100.0 * 30.0).abs() < 1.0e-6);
    assert_eq!(volume.tier, Tier::Exact);
    let wall = probe(
        &session,
        &ProbeRequest::Distance {
            from: MeasureTarget::Point(Point3::new(0.0, 0.0, 15.0)),
            to: MeasureTarget::Entity(EntitySelector::ByGeometry {
                selector: GeometricSelector::NearestTo {
                    point: Point3::new(10.0, 0.0, 15.0),
                    kind: EntityKind::Face,
                },
            }),
        },
    )
    .unwrap();
    assert_eq!(
        wall.tier,
        Tier::Approximate,
        "a cylinder wall is faceted for distance"
    );
    assert!((wall.value - 10.0).abs() < 0.05, "{wall:?}");
    let contains = probe(
        &session,
        &ProbeRequest::Contains {
            point: Point3::new(0.0, 0.0, 15.0),
            step: None,
        },
    )
    .unwrap();
    assert_eq!(contains.value, 1.0);
    assert_eq!(contains.tier, Tier::Approximate);
}

#[test]
fn the_server_reports_probes_and_describes_over_json_rpc() {
    let server = SharedSession::new();
    let run = server.handle_request(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"script.report","params":{{"source":{}}}}}"#,
        serde_json::to_string(FLANGED_HUB).unwrap()
    ));
    assert_eq!(run.error, None);
    let report = run.result.unwrap();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["steps"][1]["rung"], "revolve/full-turn");
    assert_eq!(report["body"]["tier"], "exact");

    let again = server.handle_request(r#"{"jsonrpc":"2.0","id":2,"method":"report"}"#);
    assert_eq!(again.result.unwrap()["steps"].as_array().unwrap().len(), 8);

    let probed = server.handle_request(
        r#"{"jsonrpc":"2.0","id":3,"method":"probe","params":{"probe":"volume","step":"hub"}}"#,
    );
    assert_eq!(probed.error, None);
    let volume = probed.result.unwrap();
    assert_eq!(volume["unit"], "mm^3");
    assert_eq!(volume["tier"], "exact");
    assert!(volume["value"].as_f64().unwrap() > 80_000.0);

    let described = server.handle_request(
        r#"{"jsonrpc":"2.0","id":4,"method":"query.describe","params":{"type":"by_geometry","criterion":"face_by_normal","direction":{"x":0,"y":0,"z":1},"match_kind":"closest"}}"#,
    );
    assert_eq!(described.error, None);
    let face = described.result.unwrap();
    assert_eq!(face["kind"], "face");
    assert_eq!(face["surface"], "plane");
    assert_eq!(face["normal"]["z"], 1.0);

    // A failing script is still a result, not a transport error.
    let failed = server.handle_request(
        r#"{"jsonrpc":"2.0","id":5,"method":"script.report","params":{"source":"let b = box(size: [1, 1, 1], label: \"hub\");"}}"#,
    );
    assert_eq!(failed.error, None);
    let report = failed.result.unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["failure"]["label"], "hub");
    assert_eq!(report["failure"]["code"], "invalid_input");
}

// ---------------------------------------------------------------------------
// The schema
// ---------------------------------------------------------------------------

#[test]
fn the_schema_lists_every_diagnostic_code_the_kernel_can_emit() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    let listed: BTreeSet<String> = schema["$defs"]["diagnostic_code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap().to_owned())
        .collect();
    let mut emitted = BTreeSet::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name == "step_export.rs")
            {
                // The STEP writer's upper-case literals are entity type
                // names in the file it writes, not diagnostic codes.
                continue;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                emitted.extend(uppercase_literals(&source));
            }
        }
    }
    emitted.remove("CARGO_PKG_VERSION");
    let missing: Vec<&String> = emitted.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "codes the kernel emits but docs/report-schema.json does not list: {missing:?}"
    );
    let stale: Vec<&String> = listed.difference(&emitted).collect();
    assert!(
        stale.is_empty(),
        "codes docs/report-schema.json lists that no kernel source emits: {stale:?}"
    );
}

/// Every `"UPPER_CASE"` string literal in a Rust source, the shape every
/// diagnostic code has.
fn uppercase_literals(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut index = 0;
    while let Some(open) = source[index..].find('"') {
        let start = index + open + 1;
        let Some(close) = source[start..].find('"') else {
            break;
        };
        let literal = &source[start..start + close];
        let is_code = literal.len() >= 5
            && literal
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            && bytes[start].is_ascii_uppercase();
        if is_code {
            found.insert(literal.to_owned());
        }
        index = start + close + 1;
    }
    found
}

#[test]
fn reports_conform_to_the_published_schema() {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
    for source in [
        FLANGED_HUB,
        include_str!("../examples/three_holes_and_cut.art"),
        include_str!("../examples/bearing_mount.art"),
        include_str!("../examples/filleted_flange.art"),
        "let b = box(size: [10, 10, 10], label: \"b\");\ndrill(face: faces(\">Z\"), center: [0, 0], diameter: 0.000001, depth: 5, label: \"tiny\");\n",
        "let b = box(size: [10, 10, 10], label: \"b\"\n",
    ] {
        let mut session = Session::new();
        let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
        let report = serde_json::to_value(session.report_with(outcome.failure)).unwrap();
        let mut path = Vec::new();
        let mut problems = Vec::new();
        check(&schema, &schema, &report, &mut path, &mut problems);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}

#[test]
fn interference_studies_conform_to_the_published_schema() {
    // The analysis document is a second published shape, and it earns the
    // same guard: every study the kernel can produce validates, including
    // the ones whose overlap volume the Boolean engine refuses to supply.
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/analysis-schema.json")).unwrap();
    for source in [
        "let plate = box(origin: [-20, -20, 0], size: [40, 40, 10], label: \"plate\");\nlet post = cylinder(center: [0, 0, 12], radius: 5, height: 20, label: \"post\");\nlet pin = cylinder(center: [0, 0, 5], radius: 3, height: 20, label: \"pin\");\n",
        "let a = box(size: [10, 10, 10], label: \"a\");\nlet b = box(origin: [30, 0, 0], size: [10, 10, 10], label: \"b\");\n",
        "let a = box(size: [20, 20, 20], label: \"a\");\nlet b = box(origin: [10, 0, 0], size: [20, 20, 20], label: \"b\");\n",
        "let a = box(size: [20, 20, 20], label: \"a\");\nlet b = box(origin: [20, 0, 0], size: [20, 20, 20], label: \"b\");\n",
    ] {
        let mut session = Session::new();
        let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
        assert!(outcome.succeeded(), "{:?}", outcome.failure);
        let subjects: Vec<String> = session.step_order.clone();
        let study = artificer_kernel::api::analysis::study_session_steps(
            &session,
            &subjects,
            &CancellationToken::default(),
        )
        .expect("a study");
        // Unjudged, and then under every profile the kernel ships: the
        // open-ended one is the case that would publish an infinity if the
        // upper bound were a number rather than an absence.
        let profiles = std::iter::once(None).chain(
            artificer_kernel::api::analysis::BUILT_IN_PROFILES
                .iter()
                .map(|profile| Some(profile.profile())),
        );
        for profile in profiles {
            let mut study = study.clone();
            study.judge(profile);
            let document = serde_json::to_value(study).unwrap();
            let mut path = Vec::new();
            let mut problems = Vec::new();
            check(&schema, &schema, &document, &mut path, &mut problems);
            assert!(problems.is_empty(), "{problems:#?}");
        }
    }
}

/// A small validator for the subset of JSON Schema the report schema uses:
/// types, required and closed property sets, enums and consts, `$ref` into
/// `$defs`, `items`, `oneOf`, and `allOf`.
fn check(
    root: &serde_json::Value,
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &mut Vec<String>,
    problems: &mut Vec<String>,
) {
    use serde_json::Value;
    let location = if path.is_empty() {
        "$".to_owned()
    } else {
        format!("$.{}", path.join("."))
    };
    let here = || location.clone();
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.trim_start_matches("#/$defs/");
        check(root, &root["$defs"][name], value, path, problems);
        return;
    }
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let matching = options
            .iter()
            .filter(|option| {
                let mut inner = Vec::new();
                check(root, option, value, path, &mut inner);
                inner.is_empty()
            })
            .count();
        if matching != 1 {
            problems.push(format!("{}: matches {matching} of oneOf, not one", here()));
        }
    }
    if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
        for option in all {
            check(root, option, value, path, problems);
        }
    }
    if let Some(constant) = schema.get("const")
        && constant != value
    {
        problems.push(format!("{}: {value} is not {constant}", here()));
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        problems.push(format!("{}: {value} is not one of the enum", here()));
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let ok = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_u64() || value.is_i64(),
            "boolean" => value.is_boolean(),
            other => panic!("unexpected type {other}"),
        };
        if !ok {
            problems.push(format!("{}: {value} is not {expected}", here()));
            return;
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
        && let Some(text) = value.as_str()
    {
        // The schema's patterns are hex ids and `word/word` rungs.
        let ok = match pattern {
            "^[0-9a-f]{32}$" => text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit()),
            "^[0-9a-f]{64}$" => text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()),
            "^[a-z-]+/[a-z-]+$" => {
                text.split('/').count() == 2
                    && text
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'/')
            }
            other => panic!("unexpected pattern {other}"),
        };
        if !ok {
            problems.push(format!("{}: {text:?} does not match {pattern}", here()));
        }
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64)
        && let Some(number) = value.as_f64()
        && number < minimum
    {
        problems.push(format!("{}: {number} is below {minimum}", here()));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required {
                let name = name.as_str().unwrap();
                if !object.contains_key(name) {
                    problems.push(format!("{}: missing required {name}", here()));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        let additional = schema.get("additionalProperties");
        for (name, member) in object {
            path.push(name.clone());
            match properties.and_then(|properties| properties.get(name)) {
                Some(property) => check(root, property, member, path, problems),
                None => match additional {
                    Some(Value::Bool(true)) | None => {}
                    Some(Value::Bool(false)) => {
                        problems.push(format!("{}: not a property of the schema", here()));
                    }
                    Some(extra) => check(root, extra, member, path, problems),
                },
            }
            path.pop();
        }
    }
    if let Some(items) = value.as_array()
        && let Some(item_schema) = schema.get("items")
    {
        for (index, item) in items.iter().enumerate() {
            path.push(index.to_string());
            check(root, item_schema, item, path, problems);
            path.pop();
        }
    }
}

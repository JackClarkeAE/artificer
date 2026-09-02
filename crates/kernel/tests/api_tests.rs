use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::export::{export_obj, export_stl_ascii, export_stl_binary};
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::selectors::{EntitySelector, GeometricSelector, NormalMatch};
use artificer_kernel::api::server::SharedSession;
use artificer_kernel::api::session::Session;
use artificer_kernel::api::snapshot::{CameraSpec, SnapshotOptions, SnapshotOutput, StandardView};
use artificer_protocol::{Point2, Point3, Vector3};

#[test]
fn test_session_make_box_and_query() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    let cmd = ApiCommand::MakeBox {
        label: "bracket".to_owned(),
        origin: Point3::new(0.0, 0.0, 0.0),
        size: [100.0, 50.0, 25.0],
    };

    let res = session.execute(cmd, &token).expect("Execute MakeBox");
    assert!(res.success);
    assert_eq!(res.step_label, "bracket");
    assert_eq!(res.topology.solids, 1);
    assert_eq!(res.topology.faces, 6);
    assert_eq!(res.topology.vertices, 8);

    // Query topology
    let query = session.query();
    let bodies = query.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0].topology.faces, 6);

    let bounds = query.bounds().expect("bounds");
    assert_eq!(bounds.min, Point3::new(0.0, 0.0, 0.0));
    assert_eq!(bounds.max, Point3::new(100.0, 50.0, 25.0));
}

#[test]
fn test_history_and_geometric_selectors() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "base".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [60.0, 40.0, 20.0],
            },
            &token,
        )
        .expect("MakeBox");

    // Drill hole using history selector on side/top face (center is at (0,0) in face frame)
    let drill_cmd = ApiCommand::DrillHole {
        label: "hole_1".to_owned(),
        face: EntitySelector::history_face("base", "top_face"),
        center: Point2::new(0.0, 0.0),
        diameter: 10.0,
        depth: 15.0,
    };

    let res = session.execute(drill_cmd, &token).expect("Drill hole");
    assert!(res.success);

    // Geometric selector: face pointing towards +Z
    let geom_sel = EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(0.0, 0.0, 1.0),
            match_kind: NormalMatch::Closest,
        },
    };

    let entity_info = session
        .query()
        .entity_info(&geom_sel)
        .expect("Query face info");
    assert_eq!(entity_info.kind, artificer_protocol::EntityKind::Face);
}

#[test]
fn test_script_compilation_and_execution() {
    let script = r#"
        param width: f64 = 50.0;
        param height: f64 = 30.0;
        param thickness: f64 = 15.0;

        let b = box(origin: [0, 0, 0], size: [width, height, thickness], label: "main_body");
        let top = b.face("top_face");
        drill(face: top, center: [0.0, 0.0], diameter: 8.0, depth: 10.0, label: "bore");
    "#;

    let mut overrides = BTreeMap::new();
    overrides.insert("width".to_owned(), 80.0);

    let commands = compile_script(script, &overrides).expect("Compile script");
    assert_eq!(commands.len(), 2);

    let mut session = Session::new();
    let token = CancellationToken::default();
    for cmd in commands {
        session.execute(cmd, &token).expect("Execute command");
    }

    let bounds = session.query().bounds().expect("Query bounds");
    assert_eq!(bounds.max.x, 80.0);
    assert_eq!(bounds.max.y, 30.0);
    assert_eq!(bounds.max.z, 15.0);
}

#[test]
fn test_snapshot_svg_rendering() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "cube".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [20.0, 20.0, 20.0],
            },
            &token,
        )
        .expect("MakeBox");

    let snap_opts = SnapshotOptions {
        camera: CameraSpec::preset(StandardView::Isometric),
        ..Default::default()
    };

    let output = session.snapshot(snap_opts).expect("Render snapshot");
    let SnapshotOutput::Svg(svg_text) = output else {
        panic!("Expected SVG output");
    };

    assert!(svg_text.contains("<svg"));
    assert!(svg_text.contains("polygon points="));
    assert!(svg_text.contains("</svg>"));
}

#[test]
fn test_export_stl_and_obj() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "part".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [10.0, 10.0, 10.0],
            },
            &token,
        )
        .expect("MakeBox");

    let stl_bin = export_stl_binary(&session.snapshot).expect("Export STL binary");
    assert!(stl_bin.len() >= 84);

    let stl_ascii = export_stl_ascii(&session.snapshot, "cube").expect("Export STL ascii");
    assert!(stl_ascii.contains("solid cube"));
    assert!(stl_ascii.contains("facet normal"));
    assert!(stl_ascii.contains("endsolid cube"));

    let obj = export_obj(&session.snapshot, "cube").expect("Export OBJ");
    assert!(obj.contains("v "));
    assert!(obj.contains("vn "));
    assert!(obj.contains("f "));
}

#[test]
fn test_jsonrpc_server() {
    let server = SharedSession::new();

    // 1. Execute MakeBox via JSON-RPC
    let req = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "method": "execute",
        "params": {
            "type": "make_box",
            "label": "rpc_box",
            "origin": [0.0, 0.0, 0.0],
            "size": [50.0, 25.0, 10.0]
        }
    }"#;

    let resp = server.handle_request(req);
    assert_eq!(resp.error, None);
    let result = resp.result.expect("JSON-RPC result");
    assert_eq!(result["success"], true);
    assert_eq!(result["step_label"], "rpc_box");

    // 2. Query bounds
    let q_req = r#"{
        "jsonrpc": "2.0",
        "id": 2,
        "method": "query.bounds"
    }"#;

    let q_resp = server.handle_request(q_req);
    assert_eq!(q_resp.error, None);
    let bounds = q_resp.result.expect("bounds result");
    assert_eq!(bounds["max"]["x"], 50.0);

    // 3. Snapshot via JSON-RPC
    let s_req = r#"{
        "jsonrpc": "2.0",
        "id": 3,
        "method": "snapshot",
        "params": {}
    }"#;

    let s_resp = server.handle_request(s_req);
    assert_eq!(s_resp.error, None);
    let snap = s_resp.result.expect("snapshot result");
    assert_eq!(snap["format"], "svg");
}

#[test]
fn test_journal_deterministic_replay() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "step1".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [30.0, 20.0, 10.0],
            },
            &token,
        )
        .expect("step1");

    let journal_json = session.export_journal().expect("Export journal");
    let replayed = Session::from_journal(&journal_json).expect("Replay journal");

    assert_eq!(session.snapshot.id(), replayed.snapshot.id());
    assert_eq!(session.snapshot.counts(), replayed.snapshot.counts());
    assert_eq!(session.snapshot.measures(), replayed.snapshot.measures());
}

#[test]
fn test_sketch_and_extrude() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    // 1. Sketch a 30x20 rectangle on the XY plane. A sketch is recorded as
    //    a step of its own and leaves the model empty until it is extruded.
    let sketch_res = session
        .execute(
            ApiCommand::Sketch {
                label: "sk1".to_owned(),
                on: artificer_kernel::api::commands::SketchPlane::XY,
                entities: vec![artificer_kernel::api::commands::SketchEntity::Rectangle {
                    origin: Point2::new(0.0, 0.0),
                    width: 30.0,
                    height: 20.0,
                }],
                constraints: Vec::new(),
            },
            &token,
        )
        .expect("a sketch records as a step");
    assert!(sketch_res.success);
    assert_eq!(sketch_res.topology.solids, 0);
    assert_eq!(session.journal.len(), 1);

    let ext_res = session
        .execute(
            ApiCommand::Extrude {
                label: "ext1".to_owned(),
                sketch: artificer_kernel::api::commands::StepLabel("sk1".to_owned()),
                regions: Vec::new(),
                distance: 15.0,
                operation: artificer_kernel::api::commands::ExtrudeOp::New,
                draft_degrees: 0.0,
            },
            &token,
        )
        .expect("Extrude sketch");

    assert!(ext_res.success);
    assert_eq!(ext_res.topology.faces, 6);
    assert_eq!(ext_res.topology.solids, 1);
}

#[test]
fn test_push_pull() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "box".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [20.0, 20.0, 20.0],
            },
            &token,
        )
        .expect("MakeBox");

    let pp_res = session
        .execute(
            ApiCommand::PushPull {
                label: "pull".to_owned(),
                face: EntitySelector::history_face("box", "top_face"),
                distance: 10.0,
            },
            &token,
        )
        .expect("PushPull");

    assert!(pp_res.success);
    let bounds = session.query().bounds().expect("Bounds");
    assert_eq!(bounds.max.z, 30.0);
}

#[test]
fn test_undo_redo_workflow() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "b".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [10.0, 10.0, 10.0],
            },
            &token,
        )
        .expect("MakeBox");

    let snap_after_box = session.snapshot.id();

    session
        .execute(
            ApiCommand::PushPull {
                label: "p".to_owned(),
                face: EntitySelector::history_face("b", "top_face"),
                distance: 5.0,
            },
            &token,
        )
        .expect("PushPull");

    let snap_after_push = session.snapshot.id();
    assert_ne!(snap_after_box, snap_after_push);

    // Undo
    session.undo().expect("undo");
    assert_eq!(session.snapshot.id(), snap_after_box);

    // Redo
    session.redo().expect("redo");
    assert_eq!(session.snapshot.id(), snap_after_push);
}

#[test]
fn test_measure_query() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    session
        .execute(
            ApiCommand::MakeBox {
                label: "b".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [30.0, 40.0, 0.0],
            },
            &token,
        )
        .ok(); // 0-thickness box might be rejected or accepted, so test point measure

    let query = session.query();
    let m = query
        .measure(
            &artificer_kernel::api::query::MeasureTarget::Point(Point3::new(0.0, 0.0, 0.0)),
            &artificer_kernel::api::query::MeasureTarget::Point(Point3::new(3.0, 4.0, 0.0)),
        )
        .expect("measure points");

    assert!((m.distance - 5.0).abs() < 1e-6);
}

#[test]
fn test_three_holes_and_crossing_side_cut() {
    let mut session = Session::new();
    let token = CancellationToken::default();

    // 1. Make box
    session
        .execute(
            ApiCommand::MakeBox {
                label: "base".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [100.0, 100.0, 40.0],
            },
            &token,
        )
        .expect("MakeBox");

    // 2. Drill 3 holes on top face
    session
        .execute(
            ApiCommand::DrillHole {
                label: "hole_1".to_owned(),
                face: EntitySelector::history_face("base", "top_face"),
                center: Point2::new(-25.0, -25.0),
                diameter: 16.0,
                depth: 40.0,
            },
            &token,
        )
        .expect("Drill hole 1");

    session
        .execute(
            ApiCommand::DrillHole {
                label: "hole_2".to_owned(),
                face: EntitySelector::history_face("base", "top_face"),
                center: Point2::new(25.0, -25.0),
                diameter: 16.0,
                depth: 40.0,
            },
            &token,
        )
        .expect("Drill hole 2");

    session
        .execute(
            ApiCommand::DrillHole {
                label: "hole_3".to_owned(),
                face: EntitySelector::history_face("base", "top_face"),
                center: Point2::new(0.0, 25.0),
                diameter: 16.0,
                depth: 40.0,
            },
            &token,
        )
        .expect("Drill hole 3");

    // 3. Drill crossing hole through the front/side face (-Y)
    session
        .execute(
            ApiCommand::DrillHole {
                label: "side_hole".to_owned(),
                face: EntitySelector::ByGeometry {
                    selector: GeometricSelector::FaceByNormal {
                        direction: Vector3::new(0.0, -1.0, 0.0),
                        match_kind: NormalMatch::Closest,
                    },
                },
                center: Point2::new(0.0, 0.0),
                diameter: 20.0,
                depth: 30.0,
            },
            &token,
        )
        .expect("Drill side hole");

    // 4. Render snapshot
    let snap_res = session
        .snapshot(SnapshotOptions {
            camera: CameraSpec::preset(StandardView::Trimetric),
            format: artificer_kernel::api::snapshot::SnapshotFormat::Svg,
            display_mode: "shaded".to_owned(),
            show_labels: false,
            highlight: vec![],
        })
        .expect("snapshot");

    let SnapshotOutput::Svg(svg) = snap_res else {
        panic!("expected SVG");
    };
    assert!(!svg.is_empty());
    assert!(svg.contains("<svg"));
    println!("Generated SVG snapshot: {} bytes", svg.len());
}

#[test]
fn a_deeply_nested_script_is_an_error_not_a_stack_overflow() {
    let depth = 100_000;
    let source = format!("let x = {}1{};", "(".repeat(depth), ")".repeat(depth));
    let error = artificer_kernel::api::scripting::compile_script(&source, &BTreeMap::new())
        .expect_err("nesting past the limit is refused");
    assert!(error.to_string().contains("nested deeper"), "{error}");
}

#[test]
fn geometric_selectors_round_trip_through_json() {
    let selector = EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(0.0, 0.0, 1.0),
            match_kind: NormalMatch::Closest,
        },
    };
    let json = serde_json::to_string(&selector).expect("serialize");
    assert!(json.contains("\"type\":\"by_geometry\""), "{json}");
    assert!(json.contains("\"criterion\":\"face_by_normal\""), "{json}");
    let back: EntitySelector = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, selector);

    // A journal that names faces geometrically replays.
    let mut session = Session::new();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::MakeBox {
                label: "base".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [40.0, 30.0, 20.0],
            },
            &token,
        )
        .expect("box");
    session
        .execute(
            ApiCommand::DrillHole {
                label: "hole".to_owned(),
                face: selector,
                center: Point2::new(0.0, 0.0),
                diameter: 8.0,
                depth: 20.0,
            },
            &token,
        )
        .expect("drill through a geometrically selected face");
    let journal = session.export_journal().expect("journal");
    let replayed = Session::from_journal(&journal).expect("a geometric journal replays");
    assert_eq!(replayed.snapshot.id(), session.snapshot.id());
}

#[test]
fn a_sketch_with_a_hole_extrudes_into_a_holed_block() {
    let mut session = Session::new();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::Sketch {
                label: "plate".to_owned(),
                on: artificer_kernel::api::commands::SketchPlane::XY,
                entities: vec![
                    artificer_kernel::api::commands::SketchEntity::Rectangle {
                        origin: Point2::new(0.0, 0.0),
                        width: 60.0,
                        height: 40.0,
                    },
                    artificer_kernel::api::commands::SketchEntity::Circle {
                        center: Point2::new(30.0, 20.0),
                        radius: 5.0,
                    },
                    // A triangle drawn as three lines, chained by endpoints.
                    artificer_kernel::api::commands::SketchEntity::Line {
                        start: Point2::new(5.0, 5.0),
                        end: Point2::new(15.0, 5.0),
                    },
                    artificer_kernel::api::commands::SketchEntity::Line {
                        start: Point2::new(15.0, 5.0),
                        end: Point2::new(10.0, 15.0),
                    },
                    artificer_kernel::api::commands::SketchEntity::Line {
                        start: Point2::new(5.0, 5.0),
                        end: Point2::new(10.0, 15.0),
                    },
                ],
                constraints: Vec::new(),
            },
            &token,
        )
        .expect("sketch records");
    let result = session
        .execute(
            ApiCommand::Extrude {
                label: "block".to_owned(),
                sketch: artificer_kernel::api::commands::StepLabel("plate".to_owned()),
                regions: Vec::new(),
                distance: 10.0,
                operation: artificer_kernel::api::commands::ExtrudeOp::New,
                draft_degrees: 0.0,
            },
            &token,
        )
        .expect("a rectangle with a round and a triangular hole extrudes");
    assert!(result.success);
    let expected =
        60.0 * 40.0 * 10.0 - std::f64::consts::PI * 25.0 * 10.0 - 0.5 * 10.0 * 10.0 * 10.0;
    let volume = session.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
}

#[test]
fn an_open_sketch_chain_is_refused_with_its_loose_end_named() {
    let mut session = Session::new();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::Sketch {
                label: "open".to_owned(),
                on: artificer_kernel::api::commands::SketchPlane::XY,
                entities: vec![
                    artificer_kernel::api::commands::SketchEntity::Line {
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(10.0, 0.0),
                    },
                    artificer_kernel::api::commands::SketchEntity::Line {
                        start: Point2::new(10.0, 0.0),
                        end: Point2::new(10.0, 10.0),
                    },
                ],
                constraints: Vec::new(),
            },
            &token,
        )
        .expect("sketch records");
    let error = session
        .execute(
            ApiCommand::Extrude {
                label: "nope".to_owned(),
                sketch: artificer_kernel::api::commands::StepLabel("open".to_owned()),
                regions: Vec::new(),
                distance: 5.0,
                operation: artificer_kernel::api::commands::ExtrudeOp::New,
                draft_degrees: 0.0,
            },
            &token,
        )
        .expect_err("an open chain cannot extrude");
    assert!(error.message.contains("open chain"), "{error}");
}

#[test]
fn labels_must_be_unique_and_boolean_targets_must_exist() {
    let mut session = Session::new();
    let token = CancellationToken::default();
    let make = |label: &str| ApiCommand::MakeBox {
        label: label.to_owned(),
        origin: Point3::new(0.0, 0.0, 0.0),
        size: [10.0, 10.0, 10.0],
    };
    session.execute(make("a"), &token).expect("first box");
    let duplicate = session
        .execute(make("a"), &token)
        .expect_err("a reused label is refused");
    assert!(duplicate.message.contains("already used"), "{duplicate}");

    session.execute(make("b"), &token).expect("second box");
    let missing = session
        .execute(
            ApiCommand::BooleanUnion {
                label: "u".to_owned(),
                target: artificer_kernel::api::commands::StepLabel("typo".to_owned()),
                tool: artificer_kernel::api::commands::StepLabel("b".to_owned()),
            },
            &token,
        )
        .expect_err("a misspelt target is an error, not the current model");
    assert!(missing.message.contains("typo"), "{missing}");
}

#[test]
fn the_server_follows_json_rpc_framing() {
    let server = SharedSession::new();
    // A notification is executed but not answered.
    let notification = r#"{"jsonrpc":"2.0","method":"execute","params":{"type":"make_box","label":"n","origin":{"x":0,"y":0,"z":0},"size":[10,10,10]}}"#;
    assert_eq!(server.handle_message(notification), None);
    // It did run: the bounds are those of the box.
    let bounds = server
        .handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"query.bounds"}"#)
        .expect("a request is answered");
    assert!(bounds.contains("\"max\""), "{bounds}");
    // A batch answers every request that carries an id, as an array.
    let batch = r#"[{"jsonrpc":"2.0","id":2,"method":"query.bounds"},{"jsonrpc":"2.0","method":"query.bounds"},{"jsonrpc":"2.0","id":3,"method":"nope"}]"#;
    let answer = server.handle_message(batch).expect("batch answer");
    let parsed: serde_json::Value = serde_json::from_str(&answer).expect("json");
    let answers = parsed.as_array().expect("an array");
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[1]["error"]["code"], -32601);
    // The protocol version is checked.
    let wrong = server
        .handle_message(r#"{"jsonrpc":"1.0","id":4,"method":"query.bounds"}"#)
        .expect("answered");
    assert!(wrong.contains("-32600"), "{wrong}");
    // Malformed snapshot params are the caller's mistake.
    let bad = server
        .handle_message(
            r#"{"jsonrpc":"2.0","id":5,"method":"snapshot","params":{"camera":"garbage"}}"#,
        )
        .expect("answered");
    assert!(bad.contains("-32602"), "{bad}");
    // Domain errors carry the structured error in `data`.
    let domain = server
        .handle_message(r#"{"jsonrpc":"2.0","id":6,"method":"execute","params":{"type":"make_box","label":"n","origin":{"x":0,"y":0,"z":0},"size":[1,1,1]}}"#)
        .expect("answered");
    let parsed: serde_json::Value = serde_json::from_str(&domain).expect("json");
    assert_eq!(parsed["error"]["code"], -32000);
    assert_eq!(parsed["error"]["data"]["code"], "invalid_input");
}

#[test]
fn a_png_snapshot_is_a_real_png() {
    let mut session = Session::new();
    session
        .execute(
            ApiCommand::MakeBox {
                label: "b".to_owned(),
                origin: Point3::new(0.0, 0.0, 0.0),
                size: [20.0, 20.0, 20.0],
            },
            &CancellationToken::default(),
        )
        .expect("box");
    let options = artificer_kernel::api::snapshot::SnapshotOptions {
        format: artificer_kernel::api::snapshot::SnapshotFormat::Png,
        ..Default::default()
    };
    let output = session.snapshot(options).expect("png");
    let artificer_kernel::api::snapshot::SnapshotOutput::Png(bytes) = output else {
        panic!("a PNG was requested");
    };
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );
    assert_eq!(&bytes[12..16], b"IHDR");
    assert_eq!(&bytes[bytes.len() - 8..bytes.len() - 4], b"IEND");
}

#[test]
fn the_shipped_examples_compile_and_run() {
    for example in [
        "examples/bearing_mount.art",
        "examples/filleted_cube.art",
        "examples/three_holes_and_cut.art",
        "examples/flanged_hub.art",
        "examples/filleted_flange.art",
        "examples/standoff_plate.art",
    ] {
        let source = std::fs::read_to_string(example).expect(example);
        let commands = artificer_kernel::api::scripting::compile_script(&source, &BTreeMap::new())
            .unwrap_or_else(|error| panic!("{example}: {error}"));
        let mut session = Session::new();
        for command in commands {
            session
                .execute(command, &CancellationToken::default())
                .unwrap_or_else(|error| panic!("{example}: {error}"));
        }
        assert!(session.snapshot.counts().solids >= 1, "{example}");
    }
}

#[test]
fn a_drafted_extrusion_lofts_to_the_offset_section_and_only_for_new_bodies() {
    let mut session = Session::new();
    let token = CancellationToken::default();
    session
        .execute(
            ApiCommand::Sketch {
                label: "sk".to_owned(),
                on: artificer_kernel::api::commands::SketchPlane::XY,
                entities: vec![artificer_kernel::api::commands::SketchEntity::Rectangle {
                    origin: Point2::new(0.0, 0.0),
                    width: 20.0,
                    height: 20.0,
                }],
                constraints: Vec::new(),
            },
            &token,
        )
        .expect("sketch");
    let drafted = session
        .execute(
            ApiCommand::Extrude {
                label: "draft".to_owned(),
                sketch: artificer_kernel::api::commands::StepLabel("sk".to_owned()),
                regions: Vec::new(),
                distance: 10.0,
                operation: artificer_kernel::api::commands::ExtrudeOp::New,
                draft_degrees: -10.0,
            },
            &token,
        )
        .expect("a drafted new body");
    assert_eq!(drafted.topology.faces, 6);
    // A 10 degree inward draft over 10 mm pulls each wall in by 10·tan(10°).
    let inset = 10.0 * 10.0_f64.to_radians().tan();
    let top = 20.0 - 2.0 * inset;
    let expected = 10.0 / 3.0 * (400.0 + top * top + (400.0 * top * top).sqrt());
    let volume = session.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "{volume} vs {expected}"
    );
    assert!(matches!(
        session.journal.entries.last().map(|entry| &entry.command),
        Some(ApiCommand::Extrude { draft_degrees, .. }) if *draft_degrees == -10.0
    ));
    let added = session
        .execute(
            ApiCommand::Extrude {
                label: "draft_add".to_owned(),
                sketch: artificer_kernel::api::commands::StepLabel("sk".to_owned()),
                regions: Vec::new(),
                distance: 5.0,
                operation: artificer_kernel::api::commands::ExtrudeOp::Add,
                draft_degrees: 5.0,
            },
            &token,
        )
        .expect_err("add and cut extrusions build straight walls");
    assert!(added.message.contains("new-body"), "{}", added.message);

    // JSON keeps the draft, and omits it when it is zero.
    let json = serde_json::to_string(&ApiCommand::Extrude {
        label: "draft".to_owned(),
        sketch: artificer_kernel::api::commands::StepLabel("sk".to_owned()),
        regions: Vec::new(),
        distance: 10.0,
        operation: artificer_kernel::api::commands::ExtrudeOp::New,
        draft_degrees: 0.0,
    })
    .unwrap();
    assert!(!json.contains("draft_degrees"));
    let parsed: ApiCommand = serde_json::from_str(
        r#"{"type":"extrude","label":"d","sketch":"sk","distance":10.0,"operation":"new","draft_degrees":5.0}"#,
    )
    .unwrap();
    assert!(matches!(parsed, ApiCommand::Extrude { draft_degrees, .. } if draft_degrees == 5.0));
}

#[test]
fn a_script_sketches_extrudes_and_joins_bodies() {
    let script = r#"
        param size: f64 = 20.0;
        let plate = sketch(on: "XY", label: "plate", entities: [
            rect(origin: [0, 0], width: size * 2, height: size),
        ]);
        let base = extrude(sketch: plate, distance: 5, label: "base");
        let boss = cylinder(center: [size, size / 2, 5], radius: 4, height: 10, label: "boss");
        let joined = union(target: base, tool: boss, label: "joined");
        // A face sketch's origin is the face centre: the boss top.
        let pocket = sketch(on: faces(">Z"), label: "pocket", entities: [
            circle(center: [0, 0], radius: 2),
        ]);
        extrude(sketch: pocket, distance: 6, operation: "cut", label: "bore");
    "#;
    let commands = compile_script(script, &BTreeMap::new()).expect("compile");
    assert_eq!(commands.len(), 6);
    let mut session = Session::new();
    for command in commands {
        session
            .execute(command, &CancellationToken::default())
            .expect("execute");
    }
    let volume = session.snapshot.measures().volume;
    let expected =
        40.0 * 20.0 * 5.0 + std::f64::consts::PI * 16.0 * 10.0 - std::f64::consts::PI * 4.0 * 6.0;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
}

#[test]
fn a_script_revolves_a_section_and_drafts_an_extrusion() {
    let script = r#"
        let ring = sketch(on: "XZ", label: "ring", entities: [
            rect(origin: [10, 0], width: 5, height: 4),
        ]);
        let tube = revolve(sketch: ring, axis: [0, 0, 1], label: "tube");
    "#;
    let commands = compile_script(script, &BTreeMap::new()).expect("compile");
    let mut session = Session::new();
    for command in commands {
        session
            .execute(command, &CancellationToken::default())
            .expect("execute");
    }
    let volume = session.snapshot.measures().volume;
    let expected = std::f64::consts::PI * (15.0_f64.powi(2) - 10.0_f64.powi(2)) * 4.0;
    assert!(((volume - expected) / expected).abs() < 1.0e-9, "{volume}");

    let drafted = r#"
        let square = sketch(on: "XY", label: "square", entities: [rect(width: 20, height: 20)]);
        extrude(sketch: square, distance: 10, draft: 5, label: "frustum");
    "#;
    let commands = compile_script(drafted, &BTreeMap::new()).expect("compile");
    assert!(matches!(
        commands[1],
        ApiCommand::Extrude { draft_degrees, .. } if draft_degrees == 5.0
    ));
}

#[test]
fn script_math_builtins_and_pi_evaluate_in_degrees() {
    let script = r#"
        param r: f64 = 10.0;
        let s = box(size: [r * cos(60), sqrt(16) + max(1, 2, 3), round(pi * 2)], label: "b");
    "#;
    let commands = compile_script(script, &BTreeMap::new()).expect("compile");
    let ApiCommand::MakeBox { size, .. } = &commands[0] else {
        panic!("a box");
    };
    assert!((size[0] - 5.0).abs() < 1.0e-12);
    assert!((size[1] - 7.0).abs() < 1.0e-12);
    assert!((size[2] - 6.0).abs() < 1.0e-12);
}

#[test]
fn a_script_error_names_its_line_and_column() {
    let script = "let a = box(size: [1, 2, 3], label: \"a\");\n\nlet b = cylinder(radius: 2);\n";
    let error = compile_script(script, &BTreeMap::new()).expect_err("height is missing");
    assert_eq!(error.location(), Some((3, 9)), "{error}");
    assert!(error.to_string().contains("line 3"), "{error}");
    assert!(error.message().contains("height"), "{error}");

    let unparsable = "let a = box(size: [1, 2, 3]\nlet b = 4;";
    let error = compile_script(unparsable, &BTreeMap::new()).expect_err("unclosed call");
    assert!(error.location().is_some(), "{error}");
}

#[test]
fn script_parameters_list_in_order_with_evaluated_defaults() {
    let script = r#"
        param width: f64 = 40.0;
        param half = width / 2;
        param label_only: f64 = 3;
        let b = box(size: [width, half, 1], label: "b");
    "#;
    let parameters = artificer_kernel::api::scripting::script_parameters(script).expect("params");
    let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["width", "half", "label_only"]);
    assert_eq!(parameters[1].default, Some(20.0));
    assert_eq!(parameters[0].line, 2);
}

#[test]
fn edge_selectors_never_answer_with_faces() {
    let script = r#"chamfer(edges: edges("planar"), distance: 1, label: "c");"#;
    let error = compile_script(script, &BTreeMap::new()).expect_err("planar is not an edge rule");
    assert!(error.message().contains("Unknown edge selector"), "{error}");
}

#[test]
fn a_direction_selector_prefers_the_face_farthest_along_it() {
    // A boss on a plate: two faces look up, and ">Z" means the higher one.
    let script = r#"
        let plate = box(size: [40, 40, 10], label: "plate");
        let boss_profile = sketch(on: faces(">Z"), label: "boss_profile", entities: [
            rect(width: 16, height: 16),
        ]);
        extrude(sketch: boss_profile, distance: 12, operation: "add", label: "boss");
    "#;
    let commands = compile_script(script, &BTreeMap::new()).expect("compile");
    let mut session = Session::new();
    for command in commands {
        session
            .execute(command, &CancellationToken::default())
            .expect("execute");
    }
    let top = EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(0.0, 0.0, 1.0),
            match_kind: NormalMatch::Closest,
        },
    };
    let boss_top = EntitySelector::ByGeometry {
        selector: GeometricSelector::NearestTo {
            point: Point3::new(20.0, 20.0, 22.0),
            kind: artificer_protocol::EntityKind::Face,
        },
    };
    let bottom = EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(0.0, 0.0, 1.0),
            match_kind: NormalMatch::Farthest,
        },
    };
    let plate_bottom = EntitySelector::ByGeometry {
        selector: GeometricSelector::NearestTo {
            point: Point3::new(20.0, 20.0, 0.0),
            kind: artificer_protocol::EntityKind::Face,
        },
    };
    let query = session.query();
    let resolve =
        |selector: &EntitySelector| query.entity_info(selector).expect("resolve").entity_ref;
    assert_eq!(resolve(&top), resolve(&boss_top), "the boss top is the top");
    assert_eq!(resolve(&bottom), resolve(&plate_bottom));
}

#[test]
fn a_for_loop_repeats_steps_with_joined_labels() {
    use artificer_kernel::api::scripting::compile_program;
    let script = r#"
        param holes: f64 = 3;
        let b = box(size: [60, 20, 10], label: "b");
        let top = faces(">Z");
        for i in 0..holes {
            drill(face: top, center: [(i - 1) * 15, 0], diameter: 4, depth: 5, label: "hole_" + i);
        }
    "#;
    let program = compile_program(script, &BTreeMap::new()).expect("compile");
    let labels: Vec<&str> = program.commands.iter().map(ApiCommand::label).collect();
    assert_eq!(labels, ["b", "hole_0", "hole_1", "hole_2"]);
    assert_eq!(program.names.len(), 1);
    assert_eq!(program.names[0].0, "top");

    let mut session = Session::new();
    for command in program.commands {
        session
            .execute(command, &CancellationToken::default())
            .expect("execute");
    }
    let volume = session.snapshot.measures().volume;
    let expected = 60.0 * 20.0 * 10.0 - 3.0 * std::f64::consts::PI * 4.0 * 5.0;
    assert!(((volume - expected) / expected).abs() < 1.0e-9, "{volume}");

    // Overriding the count re-runs the loop with more holes.
    let mut overrides = BTreeMap::new();
    overrides.insert("holes".to_owned(), 2.0);
    let fewer = compile_script(script, &overrides).expect("compile");
    assert_eq!(fewer.len(), 3);

    // A runaway range is an error with the loop's location, not a hang.
    let runaway = "for i in 0..1000000 { let b = box(size: [1, 1, 1], label: \"b\" + i); }";
    let error = compile_script(runaway, &BTreeMap::new()).expect_err("too many iterations");
    assert!(error.message().contains("iterations"), "{error}");
    assert_eq!(error.location(), Some((1, 1)));

    // `+` joins strings and whole numbers without a fraction.
    let joined = "let b = box(size: [1, 1, 1], label: \"part_\" + 2 * 3 + \"_\" + 0.5);";
    let commands = compile_script(joined, &BTreeMap::new()).expect("compile");
    assert_eq!(commands[0].label(), "part_6_0.5");
}

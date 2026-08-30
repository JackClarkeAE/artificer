use std::collections::BTreeMap;

use artificer_api::commands::ApiCommand;
use artificer_api::export::{export_obj, export_stl_ascii, export_stl_binary};
use artificer_api::scripting::compile_script;
use artificer_api::selectors::{EntitySelector, GeometricSelector, NormalMatch};
use artificer_api::server::SharedSession;
use artificer_api::session::Session;
use artificer_api::snapshot::{CameraSpec, SnapshotOptions, SnapshotOutput, StandardView};
use artificer_api::CancellationToken;
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
    let geom_sel = EntitySelector::ByGeometry(GeometricSelector::FaceByNormal {
        direction: Vector3::new(0.0, 0.0, 1.0),
        match_kind: NormalMatch::Closest,
    });

    let entity_info = session.query().entity_info(&geom_sel).expect("Query face info");
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

    // 1. Sketch a 30x20 rectangle on XY plane
    session
        .execute(
            ApiCommand::Sketch {
                label: "sk1".to_owned(),
                on: artificer_api::commands::SketchPlane::XY,
                entities: vec![artificer_api::commands::SketchEntity::Rectangle {
                    origin: Point2::new(0.0, 0.0),
                    width: 30.0,
                    height: 20.0,
                }],
                constraints: Vec::new(),
            },
            &token,
        )
        .expect_err("Sketch by itself cannot be executed without extrude/revolve");

    // Push sketch to journal manually for extrude to consume
    session.journal.push(artificer_api::journal::JournalEntry::new(ApiCommand::Sketch {
        label: "sk1".to_owned(),
        on: artificer_api::commands::SketchPlane::XY,
        entities: vec![artificer_api::commands::SketchEntity::Rectangle {
            origin: Point2::new(0.0, 0.0),
            width: 30.0,
            height: 20.0,
        }],
        constraints: Vec::new(),
    }));

    let ext_res = session
        .execute(
            ApiCommand::Extrude {
                label: "ext1".to_owned(),
                sketch: artificer_api::commands::StepLabel("sk1".to_owned()),
                regions: Vec::new(),
                distance: 15.0,
                operation: artificer_api::commands::ExtrudeOp::New,
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
            &artificer_api::query::MeasureTarget::Point(Point3::new(0.0, 0.0, 0.0)),
            &artificer_api::query::MeasureTarget::Point(Point3::new(3.0, 4.0, 0.0)),
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
                face: EntitySelector::ByGeometry(GeometricSelector::FaceByNormal {
                    direction: Vector3::new(0.0, -1.0, 0.0),
                    match_kind: NormalMatch::Closest,
                }),
                center: Point2::new(0.0, 0.0),
                diameter: 20.0,
                depth: 30.0,
            },
            &token,
        )
        .expect("Drill side hole");

    // 4. Render snapshot
    let snap_res = session.snapshot(SnapshotOptions {
        camera: CameraSpec::preset(StandardView::Trimetric),
        format: artificer_api::snapshot::SnapshotFormat::Svg,
        display_mode: "shaded".to_owned(),
        show_labels: false,
        highlight: vec![],
    }).expect("snapshot");

    let SnapshotOutput::Svg(svg) = snap_res else { panic!("expected SVG"); };
    assert!(!svg.is_empty());
    assert!(svg.contains("<svg"));
    println!("Generated SVG snapshot: {} bytes", svg.len());
}

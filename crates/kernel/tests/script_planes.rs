//! Planes placed by a body's faces and edges in `.art` scripts (ADR 0048):
//! `plane(on: face, offset:)`, `plane(between: [a, b])` and
//! `plane(through: edge, face:, angle:)`. Each is checked by building a small
//! body on the plane and reading where it landed, against coordinates worked
//! out here from the box they are placed on.

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::decompile::{DecompileOptions, decompile_journal};
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::session::Session;
use artificer_protocol::Aabb3;

/// Runs a script and returns the session, with every step run.
fn run(script: &str) -> Session {
    let commands = compile_script(script, &BTreeMap::new()).expect("the script compiles");
    let mut session = Session::new();
    let token = CancellationToken::default();
    for command in commands {
        session.execute(command, &token).expect("the step runs");
    }
    session
}

/// The error the first failing step of a script gives. A sketch's plane is
/// resolved when a feature first uses it, so the scripts extrude.
fn refusal(script: &str) -> String {
    let commands = compile_script(script, &BTreeMap::new()).expect("the script compiles");
    let mut session = Session::new();
    let token = CancellationToken::default();
    for command in commands {
        if let Err(error) = session.execute(command, &token) {
            return error.message;
        }
    }
    panic!("the script ran without refusal");
}

fn bounds(session: &Session) -> Aabb3 {
    session
        .snapshot
        .measures()
        .bounds
        .expect("the body has bounds")
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= 1.0e-9,
        "{what}: {actual} is not {expected}"
    );
}

/// A 20 × 20 × 10 box with its minimum corner at the origin.
const BOX: &str = "let b = box(size: [20, 20, 10], label: \"b\");\n";

#[test]
fn a_plane_on_a_face_stands_off_it_along_its_outward_normal() {
    let session = run(&format!(
        "{BOX}let top = sketch(on: plane(on: faces(\">Z\"), offset: 5), entities: [rect(width: 4, height: 4)], label: \"top\");
let post = extrude(sketch: top, distance: 3, label: \"post\");"
    ));
    let bounds = bounds(&session);
    // The top face is at z = 10; the plane stands 5 above it and the post
    // runs 3 further, centred where a sketch on the face would be.
    assert_close(bounds.min.z, 15.0, "post bottom");
    assert_close(bounds.max.z, 18.0, "post top");
    assert_close(bounds.min.x + bounds.max.x, 20.0, "post centred in x");
    assert_close(bounds.min.y + bounds.max.y, 20.0, "post centred in y");
}

#[test]
fn a_midplane_lies_halfway_between_two_parallel_faces() {
    let session = run(&format!(
        "{BOX}let middle = sketch(on: plane(between: [faces(\"<X\"), faces(\">X\")]), entities: [rect(width: 4, height: 4)], label: \"middle\");
let web = extrude(sketch: middle, distance: 2, label: \"web\");"
    ));
    let bounds = bounds(&session);
    // Halfway between x = 0 and x = 20, facing as the first face does: −X.
    assert_close(bounds.max.x, 10.0, "web face on the midplane");
    assert_close(bounds.min.x, 8.0, "web grown along −X");
}

#[test]
fn a_plane_through_an_edge_turns_about_it() {
    // The top edge along x at y = 0. From the top face, the plane starts on
    // the face (leaning into it, +Y) and at 90° stands straight up (+Z), with
    // its x along the edge; its normal is then x × z = −Y.
    let edge = "nearest(point: [10, 0, 10], kind: \"edge\")";
    let upright = run(&format!(
        "{BOX}let fin = sketch(on: plane(through: {edge}, face: faces(\">Z\"), angle: 90), entities: [rect(width: 4, height: 4)], label: \"fin\");
let tab = extrude(sketch: fin, distance: 1, label: \"tab\");"
    ));
    let bounds_upright = bounds(&upright);
    assert_close(bounds_upright.max.y, 0.0, "tab face on the edge's plane");
    assert_close(bounds_upright.min.y, -1.0, "tab grown along −Y");
    // Hinged on the edge: the plane's origin is half an edge-and-a-bit
    // (0.5 × 20 × 1.15 = 11.5) up from the edge's middle.
    assert_close(
        bounds_upright.min.z + bounds_upright.max.z,
        2.0 * 21.5,
        "tab centre height",
    );
    assert_close(
        bounds_upright.min.x + bounds_upright.max.x,
        20.0,
        "tab centred along the edge",
    );

    // At no angle the plane is the top face's plane, facing up.
    let flat = run(&format!(
        "{BOX}let pad = sketch(on: plane(through: {edge}, face: faces(\">Z\")), entities: [rect(width: 4, height: 4)], label: \"pad\");
let block = extrude(sketch: pad, distance: 2, label: \"block\");"
    ));
    let bounds_flat = bounds(&flat);
    assert_close(bounds_flat.min.z, 10.0, "block on the face");
    assert_close(bounds_flat.max.z, 12.0, "block grown up");
    assert_close(
        bounds_flat.min.y + bounds_flat.max.y,
        2.0 * 11.5,
        "block leans into the face",
    );
}

#[test]
fn offset_and_flip_apply_to_every_placed_plane() {
    let session = run(&format!(
        "{BOX}let under = sketch(on: plane(on: faces(\">Z\"), offset: 2, flip: true), entities: [rect(width: 4, height: 4)], label: \"under\");
let peg = extrude(sketch: under, distance: 3, label: \"peg\");"
    ));
    let bounds = bounds(&session);
    // Two above the top face, then turned to face down: the peg grows back
    // toward the box.
    assert_close(bounds.max.z, 12.0, "peg starts on the plane");
    assert_close(bounds.min.z, 9.0, "peg grown down");
}

#[test]
fn placed_planes_decompile_to_the_script_that_made_them() {
    let script = format!(
        "{BOX}let a = sketch(on: plane(on: faces(\">Z\"), offset: 5), entities: [circle(radius: 2)], label: \"a\");
let b2 = sketch(on: plane(between: [faces(\"<X\"), faces(\">X\")], flip: true), entities: [circle(radius: 2)], label: \"b2\");
let c = sketch(on: plane(through: nearest(point: [10, 0, 10], kind: \"edge\"), face: faces(\">Z\"), angle: 30, offset: 1), entities: [circle(radius: 2)], label: \"c\");"
    );
    let commands = compile_script(&script, &BTreeMap::new()).expect("the script compiles");
    let session = run(&script);
    let written = decompile_journal(&session.journal, &DecompileOptions::default())
        .expect("the journal decompiles");
    let again = compile_script(&written, &BTreeMap::new()).expect("the decompiled script compiles");
    assert_eq!(again, commands, "{written}");
}

#[test]
fn placed_planes_are_refused_by_name_where_they_cannot_be_placed() {
    let two_faces = refusal(&format!(
        "{BOX}let s = sketch(on: plane(through: nearest(point: [10, 0, 10], kind: \"edge\")), entities: [circle(radius: 2)], label: \"s\");
let e = extrude(sketch: s, distance: 1, label: \"e\");"
    ));
    assert!(two_faces.contains("name the one"), "{two_faces}");
    let square = refusal(&format!(
        "{BOX}let s = sketch(on: plane(between: [faces(\">Z\"), faces(\">X\")]), entities: [circle(radius: 2)], label: \"s\");
let e = extrude(sketch: s, distance: 1, label: \"e\");"
    ));
    assert!(square.contains("parallel"), "{square}");
}

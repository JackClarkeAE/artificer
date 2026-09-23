//! `axis(...)`: construction axes in a script, and a revolve about one.
//! Every volume is a tube's, from its closed form.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::session::Session;

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

fn failure(source: &str) -> String {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    let failure = outcome.failure.expect("the script should fail");
    format!("{} {:?}", failure.message, failure.diagnostics)
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= 1.0e-9 * expected.abs().max(1.0),
        "{what}: {actual} is not {expected}"
    );
}

/// A 2 × 2 square on the XZ plane, from radius 2 to 4 off the Z axis, turned
/// about `axis`: a tube of volume π(16 − 4)·2 for a full turn.
fn tube_about(axis: &str, angle: f64) -> String {
    format!(
        "let block = box(size: [10, 10, 10], label: \"block\");
let ring = sketch(on: \"XZ\", label: \"ring\", entities: [
    rect(origin: [2, 0], width: 2, height: 2),
]);
revolve(sketch: ring, axis: {axis}, angle: {angle}, label: \"tube\");
"
    )
}

const TUBE: f64 = PI * 12.0 * 2.0;

#[test]
fn a_world_axis_and_a_line_in_space_turn_as_the_numbers_do() {
    let numbers = run(&tube_about("[0, 0, 1]", 360.0));
    for axis in [
        "axis(from: \"Z\")",
        "axis(origin: [0, 0, 5], direction: [0, 0, 2])",
    ] {
        let session = run(&tube_about(axis, 360.0));
        assert_close(session.snapshot.measures().volume, TUBE, axis);
        assert_eq!(
            session.snapshot.semantic_digest(),
            numbers.snapshot.semantic_digest(),
            "{axis}"
        );
    }
}

#[test]
fn an_axis_along_an_edge_or_where_two_faces_meet_is_found_on_the_body() {
    // The block's edge up the Z axis, named by a point on it, and the two
    // faces x = 0 and y = 0 that meet there.
    for axis in [
        "axis(along: nearest(point: [0, 0, 5], kind: \"edge\"))",
        "axis(between: [faces(\"<X\"), faces(\"<Y\")])",
    ] {
        let session = run(&tube_about(axis, 360.0));
        assert_close(session.snapshot.measures().volume, TUBE, axis);
    }
}

#[test]
fn an_axis_through_a_curved_face_is_its_carrier_axis() {
    // A flange turned about the post's own axis joins it exactly.
    let session = run(
        "let post = cylinder(radius: 5, height: 10, label: \"post\");
let flange = sketch(on: \"XZ\", label: \"flange\", entities: [
    rect(origin: [4, 2], width: 4, height: 2),
]);
revolve(sketch: flange, axis: axis(through: nearest(point: [5, 0, 5], kind: \"face\")), operation: \"add\", label: \"flanged\");
",
    );
    assert_close(
        session.snapshot.measures().volume,
        PI * 25.0 * 10.0 + PI * (64.0 - 25.0) * 2.0,
        "flanged post",
    );
}

#[test]
fn a_flipped_axis_turns_a_partial_revolve_the_other_way() {
    let centroid_y = |axis: &str| {
        run(&tube_about(axis, 90.0))
            .snapshot
            .measures()
            .centroid
            .expect("a centroid")
            .y
    };
    let forward = centroid_y("axis(from: \"Z\")");
    let flipped = centroid_y("axis(from: \"Z\", flip: true)");
    assert!(forward > 1.0, "{forward}");
    assert_close(flipped, -forward, "the mirror image");
}

#[test]
fn an_axis_placed_by_the_body_decompiles_to_itself() {
    let session = run(&tube_about(
        "axis(between: [faces(\"<X\"), faces(\"<Y\")], flip: true)",
        90.0,
    ));
    let decompiled = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(decompiled.contains("axis(between: ["), "{decompiled}");
    assert!(decompiled.contains("flip: true)"), "{decompiled}");
    assert_eq!(
        run(&decompiled).snapshot.semantic_digest(),
        session.snapshot.semantic_digest(),
        "{decompiled}"
    );
    let journal = session.export_journal().unwrap();
    assert_eq!(
        Session::from_journal(&journal)
            .unwrap()
            .snapshot
            .semantic_digest(),
        session.snapshot.semantic_digest()
    );
}

#[test]
fn an_axis_refuses_what_is_not_one() {
    // A curved edge has no line to run along.
    let message = failure(
        "let post = cylinder(radius: 5, height: 10, label: \"post\");
let ring = sketch(on: \"XZ\", label: \"ring\", entities: [
    rect(origin: [6, 0], width: 2, height: 2),
]);
revolve(sketch: ring, axis: axis(along: nearest(point: [5, 0, 10], kind: \"edge\")), label: \"tube\");
",
    );
    assert!(message.contains("straight edge"), "{message}");
    // An axis(...) says where it runs; an origin beside it is a mistake.
    let message = failure(&tube_about(
        "axis(from: \"Z\"), axis_origin: [1, 0, 0]",
        360.0,
    ));
    assert!(message.contains("axis_origin"), "{message}");
    let message = failure(&tube_about("axis(from: \"W\")", 360.0));
    assert!(message.contains("\"X\", \"Y\" or \"Z\""), "{message}");
}

//! Shell: the closed-form volumes a uniform wall must leave, the wall the
//! probe must read back, and the refusals that keep it honest.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::debug::ApiErrorCode;
use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::probe::{ProbeRequest, probe};
use artificer_kernel::api::selectors::{EntitySelector, GeometricSelector, NormalMatch};
use artificer_kernel::api::session::Session;
use artificer_protocol::{Tier, Vector3};

const B: f64 = 60.0;
const D: f64 = 40.0;
const H: f64 = 25.0;
const W: f64 = 3.0;

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    let tolerance = 1.0e-9 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not {expected} (off by {})",
        actual - expected
    );
}

fn min_wall(session: &Session) -> f64 {
    probe(session, &ProbeRequest::MinWall { step: None })
        .expect("min wall probe")
        .value
}

fn face_toward(x: f64, y: f64, z: f64) -> EntitySelector {
    EntitySelector::ByGeometry {
        selector: GeometricSelector::FaceByNormal {
            direction: Vector3::new(x, y, z),
            match_kind: NormalMatch::Closest,
        },
    }
}

fn box_script() -> String {
    format!("let block = box(size: [{B}, {D}, {H}], label: \"block\");\n")
}

fn rung_of(session: &Session, label: &str) -> String {
    session
        .report()
        .steps
        .iter()
        .find(|step| step.label == label)
        .and_then(|step| step.rung.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Open shells
// ---------------------------------------------------------------------------

#[test]
fn a_box_shelled_open_at_the_top_keeps_walls_and_a_floor() {
    let script = format!(
        "{}shell(open: faces(\">Z\"), wall: {W}, label: \"shelled\");\n",
        box_script()
    );
    let session = run(&script);
    assert_close(
        session.snapshot.measures().volume,
        B * D * H - (B - 2.0 * W) * (D - 2.0 * W) * (H - W),
        "volume",
    );
    assert_eq!(session.snapshot.counts().solids, 1);
    assert_eq!(session.snapshot.counts().shells, 1);
    // Six outer faces less the open top, plus the pocket's four walls and
    // floor.
    assert_eq!(session.snapshot.counts().faces, 6 + 5);
    assert_eq!(rung_of(&session, "shelled"), "shell/open-prism");
    assert_eq!(session.report().tier, Tier::Exact);
    assert_close(min_wall(&session), W, "thinnest wall");
}

#[test]
fn a_box_opens_on_any_face() {
    // A box is a prism along each axis, so the shell opens on a side too.
    let script = format!(
        "{}shell(open: faces(\">X\"), wall: {W}, label: \"shelled\");\n",
        box_script()
    );
    let session = run(&script);
    assert_close(
        session.snapshot.measures().volume,
        B * D * H - (D - 2.0 * W) * (H - 2.0 * W) * (B - W),
        "volume",
    );
    assert_close(min_wall(&session), W, "thinnest wall");
}

#[test]
fn a_cylinder_shelled_open_at_the_top_is_a_cup() {
    let (r, h) = (20.0, 30.0);
    let script = format!(
        "let cup = cylinder(radius: {r}, height: {h}, label: \"cup\");\nshell(open: faces(\">Z\"), wall: {W}, label: \"shelled\");\n"
    );
    let session = run(&script);
    assert_close(
        session.snapshot.measures().volume,
        PI * r * r * h - PI * (r - W) * (r - W) * (h - W),
        "volume",
    );
    assert_eq!(session.report().tier, Tier::Exact);
    let body = session.report().body.unwrap();
    assert!(body.surfaces.cylinders >= 2, "{:?}", body.surfaces);
    // Facets under-read a curved wall by their chord; the floor reads
    // exactly.
    let wall = min_wall(&session);
    assert!((wall - W).abs() <= 1.0e-2, "thinnest wall {wall}");
}

#[test]
fn a_box_open_at_both_ends_is_a_tube() {
    let script = format!(
        "{}shell(open: [faces(\">Z\"), faces(\"<Z\")], wall: {W}, label: \"tube\");\n",
        box_script()
    );
    let session = run(&script);
    assert_close(
        session.snapshot.measures().volume,
        (B * D - (B - 2.0 * W) * (D - 2.0 * W)) * H,
        "volume",
    );
    // Four outer walls, four inner walls, and the two caps as frames.
    assert_eq!(session.snapshot.counts().faces, 10);
    assert_close(min_wall(&session), W, "thinnest wall");
}

#[test]
fn a_hole_through_the_open_face_keeps_a_wall_around_it() {
    let r = 6.0;
    let script = format!(
        "{}let hole = drill(face: faces(\">Z\"), center: [10, 5], diameter: {}, depth: {H}, label: \"hole\");\nshell(open: faces(\">Z\"), wall: {W}, label: \"shelled\");\n",
        box_script(),
        2.0 * r
    );
    let session = run(&script);
    // The pocket is the shrunk outline less the grown hole, one wall deep
    // short of the floor.
    let pocket_area = (B - 2.0 * W) * (D - 2.0 * W) - PI * (r + W) * (r + W);
    assert_close(
        session.snapshot.measures().volume,
        B * D * H - PI * r * r * H - pocket_area * (H - W),
        "volume",
    );
    assert_eq!(session.report().tier, Tier::Exact);
    // The wall around the hole is read between two facetted cylinders,
    // so it comes back short by their chords.
    let wall = min_wall(&session);
    assert!((wall - W).abs() <= 1.0e-2, "thinnest wall {wall}");
}

#[test]
fn a_closed_cylinder_shell_is_a_sealed_can() {
    let (r, h) = (20.0, 30.0);
    let script = format!(
        "let can = cylinder(radius: {r}, height: {h}, label: \"can\");\nshell(wall: {W}, label: \"hollow\");\n"
    );
    let session = run(&script);
    assert_close(
        session.snapshot.measures().volume,
        PI * r * r * h - PI * (r - W) * (r - W) * (h - 2.0 * W),
        "volume",
    );
    assert_eq!(session.snapshot.counts().shells, 2);
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_shelled_extrusion_with_arcs_offsets_its_arcs() {
    // A slot outline: two semicircles joined by two lines.
    let (length, radius, height) = (50.0, 10.0, 20.0);
    let script = format!(
        "let s = sketch(on: \"XY\", entities: [\
line(start: [0, -{radius}], end: [{length}, -{radius}]), \
arc(center: [{length}, 0], radius: {radius}, start_angle: -90, end_angle: 90), \
line(start: [{length}, {radius}], end: [0, {radius}]), \
arc(center: [0, 0], radius: {radius}, start_angle: 90, end_angle: 270)], label: \"s\");\n\
let slot = extrude(sketch: s, distance: {height}, label: \"slot\");\n\
shell(open: faces(\">Z\"), wall: {W}, label: \"shelled\");\n"
    );
    let session = run(&script);
    let area = |r: f64| length * 2.0 * r + PI * r * r;
    assert_close(
        session.snapshot.measures().volume,
        area(radius) * height - area(radius - W) * (height - W),
        "volume",
    );
    assert_eq!(session.report().tier, Tier::Exact);
}

// ---------------------------------------------------------------------------
// Closed shells
// ---------------------------------------------------------------------------

#[test]
fn a_closed_shell_leaves_a_void_one_wall_in_from_every_face() {
    let script = format!("{}shell(wall: {W}, label: \"hollow\");\n", box_script());
    let session = run(&script);
    assert_close(
        session.snapshot.measures().volume,
        B * D * H - (B - 2.0 * W) * (D - 2.0 * W) * (H - 2.0 * W),
        "volume",
    );
    let counts = session.snapshot.counts();
    assert_eq!(counts.solids, 1);
    assert_eq!(counts.shells, 2, "an outer shell and the void");
    assert_eq!(counts.faces, 12);
    assert_eq!(rung_of(&session, "hollow"), "shell/closed-prism");
    assert_eq!(session.report().tier, Tier::Exact);
    assert_close(min_wall(&session), W, "thinnest wall");
}

// ---------------------------------------------------------------------------
// Replay, journal, and the API
// ---------------------------------------------------------------------------

#[test]
fn a_shell_replays_to_the_same_digest_through_the_journal_and_the_script() {
    let script = format!(
        "{}let hole = drill(face: faces(\">Z\"), center: [10, 5], diameter: 8, depth: {H}, label: \"hole\");\nshell(open: faces(\">Z\"), wall: {W}, label: \"shelled\");\n",
        box_script()
    );
    let first = run(&script);
    let digest = first.snapshot.semantic_digest();
    assert_eq!(run(&script).snapshot.semantic_digest(), digest);
    let journal = first.export_journal().unwrap();
    assert!(journal.contains("\"shell\""), "{journal}");
    assert_eq!(
        Session::from_journal(&journal)
            .unwrap()
            .snapshot
            .semantic_digest(),
        digest
    );
    let decompiled = first.to_art(&DecompileOptions::default()).unwrap();
    assert!(decompiled.contains("shell(open: "), "{decompiled}");
    assert_eq!(
        run(&decompiled).snapshot.semantic_digest(),
        digest,
        "{decompiled}"
    );
}

#[test]
fn the_api_command_shells_with_selectors() {
    let mut session = run(&box_script());
    let result = session
        .execute(
            ApiCommand::Shell {
                label: "shelled".to_owned(),
                open: vec![face_toward(0.0, 0.0, -1.0)],
                wall: 2.0,
            },
            &CancellationToken::default(),
        )
        .expect("shell");
    assert_eq!(result.rung.as_deref(), Some("shell/open-prism"));
    assert_eq!(result.tier, Tier::Exact);
    assert_close(
        session.snapshot.measures().volume,
        B * D * H - (B - 4.0) * (D - 4.0) * (H - 2.0),
        "volume",
    );
    session.undo().unwrap();
    assert_close(session.snapshot.measures().volume, B * D * H, "undone");
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

fn refusal(script: &str) -> (ApiErrorCode, String) {
    let mut session = Session::new();
    let outcome = session.run_script(script, &BTreeMap::new(), &CancellationToken::default());
    let failure = outcome.failure.expect("the shell should be refused");
    assert_eq!(failure.label, "shelled");
    let codes = failure
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.clone())
        .collect::<Vec<_>>()
        .join(",");
    (failure.code, codes)
}

#[test]
fn shells_refuse_walls_that_leave_nothing_and_bodies_that_are_not_prisms() {
    // Too thick for a floor.
    let (code, codes) = refusal(&format!(
        "{}shell(open: faces(\">Z\"), wall: {H}, label: \"shelled\");\n",
        box_script()
    ));
    assert_eq!(code, ApiErrorCode::KernelError);
    assert!(codes.contains("SHELL_WALL_INVALID"), "{codes}");

    // Too thick for a core.
    let (_, codes) = refusal(&format!(
        "{}shell(wall: {}, label: \"shelled\");\n",
        box_script(),
        H / 2.0
    ));
    assert!(codes.contains("SHELL_WALL_INVALID"), "{codes}");

    // Thicker than half the outline: the offset closes up.
    let (_, codes) = refusal(&format!(
        "{}shell(open: faces(\">Z\"), wall: {}, label: \"shelled\");\n",
        box_script(),
        D / 2.0 + 1.0
    ));
    assert!(
        codes.contains("SHELL_WALL_INVALID") || codes.contains("SHELL_SELF_INTERSECTS"),
        "{codes}"
    );

    // No wall at all.
    let (_, codes) = refusal(&format!(
        "{}shell(open: faces(\">Z\"), wall: 0, label: \"shelled\");\n",
        box_script()
    ));
    assert!(codes.contains("SHELL_WALL_INVALID"), "{codes}");

    // A blended body is not a prism.
    let (_, codes) = refusal(&format!(
        "{}fillet(edges: [nearest(point: [{B}, {}, {H}], kind: \"edge\")], radius: 2, label: \"round\");\nshell(open: faces(\"<Z\"), wall: {W}, label: \"shelled\");\n",
        box_script(),
        D / 2.0
    ));
    assert!(codes.contains("SHELL_DOMAIN_UNSUPPORTED"), "{codes}");

    // Two open faces that are not opposite.
    let (_, codes) = refusal(&format!(
        "{}shell(open: [faces(\">Z\"), faces(\">X\")], wall: {W}, label: \"shelled\");\n",
        box_script()
    ));
    assert!(codes.contains("SHELL_OPEN_FACES_UNSUPPORTED"), "{codes}");
}

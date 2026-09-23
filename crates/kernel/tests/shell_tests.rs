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
    assert!(
        failure.label == "shelled" || failure.label == "hollow" || failure.label == "cup",
        "{}",
        failure.label
    );
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

// ---------------------------------------------------------------------------
// Solids of revolution
// ---------------------------------------------------------------------------

/// A two-diameter turned hub: a 30 mm flange 10 mm thick under a 12 mm
/// boss 30 mm tall. No face of it is a prism about any other, so the
/// prism reading refuses and the section reading owns it.
fn stepped_hub() -> String {
    "let section = sketch(on: \"XZ\", label: \"section\", entities: [
    line(start: [0, 0], end: [30, 0]),
    line(start: [30, 0], end: [30, 10]),
    line(start: [30, 10], end: [12, 10]),
    line(start: [12, 10], end: [12, 40]),
    line(start: [12, 40], end: [0, 40]),
    line(start: [0, 40], end: [0, 0]),
]);
let hub = revolve(sketch: section, axis: [0, 0, 1], label: \"hub\");
"
    .to_owned()
}

#[test]
fn a_stepped_hub_shells_closed_through_its_section() {
    let session = run(&format!(
        "{}shell(wall: 3, label: \"hollow\");\n",
        stepped_hub()
    ));
    // The core is the section offset 3 mm inward: a 27 mm disc from z = 3
    // to 7 under a 9 mm post to z = 37.
    let core = PI * (27.0 * 27.0 * 4.0 + 9.0 * 9.0 * 30.0);
    let body = PI * (30.0 * 30.0 * 10.0 + 12.0 * 12.0 * 30.0);
    assert_close(session.snapshot.measures().volume, body - core, "volume");
    assert_eq!(
        session.snapshot.counts().shells,
        2,
        "an outer shell and a void"
    );
    assert_eq!(rung_of(&session, "hollow"), "shell/closed-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
    let surfaces = session.report().body.expect("body").surfaces;
    assert_eq!(surfaces.planes + surfaces.cylinders, surfaces.total());
    // Between two facetted bores the probe reads short by their chords.
    let wall = min_wall(&session);
    assert!((wall - 3.0).abs() <= 1.0e-2, "thinnest wall {wall}");
}

#[test]
fn a_stepped_hub_opens_at_its_top_cap() {
    let session = run(&format!(
        "{}shell(open: faces(\">Z\"), wall: 3, label: \"cup\");\n",
        stepped_hub()
    ));
    // The same core, less the wall it would have left over the boss: the
    // bore runs right out through the top.
    let removed = PI * (27.0 * 27.0 * 4.0 + 9.0 * 9.0 * 33.0);
    let body = PI * (30.0 * 30.0 * 10.0 + 12.0 * 12.0 * 30.0);
    assert_close(session.snapshot.measures().volume, body - removed, "volume");
    assert_eq!(session.snapshot.counts().shells, 1, "the cup is open");
    assert_eq!(rung_of(&session, "cup"), "shell/open-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_tapered_post_shells_closed_with_conical_walls() {
    let script = "let s = sketch(on: \"XZ\", label: \"s\", entities: [
    line(start: [0, 0], end: [20, 0]),
    line(start: [20, 0], end: [12, 30]),
    line(start: [12, 30], end: [0, 30]),
    line(start: [0, 30], end: [0, 0]),
]);
let post = revolve(sketch: s, axis: [0, 0, 1], label: \"post\");
shell(wall: 2, label: \"hollow\");
";
    let session = run(script);
    // The wall is measured square to the slant, so the core's radius line
    // is the post's shifted by one wall along its own normal.
    let (wall, slope): (f64, f64) = (2.0, 8.0 / 30.0);
    let inset = wall * (1.0 + slope * slope).sqrt();
    let radius = |z: f64| 20.0 - inset - slope * z;
    let (lower, upper) = (radius(wall), radius(30.0 - wall));
    let core = PI * (30.0 - 2.0 * wall) / 3.0 * (lower * lower + lower * upper + upper * upper);
    let body = PI * 30.0 / 3.0 * (400.0 + 20.0 * 12.0 + 144.0);
    assert_close(session.snapshot.measures().volume, body - core, "volume");
    assert_eq!(session.snapshot.counts().shells, 2);
    let surfaces = session.report().body.expect("body").surfaces;
    assert_eq!(surfaces.cones, 4, "two cone halves outside and two in");
    assert_eq!(rung_of(&session, "hollow"), "shell/closed-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_revolved_shell_replays_to_the_same_digest() {
    let script = format!("{}shell(wall: 3, label: \"hollow\");\n", stepped_hub());
    let first = run(&script);
    assert_eq!(
        run(&script).snapshot.semantic_digest(),
        first.snapshot.semantic_digest()
    );
    let journal = first.export_journal().unwrap();
    assert_eq!(
        Session::from_journal(&journal)
            .unwrap()
            .snapshot
            .semantic_digest(),
        first.snapshot.semantic_digest()
    );
    let decompiled = first.to_art(&DecompileOptions::default()).unwrap();
    assert_eq!(
        run(&decompiled).snapshot.semantic_digest(),
        first.snapshot.semantic_digest(),
        "{decompiled}"
    );
}

#[test]
fn a_blend_or_a_dome_is_refused_by_name() {
    // The wall's inner surface would be the offset of a torus, with the
    // material on the far side of the tube from where this kernel's
    // carriers put it.
    let (_, codes) = refusal(
        "let c = cylinder(radius: 20, height: 30, label: \"c\");
fillet(edges: [nearest(point: [20, 0, 30], kind: \"edge\"), nearest(point: [-20, 0, 30], kind: \"edge\")], radius: 5, label: \"rim\");
shell(wall: 3, label: \"shelled\");
",
    );
    assert!(codes.contains("SHELL_BLEND_UNSUPPORTED"), "{codes}");

    // The same for a dome, whose inner surface would be a sphere.
    let (_, codes) = refusal(
        "let c = cylinder(radius: 5, height: 10, label: \"c\");
fillet(edges: [nearest(point: [5, 0, 10], kind: \"edge\"), nearest(point: [-5, 0, 10], kind: \"edge\")], radius: 5, label: \"dome\");
shell(wall: 1, label: \"shelled\");
",
    );
    assert!(codes.contains("SHELL_BLEND_UNSUPPORTED"), "{codes}");
}

#[test]
fn a_tapered_post_opens_at_its_top_through_the_coaxial_boolean() {
    // The post and its core turn about one axis, so the wall is taken away
    // in their shared section and the cones come back exact.
    let session = run("let s = sketch(on: \"XZ\", label: \"s\", entities: [
    line(start: [0, 0], end: [20, 0]),
    line(start: [20, 0], end: [12, 30]),
    line(start: [12, 30], end: [0, 30]),
    line(start: [0, 30], end: [0, 0]),
]);
let post = revolve(sketch: s, axis: [0, 0, 1], label: \"post\");
shell(open: faces(\">Z\"), wall: 2, label: \"cup\");
");
    let (wall, slope): (f64, f64) = (2.0, 8.0 / 30.0);
    let inset = wall * (1.0 + slope * slope).sqrt();
    let radius = |z: f64| 20.0 - inset - slope * z;
    let (lower, upper) = (radius(wall), radius(30.0));
    let removed = PI * (30.0 - wall) / 3.0 * (lower * lower + lower * upper + upper * upper);
    let body = PI * 30.0 / 3.0 * (400.0 + 20.0 * 12.0 + 144.0);
    assert_close(session.snapshot.measures().volume, body - removed, "volume");
    assert_eq!(session.snapshot.counts().shells, 1, "the cup is open");
    assert_eq!(rung_of(&session, "cup"), "shell/open-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn opening_a_partial_cone_names_the_shell_first() {
    // A partial cone's core is cut on the faceted tier, and a faceted core
    // cannot be taken away exactly: the refusal says so in the shell's
    // own words.
    let (_, codes) = refusal(
        "let s = sketch(on: \"XZ\", label: \"s\", entities: [
    line(start: [0, 0], end: [20, 0]),
    line(start: [20, 0], end: [12, 30]),
    line(start: [12, 30], end: [0, 30]),
    line(start: [0, 30], end: [0, 0]),
]);
let post = revolve(sketch: s, axis: [0, 0, 1], angle: 90, label: \"post\");
shell(open: faces(\">Z\"), wall: 2, label: \"shelled\");
",
    );
    assert!(
        codes.starts_with("SHELL_OPEN_REVOLVE_UNSUPPORTED"),
        "{codes}"
    );
}

// ---------------------------------------------------------------------------
// Partial turns
// ---------------------------------------------------------------------------

/// The stepped hub turned `angle` degrees from `+X` towards `+Y`, so its
/// wedge faces lie on `y = 0` and at `angle`.
fn partial_hub(angle: f64) -> String {
    stepped_hub().replace(
        "axis: [0, 0, 1], label",
        &format!("axis: [0, 0, 1], angle: {angle}, label"),
    )
}

/// The integral of `√(ρ² − t²)` from zero to `t`.
fn under_circle(rho: f64, t: f64) -> f64 {
    (t * (rho * rho - t * t).sqrt() + rho * rho * (t / rho).asin()) / 2.0
}

/// The area of a disc of radius `rho` about the axis kept by a quarter
/// turn's core: one wall in from both wedge faces, `x ≥ w` and `y ≥ w`.
fn quarter_core_area(rho: f64, wall: f64) -> f64 {
    let far = (rho * rho - wall * wall).sqrt();
    under_circle(rho, far) - under_circle(rho, wall) - wall * (far - wall)
}

/// The same for three quarters of a turn: the disc less the empty quarter,
/// a band one wall wide along each wedge face, and the quarter of the disc
/// one wall about the axis where the bands meet.
fn three_quarter_core_area(rho: f64, wall: f64) -> f64 {
    0.75 * PI * rho * rho - 2.0 * under_circle(rho, wall) - PI * wall * wall / 4.0
}

/// The full hub's volume.
fn hub_volume() -> f64 {
    PI * (30.0 * 30.0 * 10.0 + 12.0 * 12.0 * 30.0)
}

#[test]
fn a_quarter_hub_shells_closed_one_wall_in_from_its_wedge_faces() {
    let session = run(&format!(
        "{}shell(wall: 3, label: \"hollow\");\n",
        partial_hub(90.0)
    ));
    // The core is the section offset 3 mm inward, turned, and kept one wall
    // clear of both wedge faces: a 27 mm disc from z = 3 to 7 and a 9 mm
    // post to z = 37, each cut to `x ≥ 3, y ≥ 3`.
    let core = 4.0 * quarter_core_area(27.0, 3.0) + 30.0 * quarter_core_area(9.0, 3.0);
    assert_close(
        session.snapshot.measures().volume,
        hub_volume() / 4.0 - core,
        "volume",
    );
    assert_eq!(session.snapshot.counts().shells, 2, "a shell and a void");
    assert_eq!(rung_of(&session, "hollow"), "shell/closed-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
    let wall = min_wall(&session);
    assert!((wall - 3.0).abs() <= 1.0e-2, "thinnest wall {wall}");
}

#[test]
fn a_three_quarter_hub_shells_closed_round_its_axis() {
    let session = run(&format!(
        "{}shell(wall: 3, label: \"hollow\");\n",
        partial_hub(270.0)
    ));
    // Beyond half a turn the wall bends round the axis between the two
    // wedge faces: the void stays one wall clear of the axis itself.
    let core = 4.0 * three_quarter_core_area(27.0, 3.0) + 30.0 * three_quarter_core_area(9.0, 3.0);
    assert_close(
        session.snapshot.measures().volume,
        hub_volume() * 0.75 - core,
        "volume",
    );
    assert_eq!(session.snapshot.counts().shells, 2);
    assert_eq!(rung_of(&session, "hollow"), "shell/closed-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_quarter_hub_opens_at_its_top_cap() {
    let session = run(&format!(
        "{}shell(open: faces(\">Z\"), wall: 3, label: \"cup\");\n",
        partial_hub(90.0)
    ));
    // As closed, but the bore runs out through the top.
    let removed = 4.0 * quarter_core_area(27.0, 3.0) + 33.0 * quarter_core_area(9.0, 3.0);
    assert_close(
        session.snapshot.measures().volume,
        hub_volume() / 4.0 - removed,
        "volume",
    );
    assert_eq!(session.snapshot.counts().shells, 1);
    assert_eq!(rung_of(&session, "cup"), "shell/open-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_half_hub_opens_through_its_wedge_faces_as_a_cutaway() {
    // Both wedge faces lie on y = 0, facing -Y, one each side of the axis;
    // opened with the top cap, what is left is the half of a hollow hub's
    // wall.
    let session = run(&format!(
        "{}shell(open: [nearest(point: [20, 0, 5], kind: \"face\"), nearest(point: [-20, 0, 5], kind: \"face\"), faces(\">Z\")], wall: 3, label: \"cup\");\n",
        partial_hub(180.0)
    ));
    let removed = PI * (27.0 * 27.0 * 4.0 + 9.0 * 9.0 * 33.0) / 2.0;
    assert_close(
        session.snapshot.measures().volume,
        hub_volume() / 2.0 - removed,
        "volume",
    );
    assert_eq!(rung_of(&session, "cup"), "shell/open-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_quarter_tapered_post_shells_closed_on_the_faceted_tier() {
    let script = "let s = sketch(on: \"XZ\", label: \"s\", entities: [
    line(start: [0, 0], end: [20, 0]),
    line(start: [20, 0], end: [12, 30]),
    line(start: [12, 30], end: [0, 30]),
    line(start: [0, 30], end: [0, 0]),
]);
let post = revolve(sketch: s, axis: [0, 0, 1], angle: 90, label: \"post\");
shell(wall: 2, label: \"hollow\");
";
    let session = run(script);
    // A wedge face's wall meets the conical core in a hyperbola, outside
    // the exact engines' vocabulary: the cut is faceted and says so.
    assert_eq!(rung_of(&session, "hollow"), "shell/faceted");
    assert_eq!(session.report().tier, Tier::Approximate);
    let (wall, slope): (f64, f64) = (2.0, 8.0 / 30.0);
    let inset = wall * (1.0 + slope * slope).sqrt();
    let steps = 2000;
    let span = 30.0 - 2.0 * wall;
    let core: f64 = (0..steps)
        .map(|index| {
            let z = wall + span * (index as f64 + 0.5) / steps as f64;
            quarter_core_area(20.0 - inset - slope * z, wall) * span / steps as f64
        })
        .sum();
    let body = PI * 30.0 / 3.0 * (400.0 + 20.0 * 12.0 + 144.0) / 4.0;
    let volume = session.snapshot.measures().volume;
    assert!(
        ((volume - (body - core)) / (body - core)).abs() < 1.0e-2,
        "volume {volume} should be near {}",
        body - core
    );
    assert_eq!(session.snapshot.counts().shells, 2);
}

#[test]
fn an_open_wedge_face_beyond_half_a_turn_is_refused() {
    // Near the axis the empty wedge is narrower than the wall the other
    // face keeps, so there is nowhere for the open face's core to run.
    let (_, codes) = refusal(&format!(
        "{}shell(open: faces(\"<Y\"), wall: 3, label: \"cup\");\n",
        partial_hub(270.0)
    ));
    assert!(codes.contains("SHELL_OPEN_FACES_UNSUPPORTED"), "{codes}");
}

#[test]
fn a_quarter_cylinder_opens_at_its_top_and_one_wedge_face() {
    // A quarter cylinder is a prism about its caps, but the prism opens
    // only on one cap or two opposite ones; a cap with a wedge face is the
    // revolved reading's to take.
    let session = run("let s = sketch(on: \"XZ\", label: \"s\", entities: [
    rect(origin: [0, 0], width: 20, height: 30),
]);
let quarter = revolve(sketch: s, axis: [0, 0, 1], angle: 90, label: \"quarter\");
shell(open: [faces(\">Z\"), faces(\"<Y\")], wall: 2, label: \"cup\");
");
    // The core keeps x ≥ 2 against the closed wedge face and runs past the
    // open one at y = 0, inside radius 18 from z = 2 up through the top.
    let removed = 28.0 * (under_circle(18.0, 18.0) - under_circle(18.0, 2.0));
    assert_close(
        session.snapshot.measures().volume,
        PI * 20.0 * 20.0 * 30.0 / 4.0 - removed,
        "volume",
    );
    assert_eq!(rung_of(&session, "cup"), "shell/open-revolve");
    assert_eq!(session.report().tier, Tier::Exact);
}

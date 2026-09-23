//! The coaxial Boolean (ADR 0026 F4): turned shapes added to and cut from
//! turned bodies, exactly, cones, spheres and tori included. Every volume is
//! derived here from Pappus or a closed-form solid.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::session::Session;
use artificer_protocol::Tier;

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

fn rung_of(session: &Session, label: &str) -> String {
    session
        .report()
        .steps
        .iter()
        .find(|step| step.label == label)
        .and_then(|step| step.rung.clone())
        .unwrap_or_default()
}

/// A shaft of radius `radius` and height `height`, turned from its section.
fn shaft(radius: f64, height: f64) -> String {
    format!(
        "let shaft_section = sketch(on: \"XZ\", label: \"shaft_section\", entities: [
    rect(origin: [0, 0], width: {radius}, height: {height}),
]);
let shaft = revolve(sketch: shaft_section, axis: [0, 0, 1], label: \"shaft\");
"
    )
}

/// A closed polygon on the XZ plane, turned about Z with `operation`.
fn turned(label: &str, points: &[(f64, f64)], operation: &str) -> String {
    let mut lines = String::new();
    for (index, start) in points.iter().enumerate() {
        let end = points[(index + 1) % points.len()];
        lines.push_str(&format!(
            "    line(start: [{}, {}], end: [{}, {}]),\n",
            start.0, start.1, end.0, end.1
        ));
    }
    format!(
        "let {label}_section = sketch(on: \"XZ\", label: \"{label}_section\", entities: [
{lines}]);
revolve(sketch: {label}_section, axis: [0, 0, 1], operation: \"{operation}\", label: \"{label}\");
"
    )
}

#[test]
fn a_v_groove_cuts_a_shaft_exactly_with_conical_flanks() {
    // The groove's triangle reaches past the shaft; inside radius 10 it is
    // the triangle (8, 20), (10, 21.5), (10, 18.5), of area 3 about a
    // centroid at radius 28/3.
    let session = run(&format!(
        "{}{}",
        shaft(10.0, 40.0),
        turned("groove", &[(8.0, 20.0), (12.0, 17.0), (12.0, 23.0)], "cut")
    ));
    let removed = 2.0 * PI * (28.0 / 3.0) * 3.0;
    assert_close(
        session.snapshot.measures().volume,
        PI * 100.0 * 40.0 - removed,
        "volume",
    );
    assert_eq!(rung_of(&session, "groove"), "revolve/boolean-coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
    let surfaces = session.report().body.expect("body").surfaces;
    assert!(surfaces.cones >= 2, "the groove's flanks are cones");
}

#[test]
fn a_ball_joins_a_shaft_end_exactly() {
    // A ball of radius 6 about the top of a shaft of radius 5, 10 tall. Its
    // upper half stands clear; of its lower half, all but the ring outside
    // radius 5 lies inside the shaft.
    let ball = "let ball_section = sketch(on: \"XZ\", label: \"ball_section\", entities: [
    arc(center: [0, 10], radius: 6, start_angle: -90, end_angle: 90),
    line(start: [0, 16], end: [0, 4]),
]);
revolve(sketch: ball_section, axis: [0, 0, 1], operation: \"add\", label: \"ball\");
";
    let session = run(&format!("{}{ball}", shaft(5.0, 10.0)));
    let root = 11.0_f64.sqrt();
    let outside_ring = PI * root * 22.0 / 3.0;
    let overlap = 144.0 * PI - outside_ring;
    assert_close(
        session.snapshot.measures().volume,
        250.0 * PI + 288.0 * PI - overlap,
        "volume",
    );
    assert_eq!(rung_of(&session, "ball"), "revolve/boolean-coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
    assert!(session.report().body.expect("body").surfaces.spheres >= 1);
}

#[test]
fn a_chamfer_turns_on_cylinder_stock() {
    // The stock is a plain cylinder; the tool's slant z = 38 − r crosses its
    // top at r = 8 and its side at z = 28, leaving a triangle of area 2
    // about radius 28/3.
    let session = run(&format!(
        "let stock = cylinder(radius: 10, height: 30, label: \"stock\");\n{}",
        turned("chamfer", &[(7.0, 31.0), (11.0, 27.0), (11.0, 31.0)], "cut")
    ));
    let removed = 2.0 * PI * (28.0 / 3.0) * 2.0;
    assert_close(
        session.snapshot.measures().volume,
        PI * 100.0 * 30.0 - removed,
        "volume",
    );
    assert_eq!(rung_of(&session, "chamfer"), "revolve/boolean-coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_bore_through_a_tapered_post_leaves_a_conical_tube() {
    let session = run(&format!(
        "{}{}",
        turned(
            "post",
            &[(0.0, 0.0), (20.0, 0.0), (12.0, 30.0), (0.0, 30.0)],
            "new"
        ),
        turned(
            "bore",
            &[(0.0, -5.0), (4.0, -5.0), (4.0, 35.0), (0.0, 35.0)],
            "cut"
        )
    ));
    let frustum = PI * 30.0 / 3.0 * (400.0 + 20.0 * 12.0 + 144.0);
    assert_close(
        session.snapshot.measures().volume,
        frustum - PI * 16.0 * 30.0,
        "volume",
    );
    assert_eq!(rung_of(&session, "bore"), "revolve/boolean-coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_revolve_about_another_axis_is_not_coaxial() {
    // A cone turned about an axis beside the shaft's still combines, on the
    // faceted tier, which labels it.
    let off_axis = "let cone_section = sketch(on: \"XZ\", label: \"cone_section\", entities: [
    line(start: [20, 10], end: [26, 10]),
    line(start: [26, 10], end: [20, 18]),
    line(start: [20, 18], end: [20, 10]),
]);
revolve(sketch: cone_section, axis: [0, 0, 1], axis_origin: [20, 0, 0], operation: \"cut\", label: \"cone\");
";
    let session = run(&format!("{}{off_axis}", shaft(22.0, 20.0)));
    assert_ne!(rung_of(&session, "cone"), "revolve/boolean-coaxial");
}

#[test]
fn a_round_groove_cuts_a_torus_into_a_shaft() {
    // A circle of radius 2 centred on the shaft's surface: the half inside
    // has area 2π about a centroid 8/(3π) in from radius 10.
    let groove = "let groove_section = sketch(on: \"XZ\", label: \"groove_section\", entities: [
    circle(center: [10, 20], radius: 2),
]);
revolve(sketch: groove_section, axis: [0, 0, 1], operation: \"cut\", label: \"groove\");
";
    let session = run(&format!("{}{groove}", shaft(10.0, 40.0)));
    let removed = 2.0 * PI * (10.0 - 8.0 / (3.0 * PI)) * (2.0 * PI);
    assert_close(
        session.snapshot.measures().volume,
        PI * 100.0 * 40.0 - removed,
        "volume",
    );
    assert_eq!(rung_of(&session, "groove"), "revolve/boolean-coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
    assert!(session.report().body.expect("body").surfaces.tori >= 1);
}

#[test]
fn a_quarter_groove_cuts_a_quarter_shaft() {
    // Both turn through the same quarter from the same plane, so the pair
    // meets in its section as a full turn does, a quarter of the result.
    let quarter_shaft = shaft(10.0, 40.0).replace(
        "axis: [0, 0, 1], label: \"shaft\"",
        "axis: [0, 0, 1], angle: 90, label: \"shaft\"",
    );
    let quarter_groove = turned("groove", &[(8.0, 20.0), (12.0, 17.0), (12.0, 23.0)], "cut")
        .replace(
            "axis: [0, 0, 1], operation",
            "axis: [0, 0, 1], angle: 90, operation",
        );
    let session = run(&format!("{quarter_shaft}{quarter_groove}"));
    let removed = 2.0 * PI * (28.0 / 3.0) * 3.0;
    assert_close(
        session.snapshot.measures().volume,
        (PI * 100.0 * 40.0 - removed) / 4.0,
        "volume",
    );
    assert_eq!(rung_of(&session, "groove"), "revolve/boolean-coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_script_difference_of_coaxial_bodies_is_exact() {
    // Two bodies each turned on their own, then one taken from the other.
    let cone = "let cone_section = sketch(on: \"XZ\", label: \"cone_section\", entities: [
    line(start: [0, 25], end: [6, 45]),
    line(start: [6, 45], end: [0, 45]),
    line(start: [0, 45], end: [0, 25]),
]);
let tip = revolve(sketch: cone_section, axis: [0, 0, 1], label: \"tip\");
difference(target: shaft, tool: tip, label: \"drilled\");
";
    let session = run(&format!("{}{cone}", shaft(10.0, 40.0)));
    // The cone's apex is at z = 25 and it widens by 0.3 per unit up to the
    // shaft's top at z = 40, radius 4.5 there.
    let taken = PI * 4.5 * 4.5 * 15.0 / 3.0;
    assert_close(
        session.snapshot.measures().volume,
        PI * 100.0 * 40.0 - taken,
        "volume",
    );
    assert_eq!(rung_of(&session, "drilled"), "boolean/coaxial");
    assert_eq!(session.report().tier, Tier::Exact);
}

//! The general Boolean's frontier (ADR 0056, Track B): cones, spheres and
//! tori through the analytic engine where the matrix answers, and the
//! numerical intersection rung where it does not.
//!
//! Every expectation is derived here, never recorded from the kernel: frustum
//! arithmetic, Pappus, the spherical cap, and where no closed form exists an
//! independent slice quadrature of the two analytic solids written in the
//! test. Every pair also satisfies conservation, `V(A) + V(B) = V(A ∪ B) +
//! V(A ∩ B)`, and validates clean.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{Tier, ValidationProfile};

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let validation = NativeKernel::validate(&session.snapshot, ValidationProfile::Solid);
    assert!(
        validation.valid,
        "the result must validate: {:?}",
        validation.diagnostics
    );
    session
}

fn volume(session: &Session) -> f64 {
    session.snapshot.measures().volume
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

fn tier_of(session: &Session, label: &str) -> Tier {
    session
        .report()
        .steps
        .iter()
        .find(|step| step.label == label)
        .map(|step| step.tier)
        .expect("the step is recorded")
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not {expected} (off by {}, tolerance {tolerance})",
        actual - expected
    );
}

/// Two bodies `a` and `b` built by `setup`, combined every way. Asserts
/// conservation to `tolerance` and hands back the union, difference and
/// intersection sessions for the caller's own closed forms.
fn conserved(setup: &str, tolerance: f64) -> (Session, Session, Session) {
    let union = run(&format!(
        "{setup}\nunion(target: a, tool: b, label: \"combined\");\n"
    ));
    let difference = run(&format!(
        "{setup}\ndifference(target: a, tool: b, label: \"combined\");\n"
    ));
    let intersection = run(&format!(
        "{setup}\nintersection(target: a, tool: b, label: \"combined\");\n"
    ));
    let a = run(&format!("{setup}\n"));
    let volume_a = a
        .report()
        .steps
        .iter()
        .find(|step| step.label == "a")
        .map(|step| step.volume)
        .expect("a is a step");
    let volume_b = a
        .report()
        .steps
        .iter()
        .find(|step| step.label == "b")
        .map(|step| step.volume)
        .expect("b is a step");
    assert_close(
        volume_a + volume_b,
        volume(&union) + volume(&intersection),
        tolerance,
        "conservation: V(A) + V(B) = V(A ∪ B) + V(A ∩ B)",
    );
    assert_close(
        volume_a,
        volume(&difference) + volume(&intersection),
        tolerance,
        "conservation: V(A) = V(A − B) + V(A ∩ B)",
    );
    (union, difference, intersection)
}

// ---------------------------------------------------------------------------
// B1: the analytic engine widened to cones, spheres and tori
// ---------------------------------------------------------------------------

/// A ball of radius 8 whose centre stands 5 above a plate's top face, so
/// the plate takes a cap of height 3 out of it.
const SPHERE_ON_PLATE: &str = "let a = box(origin: [0, -30, 0], size: [60, 60, 10], label: \"a\");
let ball_section = sketch(on: \"XZ\", label: \"ball_section\", entities: [
    arc(center: [30, 15], radius: 8, start_angle: -90, end_angle: 90),
    line(start: [30, 23], end: [30, 7]),
]);
let b = revolve(sketch: ball_section, axis: [0, 0, 1], axis_origin: [30, 0, 0], label: \"b\");
";

#[test]
fn a_sphere_seated_on_a_plate_joins_exactly() {
    let plate = 60.0 * 60.0 * 10.0;
    let ball = 4.0 / 3.0 * PI * 512.0;
    let cap = PI * 9.0 * (24.0 - 3.0) / 3.0;
    let (union, difference, intersection) = conserved(SPHERE_ON_PLATE, 1.0e-9 * plate);
    assert_close(volume(&union), plate + ball - cap, 1.0e-9 * plate, "union");
    assert_close(volume(&difference), plate - cap, 1.0e-9 * plate, "difference");
    assert_close(volume(&intersection), cap, 1.0e-9 * plate, "intersection");
    for session in [&union, &difference, &intersection] {
        assert_eq!(rung_of(session, "combined"), "boolean/analytic");
        assert_eq!(tier_of(session, "combined"), Tier::Exact);
    }
    assert!(union.report().body.expect("body").surfaces.spheres >= 1);
}

/// A drafted boss — a frustum from radius 12 to 8 over 12 of height —
/// standing on a plate, then counterbored coaxially to radius 5, 6 deep.
const DRAFTED_BOSS: &str = "let plate = box(origin: [-30, -30, 0], size: [60, 60, 10], label: \"plate\");
let boss_section = sketch(on: \"XZ\", label: \"boss_section\", entities: [
    line(start: [0, 10], end: [12, 10]),
    line(start: [12, 10], end: [8, 22]),
    line(start: [8, 22], end: [0, 22]),
    line(start: [0, 22], end: [0, 10]),
]);
revolve(sketch: boss_section, axis: [0, 0, 1], operation: \"add\", label: \"boss\");
let bore_section = sketch(on: \"XZ\", label: \"bore_section\", entities: [
    rect(origin: [0, 16], width: 5, height: 9),
]);
revolve(sketch: bore_section, axis: [0, 0, 1], operation: \"cut\", label: \"counterbore\");
";

#[test]
fn a_coaxial_counterbore_cuts_into_a_drafted_boss_on_a_plate() {
    let session = run(DRAFTED_BOSS);
    let plate = 60.0 * 60.0 * 10.0;
    let frustum = PI * 12.0 / 3.0 * (144.0 + 12.0 * 8.0 + 64.0);
    let counterbore = PI * 25.0 * 6.0;
    assert_close(
        volume(&session),
        plate + frustum - counterbore,
        1.0e-9 * plate,
        "volume",
    );
    assert_eq!(rung_of(&session, "boss"), "revolve/boolean-analytic");
    assert_eq!(rung_of(&session, "counterbore"), "revolve/boolean-analytic");
    assert_eq!(session.report().tier, Tier::Exact);
    assert!(session.report().body.expect("body").surfaces.cones >= 1);
}

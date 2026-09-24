//! When the exact route stands aside, the report says why.
//!
//! A hole drilled through a blended rim meets the blend's torus off its axis,
//! a pair the exact engine does not carry. The numerical intersection rung
//! answers (ADR 0056 Track B; the faceted tier did before it), and the user
//! used to read a message about the approximation alone for a problem that
//! was about the exact route. That route's own reason travels with the
//! outcome, beside the approximation it made necessary.
//!
//! This fixture was two bores of unequal radius crossing until ADR 0047 made
//! that cut exact; `crossing_bores_of_unequal_radius.rs` pins it now.

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::session::Session;
use artificer_protocol::{DiagnosticSeverity, Tier};

#[test]
fn a_hole_through_a_blended_rim_says_why_the_exact_route_declined() {
    let mut session = Session::new();
    let outcome = session.run_script(
        include_str!("../examples/blend_then_drill.art"),
        &BTreeMap::new(),
        &CancellationToken::default(),
    );
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let report = session.report();
    let step = report
        .steps
        .iter()
        .find(|step| step.label == "rim_hole")
        .expect("the hole is a step of its own");
    assert_eq!(
        step.tier,
        Tier::Approximate,
        "the numerical intersection rung answered"
    );
    assert_eq!(step.rung.as_deref(), Some("face-feature/numerical-boolean"));

    let decline = step
        .warnings
        .iter()
        .find(|warning| warning.code == "FACE_FEATURE_EXACT_ROUTE_DECLINED")
        .expect("an approximation says why the exact route stood aside");
    assert_eq!(decline.severity, DiagnosticSeverity::Warning);
    assert!(
        decline.message.contains("torus"),
        "the reason names the carrier it is about: {}",
        decline.message
    );
    let approximation = step
        .warnings
        .iter()
        .find(|warning| warning.code == "BOOLEAN_INTERSECTION_APPROXIMATED")
        .expect("and the approximation is still labelled as one");
    // The approximation says what it approximated and how far it departs,
    // and points at the reason beside it. (The report's warnings are sorted
    // by code, so which is first is not a promise either makes.)
    assert!(
        approximation.message.contains("traced numerically"),
        "{}",
        approximation.message
    );
    assert_eq!(step.warnings.len(), 2, "{:?}", step.warnings);
}

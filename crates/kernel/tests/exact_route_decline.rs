//! When the exact route stands aside, the report says why.
//!
//! A hole drilled through a blended rim meets the blend's torus off its axis,
//! a pair the exact engine does not carry. The faceted tier answers, and the
//! user used to read a message about tessellation for a problem that was
//! about the exact route. That route's own reason now travels with the
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
    assert_eq!(step.tier, Tier::Approximate, "the faceted tier answered");

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
    assert!(
        step.warnings
            .iter()
            .any(|warning| warning.code == "FACE_FEATURE_FACETED_APPROXIMATION"),
        "and the approximation is still labelled as one"
    );
    assert_eq!(
        step.warnings[0].code, "FACE_FEATURE_EXACT_ROUTE_DECLINED",
        "the reason is the first thing said: {:?}",
        step.warnings
    );
}

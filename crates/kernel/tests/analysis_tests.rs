//! Interference studies: the pairwise answer an assembly is checked by.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::analysis::{
    ANALYSIS_SCHEMA_VERSION, FitVerdict, Subject, built_in_profile, clearance_fields,
    interference_study, study_session_steps,
};
use artificer_kernel::api::interference::{ClearanceState, Placement};
use artificer_kernel::api::session::Session;
use artificer_protocol::{PrecisionPolicy, Tier};

/// A plate with a post standing clear above it and a pin driven through it.
const STACK: &str = "\
let plate = box(origin: [-20, -20, 0], size: [40, 40, 10], label: \"plate\");
let post = cylinder(center: [0, 0, 12], radius: 5, height: 20, label: \"post\");
let pin = cylinder(center: [0, 0, 5], radius: 3, height: 20, label: \"pin\");
";

fn session(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

fn names(steps: &[&str]) -> Vec<String> {
    steps.iter().map(|step| (*step).to_owned()).collect()
}

#[test]
fn a_study_measures_every_pair_and_says_which_ones_meet() {
    let session = session(STACK);
    let report = study_session_steps(
        &session,
        &names(&["plate", "post", "pin"]),
        &CancellationToken::default(),
    )
    .expect("a study");

    assert_eq!(report.schema_version, ANALYSIS_SCHEMA_VERSION);
    assert_eq!(report.pairs.len(), 3, "three bodies make three pairs");
    assert_eq!(report.interfering, 2);
    assert_eq!(report.clear, 1);
    assert_eq!(report.touching, 0);

    let pair = |a: &str, b: &str| {
        report
            .pairs
            .iter()
            .find(|pair| pair.a == a && pair.b == b)
            .unwrap_or_else(|| panic!("no pair {a}/{b}"))
    };

    // The post stands 2 mm above the plate it never touches.
    let above = pair("plate", "post");
    assert_eq!(above.state, ClearanceState::Clear);
    assert!((above.distance - 2.0).abs() <= 1.0e-9, "{}", above.distance);
    assert!((above.witness_a.z - 10.0).abs() <= 1.0e-9);
    assert!((above.witness_b.z - 12.0).abs() <= 1.0e-9);

    // The pin is driven through the plate: 5 mm of a 3 mm radius.
    let driven = pair("plate", "pin");
    assert_eq!(driven.state, ClearanceState::Interfering);
    let shared = driven.overlap_volume.expect("the engine carries this pair");
    assert!(
        (shared - PI * 9.0 * 5.0).abs() <= 1.0e-6,
        "overlap {shared}"
    );

    // And it reaches 13 mm into the post above it.
    let inside = pair("post", "pin");
    assert_eq!(inside.state, ClearanceState::Interfering);
    let shared = inside.overlap_volume.expect("coaxial cylinders");
    assert!(
        (shared - PI * 9.0 * 13.0).abs() <= 1.0e-6,
        "overlap {shared}"
    );

    // One clear pair, so it is the tightest.
    let tightest = report.tightest.as_ref().expect("a clear pair");
    assert_eq!(
        (tightest.a.as_str(), tightest.b.as_str()),
        ("plate", "post")
    );
    assert!((tightest.distance - 2.0).abs() <= 1.0e-9);

    // Curved bodies take part, so the study says it rests on chords.
    assert_eq!(report.tier, Tier::Approximate);
    assert!(report.pairs.iter().all(|pair| pair.bound >= 0.0));
}

#[test]
fn a_study_of_planar_bodies_is_exact_and_needs_no_bound() {
    let session = session(
        "let a = box(size: [10, 10, 10], label: \"a\");
let b = box(origin: [30, 0, 0], size: [10, 10, 10], label: \"b\");
",
    );
    let report = study_session_steps(&session, &names(&["a", "b"]), &CancellationToken::default())
        .expect("a study");
    assert_eq!(report.tier, Tier::Exact);
    assert_eq!(report.pairs[0].bound, 0.0);
    assert!((report.pairs[0].distance - 20.0).abs() <= 1.0e-9);
}

#[test]
fn an_interfering_pair_the_boolean_refuses_keeps_its_clearance_and_says_why() {
    // Two boxes sharing a face plane and overlapping: the engine refuses
    // coincident geometry, and the study still reports the interference.
    let session = session(
        "let a = box(size: [20, 20, 20], label: \"a\");
let b = box(origin: [10, 0, 0], size: [20, 20, 20], label: \"b\");
",
    );
    let report = study_session_steps(&session, &names(&["a", "b"]), &CancellationToken::default())
        .expect("a study");
    let pair = &report.pairs[0];
    assert_eq!(pair.state, ClearanceState::Interfering);
    assert!(pair.overlap_volume.is_none(), "the engine cannot say");
    assert_eq!(
        pair.overlap_unavailable.as_deref(),
        Some("BOOLEAN_CONTACT_UNSUPPORTED"),
        "the refusal is named rather than dropped"
    );
}

#[test]
fn a_study_places_its_subjects_where_the_assembly_puts_them() {
    let session = session("let b = box(size: [10, 10, 10], label: \"b\");\n");
    let body = session.snapshot.clone();
    let subjects = vec![
        Subject::new("first", body.clone()),
        Subject::new("second", body).at(Placement {
            columns: Placement::IDENTITY.columns,
            translation: [25.0, 0.0, 0.0],
        }),
    ];
    let report = interference_study(
        &subjects,
        PrecisionPolicy::default(),
        &CancellationToken::default(),
    );
    assert_eq!(report.pairs.len(), 1);
    assert_eq!(report.pairs[0].state, ClearanceState::Clear);
    assert!((report.pairs[0].distance - 15.0).abs() <= 1.0e-9);

    // Overlapping the same two, the placed copy's overlap volume is a real
    // Boolean over the moved body.
    let body = session.snapshot.clone();
    let subjects = vec![
        Subject::new("first", body.clone()),
        Subject::new("second", body).at(Placement {
            columns: Placement::IDENTITY.columns,
            translation: [4.0, 4.0, 4.0],
        }),
    ];
    let report = interference_study(
        &subjects,
        PrecisionPolicy::default(),
        &CancellationToken::default(),
    );
    assert_eq!(report.pairs[0].state, ClearanceState::Interfering);
    let shared = report.pairs[0]
        .overlap_volume
        .expect("boxes offset on every axis are an ordinary Boolean");
    assert!(
        (shared - 6.0_f64.powi(3)).abs() <= 1.0e-6,
        "overlap {shared}"
    );
}

#[test]
fn a_study_needs_two_bodies_and_names_a_step_it_cannot_find() {
    let session = session("let b = box(size: [10, 10, 10], label: \"b\");\n");
    assert!(
        study_session_steps(&session, &names(&["b"]), &CancellationToken::default()).is_err(),
        "one body is not a study"
    );
    let error = study_session_steps(
        &session,
        &names(&["b", "nowhere"]),
        &CancellationToken::default(),
    )
    .expect_err("an unknown step");
    assert!(error.message.contains("nowhere"), "{}", error.message);
}

#[test]
fn a_study_paints_each_subject_by_its_clearance_to_the_others() {
    let session = session(STACK);
    let subject = |label: &str| {
        let id = session.step_snapshots.get(label).expect("a step");
        Subject::new(
            label,
            session.snapshot_cache.get(id).expect("a snapshot").clone(),
        )
    };
    let subjects = vec![subject("plate"), subject("post"), subject("pin")];
    let fields = clearance_fields(&subjects, &CancellationToken::default());

    assert_eq!(fields.len(), 3, "one field per subject");
    for (subject, field) in subjects.iter().zip(&fields) {
        let scene = artificer_kernel::NativeKernel::debug_scene(&subject.snapshot);
        assert_eq!(
            field.len(),
            scene.triangles.len() * 3,
            "{} reads at every facet corner",
            subject.name
        );
    }

    // The post stands 2 mm clear of the plate, so nothing on it is nearer
    // than that except where the pin is driven through it.
    let post = &fields[1];
    assert!(
        post.iter().any(|value| *value < 0.0),
        "the pin passes through the post"
    );
    let clear = post
        .iter()
        .copied()
        .filter(|value| *value > 0.0)
        .fold(f64::INFINITY, f64::min);
    assert!(clear <= 2.0 + 1.0e-6, "nearest clear reading {clear}");

    // A cancelled study leaves the fields it never reached empty rather
    // than reporting an unmeasured body as clear.
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert!(
        clearance_fields(&subjects, &cancelled)
            .iter()
            .all(Vec::is_empty)
    );
}

#[test]
fn a_profile_turns_a_measurement_into_a_pass_or_a_failure() {
    // Three plates in a row, 0.15 mm and then 0.40 mm apart. An FDM press
    // fit wants 0.10 to 0.20 and a sliding fit 0.30 to 0.50, so one gap is
    // the fit under each and the other is wrong under it.
    let session = session(
        "let a = box(size: [10, 10, 10], label: \"a\");
let b = box(origin: [10.15, 0, 0], size: [10, 10, 10], label: \"b\");
let c = box(origin: [20.55, 0, 0], size: [10, 10, 10], label: \"c\");
",
    );
    let mut report = study_session_steps(
        &session,
        &names(&["a", "b", "c"]),
        &CancellationToken::default(),
    )
    .expect("a study");

    // Unjudged, a study has measurements and no verdicts.
    assert!(report.pairs.iter().all(|pair| pair.verdict.is_none()));
    assert_eq!(report.failing, 0);
    assert!(report.profile.is_none());

    let press = built_in_profile("fdm-press").expect("a shipped profile");
    report.judge(Some(press.clone()));
    assert_eq!(report.profile.as_ref(), Some(&press));

    fn verdict(
        report: &artificer_kernel::api::analysis::InterferenceReport,
        a: &str,
        b: &str,
    ) -> FitVerdict {
        report
            .pairs
            .iter()
            .find(|pair| pair.a == a && pair.b == b)
            .and_then(|pair| pair.verdict)
            .unwrap_or_else(|| panic!("no verdict for {a}/{b}"))
    }
    assert_eq!(
        verdict(&report, "a", "b"),
        FitVerdict::Pass,
        "0.15 is the fit"
    );
    assert_eq!(
        verdict(&report, "b", "c"),
        FitVerdict::Loose,
        "0.40 is twice what this press fit needs"
    );
    assert_eq!(
        verdict(&report, "a", "c"),
        FitVerdict::Loose,
        "10.55 mm is not a fit at all"
    );
    assert_eq!(report.failing, 0, "nothing is too close");
    assert_eq!(report.loose, 2);
    assert!(report.worst_fit().is_none(), "nothing failed");

    // A sliding fit wants 0.30 to 0.50, so the 0.15 gap now fails and the
    // report names it and where on the two bodies it was read.
    report.judge(built_in_profile("fdm-sliding"));
    assert_eq!(verdict(&report, "a", "b"), FitVerdict::TooClose);
    assert_eq!(
        verdict(&report, "b", "c"),
        FitVerdict::Pass,
        "the gap the press fit called loose is this one's fit"
    );
    assert_eq!(report.failing, 1);
    assert_eq!(report.loose, 1);
    let worst = report.worst_fit().expect("a failing pair");
    assert_eq!((worst.a.as_str(), worst.b.as_str()), ("a", "b"));
    assert!((worst.distance - 0.15).abs() <= 1.0e-9);
    assert!((worst.witness_a.x - 10.0).abs() <= 1.0e-9);
    assert!((worst.witness_b.x - 10.15).abs() <= 1.0e-9);

    // And withdrawing the profile leaves the measurements standing.
    report.judge(None);
    assert!(report.pairs.iter().all(|pair| pair.verdict.is_none()));
    assert_eq!(report.failing, 0);
    assert!((report.pairs[0].distance - 0.15).abs() <= 1.0e-9);
}

#[test]
fn contact_passes_only_the_profile_that_asks_for_no_gap() {
    let session = session(
        "let a = box(size: [10, 10, 10], label: \"a\");
let b = box(origin: [10, 0, 0], size: [10, 10, 10], label: \"b\");
",
    );
    let mut report =
        study_session_steps(&session, &names(&["a", "b"]), &CancellationToken::default())
            .expect("a study");
    assert_eq!(report.pairs[0].state, ClearanceState::Touching);

    report.judge(built_in_profile("assembly"));
    assert_eq!(
        report.pairs[0].verdict,
        Some(FitVerdict::Pass),
        "touching parts do not occupy the same space"
    );
    assert_eq!(report.failing, 0);

    report.judge(built_in_profile("fdm-press"));
    assert_eq!(
        report.pairs[0].verdict,
        Some(FitVerdict::TooClose),
        "a press fit needs a gap and there is none"
    );
    assert_eq!(report.failing, 1);
}

#[test]
fn an_interfering_pair_fails_every_profile_including_the_open_ended_one() {
    let session = session(
        "let a = box(size: [20, 20, 20], label: \"a\");
let b = box(origin: [10, 10, 10], size: [20, 20, 20], label: \"b\");
",
    );
    let mut report =
        study_session_steps(&session, &names(&["a", "b"]), &CancellationToken::default())
            .expect("a study");
    assert_eq!(report.pairs[0].state, ClearanceState::Interfering);

    for profile in artificer_kernel::api::analysis::BUILT_IN_PROFILES {
        report.judge(Some(profile.profile()));
        assert_eq!(
            report.pairs[0].verdict,
            Some(FitVerdict::TooClose),
            "{} let an overlap pass",
            profile.key
        );
        assert_eq!(report.failing, 1);
    }
    // The open-ended profile publishes no upper bound rather than an
    // infinite one, because infinity is not a JSON number.
    let assembly = built_in_profile("assembly").expect("a shipped profile");
    assert!(assembly.maximum.is_none());
    assert!(
        serde_json::to_value(&assembly).unwrap()["maximum"].is_null(),
        "an absent bound, not a null number"
    );
}

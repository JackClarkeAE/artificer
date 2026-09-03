//! Interference over a motion: a mechanism stepped through its travel.

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::analysis::{Subject, built_in_profile};
use artificer_kernel::api::interference::{ClearanceState, Placement};
use artificer_kernel::api::session::Session;
use artificer_kernel::api::sweep::{SWEEP_SCHEMA_VERSION, SweepStep, interference_sweep};
use artificer_protocol::PrecisionPolicy;

fn build(source: &str) -> artificer_kernel::Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

fn cuboid(origin: [f64; 3], size: [f64; 3]) -> artificer_kernel::Snapshot {
    build(&format!(
        "let b = box(origin: [{}, {}, {}], size: [{}, {}, {}], label: \"b\");\n",
        origin[0], origin[1], origin[2], size[0], size[1], size[2]
    ))
}

/// An arm 20 mm long swinging about the world Z axis, and a post standing
/// in the quarter turn ahead of it.
///
/// At rest the arm points along +x and clears the post by 10 mm across.
/// Swung far enough it goes through the post, which is the collision the
/// sweep has to find and stop at.
fn mechanism() -> Vec<Subject> {
    vec![
        Subject::new("post", cuboid([-3.0, 12.0, -10.0], [6.0, 8.0, 20.0])),
        Subject::new("arm", cuboid([0.0, -2.0, -2.0], [20.0, 4.0, 4.0])),
    ]
}

/// The arm turned by `radians`; the post never moves.
fn swing(radians: f64) -> SweepStep {
    let half = radians / 2.0;
    SweepStep::new(
        vec![radians],
        vec![
            Placement::IDENTITY,
            Placement::from_quaternion([half.cos(), 0.0, 0.0, half.sin()], [0.0, 0.0, 0.0])
                .expect("a unit quaternion"),
        ],
    )
}

fn steps(to: f64, count: usize) -> Vec<SweepStep> {
    (0..count)
        .map(|step| swing(to * step as f64 / (count - 1) as f64))
        .collect()
}

fn precision() -> PrecisionPolicy {
    PrecisionPolicy::default()
}

fn ignore(_: usize, _: usize) {}

#[test]
fn a_sweep_that_clears_reports_the_tightest_the_motion_ever_got() {
    // A quarter of the way round, the arm's side passes about 2 mm from
    // the post's corner and never reaches it.
    let subjects = mechanism();
    let steps = steps(1.0, 30);
    let sweep = interference_sweep(
        &subjects,
        &steps,
        precision(),
        None,
        &CancellationToken::default(),
        &mut ignore,
    );
    let report = &sweep.report;

    assert_eq!(report.schema_version, SWEEP_SCHEMA_VERSION);
    assert!(report.collision.is_none(), "{:?}", report.collision);
    assert!(report.clears());
    assert_eq!(report.steps_measured, report.steps_offered);
    assert_eq!(report.steps_offered, 30);
    assert_eq!(report.pairs.len(), 1, "two bodies make one pair");

    let tightest = report.tightest().expect("a clear pair");
    assert_eq!((tightest.a.as_str(), tightest.b.as_str()), ("post", "arm"));
    assert_eq!(tightest.state, ClearanceState::Clear);
    // At rest they are 10 mm apart across the corner; the swing brings the
    // arm much closer without touching.
    assert!(
        tightest.distance > 0.0 && tightest.distance < 3.0,
        "tightest {}",
        tightest.distance
    );
    // And it happened at the end of the travel, not at the start.
    assert_eq!(
        tightest.step,
        steps.len() - 1,
        "the arm is still closing at the last step"
    );
    assert_eq!(tightest.drivers, vec![1.0]);

    // The picture agrees with the table. The field is sampled at facet
    // corners, so where the closest approach runs from a corner of one
    // part to the middle of the other's face — which is this one — the
    // field reads looser than the surfaces came. What it must never do is
    // read closer.
    assert_eq!(sweep.fields.len(), 2, "one field per subject");
    let closest = |sweep: &artificer_kernel::api::sweep::Sweep| {
        sweep
            .fields
            .iter()
            .flatten()
            .copied()
            .fold(f64::INFINITY, f64::min)
    };
    let swept = closest(&sweep);
    assert!(
        swept >= tightest.distance - 1.0e-9,
        "a corner read closer than the surfaces ever came: {swept} against {}",
        tightest.distance
    );

    // And it is the worst of the whole motion rather than of one pose:
    // the same assembly measured only at rest reads looser everywhere.
    let at_rest = interference_sweep(
        &subjects,
        &steps[..1],
        precision(),
        None,
        &CancellationToken::default(),
        &mut ignore,
    );
    assert!(
        swept < closest(&at_rest),
        "the swing brought the parts closer than rest: {swept} against {}",
        closest(&at_rest)
    );
}

#[test]
fn a_sweep_stops_at_the_first_collision_and_says_where_it_was() {
    let subjects = mechanism();
    let steps = steps(std::f64::consts::FRAC_PI_2, 40);
    let sweep = interference_sweep(
        &subjects,
        &steps,
        precision(),
        None,
        &CancellationToken::default(),
        &mut ignore,
    );
    let report = &sweep.report;

    let collision = report.collision.as_ref().expect("the arm goes through it");
    assert_eq!(
        (collision.a.as_str(), collision.b.as_str()),
        ("post", "arm")
    );
    assert!(collision.step > 0, "not at rest");
    assert!(!report.clears());

    // The rest of the travel is a different mechanism's question: once the
    // parts have passed through one another nothing after that is a pose
    // the real thing reaches.
    assert_eq!(
        report.steps_measured,
        collision.step + 1,
        "the sweep stopped at the collision"
    );
    assert!(report.steps_measured < report.steps_offered);
    assert_eq!(collision.drivers.len(), 1);
    assert!(
        collision.drivers[0] > 1.0,
        "the arm clears the post for the first radian: {:?}",
        collision.drivers
    );

    // The picture is painted at the colliding step, so somewhere on the
    // arm reads as penetration rather than as a gap.
    let deepest = sweep
        .fields
        .iter()
        .flatten()
        .copied()
        .fold(f64::INFINITY, f64::min);
    assert!(deepest < 0.0, "nothing reads as a collision: {deepest}");
}

#[test]
fn a_profile_judges_a_sweep_at_the_step_each_pair_came_closest() {
    let subjects = mechanism();
    let steps = steps(1.0, 30);
    // The arm passes about 2 mm from the post, which no printed fit calls
    // a fit: an FDM sliding fit wants 0.30 to 0.50, so 2 mm is loose.
    let sliding = built_in_profile("fdm-sliding").expect("a shipped profile");
    let sweep = interference_sweep(
        &subjects,
        &steps,
        precision(),
        Some(&sliding),
        &CancellationToken::default(),
        &mut ignore,
    );
    assert_eq!(sweep.report.profile.as_ref(), Some(&sliding));
    assert_eq!(sweep.report.failing, 0, "nothing is too close");
    assert_eq!(
        sweep.report.pairs[0].verdict,
        Some(artificer_kernel::api::analysis::FitVerdict::Loose)
    );

    // A fit that wants more room than the mechanism leaves fails on the
    // same measurement.
    let wide = artificer_kernel::api::analysis::ClearanceProfile {
        key: "wide".to_owned(),
        name: "Cable route".to_owned(),
        minimum: 5.0,
        maximum: None,
        note: "A harness has to pass through the swing.".to_owned(),
    };
    let sweep = interference_sweep(
        &subjects,
        &steps,
        precision(),
        Some(&wide),
        &CancellationToken::default(),
        &mut ignore,
    );
    assert_eq!(sweep.report.failing, 1);
    assert!(!sweep.report.clears(), "a fit it fails is not clear");
}

#[test]
fn a_cancelled_sweep_says_so_and_keeps_what_it_measured() {
    let subjects = mechanism();
    let steps = steps(1.0, 30);
    let cancellation = CancellationToken::default();
    let mut seen = 0;
    let mut progress = |step: usize, total: usize| {
        assert_eq!(total, 30);
        seen += 1;
        if step == 5 {
            cancellation.cancel();
        }
    };
    let sweep = interference_sweep(
        &subjects,
        &steps,
        precision(),
        None,
        &cancellation,
        &mut progress,
    );
    assert!(sweep.report.cancelled);
    assert!(!sweep.report.clears(), "an unfinished sweep clears nothing");
    assert!(
        sweep.report.steps_measured >= 6 && sweep.report.steps_measured < 30,
        "measured {}",
        sweep.report.steps_measured
    );
    assert_eq!(sweep.report.pairs.len(), 1, "what it did measure stands");
    // Cancelling inside the sixth step still finishes it: the token is
    // read at the top of the next one, which is where the sweep stops.
    assert_eq!(seen, sweep.report.steps_measured);
}

#[test]
fn a_step_that_does_not_place_every_body_is_skipped_rather_than_half_applied() {
    let subjects = mechanism();
    let mut steps = steps(1.0, 10);
    steps[3] = SweepStep::new(vec![0.3], vec![Placement::IDENTITY]);
    let sweep = interference_sweep(
        &subjects,
        &steps,
        precision(),
        None,
        &CancellationToken::default(),
        &mut ignore,
    );
    assert_eq!(sweep.report.steps_offered, 10);
    assert_eq!(
        sweep.report.steps_measured, 9,
        "the short step is not a pose"
    );
    assert!(sweep.report.collision.is_none());
}

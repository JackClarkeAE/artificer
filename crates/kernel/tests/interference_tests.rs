//! Clearance between two bodies: the gap, where it is, and whether the
//! bodies are apart, touching or inside one another.

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::interference::{ClearanceState, FacetIndex, Placement, clearance};
use artificer_kernel::api::session::Session;
use artificer_protocol::{PrecisionPolicy, Tier};

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

fn index(snapshot: &artificer_kernel::Snapshot) -> FacetIndex {
    FacetIndex::build(snapshot, Placement::IDENTITY)
}

fn precision() -> PrecisionPolicy {
    PrecisionPolicy::default()
}

#[test]
fn two_planar_bodies_apart_report_an_exact_gap_and_where_it_is() {
    let left = index(&cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
    let right = index(&cuboid([15.0, 2.0, 2.0], [10.0, 6.0, 6.0]));
    let report = clearance(&left, &right, precision());

    assert_eq!(report.state, ClearanceState::Clear);
    assert_eq!(report.tier, Tier::Exact, "planar facets are the surface");
    assert_eq!(report.bound, 0.0, "an exact answer needs no bound");
    assert!(
        (report.distance - 5.0).abs() <= 1.0e-9,
        "gap {}",
        report.distance
    );
    // The witnesses sit on the two facing walls, opposite one another.
    assert!(
        (report.witness_a.x - 10.0).abs() <= 1.0e-9,
        "{:?}",
        report.witness_a
    );
    assert!(
        (report.witness_b.x - 15.0).abs() <= 1.0e-9,
        "{:?}",
        report.witness_b
    );
    assert!((report.witness_a.y - report.witness_b.y).abs() <= 1.0e-9);
    assert!((report.witness_a.z - report.witness_b.z).abs() <= 1.0e-9);
}

#[test]
fn bodies_that_meet_are_touching_and_bodies_that_overlap_are_interfering() {
    let base = cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
    let flush = clearance(
        &index(&base),
        &index(&cuboid([10.0, 0.0, 0.0], [10.0, 10.0, 10.0])),
        precision(),
    );
    assert_eq!(flush.state, ClearanceState::Touching);
    assert!(flush.distance <= 1.0e-9, "{}", flush.distance);

    let overlapping = clearance(
        &index(&base),
        &index(&cuboid([8.0, 0.0, 0.0], [10.0, 10.0, 10.0])),
        precision(),
    );
    assert_eq!(overlapping.state, ClearanceState::Interfering);
    assert!(overlapping.distance <= 1.0e-9);

    // A body wholly inside another is interfering even though no facet of
    // the outer body is near the inner one's surface.
    let swallowed = clearance(
        &index(&cuboid([0.0, 0.0, 0.0], [40.0, 40.0, 40.0])),
        &index(&cuboid([10.0, 10.0, 10.0], [5.0, 5.0, 5.0])),
        precision(),
    );
    assert_eq!(swallowed.state, ClearanceState::Interfering);
}

#[test]
fn a_curved_gap_is_bounded_by_the_sagitta_of_the_chords_it_was_read_from() {
    // Two parallel cylinders of radius 10, axes 40 apart: the true gap
    // between the surfaces is 20.
    let left_body = build("let c = cylinder(radius: 10, height: 30, label: \"c\");\n");
    let left = index(&left_body);
    let right = index(&build(
        "let c = cylinder(center: [40, 0, 0], radius: 10, height: 30, label: \"c\");\n",
    ));
    let report = clearance(&left, &right, precision());

    assert_eq!(report.state, ClearanceState::Clear);
    assert_eq!(report.tier, Tier::Approximate, "chords are not the surface");
    assert!(report.bound > 0.0, "a curved pair publishes its bound");
    // The bound is the sagitta of the chords the display scene spent, once
    // per body, and nothing smaller: the kernel names that figure itself.
    let sagitta = artificer_kernel::NativeKernel::display_chord_deviation(&left_body);
    assert!(
        sagitta > 1.0e-3,
        "a display chord on r = 10 is millimetric, not {sagitta}"
    );
    assert!(
        (report.bound - 2.0 * sagitta).abs() <= 1.0e-9,
        "bound {} against two sagittas of {sagitta}",
        report.bound
    );
    // Facets of a convex face are inscribed, so the measured gap is never
    // smaller than the true one and never larger than it by more than the
    // bound.
    assert!(
        report.distance >= 20.0 - 1.0e-9,
        "under-reported: {}",
        report.distance
    );
    assert!(
        report.distance <= 20.0 + report.bound + 1.0e-9,
        "over-reported past its own bound: {} against {}",
        report.distance,
        report.bound
    );
}

/// Two cylinders of radius 5, height 20, with a true gap of 0.020 mm: the
/// second is centred 10.02 from the first at `degrees` round the z axis.
/// Turning the line of centres walks the closest approach from a vertex
/// of each tessellation onto the middle of a chord, which is where the
/// facets over-read the gap most.
fn cylinders_a_hair_apart(
    degrees: f64,
) -> (artificer_kernel::Snapshot, artificer_kernel::Snapshot) {
    let theta = degrees.to_radians();
    (
        build("let c = cylinder(radius: 5, height: 20, label: \"c\");\n"),
        build(&format!(
            "let c = cylinder(center: [{}, {}, 0], radius: 5, height: 20, label: \"c\");\n",
            10.02 * theta.cos(),
            10.02 * theta.sin()
        )),
    )
}

#[test]
fn the_published_bound_holds_at_every_angle_and_a_running_fit_is_never_passed_on_chords() {
    use artificer_kernel::api::analysis::{FitVerdict, Subject, built_in_profile};

    let running = built_in_profile("machined-running").expect("a shipped profile");
    assert!((running.minimum - 0.02).abs() <= 1.0e-12, "{running:?}");
    for degrees in [0.0, 0.5, 1.0, 1.3, 1.5, 2.0, 2.6, 3.0, 4.0, 5.0] {
        let (first, second) = cylinders_a_hair_apart(degrees);
        let report = clearance(&index(&first), &index(&second), precision());
        // The true gap is 0.020: never above the measurement, never below
        // the measurement less the bound.
        assert!(
            report.distance >= 0.02 - 1.0e-9,
            "at {degrees}°: facets inside the surfaces read {} below the true gap",
            report.distance
        );
        assert!(
            report.distance - report.bound <= 0.02 + 1.0e-9,
            "at {degrees}°: the bound {} does not cover the over-read of {}",
            report.bound,
            report.distance
        );
        assert_eq!(
            report.state,
            ClearanceState::Clear,
            "at {degrees}°: 0.02 is clear of a 0.01 bound"
        );
        // A running fit wants 0.02 at least, and the facets can only say
        // the gap is at least 0.02 less their bound: too close, at every
        // angle, rather than a pass that depends on where a chord fell.
        assert_eq!(
            FitVerdict::of(report.state, report.distance, report.bound, &running),
            FitVerdict::TooClose,
            "at {degrees}°: distance {} bound {}",
            report.distance,
            report.bound
        );

        // The same through a study, which is what the wire answers with.
        let mut study = artificer_kernel::api::analysis::interference_study(
            &[Subject::new("a", first), Subject::new("b", second)],
            precision(),
            &CancellationToken::default(),
        );
        study.judge(Some(running.clone()));
        assert_eq!(study.pairs[0].verdict, Some(FitVerdict::TooClose));
        assert_eq!(study.failing, 1, "at {degrees}°");
        assert!((study.pairs[0].bound - report.bound).abs() <= 1.0e-12);
    }
}

#[test]
fn a_pin_in_a_bore_can_under_read_its_gap_and_the_bound_covers_that_too() {
    // A 10 mm bore through a plate and a pin of radius 4.98 in it: the
    // true gap is 0.020 all round. The bore's chords lie in the void, so
    // a pin turned to put a vertex opposite a chord's middle reads closer
    // than the surfaces are.
    let plate = build(
        "let p = box(origin: [-10, -10, 0], size: [20, 20, 10], label: \"p\");
let h = drill(face: faces(\">Z\"), center: [0, 0], diameter: 10, depth: 10, label: \"h\");
",
    );
    let bore = index(&plate);
    let pin =
        build("let c = cylinder(center: [0, 0, -5], radius: 4.98, height: 20, label: \"c\");\n");
    let mut under_read = false;
    for degrees in [0.0, 1.0, 2.0, 2.5, 3.0] {
        let half = f64::to_radians(degrees) / 2.0;
        let turned = FacetIndex::build(
            &pin,
            Placement::from_quaternion([half.cos(), 0.0, 0.0, half.sin()], [0.0, 0.0, 0.0])
                .expect("a unit quaternion"),
        );
        let report = clearance(&bore, &turned, precision());
        assert_eq!(
            report.state,
            ClearanceState::Clear,
            "at {degrees}°: {report:?}"
        );
        under_read |= report.distance < 0.02 - 1.0e-6;
        assert!(
            report.distance - report.bound <= 0.02 + 1.0e-9
                && report.distance + report.bound >= 0.02 - 1.0e-9,
            "at {degrees}°: the true gap 0.02 is outside [{} - {b}, {} + {b}]",
            report.distance,
            report.distance,
            b = report.bound
        );
    }
    assert!(
        under_read,
        "a bore's chords lie in the void, so some turn of the pin reads under the true gap"
    );
}

#[test]
fn contact_that_rests_on_chords_is_never_passed_and_the_bound_says_which_chords() {
    // A cylinder standing on a plate touches it cap to face, both planar.
    // But the wall's chords end on the same rim, at no distance from the
    // plate at all, and the descent does not know a wall bulges sideways
    // rather than down: the pair carries the wall's sagitta as its bound,
    // one body's worth and not two, and no profile passes it. The
    // alternative — passing a contact the facets cannot vouch for — is the
    // optimism this bound exists to remove.
    use artificer_kernel::api::analysis::{FitVerdict, built_in_profile};

    let plate = index(&cuboid([-20.0, -20.0, -10.0], [40.0, 40.0, 10.0]));
    let post_body = build("let c = cylinder(radius: 5, height: 20, label: \"c\");\n");
    let post = index(&post_body);
    let report = clearance(&plate, &post, precision());
    assert_eq!(report.state, ClearanceState::Touching);
    assert_eq!(report.tier, Tier::Approximate, "a body is curved");
    assert!(report.distance <= 1.0e-9, "{report:?}");
    let sagitta = artificer_kernel::NativeKernel::display_chord_deviation(&post_body);
    assert!(
        (report.bound - sagitta).abs() <= 1.0e-9,
        "one wall's sagitta {sagitta}, not two and not none: {report:?}"
    );
    assert!(report.may_overlap());
    let assembly = built_in_profile("assembly").expect("a shipped profile");
    assert_eq!(
        FitVerdict::of(report.state, report.distance, report.bound, &assembly),
        FitVerdict::TooClose
    );

    // Two plates in contact are exact, and the same profile passes them.
    let lid = index(&cuboid([-20.0, -20.0, 0.0], [40.0, 40.0, 10.0]));
    let report = clearance(&plate, &lid, precision());
    assert_eq!(report.state, ClearanceState::Touching);
    assert_eq!(report.bound, 0.0);
    assert!(!report.may_overlap());
    assert_eq!(
        FitVerdict::of(report.state, report.distance, report.bound, &assembly),
        FitVerdict::Pass
    );

    // Two cylinders side by side whose surfaces meet are a contact the
    // chords cannot tell from an overlap: touching, with a bound, and no
    // profile passes it.
    let other = index(&build(
        "let c = cylinder(center: [10, 0, 0], radius: 5, height: 20, label: \"c\");\n",
    ));
    let report = clearance(&post, &other, precision());
    assert_eq!(report.state, ClearanceState::Touching, "{report:?}");
    assert!(report.bound > 0.0);
    assert!(report.may_overlap());
    assert_eq!(
        FitVerdict::of(report.state, report.distance, report.bound, &assembly),
        FitVerdict::TooClose
    );
}

#[test]
fn a_placement_moves_the_body_the_index_is_built_over() {
    let unit = cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
    let here = index(&unit);
    let moved = FacetIndex::build(
        &unit,
        Placement {
            columns: Placement::IDENTITY.columns,
            translation: [30.0, 0.0, 0.0],
        },
    );
    let report = clearance(&here, &moved, precision());
    assert_eq!(report.state, ClearanceState::Clear);
    assert!(
        (report.distance - 20.0).abs() <= 1.0e-9,
        "{}",
        report.distance
    );

    // A quarter turn about Z carries the body to x in -10..0, so the same
    // gap needs ten more millimetres of travel. The turn is about the world
    // origin, which is what an occurrence pose means.
    let turned = FacetIndex::build(
        &unit,
        Placement::from_quaternion(
            [
                std::f64::consts::FRAC_PI_4.cos(),
                0.0,
                0.0,
                std::f64::consts::FRAC_PI_4.sin(),
            ],
            [40.0, 0.0, 0.0],
        )
        .expect("a unit quaternion"),
    );
    let report = clearance(&here, &turned, precision());
    assert!(
        (report.distance - 20.0).abs() <= 1.0e-9,
        "turned gap {}",
        report.distance
    );
}

#[test]
fn a_pair_of_real_parts_answers_without_comparing_every_facet_pair() {
    // Two flanged hubs, 200 mm apart. Brute force would be tens of millions
    // of triangle pairs; the hierarchy has to make this ordinary.
    let source = include_str!("../examples/flanged_hub.art");
    let hub = build(source);
    let left = index(&hub);
    let right = FacetIndex::build(
        &hub,
        Placement {
            columns: Placement::IDENTITY.columns,
            translation: [200.0, 0.0, 0.0],
        },
    );
    assert!(
        left.facet_count() > 1_000,
        "a representative part, not a toy: {}",
        left.facet_count()
    );

    let started = std::time::Instant::now();
    let report = clearance(&left, &right, precision());
    let elapsed = started.elapsed();

    assert_eq!(report.state, ClearanceState::Clear);
    // The hub spans 90 mm across the flange, so the gap is 200 less the two
    // half-widths that face one another.
    assert!(
        (report.distance - 110.0).abs() <= 1.0e-3,
        "gap {}",
        report.distance
    );
    assert!(
        elapsed.as_millis() < 500,
        "a pair of real parts took {elapsed:?}"
    );
}

#[test]
fn an_empty_body_is_never_close_to_anything() {
    let empty = FacetIndex::build(
        &artificer_kernel::NativeKernel::empty(),
        Placement::IDENTITY,
    );
    assert!(empty.is_empty());
    let report = clearance(
        &empty,
        &index(&cuboid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0])),
        precision(),
    );
    assert_eq!(report.state, ClearanceState::Clear);
    assert!(report.distance.is_infinite());
}

#[test]
fn a_clearance_field_reads_at_every_corner_and_signs_penetration() {
    // Two cubes 5 mm apart on x. The near wall of the left one reads 5;
    // its far wall reads 15, the whole span of the right cube away.
    let left = cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
    let right = index(&cuboid([15.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
    let scene = artificer_kernel::NativeKernel::debug_scene(&left);
    let field = artificer_kernel::api::interference::clearance_field(
        &scene,
        Placement::IDENTITY,
        &[&right],
    );

    assert_eq!(
        field.len(),
        scene.triangles.len() * 3,
        "one reading per facet corner"
    );
    let nearest = field.iter().copied().fold(f64::INFINITY, f64::min);
    let farthest = field.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert!((nearest - 5.0).abs() <= 1.0e-9, "nearest {nearest}");
    assert!((farthest - 15.0).abs() <= 1.0e-9, "farthest {farthest}");
    assert!(field.iter().all(|value| *value > 0.0), "nothing overlaps");

    // Driven 3 mm into a cube that swallows it in y and z, the corners
    // inside read negative, and the deepest is the 3 mm of travel.
    let driven = index(&cuboid([7.0, -5.0, -5.0], [10.0, 20.0, 20.0]));
    let field = artificer_kernel::api::interference::clearance_field(
        &scene,
        Placement::IDENTITY,
        &[&driven],
    );
    let deepest = field.iter().copied().fold(f64::INFINITY, f64::min);
    assert!((deepest + 3.0).abs() <= 1.0e-9, "deepest {deepest}");
    assert!(
        field.iter().any(|value| *value > 0.0),
        "the far side of the cube is still clear of it"
    );
}

#[test]
fn a_clearance_field_takes_the_nearest_of_several_bodies_and_is_infinite_alone() {
    let subject = cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 10.0]);
    let scene = artificer_kernel::NativeKernel::debug_scene(&subject);
    let far = index(&cuboid([40.0, 0.0, 0.0], [10.0, 10.0, 10.0]));
    let near = index(&cuboid([-4.0, 0.0, 0.0], [2.0, 10.0, 10.0]));

    let alone =
        artificer_kernel::api::interference::clearance_field(&scene, Placement::IDENTITY, &[]);
    assert!(
        alone.iter().all(|value| value.is_infinite()),
        "nothing to measure against is not a clearance of zero"
    );

    let both = artificer_kernel::api::interference::clearance_field(
        &scene,
        Placement::IDENTITY,
        &[&far, &near],
    );
    let nearest = both.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(
        (nearest - 2.0).abs() <= 1.0e-9,
        "the near body wins: {nearest}"
    );
}

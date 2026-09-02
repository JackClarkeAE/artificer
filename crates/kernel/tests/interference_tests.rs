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
fn a_curved_gap_is_bounded_by_the_chord_budget_it_was_tessellated_to() {
    // Two parallel cylinders of radius 10, axes 40 apart: the true gap
    // between the surfaces is 20.
    let left = index(&build(
        "let c = cylinder(radius: 10, height: 30, label: \"c\");\n",
    ));
    let right = index(&build(
        "let c = cylinder(center: [40, 0, 0], radius: 10, height: 30, label: \"c\");\n",
    ));
    let report = clearance(&left, &right, precision());

    assert_eq!(report.state, ClearanceState::Clear);
    assert_eq!(report.tier, Tier::Approximate, "chords are not the surface");
    assert!(report.bound > 0.0, "a curved pair publishes its bound");
    // Facets are inscribed, so the measured gap is never smaller than the
    // true one and never larger than the true one plus both chord budgets.
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

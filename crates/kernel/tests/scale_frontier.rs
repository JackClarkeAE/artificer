//! Robustness at scale (ADR 0056, Track R): bodies of hundreds to thousands
//! of faces built, combined, validated and displayed, with every measure
//! checked in closed form and every stage timed.
//!
//! Nothing here asserts a wall-clock time: the CI machines vary, and a
//! timing assertion that passes on one and fails on another says nothing
//! about the kernel. The times are printed as a table (run with
//! `--nocapture`), so a change can be judged by numbers taken before and
//! after it on one machine. What *is* asserted is correctness at size —
//! volumes by closed form, the validator clean, the digests of the existing
//! example parts unchanged — since a faster kernel that answers differently
//! is not faster, it is wrong.
//!
//! `ARTIFICER_SCALE=large` extends the sizes to the 10³- and 10⁴-face
//! bodies the targets name; the default set is what every `cargo test`
//! run pays for.

mod support;

use std::collections::BTreeMap;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{BooleanOperation, Tier, ValidationProfile, Vector3};
use support::scale::{self, Timing, TimingRow, timed};

fn large() -> bool {
    std::env::var("ARTIFICER_SCALE").is_ok_and(|value| value == "large")
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    let tolerance = 1.0e-9 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not {expected} (off by {:.3e})",
        actual - expected
    );
}

fn assert_valid(snapshot: &Snapshot, what: &str) {
    let report = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(
        report.valid,
        "{what} should validate: {:?}",
        report.diagnostics
    );
}

fn tier_of(session: &Session) -> Tier {
    session
        .step_reports
        .values()
        .map(artificer_protocol::OperationReport::tier)
        .fold(Tier::Exact, Tier::combine)
}

/// The stages every body is put through, timed: validation, display
/// tessellation, and the count check that the body is what the generator
/// said it would be.
fn common_stages(snapshot: &Snapshot, faces: u64, what: &str) -> Vec<(&'static str, Timing)> {
    assert_eq!(snapshot.counts().faces, faces, "{what}: face count");
    let (report, validate) = timed(|| NativeKernel::validate(snapshot, ValidationProfile::Solid));
    assert!(
        report.valid,
        "{what} should validate: {:?}",
        report.diagnostics
    );
    let mut stages = vec![("validate", validate)];
    // Display tessellation of a face with hundreds of holes is quadratic in
    // the hole count today (the hole-stitching in `triangulate_face_boundaries`
    // — a finding of this track, not yet fixed), so a many-holed body is only
    // timed up to a size that keeps the suite quick; the number is still
    // printed where it is taken.
    if faces <= 300 {
        let (scene, tessellate) = timed(|| NativeKernel::debug_scene(snapshot));
        assert!(!scene.triangles.is_empty(), "{what}: the scene is empty");
        stages.push(("tessellate", tessellate));
    }
    stages
}

// ---------------------------------------------------------------------------
// A plate with an n × n grid of drilled holes
// ---------------------------------------------------------------------------

#[test]
fn drilled_plates_are_exact_at_every_size_and_print_their_timings() {
    let sizes: &[usize] = if large() {
        &[4, 8, 16, 22, 32]
    } else {
        &[4, 8]
    };
    let mut rows = Vec::new();
    for &n in sizes {
        let what = format!("drilled plate {n}×{n}");
        let (session, build) = timed(|| scale::drilled_plate(n));
        let snapshot = &session.snapshot;
        assert_close(
            snapshot.measures().volume,
            scale::drilled_plate_volume(n),
            &what,
        );
        assert_eq!(tier_of(&session), Tier::Exact, "{what}: tier");
        let mut stages = vec![("build", build)];
        scale::print_stage_totals(&format!("{what} build"));
        stages.extend(common_stages(
            snapshot,
            scale::drilled_plate_faces(n),
            &what,
        ));
        scale::print_stage_totals(&format!("{what} validate and tessellate"));
        let (first, last) = scale::step_time_spread(&session);
        println!("{what}: fastest step {first} ms, slowest step {last} ms");
        rows.push(TimingRow {
            fixture: what,
            faces: snapshot.counts().faces,
            stages,
        });
    }
    scale::print_timing_table("drilled plates", &rows);
}

// ---------------------------------------------------------------------------
// Two drilled plates through the Boolean ladder
// ---------------------------------------------------------------------------

#[test]
fn two_drilled_plates_combine_exactly_and_conserve_volume() {
    // The default set includes n = 8 (134 faces per operand) so CI exercises
    // the face-extent index, which only engages above its face threshold.
    let sizes: &[usize] = if large() { &[4, 8, 16, 22] } else { &[4, 8] };
    let mut rows = Vec::new();
    for &n in sizes {
        let what = format!("two plates {n}×{n}");
        let first = scale::drilled_plate(n).snapshot;
        let second = scale::second_drilled_plate(n);
        assert_close(
            second.measures().volume,
            scale::second_drilled_plate_volume(n),
            &format!("{what}: second plate"),
        );
        let _ = artificer_kernel::perf::take_stage_totals();
        let (union, union_time) =
            timed(|| scale::boolean(&first, &second, BooleanOperation::Union));
        scale::print_stage_totals(&format!("{what} union"));
        let (intersection, intersection_time) =
            timed(|| scale::boolean(&first, &second, BooleanOperation::Intersection));
        let (difference, difference_time) =
            timed(|| scale::boolean(&first, &second, BooleanOperation::Difference));
        let _ = artificer_kernel::perf::take_stage_totals();
        assert_close(
            union.measures().volume,
            scale::two_plate_union_volume(n),
            &format!("{what}: union"),
        );
        assert_close(
            intersection.measures().volume,
            scale::two_plate_intersection_volume(n),
            &format!("{what}: intersection"),
        );
        assert_close(
            difference.measures().volume,
            scale::drilled_plate_volume(n) - scale::two_plate_intersection_volume(n),
            &format!("{what}: difference"),
        );
        // Conservation: V(A) + V(B) = V(A ∪ B) + V(A ∩ B).
        assert_close(
            union.measures().volume + intersection.measures().volume,
            first.measures().volume + second.measures().volume,
            &format!("{what}: conservation"),
        );
        for (name, body) in [
            ("union", &union),
            ("intersection", &intersection),
            ("difference", &difference),
        ] {
            assert_valid(body, &format!("{what}: {name}"));
            assert_eq!(body.counts().solids, 1, "{what}: {name} solids");
        }
        rows.push(TimingRow {
            fixture: what,
            faces: first.counts().faces + second.counts().faces,
            stages: vec![
                ("union", union_time),
                ("intersection", intersection_time),
                ("difference", difference_time),
            ],
        });
    }
    scale::print_timing_table("two drilled plates, analytic Boolean", &rows);
}

// ---------------------------------------------------------------------------
// A plate with an n × n array of bosses
// ---------------------------------------------------------------------------

#[test]
fn boss_arrays_are_exact_at_every_size_and_print_their_timings() {
    let sizes: &[usize] = if large() { &[4, 8, 16, 18] } else { &[3, 6] };
    let mut rows = Vec::new();
    for &n in sizes {
        let what = format!("boss array {n}×{n}");
        let (session, build) = timed(|| scale::boss_array(n));
        let snapshot = &session.snapshot;
        assert_close(
            snapshot.measures().volume,
            scale::boss_array_volume(n),
            &what,
        );
        assert_eq!(tier_of(&session), Tier::Exact, "{what}: tier");
        let mut stages = vec![("build", build)];
        stages.extend(common_stages(snapshot, scale::boss_array_faces(n), &what));
        rows.push(TimingRow {
            fixture: what,
            faces: snapshot.counts().faces,
            stages,
        });
    }
    scale::print_timing_table("boss arrays", &rows);
}

// ---------------------------------------------------------------------------
// A plate with n filleted hole rims
// ---------------------------------------------------------------------------

#[test]
fn filleted_hole_rims_are_exact_at_every_count_and_print_their_timings() {
    let counts: &[usize] = if large() { &[4, 16, 64, 144] } else { &[4, 9] };
    let mut rows = Vec::new();
    for &count in counts {
        let what = format!("{count} filleted rims");
        let (session, build) = timed(|| scale::filleted_holes(count));
        let snapshot = &session.snapshot;
        assert_close(
            snapshot.measures().volume,
            scale::filleted_holes_volume(count),
            &what,
        );
        assert_eq!(tier_of(&session), Tier::Exact, "{what}: tier");
        let n = scale::grid_for(count);
        // Each blend adds a torus band, in two halves like the rim it rounds.
        let faces = scale::drilled_plate_faces(n) + 2 * count as u64;
        let mut stages = vec![("build", build)];
        stages.extend(common_stages(snapshot, faces, &what));
        rows.push(TimingRow {
            fixture: what,
            faces: snapshot.counts().faces,
            stages,
        });
    }
    scale::print_timing_table("filleted hole rims", &rows);
}

// ---------------------------------------------------------------------------
// A prism on a polygon of thousands of curves, through the prism Boolean
// ---------------------------------------------------------------------------

#[test]
fn arc_disc_prisms_combine_exactly_and_print_their_timings() {
    let arc_counts: &[usize] = if large() { &[256, 1024] } else { &[64, 256] };
    let radius = 25.0;
    let height = 10.0;
    let mut rows = Vec::new();
    for &arcs in arc_counts {
        let what = format!("{arcs}-arc disc prism");
        let (first, build) = timed(|| scale::arc_disc_prism((0.0, 0.0), radius, arcs, height));
        assert_close(
            first.measures().volume,
            scale::arc_disc_prism_volume(radius, height),
            &what,
        );
        let mut stages = vec![("build", build)];
        // The side-face count is the kernel's to decide (arcs may merge on
        // one carrier); assert only that the body is valid and measures true.
        stages.extend(common_stages(&first, first.counts().faces, &what));
        // The same disc shifted off-centre: two profiles of `arcs` curves
        // each, whose boundaries cross in two places.
        let second = scale::arc_disc_prism((radius / 3.0, radius / 7.0), radius, arcs, height);
        let _ = artificer_kernel::perf::take_stage_totals();
        let (union, union_time) =
            timed(|| scale::boolean(&first, &second, BooleanOperation::Union));
        scale::print_stage_totals(&format!("{what} union"));
        let (intersection, intersection_time) =
            timed(|| scale::boolean(&first, &second, BooleanOperation::Intersection));
        assert_close(
            union.measures().volume + intersection.measures().volume,
            2.0 * first.measures().volume,
            &format!("{what}: conservation"),
        );
        assert_valid(&union, &format!("{what}: union"));
        assert_valid(&intersection, &format!("{what}: intersection"));
        stages.push(("union", union_time));
        stages.push(("intersection", intersection_time));
        rows.push(TimingRow {
            fixture: what,
            faces: first.counts().faces,
            stages,
        });
    }
    scale::print_timing_table("arc-disc prisms, prism Boolean", &rows);
}

// ---------------------------------------------------------------------------
// The example parts answer exactly as they did before this track
// ---------------------------------------------------------------------------

/// Every example script, with the semantic digest and tier its body had
/// before the scale work began. A digest that moves is a body that changed,
/// which no optimisation may do. (`blend_then_drill` moved once, when the
/// numerical intersection rung of ADR 0056 Track B took its drill through
/// the torus band over from the faceted tier; the table carries that body.)
const EXAMPLE_DIGESTS: &[(&str, &str, &str, Tier)] = &[
    (
        "bearing_mount",
        "9ce84a69aae7bbfd08b6208db5576f32606e06f8167d00993c3ab499c7b29fac",
        "9ce84a69aae7bbfd08b6208db5576f32",
        Tier::Exact,
    ),
    (
        "blend_then_drill",
        "c5d91023d64cc6f3666119ca956c3fc882247daa5d0d43c697926f4f21a5aa4c",
        "c5d91023d64cc6f3666119ca956c3fc8",
        Tier::Approximate,
    ),
    (
        "filleted_cube",
        "178163bc63e983296206c22346b52eb8b2aabfd46704a4e103973075e284aa1a",
        "178163bc63e983296206c22346b52eb8",
        Tier::Exact,
    ),
    (
        "filleted_flange",
        "f8e26c18c7d986036c51d9e7492033d8f2b501ecd4222fca6af1d5a8628fe83a",
        "f8e26c18c7d986036c51d9e7492033d8",
        Tier::Exact,
    ),
    (
        "flanged_hub",
        "9bf225a576f77c0370de9962ac67ad4862ba5cbc7f49a76acb306695df6e21fb",
        "9bf225a576f77c0370de9962ac67ad48",
        Tier::Exact,
    ),
    (
        "spline_vase",
        "a9ba59205667e4b9b6a50224d4dcabcc34401c099f22778a0ecce2c18025eb7b",
        "a9ba59205667e4b9b6a50224d4dcabcc",
        Tier::Approximate,
    ),
    (
        "square_to_circle_loft",
        "002b43c49b2813dfb292d71fc66f72543d745ec8ffa80fe981308147b4a359de",
        "002b43c49b2813dfb292d71fc66f7254",
        Tier::Approximate,
    ),
    (
        "standoff_plate",
        "1a6dcf8c43c48e367825b0e66262d2bddb996a271ab34100b70e9f1d23691a94",
        "1a6dcf8c43c48e367825b0e66262d2bd",
        Tier::Exact,
    ),
    (
        "three_holes_and_cut",
        "b46c1d3b20055c28c54cd1af1444470b232d97aa38a34845452f377a59ada34b",
        "b46c1d3b20055c28c54cd1af1444470b",
        Tier::Exact,
    ),
];

fn example_sessions() -> Vec<(&'static str, Session)> {
    let sources: [(&str, &str); 9] = [
        (
            "bearing_mount",
            include_str!("../examples/bearing_mount.art"),
        ),
        (
            "blend_then_drill",
            include_str!("../examples/blend_then_drill.art"),
        ),
        (
            "filleted_cube",
            include_str!("../examples/filleted_cube.art"),
        ),
        (
            "filleted_flange",
            include_str!("../examples/filleted_flange.art"),
        ),
        ("flanged_hub", include_str!("../examples/flanged_hub.art")),
        ("spline_vase", include_str!("../examples/spline_vase.art")),
        (
            "square_to_circle_loft",
            include_str!("../examples/square_to_circle_loft.art"),
        ),
        (
            "standoff_plate",
            include_str!("../examples/standoff_plate.art"),
        ),
        (
            "three_holes_and_cut",
            include_str!("../examples/three_holes_and_cut.art"),
        ),
    ];
    sources
        .into_iter()
        .map(|(name, source)| {
            let mut session = Session::new();
            let outcome =
                session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
            assert!(outcome.succeeded(), "{name}: {:?}", outcome.failure);
            (name, session)
        })
        .collect()
}

#[test]
fn example_parts_keep_their_digests() {
    let sessions = example_sessions();
    let mut lines = Vec::new();
    for (name, session) in &sessions {
        lines.push(format!(
            "    (\"{name}\", \"{}\", \"{}\", Tier::{:?}),",
            session.snapshot.semantic_digest(),
            session.snapshot.id(),
            tier_of(session)
        ));
    }
    println!("const EXAMPLE_DIGESTS: &[(&str, &str, &str, Tier)] = &[");
    for line in &lines {
        println!("{line}");
    }
    println!("];");
    if EXAMPLE_DIGESTS.is_empty() {
        return;
    }
    assert_eq!(EXAMPLE_DIGESTS.len(), sessions.len());
    for ((name, session), (expected_name, digest, id, tier)) in sessions.iter().zip(EXAMPLE_DIGESTS)
    {
        assert_eq!(name, expected_name);
        assert_eq!(
            &session.snapshot.semantic_digest().to_string(),
            digest,
            "{name}: digest"
        );
        assert_eq!(
            &session.snapshot.id().to_string(),
            id,
            "{name}: snapshot id"
        );
        assert_eq!(&tier_of(session), tier, "{name}: tier");
    }
}

// ---------------------------------------------------------------------------
// Scale invariance (ADR 0056 G1): the same construction at 10⁻³, 1 and 10³
// scale, translated by 10⁵, is the same body — same counts, same tier, and a
// volume that scales as the cube of the length. This is the gate the one
// agreement model exists to pass: a rung whose tolerance did not scale with
// the body would drop or fabricate a feature at an extreme scale and break it.
// ---------------------------------------------------------------------------

#[test]
fn drilled_plate_construction_is_scale_invariant() {
    let n = 5;
    let base = scale::drilled_plate(n).snapshot;
    let base_counts = base.counts();
    let base_volume = base.measures().volume;
    assert_close(base_volume, scale::drilled_plate_volume(n), "base volume");
    for scale_factor in [1.0e-3_f64, 1.0e3] {
        for translate in [0.0_f64, 1.0e5] {
            let what = format!("drilled plate scale {scale_factor:e} translate {translate:e}");
            let session = scale::drilled_plate_at(n, scale_factor, translate);
            let snapshot = &session.snapshot;
            // Topology and tier are exact under any similarity, so they must
            // match the base body bit for bit whatever the scale or offset.
            assert_eq!(snapshot.counts(), base_counts, "{what}: topology counts");
            assert_eq!(tier_of(&session), Tier::Exact, "{what}: tier");
            let expected = base_volume * scale_factor.powi(3);
            let relative = (snapshot.measures().volume - expected).abs() / expected.abs();
            assert!(
                relative <= volume_tolerance(scale_factor, translate),
                "{what}: volume {} is not {expected} to {:e} relative (off {relative:e})",
                snapshot.measures().volume,
                volume_tolerance(scale_factor, translate),
            );
            assert_valid(snapshot, &what);
        }
    }
}

/// How close the measured volume must be, relative. Pure scaling holds to
/// 1e-9; so does a translation of a body near unit size. The one corner this
/// loosens is a body scaled to 10⁻³ *and* moved 10⁵ away — a fifty-micron
/// plate a hundred kilometres from the origin — where the divergence-theorem
/// integral loses about a decimal digit to cancellation between the huge
/// coordinates and the tiny volume. That is a measure-reduction limit
/// (Track R5's deterministic reductions), not a construction one: the
/// topology and tier above are still exact, so the shape is right and only
/// its reported volume is a nudge coarse. Listed, not owned here.
fn volume_tolerance(scale_factor: f64, translate: f64) -> f64 {
    if scale_factor < 1.0 && translate > 0.0 {
        1.0e-7
    } else {
        1.0e-9
    }
}

#[test]
fn arc_disc_prism_construction_is_scale_invariant() {
    let arcs = 48;
    let (radius, height) = (25.0, 10.0);
    let base = scale::arc_disc_prism((0.0, 0.0), radius, arcs, height);
    let base_counts = base.counts();
    let base_volume = base.measures().volume;
    assert_close(
        base_volume,
        scale::arc_disc_prism_volume(radius, height),
        "base volume",
    );
    for scale_factor in [1.0e-3_f64, 1.0e3] {
        for translate in [0.0_f64, 1.0e5] {
            let what = format!("arc disc scale {scale_factor:e} translate {translate:e}");
            let body = scale::arc_disc_prism_at(radius, arcs, height, scale_factor, translate);
            assert_eq!(body.counts(), base_counts, "{what}: topology counts");
            let expected = base_volume * scale_factor.powi(3);
            let relative = (body.measures().volume - expected).abs() / expected.abs();
            assert!(
                relative <= volume_tolerance(scale_factor, translate),
                "{what}: volume {} is not {expected} to {:e} relative (off {relative:e})",
                body.measures().volume,
                volume_tolerance(scale_factor, translate),
            );
            assert_valid(&body, &what);
        }
    }
}

#[test]
fn analytic_boolean_is_scale_invariant() {
    // The two-plate analytic Boolean at unit scale and at 10³ scale, the
    // larger operands the exact scaling of the smaller ones (a similarity
    // transform is exact, so the two configurations differ only in size and
    // position). The sew weld, the section chaining and the face-extent index
    // all read the one agreement model, so the larger body must combine to
    // the same topology counts and conserve volume as the cube of the length.
    let n = 4;
    let first = scale::drilled_plate(n).snapshot;
    let second = scale::second_drilled_plate(n);
    let combine = |a: &Snapshot, b: &Snapshot| {
        let union = scale::boolean(a, b, BooleanOperation::Union);
        let intersection = scale::boolean(a, b, BooleanOperation::Intersection);
        assert_valid(&union, "union");
        assert_valid(&intersection, "intersection");
        assert_close(
            union.measures().volume + intersection.measures().volume,
            a.measures().volume + b.measures().volume,
            "conservation",
        );
        (union.counts(), union.measures().volume)
    };
    let (unit_counts, unit_volume) = combine(&first, &second);

    let scale_factor = 1.0e3_f64;
    let big_first = scale::transformed(&first, Vector3::new(0.0, 0.0, 0.0), scale_factor);
    let big_second = scale::transformed(&second, Vector3::new(0.0, 0.0, 0.0), scale_factor);
    let (big_counts, big_volume) = combine(&big_first, &big_second);

    assert_eq!(unit_counts, big_counts, "union counts across scales");
    let expected = unit_volume * scale_factor.powi(3);
    let relative = (big_volume - expected).abs() / expected.abs();
    assert!(
        relative <= 1.0e-9,
        "scaled union volume {big_volume} is not {expected} to 1e-9 relative (off {relative:e})"
    );
}

#[test]
fn a_moved_plate_keeps_its_measures() {
    // The transform that makes a second Boolean operand must not move the
    // body's own measures; a translation is exact.
    let plate = scale::drilled_plate(3).snapshot;
    let moved = scale::transformed(&plate, Vector3::new(1.0e3, -2.0e3, 5.0e2), 1.0);
    assert_close(
        moved.measures().volume,
        plate.measures().volume,
        "moved plate volume",
    );
    assert_valid(&moved, "moved plate");
}

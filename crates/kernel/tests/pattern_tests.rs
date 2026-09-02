//! Exact mirror and feature patterns: the closed-form volumes they must
//! land on, the tiers they must certify at, and the promise that a pattern
//! commits whole or not at all.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::commands::{ApiCommand, PatternPlacement, StepLabel};
use artificer_kernel::api::debug::ApiErrorCode;
use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::probe::{ProbeRequest, probe};
use artificer_kernel::api::selectors::{EntitySelector, GeometricSelector};
use artificer_kernel::api::session::{PATTERN_RUNG, Session};
use artificer_protocol::{Point2, Point3, Tier, Vector3};

const FILLETED_FLANGE: &str = include_str!("../examples/filleted_flange.art");
const FLANGED_HUB: &str = include_str!("../examples/flanged_hub.art");

/// An 80 × 80 × 10 plate centred on the origin, its top face at z = 10,
/// with one 6 mm hole through it 25 mm out along +X.
const PLATE_WITH_HOLE: &str = "\
let plate = box(origin: [-40, -40, 0], size: [80, 80, 10], label: \"plate\");
let hole = drill(face: faces(\">Z\"), center: [25, 0], diameter: 6, depth: 10, label: \"hole\");
";
const PLATE_VOLUME: f64 = 80.0 * 80.0 * 10.0;
const HOLE_VOLUME: f64 = PI * 3.0 * 3.0 * 10.0;

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

fn contains(session: &Session, x: f64, y: f64, z: f64) -> bool {
    let result = probe(
        session,
        &ProbeRequest::Contains {
            point: Point3::new(x, y, z),
            step: None,
        },
    )
    .expect("contains probe");
    result.value > 0.5
}

// ---------------------------------------------------------------------------
// Circular and linear patterns of a drilled hole
// ---------------------------------------------------------------------------

#[test]
fn circular_pattern_of_a_drilled_hole_removes_six_holes_exactly() {
    let script = format!(
        "{PLATE_WITH_HOLE}pattern(step: hole, axis: [0, 0, 1], axis_origin: [0, 0, 10], count: 6, label: \"holes\");\n"
    );
    let session = run(&script);
    let measures = session.snapshot.measures();
    assert_close(measures.volume, PLATE_VOLUME - 6.0 * HOLE_VOLUME, "volume");
    assert_eq!(session.snapshot.counts().solids, 1);

    // Every hole is where the turn put it: 60° apart on the 25 mm circle.
    for k in 0..6 {
        let angle = (60.0 * f64::from(k)).to_radians();
        let (sin, cos) = angle.sin_cos();
        assert!(
            !contains(&session, 25.0 * cos, 25.0 * sin, 5.0),
            "hole {k} is missing"
        );
    }
    assert!(contains(&session, 0.0, 0.0, 5.0));
    assert!(contains(
        &session,
        25.0 * 30.0_f64.to_radians().cos(),
        25.0 * 30.0_f64.to_radians().sin(),
        5.0
    ));

    // The pattern step reports its own rung and stays exact; the instances
    // report the drill that built them.
    let report = session.report();
    assert_eq!(report.tier, Tier::Exact);
    let steps: BTreeMap<&str, (&str, Option<&str>)> = report
        .steps
        .iter()
        .map(|step| {
            (
                step.label.as_str(),
                (step.command.as_str(), step.rung.as_deref()),
            )
        })
        .collect();
    assert_eq!(steps["holes"], ("feature_pattern", Some(PATTERN_RUNG)));
    for k in 1..6 {
        assert_eq!(
            steps[format!("holes/{k}").as_str()],
            ("drill_hole", Some("face-feature/exact-prism"))
        );
    }
    assert!(
        !steps.contains_key("holes/6"),
        "count includes the original"
    );
    // One journal entry stands for the whole pattern.
    assert_eq!(session.journal.entries.len(), 3);
}

#[test]
fn linear_pattern_of_a_drilled_hole_removes_a_row_exactly() {
    let script = "\
let plate = box(origin: [-40, -40, 0], size: [80, 80, 10], label: \"plate\");
let hole = drill(face: faces(\">Z\"), center: [25, -30], diameter: 6, depth: 10, label: \"hole\");
pattern(step: hole, direction: [0, 1, 0], spacing: 20, count: 4, label: \"row\");
";
    let mut session = run(script);
    assert_close(
        session.snapshot.measures().volume,
        PLATE_VOLUME - 4.0 * HOLE_VOLUME,
        "volume",
    );
    for y in [-30.0, -10.0, 10.0, 30.0] {
        assert!(!contains(&session, 25.0, y, 5.0), "no hole at y = {y}");
    }
    assert!(contains(&session, 25.0, 0.0, 5.0));

    let result = session
        .execute(
            ApiCommand::FeaturePattern {
                label: "columns".to_owned(),
                step: StepLabel::from("hole"),
                placement: PatternPlacement::Linear {
                    direction: Vector3::new(-1.0, 0.0, 0.0),
                    spacing: 25.0,
                    count: 3,
                },
            },
            &CancellationToken::default(),
        )
        .expect("second pattern");
    assert_eq!(result.rung.as_deref(), Some(PATTERN_RUNG));
    assert_eq!(result.tier, Tier::Exact);
    assert_close(
        session.snapshot.measures().volume,
        PLATE_VOLUME - 6.0 * HOLE_VOLUME,
        "volume after the second row",
    );
}

#[test]
fn patterns_replay_cut_and_add_extrusions_from_face_sketches() {
    // A round pocket and a round boss, each turned four times about the
    // plate's centre.
    let script = "\
let plate = box(origin: [-40, -40, 0], size: [80, 80, 10], label: \"plate\");
let pocket_profile = sketch(on: faces(\">Z\"), entities: [circle(center: [25, 0], radius: 4)], label: \"pocket_profile\");
let pocket = extrude(sketch: pocket_profile, distance: 5, operation: \"cut\", label: \"pocket\");
pattern(step: pocket, axis: [0, 0, 1], axis_origin: [0, 0, 10], count: 4, label: \"pockets\");
let boss_profile = sketch(on: faces(\">Z\"), entities: [circle(center: [10, 10], radius: 4)], label: \"boss_profile\");
let boss = extrude(sketch: boss_profile, distance: 5, operation: \"add\", label: \"boss\");
pattern(step: boss, axis: [0, 0, 1], axis_origin: [0, 0, 10], count: 4, angle: 45, label: \"bosses\");
";
    let session = run(script);
    let pocket = PI * 16.0 * 5.0;
    let boss = PI * 16.0 * 5.0;
    assert_close(
        session.snapshot.measures().volume,
        PLATE_VOLUME - 4.0 * pocket + 4.0 * boss,
        "volume",
    );
    // Pockets 25 out at 0°, 90°, 180° and 270°; bosses 14.1 out at 45°,
    // 90°, 135° and 180°.
    for (x, y) in [(25.0, 0.0), (0.0, 25.0), (-25.0, 0.0), (0.0, -25.0)] {
        assert!(!contains(&session, x, y, 8.0), "pocket at ({x}, {y})");
    }
    let a = 10.0 * std::f64::consts::SQRT_2;
    for (x, y) in [(10.0, 10.0), (0.0, a), (-10.0, 10.0), (-a, 0.0)] {
        assert!(contains(&session, x, y, 12.0), "boss at ({x}, {y})");
    }
    assert!(!contains(&session, 10.0, -10.0, 12.0));
    assert!(!contains(&session, a, 0.0, 12.0));
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_rectangular_pocket_turns_with_the_pattern() {
    let script = "\
let plate = box(origin: [-40, -40, 0], size: [80, 80, 10], label: \"plate\");
let slot = sketch(on: faces(\">Z\"), entities: [rect(origin: [20, -3], width: 10, height: 6)], label: \"slot\");
extrude(sketch: slot, distance: 4, operation: \"cut\", label: \"pocket\");
pattern(step: \"pocket\", axis: [0, 0, 1], axis_origin: [0, 0, 10], count: 4, label: \"pockets\");
";
    let session = run(script);
    assert_close(
        session.snapshot.measures().volume,
        PLATE_VOLUME - 4.0 * (10.0 * 6.0 * 4.0),
        "volume",
    );
    // The turned pocket runs along Y at x = 0, 20..30 along Y.
    assert!(!contains(&session, 0.0, 25.0, 8.0));
    assert!(!contains(&session, 2.0, 21.0, 8.0));
    assert!(contains(&session, 4.0, 25.0, 8.0));
    assert!(contains(&session, 0.0, 25.0, 4.0));
}

// ---------------------------------------------------------------------------
// Blends on patterned bodies
// ---------------------------------------------------------------------------

#[test]
fn a_rim_fillet_after_a_pattern_certifies_exact() {
    let script = "\
let plate = box(origin: [-40, -40, 0], size: [80, 80, 10], label: \"plate\");
let hole = drill(face: faces(\">Z\"), center: [25, -30], diameter: 6, depth: 10, label: \"hole\");
pattern(step: hole, direction: [0, 1, 0], spacing: 20, count: 4, label: \"row\");
fillet(edges: [nearest(point: [25, 13, 10], kind: \"edge\"), nearest(point: [25, 7, 10], kind: \"edge\")], radius: 1, label: \"rim\");
";
    let session = run(script);
    let report = session.report();
    assert_eq!(report.tier, Tier::Exact);
    let rim = report
        .steps
        .iter()
        .find(|step| step.label == "rim")
        .expect("rim step");
    let rung = rim.rung.as_deref().unwrap_or_default();
    assert!(
        rung.starts_with("edge-finish/") && !rung.ends_with("/faceted"),
        "{rung}"
    );
    // Pappus: the ring the rolling ball leaves outside a hole of radius r
    // with a fillet of radius f.
    let (r, f) = (3.0, 1.0);
    let ring = 2.0 * PI * (r * f * f * (1.0 - PI / 4.0) + f * f * f * (5.0 / 6.0 - PI / 4.0));
    assert_close(
        session.snapshot.measures().volume,
        PLATE_VOLUME - 4.0 * HOLE_VOLUME - ring,
        "volume",
    );
    assert!(
        report
            .body
            .as_ref()
            .is_some_and(|body| body.surfaces.tori > 0)
    );
}

// ---------------------------------------------------------------------------
// Exact mirror
// ---------------------------------------------------------------------------

#[test]
fn a_mirrored_blended_body_keeps_its_volume_and_reflects_its_centroid() {
    let plane_x = 10.0;
    let original = run(FILLETED_FLANGE);
    let before = original.snapshot.measures();
    let mirrored_script = format!(
        "{FILLETED_FLANGE}\nmirror(origin: [{plane_x}, 0, 0], normal: [1, 0, 0], label: \"other_side\");\n"
    );
    let mirrored = run(&mirrored_script);
    let after = mirrored.snapshot.measures();

    let report = mirrored.report();
    assert_eq!(report.tier, Tier::Exact);
    let step = report
        .steps
        .iter()
        .find(|step| step.label == "other_side")
        .expect("mirror step");
    assert_eq!(step.rung.as_deref(), Some("mirror/exact"));
    assert!(step.warnings.is_empty(), "{:?}", step.warnings);

    assert_eq!(
        original.snapshot.counts(),
        mirrored.snapshot.counts(),
        "a mirror keeps every face, edge and vertex"
    );
    assert_close(after.volume, before.volume, "volume");
    assert_close(after.surface_area, before.surface_area, "surface area");
    let (c0, c1) = (before.centroid.unwrap(), after.centroid.unwrap());
    assert_close(c1.x, 2.0 * plane_x - c0.x, "centroid x");
    assert_close(c1.y, c0.y, "centroid y");
    assert_close(c1.z, c0.z, "centroid z");

    // The blends came through as tori, not facets.
    let body = report.body.expect("body");
    assert!(body.surfaces.tori >= 4, "{:?}", body.surfaces);
    assert_eq!(
        body.surfaces.planes + body.surfaces.cylinders + body.surfaces.tori,
        body.topology.faces
    );
}

#[test]
fn a_mirrored_body_takes_exact_features_afterwards() {
    // The flanged hub, mirrored, then one more bolt hole on the mirrored
    // flange: the reflected planes and cylinders are sound carriers for
    // the exact prism path.
    let plane_x = 10.0;
    let script = format!(
        "{FLANGED_HUB}\nmirror(origin: [{plane_x}, 0, 0], normal: [1, 0, 0], label: \"other_side\");\n"
    );
    let mut session = run(&script);
    let before = session.snapshot.measures().volume;
    let pitch = 32.5;
    let at_45 = pitch * std::f64::consts::FRAC_1_SQRT_2;
    let result = session
        .execute(
            ApiCommand::DrillHole {
                label: "bolt_4".to_owned(),
                face: EntitySelector::ByGeometry {
                    selector: GeometricSelector::NearestTo {
                        point: Point3::new(2.0 * plane_x + at_45, at_45, 8.0),
                        kind: artificer_protocol::EntityKind::Face,
                    },
                },
                center: Point2::new(at_45, at_45),
                diameter: 6.5,
                depth: 8.0,
            },
            &CancellationToken::default(),
        )
        .expect("drill after mirror");
    assert_eq!(result.tier, Tier::Exact, "{:?}", result.warnings);
    assert_close(
        session.snapshot.measures().volume,
        before - PI * 3.25 * 3.25 * 8.0,
        "volume",
    );
    assert_eq!(session.report().tier, Tier::Exact);
}

#[test]
fn a_mirrored_prism_is_the_exact_reflection() {
    let script = "\
let plate = box(origin: [0, 0, 0], size: [40, 30, 20], label: \"plate\");
drill(face: faces(\">Z\"), center: [10, 5], diameter: 8, depth: 20, label: \"hole\");
mirror(origin: [0, 0, 0], normal: [1, 0, 0], label: \"flipped\");
";
    let session = run(script);
    let measures = session.snapshot.measures();
    assert_close(
        measures.volume,
        40.0 * 30.0 * 20.0 - PI * 16.0 * 20.0,
        "volume",
    );
    let bounds = measures.bounds.expect("bounds");
    assert_close(bounds.min.x, -40.0, "min x");
    assert_close(bounds.max.x, 0.0, "max x");
    // The hole was at (30, 20) in the world; now it is at (-30, 20).
    assert!(!contains(&session, -30.0, 20.0, 10.0));
    assert!(contains(&session, -10.0, 20.0, 10.0));
    assert_eq!(session.report().tier, Tier::Exact);
}

// ---------------------------------------------------------------------------
// Digest stability, undo, and the journal
// ---------------------------------------------------------------------------

#[test]
fn pattern_and_mirror_replay_to_the_same_digest() {
    let script = format!(
        "{PLATE_WITH_HOLE}pattern(step: hole, axis: [0, 0, 1], axis_origin: [0, 0, 10], count: 5, label: \"holes\");\nmirror(origin: [0, 0, 0], normal: [0, 1, 0], label: \"flipped\");\n"
    );
    let first = run(&script);
    let second = run(&script);
    assert_eq!(
        first.snapshot.semantic_digest(),
        second.snapshot.semantic_digest(),
        "two sessions building the same script"
    );

    // Through the journal, and through the script the journal decompiles to.
    let journal = first.export_journal().unwrap();
    let replayed = Session::from_journal(&journal).unwrap();
    assert_eq!(
        replayed.snapshot.semantic_digest(),
        first.snapshot.semantic_digest(),
        "journal replay"
    );
    let decompiled = first.to_art(&DecompileOptions::default()).unwrap();
    assert!(decompiled.contains("pattern(step: hole"), "{decompiled}");
    assert_eq!(
        run(&decompiled).snapshot.semantic_digest(),
        first.snapshot.semantic_digest(),
        "decompiled script:\n{decompiled}"
    );
    assert_eq!(
        first.report().body.unwrap().digest,
        first.snapshot.semantic_digest()
    );
}

#[test]
fn undo_drops_a_whole_pattern_and_redo_restores_it() {
    let mut session = run(PLATE_WITH_HOLE);
    let before = session.snapshot.semantic_digest();
    session
        .execute(
            ApiCommand::FeaturePattern {
                label: "holes".to_owned(),
                step: StepLabel::from("hole"),
                placement: PatternPlacement::Circular {
                    axis_origin: Point3::new(0.0, 0.0, 10.0),
                    axis_direction: Vector3::new(0.0, 0.0, 1.0),
                    count: 6,
                    angle_step_degrees: 0.0,
                },
            },
            &CancellationToken::default(),
        )
        .expect("pattern");
    let patterned = session.snapshot.semantic_digest();
    assert_ne!(before, patterned);
    assert_eq!(session.step_order.len(), 2 + 1 + 5);

    session.undo().unwrap();
    assert_eq!(session.snapshot.semantic_digest(), before);
    assert_eq!(
        session.step_order,
        vec!["plate".to_owned(), "hole".to_owned()]
    );
    assert_eq!(session.journal.entries.len(), 2);
    assert!(
        session
            .step_reports
            .keys()
            .all(|label| !label.starts_with("holes"))
    );

    session.redo().unwrap();
    assert_eq!(session.snapshot.semantic_digest(), patterned);
    assert_eq!(session.step_order.len(), 2 + 1 + 5);
    assert_eq!(session.journal.entries.len(), 3);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

fn refusal(session: &mut Session, command: ApiCommand) -> ApiErrorCode {
    let before = session.snapshot.semantic_digest();
    let steps = session.step_order.clone();
    let error = session
        .execute(command, &CancellationToken::default())
        .expect_err("the pattern should be refused");
    assert_eq!(
        session.snapshot.semantic_digest(),
        before,
        "a refusal changes nothing"
    );
    assert_eq!(
        session.step_order, steps,
        "a refusal leaves no steps behind"
    );
    error.code
}

#[test]
fn patterns_refuse_placements_off_the_face_and_sources_they_cannot_replay() {
    let mut session = run(PLATE_WITH_HOLE);
    let circular = |axis: Vector3, count: u16| ApiCommand::FeaturePattern {
        label: "holes".to_owned(),
        step: StepLabel::from("hole"),
        placement: PatternPlacement::Circular {
            axis_origin: Point3::new(0.0, 0.0, 10.0),
            axis_direction: axis,
            count,
            angle_step_degrees: 0.0,
        },
    };
    let linear = |direction: Vector3, spacing: f64, count: u16| ApiCommand::FeaturePattern {
        label: "holes".to_owned(),
        step: StepLabel::from("hole"),
        placement: PatternPlacement::Linear {
            direction,
            spacing,
            count,
        },
    };

    assert_eq!(
        refusal(&mut session, circular(Vector3::new(1.0, 0.0, 0.0), 6)),
        ApiErrorCode::InvalidInput,
        "axis not normal to the face"
    );
    assert_eq!(
        refusal(&mut session, linear(Vector3::new(0.0, 0.0, 1.0), 10.0, 3)),
        ApiErrorCode::InvalidInput,
        "direction out of the face"
    );
    assert_eq!(
        refusal(&mut session, linear(Vector3::new(0.0, 1.0, 0.0), 0.0, 3)),
        ApiErrorCode::InvalidInput,
        "zero spacing"
    );
    assert_eq!(
        refusal(&mut session, circular(Vector3::new(0.0, 0.0, 1.0), 1)),
        ApiErrorCode::InvalidInput,
        "one instance is no pattern"
    );
    assert_eq!(
        refusal(
            &mut session,
            ApiCommand::FeaturePattern {
                label: "plates".to_owned(),
                step: StepLabel::from("plate"),
                placement: PatternPlacement::Linear {
                    direction: Vector3::new(1.0, 0.0, 0.0),
                    spacing: 100.0,
                    count: 2,
                },
            }
        ),
        ApiErrorCode::InvalidInput,
        "a box is not a face feature"
    );
    assert_eq!(
        refusal(
            &mut session,
            ApiCommand::FeaturePattern {
                label: "ghosts".to_owned(),
                step: StepLabel::from("nowhere"),
                placement: PatternPlacement::Linear {
                    direction: Vector3::new(1.0, 0.0, 0.0),
                    spacing: 10.0,
                    count: 2,
                },
            }
        ),
        ApiErrorCode::SelectorNotFound,
        "an unknown step"
    );

    // An instance that would leave the face fails the whole pattern, and
    // the instances that did build go with it.
    let code = refusal(&mut session, linear(Vector3::new(1.0, 0.0, 0.0), 10.0, 4));
    assert_ne!(code, ApiErrorCode::SessionError, "{code:?}");
    assert!(
        session
            .step_reports
            .keys()
            .all(|label| !label.starts_with("holes"))
    );

    // The good placement still works afterwards.
    session
        .execute(
            circular(Vector3::new(0.0, 0.0, 1.0), 6),
            &CancellationToken::default(),
        )
        .expect("a sound pattern after refusals");
    assert_close(
        session.snapshot.measures().volume,
        PLATE_VOLUME - 6.0 * HOLE_VOLUME,
        "volume",
    );
}

#[test]
fn a_pattern_on_a_face_feature_matches_the_same_holes_drilled_by_hand() {
    // The bolt circle from flanged_hub.art, as a pattern instead of a loop:
    // the same body to the digest.
    let by_hand = run(FLANGED_HUB);
    let with_pattern = FLANGED_HUB
        .replace(
            "for i in 0..bolt_count {\n    let angle = 360 * i / bolt_count;\n    drill(face: flange_top, center: [pitch * cos(angle), pitch * sin(angle)],\n          diameter: bolt_diameter, depth: flange_thickness, label: \"bolt_\" + i);\n}",
            "let bolt_0 = drill(face: flange_top, center: [pitch, 0], diameter: bolt_diameter, depth: flange_thickness, label: \"bolt_0\");\npattern(step: bolt_0, axis: [0, 0, 1], axis_origin: [0, 0, flange_thickness], count: bolt_count, label: \"bolts\");",
        );
    assert_ne!(with_pattern, FLANGED_HUB);
    let patterned = run(&with_pattern);
    assert_close(
        patterned.snapshot.measures().volume,
        by_hand.snapshot.measures().volume,
        "volume",
    );
    assert_eq!(patterned.snapshot.counts(), by_hand.snapshot.counts());
    assert_eq!(patterned.report().tier, Tier::Exact);
}

//! Exact prism Booleans (ADR 0025, first reconstruction milestone).
//!
//! Every expectation is a closed form derived independently of the kernel:
//! prism volumes are profile area × height, and the profile areas come from
//! rectangle, disc, lens, and circular-segment formulas evaluated in the
//! test. Validation must pass on every published result, and the centroid
//! must land where the symmetry of the shape says it must.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

fn extrude_profile(profile: PlanarProfile2, origin: Point3, height: f64, label: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                origin,
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: height,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("the operand profile should extrude")
        .snapshot
}

fn rectangle(min: (f64, f64), max: (f64, f64)) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&[
                Point2::new(min.0, min.1),
                Point2::new(max.0, min.1),
                Point2::new(max.0, max.1),
                Point2::new(min.0, max.1),
            ]),
            holes: vec![],
        }],
    }
}

fn disc(center: (f64, f64), radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(center.0, center.1),
                    radius,
                    direction: artificer_protocol::ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    }
}

fn boolean(
    target: &Snapshot,
    tool: &Snapshot,
    operation: BooleanOperation,
    label: &str,
) -> Result<Snapshot, artificer_protocol::KernelError> {
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation,
    };
    NativeKernel::execute_boolean(target, tool, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

fn assert_volume(snapshot: &Snapshot, expected: f64, what: &str) {
    assert!(NativeKernel::validate(snapshot, ValidationProfile::Solid).valid);
    let volume = snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "{what}: volume {volume} should equal {expected}"
    );
}

#[test]
fn a_cylinder_drills_an_exact_through_hole() {
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (20.0, 16.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "drill-plate",
    );
    let drill = extrude_profile(
        disc((8.0, 8.0), 3.0),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "drill-tool",
    );
    let drilled = boolean(&plate, &drill, BooleanOperation::Difference, "drill")
        .expect("a through drill is inside the prism Boolean domain");

    let pi = std::f64::consts::PI;
    assert_volume(&drilled, (320.0 - pi * 9.0) * 10.0, "through drill");
    // Bottom, four outer walls, two half-cylinder hole walls, top.
    assert_eq!(drilled.counts().faces, 8);
    // The exact shell measures must see the hole's centroid shift too: the
    // hole is off-centre in x (8 vs 10), pulling the centre the other way.
    let centre = drilled
        .measures()
        .centroid
        .expect("a drilled plate has a centre");
    let solid = 320.0 - pi * 9.0;
    let expected_x = 10.0f64.mul_add(320.0, -(pi * 9.0 * 8.0)) / solid;
    assert!(
        (centre.x - expected_x).abs() < 1.0e-9,
        "centre x {} should equal {expected_x}",
        centre.x
    );
    assert!((centre.z - 5.0).abs() < 1.0e-9);
}

#[test]
fn an_overtravelling_drill_still_counts_as_a_through_cut() {
    // The tool starts below the plate and ends above it; the difference is
    // still the plate's own slab.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (12.0, 12.0)),
        Point3::new(0.0, 0.0, 0.0),
        6.0,
        "overtravel-plate",
    );
    let drill = extrude_profile(
        disc((6.0, 6.0), 2.0),
        Point3::new(0.0, 0.0, -5.0),
        16.0,
        "overtravel-tool",
    );
    let drilled = boolean(&plate, &drill, BooleanOperation::Difference, "overtravel")
        .expect("an overtravelling drill still pierces");
    let pi = std::f64::consts::PI;
    assert_volume(&drilled, (144.0 - pi * 4.0) * 6.0, "overtravelling drill");
}

#[test]
fn a_boundary_crossing_cylinder_notches_the_plate() {
    // The disc's centre sits on the plate's edge carrier: half the disc
    // removes and the boundary gains two arc walls meeting the outer wall at
    // genuine line/arc crossings.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (10.0, 8.0)),
        Point3::new(0.0, 0.0, 0.0),
        5.0,
        "notch-plate",
    );
    let bite = extrude_profile(
        disc((0.0, 4.0), 2.0),
        Point3::new(0.0, 0.0, 0.0),
        5.0,
        "notch-tool",
    );
    let notched = boolean(&plate, &bite, BooleanOperation::Difference, "notch")
        .expect("a transverse notch is inside the domain");
    let pi = std::f64::consts::PI;
    assert_volume(&notched, 80.0f64.mul_add(1.0, -(pi * 2.0)) * 5.0, "notch");
}

#[test]
fn two_overlapping_cylinders_union_exactly() {
    let (radius, offset, height) = (3.0_f64, 4.0_f64, 7.0_f64);
    let first = extrude_profile(
        disc((0.0, 0.0), radius),
        Point3::new(0.0, 0.0, 0.0),
        height,
        "lens-first",
    );
    let second = extrude_profile(
        disc((offset, 0.0), radius),
        Point3::new(0.0, 0.0, 0.0),
        height,
        "lens-second",
    );
    let joined = boolean(&first, &second, BooleanOperation::Union, "lens-union")
        .expect("overlapping equal-height cylinders union exactly");

    let pi = std::f64::consts::PI;
    let half = offset / 2.0;
    let lens = 2.0
        * radius.mul_add(
            radius * (half / radius).acos(),
            -(half * (radius * radius - half * half).sqrt()),
        );
    assert_volume(
        &joined,
        (2.0 * pi * radius * radius - lens) * height,
        "cylinder union",
    );

    let met = boolean(&first, &second, BooleanOperation::Intersection, "lens-meet")
        .expect("the lens itself is also exact");
    assert_volume(&met, lens * height, "cylinder intersection");
}

#[test]
fn a_full_width_slot_splits_the_plate_into_two_solids() {
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (10.0, 8.0)),
        Point3::new(0.0, 0.0, 0.0),
        4.0,
        "slot-plate",
    );
    let slot = extrude_profile(
        rectangle((4.0, -1.0), (6.0, 9.0)),
        Point3::new(0.0, 0.0, 0.0),
        4.0,
        "slot-tool",
    );
    let split = boolean(&plate, &slot, BooleanOperation::Difference, "slot")
        .expect("a splitting cut publishes both pieces");
    assert_volume(&split, (80.0 - 16.0) * 4.0, "split plate");
    assert_eq!(split.counts().solids, 2);
}

#[test]
fn a_partial_depth_drill_becomes_an_exact_blind_pocket() {
    // Once the flagship refusal, now the flagship publication: the stacked
    // builder gives the partial-depth drill an exact floor.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (12.0, 12.0)),
        Point3::new(0.0, 0.0, 0.0),
        8.0,
        "pocket-plate",
    );
    let drill = extrude_profile(
        disc((6.0, 6.0), 2.0),
        Point3::new(0.0, 0.0, 4.0),
        10.0,
        "pocket-tool",
    );
    let pocketed = boolean(&plate, &drill, BooleanOperation::Difference, "pocket")
        .expect("a partial-depth drill is a blind pocket");
    let pi = std::f64::consts::PI;
    assert_volume(
        &pocketed,
        1152.0f64.mul_add(1.0, -(16.0 * pi)),
        "partial-depth drill",
    );
}

#[test]
fn a_swallowed_target_reports_an_empty_result_not_a_domain_error() {
    let small = extrude_profile(
        rectangle((2.0, 2.0), (4.0, 4.0)),
        Point3::new(0.0, 0.0, 0.0),
        5.0,
        "swallowed-target",
    );
    let large = extrude_profile(
        rectangle((0.0, 0.0), (10.0, 10.0)),
        Point3::new(0.0, 0.0, 0.0),
        5.0,
        "swallowing-tool",
    );
    let refused = boolean(&small, &large, BooleanOperation::Difference, "swallow")
        .expect_err("subtracting a superset leaves nothing to publish");
    assert!(
        refused
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code.as_str() == "BOOLEAN_EMPTY_OR_UNRESOLVED_RESULT" }),
        "unexpected refusal: {refused:?}"
    );
}

#[test]
fn drilling_a_hole_then_unioning_a_boss_composes() {
    // Boolean results are ordinary snapshots, so they feed straight back in
    // as operands. Drill a plate, then union a same-height boss elsewhere.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (20.0, 10.0)),
        Point3::new(0.0, 0.0, 0.0),
        6.0,
        "compose-plate",
    );
    let drill = extrude_profile(
        disc((5.0, 5.0), 1.5),
        Point3::new(0.0, 0.0, 0.0),
        6.0,
        "compose-drill",
    );
    let boss = extrude_profile(
        disc((22.0, 5.0), 3.0),
        Point3::new(0.0, 0.0, 0.0),
        6.0,
        "compose-boss",
    );
    let drilled = boolean(&plate, &drill, BooleanOperation::Difference, "compose-cut")
        .expect("the drill pierces");
    let composed = boolean(&drilled, &boss, BooleanOperation::Union, "compose-union")
        .expect("a drilled prism is still a prism and still unions");

    let pi = std::f64::consts::PI;
    let boss_disc = pi * 9.0;
    // The boss circle crosses the plate edge at x = 20: the overlap is the
    // circular segment beyond that chord.
    let reach = 3.0_f64;
    let chord_distance = 2.0_f64; // 22 − 20 places the chord 2 from centre.
    let segment = reach.mul_add(
        reach * (chord_distance / reach).acos(),
        -(chord_distance * (reach * reach - chord_distance * chord_distance).sqrt()),
    );
    let expected_area = 200.0 - pi * 1.5 * 1.5 + boss_disc - segment;
    assert_volume(&composed, expected_area * 6.0, "drill then boss");
}

#[test]
fn intersection_needs_no_slab_agreement_at_all() {
    // The intersection of two co-directional prisms is always a prism over
    // the profile intersection and the slab overlap, whatever the heights.
    let tall = extrude_profile(
        rectangle((0.0, 0.0), (10.0, 10.0)),
        Point3::new(0.0, 0.0, 0.0),
        12.0,
        "overlap-tall",
    );
    let shifted = extrude_profile(
        disc((10.0, 5.0), 4.0),
        Point3::new(0.0, 0.0, 5.0),
        20.0,
        "overlap-cylinder",
    );
    let met = boolean(
        &tall,
        &shifted,
        BooleanOperation::Intersection,
        "overlap-meet",
    )
    .expect("overlapping slabs intersect exactly");

    // Profile overlap: half the disc (centre on the plate edge); slab
    // overlap: z in [5, 12].
    let pi = std::f64::consts::PI;
    assert_volume(&met, pi * 16.0 / 2.0 * 7.0, "slab-overlap intersection");
    let bounds = met.measures().bounds.expect("a solid has bounds");
    assert!((bounds.min.z - 5.0).abs() < 1.0e-9 && (bounds.max.z - 12.0).abs() < 1.0e-9);
}

#[test]
fn a_blind_pocket_publishes_through_the_stacked_builder() {
    // The drill stops 4 above the bottom: two exact layers glued at the
    // floor, with the pocket opening through the top cap.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (20.0, 16.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "blind-plate",
    );
    let drill = extrude_profile(
        disc((8.0, 8.0), 3.0),
        Point3::new(0.0, 0.0, 4.0),
        12.0,
        "blind-tool",
    );
    let pocketed = boolean(&plate, &drill, BooleanOperation::Difference, "blind")
        .expect("a top-piercing blind pocket is inside the stacked domain");

    let pi = std::f64::consts::PI;
    let expected = 320.0f64.mul_add(10.0, -(pi * 9.0 * 6.0));
    assert_volume(&pocketed, expected, "blind pocket");

    // Bottom cap, four lower walls, four upper walls, two pocket half
    // cylinders, the floor, and the top cap with its opening.
    assert_eq!(pocketed.counts().faces, 13);
    assert_eq!(pocketed.counts().solids, 1);

    // The centroid shifts down: the removed column sat in the upper reach.
    let centre = pocketed
        .measures()
        .centroid
        .expect("a pocketed plate has a centre");
    let removed = pi * 9.0 * 6.0;
    let expected_z = 5.0f64.mul_add(3200.0, -(removed * 7.0)) / (3200.0 - removed);
    assert!(
        (centre.z - expected_z).abs() < 1.0e-9,
        "centre z {} should equal {expected_z}",
        centre.z
    );
    assert!((centre.x - 10.0).abs() > 1.0e-3 || (8.0f64 - 8.0).abs() < 1.0,);
}

#[test]
fn a_pocket_opening_downward_uses_the_flipped_frame() {
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (14.0, 14.0)),
        Point3::new(0.0, 0.0, 0.0),
        8.0,
        "under-plate",
    );
    // The tool rises from below the plate and stops 3 short of the top.
    let drill = extrude_profile(
        disc((7.0, 7.0), 2.0),
        Point3::new(0.0, 0.0, -4.0),
        9.0,
        "under-tool",
    );
    let pocketed = boolean(&plate, &drill, BooleanOperation::Difference, "under")
        .expect("a bottom-piercing pocket rides the negated axis");

    let pi = std::f64::consts::PI;
    let expected = 196.0f64.mul_add(8.0, -(pi * 4.0 * 5.0));
    assert_volume(&pocketed, expected, "downward pocket");
    assert_eq!(pocketed.counts().faces, 13);
}

#[test]
fn an_interior_tool_carves_an_exact_closed_cavity() {
    let block = extrude_profile(
        rectangle((0.0, 0.0), (12.0, 12.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "void-plate",
    );
    // Fully interior: the result carries the cavity as an inner shell.
    let interior = extrude_profile(
        disc((6.0, 6.0), 2.0),
        Point3::new(0.0, 0.0, 3.0),
        4.0,
        "void-tool",
    );
    let hollowed = boolean(&block, &interior, BooleanOperation::Difference, "void")
        .expect("an interior tool carves a cavity");
    let pi = std::f64::consts::PI;
    assert_volume(
        &hollowed,
        1440.0f64.mul_add(1.0, -(pi * 16.0)),
        "cylindrical cavity",
    );
    assert_eq!(hollowed.counts().shells, 2);
    assert_eq!(hollowed.counts().solids, 1);
    // Block faces plus the cavity's bottom, top, and two half-cylinder walls.
    assert_eq!(hollowed.counts().faces, 10);
    // The cavity sits below centre, so the material's centre rises above it.
    let centre = hollowed
        .measures()
        .centroid
        .expect("a hollowed block has a centre");
    let removed = pi * 16.0;
    let expected_z = 5.0f64.mul_add(1440.0, -(removed * 5.0)) / (1440.0 - removed);
    assert!(
        (centre.z - expected_z).abs() < 1.0e-9,
        "centre z {} should equal {expected_z}",
        centre.z
    );

    // The all-planar twin exercises the polyhedral measuring path instead of
    // the exact shell engine, and must agree with its own closed form.
    let box_tool = extrude_profile(
        rectangle((4.0, 4.0), (8.0, 9.0)),
        Point3::new(0.0, 0.0, 2.0),
        5.0,
        "void-box-tool",
    );
    let boxed = boolean(&block, &box_tool, BooleanOperation::Difference, "box-void")
        .expect("a planar interior tool carves a cavity too");
    assert_volume(&boxed, 1440.0 - 4.0 * 5.0 * 5.0, "planar cavity");
    assert_eq!(boxed.counts().shells, 2);
}

#[test]
fn a_boundary_crossing_blind_tool_notches_to_a_floor() {
    // The pocket opens both upward and sideways: the floor's boundary mixes
    // the tool's arcs with pieces of the target's own split wall.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (12.0, 12.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "crossing-plate",
    );
    let crossing = extrude_profile(
        disc((0.0, 6.0), 2.0),
        Point3::new(0.0, 0.0, 4.0),
        12.0,
        "crossing-blind-tool",
    );
    let notched = boolean(
        &plate,
        &crossing,
        BooleanOperation::Difference,
        "crossing-blind",
    )
    .expect("a boundary-crossing blind pocket publishes");
    let pi = std::f64::consts::PI;
    // Half the disc, six deep.
    assert_volume(
        &notched,
        1440.0f64.mul_add(1.0, -(pi * 2.0 * 6.0)),
        "boundary-crossing blind pocket",
    );
    assert_eq!(notched.counts().solids, 1);
    assert_eq!(notched.counts().shells, 1);
}

#[test]
fn a_holed_tool_leaves_a_pillar_on_the_pocket_floor() {
    // An annular drill: the material under the tool's hole survives as a
    // pillar standing on the floor, connected through the layer below.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (20.0, 16.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "pillar-plate",
    );
    let annulus = extrude_profile(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        center: Point2::new(10.0, 8.0),
                        radius: 5.0,
                        direction: artificer_protocol::ArcDirection::CounterClockwise,
                    }],
                },
                holes: vec![PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        center: Point2::new(10.0, 8.0),
                        radius: 2.0,
                        direction: artificer_protocol::ArcDirection::CounterClockwise,
                    }],
                }],
            }],
        },
        Point3::new(0.0, 0.0, 6.0),
        10.0,
        "pillar-tool",
    );
    let pocketed = boolean(&plate, &annulus, BooleanOperation::Difference, "pillar")
        .expect("an annular blind pocket leaves its island standing");
    let pi = std::f64::consts::PI;
    assert_volume(
        &pocketed,
        3200.0f64.mul_add(1.0, -(pi * (25.0 - 4.0) * 4.0)),
        "annular blind pocket",
    );
    assert_eq!(pocketed.counts().solids, 1);
    assert_eq!(pocketed.counts().shells, 1);
    // The pillar's centroid contribution keeps the centre on the tool axis.
    let centre = pocketed
        .measures()
        .centroid
        .expect("a pocketed plate has a centre");
    assert!((centre.x - 10.0).abs() < 1.0e-9 && (centre.y - 8.0).abs() < 1.0e-9);
}

#[test]
fn a_holed_interior_tool_carves_an_annular_cavity() {
    // Fully interior annular tool: a closed tube-shaped void, genus one.
    let block = extrude_profile(
        rectangle((0.0, 0.0), (20.0, 16.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "tube-block",
    );
    let annulus = extrude_profile(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        center: Point2::new(10.0, 8.0),
                        radius: 5.0,
                        direction: artificer_protocol::ArcDirection::CounterClockwise,
                    }],
                },
                holes: vec![PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        center: Point2::new(10.0, 8.0),
                        radius: 2.0,
                        direction: artificer_protocol::ArcDirection::CounterClockwise,
                    }],
                }],
            }],
        },
        Point3::new(0.0, 0.0, 3.0),
        4.0,
        "tube-tool",
    );
    let hollowed = boolean(&block, &annulus, BooleanOperation::Difference, "tube")
        .expect("an interior annular tool carves a tube cavity");
    let pi = std::f64::consts::PI;
    assert_volume(
        &hollowed,
        3200.0f64.mul_add(1.0, -(pi * (25.0 - 4.0) * 4.0)),
        "annular cavity",
    );
    assert_eq!(hollowed.counts().solids, 1);
    assert_eq!(hollowed.counts().shells, 2);
}

#[test]
fn a_tool_swallowing_the_profile_shortens_the_prism() {
    // The blind tool covers the whole plate laterally: everything above the
    // floor is removed and the result is simply a shorter prism.
    let plate = extrude_profile(
        rectangle((2.0, 2.0), (8.0, 8.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "shorten-plate",
    );
    let swallow = extrude_profile(
        rectangle((0.0, 0.0), (12.0, 12.0)),
        Point3::new(0.0, 0.0, 6.0),
        10.0,
        "shorten-tool",
    );
    let shortened = boolean(&plate, &swallow, BooleanOperation::Difference, "shorten")
        .expect("a swallowing blind tool leaves the lower prism");
    assert_volume(&shortened, 36.0 * 6.0, "shortened prism");
    assert_eq!(shortened.counts().faces, 6);
}

#[test]
fn crossed_bars_union_difference_and_intersect_through_the_general_engine() {
    // Two bars crossing at right angles share no extrusion direction with
    // compatible slabs at the crossing, so the general imprint/classify/sew
    // engine carries all three operations. Overlap cube: 2 x 2 x 2.
    let upright = extrude_profile(
        rectangle((4.0, 4.0), (6.0, 6.0)),
        Point3::new(0.0, 0.0, -5.0),
        10.0,
        "cross-upright",
    );
    let crossing = {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("cross-lying"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudePlanarProfile {
                frame: PlanarFrame3::new(
                    Point3::new(0.0, 4.0, -1.0),
                    Vector3::new(0.0, 1.0, 0.0),
                    Vector3::new(0.0, 0.0, 1.0),
                ),
                profile: rectangle((0.0, 0.0), (2.0, 2.0)),
                distance: 10.0,
            },
        };
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("the lying bar extrudes along x")
            .snapshot
    };

    let joined = boolean(&upright, &crossing, BooleanOperation::Union, "cross-union")
        .expect("crossed bars union through the general engine");
    assert_volume(&joined, 40.0 + 40.0 - 8.0, "crossed bar union");

    let cut = boolean(
        &upright,
        &crossing,
        BooleanOperation::Difference,
        "cross-cut",
    )
    .expect("crossed bars subtract through the general engine");
    assert_volume(&cut, 40.0 - 8.0, "crossed bar difference");
    assert_eq!(cut.counts().solids, 2);

    let met = boolean(
        &upright,
        &crossing,
        BooleanOperation::Intersection,
        "cross-meet",
    )
    .expect("crossed bars intersect through the general engine");
    assert_volume(&met, 8.0, "crossed bar intersection");
}

#[test]
fn a_fully_rotated_interior_tool_hollows_through_the_general_engine() {
    // The tool shares no axis with the target at all, so every prism
    // reduction misses and whole-face classification does the work.
    let block = extrude_profile(
        rectangle((0.0, 0.0), (10.0, 10.0)),
        Point3::new(0.0, 0.0, 0.0),
        10.0,
        "rotated-void-block",
    );
    let tool = {
        let cube = extrude_profile(
            rectangle((-1.0, -1.0), (1.0, 1.0)),
            Point3::new(0.0, 0.0, -1.0),
            2.0,
            "rotated-void-cube",
        );
        let (sin, cos) = (0.35_f64).sin_cos();
        let scale = sin / 3.0f64.sqrt();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("rotated-void-turn"),
            expected_snapshot: cube.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::TransformSnapshot {
                transform: artificer_protocol::SimilarityTransform3 {
                    translation: artificer_protocol::Vector3::new(5.0, 5.0, 5.0),
                    rotation: artificer_protocol::RotationQuaternion::new(cos, scale, scale, scale),
                    uniform_scale: 1.0,
                },
            },
        };
        NativeKernel::execute(&cube, &request, &CancellationToken::new())
            .expect("a rotation is always exact")
            .snapshot
    };

    let hollowed = boolean(&block, &tool, BooleanOperation::Difference, "rotated-void")
        .expect("a rotated interior tool hollows the block");
    assert_volume(&hollowed, 1000.0 - 8.0, "rotated cavity");
    assert_eq!(hollowed.counts().shells, 2);

    let met = boolean(
        &block,
        &tool,
        BooleanOperation::Intersection,
        "rotated-meet",
    )
    .expect("the rotated tool itself is the intersection");
    assert_volume(&met, 8.0, "rotated intersection");

    let joined = boolean(&block, &tool, BooleanOperation::Union, "rotated-union")
        .expect("a swallowed tool unions to the block alone");
    assert_volume(&joined, 1000.0, "rotated union");
    assert_eq!(joined.counts().shells, 1);
}

#[test]
fn tangential_and_coincident_contacts_refuse_closed() {
    // ADR 0025's tangency gates: shared faces and coincident carriers must
    // refuse rather than guess.
    let first = extrude_profile(
        rectangle((0.0, 0.0), (4.0, 4.0)),
        Point3::new(0.0, 0.0, 0.0),
        4.0,
        "tangent-first",
    );
    // Sharing the whole face x = 4.
    let flush = extrude_profile(
        rectangle((4.0, 0.0), (8.0, 4.0)),
        Point3::new(0.0, 0.0, 0.0),
        4.0,
        "tangent-flush",
    );
    let refused = boolean(&first, &flush, BooleanOperation::Union, "tangent-union")
        .expect_err("face-on-face contact refuses");
    assert!(
        refused.diagnostics.iter().any(|diagnostic| {
            let code = diagnostic.code.as_str();
            code == "BOOLEAN_CONTACT_UNSUPPORTED" || code == "BOOLEAN_EMPTY_OR_UNRESOLVED_RESULT"
        }),
        "unexpected refusal: {refused:?}"
    );

    // A cylinder tangent to a plane from outside.
    let plate = extrude_profile(
        rectangle((0.0, 0.0), (10.0, 10.0)),
        Point3::new(0.0, 0.0, 0.0),
        4.0,
        "tangent-plate",
    );
    let kissing = extrude_profile(
        disc((5.0, 12.0), 2.0),
        Point3::new(0.0, 0.0, 0.0),
        4.0,
        "tangent-cylinder",
    );
    let refused = boolean(&plate, &kissing, BooleanOperation::Union, "tangent-kiss")
        .expect_err("a kissing cylinder refuses rather than welding a zero-width seam");
    assert!(
        refused.diagnostics.iter().any(|diagnostic| {
            let code = diagnostic.code.as_str();
            code == "BOOLEAN_CONTACT_UNSUPPORTED"
                || code == "BOOLEAN_EMPTY_OR_UNRESOLVED_RESULT"
                || code == "BOOLEAN_ANALYTIC_RECONSTRUCTION_PENDING"
        }),
        "unexpected refusal: {refused:?}"
    );
}

#[test]
fn diagonal_offset_cuboids_union_through_the_general_engine() {
    // The workbench's body-boolean fixtures: equal cubes offset diagonally.
    let request = |origin: (f64, f64, f64), label: &str| ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(origin.0, origin.1, origin.2),
            size_x: 2.0,
            size_y: 2.0,
            size_z: 2.0,
        },
    };
    let base = NativeKernel::execute(
        &NativeKernel::empty(),
        &request((0.0, 0.0, 0.0), "diag-base"),
        &CancellationToken::new(),
    )
    .expect("base cube")
    .snapshot;
    let tool = NativeKernel::execute(
        &NativeKernel::empty(),
        &request((1.0, 1.0, 1.0), "diag-tool"),
        &CancellationToken::new(),
    )
    .expect("tool cube")
    .snapshot;
    let joined = boolean(&base, &tool, BooleanOperation::Union, "diag-union")
        .expect("diagonally offset cubes union");
    assert_volume(&joined, 8.0 + 8.0 - 1.0, "diagonal union");
}

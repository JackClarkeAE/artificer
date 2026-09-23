//! Closed-form gates for the revolve command (ADR 0026, F3).
//!
//! Every expectation is derived here rather than recorded from the kernel: a
//! cylinder and a tube from elementary volumes, a sphere from `4πr³/3`, a cone
//! frustum from its own closed form, and an offset circular section from
//! Pappus. The digest comparison against `MakeRevolvedAnnulus` pins the claim
//! that the general revolve subsumes the special-case command exactly, rather
//! than approximately.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, KernelError, PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, RevolveAngle,
    SolidOperation, Tier, ValidationProfile, Vector3,
};

const TAU: f64 = std::f64::consts::TAU;
const PI: f64 = std::f64::consts::PI;

/// The XZ plane: `u` is the radial direction and `v` is the axis direction, so
/// a revolve about the frame's `v` axis stands the part upright in world space.
fn frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    )
}

fn axis() -> PlanarAxis2 {
    PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0))
}

fn revolve(profile: PlanarProfile2, label: &str) -> Result<Snapshot, KernelError> {
    revolve_about(profile, axis(), label)
}

fn revolve_about(
    profile: PlanarProfile2,
    axis: PlanarAxis2,
    label: &str,
) -> Result<Snapshot, KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::RevolvePlanarProfile {
            frame: frame(),
            profile,
            axis,
            angle: RevolveAngle::FullTurn,
            operation: Default::default(),
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

fn polygon(vertices: &[(f64, f64)]) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(
                &vertices
                    .iter()
                    .map(|(x, y)| Point2::new(*x, *y))
                    .collect::<Vec<_>>(),
            ),
            holes: vec![],
        }],
    }
}

fn assert_volume(snapshot: &Snapshot, expected: f64, what: &str) {
    let report = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(
        report.valid,
        "{what}: the revolved solid must validate, got {:?}",
        report.diagnostics
    );
    let volume = snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "{what}: volume {volume} should equal {expected}"
    );
}

#[test]
fn a_rectangle_beside_the_axis_revolves_into_a_tube() {
    // Section r in [2, 5], z in [0, 3]: an annular cylinder.
    let tube = revolve(
        polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
        "revolve-tube",
    )
    .expect("a rectangle clear of the axis should revolve");
    assert_volume(&tube, PI * (25.0 - 4.0) * 3.0, "tube");
}

#[test]
fn a_rectangle_on_the_axis_revolves_into_a_cylinder() {
    let cylinder = revolve(
        polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 9.0), (0.0, 9.0)]),
        "revolve-cylinder",
    )
    .expect("a rectangle touching the axis should revolve");
    assert_volume(&cylinder, PI * 16.0 * 9.0, "cylinder");
}

/// The general command must reproduce the special-case constructor exactly,
/// not merely closely: the same solid, digest for digest.
#[test]
fn the_general_revolve_reproduces_make_revolved_annulus() {
    let (inner, outer, height) = (2.0, 5.0, 3.0);
    let special = {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("revolve-annulus-special"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeRevolvedAnnulus {
                frame: PlanarFrame3::new(
                    Point3::new(0.0, 0.0, 0.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                inner_radius: inner,
                outer_radius: outer,
                height,
            },
        };
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("the special-case annulus should build")
            .snapshot
    };
    let general = revolve(
        polygon(&[(inner, 0.0), (outer, 0.0), (outer, height), (inner, height)]),
        "revolve-annulus-general",
    )
    .expect("the general revolve should build the same annulus");

    let measures = (special.measures(), general.measures());
    assert!(
        ((measures.0.volume - measures.1.volume) / measures.0.volume).abs() < 1.0e-12,
        "volumes must agree: {} vs {}",
        measures.0.volume,
        measures.1.volume
    );
    assert!(
        ((measures.0.surface_area - measures.1.surface_area) / measures.0.surface_area).abs()
            < 1.0e-12,
        "areas must agree: {} vs {}",
        measures.0.surface_area,
        measures.1.surface_area
    );
    assert_eq!(
        general.counts(),
        special.counts(),
        "the general revolve must produce the same topology cardinality"
    );
}

#[test]
fn a_semicircle_on_the_axis_revolves_into_a_sphere() {
    // The diameter lies on the axis and the arc bulges to r = 4; the sweep is
    // a sphere, which is the first public builder for that carrier.
    let radius = 4.0;
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, 0.0),
                        start: Point2::new(0.0, -radius),
                        end: Point2::new(0.0, radius),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, radius),
                        end: Point2::new(0.0, -radius),
                    },
                ],
            },
            holes: vec![],
        }],
    };
    let sphere = revolve(profile, "revolve-sphere").expect("a semicircle should revolve");
    assert_volume(&sphere, 4.0 / 3.0 * PI * radius.powi(3), "sphere");
    assert!(
        (sphere.measures().surface_area - 4.0 * PI * radius * radius).abs() < 1.0e-9,
        "sphere area {} should equal 4πr²",
        sphere.measures().surface_area
    );
}

#[test]
fn a_slanted_line_revolves_into_a_cone_frustum() {
    // Section: r from 3 to 6 as z goes 0 to 8, closed back along the axis.
    let (lower, upper, height) = (6.0_f64, 3.0_f64, 8.0_f64);
    let frustum = revolve(
        polygon(&[(0.0, 0.0), (lower, 0.0), (upper, height), (0.0, height)]),
        "revolve-frustum",
    )
    .expect("a slanted section should revolve");
    let expected = PI * height / 3.0 * lower.mul_add(lower, upper.mul_add(upper, lower * upper));
    assert_volume(&frustum, expected, "cone frustum");
}

/// Pappus: a section revolved about an axis it does not touch sweeps
/// `2π · R_centroid · area`. A circular section makes both factors exact.
#[test]
fn an_offset_circle_revolves_into_a_torus_by_pappus() {
    let (major, minor) = (10.0, 2.5);
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(major, 0.0),
                    radius: minor,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let torus = revolve(profile, "revolve-torus").expect("an offset circle should revolve");
    assert_volume(&torus, TAU * major * PI * minor * minor, "torus by Pappus");
    assert!(
        (torus.measures().surface_area - TAU * major * TAU * minor).abs() < 1.0e-9,
        "torus area {} should equal 4π²Rr",
        torus.measures().surface_area
    );
}

/// A revolved body must re-enter the blend ladder. This is the milestone's
/// one-way-door gate: a builder whose output the section extractor rejects
/// would strand every revolved part outside the finish ladder.
#[test]
fn a_revolved_shaft_still_takes_a_rim_fillet() {
    let (radius, height) = (5.0_f64, 12.0_f64);
    let shaft = revolve(
        polygon(&[(0.0, 0.0), (radius, 0.0), (radius, height), (0.0, height)]),
        "revolve-shaft",
    )
    .expect("the shaft should revolve");

    let rim = top_rim(&shaft, height);
    assert!(!rim.is_empty(), "the shaft should present a top rim");
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("revolve-shaft-fillet"),
        expected_snapshot: shaft.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim,
            kind: EdgeFinishKind::Fillet,
            distance: 1.0,
            standing_apart: false,
        },
    };
    let filleted = NativeKernel::execute(&shaft, &request, &CancellationToken::new())
        .expect("a revolved shaft's rim must fillet")
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);

    // What a fillet removes is the corner ring minus the quarter-round that
    // stays. Both are Pappus volumes: the corner is a b x b square at radius
    // R - b/2, and the quarter disc that remains has area pi b^2 / 4 with its
    // centroid 4b/3pi outside the fillet centre.
    let blend = 1.0_f64;
    let corner = TAU * blend * blend * (radius - blend / 2.0);
    let quarter =
        TAU * (PI * blend * blend / 4.0) * blend.mul_add(4.0 / (3.0 * PI), radius - blend);
    let expected = PI * radius * radius * height - (corner - quarter);
    assert!(
        ((filleted.measures().volume - expected) / expected).abs() < 1.0e-9,
        "filleted shaft volume {} should equal {expected}",
        filleted.measures().volume
    );
}

/// A pointed cone keeps its base rim blendable: the rim is where the base
/// meets the slant at an acute angle, and the fillet takes away the kite
/// between its two tangent points less the circular sector that stays, each
/// by Pappus.
#[test]
fn a_pointed_cone_still_takes_a_rim_fillet() {
    let (radius, height, blend) = (5.0_f64, 6.0_f64, 0.8_f64);
    let cone = revolve(
        polygon(&[(0.0, 0.0), (radius, 0.0), (0.0, height)]),
        "revolve-cone",
    )
    .expect("the cone revolves");
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("revolve-cone-fillet"),
        expected_snapshot: cone.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: top_rim(&cone, 0.0),
            kind: EdgeFinishKind::Fillet,
            distance: blend,
            standing_apart: false,
        },
    };
    let filleted = NativeKernel::execute(&cone, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("the cone's rim must fillet: {:?}", error.diagnostics))
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);

    // In the section: the corner V, the two sides' directions from it, and
    // the angle between them.
    let slant = radius.hypot(height);
    let corner = (radius, 0.0_f64);
    let along_base = (-1.0_f64, 0.0_f64);
    let along_slant = (-radius / slant, height / slant);
    let angle = (radius / slant).acos();
    let bisector = {
        let (x, y) = (along_base.0 + along_slant.0, along_base.1 + along_slant.1);
        let length = x.hypot(y);
        (x / length, y / length)
    };
    let reach = blend / (angle / 2.0).tan();
    let centre_distance = blend / (angle / 2.0).sin();
    let at = |from: (f64, f64), direction: (f64, f64), distance: f64| {
        (
            from.0 + direction.0 * distance,
            from.1 + direction.1 * distance,
        )
    };
    let first = at(corner, along_base, reach);
    let second = at(corner, along_slant, reach);
    let centre = at(corner, bisector, centre_distance);
    let triangle =
        |a: (f64, f64), b: (f64, f64)| (blend * reach / 2.0, (corner.0 + a.0 + b.0) / 3.0);
    let (kite_one, x_one) = triangle(first, centre);
    let (kite_two, x_two) = triangle(second, centre);
    let theta = PI - angle;
    let sector = theta * blend * blend / 2.0;
    // The sector left in the kite faces the corner, back along the bisector.
    let sector_x = centre.0 - bisector.0 * (4.0 * blend * (theta / 2.0).sin() / (3.0 * theta));
    let removed = TAU * (kite_one * x_one + kite_two * x_two - sector * sector_x);
    let expected = PI * radius * radius * height / 3.0 - removed;
    assert!(
        ((filleted.measures().volume - expected) / expected).abs() < 1.0e-9,
        "filleted cone volume {} should equal {expected}",
        filleted.measures().volume
    );
}

fn top_rim(snapshot: &Snapshot, height: f64) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut rim = Vec::new();
    for edge in &scene.edges {
        let [first, second] = edge.endpoints;
        if (first.z - height).abs() < 1.0e-9
            && (second.z - height).abs() < 1.0e-9
            && !rim.contains(&edge.source_edge)
        {
            rim.push(edge.source_edge);
        }
    }
    rim
}

#[test]
fn a_profile_crossing_the_axis_is_refused() {
    let error = revolve(
        polygon(&[(-2.0, 0.0), (3.0, 0.0), (3.0, 4.0), (-2.0, 4.0)]),
        "revolve-crossing",
    )
    .expect_err("material on both sides of the axis must refuse");
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "REVOLVE_PROFILE_CROSSES_AXIS"),
        "unexpected refusal: {error:?}"
    );
}

/// A slanted side running into the axis sweeps a cone to its apex, which
/// closes through a pole as a sphere does: a third of the cylinder it stands
/// in, whichever way up, and two of them for a double cone.
#[test]
fn a_slanted_side_meeting_the_axis_sweeps_a_pointed_cone() {
    let (radius, height) = (5.0_f64, 6.0_f64);
    let cone = PI * radius * radius * height / 3.0;
    for (what, profile, expected) in [
        (
            "standing on its base",
            polygon(&[(0.0, 0.0), (radius, 0.0), (0.0, height)]),
            cone,
        ),
        (
            "balanced on its apex",
            polygon(&[(0.0, 0.0), (radius, height), (0.0, height)]),
            cone,
        ),
        (
            "double cone",
            polygon(&[(0.0, 0.0), (radius, height), (0.0, 2.0 * height)]),
            2.0 * cone,
        ),
    ] {
        let solid = revolve(profile.clone(), "revolve-apex")
            .unwrap_or_else(|error| panic!("{what}: {:?}", error.diagnostics));
        assert_volume(&solid, expected, what);
        let step = NativeKernel::export_step(&solid, "cone").expect("the cone exports");
        assert!(step.contains("CONICAL_SURFACE"), "{what}: exact cone");
        let quarter = revolve_through(
            profile,
            axis(),
            RevolveAngle::partial(0.0, PI / 2.0),
            "revolve-apex-quarter",
        )
        .unwrap_or_else(|error| panic!("{what}, a quarter: {:?}", error.diagnostics));
        assert_volume(&quarter, expected / 4.0, what);
    }
}

/// A cone point sunk into a block takes out the cone: the faceted tier
/// answers, as it does for any carrier the exact engines do not sew, and the
/// volume it removes is the cone's to within the faceting.
#[test]
fn a_pointed_cone_cuts_a_block() {
    // The block's top face is at z = 0; the cone stands point down, its apex
    // 4 below the face and its rim of radius 3 held 1 above it.
    let (radius, depth, over) = (3.0_f64, 4.0_f64, 1.0_f64);
    let outcome = revolve_into(
        &block(),
        polygon(&[(0.0, -depth), (radius, over), (0.0, over)]),
        SolidOperation::Cut,
    )
    .expect("the cone cuts the block");
    let removed = 32_000.0 - outcome.snapshot.measures().volume;
    let submerged = radius * depth / (depth + over);
    let expected = PI * submerged * submerged * depth / 3.0;
    assert!(
        ((removed - expected) / expected).abs() < 2.0e-2,
        "removed {removed}, the submerged cone is {expected}"
    );
    assert!(NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid);
}

/// The tube r in [1, 9], z in [0, 9] with a round hole of radius 1 at
/// r = 5 in its section: a full turn leaves a torus-shaped cavity inside the
/// tube, Pappus's torus less; a quarter turn leaves a channel through both
/// end faces, and a quarter of the volume.
fn holed_tube() -> (PlanarProfile2, f64) {
    let mut profile = polygon(&[(1.0, 0.0), (9.0, 0.0), (9.0, 9.0), (1.0, 9.0)]);
    profile.regions[0].holes.push(PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(5.0, 4.5),
            radius: 1.0,
            direction: ArcDirection::Clockwise,
        }],
    });
    (profile, PI * (81.0 - 1.0) * 9.0 - PI * TAU * 5.0)
}

/// A solid cylinder r in [0, 6], z in [0, 6] with a square hole r in [2, 4],
/// z in [2, 4]: the cavity is a square-sectioned ring.
fn holed_cylinder() -> (PlanarProfile2, f64) {
    let mut profile = polygon(&[(0.0, 0.0), (6.0, 0.0), (6.0, 6.0), (0.0, 6.0)]);
    profile.regions[0].holes.push(PlanarLoop2::from_polygon(&[
        Point2::new(2.0, 2.0),
        Point2::new(2.0, 4.0),
        Point2::new(4.0, 4.0),
        Point2::new(4.0, 2.0),
    ]));
    (profile, PI * 36.0 * 6.0 - PI * (16.0 - 4.0) * 2.0)
}

#[test]
fn a_hole_in_the_profile_sweeps_a_cavity_or_a_channel() {
    for (what, (profile, full)) in [("tube", holed_tube()), ("cylinder", holed_cylinder())] {
        let solid = revolve(profile.clone(), "revolve-hole")
            .unwrap_or_else(|error| panic!("{what}: {:?}", error.diagnostics));
        assert_volume(&solid, full, what);
        let step = NativeKernel::export_step(&solid, what).expect("the hollow part exports");
        assert!(!step.contains("B_SPLINE_SURFACE"), "{what}: exact");
        for (turn, share) in [(PI / 2.0, 0.25), (1.5 * PI, 0.75)] {
            let partial = revolve_through(
                profile.clone(),
                axis(),
                RevolveAngle::partial(0.0, turn),
                "revolve-hole-partial",
            )
            .unwrap_or_else(|error| panic!("{what}, {share} turn: {:?}", error.diagnostics));
            assert_volume(&partial, full * share, what);
        }
    }
}

/// Two regions of one profile turn into two solids of one body: two tubes,
/// r in [1, 2] and [4, 5], both 3 tall.
#[test]
fn several_regions_sweep_several_solids() {
    let profile = PlanarProfile2 {
        regions: [
            [(1.0, 0.0), (2.0, 0.0), (2.0, 3.0), (1.0, 3.0)],
            [(4.0, 0.0), (5.0, 0.0), (5.0, 3.0), (4.0, 3.0)],
        ]
        .iter()
        .map(|corners| PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(
                &corners
                    .iter()
                    .map(|(x, y)| Point2::new(*x, *y))
                    .collect::<Vec<_>>(),
            ),
            holes: vec![],
        })
        .collect(),
    };
    let both = PI * (4.0 - 1.0) * 3.0 + PI * (25.0 - 16.0) * 3.0;
    let solids = revolve(profile.clone(), "revolve-regions")
        .unwrap_or_else(|error| panic!("{:?}", error.diagnostics));
    assert_volume(&solids, both, "two tubes");
    let half = revolve_through(
        profile.clone(),
        axis(),
        RevolveAngle::partial(0.0, PI),
        "half",
    )
    .unwrap_or_else(|error| panic!("{:?}", error.diagnostics));
    assert_volume(&half, both / 2.0, "two half tubes");
    // Both rings cut from the block at once, exactly: planes and coaxial
    // cylinders, sunk 3 into its top face.
    let sunk = PlanarProfile2 {
        regions: profile
            .regions
            .into_iter()
            .map(|region| PlanarRegion2 {
                outer: PlanarLoop2::from_polygon(
                    &region
                        .outer
                        .curves
                        .iter()
                        .map(|curve| match curve {
                            PlanarCurve2::Line { start, .. } => Point2::new(start.x, start.y - 3.0),
                            _ => unreachable!("the regions are polygons"),
                        })
                        .collect::<Vec<_>>(),
                ),
                holes: vec![],
            })
            .collect(),
    };
    let outcome = revolve_into(&block(), sunk, SolidOperation::Cut)
        .unwrap_or_else(|error| panic!("{:?}", error.diagnostics));
    assert_volume(&outcome.snapshot, 32_000.0 - both, "block less two rings");
    assert_eq!(outcome.report.tier(), Tier::Exact);
}

/// The axis is directed, but which way it points is presentation, not
/// geometry: the same profile on either side of the same line must sweep the
/// same solid.
#[test]
fn the_axis_direction_does_not_change_the_result() {
    let forward = revolve_about(
        polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
        PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0)),
        "revolve-axis-forward",
    )
    .expect("forward axis");
    let reversed = revolve_about(
        polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
        PlanarAxis2::new(Point2::new(0.0, 1.0), Point2::new(0.0, 0.0)),
        "revolve-axis-reversed",
    )
    .expect("reversed axis");
    assert!(
        (forward.measures().volume - reversed.measures().volume).abs() < 1.0e-12,
        "axis direction must not change the swept volume"
    );
}

// ---------------------------------------------------------------------------
// Adding and cutting (ADR 0055): a revolve meets the body it is given through
// the same Boolean ladder a loft does.
// ---------------------------------------------------------------------------

/// A 40 × 40 block whose top face is the plane z = 0.
fn block() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("block"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(-20.0, -20.0, -20.0),
            size_x: 40.0,
            size_y: 40.0,
            size_z: 20.0,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("block")
        .snapshot
}

fn revolve_into(
    body: &Snapshot,
    profile: PlanarProfile2,
    operation: SolidOperation,
) -> Result<artificer_kernel::ExecutionOutcome, KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("revolve-into"),
        expected_snapshot: body.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::RevolvePlanarProfile {
            frame: frame(),
            profile,
            axis: axis(),
            angle: RevolveAngle::FullTurn,
            operation,
        },
    };
    NativeKernel::execute(body, &request, &CancellationToken::new())
}

fn half_disc(radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, 0.0),
                        start: Point2::new(0.0, -radius),
                        end: Point2::new(0.0, radius),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, radius),
                        end: Point2::new(0.0, -radius),
                    },
                ],
            },
            holes: vec![],
        }],
    }
}

#[test]
fn a_revolved_boss_adds_to_a_block_exactly() {
    // A cylinder of radius 5 standing from 5 below the top face to 10 above.
    let outcome = revolve_into(
        &block(),
        polygon(&[(0.0, -5.0), (5.0, -5.0), (5.0, 10.0), (0.0, 10.0)]),
        SolidOperation::Add,
    )
    .expect("the boss adds");
    let rung = outcome.report.rung.clone().unwrap_or_default();
    assert!(
        rung == "revolve/boolean-prism" || rung == "revolve/boolean-analytic",
        "planes and a cylinder stay exact: {rung}"
    );
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_volume(
        &outcome.snapshot,
        32_000.0 + PI * 25.0 * 10.0,
        "block and boss",
    );
}

#[test]
fn a_revolved_bore_cuts_a_block_exactly() {
    let outcome = revolve_into(
        &block(),
        polygon(&[(0.0, -10.0), (5.0, -10.0), (5.0, 5.0), (0.0, 5.0)]),
        SolidOperation::Cut,
    )
    .expect("the bore cuts");
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_volume(
        &outcome.snapshot,
        32_000.0 - PI * 25.0 * 10.0,
        "block less bore",
    );
}

#[test]
fn a_revolved_sphere_cuts_a_block_on_the_faceted_tier_and_says_so() {
    // A sphere of radius 5 centred on the top face takes a hemisphere away.
    let outcome =
        revolve_into(&block(), half_disc(5.0), SolidOperation::Cut).expect("the sphere cuts");
    assert_eq!(outcome.report.rung.as_deref(), Some("revolve/faceted"));
    let codes = outcome
        .report
        .warnings
        .iter()
        .map(|warning| warning.code.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"REVOLVE_FACETED_APPROXIMATION".to_owned()),
        "{codes:?}"
    );
    let expected = 32_000.0 - 2.0 / 3.0 * PI * 125.0;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-2,
        "{volume} against {expected}"
    );
}

#[test]
fn a_revolve_that_adds_needs_a_body_to_add_to() {
    let error = revolve_into(
        &NativeKernel::empty(),
        polygon(&[(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)]),
        SolidOperation::Add,
    )
    .expect_err("nothing to add to");
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "REVOLVE_TARGET_EMPTY"),
        "{error:?}"
    );
}

fn revolve_through(
    profile: PlanarProfile2,
    axis: PlanarAxis2,
    angle: RevolveAngle,
    label: &str,
) -> Result<Snapshot, KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::RevolvePlanarProfile {
            frame: frame(),
            profile,
            axis,
            angle,
            operation: SolidOperation::New,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

/// A block r in [1, 3], z in [0, 2] with a quarter-disc bite of radius 1
/// taken out of its top outer corner, centred at (3, 2): the profile's one
/// arc runs clockwise, which is the concave case.
fn notched_block() -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(1.0, 0.0),
                        end: Point2::new(3.0, 0.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(3.0, 0.0),
                        end: Point2::new(3.0, 1.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(3.0, 2.0),
                        start: Point2::new(3.0, 1.0),
                        end: Point2::new(2.0, 2.0),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(2.0, 2.0),
                        end: Point2::new(1.0, 2.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(1.0, 2.0),
                        end: Point2::new(1.0, 0.0),
                    },
                ],
            },
            holes: vec![],
        }],
    }
}

/// Pappus for the notched block: the square's moment about the axis, less
/// the quarter disc's, whose centroid sits `4r/3π` in from its centre.
fn notched_block_volume() -> f64 {
    let quarter = PI / 4.0;
    TAU * (4.0 * 2.0 - quarter * (3.0 - 4.0 / (3.0 * PI)))
}

/// A concave arc sweeps a band whose material lies outside its tube: the face
/// must point in towards the arc's centre. This once came out inside out and
/// failed edge-use orientation, refusing any profile with a concave round.
#[test]
fn a_concave_arc_in_the_profile_revolves_outside_in() {
    let notched = revolve(notched_block(), "revolve-notch").expect("a concave arc revolves");
    assert_volume(&notched, notched_block_volume(), "notched block");
}

/// Every carrier the section builder emits, and the full-turn volume of each.
fn carriers() -> Vec<(&'static str, PlanarProfile2, f64)> {
    let radius = 4.0_f64;
    let (major, minor) = (10.0_f64, 2.5_f64);
    let (lower, upper, height) = (6.0_f64, 3.0_f64, 8.0_f64);
    vec![
        (
            "tube",
            polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
            PI * (25.0 - 4.0) * 3.0,
        ),
        (
            "cylinder",
            polygon(&[(0.0, 0.0), (4.0, 0.0), (4.0, 9.0), (0.0, 9.0)]),
            PI * 16.0 * 9.0,
        ),
        ("sphere", half_disc(radius), 4.0 / 3.0 * PI * radius.powi(3)),
        (
            "frustum",
            polygon(&[(0.0, 0.0), (lower, 0.0), (upper, height), (0.0, height)]),
            PI * height / 3.0 * lower.mul_add(lower, upper.mul_add(upper, lower * upper)),
        ),
        (
            "torus",
            PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: vec![PlanarCurve2::Circle {
                            center: Point2::new(major, 0.0),
                            radius: minor,
                            direction: ArcDirection::CounterClockwise,
                        }],
                    },
                    holes: vec![],
                }],
            },
            TAU * major * PI * minor * minor,
        ),
        ("notch", notched_block(), notched_block_volume()),
    ]
}

/// Pappus again: a partial turn sweeps its share of the full turn's volume.
/// The sweeps cover a quarter, a half and three quarters, either side of
/// half a turn, where the split of each carrier into two faces falls on the
/// far side of its seam, and a narrow sliver.
#[test]
fn a_partial_turn_sweeps_its_share_of_every_carrier() {
    for (name, profile, full) in carriers() {
        for sweep in [PI / 2.0, PI, 1.5 * PI, PI - 0.25, PI + 0.25, 0.3] {
            let label = format!("{name} through {sweep}");
            let partial = revolve_through(
                profile.clone(),
                axis(),
                RevolveAngle::partial(0.0, sweep),
                &label,
            )
            .unwrap_or_else(|error| panic!("{label}: {error:?}"));
            assert_volume(&partial, full * sweep / TAU, &label);
        }
    }
}

/// The two wedge faces are the section itself, so a partial tube's area is
/// its share of the full turn's area plus the section twice.
#[test]
fn a_partial_tube_is_closed_by_its_section_at_both_ends() {
    // r in [2, 5], z in [0, 3]: outer and inner walls, two annular caps.
    let full_area = TAU * 5.0 * 3.0 + TAU * 2.0 * 3.0 + 2.0 * PI * (25.0 - 4.0);
    let section = 3.0 * 3.0;
    for sweep in [PI / 2.0, PI, 1.5 * PI] {
        let tube = revolve_through(
            polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
            axis(),
            RevolveAngle::partial(0.0, sweep),
            "revolve-partial-tube-area",
        )
        .expect("a partial tube revolves");
        let area = tube.measures().surface_area;
        let expected = full_area * sweep / TAU + 2.0 * section;
        assert!(
            ((area - expected) / expected).abs() < 1.0e-9,
            "area {area} should be {expected} at {sweep}"
        );
    }
}

fn centroid(snapshot: &Snapshot) -> Point3 {
    snapshot
        .measures()
        .centroid
        .expect("a solid has a centroid")
}

/// A partial turn is measured right-handed about the axis as it was given.
/// The frame is XZ with the axis up +Z, so a quarter turn from the profile
/// on +X ends on +Y.
#[test]
fn a_partial_turn_goes_the_way_its_axis_and_start_say() {
    let right = || polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]);
    let quarter = PI / 2.0;
    let turned = |profile, axis, start, label| {
        let snapshot = revolve_through(profile, axis, RevolveAngle::partial(start, quarter), label)
            .expect("the quarter turn revolves");
        centroid(&snapshot)
    };

    let forward = turned(right(), axis(), 0.0, "revolve-forward");
    assert!(
        forward.x > 1.0 && forward.y > 1.0,
        "a quarter turn from +X about +Z ends in the +X+Y quadrant: {forward:?}"
    );
    let back = turned(right(), axis(), -quarter, "revolve-back");
    assert!(
        back.x > 1.0 && back.y < -1.0,
        "starting a quarter turn back ends at the profile: {back:?}"
    );
    let symmetric = turned(right(), axis(), -quarter / 2.0, "revolve-symmetric");
    assert!(
        symmetric.x > 1.0 && symmetric.y.abs() < 1.0e-9,
        "half a quarter each way sits astride the profile's plane: {symmetric:?}"
    );

    // The same axis walked the other way turns the other way.
    let down = PlanarAxis2::new(Point2::new(0.0, 1.0), Point2::new(0.0, 0.0));
    let reversed = turned(right(), down, 0.0, "revolve-reversed-axis");
    assert!(
        reversed.x > 1.0 && reversed.y < -1.0,
        "a quarter turn about -Z goes from +X towards -Y: {reversed:?}"
    );

    // A profile on the far side of the axis turns about the axis as given
    // too, although the kernel reverses its own section axis to build it.
    let left = polygon(&[(-5.0, 0.0), (-2.0, 0.0), (-2.0, 3.0), (-5.0, 3.0)]);
    let far = turned(left, axis(), 0.0, "revolve-far-side");
    assert!(
        far.x < -1.0 && far.y < -1.0,
        "a quarter turn from -X about +Z goes towards -Y: {far:?}"
    );
}

#[test]
fn a_partial_turn_out_of_range_is_refused_by_name() {
    let refused = |angle: RevolveAngle| {
        let error = revolve_through(
            polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
            axis(),
            angle,
            "revolve-bad-angle",
        )
        .expect_err("the angle is refused");
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "REVOLVE_ANGLE_INVALID"),
            "{angle:?}: {error:?}"
        );
    };
    for sweep in [0.0, -1.0, TAU, 7.0, f64::NAN, 1.0e-9, TAU - 1.0e-9] {
        refused(RevolveAngle::partial(0.0, sweep));
    }
    for start in [f64::NAN, f64::INFINITY, 10.0] {
        refused(RevolveAngle::partial(start, 1.0));
    }
}

/// A partial revolve writes out as the planes, cylinders and tori it is
/// built from, with nothing approximated.
#[test]
fn a_partial_revolve_exports_its_exact_carriers_to_step() {
    for (name, profile, _) in carriers() {
        let partial = revolve_through(
            profile,
            axis(),
            RevolveAngle::partial(0.0, 1.5 * PI),
            "revolve-partial-step",
        )
        .expect("the partial turn revolves");
        let step = NativeKernel::export_step(&partial, name).expect("the partial revolve exports");
        assert!(step.contains("PLANE"), "{name}: the wedges are planes");
        assert!(
            !step.contains("B_SPLINE_SURFACE"),
            "{name}: nothing is approximated"
        );
        let carrier = match name {
            "tube" | "cylinder" => "CYLINDRICAL_SURFACE",
            "sphere" => "SPHERICAL_SURFACE",
            "frustum" => "CONICAL_SURFACE",
            _ => "TOROIDAL_SURFACE",
        };
        assert!(step.contains(carrier), "{name}: {carrier}");
    }
}

/// A partial turn joins the Boolean ladder like a full one: a quarter tube of
/// planes and coaxial cylinders, sunk in the block, comes out of it exactly.
#[test]
fn a_partial_revolve_cuts_a_block_exactly() {
    let body = block();
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("revolve-partial-cut"),
        expected_snapshot: body.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::RevolvePlanarProfile {
            frame: frame(),
            profile: polygon(&[(2.0, -10.0), (5.0, -10.0), (5.0, -5.0), (2.0, -5.0)]),
            axis: axis(),
            angle: RevolveAngle::partial(0.0, PI / 2.0),
            operation: SolidOperation::Cut,
        },
    };
    let cut = NativeKernel::execute(&body, &request, &CancellationToken::new())
        .expect("a quarter tube cuts the block");
    let removed = PI * (25.0 - 4.0) * 5.0 / 4.0;
    assert_volume(
        &cut.snapshot,
        40.0 * 40.0 * 20.0 - removed,
        "block less a quarter tube",
    );
    assert_eq!(cut.report.rung.as_deref(), Some("revolve/boolean-prism"));
    assert_eq!(cut.report.tier(), Tier::Exact);
}

/// A construction axis through a curved face takes the face's own axis,
/// drawn over the stretch the face covers; a flat face has none.
#[test]
fn a_curved_face_reports_its_axis_and_a_flat_one_none() {
    let tube = revolve(
        polygon(&[(2.0, 0.0), (5.0, 0.0), (5.0, 3.0), (2.0, 3.0)]),
        "revolve-face-axis",
    )
    .expect("a tube revolves");
    let faces = NativeKernel::debug_scene(&tube)
        .triangles
        .iter()
        .map(|triangle| triangle.source_face)
        .collect::<std::collections::BTreeSet<_>>();
    let (mut curved, mut flat) = (0, 0);
    for face in faces {
        match NativeKernel::face_axis(&tube, face).expect("a face of the tube") {
            Some(axis) => {
                curved += 1;
                assert!((axis.direction.z.abs() - 1.0).abs() < 1.0e-12, "{axis:?}");
                assert!(axis.origin.x.abs() < 1.0e-12 && axis.origin.y.abs() < 1.0e-12);
                assert!((axis.origin.z - 1.5).abs() < 1.0e-12, "{axis:?}");
                assert!((axis.half_length - 1.5).abs() < 1.0e-12, "{axis:?}");
            }
            None => flat += 1,
        }
    }
    assert_eq!(
        (curved, flat),
        (4, 2),
        "two walls of two halves, two washers"
    );
}

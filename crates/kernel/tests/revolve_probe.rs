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
    ValidationProfile, Vector3,
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

#[test]
fn an_oblique_axis_contact_is_refused() {
    // A triangle whose slanted side runs into the axis would sweep an apex.
    let error = revolve(
        polygon(&[(0.0, 0.0), (5.0, 0.0), (0.0, 6.0)]),
        "revolve-apex",
    )
    .expect_err("a cone apex must refuse");
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "REVOLVE_OBLIQUE_AXIS_CONTACT"),
        "unexpected refusal: {error:?}"
    );
}

#[test]
fn a_profile_with_a_hole_is_refused() {
    let mut profile = polygon(&[(1.0, 0.0), (9.0, 0.0), (9.0, 9.0), (1.0, 9.0)]);
    profile.regions[0].holes.push(PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(5.0, 4.5),
            radius: 1.0,
            direction: ArcDirection::Clockwise,
        }],
    });
    let error = revolve(profile, "revolve-hole").expect_err("a hole must refuse in v1");
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "REVOLVE_SINGLE_REGION_ONLY"),
        "unexpected refusal: {error:?}"
    );
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

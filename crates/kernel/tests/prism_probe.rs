//! Exact vertical-edge finishes on arbitrary line/arc prisms (ADR 0023
//! frontier, milestone A).
//!
//! Every expectation is a closed form derived from the profile area, because
//! extruding a profile of area `A` to height `h` has volume `A·h` exactly. A
//! convex fillet of radius `f` at a right angle removes the corner square
//! minus the quarter disc, `(1 − π/4)f²`; a concave one adds the same area; a
//! chamfer of setback `d` removes the triangle `d²/2`.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const HEIGHT: f64 = 7.0;
/// L-profile dimensions: outer box `A_X × D_Y` with a bite taken out.
const A: f64 = 6.0;
const B: f64 = 10.0;
const C: f64 = 4.0;
const D: f64 = 9.0;

fn l_profile() -> PlanarProfile2 {
    // (0,0) → (B,0) → (B,C) → (A,C) → (A,D) → (0,D), counter-clockwise.
    // The vertex at (A, C) is the reflex (concave) corner.
    let corners = [(0.0, 0.0), (B, 0.0), (B, C), (A, C), (A, D), (0.0, D)];
    let curves = (0..corners.len())
        .map(|index| {
            let start = corners[index];
            let end = corners[(index + 1) % corners.len()];
            PlanarCurve2::Line {
                start: Point2::new(start.0, start.1),
                end: Point2::new(end.0, end.1),
            }
        })
        .collect();
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves },
            holes: vec![],
        }],
    }
}

fn profile_area() -> f64 {
    B * C + A * (D - C)
}

fn extrude(profile: PlanarProfile2, label: &str) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: HEIGHT,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("profile should extrude")
        .snapshot
}

/// The full-height vertical edge whose base sits at `(x, y)`.
fn vertical_edge_at(snapshot: &Snapshot, x: f64, y: f64) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .edges
        .iter()
        .find(|edge| {
            let [first, second] = edge.endpoints;
            let vertical = (first.x - second.x).abs() < 1.0e-9
                && (first.y - second.y).abs() < 1.0e-9
                && (first.z - second.z).abs() > HEIGHT - 1.0e-9;
            vertical && (first.x - x).abs() < 1.0e-9 && (first.y - y).abs() < 1.0e-9
        })
        .unwrap_or_else(|| panic!("no vertical edge at ({x}, {y})"))
        .source_edge
}

fn finish(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
    label: &str,
) -> Result<Snapshot, artificer_protocol::KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: targets,
            kind,
            distance,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < 1.0e-9,
        "{what}: {actual} should equal {expected}"
    );
}

#[test]
fn a_convex_vertical_fillet_removes_the_exact_corner_area() {
    let base = extrude(l_profile(), "prism-fillet-base");
    let fillet = 1.5_f64;
    let target = vertical_edge_at(&base, B, 0.0);
    let finished = finish(
        &base,
        vec![target],
        EdgeFinishKind::Fillet,
        fillet,
        "prism-fillet-convex",
    )
    .expect("a convex vertical fillet must commit");

    assert!(NativeKernel::validate(&finished, ValidationProfile::Solid).valid);
    let removed = (1.0 - std::f64::consts::FRAC_PI_4) * fillet * fillet;
    assert_close(
        finished.measures().volume,
        (profile_area() - removed) * HEIGHT,
        "filleted prism volume",
    );
}

#[test]
fn a_concave_vertical_fillet_adds_the_exact_corner_area() {
    let base = extrude(l_profile(), "prism-concave-base");
    let fillet = 1.25_f64;
    let target = vertical_edge_at(&base, A, C);
    let finished = finish(
        &base,
        vec![target],
        EdgeFinishKind::Fillet,
        fillet,
        "prism-fillet-concave",
    )
    .expect("a concave vertical fillet must commit");

    assert!(NativeKernel::validate(&finished, ValidationProfile::Solid).valid);
    let added = (1.0 - std::f64::consts::FRAC_PI_4) * fillet * fillet;
    assert_close(
        finished.measures().volume,
        (profile_area() + added) * HEIGHT,
        "concave filleted prism volume",
    );
}

#[test]
fn a_vertical_chamfer_removes_the_exact_setback_triangle() {
    let base = extrude(l_profile(), "prism-chamfer-base");
    let setback = 2.0_f64;
    let target = vertical_edge_at(&base, B, C);
    let finished = finish(
        &base,
        vec![target],
        EdgeFinishKind::Chamfer,
        setback,
        "prism-chamfer",
    )
    .expect("a vertical chamfer must commit");

    assert!(NativeKernel::validate(&finished, ValidationProfile::Solid).valid);
    assert_close(
        finished.measures().volume,
        (profile_area() - setback * setback / 2.0) * HEIGHT,
        "chamfered prism volume",
    );
}

#[test]
fn several_vertical_edges_finish_in_one_transaction() {
    let base = extrude(l_profile(), "prism-multi-base");
    let fillet = 1.0_f64;
    let targets = vec![
        vertical_edge_at(&base, 0.0, 0.0),
        vertical_edge_at(&base, B, 0.0),
        vertical_edge_at(&base, 0.0, D),
    ];
    let finished = finish(
        &base,
        targets,
        EdgeFinishKind::Fillet,
        fillet,
        "prism-multi-fillet",
    )
    .expect("three convex vertical fillets must commit together");

    assert!(NativeKernel::validate(&finished, ValidationProfile::Solid).valid);
    let removed = 3.0 * (1.0 - std::f64::consts::FRAC_PI_4) * fillet * fillet;
    assert_close(
        finished.measures().volume,
        (profile_area() - removed) * HEIGHT,
        "multi-fillet prism volume",
    );
}

#[test]
fn finishes_compose_across_transactions() {
    let base = extrude(l_profile(), "prism-compose-base");
    let fillet = 1.0_f64;
    let first = finish(
        &base,
        vec![vertical_edge_at(&base, 0.0, 0.0)],
        EdgeFinishKind::Fillet,
        fillet,
        "prism-compose-first",
    )
    .expect("the first fillet must commit");
    // The profile is recovered from committed topology, so a second finish on
    // the rebuilt body works without provenance.
    let second = finish(
        &first,
        vec![vertical_edge_at(&first, B, 0.0)],
        EdgeFinishKind::Fillet,
        fillet,
        "prism-compose-second",
    )
    .expect("a fillet on the already-finished prism must commit");

    assert!(NativeKernel::validate(&second, ValidationProfile::Solid).valid);
    let removed = 2.0 * (1.0 - std::f64::consts::FRAC_PI_4) * fillet * fillet;
    assert_close(
        second.measures().volume,
        (profile_area() - removed) * HEIGHT,
        "composed fillet volume",
    );
}

#[test]
fn a_line_to_arc_corner_finishes_against_an_independent_area_derivation() {
    // Quarter pie slice: origin, radial line to (R,0), arc back to (0,R).
    let radius = 8.0_f64;
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(radius, 0.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, 0.0),
                        start: Point2::new(radius, 0.0),
                        end: Point2::new(0.0, radius),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, radius),
                        end: Point2::new(0.0, 0.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };
    let base = extrude(profile, "pie-base");
    let fillet = 1.5_f64;
    let target = vertical_edge_at(&base, radius, 0.0);
    let finished = finish(
        &base,
        vec![target],
        EdgeFinishKind::Fillet,
        fillet,
        "pie-fillet",
    )
    .expect("a line/arc corner fillet must commit");
    assert!(NativeKernel::validate(&finished, ValidationProfile::Solid).valid);

    // Independent derivation via Green's theorem over the removed region,
    // bounded by: line T1 → corner, big arc corner → T2, fillet arc T2 → T1.
    let center_x = ((radius - fillet) * (radius - fillet) - fillet * fillet).sqrt();
    let center = (center_x, fillet);
    let tangency_line = (center.0, 0.0);
    let scale = radius / (radius - fillet);
    let tangency_arc = (center.0 * scale, center.1 * scale);
    let corner = (radius, 0.0);

    let line_term = |from: (f64, f64), to: (f64, f64)| 0.5 * (from.0 * to.1 - from.1 * to.0);
    let arc_term = |c: (f64, f64), from: (f64, f64), to: (f64, f64), r: f64| {
        let start = (from.1 - c.1).atan2(from.0 - c.0);
        let end = (to.1 - c.1).atan2(to.0 - c.0);
        let mut sweep = end - start;
        while sweep > std::f64::consts::PI {
            sweep -= std::f64::consts::TAU;
        }
        while sweep < -std::f64::consts::PI {
            sweep += std::f64::consts::TAU;
        }
        0.5 * (c.0 * (to.1 - from.1) - c.1 * (to.0 - from.0) + r * r * sweep)
    };
    let removed = line_term(tangency_line, corner)
        + arc_term((0.0, 0.0), corner, tangency_arc, radius)
        + arc_term(center, tangency_arc, tangency_line, fillet);

    let source_area = std::f64::consts::FRAC_PI_4 * radius * radius;
    assert_close(
        finished.measures().volume,
        (source_area - removed.abs()) * HEIGHT,
        "pie-slice fillet volume",
    );
}

#[test]
fn oversized_and_tangent_requests_reject_transactionally() {
    let base = extrude(l_profile(), "prism-reject-base");
    // Two fillets whose trims overlap on the short C-length side.
    let crowded = finish(
        &base,
        vec![
            vertical_edge_at(&base, B, 0.0),
            vertical_edge_at(&base, B, C),
        ],
        EdgeFinishKind::Fillet,
        C * 0.75,
        "prism-crowded",
    );
    assert!(
        crowded.is_err(),
        "two fillets that overrun their shared side must reject"
    );

    // A radius larger than the whole profile cannot resolve.
    let oversized = finish(
        &base,
        vec![vertical_edge_at(&base, B, 0.0)],
        EdgeFinishKind::Fillet,
        50.0,
        "prism-oversized",
    );
    assert!(oversized.is_err(), "an oversized fillet must reject");

    // The committed snapshot is untouched by either rejection.
    assert_close(
        base.measures().volume,
        profile_area() * HEIGHT,
        "source volume after rejections",
    );
}

/// A rectangle with a rectangular hole, on a frame turned off every world
/// axis. The whole linear-profile path — extrusion, then a through cut for the
/// hole on the extrusion's own cap — reads coordinates in the sketch's frame,
/// so the result must match the aligned one exactly in volume and counts.
#[test]
fn a_holed_linear_profile_extrudes_on_a_turned_frame() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2::from_polygon(&[
                Point2::new(0.0, 0.0),
                Point2::new(10.0, 0.0),
                Point2::new(10.0, 6.0),
                Point2::new(0.0, 6.0),
            ]),
            holes: vec![PlanarLoop2::from_polygon(&[
                Point2::new(3.0, 2.0),
                Point2::new(7.0, 2.0),
                Point2::new(7.0, 4.0),
                Point2::new(3.0, 4.0),
            ])],
        }],
    };

    // An orthonormal frame that shares no axis with the world.
    let root = 1.0 / 3.0_f64.sqrt();
    let normal = Vector3::new(root, root, root);
    let u = {
        let raw = Vector3::new(1.0, -1.0, 0.0);
        let length = (raw.x * raw.x + raw.y * raw.y + raw.z * raw.z).sqrt();
        Vector3::new(raw.x / length, raw.y / length, raw.z / length)
    };
    let v = Vector3::new(
        normal.y * u.z - normal.z * u.y,
        normal.z * u.x - normal.x * u.z,
        normal.x * u.y - normal.y * u.x,
    );

    let turned = NativeKernel::execute(
        &NativeKernel::empty(),
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("turned-holed-profile"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudePlanarProfile {
                frame: PlanarFrame3::new(Point3::new(2.0, -3.0, 5.0), u, v),
                profile: profile.clone(),
                distance: HEIGHT,
            },
        },
        &CancellationToken::new(),
    )
    .expect("a turned frame must extrude a holed profile")
    .snapshot;
    assert!(NativeKernel::validate(&turned, ValidationProfile::Solid).valid);

    let aligned = NativeKernel::execute(
        &NativeKernel::empty(),
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("aligned-holed-profile"),
            expected_snapshot: NativeKernel::empty().id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::ExtrudePlanarProfile {
                frame: PlanarFrame3::new(
                    Point3::new(0.0, 0.0, 0.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                profile,
                distance: HEIGHT,
            },
        },
        &CancellationToken::new(),
    )
    .expect("the aligned frame extrudes the same profile")
    .snapshot;

    assert_eq!(turned.counts(), aligned.counts());
    let expected = (10.0f64.mul_add(6.0, -(4.0 * 2.0))) * HEIGHT;
    assert!(
        ((turned.measures().volume - expected) / expected).abs() < 1.0e-9,
        "turned holed profile volume {} should equal {expected}",
        turned.measures().volume
    );
}

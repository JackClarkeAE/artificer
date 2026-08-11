//! Rim-loop chamfers on straight prisms (ADR 0023 frontier, milestone B).
//!
//! Chamfering a whole cap rim shrinks the cap to the mitred inward offset of
//! the profile. For a convex polygon offset inward by `d`, the removed volume
//! is the prismatoid between the profile at `h − d` and the spine at `h`, so
//! `V = A·h − d·(A + A' + √(A·A'))/3 · ...` is avoided here in favour of the
//! direct prismatoid rule, which is exact for a linear cross-section sweep:
//! `V_removed = (d/6)·(A_low + 4·A_mid + A_high)` with `A_mid` the section
//! halfway up. Every expectation below is derived that way, independently of
//! the kernel's own per-face measures.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const HEIGHT: f64 = 12.0;

fn polygon_profile(corners: &[(f64, f64)]) -> PlanarProfile2 {
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

fn extrude(corners: &[(f64, f64)], label: &str) -> Snapshot {
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
            profile: polygon_profile(corners),
            distance: HEIGHT,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("profile should extrude")
        .snapshot
}

fn polygon_area(corners: &[(f64, f64)]) -> f64 {
    let mut total = 0.0;
    for index in 0..corners.len() {
        let (x1, y1) = corners[index];
        let (x2, y2) = corners[(index + 1) % corners.len()];
        total += x1.mul_add(y2, -(y1 * x2));
    }
    total / 2.0
}

/// The mitred inward offset of a convex polygon, computed independently here.
fn inset(corners: &[(f64, f64)], distance: f64) -> Vec<(f64, f64)> {
    let count = corners.len();
    let mut offset_lines = Vec::with_capacity(count);
    for index in 0..count {
        let (x1, y1) = corners[index];
        let (x2, y2) = corners[(index + 1) % count];
        let length = (x2 - x1).hypot(y2 - y1);
        let direction = ((x2 - x1) / length, (y2 - y1) / length);
        // Inward is the left normal on a counter-clockwise loop.
        let normal = (-direction.1, direction.0);
        offset_lines.push((
            (x1 + normal.0 * distance, y1 + normal.1 * distance),
            direction,
        ));
    }
    (0..count)
        .map(|index| {
            let previous = (index + count - 1) % count;
            let (point_a, dir_a) = offset_lines[previous];
            let (point_b, dir_b) = offset_lines[index];
            let determinant = dir_a.0.mul_add(dir_b.1, -(dir_a.1 * dir_b.0));
            let delta = (point_b.0 - point_a.0, point_b.1 - point_a.1);
            let travel = delta.0.mul_add(dir_b.1, -(delta.1 * dir_b.0)) / determinant;
            (point_a.0 + dir_a.0 * travel, point_a.1 + dir_a.1 * travel)
        })
        .collect()
}

/// The chamfered prism's volume, from the prismatoid rule over the swept band
/// plus the untouched lower prism.
fn expected_volume(corners: &[(f64, f64)], distance: f64) -> f64 {
    let low = polygon_area(corners);
    let high = polygon_area(&inset(corners, distance));
    let middle = polygon_area(&inset(corners, distance / 2.0));
    let band = distance / 6.0 * 4.0f64.mul_add(middle, low + high);
    low * (HEIGHT - distance) + band
}

fn rim_loop(snapshot: &Snapshot) -> Vec<EntityRef> {
    rim_loop_at(snapshot, HEIGHT)
}

/// Every horizontal edge lying at one cap height.
fn rim_loop_at(snapshot: &Snapshot, height: f64) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut seen = Vec::new();
    for edge in &scene.edges {
        let [first, second] = edge.endpoints;
        if (first.z - height).abs() < 1.0e-9
            && (second.z - height).abs() < 1.0e-9
            && !seen.contains(&edge.source_edge)
        {
            seen.push(edge.source_edge);
        }
    }
    seen
}

fn finish(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
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
            kind: EdgeFinishKind::Chamfer,
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
fn a_box_top_rim_chamfers_to_its_mitred_offset() {
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let base = extrude(&corners, "rim-box-base");
    let distance = 1.5_f64;
    let chamfered = finish(&base, rim_loop(&base), distance, "rim-box-chamfer")
        .expect("a complete top rim must chamfer");

    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);
    assert_close(
        chamfered.measures().volume,
        expected_volume(&corners, distance),
        "box rim chamfer volume",
    );
    // Bottom cap, four walls, four slants, top cap.
    assert_eq!(chamfered.counts().faces, 10);
}

#[test]
fn a_hexagon_rim_chamfers_with_six_slants() {
    let corners: Vec<(f64, f64)> = (0..6)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / 6.0;
            (5.0 * angle.cos(), 5.0 * angle.sin())
        })
        .collect();
    let base = extrude(&corners, "rim-hex-base");
    let distance = 0.8_f64;
    let chamfered = finish(&base, rim_loop(&base), distance, "rim-hex-chamfer")
        .expect("a hexagonal rim must chamfer");

    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);
    assert_close(
        chamfered.measures().volume,
        expected_volume(&corners, distance),
        "hexagon rim chamfer volume",
    );
    assert_eq!(chamfered.counts().faces, 14);
}

#[test]
fn a_reflex_profile_corner_rejects_the_rim_loop() {
    // L-profile: the vertex at (6,4) is concave.
    let corners = [
        (0.0, 0.0),
        (10.0, 0.0),
        (10.0, 4.0),
        (6.0, 4.0),
        (6.0, 9.0),
        (0.0, 9.0),
    ];
    let base = extrude(&corners, "rim-reflex-base");
    let refused = finish(&base, rim_loop(&base), 1.0, "rim-reflex");
    assert!(
        refused.is_err(),
        "a concave profile corner must reject a rim-loop finish"
    );
}

#[test]
fn an_oversized_rim_chamfer_rejects_transactionally() {
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let base = extrude(&corners, "rim-oversize-base");
    // Half the short side leaves no cap behind.
    let collapsed = finish(&base, rim_loop(&base), 3.0, "rim-collapse");
    assert!(collapsed.is_err(), "a collapsing offset must reject");
    // Taller than the prism.
    let too_tall = finish(&base, rim_loop(&base), HEIGHT, "rim-too-tall");
    assert!(too_tall.is_err(), "a chamfer past the wall must reject");

    assert_close(
        base.measures().volume,
        polygon_area(&corners) * HEIGHT,
        "source volume after rejections",
    );
}

#[test]
fn a_partial_rim_selection_does_not_enter_the_rim_loop_path() {
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let base = extrude(&corners, "rim-partial-base");
    let mut partial = rim_loop(&base);
    partial.truncate(2);
    // Two of four rim edges is not a loop; the request must not silently
    // chamfer the whole rim.
    if let Ok(finished) = finish(&base, partial, 1.0, "rim-partial") {
        let complete = finish(&base, rim_loop(&base), 1.0, "rim-complete")
            .expect("the complete rim must chamfer");
        assert!(
            (finished.measures().volume - complete.measures().volume).abs() > 1.0e-9,
            "a partial selection must not produce the complete rim result"
        );
    }
}

#[test]
fn rim_loop_group_expands_a_seed_to_the_whole_cap_boundary() {
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let base = extrude(&corners, "rim-group-base");
    let seed = rim_loop(&base)[0];
    let group = NativeKernel::rim_loop_group(&base, seed).expect("a rim seed resolves");
    assert_eq!(group.len(), 4, "a box cap rim is four edges");
    assert!(group.contains(&seed));

    // A vertical generator is not on a cap loop, so it falls back to its own
    // carrier group.
    let scene = NativeKernel::debug_scene(&base);
    let generator = scene
        .edges
        .iter()
        .find(|edge| {
            let [first, second] = edge.endpoints;
            (first.z - second.z).abs() > 1.0e-9
        })
        .expect("a prism has vertical generators")
        .source_edge;
    let alone = NativeKernel::rim_loop_group(&base, generator).expect("a generator resolves");
    assert_eq!(alone, vec![generator]);
}

/// Volume of a box with its whole top rim filleted, derived two ways.
///
/// Setback decomposition: from the full box remove, along each of the four
/// top edges, the quarter-round prism over its trimmed length, then at each of
/// the four corners remove the corner cube minus the sphere octant the ball
/// leaves behind.
fn box_fillet_volume(width: f64, depth: f64, height: f64, fillet: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let quarter = 1.0 - pi / 4.0;
    // Each top edge keeps `length - 2f` of straight quarter-round.
    let edges = 2.0 * ((width - 2.0 * fillet) + (depth - 2.0 * fillet)) * quarter * fillet * fillet;
    // Each corner: the f-cube minus the octant of the ball inside it.
    let corners = 4.0 * (fillet.powi(3) - pi * fillet.powi(3) / 6.0);
    width * depth * height - edges - corners
}

/// The same volume by horizontal slicing: below `h - f` the section is the
/// full rectangle; above it the section is the spine dilated by
/// `w(t) = sqrt(f^2 - t^2)`, whose area is a polynomial in `w`.
fn box_fillet_volume_by_slices(width: f64, depth: f64, height: f64, fillet: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let spine_w = width - 2.0 * fillet;
    let spine_d = depth - 2.0 * fillet;
    // Section area at offset t above h-f: spine rectangle dilated by w.
    // A(w) = spine_w*spine_d + 2*w*(spine_w + spine_d) + pi*w^2.
    // Integrate over t in [0, f] with w = sqrt(f^2 - t^2):
    //   ∫ w dt   = pi f^2 / 4
    //   ∫ w^2 dt = 2 f^3 / 3
    let integral_w = pi * fillet * fillet / 4.0;
    let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
    let cap_band =
        spine_w * spine_d * fillet + 2.0 * (spine_w + spine_d) * integral_w + pi * integral_w2;
    width * depth * (height - fillet) + cap_band
}

#[test]
fn a_box_top_rim_fillets_with_sphere_corners() {
    let (width, depth) = (10.0_f64, 6.0_f64);
    let corners = [(0.0, 0.0), (width, 0.0), (width, depth), (0.0, depth)];
    let base = extrude(&corners, "rim-fillet-base");
    let fillet = 1.5_f64;

    // The two derivations must agree before either is trusted.
    let expected = box_fillet_volume(width, depth, HEIGHT, fillet);
    let cross_check = box_fillet_volume_by_slices(width, depth, HEIGHT, fillet);
    assert!(
        ((expected - cross_check) / expected).abs() < 1.0e-12,
        "independent derivations disagree: {expected} vs {cross_check}"
    );

    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &request, &CancellationToken::new())
        .expect("a complete top rim must fillet")
        .snapshot;

    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);
    assert_close(
        filleted.measures().volume,
        expected,
        "box rim fillet volume",
    );

    // Bottom cap, four walls, four bands, four spheres, four ledges, top cap.
    assert_eq!(filleted.counts().faces, 18);

    let pi = std::f64::consts::PI;
    let expected_area = width * depth                                   // bottom
        + (width - 2.0 * fillet) * (depth - 2.0 * fillet)               // shrunk top
        + 2.0 * (width + depth) * (HEIGHT - fillet)                     // walls
        + 4.0 * (1.0 - pi / 4.0) * fillet * fillet                      // ledges
        + (pi / 2.0)
            * fillet
            * (2.0 * ((width - 2.0 * fillet) + (depth - 2.0 * fillet))) // bands
        + 4.0 * (pi / 2.0) * fillet * fillet; // sphere octants
    assert_close(
        filleted.measures().surface_area,
        expected_area,
        "box rim fillet area",
    );
}

#[test]
fn a_hexagon_rim_fillets_with_obtuse_sphere_corners() {
    // A regular hexagon turns 60 degrees at each corner, so every sphere
    // patch sweeps 60 degrees of azimuth rather than the box's 90.
    let radius = 5.0_f64;
    let corners: Vec<(f64, f64)> = (0..6)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / 6.0;
            (radius * angle.cos(), radius * angle.sin())
        })
        .collect();
    let base = extrude(&corners, "rim-hex-fillet-base");
    let fillet = 0.6_f64;

    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-hex-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &request, &CancellationToken::new())
        .expect("a hexagonal rim must fillet")
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);
    // Bottom, six walls, six bands, six spheres, six ledges, top.
    assert_eq!(filleted.counts().faces, 26);

    // Slicing derivation: above h-f the section is the spine dilated by
    // w(t) = sqrt(f^2 - t^2). For a convex polygon of area A', perimeter P',
    // and exterior angle total 2pi, the dilated area is A' + P'w + pi w^2.
    let pi = std::f64::consts::PI;
    let spine = inset(&corners, fillet);
    let spine_area = polygon_area(&spine);
    let spine_perimeter: f64 = (0..spine.len())
        .map(|index| {
            let (x1, y1) = spine[index];
            let (x2, y2) = spine[(index + 1) % spine.len()];
            (x2 - x1).hypot(y2 - y1)
        })
        .sum();
    let integral_w = pi * fillet * fillet / 4.0;
    let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
    let expected = polygon_area(&corners) * (HEIGHT - fillet)
        + spine_area * fillet
        + spine_perimeter * integral_w
        + pi * integral_w2;
    assert_close(
        filleted.measures().volume,
        expected,
        "hexagon rim fillet volume",
    );
}

#[test]
fn an_oversized_rim_fillet_rejects_transactionally() {
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)];
    let base = extrude(&corners, "rim-fillet-reject-base");
    for bad in [3.0, HEIGHT] {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("rim-fillet-reject"),
            expected_snapshot: base.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::FinishEdges {
                target_edges: rim_loop(&base),
                kind: EdgeFinishKind::Fillet,
                distance: bad,
            },
        };
        assert!(
            NativeKernel::execute(&base, &request, &CancellationToken::new()).is_err(),
            "a rim fillet of {bad} must reject"
        );
    }
}

/// A stadium: two straight runs joined by tangent semicircles of radius `r`,
/// with the straights of length `run`. Every junction is tangent, so a rim
/// fillet needs no sphere patch and no ledge.
fn stadium_profile(run: f64, radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(run, 0.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(run, radius),
                        start: Point2::new(run, 0.0),
                        end: Point2::new(run, 2.0 * radius),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(run, 2.0 * radius),
                        end: Point2::new(0.0, 2.0 * radius),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, radius),
                        start: Point2::new(0.0, 2.0 * radius),
                        end: Point2::new(0.0, 0.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                ],
            },
            holes: vec![],
        }],
    }
}

#[test]
fn a_stadium_rim_fillets_with_torus_bands_and_no_corners() {
    let (run, radius) = (8.0_f64, 3.0_f64);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("stadium-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: stadium_profile(run, radius),
            distance: HEIGHT,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a stadium should extrude")
        .snapshot;

    let fillet = 0.75_f64;
    let finish_request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("stadium-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &finish_request, &CancellationToken::new())
        .expect("a tangent stadium rim must fillet")
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);

    // Bottom, four walls, four bands (two cylinders, two tori), top. Every
    // junction is tangent, so there is no sphere patch and no ledge.
    assert_eq!(filleted.counts().faces, 10);

    // Slicing derivation. Above h-f the section is the spine dilated by
    // w(t) = sqrt(f^2 - t^2); for a convex region of area A' and perimeter P'
    // the dilated area is A' + P'w + pi w^2.
    let pi = std::f64::consts::PI;
    let source_area = 2.0 * radius * run + pi * radius * radius;
    let spine_radius = radius - fillet;
    let spine_area = 2.0 * spine_radius * run + pi * spine_radius * spine_radius;
    let spine_perimeter = 2.0 * run + 2.0 * pi * spine_radius;
    let integral_w = pi * fillet * fillet / 4.0;
    let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
    let expected = source_area * (HEIGHT - fillet)
        + spine_area * fillet
        + spine_perimeter * integral_w
        + pi * integral_w2;
    assert_close(
        filleted.measures().volume,
        expected,
        "stadium rim fillet volume",
    );

    // Area: both caps, the walls, the two cylinder bands over the straights,
    // and the two torus half-bands over the ends.
    let wall = (2.0 * run + 2.0 * pi * radius) * (HEIGHT - fillet);
    let straight_bands = 2.0 * run * (pi / 2.0) * fillet;
    // A quarter torus of major R', minor f swept through pi:
    //   A = f * pi * (R' * pi/2 + f)
    let torus_bands = 2.0 * fillet * pi * (spine_radius * pi / 2.0 + fillet);
    let expected_area = source_area + spine_area + wall + straight_bands + torus_bands;
    assert_close(
        filleted.measures().surface_area,
        expected_area,
        "stadium rim fillet area",
    );
}

#[test]
fn a_stadium_rim_chamfers_with_cone_bands() {
    let (run, radius) = (8.0_f64, 3.0_f64);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("stadium-chamfer-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: stadium_profile(run, radius),
            distance: HEIGHT,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a stadium should extrude")
        .snapshot;

    let distance = 0.75_f64;
    let chamfered = finish(&base, rim_loop(&base), distance, "stadium-chamfer")
        .expect("a tangent stadium rim must chamfer");
    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);

    // Bottom, four walls, four slants (two planes, two cones), top. Every
    // junction is tangent, so adjacent slants share their mitre generator and
    // no corner face appears.
    assert_eq!(chamfered.counts().faces, 10);

    // Independent derivation: the section at height h - d + t is the stadium
    // inset by t, whose end radius is r - t and whose run is unchanged.
    let pi = std::f64::consts::PI;
    let area_at = |inset: f64| {
        let end = radius - inset;
        2.0f64.mul_add(end * run, pi * end * end)
    };
    let band = 2.0 * run * distance.mul_add(radius, -(distance * distance / 2.0))
        + pi * (radius.powi(3) - (radius - distance).powi(3)) / 3.0;
    let expected = area_at(0.0).mul_add(HEIGHT - distance, band);
    assert_close(
        chamfered.measures().volume,
        expected,
        "stadium rim chamfer volume",
    );

    // Area: both caps, the untouched walls, the two planar slants of width
    // sqrt(2)*d, and the two half-cone frusta, whose lateral area over a
    // sweep of pi is sqrt(2)*d*pi*(2r - d)/2 each.
    let root_two = std::f64::consts::SQRT_2;
    let wall = 2.0f64.mul_add(run, 2.0 * pi * radius) * (HEIGHT - distance);
    let planar_slants = 2.0 * run * root_two * distance;
    let cone_bands = root_two * pi * distance * 2.0f64.mul_add(radius, -distance);
    let expected_area = area_at(0.0) + area_at(distance) + wall + planar_slants + cone_bands;
    assert_close(
        chamfered.measures().surface_area,
        expected_area,
        "stadium rim chamfer area",
    );
}

#[test]
fn a_box_bottom_rim_fillets_as_the_mirror_of_its_top_rim() {
    let (width, depth) = (10.0_f64, 6.0_f64);
    let corners = [(0.0, 0.0), (width, 0.0), (width, depth), (0.0, depth)];
    let base = extrude(&corners, "bottom-rim-fillet-base");
    let fillet = 1.5_f64;

    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("bottom-rim-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop_at(&base, 0.0),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &request, &CancellationToken::new())
        .expect("a complete bottom rim must fillet")
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);

    // A box is symmetric about its mid-height, so the bottom rim must remove
    // exactly what the top rim removes.
    let expected = box_fillet_volume(width, depth, HEIGHT, fillet);
    assert_close(
        filleted.measures().volume,
        expected,
        "box bottom rim fillet volume",
    );
    assert_eq!(filleted.counts().faces, 18);

    // The blend must sit at the bottom: the lowest geometry is the shrunk cap.
    let bounds = filleted.measures().bounds.expect("a solid has bounds");
    assert!((bounds.min.z - 0.0).abs() < 1.0e-9);
    let shrunk = rim_loop_at(&filleted, 0.0);
    assert_eq!(shrunk.len(), 4, "the bottom cap keeps four mitred edges");
}

#[test]
fn a_hexagon_bottom_rim_chamfers_to_its_mitred_offset() {
    let corners: Vec<(f64, f64)> = (0..6)
        .map(|index| {
            let angle = std::f64::consts::TAU * f64::from(index) / 6.0;
            (5.0 * angle.cos(), 5.0 * angle.sin())
        })
        .collect();
    let base = extrude(&corners, "bottom-hex-chamfer-base");
    let distance = 0.8_f64;
    let chamfered = finish(
        &base,
        rim_loop_at(&base, 0.0),
        distance,
        "bottom-hex-chamfer",
    )
    .expect("a complete bottom rim must chamfer");
    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);
    assert_close(
        chamfered.measures().volume,
        expected_volume(&corners, distance),
        "hexagon bottom rim chamfer volume",
    );
    // Bottom cap, six walls, six slants, top cap.
    assert_eq!(chamfered.counts().faces, 14);
}

/// A half-disc: a straight diameter and a semicircular arc, meeting sharply at
/// both ends. Each junction is a right angle between a line and an arc, which
/// is the case a purely straight setback cannot describe.
fn half_disc_profile(radius: f64) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(-radius, 0.0),
                        end: Point2::new(radius, 0.0),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(0.0, 0.0),
                        start: Point2::new(radius, 0.0),
                        end: Point2::new(-radius, 0.0),
                        direction: ArcDirection::CounterClockwise,
                    },
                ],
            },
            holes: vec![],
        }],
    }
}

#[test]
fn a_half_disc_rim_fillets_across_sharp_line_arc_junctions() {
    let radius = 6.0_f64;
    let fillet = 1.0_f64;
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("half-disc-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: half_disc_profile(radius),
            distance: HEIGHT,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a half disc should extrude")
        .snapshot;

    let finish_request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("half-disc-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &finish_request, &CancellationToken::new())
        .expect("a sharp line/arc rim must fillet")
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);

    // Bottom cap, two walls, two bands, two sphere patches, two ledges, top.
    assert_eq!(filleted.counts().faces, 10);

    let pi = std::f64::consts::PI;
    // The spine: the diameter pushed in by f, the arc shrunk to r - f, mitred
    // where the two offsets cross.
    let spine_radius = radius - fillet;
    let half_run = (spine_radius * spine_radius - fillet * fillet).sqrt();
    let corner_angle = (fillet / spine_radius).asin();
    let arc_sweep = 2.0f64.mul_add(-corner_angle, pi);
    // Signed-area integral, straight terms plus the arc's r^2*dtheta/2.
    let spine_area = (spine_radius * spine_radius * arc_sweep / 2.0) - half_run * fillet;
    let spine_perimeter = 2.0f64.mul_add(half_run, spine_radius * arc_sweep);
    let source_area = pi * radius * radius / 2.0;
    let source_perimeter = 2.0f64.mul_add(radius, pi * radius);

    // Above h - f the section is the spine dilated by w(t) = sqrt(f^2 - t^2);
    // for a convex spine that area is A' + P'w + pi w^2.
    let integral_w = pi * fillet * fillet / 4.0;
    let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
    let expected = source_area.mul_add(
        HEIGHT - fillet,
        spine_perimeter.mul_add(integral_w, spine_area * fillet) + pi * integral_w2,
    );
    assert_close(
        filleted.measures().volume,
        expected,
        "half disc rim fillet volume",
    );

    // Area, piece by piece. The sphere patch spans the whole normal turn at
    // the junction, which on the arc side runs to the trimmed azimuth.
    let turn = pi / 2.0 + corner_angle;
    let sphere = 2.0 * fillet * fillet * turn;
    let straight_band = 2.0 * half_run * (pi / 2.0) * fillet;
    let torus_band = fillet * arc_sweep * spine_radius.mul_add(pi / 2.0, fillet);
    // Each ledge is bounded by the straight setback, the arc setback, and the
    // equator; its signed-area integral picks up one term per piece.
    let ledge = {
        let arc_term = radius * radius * corner_angle / 2.0;
        let centre_x = half_run;
        let centre_y = fillet;
        let equator = fillet.mul_add(
            fillet * (-pi / 2.0 - corner_angle),
            fillet * centre_x.mul_add(-(1.0 + corner_angle.sin()), centre_y * corner_angle.cos()),
        ) / 2.0;
        2.0 * (arc_term + equator)
    };
    let expected_area = source_area
        + spine_area
        + source_perimeter * (HEIGHT - fillet)
        + straight_band
        + torus_band
        + sphere
        + ledge;
    assert_close(
        filleted.measures().surface_area,
        expected_area,
        "half disc rim fillet area",
    );
}

/// Signed area of a closed (r, z) chain of straight and circular pieces,
/// evaluated as `1/2 * contour integral of (x dy - y dx)`.
fn signed_area(pieces: &[Piece]) -> f64 {
    let mut total = 0.0;
    for piece in pieces {
        match *piece {
            Piece::Line { start, end } => {
                total += start.0.mul_add(end.1, -(start.1 * end.0)) / 2.0;
            }
            Piece::Arc {
                centre,
                radius,
                from,
                to,
            } => {
                total += radius.mul_add(
                    radius * (to - from),
                    radius
                        * centre
                            .0
                            .mul_add(to.sin() - from.sin(), -(centre.1 * (to.cos() - from.cos()))),
                ) / 2.0;
            }
        }
    }
    total
}

fn perimeter(pieces: &[Piece]) -> f64 {
    pieces
        .iter()
        .map(|piece| match *piece {
            Piece::Line { start, end } => (end.0 - start.0).hypot(end.1 - start.1),
            Piece::Arc {
                radius, from, to, ..
            } => radius * (to - from).abs(),
        })
        .sum()
}

#[derive(Clone, Copy)]
enum Piece {
    Line {
        start: (f64, f64),
        end: (f64, f64),
    },
    Arc {
        centre: (f64, f64),
        radius: f64,
        from: f64,
        to: f64,
    },
}

#[test]
fn a_concave_arc_rim_fillets_with_a_grown_torus_band() {
    // A rectangle whose top edge dips inward along a shallow concave arc.
    let (width, chord_height) = (12.0_f64, 8.0_f64);
    let arc_radius = 20.0_f64;
    let centre_y = chord_height + (arc_radius * arc_radius - width * width / 4.0).sqrt();
    let centre = (width / 2.0, centre_y);
    let fillet = 1.0_f64;

    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(width, 0.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(width, 0.0),
                        end: Point2::new(width, chord_height),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(centre.0, centre.1),
                        start: Point2::new(width, chord_height),
                        end: Point2::new(0.0, chord_height),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, chord_height),
                        end: Point2::new(0.0, 0.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("concave-base"),
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
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a concave-topped profile should extrude")
        .snapshot;

    let finish_request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("concave-fillet"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &finish_request, &CancellationToken::new())
        .expect("a concave arc rim must fillet")
        .snapshot;
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);

    // Bottom, four walls, four bands, four spheres, four ledges, top.
    assert_eq!(filleted.counts().faces, 18);

    let azimuth =
        |point: (f64, f64), centre: (f64, f64)| (point.1 - centre.1).atan2(point.0 - centre.0);
    let source = [
        Piece::Line {
            start: (0.0, 0.0),
            end: (width, 0.0),
        },
        Piece::Line {
            start: (width, 0.0),
            end: (width, chord_height),
        },
        Piece::Arc {
            centre,
            radius: arc_radius,
            from: azimuth((width, chord_height), centre),
            to: azimuth((0.0, chord_height), centre),
        },
        Piece::Line {
            start: (0.0, chord_height),
            end: (0.0, 0.0),
        },
    ];

    // The spine: each line pushed in by f, the concave arc grown to r + f, and
    // the four corners mitred where the offsets cross.
    let spine_radius = arc_radius + fillet;
    let corner_y = centre.1 - (spine_radius * spine_radius - (width / 2.0 - fillet).powi(2)).sqrt();
    let spine = [
        Piece::Line {
            start: (fillet, fillet),
            end: (width - fillet, fillet),
        },
        Piece::Line {
            start: (width - fillet, fillet),
            end: (width - fillet, corner_y),
        },
        Piece::Arc {
            centre,
            radius: spine_radius,
            from: azimuth((width - fillet, corner_y), centre),
            to: azimuth((fillet, corner_y), centre),
        },
        Piece::Line {
            start: (fillet, corner_y),
            end: (fillet, fillet),
        },
    ];

    let source_area = signed_area(&source);
    let spine_area = signed_area(&spine);
    let spine_perimeter = perimeter(&spine);
    // The spine has positive reach f (its tightest inward curvature is the
    // grown arc, of radius r + f), so the parallel body obeys the Steiner
    // formula and the section at t above h - f is A' + P'w + pi w^2 with
    // w = sqrt(f^2 - t^2).
    let pi = std::f64::consts::PI;
    let integral_w = pi * fillet * fillet / 4.0;
    let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
    let expected = source_area.mul_add(
        HEIGHT - fillet,
        spine_perimeter.mul_add(integral_w, spine_area * fillet) + pi * integral_w2,
    );
    assert_close(
        filleted.measures().volume,
        expected,
        "concave arc rim fillet volume",
    );
}

#[test]
fn a_sharp_line_arc_junction_rejects_a_rim_chamfer() {
    // Two slants meeting sharply across a line and an arc would intersect in
    // a plane/cone conic, which is outside the curve vocabulary, so the
    // request must be refused rather than approximated.
    let (width, chord_height) = (12.0_f64, 8.0_f64);
    let arc_radius = 20.0_f64;
    let centre_y = chord_height + (arc_radius * arc_radius - width * width / 4.0).sqrt();
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(width, 0.0),
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(width, 0.0),
                        end: Point2::new(width, chord_height),
                    },
                    PlanarCurve2::CircularArc {
                        center: Point2::new(width / 2.0, centre_y),
                        start: Point2::new(width, chord_height),
                        end: Point2::new(0.0, chord_height),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::Line {
                        start: Point2::new(0.0, chord_height),
                        end: Point2::new(0.0, 0.0),
                    },
                ],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("sharp-arc-chamfer-base"),
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
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a concave-topped profile should extrude")
        .snapshot;
    let error = finish(&base, rim_loop(&base), 1.0, "sharp-arc-chamfer")
        .expect_err("a sharp line/arc chamfer corner must be refused");
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "EDGE_FINISH_BLEND_UNSUPPORTED"),
        "unexpected refusal: {error:?}"
    );
    // The refusal is transactional: the base survives untouched.
    assert_eq!(base.counts().faces, 6);
}

/// A peanut: two convex lobes joined by concave blend arcs, every junction
/// tangent. Offsetting it inward keeps the blend centres fixed, so the whole
/// family of sections has a closed form.
fn peanut_profile(lobe: f64, radius: f64, blend: f64, inset: f64) -> ([Piece; 4], PlanarProfile2) {
    let span = radius + blend;
    let height = (span * span - lobe * lobe).sqrt();
    let inner = radius - inset;
    let outer = blend + inset;
    // The tangency point on the upper-right, on the line joining the blend
    // centre to the right lobe centre.
    let tip = (outer * lobe / span, height - outer * height / span);
    let azimuth =
        |point: (f64, f64), centre: (f64, f64)| (point.1 - centre.1).atan2(point.0 - centre.0);
    let top = (0.0, height);
    let bottom = (0.0, -height);
    let right = (lobe, 0.0);
    let left = (-lobe, 0.0);
    let upper_right = tip;
    let upper_left = (-tip.0, tip.1);
    let lower_left = (-tip.0, -tip.1);
    let lower_right = (tip.0, -tip.1);

    // The lobes are traversed counter-clockwise the long way round, so each
    // end azimuth is lifted past its start rather than assumed to exceed it.
    let counter_clockwise = |from: f64, to: f64| {
        let mut to = to;
        while to <= from {
            to += std::f64::consts::TAU;
        }
        to
    };
    let left_start = azimuth(upper_left, left);
    let right_start = azimuth(lower_right, right);
    let pieces = [
        Piece::Arc {
            centre: top,
            radius: outer,
            from: azimuth(upper_right, top),
            to: azimuth(upper_left, top),
        },
        Piece::Arc {
            centre: left,
            radius: inner,
            from: left_start,
            to: counter_clockwise(left_start, azimuth(lower_left, left)),
        },
        Piece::Arc {
            centre: bottom,
            radius: outer,
            from: azimuth(lower_left, bottom),
            to: azimuth(lower_right, bottom),
        },
        Piece::Arc {
            centre: right,
            radius: inner,
            from: right_start,
            to: counter_clockwise(right_start, azimuth(upper_right, right)),
        },
    ];
    let point = |value: (f64, f64)| Point2::new(value.0, value.1);
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![
                    PlanarCurve2::CircularArc {
                        center: point(top),
                        start: point(upper_right),
                        end: point(upper_left),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::CircularArc {
                        center: point(left),
                        start: point(upper_left),
                        end: point(lower_left),
                        direction: ArcDirection::CounterClockwise,
                    },
                    PlanarCurve2::CircularArc {
                        center: point(bottom),
                        start: point(lower_left),
                        end: point(lower_right),
                        direction: ArcDirection::Clockwise,
                    },
                    PlanarCurve2::CircularArc {
                        center: point(right),
                        start: point(lower_right),
                        end: point(upper_right),
                        direction: ArcDirection::CounterClockwise,
                    },
                ],
            },
            holes: vec![],
        }],
    };
    (pieces, profile)
}

#[test]
fn a_tangent_concave_arc_rim_chamfers_with_a_grown_cone_band() {
    let (lobe, radius, blend) = (6.0_f64, 5.0_f64, 2.0_f64);
    let distance = 0.5_f64;
    let (_, profile) = peanut_profile(lobe, radius, blend, 0.0);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("peanut-base"),
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
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a peanut should extrude")
        .snapshot;
    assert_eq!(base.counts().faces, 6, "four walls and two caps");

    let chamfered = finish(&base, rim_loop(&base), distance, "peanut-chamfer")
        .expect("a tangent concave rim must chamfer");
    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);
    // Bottom, four walls, four cone slants, top. Every junction is tangent.
    assert_eq!(chamfered.counts().faces, 10);

    // The section at `t` above the wall top is the profile inset by `t`, whose
    // pieces the helper reproduces exactly.
    let section_at = |inset: f64| signed_area(&peanut_profile(lobe, radius, blend, inset).0);
    let sweep = |steps: usize| {
        let step = distance / steps as f64;
        (0..steps)
            .map(|index| {
                let low = step * index as f64;
                let high = low + step;
                step / 6.0
                    * 4.0f64.mul_add(
                        section_at((low + high) / 2.0),
                        section_at(low) + section_at(high),
                    )
            })
            .sum::<f64>()
    };
    let coarse = sweep(32);
    let fine = sweep(128);
    assert!(
        ((coarse - fine) / fine).abs() < 1.0e-13,
        "the swept-band quadrature has not converged: {coarse} vs {fine}"
    );
    let expected = section_at(0.0).mul_add(HEIGHT - distance, fine);
    assert!(
        ((chamfered.measures().volume - expected) / expected).abs() < 1.0e-11,
        "peanut rim chamfer volume: {} should equal {expected}",
        chamfered.measures().volume
    );
}

/// Asserts a centroid coordinate against an independent expectation, scaled by
/// the part's own size so the tolerance means the same thing on every axis.
fn assert_centre(actual: f64, expected: f64, scale: f64, what: &str) {
    assert!(
        ((actual - expected) / scale).abs() < 1.0e-9,
        "{what}: {actual} should equal {expected}"
    );
}

/// The first moment about `z` of a rim fillet's swept band, from the same
/// dilation the volume uses: at `t` above `h - f` the section is the spine
/// dilated by `w = sqrt(f^2 - t^2)`, sitting at height `h - f + t`.
fn fillet_band_moment(spine_area: f64, spine_perimeter: f64, fillet: f64, wall_top: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let integral_w = pi * fillet * fillet / 4.0;
    let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
    let volume = spine_perimeter.mul_add(integral_w, spine_area * fillet) + pi * integral_w2;
    // ∫ t·w dt = f³/3 and ∫ t·w² dt = f⁴/4 over [0, f].
    let weighted = spine_area.mul_add(
        fillet * fillet / 2.0,
        spine_perimeter.mul_add(fillet.powi(3) / 3.0, pi * fillet.powi(4) / 4.0),
    );
    wall_top.mul_add(volume, weighted)
}

#[test]
fn a_box_rim_fillet_reports_its_exact_centre_of_mass() {
    let (width, depth) = (10.0_f64, 6.0_f64);
    let corners = [(0.0, 0.0), (width, 0.0), (width, depth), (0.0, depth)];
    let base = extrude(&corners, "rim-centroid-base");
    let fillet = 1.5_f64;
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-centroid"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &request, &CancellationToken::new())
        .expect("a complete top rim must fillet")
        .snapshot;
    let centre = filleted
        .measures()
        .centroid
        .expect("a blended solid must report a centre of mass");

    // The part is symmetric about both mid-planes, so only `z` is at issue.
    assert_centre(centre.x, width / 2.0, width, "box rim fillet centre x");
    assert_centre(centre.y, depth / 2.0, depth, "box rim fillet centre y");

    let wall_top = HEIGHT - fillet;
    let spine_area = (width - 2.0 * fillet) * (depth - 2.0 * fillet);
    let spine_perimeter = 2.0 * ((width - 2.0 * fillet) + (depth - 2.0 * fillet));
    let lower = width * depth * wall_top;
    let volume = box_fillet_volume(width, depth, HEIGHT, fillet);
    let moment = lower.mul_add(
        wall_top / 2.0,
        fillet_band_moment(spine_area, spine_perimeter, fillet, wall_top),
    );
    assert_centre(centre.z, moment / volume, HEIGHT, "box rim fillet centre z");
}

#[test]
fn a_stadium_rim_fillet_reports_its_exact_centre_of_mass() {
    let (run, radius) = (8.0_f64, 3.0_f64);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("stadium-centroid-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: stadium_profile(run, radius),
            distance: HEIGHT,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a stadium should extrude")
        .snapshot;
    let fillet = 0.75_f64;
    let finish_request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("stadium-centroid"),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: rim_loop(&base),
            kind: EdgeFinishKind::Fillet,
            distance: fillet,
        },
    };
    let filleted = NativeKernel::execute(&base, &finish_request, &CancellationToken::new())
        .expect("a tangent stadium rim must fillet")
        .snapshot;
    let centre = filleted
        .measures()
        .centroid
        .expect("a torus-banded solid must report a centre of mass");

    // Exercises the torus bands and the cylindrical end walls: the stadium is
    // symmetric about both of its own mid-planes.
    assert_centre(centre.x, run / 2.0, run, "stadium rim fillet centre x");
    assert_centre(centre.y, radius, radius, "stadium rim fillet centre y");

    let pi = std::f64::consts::PI;
    let wall_top = HEIGHT - fillet;
    let source_area = 2.0f64.mul_add(radius * run, pi * radius * radius);
    let spine_radius = radius - fillet;
    let spine_area = 2.0f64.mul_add(spine_radius * run, pi * spine_radius * spine_radius);
    let spine_perimeter = 2.0f64.mul_add(run, 2.0 * pi * spine_radius);
    let lower = source_area * wall_top;
    let volume = lower + {
        let integral_w = pi * fillet * fillet / 4.0;
        let integral_w2 = 2.0 * fillet.powi(3) / 3.0;
        spine_perimeter.mul_add(integral_w, spine_area * fillet) + pi * integral_w2
    };
    let moment = lower.mul_add(
        wall_top / 2.0,
        fillet_band_moment(spine_area, spine_perimeter, fillet, wall_top),
    );
    assert_centre(
        centre.z,
        moment / volume,
        HEIGHT,
        "stadium rim fillet centre z",
    );
}

#[test]
fn a_peanut_rim_chamfer_reports_its_exact_centre_of_mass() {
    let (lobe, radius, blend) = (6.0_f64, 5.0_f64, 2.0_f64);
    let distance = 0.5_f64;
    let (_, profile) = peanut_profile(lobe, radius, blend, 0.0);
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("peanut-centroid-base"),
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
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a peanut should extrude")
        .snapshot;
    let chamfered = finish(&base, rim_loop(&base), distance, "peanut-centroid")
        .expect("a tangent concave rim must chamfer");
    let centre = chamfered
        .measures()
        .centroid
        .expect("a cone-banded solid must report a centre of mass");

    // Exercises the cone bands. The peanut is symmetric about both axes.
    assert_centre(centre.x, 0.0, lobe, "peanut rim chamfer centre x");
    assert_centre(centre.y, 0.0, lobe, "peanut rim chamfer centre y");

    let section_at = |inset: f64| signed_area(&peanut_profile(lobe, radius, blend, inset).0);
    let wall_top = HEIGHT - distance;
    // The section shrinks with the offset, so weight the same composite rule by
    // height and require it to converge before either result is trusted.
    let sweep = |steps: usize, weighted: bool| {
        let step = distance / steps as f64;
        (0..steps)
            .map(|index| {
                let low = step * index as f64;
                let high = low + step;
                let sample = |offset: f64| {
                    let height = if weighted { wall_top + offset } else { 1.0 };
                    height * section_at(offset)
                };
                step / 6.0 * 4.0f64.mul_add(sample((low + high) / 2.0), sample(low) + sample(high))
            })
            .sum::<f64>()
    };
    for weighted in [false, true] {
        let coarse = sweep(32, weighted);
        let fine = sweep(128, weighted);
        assert!(
            ((coarse - fine) / fine).abs() < 1.0e-13,
            "the swept-band quadrature has not converged: {coarse} vs {fine}"
        );
    }
    let lower = section_at(0.0) * wall_top;
    let volume = lower + sweep(128, false);
    let moment = lower.mul_add(wall_top / 2.0, sweep(128, true));
    assert!(
        ((centre.z - moment / volume) / HEIGHT).abs() < 1.0e-11,
        "peanut rim chamfer centre z: {} should equal {}",
        centre.z,
        moment / volume
    );
}

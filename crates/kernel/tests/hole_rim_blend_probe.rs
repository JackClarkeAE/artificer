//! Exact fillets and chamfers around the rim of a hole through a prism.
//!
//! A hole's rim is a cap rim like any other: its loop runs clockwise with the
//! material on its left, so the same mitred inward offset that shrinks an
//! outer cap grows a hole into the material around it. Every expectation
//! below is a closed form derived independently of the kernel: a chamfer
//! around a round hole removes a conical ring, `π·r·d² + π·d³/3`; a fillet
//! removes the ring swept by the corner outside the rolling ball, by Pappus
//! `2π·(r·f²·(1 − π/4) + f³·(5/6 − π/4))`; and a chamfer around a square hole
//! removes `∫₀ᵈ (d − t)·(4·s + 8·t) dt = 2·s·d² + 4·d³/3`.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, DisplaySurface, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIDE: f64 = 100.0;
const HEIGHT: f64 = 20.0;
const HOLE_RADIUS: f64 = 8.0;
const HOLE_SIDE: f64 = 16.0;
const DISTANCE: f64 = 2.0;

fn rectangle(min: (f64, f64), max: (f64, f64)) -> Vec<PlanarCurve2> {
    let corners = [
        Point2::new(min.0, min.1),
        Point2::new(max.0, min.1),
        Point2::new(max.0, max.1),
        Point2::new(min.0, max.1),
    ];
    (0..4)
        .map(|index| PlanarCurve2::Line {
            start: corners[index],
            end: corners[(index + 1) % 4],
        })
        .collect()
}

fn circle(center: (f64, f64), radius: f64) -> Vec<PlanarCurve2> {
    vec![PlanarCurve2::Circle {
        center: Point2::new(center.0, center.1),
        radius,
        direction: ArcDirection::CounterClockwise,
    }]
}

fn polygon(corners: &[(f64, f64)]) -> Vec<PlanarCurve2> {
    (0..corners.len())
        .map(|index| {
            let start = corners[index];
            let end = corners[(index + 1) % corners.len()];
            PlanarCurve2::Line {
                start: Point2::new(start.0, start.1),
                end: Point2::new(end.0, end.1),
            }
        })
        .collect()
}

fn xy_frame() -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

/// A `SIDE × SIDE × HEIGHT` block with the given holes cut by the profile.
fn block_with_holes(holes: Vec<Vec<PlanarCurve2>>, label: &str) -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: rectangle((0.0, 0.0), (SIDE, SIDE)),
            },
            holes: holes
                .into_iter()
                .map(|curves| PlanarLoop2 { curves })
                .collect(),
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: xy_frame(),
            profile,
            distance: HEIGHT,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a holed block should extrude")
        .snapshot
}

/// The rim edges of the hole at `height`: every edge lying at that height
/// whose endpoints sit inside the hole's bounding square.
fn hole_rim(snapshot: &Snapshot, center: (f64, f64), reach: f64, height: f64) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut seen = Vec::new();
    for edge in &scene.edges {
        let [first, second] = edge.endpoints;
        let inside = |point: Point3| {
            (point.z - height).abs() < 1.0e-9
                && (point.x - center.0).abs() <= reach + 1.0e-9
                && (point.y - center.1).abs() <= reach + 1.0e-9
        };
        if inside(first) && inside(second) && !seen.contains(&edge.source_edge) {
            seen.push(edge.source_edge);
        }
    }
    seen
}

fn finish(
    snapshot: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
    label: &str,
) -> Result<artificer_kernel::ExecutionOutcome, artificer_protocol::KernelError> {
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
}

fn assert_exact(outcome: &artificer_kernel::ExecutionOutcome) {
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact blend carries no approximation warning: {:?}",
        outcome.report.warnings
    );
    let validation = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
}

fn assert_volume(snapshot: &Snapshot, expected: f64) {
    let actual = snapshot.measures().volume;
    assert!(
        ((actual - expected) / expected).abs() < 1.0e-9,
        "volume {actual} should be {expected}"
    );
}

fn carrier_counts(snapshot: &Snapshot) -> (usize, usize, usize, usize) {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut counts = (0, 0, 0, 0);
    for carrier in &scene.carriers {
        match carrier.surface {
            DisplaySurface::Cylinder { .. } => counts.0 += 1,
            DisplaySurface::Cone { .. } => counts.1 += 1,
            DisplaySurface::Sphere { .. } => counts.2 += 1,
            DisplaySurface::Torus { .. } => counts.3 += 1,
        }
    }
    counts
}

fn round_hole_chamfer_removed(radius: f64, distance: f64) -> f64 {
    PI * radius * distance * distance + PI * distance.powi(3) / 3.0
}

fn round_hole_fillet_removed(radius: f64, fillet: f64) -> f64 {
    2.0 * PI
        * (radius * fillet * fillet * (1.0 - PI / 4.0) + fillet.powi(3) * (5.0 / 6.0 - PI / 4.0))
}

fn square_hole_chamfer_removed(side: f64, distance: f64) -> f64 {
    2.0 * side * distance * distance + 4.0 * distance.powi(3) / 3.0
}

#[test]
fn a_round_hole_rim_fillets_into_one_exact_torus_band() {
    let center = (50.0, 50.0);
    let base = block_with_holes(vec![circle(center, HOLE_RADIUS)], "round-hole-base");
    let rim = hole_rim(&base, center, HOLE_RADIUS, HEIGHT);
    assert_eq!(rim.len(), 2, "a round hole rim is two semicircle edges");

    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Fillet,
        DISTANCE,
        "round-hole-fillet",
    )
    .expect("a round hole rim fillets exactly");
    assert_exact(&outcome);
    let blended = &outcome.snapshot;
    // Bottom cap, top cap, four walls, two hole walls, two torus bands.
    assert_eq!(blended.counts().faces, 10);
    let (cylinders, cones, spheres, tori) = carrier_counts(blended);
    assert_eq!((cylinders, cones, spheres, tori), (2, 0, 0, 2));
    let solid = SIDE * SIDE * HEIGHT - PI * HOLE_RADIUS * HOLE_RADIUS * HEIGHT;
    assert_volume(
        blended,
        solid - round_hole_fillet_removed(HOLE_RADIUS, DISTANCE),
    );
}

#[test]
fn a_round_hole_rim_chamfers_into_one_exact_cone_band() {
    let center = (50.0, 50.0);
    let base = block_with_holes(vec![circle(center, HOLE_RADIUS)], "round-hole-chamfer-base");
    let rim = hole_rim(&base, center, HOLE_RADIUS, HEIGHT);

    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Chamfer,
        DISTANCE,
        "round-hole-chamfer",
    )
    .expect("a round hole rim chamfers exactly");
    assert_exact(&outcome);
    let blended = &outcome.snapshot;
    assert_eq!(blended.counts().faces, 10);
    let (cylinders, cones, spheres, tori) = carrier_counts(blended);
    assert_eq!((cylinders, cones, spheres, tori), (2, 2, 0, 0));
    let solid = SIDE * SIDE * HEIGHT - PI * HOLE_RADIUS * HOLE_RADIUS * HEIGHT;
    assert_volume(
        blended,
        solid - round_hole_chamfer_removed(HOLE_RADIUS, DISTANCE),
    );
}

#[test]
fn a_square_hole_rim_chamfers_into_four_planar_slants() {
    let center = (50.0, 50.0);
    let half = HOLE_SIDE / 2.0;
    let base = block_with_holes(
        vec![rectangle(
            (center.0 - half, center.1 - half),
            (center.0 + half, center.1 + half),
        )],
        "square-hole-base",
    );
    let rim = hole_rim(&base, center, half, HEIGHT);
    assert_eq!(rim.len(), 4);

    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Chamfer,
        DISTANCE,
        "square-hole-chamfer",
    )
    .expect("a square hole rim chamfers exactly: two planes meet in a line");
    assert_exact(&outcome);
    let blended = &outcome.snapshot;
    // Two caps, four outer walls, four hole walls, four slants: all planar.
    assert_eq!(blended.counts().faces, 14);
    assert_eq!(carrier_counts(blended), (0, 0, 0, 0));
    let solid = SIDE * SIDE * HEIGHT - HOLE_SIDE * HOLE_SIDE * HEIGHT;
    assert_volume(
        blended,
        solid - square_hole_chamfer_removed(HOLE_SIDE, DISTANCE),
    );
}

#[test]
fn a_square_hole_rim_fillet_is_exact_with_elliptical_mitres() {
    let center = (50.0, 50.0);
    let half = HOLE_SIDE / 2.0;
    let base = block_with_holes(
        vec![rectangle(
            (center.0 - half, center.1 - half),
            (center.0 + half, center.1 + half),
        )],
        "square-hole-fillet-base",
    );
    let rim = hole_rim(&base, center, half, HEIGHT);
    // Two cylinders of equal radius meeting at a reflex corner intersect in
    // an ellipse, which the vocabulary now admits: the blend is exact, and
    // each corner removes `f³(5/3 − π/2)` beyond the straight runs.
    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Fillet,
        DISTANCE,
        "square-hole-fillet",
    )
    .expect("the square-hole fillet is exact");
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact blend carries no approximation warning: {:?}",
        outcome.report.warnings
    );
    let straight = 4.0 * HOLE_SIDE * (1.0 - std::f64::consts::PI / 4.0) * DISTANCE * DISTANCE;
    let corners = 4.0 * DISTANCE.powi(3) * (5.0 / 3.0 - std::f64::consts::PI / 2.0);
    let expected = base.measures().volume - straight - corners;
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
}

#[test]
fn an_l_shaped_hole_rim_chamfers_exactly_with_its_reflex_corner_mitred() {
    // An L: a 16 × 16 square with its upper-right 8 × 8 quadrant filled in.
    let corners = [
        (42.0, 42.0),
        (58.0, 42.0),
        (58.0, 50.0),
        (50.0, 50.0),
        (50.0, 58.0),
        (42.0, 58.0),
    ];
    let base = block_with_holes(vec![polygon(&corners)], "l-hole-base");
    let rim = hole_rim(&base, (50.0, 50.0), 8.0, HEIGHT);
    assert_eq!(rim.len(), 6);

    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Chamfer,
        DISTANCE,
        "l-hole-chamfer",
    )
    .expect("an L-shaped hole rim chamfers exactly");
    assert_exact(&outcome);
    let blended = &outcome.snapshot;
    // Two caps, four outer walls, six hole walls, six slants.
    assert_eq!(blended.counts().faces, 18);
    assert_eq!(carrier_counts(blended), (0, 0, 0, 0));
    // Removed volume by the same integral as the square. The mitred outward
    // offset of a rectilinear loop grows its perimeter by `2t` at each of the
    // five convex corners and shrinks it by `2t` at the reflex one, so
    // `P(t) = P + 8t` and `V = ∫₀ᵈ (d − t)·(P + 8t) dt = P·d²/2 + 4·d³/3`.
    let perimeter = 16.0 + 8.0 + 8.0 + 8.0 + 8.0 + 16.0;
    let removed = perimeter * DISTANCE * DISTANCE / 2.0 + 4.0 * DISTANCE.powi(3) / 3.0;
    let l_area = 16.0 * 16.0 - 8.0 * 8.0;
    let solid = SIDE * SIDE * HEIGHT - l_area * HEIGHT;
    assert_volume(blended, solid - removed);
}

#[test]
fn a_hole_cut_into_a_primitive_cuboid_fillets_exactly() {
    let cuboid = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("cuboid"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIDE,
            size_y: SIDE,
            size_z: HEIGHT,
        },
    };
    let cuboid = NativeKernel::execute(&NativeKernel::empty(), &cuboid, &CancellationToken::new())
        .expect("cuboid")
        .snapshot;
    let top = NativeKernel::debug_scene(&cuboid)
        .triangles
        .iter()
        .find(|triangle| {
            triangle
                .vertices
                .iter()
                .all(|point| (point.z - HEIGHT).abs() < 1e-9)
        })
        .map(|triangle| triangle.source_face)
        .expect("top face");
    let center = (50.0, 50.0);
    let cut = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("through-hole"),
        expected_snapshot: cuboid.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: top,
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, HEIGHT),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: circle(center, HOLE_RADIUS),
                    },
                    holes: vec![],
                }],
            },
            distance: HEIGHT,
            operation: FaceExtrusionOperation::Cut,
        },
    };
    let holed = NativeKernel::execute(&cuboid, &cut, &CancellationToken::new())
        .expect("a through hole in a cuboid")
        .snapshot;
    let rim = hole_rim(&holed, center, HOLE_RADIUS, HEIGHT);
    assert_eq!(rim.len(), 2, "{rim:?}");

    let outcome = finish(
        &holed,
        rim,
        EdgeFinishKind::Fillet,
        DISTANCE,
        "cuboid-hole-fillet",
    )
    .expect("a hole cut into a cuboid fillets exactly");
    assert_exact(&outcome);
    let (_, _, _, tori) = carrier_counts(&outcome.snapshot);
    assert_eq!(tori, 2);
    let solid = SIDE * SIDE * HEIGHT - PI * HOLE_RADIUS * HOLE_RADIUS * HEIGHT;
    assert_volume(
        &outcome.snapshot,
        solid - round_hole_fillet_removed(HOLE_RADIUS, DISTANCE),
    );
}

#[test]
fn the_bottom_rim_of_a_hole_chamfers_like_the_top() {
    let center = (50.0, 50.0);
    let base = block_with_holes(vec![circle(center, HOLE_RADIUS)], "bottom-rim-base");
    let rim = hole_rim(&base, center, HOLE_RADIUS, 0.0);
    assert_eq!(rim.len(), 2);

    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Chamfer,
        DISTANCE,
        "bottom-rim-chamfer",
    )
    .expect("the bottom rim of a hole chamfers exactly");
    assert_exact(&outcome);
    let (_, cones, _, _) = carrier_counts(&outcome.snapshot);
    assert_eq!(cones, 2);
    let solid = SIDE * SIDE * HEIGHT - PI * HOLE_RADIUS * HOLE_RADIUS * HEIGHT;
    assert_volume(
        &outcome.snapshot,
        solid - round_hole_chamfer_removed(HOLE_RADIUS, DISTANCE),
    );
}

#[test]
fn a_hole_rim_beside_another_hole_leaves_the_other_hole_alone() {
    let first = (30.0, 50.0);
    let second = (70.0, 50.0);
    let base = block_with_holes(
        vec![circle(first, HOLE_RADIUS), circle(second, HOLE_RADIUS)],
        "two-holes-base",
    );
    let rim = hole_rim(&base, first, HOLE_RADIUS, HEIGHT);
    assert_eq!(rim.len(), 2);

    let outcome = finish(
        &base,
        rim,
        EdgeFinishKind::Fillet,
        DISTANCE,
        "two-holes-fillet",
    )
    .expect("one hole rim fillets while the other passes through");
    assert_exact(&outcome);
    let blended = &outcome.snapshot;
    // Two caps, four walls, two hole walls each, two torus bands.
    assert_eq!(blended.counts().faces, 12);
    let (cylinders, _, _, tori) = carrier_counts(blended);
    assert_eq!((cylinders, tori), (4, 2));
    let solid = SIDE * SIDE * HEIGHT - 2.0 * PI * HOLE_RADIUS * HOLE_RADIUS * HEIGHT;
    assert_volume(
        blended,
        solid - round_hole_fillet_removed(HOLE_RADIUS, DISTANCE),
    );
    // The untouched hole's rim is still one selectable rim loop.
    let other = hole_rim(blended, second, HOLE_RADIUS, HEIGHT);
    assert_eq!(other.len(), 2);
    let group = NativeKernel::rim_loop_group(blended, other[0]).expect("rim loop group");
    assert_eq!(group.len(), 2);
}

#[test]
fn a_hole_rim_edge_expands_to_its_whole_loop() {
    let center = (50.0, 50.0);
    let half = HOLE_SIDE / 2.0;
    let base = block_with_holes(
        vec![rectangle(
            (center.0 - half, center.1 - half),
            (center.0 + half, center.1 + half),
        )],
        "rim-group-base",
    );
    let rim = hole_rim(&base, center, half, HEIGHT);
    let group = NativeKernel::rim_loop_group(&base, rim[0]).expect("rim loop group");
    let mut expected = rim.clone();
    expected.sort();
    let mut actual = group;
    actual.sort();
    assert_eq!(actual, expected);
}

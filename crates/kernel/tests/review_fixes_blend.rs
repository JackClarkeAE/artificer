//! Regressions for the fillet and chamfer rungs found in review.
//!
//! Each fixture is one a rung used to accept and answer wrongly: a body it
//! mistook for one it knows, a wedge it read the wrong way round, a tool that
//! reached further than the finish, or a finish that ran into a loop it never
//! looked at. Every expectation is a closed form computed here, not read off
//! the kernel, and a refusal is the right answer wherever the rung cannot
//! build the shape exactly.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EdgeFinishKind,
    EntityRef, ExecuteRequest, FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3,
    PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId,
    ValidationProfile, Vector3,
};

fn run(input: &Snapshot, command: KernelCommand, label: &str) -> Result<ExecutionOutcome, String> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::new())
        .map_err(|error| format!("{error:?}"))
}

fn build(input: &Snapshot, command: KernelCommand, label: &str) -> Snapshot {
    run(input, command, label)
        .unwrap_or_else(|error| panic!("{label} should build: {error}"))
        .snapshot
}

fn flat_frame(z: f64) -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(0.0, 0.0, z),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    )
}

fn polygon(corners: &[(f64, f64)]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: (0..corners.len())
            .map(|index| {
                let start = corners[index];
                let end = corners[(index + 1) % corners.len()];
                PlanarCurve2::Line {
                    start: Point2::new(start.0, start.1),
                    end: Point2::new(end.0, end.1),
                }
            })
            .collect(),
    }
}

fn circle(center: (f64, f64), radius: f64) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(center.0, center.1),
            radius,
            direction: ArcDirection::CounterClockwise,
        }],
    }
}

/// A prism on the `z = 0` plane: `outer` less every loop of `holes`.
fn prism(outer: PlanarLoop2, holes: Vec<PlanarLoop2>, height: f64, label: &str) -> Snapshot {
    build(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: flat_frame(0.0),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 { outer, holes }],
            },
            distance: height,
        },
        label,
    )
}

fn cuboid(origin: (f64, f64, f64), size: (f64, f64, f64), label: &str) -> Snapshot {
    build(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(origin.0, origin.1, origin.2),
            size_x: size.0,
            size_y: size.1,
            size_z: size.2,
        },
        label,
    )
}

fn subtract(target: &Snapshot, tool: &Snapshot, label: &str) -> Snapshot {
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation: BooleanOperation::Difference,
    };
    NativeKernel::execute_boolean(target, tool, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
        .snapshot
}

fn face_where(snapshot: &Snapshot, pick: impl Fn(Point3) -> bool) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    for triangle in &scene.triangles {
        let [a, b, c] = triangle.vertices;
        let centre = Point3::new(
            (a.x + b.x + c.x) / 3.0,
            (a.y + b.y + c.y) / 3.0,
            (a.z + b.z + c.z) / 3.0,
        );
        if pick(centre) {
            return triangle.source_face;
        }
    }
    panic!("the fixture should expose the requested face");
}

/// A blind round hole of `radius` sunk `depth` into the `z = top` face at
/// `(x, y)`.
fn drill(body: &Snapshot, top: f64, at: (f64, f64), radius: f64, depth: f64) -> Snapshot {
    let face = face_where(body, |centre| (centre.z - top).abs() < 1.0e-6);
    build(
        body,
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: face,
            frame: flat_frame(top),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle(at, radius),
                    holes: vec![],
                }],
            },
            distance: depth,
            operation: FaceExtrusionOperation::Cut,
        },
        "drill",
    )
}

/// The straight edge running along `z` through `(x, y)`.
fn vertical_edge(snapshot: &Snapshot, x: f64, y: f64) -> EntityRef {
    NativeKernel::debug_scene(snapshot)
        .edges
        .iter()
        .find(|edge| {
            let [a, b] = edge.endpoints;
            (a.x - x).abs() < 1.0e-9
                && (b.x - x).abs() < 1.0e-9
                && (a.y - y).abs() < 1.0e-9
                && (b.y - y).abs() < 1.0e-9
                && (a.z - b.z).abs() > 1.0e-6
        })
        .map(|edge| edge.source_edge)
        .unwrap_or_else(|| panic!("the body has a vertical edge through ({x}, {y})"))
}

/// Every edge whose chords all lie at height `z` and pass the filter.
fn edges_at(snapshot: &Snapshot, z: f64, keep: impl Fn(Point3) -> bool) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut found: Vec<EntityRef> = Vec::new();
    for edge in &scene.edges {
        let [a, b] = edge.endpoints;
        if (a.z - z).abs() < 1.0e-9
            && (b.z - z).abs() < 1.0e-9
            && keep(a)
            && keep(b)
            && !found.contains(&edge.source_edge)
        {
            found.push(edge.source_edge);
        }
    }
    assert!(!found.is_empty(), "the fixture has edges at z = {z}");
    found
}

fn finish(
    body: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
    standing_apart: bool,
) -> Result<ExecutionOutcome, String> {
    run(
        body,
        KernelCommand::FinishEdges {
            target_edges: targets,
            kind,
            distance,
            standing_apart,
        },
        "finish",
    )
}

/// The request was refused, and by the check named `code`.
fn assert_refused(result: Result<ExecutionOutcome, String>, code: &str, what: &str) {
    match result {
        Ok(outcome) => panic!("{what} was accepted by {:?}", outcome.report.rung),
        Err(error) => assert!(error.contains(code), "{what} should be {code}: {error}"),
    }
}

fn assert_valid(snapshot: &Snapshot) {
    let validation = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < 1.0e-9,
        "{what}: {actual} should be {expected}"
    );
}

/// What a fillet of radius `r` takes from, or adds to, a right-angled edge
/// of length `length`: the square of side `r` less its quarter disc.
fn fillet_corner(radius: f64, length: f64) -> f64 {
    radius * radius * (1.0 - PI / 4.0) * length
}

// ---------------------------------------------------------------------------
// Finding 1: the cuboid rung took any six-plane body for a box.
// ---------------------------------------------------------------------------

#[test]
fn a_six_faced_prism_that_is_not_a_box_is_filleted_as_itself() {
    // A trapezoid prism has six planar faces and is no box: the cuboid rung
    // used to rebuild it from its bounding box, 10 × 5 × 7, and fillet that.
    let body = prism(
        polygon(&[(0.0, 0.0), (10.0, 0.0), (6.0, 5.0), (0.0, 5.0)]),
        Vec::new(),
        7.0,
        "trapezoid",
    );
    assert_close(body.measures().volume, 280.0, "trapezoid prism");
    let outcome = run(
        &body,
        KernelCommand::FinishEdge {
            target_edge: vertical_edge(&body, 0.0, 0.0),
            kind: EdgeFinishKind::Fillet,
            distance: 1.0,
        },
        "trapezoid fillet",
    )
    .expect("a right-angled edge of a prism fillets");
    assert_ne!(outcome.report.rung.as_deref(), Some("edge-finish/analytic"));
    assert_valid(&outcome.snapshot);
    assert_close(
        outcome.snapshot.measures().volume,
        280.0 - fillet_corner(1.0, 7.0),
        "filleted trapezoid prism",
    );
}

#[test]
fn a_box_still_takes_the_cuboid_rung() {
    let body = cuboid((0.0, 0.0, 0.0), (10.0, 5.0, 7.0), "box");
    let outcome = run(
        &body,
        KernelCommand::FinishEdge {
            target_edge: vertical_edge(&body, 0.0, 0.0),
            kind: EdgeFinishKind::Fillet,
            distance: 1.0,
        },
        "box fillet",
    )
    .expect("a box edge fillets");
    assert_eq!(outcome.report.rung.as_deref(), Some("edge-finish/analytic"));
    assert_close(
        outcome.snapshot.measures().volume,
        350.0 - fillet_corner(1.0, 7.0),
        "filleted box",
    );
}

// ---------------------------------------------------------------------------
// Finding 2: the standing-apart route read a concave edge as a convex one.
// ---------------------------------------------------------------------------

/// A 10 × 9 × 7 block less the 5 × 6 corner above `(6, 4)`, with a blind hole
/// in its top so that no prism rung owns it. The edge through `(6, 4)` is
/// reflex.
fn drilled_l_block() -> Snapshot {
    let block = cuboid((0.0, 0.0, 0.0), (10.0, 9.0, 7.0), "l-block");
    let notch = cuboid((6.0, 4.0, -1.0), (5.0, 6.0, 9.0), "l-notch");
    let l_block = subtract(&block, &notch, "l-shape");
    assert_close(l_block.measures().volume, 490.0, "L block");
    drill(&l_block, 7.0, (3.0, 3.0), 1.0, 3.0)
}

#[test]
fn a_reflex_edge_is_never_finished_by_cutting_material_away() {
    let body = drilled_l_block();
    let before = body.measures().volume;
    let edge = vertical_edge(&body, 6.0, 4.0);
    // A fillet in a reflex corner adds its corner region; it never removes
    // anything. The standing-apart cut only ever removes, so it must not be
    // the rung that answers — it used to, taking 64.5 out of the body where
    // the fillet adds 1.5. How closely a later, approximate rung lands on the
    // added corner is that rung's own affair; that it removes nothing is
    // this one's.
    if let Ok(outcome) = finish(&body, vec![edge], EdgeFinishKind::Fillet, 1.0, false) {
        assert_ne!(
            outcome.report.rung.as_deref(),
            Some("edge-finish/standing-apart")
        );
        let added = outcome.snapshot.measures().volume - before;
        assert!(
            added > -1.0e-6 && added < 2.0 * fillet_corner(1.0, 7.0),
            "a reflex fillet adds at most its corner region, not {added}"
        );
    }
    // Asked for by name, the standing-apart route refuses the edge rather
    // than cutting the wrong wedge.
    for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
        assert_refused(
            finish(&body, vec![edge], kind, 1.0, true),
            "EDGE_FINISH_APART_EDGE_UNSUPPORTED: That edge is concave",
            &format!("a {kind:?} of a reflex edge standing apart"),
        );
    }
}

// ---------------------------------------------------------------------------
// Finding 3: the standing-apart tool reached sideways across the body.
// ---------------------------------------------------------------------------

/// A U: a 30 × 5 base with 5-wide posts at each end up to `y = 20`, 10 tall.
fn u_prism() -> Snapshot {
    prism(
        polygon(&[
            (0.0, 0.0),
            (30.0, 0.0),
            (30.0, 20.0),
            (25.0, 20.0),
            (25.0, 5.0),
            (5.0, 5.0),
            (5.0, 20.0),
            (0.0, 20.0),
        ]),
        Vec::new(),
        10.0,
        "u-prism",
    )
}

#[test]
fn a_fillet_standing_apart_takes_only_its_own_corner() {
    let body = u_prism();
    let before = body.measures().volume;
    let outcome = finish(
        &body,
        vec![vertical_edge(&body, 5.0, 20.0)],
        EdgeFinishKind::Fillet,
        2.0,
        true,
    )
    .expect("a post's corner fillets standing apart");
    assert_valid(&outcome.snapshot);
    assert_close(
        before - outcome.snapshot.measures().volume,
        fillet_corner(2.0, 10.0),
        "material a standing-apart fillet removes",
    );
}

#[test]
fn a_chamfer_standing_apart_takes_only_its_own_corner() {
    let body = u_prism();
    let before = body.measures().volume;
    let outcome = finish(
        &body,
        vec![vertical_edge(&body, 5.0, 20.0)],
        EdgeFinishKind::Chamfer,
        2.0,
        true,
    )
    .expect("a post's corner bevels standing apart");
    assert_valid(&outcome.snapshot);
    assert_close(
        before - outcome.snapshot.measures().volume,
        0.5 * 2.0 * 2.0 * 10.0,
        "material a standing-apart chamfer removes",
    );
}

// ---------------------------------------------------------------------------
// Finding 4: the rim-loop spine was never checked against the cap's other
// loops.
// ---------------------------------------------------------------------------

#[test]
fn a_rim_fillet_that_would_run_into_a_hole_is_refused() {
    // A Ø4 hole whose far side is 1 from the rim: a rim fillet of 3 shrinks
    // the cap past it.
    let body = prism(
        polygon(&[(0.0, 0.0), (40.0, 0.0), (40.0, 30.0), (0.0, 30.0)]),
        vec![circle((3.0, 15.0), 2.0)],
        10.0,
        "box with a hole near the rim",
    );
    let rim = edges_at(&body, 10.0, |point| {
        point.x.abs() < 1.0e-9
            || (point.x - 40.0).abs() < 1.0e-9
            || point.y.abs() < 1.0e-9
            || (point.y - 30.0).abs() < 1.0e-9
    });
    assert_eq!(rim.len(), 4);
    for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
        assert_refused(
            finish(&body, rim.clone(), kind, 3.0, false),
            "RIM_LOOP_DISTANCE_INVALID",
            &format!("a {kind:?} whose shrunk cap crosses the hole"),
        );
    }
    // Clear of the hole, the same rim still fillets.
    let clear = finish(&body, rim, EdgeFinishKind::Fillet, 0.5, false)
        .expect("a rim fillet clear of the hole");
    assert_valid(&clear.snapshot);
}

/// A 40 × 30 × 10 block with a Ø6 through hole centred 5 from the `y = 0`
/// wall.
fn hole_near_a_wall() -> Snapshot {
    prism(
        polygon(&[(0.0, 0.0), (40.0, 0.0), (40.0, 30.0), (0.0, 30.0)]),
        vec![circle((12.0, 5.0), 3.0)],
        10.0,
        "box with a hole near a wall",
    )
}

fn hole_rim(body: &Snapshot, top: f64, at: (f64, f64), radius: f64) -> Vec<EntityRef> {
    edges_at(body, top, |point| {
        ((point.x - at.0).hypot(point.y - at.1) - radius).abs() < 1.0e-6
    })
}

/// The ring a hole-rim fillet of `distance` takes from a hole of `radius`,
/// by Pappus: the corner region at the radius of its centroid.
fn rim_fillet_ring(radius: f64, distance: f64) -> f64 {
    let area = distance * distance * (1.0 - PI / 4.0);
    let centroid = distance * (5.0 / 6.0 - PI / 4.0) / (1.0 - PI / 4.0);
    2.0 * PI * (radius + centroid) * area
}

#[test]
fn a_through_hole_rim_that_would_grow_past_a_wall_is_refused() {
    let body = hole_near_a_wall();
    let rim = hole_rim(&body, 10.0, (12.0, 5.0), 3.0);
    // Grown to 5.3, the hole would cross the wall 5 away.
    for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
        assert_refused(
            finish(&body, rim.clone(), kind, 2.3, false),
            "RIM_LOOP_DISTANCE_INVALID",
            &format!("a {kind:?} whose grown hole crosses the wall"),
        );
    }
    let before = body.measures().volume;
    let clear = finish(&body, rim, EdgeFinishKind::Fillet, 1.5, false)
        .expect("a hole-rim fillet clear of the wall");
    assert_valid(&clear.snapshot);
    assert_close(
        before - clear.snapshot.measures().volume,
        rim_fillet_ring(3.0, 1.5),
        "the ring a clear hole-rim fillet removes",
    );
}

// ---------------------------------------------------------------------------
// Finding 5: the hole-rim room check sampled the wall's other loops.
// ---------------------------------------------------------------------------

#[test]
fn a_blind_hole_rim_that_would_grow_past_an_edge_is_refused() {
    // The hole's centre is 5 from the `y = 0` edge, and the edge's nearest
    // sampled point used to be 5.39 away, past a rim grown to 5.3.
    let block = cuboid((0.0, 0.0, 0.0), (40.0, 40.0, 20.0), "blind block");
    let body = drill(&block, 20.0, (12.0, 5.0), 3.0, 8.0);
    let rim = hole_rim(&body, 20.0, (12.0, 5.0), 3.0);
    for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
        assert_refused(
            finish(&body, rim.clone(), kind, 2.3, false),
            "HOLE_RIM_DISTANCE_INVALID",
            &format!("a {kind:?} whose grown rim crosses the edge"),
        );
    }
    let before = body.measures().volume;
    let clear = finish(&body, rim, EdgeFinishKind::Fillet, 1.5, false)
        .expect("a blind hole-rim fillet clear of the edge");
    assert_eq!(
        clear.report.rung.as_deref(),
        Some("edge-finish/hole-rim-blend")
    );
    assert_valid(&clear.snapshot);
    assert_close(
        before - clear.snapshot.measures().volume,
        rim_fillet_ring(3.0, 1.5),
        "the ring a clear blind hole-rim fillet removes",
    );
}

// ---------------------------------------------------------------------------
// Finding 7: more than sixty-four edges stopped the ladder with a false
// reason.
// ---------------------------------------------------------------------------

#[test]
fn a_rim_of_more_than_sixty_four_edges_reaches_the_rim_loop_rung() {
    let sides = 72_usize;
    let apothem = 20.0_f64;
    let circumradius = apothem / (PI / sides as f64).cos();
    let corners = (0..sides)
        .map(|index| {
            let angle = std::f64::consts::TAU * index as f64 / sides as f64;
            (circumradius * angle.cos(), circumradius * angle.sin())
        })
        .collect::<Vec<_>>();
    let height = 12.0;
    let body = prism(polygon(&corners), Vec::new(), height, "72-gon");
    let rim = edges_at(&body, height, |_| true);
    assert_eq!(rim.len(), sides);
    let distance = 1.0;
    let outcome = finish(&body, rim, EdgeFinishKind::Chamfer, distance, false)
        .expect("a 72-edge rim chamfers");
    assert_eq!(
        outcome.report.rung.as_deref(),
        Some("edge-finish/rim-loop-blend")
    );
    assert_valid(&outcome.snapshot);
    // Each section of the band is the polygon inset by its depth into it, a
    // regular polygon of apothem `a − s` and area `n·tan(π/n)·(a − s)²`.
    let scale = sides as f64 * (PI / sides as f64).tan();
    let base = scale * apothem * apothem;
    let band = scale * (apothem.powi(3) - (apothem - distance).powi(3)) / 3.0;
    assert_close(
        outcome.snapshot.measures().volume,
        base * (height - distance) + band,
        "72-gon rim chamfer",
    );
}

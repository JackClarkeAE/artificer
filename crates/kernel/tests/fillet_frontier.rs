//! The general-fillet frontier (ADR 0056, track F): every capability that
//! landed there, pinned by a closed-form volume, a smoothness check along
//! each contact edge, and a sweep across radii.
//!
//! Every expectation here is computed in this file, not read off the kernel.
//! A torus band's volume is Pappus: the corner region's area swept round the
//! axis at the radius of its own centroid. A straight band's is prism
//! arithmetic. A radius sweep is here because a decision resting on
//! arithmetic rather than on shape shows itself as scattered refusals across
//! a smooth range of sizes, which no single size can reveal (ADR 0045).

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EdgeFinishKind,
    EntityRef, ExecuteRequest, FaceExtrusionOperation, KernelCommand, PlanarCurve2, PlanarFrame3,
    PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId,
    ValidationProfile, Vector3,
};

// ---------------------------------------------------------------------------
// Building fixtures
// ---------------------------------------------------------------------------

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

fn boolean(
    target: &Snapshot,
    tool: &Snapshot,
    operation: BooleanOperation,
    label: &str,
) -> Snapshot {
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation,
    };
    NativeKernel::execute_boolean(target, tool, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"))
        .snapshot
}

fn subtract(target: &Snapshot, tool: &Snapshot, label: &str) -> Snapshot {
    boolean(target, tool, BooleanOperation::Difference, label)
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

/// A round feature of `radius` on the `z = top` face at `(x, y)`: a boss
/// `distance` tall, or a blind hole `distance` deep.
fn round_feature(
    body: &Snapshot,
    top: f64,
    at: (f64, f64),
    radius: f64,
    distance: f64,
    operation: FaceExtrusionOperation,
) -> Snapshot {
    let face = face_where(body, |centre| {
        (centre.z - top).abs() < 1.0e-6 && (centre.x - at.0).hypot(centre.y - at.1) > radius
    });
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
            distance,
            operation,
        },
        "round feature",
    )
}

fn boss(body: &Snapshot, top: f64, at: (f64, f64), radius: f64, height: f64) -> Snapshot {
    round_feature(body, top, at, radius, height, FaceExtrusionOperation::Add)
}

fn drill(body: &Snapshot, top: f64, at: (f64, f64), radius: f64, depth: f64) -> Snapshot {
    round_feature(body, top, at, radius, depth, FaceExtrusionOperation::Cut)
}

// ---------------------------------------------------------------------------
// Picking edges
// ---------------------------------------------------------------------------

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

/// The rim of a round feature: every arc at height `z` on the circle of
/// `radius` about `at`.
fn rim(snapshot: &Snapshot, z: f64, at: (f64, f64), radius: f64) -> Vec<EntityRef> {
    edges_at(snapshot, z, |point| {
        ((point.x - at.0).hypot(point.y - at.1) - radius).abs() < 1.0e-6
    })
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

// ---------------------------------------------------------------------------
// Running and checking finishes
// ---------------------------------------------------------------------------

fn finish(
    body: &Snapshot,
    targets: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
) -> Result<ExecutionOutcome, String> {
    run(
        body,
        KernelCommand::FinishEdges {
            target_edges: targets,
            kind,
            distance,
            standing_apart: false,
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

/// An exact result carries no caveat.
fn assert_exact(outcome: &ExecutionOutcome, rung: &str, what: &str) {
    assert_eq!(outcome.report.rung.as_deref(), Some(rung), "{what}: rung");
    assert!(
        outcome.report.warnings.is_empty(),
        "{what}: an exact finish carries no caveat: {:?}",
        outcome.report.warnings
    );
    assert_valid(&outcome.snapshot);
}

/// The band meets each neighbour smoothly along every edge that passes the
/// filter: the display scene marks the edge a tangent rail, and the exact
/// carrier normals the two faces publish at the edge's chord ends agree to
/// the angular agreement.
fn assert_smooth_along(snapshot: &Snapshot, keep: impl Fn(Point3) -> bool, what: &str) {
    let scene = NativeKernel::debug_scene(snapshot);
    let agreement = PrecisionPolicy::default().angular_agreement_radians;
    let mut rails = 0;
    let mut compared = 0;
    for chord in scene
        .edges
        .iter()
        .filter(|edge| keep(edge.endpoints[0]) && keep(edge.endpoints[1]))
    {
        assert!(
            chord.is_tangent,
            "{what}: the contact edge through {:?} should be a tangent rail, not a crease",
            chord.endpoints[0]
        );
        rails += 1;
        let [Some(first), Some(second)] = chord.incident_faces else {
            panic!("{what}: a contact edge separates two faces");
        };
        for point in chord.endpoints {
            let normal_on = |face: EntityRef| {
                scene
                    .triangles
                    .iter()
                    .filter(|triangle| triangle.source_face == face)
                    .find_map(|triangle| {
                        (0..3)
                            .find(|slot| {
                                let vertex = triangle.vertices[*slot];
                                (vertex.x - point.x).abs() < 1.0e-6
                                    && (vertex.y - point.y).abs() < 1.0e-6
                                    && (vertex.z - point.z).abs() < 1.0e-6
                            })
                            .map(|slot| triangle.normals[slot])
                    })
            };
            let (Some(a), Some(b)) = (normal_on(first), normal_on(second)) else {
                continue;
            };
            // Two unit normals a small angle apart differ by a chord of
            // that length; comparing chords keeps the check meaningful where
            // `cos` of the agreement rounds to one.
            let apart = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt();
            assert!(
                apart <= agreement,
                "{what}: normals disagree at {point:?}: {a:?} against {b:?}"
            );
            compared += 1;
        }
    }
    assert!(rails > 0, "{what}: the filter should select a contact edge");
    assert!(
        compared > 0,
        "{what}: the two faces should publish normals at the contact edge"
    );
}

// ---------------------------------------------------------------------------
// Closed forms
// ---------------------------------------------------------------------------

/// The corner region a rolling ball of radius `d` cannot reach at a
/// right-angled edge: a square of side `d` less its inscribed quarter disc.
fn corner_area(d: f64) -> f64 {
    d * d * (1.0 - PI / 4.0)
}

/// How far that region's centroid sits from the edge's two faces.
fn corner_centroid(d: f64) -> f64 {
    d * (5.0 / 6.0 - PI / 4.0) / (1.0 - PI / 4.0)
}

/// The material a concave rim fillet adds, by Pappus: the corner region
/// swept round the axis at the radius of its centroid, which lies outside
/// the rim of a boss and inside the rim of a pocket.
fn concave_rim_fillet(radius: f64, d: f64, boss: bool) -> f64 {
    let centroid = if boss {
        radius + corner_centroid(d)
    } else {
        radius - corner_centroid(d)
    };
    2.0 * PI * centroid * corner_area(d)
}

/// The material a concave rim chamfer adds: the right triangle of leg `d`
/// swept round at the radius of its centroid, `d/3` from the wall.
fn concave_rim_chamfer(radius: f64, d: f64, boss: bool) -> f64 {
    let centroid = if boss {
        radius + d / 3.0
    } else {
        radius - d / 3.0
    };
    2.0 * PI * centroid * d * d / 2.0
}

// ---------------------------------------------------------------------------
// F2: concave rims
// ---------------------------------------------------------------------------

const PLATE: (f64, f64, f64) = (40.0, 40.0, 10.0);
const BOSS_RADIUS: f64 = 8.0;
const BOSS_HEIGHT: f64 = 6.0;
const CENTRE: (f64, f64) = (20.0, 20.0);

/// A 40 × 40 × 10 plate with an Ø16 boss 6 tall on its top.
fn boss_on_plate() -> Snapshot {
    let plate = cuboid((0.0, 0.0, 0.0), PLATE, "plate");
    let body = boss(&plate, PLATE.2, CENTRE, BOSS_RADIUS, BOSS_HEIGHT);
    assert_close(
        body.measures().volume,
        PLATE.0 * PLATE.1 * PLATE.2 + PI * BOSS_RADIUS * BOSS_RADIUS * BOSS_HEIGHT,
        "boss on plate",
    );
    body
}

const BLOCK: (f64, f64, f64) = (40.0, 40.0, 20.0);
const POCKET_DEPTH: f64 = 6.0;

/// A 40 × 40 × 20 block with an Ø16 blind pocket 6 deep in its top.
fn pocketed_block() -> Snapshot {
    let block = cuboid((0.0, 0.0, 0.0), BLOCK, "block");
    let body = drill(&block, BLOCK.2, CENTRE, BOSS_RADIUS, POCKET_DEPTH);
    assert_close(
        body.measures().volume,
        BLOCK.0 * BLOCK.1 * BLOCK.2 - PI * BOSS_RADIUS * BOSS_RADIUS * POCKET_DEPTH,
        "pocketed block",
    );
    body
}

const BORE_RADIUS: f64 = 4.0;

/// The pocketed block drilled through its floor with an Ø8 bore: a
/// counterbore, whose floor is an annulus between the two walls.
fn counterbored_block() -> Snapshot {
    let body = pocketed_block();
    let floor = BLOCK.2 - POCKET_DEPTH;
    let bored = drill(&body, floor, CENTRE, BORE_RADIUS, floor);
    assert_close(
        bored.measures().volume,
        BLOCK.0 * BLOCK.1 * BLOCK.2
            - PI * BOSS_RADIUS * BOSS_RADIUS * POCKET_DEPTH
            - PI * BORE_RADIUS * BORE_RADIUS * floor,
        "counterbored block",
    );
    bored
}

fn on_circle(at: (f64, f64), radius: f64, z: f64) -> impl Fn(Point3) -> bool {
    move |point| {
        (point.z - z).abs() < 1.0e-6
            && ((point.x - at.0).hypot(point.y - at.1) - radius).abs() < 1.0e-6
    }
}

#[test]
fn a_boss_rim_fillets_exactly_by_pappus() {
    let body = boss_on_plate();
    let before = body.measures().volume;
    let d = 2.0;
    let outcome = finish(
        &body,
        rim(&body, PLATE.2, CENTRE, BOSS_RADIUS),
        EdgeFinishKind::Fillet,
        d,
    )
    .expect("a boss rim fillets");
    assert_exact(&outcome, "edge-finish/concave-rim-blend", "boss rim fillet");
    assert_close(
        outcome.snapshot.measures().volume - before,
        concave_rim_fillet(BOSS_RADIUS, d, true),
        "material a boss rim fillet adds",
    );
    // The band meets the plate at the grown circle and the boss at height
    // `d`, tangentially at both.
    assert_smooth_along(
        &outcome.snapshot,
        on_circle(CENTRE, BOSS_RADIUS + d, PLATE.2),
        "boss fillet against the plate",
    );
    assert_smooth_along(
        &outcome.snapshot,
        on_circle(CENTRE, BOSS_RADIUS, PLATE.2 + d),
        "boss fillet against the boss",
    );
}

#[test]
fn a_boss_rim_chamfers_by_the_frustum() {
    let body = boss_on_plate();
    let before = body.measures().volume;
    let d = 2.0;
    let outcome = finish(
        &body,
        rim(&body, PLATE.2, CENTRE, BOSS_RADIUS),
        EdgeFinishKind::Chamfer,
        d,
    )
    .expect("a boss rim chamfers");
    assert_exact(
        &outcome,
        "edge-finish/concave-rim-blend",
        "boss rim chamfer",
    );
    assert_close(
        outcome.snapshot.measures().volume - before,
        concave_rim_chamfer(BOSS_RADIUS, d, true),
        "material a boss rim chamfer adds",
    );
}

#[test]
fn a_pocket_floor_rim_fillets_and_chamfers_exactly() {
    let body = pocketed_block();
    let before = body.measures().volume;
    let floor = BLOCK.2 - POCKET_DEPTH;
    let d = 2.0;
    let rounded = finish(
        &body,
        rim(&body, floor, CENTRE, BOSS_RADIUS),
        EdgeFinishKind::Fillet,
        d,
    )
    .expect("a pocket floor rim fillets");
    assert_exact(
        &rounded,
        "edge-finish/concave-rim-blend",
        "pocket rim fillet",
    );
    assert_close(
        rounded.snapshot.measures().volume - before,
        concave_rim_fillet(BOSS_RADIUS, d, false),
        "material a pocket rim fillet adds",
    );
    assert_smooth_along(
        &rounded.snapshot,
        on_circle(CENTRE, BOSS_RADIUS - d, floor),
        "pocket fillet against the floor",
    );
    assert_smooth_along(
        &rounded.snapshot,
        on_circle(CENTRE, BOSS_RADIUS, floor + d),
        "pocket fillet against the wall",
    );

    let bevelled = finish(
        &body,
        rim(&body, floor, CENTRE, BOSS_RADIUS),
        EdgeFinishKind::Chamfer,
        d,
    )
    .expect("a pocket floor rim chamfers");
    assert_exact(
        &bevelled,
        "edge-finish/concave-rim-blend",
        "pocket rim chamfer",
    );
    assert_close(
        bevelled.snapshot.measures().volume - before,
        concave_rim_chamfer(BOSS_RADIUS, d, false),
        "material a pocket rim chamfer adds",
    );
}

#[test]
fn a_counterbore_floor_rim_fillets_around_its_bore() {
    let body = counterbored_block();
    let before = body.measures().volume;
    let floor = BLOCK.2 - POCKET_DEPTH;
    let d = 2.0;
    let outcome = finish(
        &body,
        rim(&body, floor, CENTRE, BOSS_RADIUS),
        EdgeFinishKind::Fillet,
        d,
    )
    .expect("a counterbore floor rim fillets");
    assert_exact(
        &outcome,
        "edge-finish/concave-rim-blend",
        "counterbore rim fillet",
    );
    assert_close(
        outcome.snapshot.measures().volume - before,
        concave_rim_fillet(BOSS_RADIUS, d, false),
        "material a counterbore rim fillet adds",
    );
    // The bore through the floor is untouched: its rim is still there.
    assert_eq!(rim(&outcome.snapshot, floor, CENTRE, BORE_RADIUS).len(), 2);
    // Shrunk past the bore, the floor would have nothing left to stand on.
    assert_refused(
        finish(
            &body,
            rim(&body, floor, CENTRE, BOSS_RADIUS),
            EdgeFinishKind::Fillet,
            4.5,
        ),
        "CONCAVE_RIM_DISTANCE_INVALID",
        "a counterbore rim fillet reaching the bore",
    );
}

#[test]
fn a_concave_rim_finish_that_does_not_fit_is_refused_by_name() {
    let body = boss_on_plate();
    // Taller than the boss: nothing above the contact ring.
    for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
        assert_refused(
            finish(
                &body,
                rim(&body, PLATE.2, CENTRE, BOSS_RADIUS),
                kind,
                BOSS_HEIGHT,
            ),
            "CONCAVE_RIM_DISTANCE_INVALID",
            &format!("a {kind:?} as tall as the boss"),
        );
    }
    // A boss 5 from the plate's edge: grown to 5.5, the contact circle would
    // cross it.
    let plate = cuboid((0.0, 0.0, 0.0), PLATE, "plate");
    let near_edge = boss(&plate, PLATE.2, (13.0, 20.0), BOSS_RADIUS, BOSS_HEIGHT);
    assert_refused(
        finish(
            &near_edge,
            rim(&near_edge, PLATE.2, (13.0, 20.0), BOSS_RADIUS),
            EdgeFinishKind::Fillet,
            5.5,
        ),
        "CONCAVE_RIM_DISTANCE_INVALID",
        "a boss rim fillet grown past the plate's edge",
    );
    let clear = finish(
        &near_edge,
        rim(&near_edge, PLATE.2, (13.0, 20.0), BOSS_RADIUS),
        EdgeFinishKind::Fillet,
        4.5,
    )
    .expect("a boss rim fillet clear of the plate's edge");
    assert_exact(
        &clear,
        "edge-finish/concave-rim-blend",
        "boss near the edge",
    );
}

/// The same rims at twenty-four sizes, both kinds, both ways round.
#[test]
fn a_concave_rim_finishes_at_every_size() {
    let boss_body = boss_on_plate();
    let pocket_body = pocketed_block();
    let floor = BLOCK.2 - POCKET_DEPTH;
    let mut trouble = Vec::new();
    for step in 1..=24 {
        let d = f64::from(step) * 0.225;
        for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
            for (body, z, is_boss) in [(&boss_body, PLATE.2, true), (&pocket_body, floor, false)] {
                let before = body.measures().volume;
                let targets = rim(body, z, CENTRE, BOSS_RADIUS);
                let name = if is_boss { "boss" } else { "pocket" };
                match finish(body, targets, kind, d) {
                    Ok(outcome) => {
                        let want = if matches!(kind, EdgeFinishKind::Fillet) {
                            concave_rim_fillet(BOSS_RADIUS, d, is_boss)
                        } else {
                            concave_rim_chamfer(BOSS_RADIUS, d, is_boss)
                        };
                        let got = outcome.snapshot.measures().volume - before;
                        if ((got - want) / want).abs() > 1.0e-9 {
                            trouble.push(format!(
                                "{name} {kind:?} {d:.3}: added {got}, wanted {want}"
                            ));
                        }
                        if outcome.report.rung.as_deref() != Some("edge-finish/concave-rim-blend") {
                            trouble.push(format!(
                                "{name} {kind:?} {d:.3}: built by {:?}",
                                outcome.report.rung
                            ));
                        }
                    }
                    Err(error) => trouble.push(format!("{name} {kind:?} {d:.3}: {error}")),
                }
            }
        }
    }
    assert!(
        trouble.is_empty(),
        "{} of 96 concave rim finishes refused or drifted:\n{}",
        trouble.len(),
        trouble.join("\n")
    );
}

/// Every edge both of whose chord ends pass the filter.
fn edges_where(snapshot: &Snapshot, keep: impl Fn(Point3) -> bool) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut found: Vec<EntityRef> = Vec::new();
    for edge in &scene.edges {
        let [a, b] = edge.endpoints;
        if keep(a) && keep(b) && !found.contains(&edge.source_edge) {
            found.push(edge.source_edge);
        }
    }
    assert!(
        !found.is_empty(),
        "the fixture has edges passing the filter"
    );
    found
}

#[test]
fn a_boss_on_a_side_wall_fillets_by_pappus() {
    // The boss stands on the x = 40 wall, about the axis through (y, z) =
    // (20, 10): nothing about the rim is aligned with the frame a cap gives.
    let block = cuboid((0.0, 0.0, 0.0), BLOCK, "block");
    let wall = face_where(&block, |centre| (centre.x - 40.0).abs() < 1.0e-6);
    let (radius, height, d) = (6.0, 5.0, 1.5);
    let body = build(
        &block,
        KernelCommand::ExtrudeFacePlanarProfile {
            target_face: wall,
            // u along y, v along z: the frame's normal is +x, the wall's.
            frame: PlanarFrame3::new(
                Point3::new(40.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle((20.0, 10.0), radius),
                    holes: vec![],
                }],
            },
            distance: height,
            operation: FaceExtrusionOperation::Add,
        },
        "side boss",
    );
    let before = body.measures().volume;
    let on_rim = |at_radius: f64, x: f64| {
        move |point: Point3| {
            (point.x - x).abs() < 1.0e-6
                && ((point.y - 20.0).hypot(point.z - 10.0) - at_radius).abs() < 1.0e-6
        }
    };
    let outcome = finish(
        &body,
        edges_where(&body, on_rim(radius, 40.0)),
        EdgeFinishKind::Fillet,
        d,
    )
    .expect("a boss rim on a side wall fillets");
    assert_exact(&outcome, "edge-finish/concave-rim-blend", "side wall boss");
    assert_close(
        outcome.snapshot.measures().volume - before,
        concave_rim_fillet(radius, d, true),
        "material a side-wall boss fillet adds",
    );
    assert_smooth_along(
        &outcome.snapshot,
        on_rim(radius + d, 40.0),
        "against the wall",
    );
    assert_smooth_along(
        &outcome.snapshot,
        on_rim(radius, 40.0 + d),
        "against the boss",
    );
}

/// The corner region of a fillet in an air wedge of angle `alpha`, per unit
/// length: `r²·cot(α/2) − ½r²(π − α)`.
fn oblique_fillet_area(r: f64, alpha: f64) -> f64 {
    let half = alpha / 2.0;
    r * r * (half.cos() / half.sin()) - 0.5 * r * r * (PI - alpha)
}

#[test]
fn a_reflex_edge_with_an_oblique_wedge_fills_by_its_own_closed_form() {
    // The L's upright arm leans: the reflex corner at (6, 4) opens between
    // the +x direction and (−4, 5), an air wedge of atan2(5, −4).
    let body = prism(
        polygon(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 4.0),
            (6.0, 4.0),
            (2.0, 9.0),
            (0.0, 9.0),
        ]),
        Vec::new(),
        7.0,
        "leaning L",
    );
    let body = drill(&body, 7.0, (3.0, 3.0), 1.0, 3.0);
    let before = body.measures().volume;
    let alpha = 5.0_f64.atan2(-4.0);
    let mut trouble = Vec::new();
    for step in 1..=20 {
        let size = f64::from(step) * 0.12;
        for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
            let want = match kind {
                EdgeFinishKind::Fillet => oblique_fillet_area(size, alpha) * 7.0,
                EdgeFinishKind::Chamfer => 0.5 * size * size * alpha.sin() * 7.0,
            };
            match finish(&body, vec![vertical_edge(&body, 6.0, 4.0)], kind, size) {
                Ok(outcome) => {
                    let got = outcome.snapshot.measures().volume - before;
                    if ((got - want) / want).abs() > 1.0e-9 {
                        trouble.push(format!("{kind:?} {size:.2}: added {got}, wanted {want}"));
                    }
                }
                Err(error) => trouble.push(format!("{kind:?} {size:.2}: {error}")),
            }
        }
    }
    assert!(
        trouble.is_empty(),
        "{} of 40 oblique fills refused or drifted:\n{}",
        trouble.len(),
        trouble.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The shared drilled L-block
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

/// What a fillet of radius `r` adds to a right-angled concave edge of
/// `length`, or takes from a convex one: the corner region a rolling ball
/// cannot reach, swept along the edge.
fn fillet_corner(radius: f64, length: f64) -> f64 {
    corner_area(radius) * length
}

/// The same for a chamfer: the right triangle of leg `d`.
fn chamfer_corner(d: f64, length: f64) -> f64 {
    0.5 * d * d * length
}

/// A point on the line through `(x, y)` along `z`.
fn on_vertical(x: f64, y: f64) -> impl Fn(Point3) -> bool {
    move |point| (point.x - x).abs() < 1.0e-6 && (point.y - y).abs() < 1.0e-6
}

// ---------------------------------------------------------------------------
// F2: concave straight edges on a body no prism rung owns
// ---------------------------------------------------------------------------

#[test]
fn a_reflex_edge_of_a_drilled_block_fillets_by_prism_arithmetic() {
    let body = drilled_l_block();
    let before = body.measures().volume;
    let r = 1.0;
    let outcome = finish(
        &body,
        vec![vertical_edge(&body, 6.0, 4.0)],
        EdgeFinishKind::Fillet,
        r,
    )
    .expect("the reflex edge of a drilled block fillets");
    assert_exact(&outcome, "edge-finish/concave-fill", "reflex edge fillet");
    assert_close(
        outcome.snapshot.measures().volume - before,
        fillet_corner(r, 7.0),
        "material a reflex edge fillet adds",
    );
    // The band meets each wall along a line the full height of the edge,
    // tangentially.
    assert_smooth_along(
        &outcome.snapshot,
        on_vertical(6.0, 4.0 + r),
        "reflex fillet against the x = 6 wall",
    );
    assert_smooth_along(
        &outcome.snapshot,
        on_vertical(6.0 + r, 4.0),
        "reflex fillet against the y = 4 wall",
    );
    // The hole is untouched.
    assert_eq!(rim(&outcome.snapshot, 7.0, (3.0, 3.0), 1.0).len(), 2);
}

#[test]
fn a_reflex_edge_of_a_drilled_block_chamfers_by_prism_arithmetic() {
    let body = drilled_l_block();
    let before = body.measures().volume;
    let d = 1.5;
    let outcome = finish(
        &body,
        vec![vertical_edge(&body, 6.0, 4.0)],
        EdgeFinishKind::Chamfer,
        d,
    )
    .expect("the reflex edge of a drilled block chamfers");
    assert_exact(&outcome, "edge-finish/concave-fill", "reflex edge chamfer");
    assert_close(
        outcome.snapshot.measures().volume - before,
        chamfer_corner(d, 7.0),
        "material a reflex edge chamfer adds",
    );
    // A bevel's two edges are creases, not rails.
    let scene = NativeKernel::debug_scene(&outcome.snapshot);
    let creases = scene
        .edges
        .iter()
        .filter(|edge| {
            let keep = on_vertical(6.0, 4.0 + d);
            keep(edge.endpoints[0]) && keep(edge.endpoints[1])
        })
        .count();
    assert!(creases > 0, "the bevel meets the wall along an edge");
    assert!(
        scene.edges.iter().all(|edge| {
            let keep = on_vertical(6.0, 4.0 + d);
            !(keep(edge.endpoints[0]) && keep(edge.endpoints[1]) && edge.is_tangent)
        }),
        "a bevel's edge is not a tangent rail"
    );
}

/// A U: a 30 × 5 base with 5-wide posts at each end up to `y = 20`, 10 tall,
/// with a blind hole in one post so that no prism rung owns it.
fn drilled_u_channel() -> Snapshot {
    let channel = prism(
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
        "u-channel",
    );
    assert_close(
        channel.measures().volume,
        (30.0 * 5.0 + 2.0 * 5.0 * 15.0) * 10.0,
        "U channel",
    );
    drill(&channel, 10.0, (2.5, 12.0), 1.0, 3.0)
}

#[test]
fn a_drilled_u_channel_fills_both_inner_edges() {
    let body = drilled_u_channel();
    let before = body.measures().volume;
    let inner = vec![
        vertical_edge(&body, 5.0, 5.0),
        vertical_edge(&body, 25.0, 5.0),
    ];
    let r = 2.0;
    let rounded = finish(&body, inner.clone(), EdgeFinishKind::Fillet, r)
        .expect("both inner edges of a drilled channel fillet");
    assert_exact(&rounded, "edge-finish/concave-fill", "U channel fillets");
    assert_close(
        rounded.snapshot.measures().volume - before,
        2.0 * fillet_corner(r, 10.0),
        "material two inner fillets add",
    );
    for x in [5.0 + r, 25.0 - r] {
        assert_smooth_along(
            &rounded.snapshot,
            on_vertical(x, 5.0),
            "U fillet against the base",
        );
    }
    for x in [5.0, 25.0] {
        assert_smooth_along(
            &rounded.snapshot,
            on_vertical(x, 5.0 + r),
            "U fillet against a post",
        );
    }
    let bevelled = finish(&body, inner, EdgeFinishKind::Chamfer, r)
        .expect("both inner edges of a drilled channel chamfer");
    assert_exact(&bevelled, "edge-finish/concave-fill", "U channel chamfers");
    assert_close(
        bevelled.snapshot.measures().volume - before,
        2.0 * chamfer_corner(r, 10.0),
        "material two inner chamfers add",
    );
}

/// The reflex edge at twenty-two sizes, both kinds.
#[test]
fn a_concave_edge_fills_at_every_size() {
    let body = drilled_l_block();
    let before = body.measures().volume;
    let mut trouble = Vec::new();
    for step in 1..=22 {
        let size = f64::from(step) * 0.15;
        for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
            let want = match kind {
                EdgeFinishKind::Fillet => fillet_corner(size, 7.0),
                EdgeFinishKind::Chamfer => chamfer_corner(size, 7.0),
            };
            match finish(&body, vec![vertical_edge(&body, 6.0, 4.0)], kind, size) {
                Ok(outcome) => {
                    let got = outcome.snapshot.measures().volume - before;
                    if ((got - want) / want).abs() > 1.0e-9 {
                        trouble.push(format!("{kind:?} {size:.2}: added {got}, wanted {want}"));
                    }
                    if !outcome.report.warnings.is_empty() {
                        trouble.push(format!("{kind:?} {size:.2}: {:?}", outcome.report.warnings));
                    }
                }
                Err(error) => trouble.push(format!("{kind:?} {size:.2}: {error}")),
            }
        }
    }
    assert!(
        trouble.is_empty(),
        "{} of 44 concave edge finishes refused or drifted:\n{}",
        trouble.len(),
        trouble.join("\n")
    );
}

#[test]
fn a_concave_edge_finish_that_does_not_fit_is_refused_by_name() {
    let body = drilled_l_block();
    // The face at y = 4 is 4 wide: a band set back 4.5 along it runs off the
    // body, and the fill would add more than its own corner.
    for kind in [EdgeFinishKind::Fillet, EdgeFinishKind::Chamfer] {
        assert_refused(
            finish(&body, vec![vertical_edge(&body, 6.0, 4.0)], kind, 4.5),
            "CONCAVE_EDGE_DISTANCE_INVALID",
            &format!("a {kind:?} wider than the face beside the edge"),
        );
    }
    // An edge that ends against a leaning face: the block's top cut to a
    // slope, so the reflex edge's upper end is no longer square.
    let slope = build(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            // A frame in the plane y = −1 with u along z and v along x, so
            // the prism runs toward +y; the profile is the region above the
            // line z = 4 + 0.3·x.
            frame: PlanarFrame3::new(
                Point3::new(0.0, -1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
                Vector3::new(1.0, 0.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: polygon(&[(3.7, -1.0), (7.3, 11.0), (9.0, 11.0), (9.0, -1.0)]),
                    holes: vec![],
                }],
            },
            distance: 11.0,
        },
        "slope cutter",
    );
    let sloped = subtract(&body, &slope, "sloped L block");
    assert_refused(
        finish(
            &sloped,
            vec![vertical_edge(&sloped, 6.0, 4.0)],
            EdgeFinishKind::Fillet,
            1.0,
        ),
        "CONCAVE_EDGE_END_UNSUPPORTED",
        "a reflex edge ending against a leaning face",
    );
}

// ---------------------------------------------------------------------------
// F3: mixed selections
// ---------------------------------------------------------------------------

/// What two bands standing apart share at a right-angled corner, so the
/// second removes that much less (ADR 0044): `r³(5/3 − π/2)` for fillets,
/// `d³/3` for bevels.
fn pair_overlap(kind: EdgeFinishKind, size: f64) -> f64 {
    match kind {
        EdgeFinishKind::Fillet => size.powi(3) * (5.0 / 3.0 - PI / 2.0),
        EdgeFinishKind::Chamfer => size.powi(3) / 3.0,
    }
}

#[test]
fn the_l_block_finishes_its_concave_and_convex_edges_in_one_call() {
    let body = drilled_l_block();
    let before = body.measures().volume;
    let top = edges_at(&body, 7.0, |_| true)
        .into_iter()
        .filter(|edge| {
            // The top rim's straight edges, not the hole's arcs.
            NativeKernel::describe_edge(&body, *edge)
                .is_ok_and(|description| description.geometry.curve_kind() == "line")
        })
        .collect::<Vec<_>>();
    assert_eq!(top.len(), 6, "the six straight edges of the top face");
    for (kind, size) in [
        (EdgeFinishKind::Fillet, 1.0),
        (EdgeFinishKind::Chamfer, 0.8),
    ] {
        let mut targets = vec![vertical_edge(&body, 6.0, 4.0)];
        targets.extend(top.iter().copied());
        let outcome = finish(&body, targets, kind, size)
            .unwrap_or_else(|error| panic!("the L block {kind:?}s all seven edges: {error}"));
        assert_exact(&outcome, "edge-finish/concave-fill", "mixed L block");
        let corner = |length: f64| match kind {
            EdgeFinishKind::Fillet => fillet_corner(size, length),
            EdgeFinishKind::Chamfer => chamfer_corner(size, length),
        };
        // The reflex edge is filled its full height. The two top edges that
        // meet it are cut back to where its band begins, a size in from the
        // corner, and run out at their far ends. The other four are cut
        // standing apart, so each of the five convex corners of the top rim
        // is a notch two bands share.
        let expected = before + corner(7.0)
            - corner(5.0 - size)
            - corner(4.0 - size)
            - corner(10.0 + 4.0 + 6.0 + 9.0)
            + 5.0 * pair_overlap(kind, size);
        assert_close(
            outcome.snapshot.measures().volume,
            expected,
            &format!("the L block with every top edge and its reflex edge {kind:?}ed"),
        );
    }
}

// ---------------------------------------------------------------------------
// F5, first slice: a radius that changes along the edge, approximate and
// labelled
// ---------------------------------------------------------------------------

/// The material a fillet whose radius runs linearly from `r0` to `r1` takes
/// from a right-angled edge of `length`: `(1 − π/4)∫r(s)² ds`.
fn variable_fillet_removed(r0: f64, r1: f64, length: f64) -> f64 {
    (1.0 - PI / 4.0) * (r0 * r0 + r0 * r1 + r1 * r1) / 3.0 * length
}

fn variable_fillet(
    body: &Snapshot,
    target: EntityRef,
    radii: [f64; 2],
) -> Result<ExecutionOutcome, String> {
    NativeKernel::finish_edge_variable_radius(body, target, radii, PrecisionPolicy::default())
        .map_err(|error| format!("{error:?}"))
}

#[test]
fn a_fillet_whose_radius_changes_along_the_edge_is_approximate_and_says_so() {
    let side = 10.0;
    let cube = cuboid((0.0, 0.0, 0.0), (side, side, side), "cube");
    let before = cube.measures().volume;
    let (r0, r1) = (1.0, 2.0);
    let outcome = variable_fillet(&cube, vertical_edge(&cube, side, side), [r0, r1])
        .expect("a variable-radius fillet builds");
    assert_valid(&outcome.snapshot);
    assert_eq!(
        outcome.report.rung.as_deref(),
        Some("variable-radius/faceted")
    );
    assert_eq!(
        outcome.report.tier(),
        artificer_protocol::Tier::Approximate,
        "a faceted band is an approximation"
    );
    let caveat = outcome
        .report
        .warnings
        .iter()
        .find(|warning| {
            warning.code.as_str() == "EDGE_FINISH_VARIABLE_RADIUS_FACETED_APPROXIMATION"
        })
        .expect("the result carries its caveat");
    let measurement = caveat
        .measurement
        .expect("the caveat carries the measured deviation");
    assert!(
        measurement.measured > 0.0 && measurement.measured < r1 * 1.0e-2,
        "the deviation is the chords' sagitta: {}",
        measurement.measured
    );
    // The facets lie inside the true arc, so the polyhedron takes at least
    // the cone's corner and only a little more: the segments between the
    // chords and the arc, which the sagitta bounds.
    let removed = before - outcome.snapshot.measures().volume;
    let exact = variable_fillet_removed(r0, r1, side);
    assert!(
        removed >= exact - 1.0e-9,
        "removed {removed} is at least the cone's {exact}"
    );
    assert!(
        removed <= exact + measurement.measured * PI / 2.0 * r1 * side,
        "removed {removed} exceeds the cone's {exact} by more than the chords allow"
    );
}

#[test]
fn a_variable_fillet_builds_at_every_pair_of_radii() {
    let side = 10.0;
    let cube = cuboid((0.0, 0.0, 0.0), (side, side, side), "cube");
    let before = cube.measures().volume;
    let mut trouble = Vec::new();
    for step in 1..=24 {
        let r0 = 0.25 + f64::from(step) * 0.1;
        let r1 = 3.5 - f64::from(step) * 0.1;
        match variable_fillet(&cube, vertical_edge(&cube, side, side), [r0, r1]) {
            Ok(outcome) => {
                let removed = before - outcome.snapshot.measures().volume;
                let exact = variable_fillet_removed(r0, r1, side);
                let sagitta = outcome
                    .report
                    .warnings
                    .iter()
                    .find_map(|warning| warning.measurement)
                    .map_or(f64::NAN, |measurement| measurement.measured);
                if !(removed >= exact - 1.0e-9
                    && removed <= exact + sagitta * PI / 2.0 * r0.max(r1) * side)
                {
                    trouble.push(format!(
                        "{r0:.2}→{r1:.2}: removed {removed}, cone {exact}, sagitta {sagitta}"
                    ));
                }
            }
            Err(error) => trouble.push(format!("{r0:.2}→{r1:.2}: {error}")),
        }
    }
    assert!(
        trouble.is_empty(),
        "{} of 24 variable fillets refused or drifted:\n{}",
        trouble.len(),
        trouble.join("\n")
    );
}

#[test]
fn a_variable_fillet_refuses_what_it_cannot_carry_by_name() {
    let body = drilled_l_block();
    // The reflex edge: this route only removes.
    assert_refused(
        variable_fillet(&body, vertical_edge(&body, 6.0, 4.0), [1.0, 2.0]),
        "VARIABLE_RADIUS_EDGE_UNSUPPORTED",
        "a variable fillet of a reflex edge",
    );
    let cube = cuboid((0.0, 0.0, 0.0), (10.0, 10.0, 10.0), "cube");
    assert_refused(
        variable_fillet(&cube, vertical_edge(&cube, 10.0, 10.0), [1.0, 0.0]),
        "VARIABLE_RADIUS_DISTANCE_INVALID",
        "a variable fillet running to nothing",
    );
}

#[test]
fn a_cube_with_all_twelve_edges_filleted_builds() {
    let side = 10.0;
    let r = 1.5;
    let cube = cuboid((0.0, 0.0, 0.0), (side, side, side), "cube");
    let all = NativeKernel::edges(&cube);
    assert_eq!(all.len(), 12);
    let outcome = finish(&cube, all, EdgeFinishKind::Fillet, r).expect("all twelve edges round");
    assert_valid(&outcome.snapshot);
    assert!(
        outcome.report.warnings.is_empty(),
        "{:?}",
        outcome.report.warnings
    );
    // Minkowski: the inner cube, six slabs, twelve quarter rods, eight
    // sphere octants.
    let inner = side - 2.0 * r;
    assert_close(
        outcome.snapshot.measures().volume,
        inner.powi(3)
            + 6.0 * inner * inner * r
            + 3.0 * inner * PI * r * r
            + 4.0 / 3.0 * PI * r.powi(3),
        "a cube rounded on every edge",
    );
}

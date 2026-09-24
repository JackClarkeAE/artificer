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

#[test]
fn the_drilled_l_block_still_has_its_reflex_edge() {
    let body = drilled_l_block();
    assert_close(body.measures().volume, 490.0 - PI * 3.0, "drilled L block");
    let _ = vertical_edge(&body, 6.0, 4.0);
}

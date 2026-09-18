//! The exact corner-blend rung: fillets and chamfers on convex edges between
//! planar faces, closed by a sphere octant or a planar triangle at every
//! corner the selection completes.
//!
//! Every expectation here is a closed form derived in this file, not a number
//! read back from the kernel. A box rounded by `r` on all twelve edges is the
//! Minkowski sum of the box shrunk by `r` with a ball of radius `r`:
//!
//! ```text
//! V = a'b'c' + 2r(a'b' + b'c' + c'a') + πr²(a' + b' + c') + 4πr³/3
//! A = 2(a'b' + b'c' + c'a') + 2πr(a' + b' + c') + 4πr²
//! ```
//!
//! with `a' = a − 2r`. A box chamfered by `t` on all twelve edges is the box
//! less one triangular prism per edge and the corner each triple of prisms
//! shares. Over the corner cube `[0, t]³` the material that survives is the
//! tetrahedron `u + v + w ≤ t`, of volume `t³/6`, so
//!
//! ```text
//! V = abc − 2t²(a + b + c) + 16t³/3
//! A = 2(a'b' + b'c' + c'a') + 4√2·t·(a' + b' + c') + 4√3·t²
//! ```
//!
//! Note that this is *not* the solid the faceted tier produces for the same
//! request: that one extends the three bevel planes until they meet at a
//! point, which leaves `t³/4` of material at each corner instead of `t³/6`
//! and no corner face at all.

use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::time::{Duration, Instant};

use artificer_kernel::api::export::export_step;
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};

use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, Tier, ValidationProfile, Vector3,
};

/// Closed forms agree with the kernel's own exact measures to this relative
/// size; the kernel's linear agreement is `1e-9` absolute and every number
/// here is a product of a handful of exactly represented terms.
const RELATIVE: f64 = 1.0e-9;

fn close(left: f64, right: f64, what: &str) {
    let scale = left.abs().max(right.abs()).max(1.0);
    assert!(
        (left - right).abs() <= RELATIVE * scale,
        "{what}: {left} is not {right}"
    );
}

// ---------------------------------------------------------------------------
// Building bodies
// ---------------------------------------------------------------------------

fn cuboid(size: [f64; 3]) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("cuboid"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: size[0],
            size_y: size[1],
            size_z: size[2],
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a cuboid builds")
        .snapshot
}

fn extruded(profile: PlanarProfile2, distance: f64) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("extrusion"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("a profile extrudes")
        .snapshot
}

fn rectangle(width: f64, height: f64) -> PlanarLoop2 {
    let corners = [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)];
    PlanarLoop2 {
        curves: (0..4)
            .map(|index| {
                let start = corners[index];
                let end = corners[(index + 1) % 4];
                PlanarCurve2::Line {
                    start: Point2::new(start.0, start.1),
                    end: Point2::new(end.0, end.1),
                }
            })
            .collect(),
    }
}

/// A circular hole, as the two half-circles the profile vocabulary asks for.
fn circle(centre: (f64, f64), radius: f64) -> PlanarLoop2 {
    let left = Point2::new(centre.0 - radius, centre.1);
    let right = Point2::new(centre.0 + radius, centre.1);
    let centre = Point2::new(centre.0, centre.1);
    PlanarLoop2 {
        curves: vec![
            PlanarCurve2::CircularArc {
                center: centre,
                start: right,
                end: left,
                direction: ArcDirection::CounterClockwise,
            },
            PlanarCurve2::CircularArc {
                center: centre,
                start: left,
                end: right,
                direction: ArcDirection::CounterClockwise,
            },
        ],
    }
}

/// A slot: two straight runs joined by a half-circle at each end.
fn slot(centre: (f64, f64), length: f64, radius: f64) -> PlanarLoop2 {
    let right = (centre.0 + length / 2.0, centre.1);
    let left = (centre.0 - length / 2.0, centre.1);
    PlanarLoop2 {
        curves: vec![
            PlanarCurve2::CircularArc {
                center: Point2::new(right.0, right.1),
                start: Point2::new(right.0, right.1 - radius),
                end: Point2::new(right.0, right.1 + radius),
                direction: ArcDirection::CounterClockwise,
            },
            PlanarCurve2::Line {
                start: Point2::new(right.0, right.1 + radius),
                end: Point2::new(left.0, left.1 + radius),
            },
            PlanarCurve2::CircularArc {
                center: Point2::new(left.0, left.1),
                start: Point2::new(left.0, left.1 + radius),
                end: Point2::new(left.0, left.1 - radius),
                direction: ArcDirection::CounterClockwise,
            },
            PlanarCurve2::Line {
                start: Point2::new(left.0, left.1 - radius),
                end: Point2::new(right.0, right.1 - radius),
            },
        ],
    }
}

fn finish(
    snapshot: &Snapshot,
    target_edges: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
) -> Result<ExecutionOutcome, artificer_protocol::KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("finish"),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges,
            kind,
            distance,
            standing_apart: false,
        },
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
}

fn blend(
    snapshot: &Snapshot,
    target_edges: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
) -> Snapshot {
    let outcome = finish(snapshot, target_edges, kind, distance).expect("the blend is published");
    assert_eq!(
        outcome.report.rung.as_deref(),
        Some("edge-finish/vertex-blend"),
        "the corner-blend rung owns this request"
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact rung publishes no caveats: {:?}",
        outcome.report.warnings
    );
    assert!(
        NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid).valid,
        "the blended body is a valid solid"
    );
    outcome.snapshot
}

/// The refusal one request draws, as its first diagnostic code and message.
fn refusal(
    snapshot: &Snapshot,
    target_edges: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
) -> (String, String) {
    let error = finish(snapshot, target_edges, kind, distance).expect_err("this request refuses");
    let diagnostic = error
        .diagnostics
        .first()
        .unwrap_or_else(|| panic!("a refusal names a code: {error:?}"));
    (diagnostic.code.to_string(), diagnostic.message.clone())
}

// ---------------------------------------------------------------------------
// Reading bodies
// ---------------------------------------------------------------------------

fn edges_where(snapshot: &Snapshot, keep: impl Fn(Point3, Point3) -> bool) -> Vec<EntityRef> {
    let mut matching = Vec::new();
    for edge in &NativeKernel::debug_scene(snapshot).edges {
        if keep(edge.endpoints[0], edge.endpoints[1]) && !matching.contains(&edge.source_edge) {
            matching.push(edge.source_edge);
        }
    }
    matching
}

/// Every edge of the outer box of a body that stands on `[0, size]`: the
/// twelve straight edges whose ends both sit on the bounding box's own edges.
fn box_edges(snapshot: &Snapshot, size: [f64; 3]) -> Vec<EntityRef> {
    let on_edge = |point: Point3| {
        let extreme = [
            point.x.abs() < 1.0e-9 || (point.x - size[0]).abs() < 1.0e-9,
            point.y.abs() < 1.0e-9 || (point.y - size[1]).abs() < 1.0e-9,
            point.z.abs() < 1.0e-9 || (point.z - size[2]).abs() < 1.0e-9,
        ];
        extreme.into_iter().filter(|at| *at).count() >= 2
    };
    edges_where(snapshot, |start, end| on_edge(start) && on_edge(end))
}

fn carrier_kinds(snapshot: &Snapshot) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for description in NativeKernel::describe_faces(snapshot).values() {
        *counts
            .entry(description.geometry.surface_kind())
            .or_insert(0) += 1;
    }
    counts
}

// ---------------------------------------------------------------------------
// Closed forms
// ---------------------------------------------------------------------------

fn rounded_box_volume(size: [f64; 3], radius: f64) -> f64 {
    let [a, b, c] = size.map(|side| side - 2.0 * radius);
    a * b * c
        + 2.0 * radius * (a * b + b * c + c * a)
        + PI * radius * radius * (a + b + c)
        + 4.0 / 3.0 * PI * radius.powi(3)
}

fn rounded_box_area(size: [f64; 3], radius: f64) -> f64 {
    let [a, b, c] = size.map(|side| side - 2.0 * radius);
    2.0 * (a * b + b * c + c * a) + 2.0 * PI * radius * (a + b + c) + 4.0 * PI * radius * radius
}

fn chamfered_box_volume(size: [f64; 3], distance: f64) -> f64 {
    size[0] * size[1] * size[2] - 2.0 * distance * distance * (size[0] + size[1] + size[2])
        + 16.0 / 3.0 * distance.powi(3)
}

fn chamfered_box_area(size: [f64; 3], distance: f64) -> f64 {
    let [a, b, c] = size.map(|side| side - 2.0 * distance);
    2.0 * (a * b + b * c + c * a)
        + 4.0 * std::f64::consts::SQRT_2 * distance * (a + b + c)
        + 4.0 * 3.0_f64.sqrt() * distance * distance
}

/// What rounding the three edges of one corner of a box takes away.
///
/// Each band is a prism of cross-section `r² − πr²/4` — the square of side `r`
/// the edge occupied, less the quarter disc the ball leaves — running from the
/// ball's centre plane to the far face the band ends on, so `edge − r` long.
/// Over the corner cube `[0, r]³` the material that survives is one octant of
/// the ball, `πr³/6`.
fn corner_fillet_cut(edges: [f64; 3], radius: f64) -> f64 {
    let quarter = radius * radius * (1.0 - PI / 4.0);
    edges
        .into_iter()
        .map(|edge| (edge - radius) * quarter)
        .sum::<f64>()
        + radius.powi(3) * (1.0 - PI / 6.0)
}

/// What bevelling the three edges of one corner of a box takes away.
///
/// Three wedges of cross-section `t²/2` run the whole length of their edges.
/// Each pair of them overlaps in `t³/3` and all three in `t³/4`, and the
/// corner triangle takes a further tetrahedron of `t³/12` off the point where
/// the three bevel planes would otherwise meet.
fn corner_chamfer_cut(edges: [f64; 3], distance: f64) -> f64 {
    let wedge = distance * distance / 2.0;
    edges.into_iter().map(|edge| edge * wedge).sum::<f64>() - distance.powi(3)
        + distance.powi(3) / 4.0
        + distance.powi(3) / 12.0
}

/// The three edges of a box that meet at one of its eight corners.
fn corner_edges(snapshot: &Snapshot, corner: Point3) -> Vec<EntityRef> {
    let at = |point: Point3| {
        (point.x - corner.x).abs() < 1.0e-9
            && (point.y - corner.y).abs() < 1.0e-9
            && (point.z - corner.z).abs() < 1.0e-9
    };
    edges_where(snapshot, |start, end| at(start) || at(end))
}

// ---------------------------------------------------------------------------
// The exact domain
// ---------------------------------------------------------------------------

#[test]
fn a_filleted_cube_is_six_planes_twelve_cylinders_and_eight_sphere_octants() {
    let size = [40.0, 40.0, 40.0];
    let radius = 4.0;
    let cube = cuboid(size);
    let rounded = blend(
        &cube,
        box_edges(&cube, size),
        EdgeFinishKind::Fillet,
        radius,
    );

    let counts = rounded.counts();
    assert_eq!(
        (counts.faces, counts.edges, counts.vertices),
        (26, 48, 24),
        "six planes, twelve bands and eight corners"
    );
    assert_eq!(
        carrier_kinds(&rounded),
        BTreeMap::from([("plane", 6), ("cylinder", 12), ("sphere", 8)])
    );
    close(
        rounded.measures().volume,
        rounded_box_volume(size, radius),
        "volume",
    );
    close(
        rounded.measures().surface_area,
        rounded_box_area(size, radius),
        "area",
    );
}

#[test]
fn a_filleted_box_with_unequal_sides_measures_its_closed_form() {
    let size = [60.0, 34.0, 18.0];
    let radius = 3.5;
    let body = cuboid(size);
    let rounded = blend(
        &body,
        box_edges(&body, size),
        EdgeFinishKind::Fillet,
        radius,
    );

    assert_eq!(
        carrier_kinds(&rounded),
        BTreeMap::from([("plane", 6), ("cylinder", 12), ("sphere", 8)])
    );
    close(
        rounded.measures().volume,
        rounded_box_volume(size, radius),
        "volume",
    );
    close(
        rounded.measures().surface_area,
        rounded_box_area(size, radius),
        "area",
    );
    let bounds = rounded.measures().bounds.expect("a solid has bounds");
    close(bounds.max.x - bounds.min.x, size[0], "x extent");
    close(bounds.max.y - bounds.min.y, size[1], "y extent");
    close(bounds.max.z - bounds.min.z, size[2], "z extent");
}

#[test]
fn a_chamfered_cube_is_twenty_six_planes() {
    let size = [40.0, 40.0, 40.0];
    let distance = 4.0;
    let cube = cuboid(size);
    let bevelled = blend(
        &cube,
        box_edges(&cube, size),
        EdgeFinishKind::Chamfer,
        distance,
    );

    let counts = bevelled.counts();
    assert_eq!(
        (counts.faces, counts.edges, counts.vertices),
        (26, 48, 24),
        "six faces, twelve bevels and eight corner triangles"
    );
    assert_eq!(carrier_kinds(&bevelled), BTreeMap::from([("plane", 26)]));
    close(
        bevelled.measures().volume,
        chamfered_box_volume(size, distance),
        "volume",
    );
    close(
        bevelled.measures().surface_area,
        chamfered_box_area(size, distance),
        "area",
    );
}

#[test]
fn the_three_edges_of_one_corner_round_and_the_bands_run_out_flat() {
    let size = [40.0, 30.0, 24.0];
    let radius = 4.0;
    let box_body = cuboid(size);
    let corner = Point3::new(0.0, 0.0, 0.0);
    let selection = corner_edges(&box_body, corner);
    assert_eq!(selection.len(), 3, "the three edges of one corner");

    let rounded = blend(&box_body, selection, EdgeFinishKind::Fillet, radius);
    let counts = rounded.counts();
    assert_eq!(
        (counts.faces, counts.edges, counts.vertices),
        (10, 21, 13),
        "six faces plus three bands and one octant; each band runs out into \
         the face across its far end without adding one"
    );
    assert_eq!(
        carrier_kinds(&rounded),
        BTreeMap::from([("plane", 6), ("cylinder", 3), ("sphere", 1)])
    );
    let solid = size[0] * size[1] * size[2];
    close(
        rounded.measures().volume,
        solid - corner_fillet_cut(size, radius),
        "volume",
    );
    assert!(
        rounded.measures().volume < solid,
        "a fillet removes material"
    );
    let bounds = rounded.measures().bounds.expect("a solid has bounds");
    close(bounds.max.x - bounds.min.x, size[0], "x extent");
    close(bounds.max.y - bounds.min.y, size[1], "y extent");
    close(bounds.max.z - bounds.min.z, size[2], "z extent");
}

#[test]
fn the_three_edges_of_one_corner_bevel_into_a_triangle() {
    let size = [40.0, 30.0, 24.0];
    let distance = 4.0;
    let box_body = cuboid(size);
    let selection = corner_edges(&box_body, Point3::new(0.0, 0.0, 0.0));
    assert_eq!(selection.len(), 3);

    let bevelled = blend(&box_body, selection, EdgeFinishKind::Chamfer, distance);
    let counts = bevelled.counts();
    assert_eq!(
        (counts.faces, counts.edges, counts.vertices),
        (10, 21, 13),
        "six faces plus three bevels and one corner triangle"
    );
    assert_eq!(carrier_kinds(&bevelled), BTreeMap::from([("plane", 10)]));
    let solid = size[0] * size[1] * size[2];
    close(
        bevelled.measures().volume,
        solid - corner_chamfer_cut(size, distance),
        "volume",
    );
    assert!(
        bevelled.measures().volume < solid,
        "a chamfer removes material"
    );
}

#[test]
fn two_opposite_corners_blend_together_and_one_after_the_other_alike() {
    let size = [40.0, 30.0, 24.0];
    let radius = 3.0;
    let near = Point3::new(0.0, 0.0, 0.0);
    let far = Point3::new(size[0], size[1], size[2]);

    // Both corners in one feature.
    let body = cuboid(size);
    let mut selection = corner_edges(&body, near);
    selection.extend(corner_edges(&body, far));
    assert_eq!(selection.len(), 6, "six edges, two corners, six run-outs");
    let together = blend(&body, selection, EdgeFinishKind::Fillet, radius);
    assert_eq!(
        (
            together.counts().faces,
            together.counts().edges,
            together.counts().vertices
        ),
        (14, 30, 18)
    );
    assert_eq!(
        carrier_kinds(&together),
        BTreeMap::from([("plane", 6), ("cylinder", 6), ("sphere", 2)])
    );

    // The same two corners, one feature each: the second still blends on a
    // body the first already rounded.
    let body = cuboid(size);
    let first = blend(
        &body,
        corner_edges(&body, near),
        EdgeFinishKind::Fillet,
        radius,
    );
    let second = blend(
        &first,
        corner_edges(&first, far),
        EdgeFinishKind::Fillet,
        radius,
    );

    let solid = size[0] * size[1] * size[2];
    close(
        together.measures().volume,
        solid - 2.0 * corner_fillet_cut(size, radius),
        "two corners take twice one corner's material",
    );
    close(
        second.measures().volume,
        together.measures().volume,
        "either way removes the same material",
    );
    assert_eq!(second.counts().faces, together.counts().faces);
    assert_eq!(second.counts().edges, together.counts().edges);
    assert_eq!(second.counts().vertices, together.counts().vertices);
}

#[test]
fn a_second_corner_bevels_on_a_body_an_earlier_bevel_already_cut() {
    let size = [40.0, 30.0, 24.0];
    let distance = 3.0;
    let body = cuboid(size);
    let first = blend(
        &body,
        corner_edges(&body, Point3::new(0.0, 0.0, 0.0)),
        EdgeFinishKind::Chamfer,
        distance,
    );
    let second = blend(
        &first,
        corner_edges(&first, Point3::new(size[0], size[1], size[2])),
        EdgeFinishKind::Chamfer,
        distance,
    );

    assert_eq!(carrier_kinds(&second), BTreeMap::from([("plane", 14)]));
    close(
        second.measures().volume,
        size[0] * size[1] * size[2] - 2.0 * corner_chamfer_cut(size, distance),
        "volume",
    );
}

#[test]
fn a_plate_with_two_through_holes_keeps_them_while_its_box_edges_round() {
    let size = [60.0, 40.0, 10.0];
    let radius = 3.0;
    let bore = 4.0;
    let plate = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: rectangle(size[0], size[1]),
                holes: vec![circle((15.0, 20.0), bore), circle((45.0, 20.0), bore)],
            }],
        },
        size[2],
    );
    let selection = box_edges(&plate, size);
    assert_eq!(selection.len(), 12, "the twelve edges of the outer box");
    let rounded = blend(&plate, selection, EdgeFinishKind::Fillet, radius);

    assert_eq!(
        carrier_kinds(&rounded),
        BTreeMap::from([("plane", 6), ("cylinder", 12 + 4), ("sphere", 8)]),
        "the two bores pass through untouched, as two half-cylinders each"
    );
    close(
        rounded.measures().volume,
        rounded_box_volume(size, radius) - 2.0 * PI * bore * bore * size[2],
        "volume",
    );
}

#[test]
fn a_slot_plate_keeps_its_slot_while_its_box_edges_round() {
    let size = [60.0, 40.0, 10.0];
    let radius = 3.0;
    let (length, half_width) = (20.0, 5.0);
    let plate = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: rectangle(size[0], size[1]),
                holes: vec![slot((30.0, 20.0), length, half_width)],
            }],
        },
        size[2],
    );
    let selection = box_edges(&plate, size);
    assert_eq!(selection.len(), 12);
    let rounded = blend(&plate, selection, EdgeFinishKind::Fillet, radius);

    assert_eq!(
        carrier_kinds(&rounded),
        BTreeMap::from([("plane", 6 + 2), ("cylinder", 12 + 2), ("sphere", 8)]),
        "the slot keeps its two flats and two ends"
    );
    let slot_area = length * 2.0 * half_width + PI * half_width * half_width;
    close(
        rounded.measures().volume,
        rounded_box_volume(size, radius) - slot_area * size[2],
        "volume",
    );
}

// ---------------------------------------------------------------------------
// What it refuses
// ---------------------------------------------------------------------------

#[test]
fn a_corner_left_half_selected_meets_along_its_seam() {
    // Two adjacent edges of the top face: the corner they share has two of
    // its three edges chosen. This used to leave the rung nothing to draw and
    // the faceted tier answered with a caveat. ADR 0043 derived what the two
    // bands actually do — they stop against each other along one seam, and the
    // edge left sharp starts a blend's width further along — so the exact rung
    // owns it and publishes no approximation.
    let size = [40.0, 40.0, 30.0];
    let top = |start: Point3, end: Point3| {
        (start.z - size[2]).abs() < 1.0e-9 && (end.z - size[2]).abs() < 1.0e-9
    };
    let box_body = cuboid(size);
    let selection = edges_where(&box_body, |start, end| {
        top(start, end)
            && ((start.y.abs() < 1.0e-9 && end.y.abs() < 1.0e-9)
                || (start.x.abs() < 1.0e-9 && end.x.abs() < 1.0e-9))
    });
    assert_eq!(selection.len(), 2, "two top edges sharing one corner");

    let outcome =
        finish(&box_body, selection, EdgeFinishKind::Fillet, 4.0).expect("the ladder answers");
    assert_eq!(
        outcome.report.rung.as_deref(),
        Some("edge-finish/vertex-blend"),
        "a mitred corner is the exact rung's to close"
    );
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact seam carries no caveat: {:?}",
        outcome.report.warnings
    );
    close(
        outcome.snapshot.measures().volume,
        size[0] * size[1] * size[2] - two_edge_fillet_removed([size[0], size[1]], 4.0),
        "both bands and the corner they share, removed once",
    );
}

#[test]
fn a_corner_blend_that_would_reach_a_bore_is_refused_by_name() {
    // The bore sits 8 from the corner's own faces; a radius of 12 would run
    // the inset boundary of the top face straight through it.
    let size = [40.0, 40.0, 20.0];
    let plate = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: rectangle(size[0], size[1]),
                holes: vec![circle((12.0, 12.0), 4.0)],
            }],
        },
        size[2],
    );
    let selection = corner_edges(&plate, Point3::new(0.0, 0.0, 0.0));
    let (code, message) = refusal(&plate, selection, EdgeFinishKind::Fillet, 12.0);
    assert_eq!(code, "VERTEX_BLEND_DISTANCE_INVALID");
    assert!(message.contains("hole or slot"), "{message}");
}

// ---------------------------------------------------------------------------
// Two edges of a corner (ADR 0043)
// ---------------------------------------------------------------------------

/// The two edges of a cube corner that share one face, leaving the third sharp.
///
/// At the origin corner of a cuboid the three edges run along the axes. Taking
/// the two that lie in `z = 0` leaves the vertical one untouched, which is the
/// across-and-down selection a user makes without thinking about it.
fn two_edges_of_the_origin_corner(snapshot: &Snapshot) -> Vec<EntityRef> {
    edges_where(snapshot, |start, end| {
        let flat = start.z.abs() < 1.0e-9 && end.z.abs() < 1.0e-9;
        let touches_origin = |point: Point3| point.x.abs() < 1.0e-9 && point.y.abs() < 1.0e-9;
        flat && (touches_origin(start) || touches_origin(end))
    })
}

/// What a chamfer of two edges meeting at a corner must remove.
///
/// Each edge loses a triangular prism of section `½d²`. Over the corner cube
/// `[0, d]³` both prisms claim the same material, and that region — the points
/// under both bevels, `y + z ≤ d` and `x + z ≤ d` — has volume
/// `∫₀ᵈ (d − z)² dz = d³/3`. Numeric integration agrees to ten digits.
fn two_edge_chamfer_removed(lengths: [f64; 2], distance: f64) -> f64 {
    0.5 * distance * distance * (lengths[0] + lengths[1]) - distance.powi(3) / 3.0
}

/// What a fillet of the same two edges must remove.
///
/// Each edge loses `r²(1 − π/4)` of section. The shared corner is the part of
/// `[0, r]³` outside both cylinders; with `w(z) = r − √(r² − (z − r)²)` its
/// section at height `z` is `w(z)²`, and
/// `∫₀ʳ w(z)² dz = r³(2 − ⅓ − π/2) = r³(5/3 − π/2)`. Numeric integration
/// agrees to ten digits.
fn two_edge_fillet_removed(lengths: [f64; 2], radius: f64) -> f64 {
    radius * radius * (1.0 - PI / 4.0) * (lengths[0] + lengths[1])
        - radius.powi(3) * (5.0 / 3.0 - PI / 2.0)
}

/// Two edges of a corner bevel, meeting along the line their bevel planes
/// share.
///
/// ADR 0043: the seam runs from `(d, d, 0)`, the shared face's new corner, to
/// `(0, 0, d)`, the new end of the edge left sharp. No patch closes anything —
/// the bands simply stop against each other.
#[test]
fn two_edges_of_a_corner_chamfer_to_their_shared_seam() {
    let size = [40.0, 40.0, 40.0];
    let distance = 4.0;
    let cube = cuboid(size);
    let selection = two_edges_of_the_origin_corner(&cube);
    assert_eq!(selection.len(), 2, "two edges of the corner, not three");

    let bevelled = blend(&cube, selection, EdgeFinishKind::Chamfer, distance);
    close(
        bevelled.measures().volume,
        size[0] * size[1] * size[2] - two_edge_chamfer_removed([size[0], size[1]], distance),
        "a two-edge chamfer removes both prisms and the corner they share once",
    );
}

/// And round, meeting along the ellipse two equal crossing cylinders share.
#[test]
fn two_edges_of_a_corner_fillet_to_their_shared_seam() {
    let size = [40.0, 40.0, 40.0];
    let radius = 4.0;
    let cube = cuboid(size);
    let selection = two_edges_of_the_origin_corner(&cube);
    assert_eq!(selection.len(), 2);

    let rounded = blend(&cube, selection, EdgeFinishKind::Fillet, radius);
    close(
        rounded.measures().volume,
        size[0] * size[1] * size[2] - two_edge_fillet_removed([size[0], size[1]], radius),
        "a two-edge fillet removes both bands and the corner they share once",
    );
}

/// The closed forms above, checked against the integrals they came from, so a
/// later construction is measured against arithmetic rather than against a
/// number somebody once read off a screen.
#[test]
fn the_two_edge_corner_volumes_agree_with_their_integrals() {
    const STEPS: usize = 200_000;
    let radius = 1.7_f64;

    let mut chamfer = 0.0;
    let mut fillet = 0.0;
    for step in 0..STEPS {
        let height = radius * (step as f64 + 0.5) / STEPS as f64;
        chamfer += (radius - height).powi(2);
        let inset = radius
            - (radius * radius - (height - radius).powi(2))
                .max(0.0)
                .sqrt();
        fillet += inset * inset;
    }
    chamfer *= radius / STEPS as f64;
    fillet *= radius / STEPS as f64;

    // The midpoint rule meets a square root at the top of the round corner's
    // range, so it converges as a power rather than exponentially. Six digits
    // is far more than enough to tell these closed forms from a wrong one.
    let agrees = |numeric: f64, closed: f64, what: &str| {
        assert!(
            (numeric - closed).abs() <= 1.0e-6 * closed.abs().max(1.0),
            "{what}: {numeric} is not {closed}"
        );
    };
    agrees(chamfer, radius.powi(3) / 3.0, "the shared bevel corner");
    agrees(
        fillet,
        radius.powi(3) * (5.0 / 3.0 - PI / 2.0),
        "the shared round corner",
    );
}

// ---------------------------------------------------------------------------
// Composing with the other exact rungs, and publishing
// ---------------------------------------------------------------------------

#[test]
fn a_corner_that_already_carries_a_blend_of_another_size_is_refused_by_name() {
    let size = [40.0, 40.0, 40.0];
    let cube = cuboid(size);
    // The four vertical edges first, exactly, through the six-plane rung.
    let vertical = edges_where(&cube, |start, end| (start.z - end.z).abs() > 1.0);
    let rounded = finish(&cube, vertical, EdgeFinishKind::Fillet, 4.0)
        .expect("vertical edges round exactly")
        .snapshot;
    // Now a top edge at a different size: its corners meet the cylinders the
    // first feature left.
    let top = edges_where(&rounded, |start, end| {
        (start.z - size[2]).abs() < 1.0e-9
            && (end.z - size[2]).abs() < 1.0e-9
            && (start.x - end.x).abs() > 1.0
            && (start.y - end.y).abs() < 1.0e-9
    });
    assert_eq!(top.len(), 2, "the two straight top edges along x");

    let (code, message) = refusal(&rounded, top, EdgeFinishKind::Fillet, 6.0);
    assert_eq!(code, "VERTEX_BLEND_RADIUS_MISMATCH");
    assert!(message.contains("already carries a blend"), "{message}");
}

#[test]
fn blending_beside_an_earlier_blend_names_the_curved_corner() {
    let size = [40.0, 40.0, 40.0];
    let cube = cuboid(size);
    let vertical = edges_where(&cube, |start, end| (start.z - end.z).abs() > 1.0);
    let rounded = finish(&cube, vertical, EdgeFinishKind::Fillet, 4.0)
        .expect("vertical edges round exactly")
        .snapshot;
    let along_x = edges_where(&rounded, |start, end| {
        (start.x - end.x).abs() > 1.0
            && (start.y - end.y).abs() < 1.0e-9
            && (start.z - end.z).abs() < 1.0e-9
    });
    assert_eq!(along_x.len(), 4, "two top and two bottom straight runs");

    // The faceted tier is still given its turn — a corner that already carries
    // a blend is this rung's vocabulary talking, not a fact about the body —
    // but when it cannot answer either, this rung's sentence is the one the
    // caller reads, rather than the ladder's general one.
    let (code, message) = refusal(&rounded, along_x, EdgeFinishKind::Fillet, 4.0);
    assert_eq!(code, "VERTEX_BLEND_CORNER_CURVED");
    assert!(message.contains("meets a curved face"), "{message}");
}

#[test]
fn a_refusal_on_a_blended_body_comes_back_before_the_user_notices() {
    // The faceted tier rebuilds a body from its tessellation, and an exact
    // corner blend tessellates into sixteen thousand triangles. Letting it
    // try anyway cost twelve and a half seconds in release and a minute and
    // a half unoptimised — to answer "no". It declines a body that large up
    // front now, and the exact rung's own sentence is published instead.
    let size = [2.0, 3.0, 4.0];
    let body = cuboid(size);
    let corner = corner_edges(&body, Point3::new(0.0, 0.0, 0.0));
    assert_eq!(corner.len(), 3);
    let blended = finish(&body, corner, EdgeFinishKind::Fillet, 0.25)
        .expect("the corner blends exactly")
        .snapshot;
    assert_eq!(
        blended.counts().faces,
        10,
        "six planes, three bands, one octant"
    );

    // The horizontal runs beside the blend. Their corners carry it, and a
    // corner that already carries a blend is still beyond this rung, so no
    // rung can answer and the ladder runs all the way to the faceted tier.
    //
    // A half-chosen corner used to be the unanswerable selection here. It is
    // answerable now (ADR 0043), so the guard needs one that is not.
    let chain = edges_where(&blended, |start, end| {
        (start.z - end.z).abs() < 1.0e-9 && (start.x - end.x).abs() > 0.5
    });
    assert!(
        chain.len() >= 2,
        "the straight runs along x, above and below the blend"
    );

    let started = Instant::now();
    let (code, _) = refusal(&blended, chain, EdgeFinishKind::Chamfer, 0.25);
    let elapsed = started.elapsed();
    assert!(code.starts_with("VERTEX_BLEND_"), "{code}");
    // Release only, and generously: the figure this guards is measured at
    // 27 ms against the 12.5 s it replaced, so a budget of one second still
    // catches the regression without timing a loaded runner. An unoptimised
    // build is an order of magnitude slower for reasons that have nothing to
    // do with the work being done.
    if cfg!(debug_assertions) {
        eprintln!("refused in {elapsed:?} (unoptimised; not gated)");
        return;
    }
    assert!(
        elapsed < Duration::from_secs(1),
        "refusing a finish on a blended body took {elapsed:?}; the faceted tier is \
         rebuilding a body it should have declined"
    );
}

#[test]
fn a_selection_that_mixes_rims_with_straight_edges_says_how_to_split_it() {
    let size = [60.0, 40.0, 10.0];
    let plate = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: rectangle(size[0], size[1]),
                holes: vec![circle((30.0, 20.0), 6.0)],
            }],
        },
        size[2],
    );
    let mut selection = box_edges(&plate, size);
    selection.extend(edges_where(&plate, |start, end| {
        (start.z - size[2]).abs() < 1.0e-9
            && (end.z - size[2]).abs() < 1.0e-9
            && (start.x - 24.0).abs() < 1.0e-9
    }));
    assert!(selection.len() > 12, "box edges and one hole rim together");

    let (code, message) = refusal(&plate, selection, EdgeFinishKind::Fillet, 3.0);
    assert_eq!(code, "VERTEX_BLEND_MIXED_SELECTION");
    assert!(message.contains("Either order works"), "{message}");
}

#[test]
fn a_blend_that_would_not_fit_is_refused_rather_than_approximated() {
    let size = [40.0, 40.0, 6.0];
    let cube = cuboid(size);
    // Half the thinnest side: the two set-backs on the thin walls meet.
    let (code, _) = refusal(
        &cube,
        box_edges(&cube, size),
        EdgeFinishKind::Fillet,
        3.0 + 1.0e-6,
    );
    assert_eq!(code, "VERTEX_BLEND_DISTANCE_INVALID");
}

#[test]
fn a_chamfer_runs_out_on_a_face_that_is_not_square_to_it_and_a_fillet_does_not() {
    // A wedge: the slanted edge of each cap ends against a wall that is not
    // square to it. A chamfer's end there is the straight line where its
    // bevel cuts that wall, whatever the angle; a fillet's would be an
    // ellipse, which this vocabulary does not carry, so only the chamfer is
    // this rung's to own.
    let height = 30.0;
    let corners = [(0.0, 0.0), (40.0, 0.0), (40.0, 30.0), (0.0, 10.0)];
    let wedge = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: (0..4)
                        .map(|index| PlanarCurve2::Line {
                            start: Point2::new(corners[index].0, corners[index].1),
                            end: Point2::new(
                                corners[(index + 1) % 4].0,
                                corners[(index + 1) % 4].1,
                            ),
                        })
                        .collect(),
                },
                holes: Vec::new(),
            }],
        },
        height,
    );
    let slanted = edges_where(&wedge, |start, end| {
        (start.z - end.z).abs() < 1.0e-9
            && (start.x - end.x).abs() > 1.0
            && (start.y - end.y).abs() > 1.0
            && (start.z - height).abs() < 1.0e-9
    });
    assert_eq!(slanted.len(), 1, "the slanted edge of the top cap");

    let bevelled = blend(&wedge, slanted.clone(), EdgeFinishKind::Chamfer, 4.0);
    assert_eq!(carrier_kinds(&bevelled), BTreeMap::from([("plane", 7)]));
    assert!(bevelled.measures().volume < wedge.measures().volume);

    let rounded =
        finish(&wedge, slanted, EdgeFinishKind::Fillet, 4.0).expect("some rung answers the fillet");
    assert_ne!(
        rounded.report.rung.as_deref(),
        Some("edge-finish/vertex-blend"),
        "a fillet ending on a face that is not square to its edge is not exact here"
    );
}

#[test]
fn the_rungs_before_this_one_keep_the_work_they_already_owned() {
    // A cuboid's four vertical edges belong to the six-plane rung at the top
    // of the ladder, and stay its even though this rung could now run each
    // band out into the caps.
    let size = [40.0, 40.0, 30.0];
    let box_body = cuboid(size);
    let vertical = edges_where(&box_body, |start, end| (start.z - end.z).abs() > 1.0);
    assert_eq!(vertical.len(), 4);
    let outcome =
        finish(&box_body, vertical, EdgeFinishKind::Fillet, 4.0).expect("the first rung answers");
    assert_eq!(outcome.report.rung.as_deref(), Some("edge-finish/analytic"));
    assert!(outcome.report.warnings.is_empty());

    // A hexagonal prism's vertical edges are the prism rung's.
    let corners: Vec<(f64, f64)> = (0..6)
        .map(|step| {
            let angle = f64::from(step) * PI / 3.0;
            (20.0 * angle.cos(), 20.0 * angle.sin())
        })
        .collect();
    let prism = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 {
                    curves: (0..6)
                        .map(|index| PlanarCurve2::Line {
                            start: Point2::new(corners[index].0, corners[index].1),
                            end: Point2::new(
                                corners[(index + 1) % 6].0,
                                corners[(index + 1) % 6].1,
                            ),
                        })
                        .collect(),
                },
                holes: Vec::new(),
            }],
        },
        size[2],
    );
    let vertical = edges_where(&prism, |start, end| (start.z - end.z).abs() > 1.0);
    assert_eq!(vertical.len(), 6);
    let outcome =
        finish(&prism, vertical, EdgeFinishKind::Fillet, 4.0).expect("the prism rung answers");
    assert_eq!(outcome.report.rung.as_deref(), Some("edge-finish/prism"));
}

#[test]
fn a_corner_blend_leaves_a_bore_that_passes_beside_it_alone() {
    // The corner rung copies every face it does not inset, carrier and
    // p-curves intact, so a bore drilled beforehand measures the same after.
    let size = [60.0, 40.0, 20.0];
    let (radius, bore) = (4.0, 5.0);
    let plate = extruded(
        PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: rectangle(size[0], size[1]),
                holes: vec![circle((30.0, 20.0), bore)],
            }],
        },
        size[2],
    );
    let rounded = blend(
        &plate,
        corner_edges(&plate, Point3::new(0.0, 0.0, 0.0)),
        EdgeFinishKind::Fillet,
        radius,
    );

    assert_eq!(
        carrier_kinds(&rounded),
        BTreeMap::from([("plane", 6), ("cylinder", 3 + 2), ("sphere", 1)]),
        "the bore passes through untouched, as two half-cylinders"
    );
    close(
        rounded.measures().volume,
        size[0] * size[1] * size[2] - PI * bore * bore * size[2] - corner_fillet_cut(size, radius),
        "volume",
    );
}

#[test]
fn the_rounded_cube_exports_as_a_step_brep_and_reads_back() {
    let size = [40.0, 40.0, 40.0];
    let cube = cuboid(size);
    let rounded = blend(&cube, box_edges(&cube, size), EdgeFinishKind::Fillet, 4.0);
    let step = export_step(&rounded, "rounded_cube").expect("the exact body exports");

    assert!(step.starts_with("ISO-10303-21;"), "a STEP part 21 file");
    assert!(step.ends_with("END-ISO-10303-21;\n"));
    let count = |kind: &str| step.matches(kind).count();
    assert_eq!(count("SPHERICAL_SURFACE("), 8, "eight corner octants");
    assert_eq!(count("CYLINDRICAL_SURFACE("), 12, "twelve bands");
    assert_eq!(count("ADVANCED_FACE("), 26);
    assert_eq!(count("CLOSED_SHELL("), 1);
}

#[test]
fn the_report_names_the_corner_blend_rung_and_the_exact_tier() {
    let source = include_str!("../examples/filleted_cube.art");
    let commands = compile_script(source, &BTreeMap::new()).expect("the example compiles");
    let mut session = Session::new();
    let mut last = None;
    for command in commands {
        last = Some(
            session
                .execute(command, &CancellationToken::default())
                .expect("the example runs"),
        );
    }
    let result = last.expect("the example has steps");
    assert_eq!(result.rung.as_deref(), Some("edge-finish/vertex-blend"));
    assert_eq!(result.tier, Tier::Exact);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.topology.faces, 26);
}

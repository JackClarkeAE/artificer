//! A fillet around the rim of a square hole turns four sharp reflex corners.
//! Each is the Steinmetz seam of two equal cylinders — an ellipse — so the
//! result is exact, and its volume is pinned to a closed form: the straight
//! runs remove `(1 − π/4)·f²` per unit length and each corner `f³·(5/3 − π/2)`.

use std::collections::BTreeSet;
use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const SIDE: f64 = 40.0;
const HEIGHT: f64 = 10.0;
const HOLE: f64 = 10.0;
const FILLET: f64 = 2.0;

fn polygon(points: &[(f64, f64)]) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: (0..points.len())
            .map(|index| {
                let (x, y) = points[index];
                let (nx, ny) = points[(index + 1) % points.len()];
                PlanarCurve2::Line {
                    start: Point2::new(x, y),
                    end: Point2::new(nx, ny),
                }
            })
            .collect(),
    }
}

fn execute(snapshot: &Snapshot, label: &str, command: KernelCommand) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    let outcome = NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label} should build: {error:?}"));
    assert!(
        outcome.report.warnings.is_empty(),
        "{label} is exact: {:?}",
        outcome.report.warnings
    );
    outcome.snapshot
}

fn plate_with_square_hole() -> Snapshot {
    let low = (SIDE - HOLE) / 2.0;
    let high = (SIDE + HOLE) / 2.0;
    execute(
        &NativeKernel::empty(),
        "plate",
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: polygon(&[(0.0, 0.0), (SIDE, 0.0), (SIDE, SIDE), (0.0, SIDE)]),
                    holes: vec![polygon(&[
                        (low, low),
                        (high, low),
                        (high, high),
                        (low, high),
                    ])],
                }],
            },
            distance: HEIGHT,
        },
    )
}

/// The four edges of the hole on the top cap.
fn hole_rim(snapshot: &Snapshot) -> Vec<EntityRef> {
    let low = (SIDE - HOLE) / 2.0;
    let high = (SIDE + HOLE) / 2.0;
    let scene = NativeKernel::debug_scene(snapshot);
    let rim = scene
        .edges
        .iter()
        .filter(|edge| {
            edge.endpoints.iter().all(|point| {
                (point.z - HEIGHT).abs() < 1.0e-9
                    && point.x >= low - 1.0e-9
                    && point.x <= high + 1.0e-9
                    && point.y >= low - 1.0e-9
                    && point.y <= high + 1.0e-9
            })
        })
        .map(|edge| edge.source_edge)
        .collect::<BTreeSet<_>>();
    assert_eq!(rim.len(), 4, "the square hole has four rim edges");
    rim.into_iter().collect()
}

#[test]
fn a_fillet_round_a_square_hole_is_exact_with_elliptical_mitres() {
    let plate = plate_with_square_hole();
    let before = plate.measures().volume;
    assert!((before - (SIDE * SIDE * HEIGHT - HOLE * HOLE * HEIGHT)).abs() < 1.0e-9);

    let filleted = execute(
        &plate,
        "hole-rim-fillet",
        KernelCommand::FinishEdges {
            target_edges: hole_rim(&plate),
            kind: EdgeFinishKind::Fillet,
            distance: FILLET,
        },
    );
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);
    // Two caps, four outer walls, four hole walls, four quarter-cylinder
    // bands; the mitres add no faces.
    assert_eq!(filleted.counts().faces, 14);

    let straight = 4.0 * HOLE * (1.0 - PI / 4.0) * FILLET * FILLET;
    let corners = 4.0 * FILLET.powi(3) * (5.0 / 3.0 - PI / 2.0);
    let expected = before - straight - corners;
    let after = filleted.measures().volume;
    assert!(
        ((after - expected) / expected).abs() < 1.0e-9,
        "volume {after} should be {expected}"
    );

    // The rails are tangent; the mitre seams between the bands are creases
    // that run from the hole corner at the band's base out and up to the
    // mitre on the cap, which lies a fillet radius into the material.
    let scene = NativeKernel::debug_scene(&filleted);
    let low = (SIDE - HOLE) / 2.0;
    let seam_chords = scene
        .edges
        .iter()
        .filter(|edge| {
            !edge.is_smooth
                && !edge.is_tangent
                && edge.endpoints.iter().all(|point| {
                    point.z > HEIGHT - FILLET - 1.0e-9
                        && point.z < HEIGHT + 1.0e-9
                        && (point.x - point.y).abs() < 1.0e-9
                        && point.x >= low - FILLET - 1.0e-9
                        && point.x <= low + 1.0e-9
                })
        })
        .count();
    assert!(
        seam_chords >= 2,
        "the mitre at the (low, low) corner is drawn"
    );
    assert!(scene.edges.iter().any(|edge| edge.is_tangent));
    // The display covers the bands: every band strip has area.
    assert!(scene.triangles.len() > 12);
}

#[test]
fn a_chamfer_round_the_same_hole_still_mitres_on_straight_lines() {
    let plate = plate_with_square_hole();
    let chamfered = execute(
        &plate,
        "hole-rim-chamfer",
        KernelCommand::FinishEdges {
            target_edges: hole_rim(&plate),
            kind: EdgeFinishKind::Chamfer,
            distance: FILLET,
        },
    );
    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);
    // A 45° slant removes half a square per unit length, and the corner
    // overlaps add a third of a cube each.
    let before = plate.measures().volume;
    let expected = before - 4.0 * HOLE * FILLET * FILLET / 2.0 - 4.0 * FILLET.powi(3) / 3.0;
    let after = chamfered.measures().volume;
    assert!(
        ((after - expected) / expected).abs() < 1.0e-9,
        "volume {after} should be {expected}"
    );
}

#[test]
fn an_l_shaped_rim_fillets_its_reflex_corner_as_well_as_its_convex_ones() {
    let block = execute(
        &NativeKernel::empty(),
        "l-block",
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: polygon(&[
                        (0.0, 0.0),
                        (30.0, 0.0),
                        (30.0, 12.0),
                        (12.0, 12.0),
                        (12.0, 30.0),
                        (0.0, 30.0),
                    ]),
                    holes: vec![],
                }],
            },
            distance: HEIGHT,
        },
    );
    let scene = NativeKernel::debug_scene(&block);
    let rim = scene
        .edges
        .iter()
        .filter(|edge| {
            edge.endpoints
                .iter()
                .all(|point| (point.z - HEIGHT).abs() < 1.0e-9)
        })
        .map(|edge| edge.source_edge)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(rim.len(), 6);
    let filleted = execute(
        &block,
        "l-rim-fillet",
        KernelCommand::FinishEdges {
            target_edges: rim,
            kind: EdgeFinishKind::Fillet,
            distance: FILLET,
        },
    );
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);
    // Two caps, six walls, six bands, and a sphere plus a ledge at each of
    // the five convex corners; the reflex corner adds nothing.
    assert_eq!(filleted.counts().faces, 2 + 6 + 6 + 5 * 2);
    assert!(filleted.measures().volume < block.measures().volume);
}

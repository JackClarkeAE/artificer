//! Stacked rim blends on revolved solids (ADR 0023 frontier, milestone C).
//!
//! A coaxial revolved solid is described by its closed (r, z) section, so a
//! rim blend is a planar corner operation on that section. Expectations here
//! are computed independently from the section by the classical integrals
//! `V = π ∮ r² dz` and `S = 2π ∮ r ds`, written separately from the kernel's
//! own per-face closed forms so the two derivations cannot share a mistake.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const RADIUS: f64 = 25.0;
const HEIGHT: f64 = 100.0;

/// One piece of a closed (r, z) section.
#[derive(Clone, Copy, Debug)]
enum Piece {
    Line {
        from: (f64, f64),
        to: (f64, f64),
    },
    /// Arc about `center` of radius `radius`, swept from `start` to `end`
    /// angles measured in the (r, z) plane.
    Arc {
        center: (f64, f64),
        radius: f64,
        start: f64,
        end: f64,
    },
}

/// `V = π ∮ r² dz` around the closed section.
fn section_volume(pieces: &[Piece]) -> f64 {
    let mut total = 0.0;
    for piece in pieces {
        total += match *piece {
            Piece::Line { from, to } => {
                let delta_r = to.0 - from.0;
                let delta_z = to.1 - from.1;
                delta_z
                    * from
                        .0
                        .mul_add(from.0, from.0.mul_add(delta_r, delta_r * delta_r / 3.0))
            }
            Piece::Arc {
                center,
                radius,
                start,
                end,
            } => {
                // ∫ (c_r + m cos a)² · m cos a da
                let term = |a: f64| {
                    center.0.mul_add(
                        center.0 * a.sin(),
                        2.0 * center.0 * radius * (a / 2.0 + (2.0 * a).sin() / 4.0),
                    ) + radius * radius * (a.sin() - a.sin().powi(3) / 3.0)
                };
                radius * (term(end) - term(start))
            }
        };
    }
    std::f64::consts::PI * total
}

/// `S = 2π ∮ r ds` over the revolved boundary.
fn section_area(pieces: &[Piece]) -> f64 {
    let mut total = 0.0;
    for piece in pieces {
        total += match *piece {
            Piece::Line { from, to } => {
                let length = (to.0 - from.0).hypot(to.1 - from.1);
                length * (from.0 + to.0) / 2.0
            }
            Piece::Arc {
                center,
                radius,
                start,
                end,
            } => {
                let (low, high) = if start <= end {
                    (start, end)
                } else {
                    (end, start)
                };
                radius
                    * center
                        .0
                        .mul_add(high - low, radius * (high.sin() - low.sin()))
            }
        };
    }
    2.0 * std::f64::consts::PI * total
}

fn cylinder() -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: RADIUS,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("stack-base"),
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
        .expect("cylinder should build")
        .snapshot
}

/// Every source edge whose sampled chords sit at height `z` and radius `r`.
fn rim_at(snapshot: &Snapshot, radius: f64, height: f64) -> EntityRef {
    let scene = NativeKernel::debug_scene(snapshot);
    scene
        .edges
        .iter()
        .find(|edge| {
            let [first, second] = edge.endpoints;
            let curved = scene
                .edges
                .iter()
                .filter(|candidate| candidate.source_edge == edge.source_edge)
                .count()
                > 1;
            curved
                && (first.z - height).abs() < 1.0e-9
                && (second.z - height).abs() < 1.0e-9
                && (first.x.hypot(first.y) - radius).abs() < 1.0e-6
        })
        .unwrap_or_else(|| panic!("no rim at radius {radius}, height {height}"))
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
fn a_double_chamfer_matches_its_section_integrals() {
    let base = cylinder();
    let setback = 10.0_f64;
    let chamfered = finish(
        &base,
        vec![rim_at(&base, RADIUS, 0.0), rim_at(&base, RADIUS, HEIGHT)],
        EdgeFinishKind::Chamfer,
        setback,
        "stack-chamfer",
    )
    .expect("both rim chamfers must commit");
    assert!(NativeKernel::validate(&chamfered, ValidationProfile::Solid).valid);

    let inner = RADIUS - setback;
    let section = [
        Piece::Line {
            from: (0.0, 0.0),
            to: (inner, 0.0),
        },
        Piece::Line {
            from: (inner, 0.0),
            to: (RADIUS, setback),
        },
        Piece::Line {
            from: (RADIUS, setback),
            to: (RADIUS, HEIGHT - setback),
        },
        Piece::Line {
            from: (RADIUS, HEIGHT - setback),
            to: (inner, HEIGHT),
        },
        Piece::Line {
            from: (inner, HEIGHT),
            to: (0.0, HEIGHT),
        },
    ];
    assert_close(
        chamfered.measures().volume,
        section_volume(&section),
        "double-chamfer volume",
    );
    assert_close(
        chamfered.measures().surface_area,
        section_area(&section),
        "double-chamfer area",
    );
}

#[test]
fn a_fillet_stacks_onto_the_sharp_rims_a_chamfer_creates() {
    let base = cylinder();
    let setback = 10.0_f64;
    let chamfered = finish(
        &base,
        vec![rim_at(&base, RADIUS, 0.0), rim_at(&base, RADIUS, HEIGHT)],
        EdgeFinishKind::Chamfer,
        setback,
        "stack-chamfer-first",
    )
    .expect("both rim chamfers must commit");

    // The chamfer leaves four sharp 135-degree rims. Fillet the two that sit
    // where the slant meets the cylindrical wall.
    let fillet = 3.0_f64;
    let inner = RADIUS - setback;
    let stacked = finish(
        &chamfered,
        vec![
            rim_at(&chamfered, RADIUS, setback),
            rim_at(&chamfered, RADIUS, HEIGHT - setback),
        ],
        EdgeFinishKind::Fillet,
        fillet,
        "stack-fillet",
    )
    .expect("a fillet on chamfer-created rims must commit");
    assert!(NativeKernel::validate(&stacked, ValidationProfile::Solid).valid);

    // Independent section: at a 135-degree interior corner the tangent trim is
    // f/tan(67.5 degrees), and the blend arc sweeps 45 degrees.
    let half_angle = 3.0 * std::f64::consts::FRAC_PI_8; // 67.5 degrees
    let trim = fillet / half_angle.tan();
    let slant = std::f64::consts::FRAC_1_SQRT_2;
    // Lower corner at (RADIUS, setback): incoming runs up the slant, outgoing
    // runs up the wall. The blend centre sits f inside the wall.
    let low_start = (RADIUS - trim * slant, setback - trim * slant);
    let low_end = (RADIUS, setback + trim);
    let low_center = (RADIUS - fillet, setback + trim);
    let high_start = (RADIUS, HEIGHT - setback - trim);
    let high_end = (RADIUS - trim * slant, HEIGHT - setback + trim * slant);
    let high_center = (RADIUS - fillet, HEIGHT - setback - trim);
    let section = [
        Piece::Line {
            from: (0.0, 0.0),
            to: (inner, 0.0),
        },
        Piece::Line {
            from: (inner, 0.0),
            to: low_start,
        },
        Piece::Arc {
            center: low_center,
            radius: fillet,
            start: (low_start.1 - low_center.1).atan2(low_start.0 - low_center.0),
            end: 0.0,
        },
        Piece::Line {
            from: low_end,
            to: high_start,
        },
        Piece::Arc {
            center: high_center,
            radius: fillet,
            start: 0.0,
            end: (high_end.1 - high_center.1).atan2(high_end.0 - high_center.0),
        },
        Piece::Line {
            from: high_end,
            to: (inner, HEIGHT),
        },
        Piece::Line {
            from: (inner, HEIGHT),
            to: (0.0, HEIGHT),
        },
    ];
    assert_close(
        stacked.measures().volume,
        section_volume(&section),
        "stacked fillet volume",
    );
    assert_close(
        stacked.measures().surface_area,
        section_area(&section),
        "stacked fillet area",
    );
}

#[test]
fn a_chamfer_stacks_onto_a_chamfer() {
    let base = cylinder();
    let first_setback = 10.0_f64;
    let chamfered = finish(
        &base,
        vec![rim_at(&base, RADIUS, HEIGHT)],
        EdgeFinishKind::Chamfer,
        first_setback,
        "double-chamfer-first",
    )
    .expect("the first chamfer must commit");

    let second_setback = 2.0_f64;
    let stacked = finish(
        &chamfered,
        vec![rim_at(&chamfered, RADIUS, HEIGHT - first_setback)],
        EdgeFinishKind::Chamfer,
        second_setback,
        "double-chamfer-second",
    )
    .expect("a chamfer on a chamfer-created rim must commit");
    assert!(NativeKernel::validate(&stacked, ValidationProfile::Solid).valid);

    let inner = RADIUS - first_setback;
    let slant = std::f64::consts::FRAC_1_SQRT_2;
    let corner = (RADIUS, HEIGHT - first_setback);
    let down = (corner.0, corner.1 - second_setback);
    let up = (
        corner.0 - second_setback * slant,
        corner.1 + second_setback * slant,
    );
    let section = [
        Piece::Line {
            from: (0.0, 0.0),
            to: (RADIUS, 0.0),
        },
        Piece::Line {
            from: (RADIUS, 0.0),
            to: down,
        },
        Piece::Line { from: down, to: up },
        Piece::Line {
            from: up,
            to: (inner, HEIGHT),
        },
        Piece::Line {
            from: (inner, HEIGHT),
            to: (0.0, HEIGHT),
        },
    ];
    assert_close(
        stacked.measures().volume,
        section_volume(&section),
        "chamfer-on-chamfer volume",
    );
}

#[test]
fn a_fillets_own_tangency_rim_is_smooth_and_refuses_reblending() {
    let base = cylinder();
    let fillet = 8.0_f64;
    let filleted = finish(
        &base,
        vec![rim_at(&base, RADIUS, HEIGHT)],
        EdgeFinishKind::Fillet,
        fillet,
        "smooth-first",
    )
    .expect("the first fillet must commit");

    // The tangency rim where the torus band meets the wall is C1: there is no
    // corner there, and the request must reject rather than invent one.
    let tangency = rim_at(&filleted, RADIUS, HEIGHT - fillet);
    let refused = finish(
        &filleted,
        vec![tangency],
        EdgeFinishKind::Fillet,
        1.0,
        "smooth-second",
    );
    assert!(
        refused.is_err(),
        "a tangency rim has no corner and must reject"
    );

    // The committed body is untouched by the refusal.
    assert!(NativeKernel::validate(&filleted, ValidationProfile::Solid).valid);
}

#[test]
fn rim_blend_seams_present_no_hard_meridian_edges() {
    let base = cylinder();
    let fillet = 10.0_f64;
    let blended = finish(
        &base,
        vec![rim_at(&base, RADIUS, HEIGHT), rim_at(&base, RADIUS, 0.0)],
        EdgeFinishKind::Fillet,
        fillet,
        "seam-presentation",
    )
    .expect("both rim fillets must commit");

    // The torus bands are split into half-faces along meridian seams. Those
    // seams are parameterization bookkeeping, not geometry: every edge that
    // still presents as hard must be a horizontal ring, never a vertical
    // seam line running up the blend.
    let scene = NativeKernel::debug_scene(&blended);
    for edge in scene.edges.iter().filter(|edge| !edge.is_smooth) {
        let [first, second] = edge.endpoints;
        assert!(
            (first.z - second.z).abs() <= 1.0e-6,
            "hard non-ring edge drawn from z {} to z {}",
            first.z,
            second.z
        );
    }
    // The cap tangency rings remain visible, selectable rails.
    for height in [fillet, HEIGHT - fillet] {
        assert!(
            scene.edges.iter().any(|edge| {
                let [first, second] = edge.endpoints;
                !edge.is_smooth
                    && (first.z - height).abs() <= 1.0e-6
                    && (second.z - height).abs() <= 1.0e-6
            }),
            "no hard tangency ring at height {height}"
        );
    }
}

//! Round cuts that meet round cuts of another radius, exactly (ADR 0047).
//!
//! Two cylinders that are not coaxial, not parallel, and not of equal radius
//! on crossing axes meet in a space quartic. The exact engine carries that
//! curve and closes the section it leaves on every face, so these cuts are
//! exact: no approximation warning, a valid solid, and a volume equal to one
//! computed here without the kernel — the block, less each cut, plus what the
//! cuts share, which is a chord of one circle times a chord of the other
//! integrated across them.
//!
//! The last fixture is the one that was reported: a slot sketched on a
//! sloped face and cut through the part across two bores. Its round ends are
//! cylinders leaning with the slope, each meeting a vertical bore of another
//! radius.

use std::f64::consts::PI;

use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityRef, ExecuteRequest, FaceExtrusionOperation,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, RotationQuaternion, SimilarityTransform3,
    ValidationProfile, Vector3,
};

const SIZE: f64 = 40.0;

/// `∫ f` over `[from, to]`, walked through `x = from + (to − from)(3t² − 2t³)`
/// so a square root vanishing at either end becomes smooth, then composite
/// Simpson. Independent of the kernel's own quadrature on purpose.
fn integrate(from: f64, to: f64, integrand: &dyn Fn(f64) -> f64) -> f64 {
    let panels = 20_000;
    let step = 1.0 / f64::from(panels);
    let span = to - from;
    (0..=panels)
        .map(|index| {
            let t = f64::from(index) * step;
            let x = span.mul_add(t * t * 2.0f64.mul_add(-t, 3.0), from);
            let rate = 6.0 * span * t * (1.0 - t);
            let weight = if index == 0 || index == panels {
                1.0
            } else if index % 2 == 1 {
                4.0
            } else {
                2.0
            };
            weight * integrand(x) * rate
        })
        .sum::<f64>()
        * step
        / 3.0
}

/// Half a chord of a circle of `radius` at `offset` from its centre.
fn half_chord(radius: f64, offset: f64) -> f64 {
    offset.mul_add(-offset, radius * radius).max(0.0).sqrt()
}

fn execute(snapshot: &Snapshot, command: KernelCommand, label: &str) -> ExecutionOutcome {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(label),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{label}: {error:?}"))
}

fn face_where(snapshot: &Snapshot, pick: &dyn Fn(Point3) -> bool) -> EntityRef {
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

fn region(curves: Vec<PlanarCurve2>) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves },
            holes: vec![],
        }],
    }
}

fn circle(radius: f64) -> Vec<PlanarCurve2> {
    vec![PlanarCurve2::Circle {
        center: Point2::new(0.0, 0.0),
        radius,
        direction: ArcDirection::CounterClockwise,
    }]
}

fn cut(face: EntityRef, frame: PlanarFrame3, curves: Vec<PlanarCurve2>) -> KernelCommand {
    KernelCommand::ExtrudeFacePlanarProfile {
        target_face: face,
        frame,
        profile: region(curves),
        distance: 1_000.0,
        operation: FaceExtrusionOperation::Cut,
    }
}

/// A valid solid, no caveat, and the volume given.
fn assert_exact(outcome: &ExecutionOutcome, expected: f64) {
    assert!(
        outcome.report.warnings.is_empty(),
        "the cut is exact and carries no caveat: {:?}",
        outcome.report.warnings
    );
    let validation = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
    let volume = outcome.snapshot.measures().volume;
    assert!(
        ((volume - expected) / expected).abs() < 1.0e-9,
        "volume {volume} should be {expected}"
    );
}

/// A 40 block with a bore of radius 8 down through it along `z` at the
/// centre, and a second bore of radius `radius` through it along `x`, its
/// axis at `(y, z) = (20 + beside, 20 + above)`.
fn crossed_block(radius: f64, beside: f64, above: f64, label: &str) -> ExecutionOutcome {
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(0.0, 0.0, 0.0),
            size_x: SIZE,
            size_y: SIZE,
            size_z: SIZE,
        },
        &format!("{label}-block"),
    )
    .snapshot;
    let top = face_where(&block, &|centre| (centre.z - SIZE).abs() < 1.0e-6);
    let bored = execute(
        &block,
        cut(
            top,
            PlanarFrame3::new(
                Point3::new(SIZE / 2.0, SIZE / 2.0, SIZE),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            circle(8.0),
        ),
        &format!("{label}-first"),
    )
    .snapshot;
    let side = face_where(&bored, &|centre| (centre.x - SIZE).abs() < 1.0e-6);
    execute(
        &bored,
        cut(
            side,
            PlanarFrame3::new(
                Point3::new(SIZE, SIZE / 2.0 + beside, SIZE / 2.0 + above),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            circle(radius),
        ),
        &format!("{label}-second"),
    )
}

/// What the two bores share. Across the wide bore's section at height `y`
/// the narrow bore's chord in `z` is `2√(r² − (y − y₀)²)`, and the wide one's
/// chord in `x` is `2√(64 − (y − 20)²)` whatever the height — so a narrow
/// bore lifted in `z` shares exactly what a centred one does.
fn shared(radius: f64, beside: f64) -> f64 {
    let centre = 20.0 + beside;
    let from = (centre - radius).max(12.0);
    let to = (centre + radius).min(28.0);
    integrate(from, to, &|y: f64| {
        4.0 * half_chord(radius, y - centre) * half_chord(8.0, y - 20.0)
    })
}

fn block_less_bores(radius: f64) -> f64 {
    SIZE.powi(3) - PI * 64.0 * SIZE - PI * radius * radius * SIZE
}

/// The reproducer of fix 9: a bore of radius 5 straight through one of
/// radius 8, axes crossing at right angles.
#[test]
fn a_narrower_bore_straight_through_a_wider_one_is_exact() {
    let outcome = crossed_block(5.0, 0.0, 0.0, "through");
    assert_exact(&outcome, block_less_bores(5.0) + shared(5.0, 0.0));
}

/// The same, with the narrow bore lifted off the wide one's axis: the axes
/// are skew, and the curve is the general quartic.
#[test]
fn a_narrower_bore_on_a_skew_axis_is_exact() {
    let outcome = crossed_block(5.0, 0.0, 2.0, "skew");
    assert_exact(&outcome, block_less_bores(5.0) + shared(5.0, 0.0));
}

/// Moved sideways until it only bites: the narrow bore leaves the wide one's
/// wall in one loop instead of two, and both cylinders' readings of the
/// curve have branch points.
#[test]
fn a_narrower_bore_that_only_bites_is_exact() {
    let outcome = crossed_block(5.0, 6.5, 0.0, "bite");
    assert_exact(&outcome, block_less_bores(5.0) + shared(5.0, 6.5));
}

/// Moved sideways by exactly its radius, the narrow bore's wall touches the
/// plane through the wide bore's axis, and the curve turns back exactly on
/// the wide bore's seams: its branch points are the corners of the faces it
/// crosses.
#[test]
fn a_bite_whose_curve_turns_back_on_the_seams_is_exact() {
    let outcome = crossed_block(5.0, 5.0, 0.0, "seam-bite");
    assert_exact(&outcome, block_less_bores(5.0) + shared(5.0, 5.0));
}

/// A bore nearly as wide as the other, which is where the two loops come
/// closest to the Steinmetz pinch they would make at equal radii.
#[test]
fn a_bore_nearly_as_wide_as_the_other_is_exact() {
    let outcome = crossed_block(7.5, 0.0, 0.0, "nearly");
    assert_exact(&outcome, block_less_bores(7.5) + shared(7.5, 0.0));
}

/// The reported case. A block whose top slopes, `z = 40 − x/3` over
/// `x ∈ [0, 60]`, 30 deep in `y`; two vertical bores of radius 6 at `x = 20`
/// and `x = 40`; and a stadium slot — ends of radius 4 whose centres sit 10
/// either side of the face's centre along the slope — sketched on the sloped
/// face and cut square to it through the part. Each end of the slot leans
/// with the slope and crosses a bore of another radius.
#[test]
fn a_slot_cut_from_a_sloped_face_across_two_bores_is_exact() {
    let (depth, slot_radius, half_length, bore_radius) = (30.0, 4.0, 10.0, 6.0);
    let block = execute(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, depth, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            profile: region(vec![
                PlanarCurve2::Line {
                    start: Point2::new(0.0, 0.0),
                    end: Point2::new(60.0, 0.0),
                },
                PlanarCurve2::Line {
                    start: Point2::new(60.0, 0.0),
                    end: Point2::new(60.0, 20.0),
                },
                PlanarCurve2::Line {
                    start: Point2::new(60.0, 20.0),
                    end: Point2::new(0.0, 40.0),
                },
                PlanarCurve2::Line {
                    start: Point2::new(0.0, 40.0),
                    end: Point2::new(0.0, 0.0),
                },
            ]),
            distance: depth,
        },
        "slope-block",
    )
    .snapshot;
    let top = |x: f64| 40.0 - x / 3.0;
    assert!(
        (block.measures().volume - 1_800.0 * depth).abs() < 1.0e-9,
        "the sloped block: {}",
        block.measures().volume
    );

    let mut bored = block;
    for (index, x) in [20.0, 40.0].into_iter().enumerate() {
        let bottom = face_where(&bored, &|centre| centre.z.abs() < 1.0e-6);
        bored = execute(
            &bored,
            cut(
                bottom,
                PlanarFrame3::new(
                    Point3::new(x, depth / 2.0, 0.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, -1.0, 0.0),
                ),
                circle(bore_radius),
            ),
            &format!("slope-bore-{index}"),
        )
        .snapshot;
    }

    // The slope's frame: `along` runs down the slope, the normal out of it.
    let norm = 10.0f64.sqrt();
    let along = Vector3::new(3.0 / norm, 0.0, -1.0 / norm);
    let origin = Point3::new(30.0, depth / 2.0, top(30.0));
    let sloped = face_where(&bored, &|centre| {
        (centre.z - top(centre.x)).abs() < 1.0e-6 && centre.z > 1.0
    });
    let (l, r) = (half_length, slot_radius);
    let stadium = vec![
        PlanarCurve2::Line {
            start: Point2::new(-l, -r),
            end: Point2::new(l, -r),
        },
        PlanarCurve2::CircularArc {
            center: Point2::new(l, 0.0),
            start: Point2::new(l, -r),
            end: Point2::new(l, r),
            direction: ArcDirection::CounterClockwise,
        },
        PlanarCurve2::Line {
            start: Point2::new(l, r),
            end: Point2::new(-l, r),
        },
        PlanarCurve2::CircularArc {
            center: Point2::new(-l, 0.0),
            start: Point2::new(-l, r),
            end: Point2::new(-l, -r),
            direction: ArcDirection::CounterClockwise,
        },
    ];
    let outcome = execute(
        &bored,
        cut(
            sloped,
            PlanarFrame3::new(origin, along, Vector3::new(0.0, 1.0, 0.0)),
            stadium,
        ),
        "slope-slot",
    );

    // Inclusion and exclusion over block, bores and slot.
    //
    // A bore's share of the block is its disc times the block's height at
    // the disc's centre, the height being linear in `x`.
    let bores: f64 = [20.0, 40.0]
        .into_iter()
        .map(|x| PI * bore_radius * bore_radius * top(x))
        .sum();
    // The slot runs square to the slope from the sloped face down to the
    // floor, clear of every other face. A point of the stadium at `x′` down
    // the slope sits at height `30 − x′/√10`, and the slot's depth there is
    // that height over the normal's rise `3/√10`; the stadium is symmetric
    // in `x′`, so the depth at its centre, `10√10`, times its area is the
    // volume.
    let stadium_area = 4.0 * l * r + PI * r * r;
    let slot = 10.0 * norm * stadium_area;
    // Where slot and bore overlap, walk each vertical line of the bore: at
    // `(x, y)` it is inside the slot for heights whose position down the
    // slope, `x′ = (3(x − 30) − (z − 30))/√10`, lies within the stadium's
    // reach `l + √(r² − (y − 15)²)` either side of its centre, and inside the
    // block below `top(x)`. Every bound is linear in `x`, so across one line
    // of the disc the length is piecewise linear and is integrated exactly
    // between its breaks; only the sweep across `y` needs quadrature.
    let overlap = |bore_x: f64| -> f64 {
        let across = |y: f64| -> f64 {
            let reach = l + half_chord(r, y - depth / 2.0);
            let chord = half_chord(bore_radius, y - depth / 2.0);
            let (from, to) = (bore_x - chord, bore_x + chord);
            let low = |x: f64| (3.0f64.mul_add(x, -60.0) - norm * reach).max(0.0);
            let high = |x: f64| (3.0f64.mul_add(x, -60.0) + norm * reach).min(top(x));
            let length = |x: f64| (high(x) - low(x)).max(0.0);
            // Where each bound changes its active piece, and where the
            // length reaches zero.
            let mut breaks = vec![from, to];
            let candidates = [
                (60.0 + norm * reach) / 3.0,
                (100.0 - norm * reach) * 3.0 / 10.0,
                (60.0 - norm * reach) / 3.0,
                (100.0 + norm * reach) * 3.0 / 10.0,
            ];
            breaks.extend(candidates.into_iter().filter(|x| *x > from && *x < to));
            breaks.sort_by(f64::total_cmp);
            let mut total = 0.0;
            for pair in breaks.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let (fa, fb) = (high(a) - low(a), high(b) - low(b));
                if fa >= 0.0 && fb >= 0.0 {
                    total += 0.5 * (b - a) * (fa + fb);
                } else if fa > 0.0 || fb > 0.0 {
                    // The length closes to zero inside the stretch.
                    let zero = a + (b - a) * fa / (fa - fb);
                    total += if fa > 0.0 {
                        0.5 * (zero - a) * fa
                    } else {
                        0.5 * (b - zero) * fb
                    };
                }
                let _ = length;
            }
            total
        };
        integrate(depth / 2.0 - r, depth / 2.0 + r, &across)
    };
    let shared = overlap(20.0) + overlap(40.0);
    let expected = 1_800.0 * depth - bores - slot + shared;
    assert_exact(&outcome, expected);
}

/// A body that carries the curve mirrors exactly (ADR 0047): each face's
/// azimuth turns round, and each trace goes with it — on the face that holds
/// the curve's parameter as a graph over the reversed azimuth, on the other
/// as the same parameter read into the face's new coordinates. The mirror is
/// a valid solid with the volume it came from, and mirroring it back returns
/// that volume again. The plane is oblique, so no frame survives unchanged.
#[test]
fn a_body_carrying_traces_mirrors_exactly() {
    let crossed = crossed_block(5.0, 0.0, 2.0, "mirror").snapshot;
    let volume = crossed.measures().volume;
    let plane = |origin: Point3, normal: Vector3| KernelCommand::MirrorSnapshot {
        plane_origin: origin,
        plane_normal: normal,
    };
    let mirrored = execute(
        &crossed,
        plane(Point3::new(3.0, -2.0, 5.0), Vector3::new(1.0, 2.0, -0.5)),
        "mirror-once",
    );
    assert!(
        NativeKernel::validate(&mirrored.snapshot, ValidationProfile::Solid).valid,
        "the mirror is a valid solid"
    );
    assert!(
        ((mirrored.snapshot.measures().volume - volume) / volume).abs() < 1.0e-12,
        "a mirror keeps the volume: {} vs {volume}",
        mirrored.snapshot.measures().volume
    );
    let back = execute(
        &mirrored.snapshot,
        plane(Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
        "mirror-twice",
    );
    assert!(NativeKernel::validate(&back.snapshot, ValidationProfile::Solid).valid);
    assert!(((back.snapshot.measures().volume - volume) / volume).abs() < 1.0e-12);
}

/// A traced body turned about an oblique axis, moved far off and scaled is
/// the same solid: every trace is re-derived from its two moved cylinders,
/// and the edges still meet their vertices.
#[test]
fn a_body_carrying_traces_moves_and_scales_exactly() {
    let crossed = crossed_block(5.0, 1.5, -2.0, "move").snapshot;
    let volume = crossed.measures().volume;
    // A turn of 0.9 rad about (1, 2, 2)/3.
    let (sin, cos) = 0.45f64.sin_cos();
    let scale = 2.5;
    let moved = execute(
        &crossed,
        KernelCommand::TransformSnapshot {
            transform: SimilarityTransform3 {
                translation: Vector3::new(1200.0, -350.0, 80.0),
                rotation: RotationQuaternion::new(cos, sin / 3.0, 2.0 * sin / 3.0, 2.0 * sin / 3.0),
                uniform_scale: scale,
            },
        },
        "move-once",
    );
    let validation = NativeKernel::validate(&moved.snapshot, ValidationProfile::Solid);
    assert!(validation.valid, "{:?}", validation.diagnostics);
    let expected = volume * scale * scale * scale;
    assert!(
        ((moved.snapshot.measures().volume - expected) / expected).abs() < 1.0e-12,
        "a similarity scales the volume by the cube: {} vs {expected}",
        moved.snapshot.measures().volume
    );
}

//! A loft between two planar sections (ADR 0049, K-A), pinned to closed
//! forms computed here without the kernel.
//!
//! Between parallel sections every cross-section of a ruled loft is the same
//! blend of the two section loops, so its area is a quadratic in the height,
//! and Simpson's rule — the prismoidal formula `V = h/6·(A₀ + 4·Aₘ + A₁)` — is
//! exact for it. `Aₘ` is the area of the loop halfway between the sections,
//! which each test computes from the correspondence the loft is specified to
//! make (corners paired in order; a square's corners at the quarter points of
//! a circle cut where it comes nearest the square's first corner), by Green's
//! theorem over that loop with a quadrature of its own. Frustums and
//! Cavalieri's principle cover the cases with a textbook answer.
//!
//! Sections on planes that are not parallel have no such formula. That case
//! is checked against an independent quadrature: the divergence theorem over
//! the loft's own boundary, written here from the section corners — both caps
//! and every wall, each wall the bilinear patch between two straight edges,
//! integrated by a Gauss–Legendre rule of an order at which its polynomial
//! integrand is integrated exactly — so it shares no code with the kernel's
//! measures. The same body is then moved, turned and mirrored, and must keep
//! its volume.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, LoftOperation,
    LoftSection, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, RotationQuaternion, SimilarityTransform3, Tier,
    ValidationProfile, Vector3,
};

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn frame(origin: [f64; 3], u: [f64; 3], v: [f64; 3]) -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(origin[0], origin[1], origin[2]),
        Vector3::new(u[0], u[1], u[2]),
        Vector3::new(v[0], v[1], v[2]),
    )
}

fn level(z: f64) -> PlanarFrame3 {
    frame([0.0, 0.0, z], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0])
}

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

/// A rectangle about the origin, turned by `degrees`, first corner at its
/// own lower left.
fn rectangle(width: f64, height: f64, degrees: f64) -> Vec<(f64, f64)> {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)]
        .iter()
        .map(|(x, y)| {
            let (x, y) = (x * width, y * height);
            (x * cos - y * sin, x * sin + y * cos)
        })
        .collect()
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

fn section(frame: PlanarFrame3, outer: PlanarLoop2, holes: Vec<PlanarLoop2>) -> LoftSection {
    LoftSection {
        frame,
        profile: PlanarProfile2 {
            regions: vec![PlanarRegion2 { outer, holes }],
        },
    }
}

// ---------------------------------------------------------------------------
// Running the kernel
// ---------------------------------------------------------------------------

fn execute(
    input: &Snapshot,
    sections: Vec<LoftSection>,
    operation: LoftOperation,
) -> Result<ExecutionOutcome, Vec<String>> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("loft"),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::LoftPlanarSections {
            sections,
            operation,
        },
    };
    NativeKernel::execute(input, &request, &CancellationToken::new()).map_err(|error| {
        error
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str().to_owned())
            .collect()
    })
}

/// A new body lofted from the empty snapshot, which must be exact and valid.
fn loft(sections: Vec<LoftSection>) -> Snapshot {
    let outcome =
        execute(&NativeKernel::empty(), sections, LoftOperation::New).expect("the loft builds");
    assert_eq!(outcome.report.rung.as_deref(), Some("loft/sections"));
    assert!(
        outcome.report.warnings.is_empty(),
        "a new loft is exact: {:?}",
        outcome.report.warnings
    );
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_valid(&outcome.snapshot);
    outcome.snapshot
}

fn refusal(sections: Vec<LoftSection>) -> Vec<String> {
    match execute(&NativeKernel::empty(), sections, LoftOperation::New) {
        Ok(outcome) => panic!(
            "the loft should be refused, but built {:?}",
            outcome.snapshot.counts()
        ),
        Err(codes) => codes,
    }
}

fn assert_valid(snapshot: &Snapshot) {
    let report = NativeKernel::validate(snapshot, ValidationProfile::Solid);
    assert!(report.valid, "{:#?}", report.diagnostics);
}

fn assert_relative(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < tolerance,
        "{what}: {actual} should be {expected} (relative error {:e})",
        ((actual - expected) / expected).abs()
    );
}

fn surfaces(snapshot: &Snapshot) -> artificer_kernel::SurfaceCounts {
    NativeKernel::surface_counts(snapshot)
}

// ---------------------------------------------------------------------------
// Independent geometry
// ---------------------------------------------------------------------------

/// Composite Simpson over `[from, to]`, independent of the kernel's rules.
fn simpson(from: f64, to: f64, integrand: &dyn Fn(f64) -> f64) -> f64 {
    let panels = 20_000;
    let step = (to - from) / f64::from(panels);
    (0..=panels)
        .map(|index| {
            let weight = if index == 0 || index == panels {
                1.0
            } else if index % 2 == 1 {
                4.0
            } else {
                2.0
            };
            weight * integrand(from + step * f64::from(index))
        })
        .sum::<f64>()
        * step
        / 3.0
}

/// A piece of a plane curve over `[0, 1]`: its point and its rate there.
type PlanePiece<'a> = &'a dyn Fn(f64) -> ((f64, f64), (f64, f64));

/// The area a closed plane curve encloses, `½∮(x dy − y dx)`, from its
/// pieces.
fn enclosed_area(pieces: &[PlanePiece<'_>]) -> f64 {
    pieces
        .iter()
        .map(|piece| {
            simpson(0.0, 1.0, &|t| {
                let ((x, y), (dx, dy)) = piece(t);
                0.5 * (x * dy - y * dx)
            })
        })
        .sum()
}

fn shoelace(points: &[(f64, f64)]) -> f64 {
    (0..points.len())
        .map(|index| {
            let (x0, y0) = points[index];
            let (x1, y1) = points[(index + 1) % points.len()];
            0.5 * (x0 * y1 - x1 * y0)
        })
        .sum()
}

fn prismoid(height: f64, bottom: f64, middle: f64, top: f64) -> f64 {
    height / 6.0 * (bottom + 4.0 * middle + top)
}

fn frustum(height: f64, bottom: f64, top: f64) -> f64 {
    height / 3.0 * (bottom + top + (bottom * top).sqrt())
}

// ---------------------------------------------------------------------------
// New bodies
// ---------------------------------------------------------------------------

#[test]
fn two_squares_on_parallel_planes_loft_to_a_frustum_of_planes() {
    let solid = loft(vec![
        section(level(0.0), polygon(&rectangle(20.0, 20.0, 0.0)), vec![]),
        section(level(15.0), polygon(&rectangle(10.0, 10.0, 0.0)), vec![]),
    ]);
    let counts = surfaces(&solid);
    assert_eq!((counts.planes, counts.ruled, counts.total()), (6, 0, 6));
    assert_relative(
        solid.measures().volume,
        frustum(15.0, 400.0, 100.0),
        1.0e-12,
        "frustum volume",
    );
    // A section whose frame faces back toward the other is turned round, so
    // the same squares loft to the same frustum whichever way their planes
    // were drawn.
    let facing_back = loft(vec![
        section(
            frame([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
            polygon(&rectangle(20.0, 20.0, 0.0)),
            vec![],
        ),
        section(
            frame([0.0, 0.0, 15.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
            polygon(&rectangle(10.0, 10.0, 0.0)),
            vec![],
        ),
    ]);
    assert_eq!(surfaces(&facing_back).planes, 6);
    assert_relative(
        facing_back.measures().volume,
        frustum(15.0, 400.0, 100.0),
        1.0e-12,
        "frustum from frames facing back",
    );
}

#[test]
fn a_square_lofts_to_a_circle_by_the_prismoidal_formula() {
    let (side, radius, height) = (20.0, 8.0, 12.0);
    let solid = loft(vec![
        section(level(0.0), polygon(&rectangle(side, side, 0.0)), vec![]),
        section(level(height), circle((0.0, 0.0), radius), vec![]),
    ]);
    let counts = surfaces(&solid);
    // Four walls, each a straight side ruled to a quarter circle.
    assert_eq!((counts.planes, counts.ruled), (2, 4));

    // The circle is cut where it comes nearest the square's first corner,
    // (−10, −10), at 225°, and then at the square's other corners' quarter
    // positions, so side `k` is ruled to the arc from 225° + 90°·k.
    let half = side / 2.0;
    let corners = [(-half, -half), (half, -half), (half, half), (-half, half)];
    let walls = (0..4)
        .map(|k| {
            let (a, b) = (corners[k], corners[(k + 1) % 4]);
            let start = (225.0 + 90.0 * k as f64).to_radians();
            move |t: f64| {
                let angle = start + t * PI / 2.0;
                let (sin, cos) = angle.sin_cos();
                (
                    (
                        0.5 * (a.0 + (b.0 - a.0) * t + radius * cos),
                        0.5 * (a.1 + (b.1 - a.1) * t + radius * sin),
                    ),
                    (
                        0.5 * ((b.0 - a.0) - radius * PI / 2.0 * sin),
                        0.5 * ((b.1 - a.1) + radius * PI / 2.0 * cos),
                    ),
                )
            }
        })
        .collect::<Vec<_>>();
    let middle = enclosed_area(&[&walls[0], &walls[1], &walls[2], &walls[3]]);
    let expected = prismoid(height, side * side, middle, PI * radius * radius);
    assert_relative(
        solid.measures().volume,
        expected,
        1.0e-9,
        "square to circle",
    );
}

#[test]
fn a_circle_lofts_to_an_offset_circle_by_cavalieri() {
    // Offset sideways, so no wall is a cone: each is ruled between two
    // circles whose centres do not share an axis. Every section between
    // them is still a circle — the pairing is at equal angles — whose
    // radius and centre move linearly, which is Cavalieri's frustum.
    let (bottom, top, height) = (10.0, 6.0, 20.0);
    let solid = loft(vec![
        section(level(0.0), circle((0.0, 0.0), bottom), vec![]),
        section(level(height), circle((5.0, 0.0), top), vec![]),
    ]);
    let counts = surfaces(&solid);
    assert_eq!((counts.planes, counts.ruled, counts.cones), (2, 2, 0));
    let expected = PI * height / 3.0 * (bottom * bottom + bottom * top + top * top);
    assert_relative(solid.measures().volume, expected, 1.0e-9, "oblique frustum");

    // Coaxial, the same two circles span cones — exact carriers the Boolean
    // engines take, and STEP writes as themselves.
    let coaxial = loft(vec![
        section(level(0.0), circle((0.0, 0.0), bottom), vec![]),
        section(level(height), circle((0.0, 0.0), top), vec![]),
    ]);
    let counts = surfaces(&coaxial);
    assert_eq!((counts.planes, counts.ruled, counts.cones), (2, 0, 2));
    assert_relative(coaxial.measures().volume, expected, 1.0e-12, "cone frustum");
    let step = NativeKernel::export_step(&coaxial, "cone").expect("the cone exports");
    assert!(step.contains("CONICAL_SURFACE") && !step.contains("B_SPLINE_SURFACE"));

    // Of equal radius, they span cylinders.
    let straight = loft(vec![
        section(level(0.0), circle((0.0, 0.0), bottom), vec![]),
        section(level(height), circle((0.0, 0.0), bottom), vec![]),
    ]);
    let counts = surfaces(&straight);
    assert_eq!((counts.planes, counts.cylinders, counts.ruled), (2, 2, 0));
    assert_relative(
        straight.measures().volume,
        PI * bottom * bottom * height,
        1.0e-12,
        "cylinder",
    );
}

#[test]
fn a_triangle_lofts_to_a_hexagon_cut_where_the_hexagon_has_corners() {
    // An equilateral triangle and a regular hexagon, corners on circles of
    // 12 and 8, both with a corner straight up. The hexagon has the more
    // corners, so its first corner holds still and the triangle starts from
    // whichever of its corners makes the rungs shortest — the one straight
    // up — and each side of the triangle is cut in half, where the hexagon's
    // corners fall: every half-side is ruled to one side of the hexagon.
    let height = 18.0;
    let corner = |radius: f64, degrees: f64| {
        let (sin, cos) = degrees.to_radians().sin_cos();
        (radius * cos, radius * sin)
    };
    let triangle = [90.0, 210.0, 330.0].map(|degrees| corner(12.0, degrees));
    let hexagon = (0..6)
        .map(|index| corner(8.0, 90.0 + 60.0 * f64::from(index)))
        .collect::<Vec<_>>();
    let solid = loft(vec![
        section(level(0.0), polygon(&triangle), vec![]),
        section(level(height), polygon(&hexagon), vec![]),
    ]);
    let counts = surfaces(&solid);
    assert_eq!(counts.planes + counts.ruled, 8, "{counts:?}");
    let halves = (0..6)
        .map(|index| {
            let (a, b) = (triangle[index / 2], triangle[(index / 2 + 1) % 3]);
            if index % 2 == 0 {
                a
            } else {
                (0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1))
            }
        })
        .collect::<Vec<_>>();
    let middle = halves
        .iter()
        .zip(&hexagon)
        .map(|(a, b)| (0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1)))
        .collect::<Vec<_>>();
    let expected = prismoid(
        height,
        shoelace(&triangle),
        shoelace(&middle),
        shoelace(&hexagon),
    );
    assert_relative(
        solid.measures().volume,
        expected,
        1.0e-12,
        "triangle to hexagon",
    );
}

#[test]
fn a_rectangle_turned_thirty_degrees_twists_its_walls() {
    let (width, depth, height) = (40.0, 20.0, 25.0);
    let bottom = rectangle(width, depth, 0.0);
    let top = rectangle(width, depth, 30.0);
    let solid = loft(vec![
        section(level(0.0), polygon(&bottom), vec![]),
        section(level(height), polygon(&top), vec![]),
    ]);
    let counts = surfaces(&solid);
    assert_eq!((counts.planes, counts.ruled), (2, 4));
    // Straight edges ruled to straight edges: halfway up, the section is the
    // polygon of the corners' midpoints.
    let middle = bottom
        .iter()
        .zip(&top)
        .map(|(a, b)| (0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1)))
        .collect::<Vec<_>>();
    let expected = prismoid(height, width * depth, shoelace(&middle), width * depth);
    assert_relative(solid.measures().volume, expected, 1.0e-12, "twisted prism");
}

#[test]
fn a_section_with_a_hole_lofts_its_hole_too() {
    let height = 20.0;
    let solid = loft(vec![
        section(
            level(0.0),
            polygon(&rectangle(30.0, 30.0, 0.0)),
            vec![circle((0.0, 0.0), 5.0)],
        ),
        section(
            level(height),
            polygon(&rectangle(20.0, 20.0, 0.0)),
            vec![circle((0.0, 0.0), 4.0)],
        ),
    ]);
    let counts = surfaces(&solid);
    // The outer walls are planes and the hole's two halves cones.
    assert_eq!((counts.planes, counts.cones, counts.ruled), (6, 2, 0));
    let expected = frustum(height, 900.0, 400.0) - PI * height / 3.0 * (25.0 + 20.0 + 16.0);
    assert_relative(solid.measures().volume, expected, 1.0e-12, "holed frustum");
}

/// Section 0 is a square on the ground; section 1 a smaller square on a plane
/// tilted twenty degrees about the x axis. Straight edges pair with straight
/// edges, so every wall is the bilinear patch between them, and the volume
/// is the divergence theorem over the boundary, `V = ⅓∮x·n dA`, written out
/// here: each wall `S(u, v) = (1 − v)·A(u) + v·B(u)` contributes
/// `⅓∫∫S·(S_u × S_v)`, a polynomial of degree three in each parameter that a
/// three-point Gauss–Legendre rule integrates exactly, and each cap `⅓(p·n)A`.
fn tilted_sections() -> (Vec<LoftSection>, f64) {
    let tilt = 20.0_f64.to_radians();
    let (sin, cos) = tilt.sin_cos();
    let top_frame = frame([0.0, 2.0, 30.0], [1.0, 0.0, 0.0], [0.0, cos, sin]);
    let bottom = rectangle(20.0, 20.0, 0.0);
    let top = rectangle(12.0, 12.0, 0.0);
    let lift = |(x, y): (f64, f64)| [x, 2.0 + y * cos, 30.0 + y * sin];
    let low = bottom
        .iter()
        .map(|(x, y)| [*x, *y, 0.0])
        .collect::<Vec<_>>();
    let high = top.iter().map(|point| lift(*point)).collect::<Vec<_>>();
    let gauss = [
        (-(0.6_f64.sqrt()), 5.0 / 9.0),
        (0.0, 8.0 / 9.0),
        (0.6_f64.sqrt(), 5.0 / 9.0),
    ];
    let mut flux = 0.0;
    for k in 0..4 {
        let (a0, a1) = (low[k], low[(k + 1) % 4]);
        let (b0, b1) = (high[k], high[(k + 1) % 4]);
        for (su, wu) in gauss {
            for (sv, wv) in gauss {
                let (u, v) = (0.5 * (su + 1.0), 0.5 * (sv + 1.0));
                let a = |i: usize| a0[i] + (a1[i] - a0[i]) * u;
                let b = |i: usize| b0[i] + (b1[i] - b0[i]) * u;
                let s = [0, 1, 2].map(|i| (1.0 - v) * a(i) + v * b(i));
                let su_ = [0, 1, 2].map(|i| (1.0 - v) * (a1[i] - a0[i]) + v * (b1[i] - b0[i]));
                let sv_ = [0, 1, 2].map(|i| b(i) - a(i));
                let n = [
                    su_[1] * sv_[2] - su_[2] * sv_[1],
                    su_[2] * sv_[0] - su_[0] * sv_[2],
                    su_[0] * sv_[1] - su_[1] * sv_[0],
                ];
                flux += 0.25 * wu * wv * (s[0] * n[0] + s[1] * n[1] + s[2] * n[2]);
            }
        }
    }
    // The bottom cap faces down at z = 0 and contributes nothing; the top
    // faces up its plane's normal, at the plane's distance from the origin.
    let normal = [0.0, -sin, cos];
    let distance = 2.0 * normal[1] + 30.0 * normal[2];
    flux += distance * 144.0;
    let sections = vec![
        section(level(0.0), polygon(&bottom), vec![]),
        section(top_frame, polygon(&top), vec![]),
    ];
    (sections, flux / 3.0)
}

#[test]
fn sections_on_planes_that_are_not_parallel_measure_by_the_divergence_theorem() {
    let (sections, expected) = tilted_sections();
    let solid = loft(sections);
    let counts = surfaces(&solid);
    // The two walls whose edges run along the tilt axis stay parallel and
    // span planes; the other two are twisted.
    assert_eq!((counts.planes, counts.ruled), (4, 2));
    assert_relative(solid.measures().volume, expected, 1.0e-10, "tilted loft");
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

/// The display chords of the rungs: the edges that climb from one section to
/// the other.
fn rung_chords(snapshot: &Snapshot, bottom: f64, top: f64) -> Vec<artificer_kernel::DebugEdge> {
    NativeKernel::debug_scene(snapshot)
        .edges
        .into_iter()
        .filter(|edge| {
            let [a, b] = edge.endpoints;
            ((a.z - bottom).abs() < 1.0e-9 && (b.z - top).abs() < 1.0e-9)
                || ((b.z - bottom).abs() < 1.0e-9 && (a.z - top).abs() < 1.0e-9)
        })
        .collect()
}

/// A four-arc oval: two arcs of 2 about `(±3, 0)` and two of 7 about
/// `(0, ∓4)`, meeting tangentially at `(±4.2, ±1.6)`, moved by `shift`.
fn oval(shift: (f64, f64)) -> PlanarLoop2 {
    let point = |x: f64, y: f64| Point2::new(x + shift.0, y + shift.1);
    let arc = |center: (f64, f64), start: (f64, f64), end: (f64, f64)| PlanarCurve2::CircularArc {
        center: point(center.0, center.1),
        start: point(start.0, start.1),
        end: point(end.0, end.1),
        direction: ArcDirection::CounterClockwise,
    };
    PlanarLoop2 {
        curves: vec![
            arc((3.0, 0.0), (4.2, -1.6), (4.2, 1.6)),
            arc((0.0, -4.0), (4.2, 1.6), (-4.2, 1.6)),
            arc((-3.0, 0.0), (-4.2, 1.6), (-4.2, -1.6)),
            arc((0.0, 4.0), (-4.2, -1.6), (4.2, -1.6)),
        ],
    }
}

#[test]
fn walls_that_meet_at_a_corner_draw_it_and_walls_that_meet_smoothly_do_not() {
    // Square to circle: each rung starts at a corner of the square, so the
    // two walls either side of it meet at an angle there, though tangent at
    // the circle. Compared along the whole rung, it is a crease.
    let square_to_circle = loft(vec![
        section(level(0.0), polygon(&rectangle(20.0, 20.0, 0.0)), vec![]),
        section(level(12.0), circle((0.0, 0.0), 8.0), vec![]),
    ]);
    let rungs = rung_chords(&square_to_circle, 0.0, 12.0);
    assert_eq!(rungs.len(), 4);
    assert!(
        rungs.iter().all(|rung| !rung.is_smooth && !rung.is_tangent),
        "{rungs:#?}"
    );

    // An oval swept obliquely: every rung is the same vector, and the arcs
    // either side of each rung share their tangent at both ends, so the
    // walls — different ruled surfaces — meet tangentially along the whole
    // rung. It is real topology but not a crease.
    let oblique = loft(vec![
        section(level(0.0), oval((0.0, 0.0)), vec![]),
        section(level(10.0), oval((3.0, 2.0)), vec![]),
    ]);
    assert_eq!(surfaces(&oblique).ruled, 4);
    let rungs = rung_chords(&oblique, 0.0, 10.0);
    assert_eq!(rungs.len(), 4);
    assert!(
        rungs.iter().all(|rung| rung.is_tangent && !rung.is_smooth),
        "{rungs:#?}"
    );
    // Every section of an oblique prism is the oval moved, so its volume is
    // the oval's area times the height.
    let arc = |center: (f64, f64), radius: f64, from: f64, to: f64| {
        move |t: f64| {
            let angle = from + (to - from) * t;
            let (sin, cos) = angle.sin_cos();
            (
                (center.0 + radius * cos, center.1 + radius * sin),
                (-radius * (to - from) * sin, radius * (to - from) * cos),
            )
        }
    };
    let (small, large) = (1.6_f64.atan2(1.2), 5.6_f64.atan2(4.2));
    let area = enclosed_area(&[
        &arc((3.0, 0.0), 2.0, -small, small),
        &arc((0.0, -4.0), 7.0, large, PI - large),
        &arc((-3.0, 0.0), 2.0, PI - small, PI + small),
        &arc((0.0, 4.0), 7.0, PI + large, 2.0 * PI - large),
    ]);
    assert_relative(
        oblique.measures().volume,
        area * 10.0,
        1.0e-9,
        "oblique oval",
    );

    // Two halves of one ruled surface — a circle to an offset circle, split
    // in two — are one carrier, and the seams between them do not draw.
    let halves = loft(vec![
        section(level(0.0), circle((0.0, 0.0), 10.0), vec![]),
        section(level(20.0), circle((5.0, 0.0), 6.0), vec![]),
    ]);
    let rungs = rung_chords(&halves, 0.0, 20.0);
    assert_eq!(rungs.len(), 2);
    assert!(rungs.iter().all(|rung| rung.is_smooth), "{rungs:#?}");
}

#[test]
fn a_ruled_wall_draws_its_silhouette_where_it_turns_from_the_viewer() {
    let ruled_carriers = |snapshot: &Snapshot| {
        NativeKernel::debug_scene(snapshot)
            .carriers
            .into_iter()
            .filter(|carrier| {
                matches!(
                    carrier.surface,
                    artificer_kernel::DisplaySurface::Ruled { .. }
                )
            })
            .collect::<Vec<_>>()
    };
    // A circle swept obliquely along x: each half is a slanted cylinder, and
    // seen along y its outline is the two rungs at the far left and right,
    // from one section to the other — where the circle's tangent is along y.
    let slanted = loft(vec![
        section(level(0.0), circle((0.0, 0.0), 10.0), vec![]),
        section(level(20.0), circle((5.0, 0.0), 10.0), vec![]),
    ]);
    let carriers = ruled_carriers(&slanted);
    assert_eq!(carriers.len(), 2);
    let chords = carriers
        .iter()
        .flat_map(|carrier| {
            carrier
                .surface
                .ruled_silhouette(carrier.domain, [0.0, 1.0, 0.0])
        })
        .collect::<Vec<_>>();
    assert!(!chords.is_empty());
    for point in chords.iter().flatten() {
        // On the rung through (±10, 0, 0): x = ±10 + z/4, y = 0, to within
        // the sweep's sampling of where the rung is.
        let expected = if point.x > 2.5 { 10.0 } else { -10.0 } + point.z / 4.0;
        assert!(
            point.y.abs() < 0.2 && (point.x - expected).abs() < 1.0e-3,
            "{point:?}"
        );
    }
    for sign in [1.0, -1.0] {
        let (low, high) = chords
            .iter()
            .flatten()
            .filter(|point| (point.x - 2.5).signum() == sign)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), point| {
                (low.min(point.z), high.max(point.z))
            });
        assert!(low < 1.0e-9 && high > 20.0 - 1.0e-9, "{sign}: {low} {high}");
    }

    // A square to a circle seen obliquely: the outline crosses the rungs of
    // its twisted walls.
    let square_to_circle = loft(vec![
        section(level(0.0), polygon(&rectangle(20.0, 20.0, 0.0)), vec![]),
        section(level(12.0), circle((0.0, 0.0), 8.0), vec![]),
    ]);
    let carriers = ruled_carriers(&square_to_circle);
    assert_eq!(carriers.len(), 4);
    let chords = carriers
        .iter()
        .flat_map(|carrier| {
            carrier
                .surface
                .ruled_silhouette(carrier.domain, [0.8, 0.6, -0.3])
        })
        .collect::<Vec<_>>();
    assert!(!chords.is_empty());
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn a_loft_refuses_by_name_what_it_cannot_build() {
    let square = || polygon(&rectangle(20.0, 20.0, 0.0));
    // Two sections on one plane.
    assert_eq!(
        refusal(vec![
            section(level(0.0), square(), vec![]),
            section(level(0.0), circle((40.0, 0.0), 5.0), vec![]),
        ]),
        ["LOFT_SECTIONS_COPLANAR"]
    );
    // One section is not a loft.
    assert_eq!(
        refusal(vec![section(level(0.0), square(), vec![])]),
        ["LOFT_TOO_FEW_SECTIONS"]
    );
    // A hole with nothing to pair with.
    assert_eq!(
        refusal(vec![
            section(level(0.0), square(), vec![circle((0.0, 0.0), 3.0)]),
            section(level(10.0), square(), vec![]),
        ]),
        ["LOFT_HOLE_COUNT_MISMATCH"]
    );
    // Two holes that trade places: nearest centroids pair the one near the
    // middle with the one on the right, so the other must cross it.
    assert_eq!(
        refusal(vec![
            section(
                level(0.0),
                polygon(&rectangle(40.0, 40.0, 0.0)),
                vec![circle((1.0, 0.0), 2.0), circle((10.0, 0.0), 2.0)],
            ),
            section(
                level(10.0),
                polygon(&rectangle(40.0, 40.0, 0.0)),
                vec![circle((8.0, 0.0), 2.0), circle((-8.0, 0.0), 2.0)],
            ),
        ]),
        ["LOFT_RUNGS_CROSS"]
    );
    // A section tilted until it reaches through the other's plane.
    assert_eq!(
        refusal(vec![
            section(level(0.0), square(), vec![]),
            section(
                frame([0.0, 0.0, 5.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                square(),
                vec![],
            ),
        ]),
        ["LOFT_SECTION_CROSSES_PLANE"]
    );
    // Two regions in one section.
    assert_eq!(
        refusal(vec![
            section(level(0.0), square(), vec![]),
            LoftSection {
                frame: level(10.0),
                profile: PlanarProfile2 {
                    regions: vec![
                        PlanarRegion2 {
                            outer: circle((-10.0, 0.0), 3.0),
                            holes: vec![],
                        },
                        PlanarRegion2 {
                            outer: circle((10.0, 0.0), 3.0),
                            holes: vec![],
                        },
                    ],
                },
            },
        ]),
        ["LOFT_SECTION_REGIONS_UNSUPPORTED"]
    );
}

/// Two things this stage refused are built since ADR 0050, and each is
/// pinned here to its answer rather than its old refusal: a loft through
/// three sections, and a section drawn with a spline. The first is three
/// equal squares, whose smooth loft is the prism through them; the second a
/// square lofted to a spline arch closed by a line, measured independently
/// in `bspline_surfaces.rs` and here only built, exact and valid.
#[test]
fn what_this_stage_refused_is_built_by_the_next() {
    let square = || polygon(&rectangle(20.0, 20.0, 0.0));
    let outcome = execute(
        &NativeKernel::empty(),
        vec![
            section(level(0.0), square(), vec![]),
            section(level(10.0), square(), vec![]),
            section(level(20.0), square(), vec![]),
        ],
        LoftOperation::New,
    )
    .expect("three sections loft");
    assert_eq!(outcome.report.rung.as_deref(), Some("loft/skinned"));
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_valid(&outcome.snapshot);
    assert_relative(
        outcome.snapshot.measures().volume,
        8_000.0,
        1.0e-9,
        "the prism through three squares",
    );

    let arch = PlanarLoop2 {
        curves: vec![
            PlanarCurve2::Bspline {
                degree: 2,
                control_points: vec![
                    Point2::new(-5.0, 0.0),
                    Point2::new(0.0, 8.0),
                    Point2::new(5.0, 0.0),
                ],
                knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                weights: None,
            },
            PlanarCurve2::Line {
                start: Point2::new(5.0, 0.0),
                end: Point2::new(-5.0, 0.0),
            },
        ],
    };
    let outcome = execute(
        &NativeKernel::empty(),
        vec![
            section(level(0.0), square(), vec![]),
            section(level(10.0), arch, vec![]),
        ],
        LoftOperation::New,
    )
    .expect("a spline section lofts");
    assert_eq!(outcome.report.rung.as_deref(), Some("loft/sections"));
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_valid(&outcome.snapshot);
}

// ---------------------------------------------------------------------------
// Moving the body
// ---------------------------------------------------------------------------

fn transformed(snapshot: &Snapshot, command: KernelCommand) -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("move"),
        expected_snapshot: snapshot.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    let outcome = NativeKernel::execute(snapshot, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert_valid(&outcome.snapshot);
    outcome.snapshot
}

#[test]
fn a_loft_keeps_its_volume_when_moved_turned_mirrored_or_scaled() {
    for sections in [
        vec![
            section(level(0.0), polygon(&rectangle(20.0, 20.0, 0.0)), vec![]),
            section(level(12.0), circle((0.0, 0.0), 8.0), vec![]),
        ],
        tilted_sections().0,
    ] {
        let solid = loft(sections);
        let volume = solid.measures().volume;
        let area = solid.measures().surface_area;
        let turn = 0.5 * 37.0_f64.to_radians();
        let moved = transformed(
            &solid,
            KernelCommand::TransformSnapshot {
                transform: SimilarityTransform3 {
                    translation: Vector3::new(13.0, -7.0, 101.0),
                    rotation: RotationQuaternion::new(turn.cos(), 0.3, -0.5, turn.sin()),
                    uniform_scale: 1.0,
                },
            },
        );
        assert_relative(moved.measures().volume, volume, 1.0e-9, "moved volume");
        assert_relative(moved.measures().surface_area, area, 1.0e-9, "moved area");
        let mirrored = transformed(
            &moved,
            KernelCommand::MirrorSnapshot {
                plane_origin: Point3::new(1.0, 2.0, 3.0),
                plane_normal: Vector3::new(0.2, -1.0, 0.4),
            },
        );
        assert_relative(
            mirrored.measures().volume,
            volume,
            1.0e-9,
            "mirrored volume",
        );
        assert_relative(
            mirrored.measures().surface_area,
            area,
            1.0e-9,
            "mirrored area",
        );
        let scaled = transformed(
            &mirrored,
            KernelCommand::TransformSnapshot {
                transform: SimilarityTransform3 {
                    translation: Vector3::new(0.0, 0.0, 0.0),
                    rotation: RotationQuaternion::IDENTITY,
                    uniform_scale: 2.0,
                },
            },
        );
        assert_relative(
            scaled.measures().volume,
            8.0 * volume,
            1.0e-9,
            "scaled volume",
        );
    }
}

// ---------------------------------------------------------------------------
// Add and cut
// ---------------------------------------------------------------------------

fn block() -> Snapshot {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("block"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(-20.0, -20.0, -20.0),
            size_x: 40.0,
            size_y: 40.0,
            size_z: 20.0,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("block")
        .snapshot
}

#[test]
fn a_frustum_adds_to_a_block_exactly() {
    let body = block();
    let outcome = execute(
        &body,
        vec![
            section(level(0.0), polygon(&rectangle(20.0, 20.0, 0.0)), vec![]),
            section(level(15.0), polygon(&rectangle(10.0, 10.0, 0.0)), vec![]),
        ],
        LoftOperation::Add,
    )
    .expect("the add builds");
    let rung = outcome.report.rung.clone().unwrap_or_default();
    assert!(
        rung == "loft/boolean-analytic" || rung == "loft/boolean-prism",
        "an all-planar loft stays exact: {rung}"
    );
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_valid(&outcome.snapshot);
    assert_relative(
        outcome.snapshot.measures().volume,
        32_000.0 + frustum(15.0, 400.0, 100.0),
        1.0e-12,
        "block and frustum",
    );
}

#[test]
fn a_frustum_cuts_a_block_exactly() {
    let body = block();
    // Standing from ten below the top face to five above it: what the cut
    // takes is the frustum below the face, whose side there is 32/3.
    let outcome = execute(
        &body,
        vec![
            section(level(-10.0), polygon(&rectangle(16.0, 16.0, 0.0)), vec![]),
            section(level(5.0), polygon(&rectangle(8.0, 8.0, 0.0)), vec![]),
        ],
        LoftOperation::Cut,
    )
    .expect("the cut builds");
    assert_eq!(
        outcome.report.rung.as_deref(),
        Some("loft/boolean-analytic")
    );
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert_valid(&outcome.snapshot);
    let side = 32.0 / 3.0;
    assert_relative(
        outcome.snapshot.measures().volume,
        32_000.0 - frustum(10.0, 256.0, side * side),
        1.0e-12,
        "block less frustum",
    );
}

#[test]
fn a_ruled_loft_cuts_a_block_on_the_faceted_tier_and_says_so() {
    let body = block();
    // A square pocket at the top face narrowing to a round floor.
    let outcome = execute(
        &body,
        vec![
            section(level(-12.0), circle((0.0, 0.0), 5.0), vec![]),
            section(level(4.0), polygon(&rectangle(16.0, 16.0, 0.0)), vec![]),
        ],
        LoftOperation::Cut,
    )
    .expect("the cut builds");
    assert_eq!(outcome.report.rung.as_deref(), Some("loft/faceted"));
    assert_eq!(outcome.report.tier(), Tier::Approximate);
    let codes = outcome
        .report
        .warnings
        .iter()
        .map(|warning| warning.code.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"LOFT_FACETED_APPROXIMATION".to_owned()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"LOFT_EXACT_ROUTE_DECLINED".to_owned()),
        "{codes:?}"
    );
    assert_valid(&outcome.snapshot);
    // What the cut took is the loft below the top face, three quarters of
    // the way from the circle to the square: the prismoidal formula over
    // that stretch, whose sections blend each side of the square with a
    // quarter of the circle — the circle cut where it comes nearest the
    // square's first corner, at 225°. The faceted tier answers within its
    // budget, not exactly.
    let blend = |t: f64| {
        let corners = [(-8.0, -8.0), (8.0, -8.0), (8.0, 8.0), (-8.0, 8.0)];
        let walls = (0..4)
            .map(|k| {
                let (a, b) = (corners[k], corners[(k + 1) % 4]);
                let start = (225.0 + 90.0 * k as f64).to_radians();
                move |s: f64| {
                    let angle = start + s * PI / 2.0;
                    let (sin, cos) = angle.sin_cos();
                    (
                        (
                            (1.0 - t) * 5.0 * cos + t * (a.0 + (b.0 - a.0) * s),
                            (1.0 - t) * 5.0 * sin + t * (a.1 + (b.1 - a.1) * s),
                        ),
                        (
                            -(1.0 - t) * 5.0 * PI / 2.0 * sin + t * (b.0 - a.0),
                            (1.0 - t) * 5.0 * PI / 2.0 * cos + t * (b.1 - a.1),
                        ),
                    )
                }
            })
            .collect::<Vec<_>>();
        enclosed_area(&[&walls[0], &walls[1], &walls[2], &walls[3]])
    };
    let inside = prismoid(12.0, blend(0.0), blend(0.375), blend(0.75));
    let volume = outcome.snapshot.measures().volume;
    assert_relative(
        volume,
        32_000.0 - inside,
        2.0e-3,
        "block less the loft inside it",
    );
}

#[test]
fn a_cut_into_a_ruled_wall_falls_to_the_faceted_tier_with_its_label() {
    // A round pocket up from the square end of a square-to-circle loft. It
    // fits the square, but the walls lean in above it, so the pocket runs
    // into them: the local rewrite stands aside, the analytic engine
    // declines the ruled walls by name, and the faceted tier answers — from
    // a tessellation that includes the ruled walls — and says it is an
    // approximation, exactly as for any cut the exact route cannot carry.
    let solid = loft(vec![
        section(level(0.0), polygon(&rectangle(20.0, 20.0, 0.0)), vec![]),
        section(level(12.0), circle((0.0, 0.0), 8.0), vec![]),
    ]);
    let bottom = NativeKernel::faces(&solid)
        .into_iter()
        .find(|face| {
            NativeKernel::describe_face(&solid, *face)
                .is_ok_and(|description| description.normal.z < -0.999)
        })
        .expect("the square end");
    let support = NativeKernel::planar_face_support(&solid, bottom).expect("a planar face");
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("pocket"),
        expected_snapshot: solid.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudeFacePlanarProfile {
            target_face: bottom,
            frame: support.frame,
            profile: PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: circle((0.0, 0.0), 9.0),
                    holes: vec![],
                }],
            },
            distance: 6.0,
            operation: artificer_protocol::FaceExtrusionOperation::Cut,
        },
    };
    let outcome = NativeKernel::execute(&solid, &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(outcome.report.rung.as_deref(), Some("face-feature/faceted"));
    assert_eq!(outcome.report.tier(), Tier::Approximate);
    let warnings = outcome
        .report
        .warnings
        .iter()
        .map(|warning| (warning.code.as_str().to_owned(), warning.message.clone()))
        .collect::<Vec<_>>();
    assert!(
        warnings
            .iter()
            .any(|(code, _)| code == "FACE_FEATURE_FACETED_APPROXIMATION"),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(
            |(code, message)| code == "FACE_FEATURE_EXACT_ROUTE_DECLINED"
                && message.contains("ruled")
        ),
        "{warnings:?}"
    );
    assert_valid(&outcome.snapshot);
    let before = solid.measures().volume;
    let after = outcome.snapshot.measures().volume;
    assert!(
        after < before && after > before - PI * 81.0 * 6.0,
        "{before} {after}"
    );
}

// ---------------------------------------------------------------------------
// STEP
// ---------------------------------------------------------------------------

struct Entity {
    kind: String,
    args: Vec<String>,
}

fn parse_step(step: &str) -> BTreeMap<u64, Entity> {
    let data = step
        .split("DATA;")
        .nth(1)
        .and_then(|data| data.split("ENDSEC;").next())
        .expect("a DATA section");
    let mut entities = BTreeMap::new();
    for line in data.lines().filter(|line| line.starts_with('#')) {
        let (id, body) = line.split_once('=').expect("#id=");
        let body = body.trim_end_matches(';');
        if body.starts_with('(') {
            continue;
        }
        let open = body.find('(').expect("an argument list");
        entities.insert(
            id[1..].parse().expect("an entity number"),
            Entity {
                kind: body[..open].to_owned(),
                args: split_args(&body[open + 1..body.len() - 1]),
            },
        );
    }
    entities
}

fn split_args(text: &str) -> Vec<String> {
    let (mut args, mut current, mut depth, mut quoted) = (Vec::new(), String::new(), 0, false);
    for character in text.chars() {
        match character {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                args.push(current.trim().to_owned());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_owned());
    }
    args
}

/// The items of one parenthesised Part 21 list.
fn list(text: &str) -> Vec<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(text);
    split_args(inner)
}

fn point(entities: &BTreeMap<u64, Entity>, reference: &str) -> [f64; 3] {
    let entity = &entities[&reference
        .trim_start_matches('#')
        .parse()
        .expect("a reference")];
    assert_eq!(entity.kind, "CARTESIAN_POINT");
    let values = list(&entity.args[1])
        .iter()
        .map(|value| value.parse().expect("a real"))
        .collect::<Vec<f64>>();
    [values[0], values[1], values[2]]
}

/// A cubic B-spline's point at `t`, by de Boor's recursion, over a full knot
/// vector.
fn de_boor(knots: &[f64], points: &[[f64; 3]], t: f64) -> [f64; 3] {
    const DEGREE: usize = 3;
    let last = points.len() - 1;
    let span = (DEGREE..=last)
        .rev()
        .find(|&k| knots[k] <= t && knots[k] < knots[k + 1])
        .unwrap_or(DEGREE);
    let mut local = (0..=DEGREE)
        .map(|j| points[span - DEGREE + j])
        .collect::<Vec<_>>();
    for r in 1..=DEGREE {
        for j in (r..=DEGREE).rev() {
            let index = span - DEGREE + j;
            let alpha = (t - knots[index]) / (knots[index + DEGREE + 1 - r] - knots[index]);
            local[j] = [0, 1, 2].map(|i| (1.0 - alpha) * local[j - 1][i] + alpha * local[j][i]);
        }
    }
    local[DEGREE]
}

#[test]
fn a_ruled_wall_is_written_to_step_within_a_tenth_of_a_micron() {
    let (side, radius, height) = (20.0, 8.0, 12.0);
    let solid = loft(vec![
        section(level(0.0), polygon(&rectangle(side, side, 0.0)), vec![]),
        section(level(height), circle((0.0, 0.0), radius), vec![]),
    ]);
    let step = NativeKernel::export_step(&solid, "loft").expect("the loft exports");
    let entities = parse_step(&step);
    let surfaces = entities
        .values()
        .filter(|entity| entity.kind == "B_SPLINE_SURFACE_WITH_KNOTS")
        .collect::<Vec<_>>();
    assert_eq!(surfaces.len(), 4, "one spline surface per ruled wall");
    for surface in surfaces {
        assert_eq!(
            (surface.args[1].as_str(), surface.args[2].as_str()),
            ("3", "1")
        );
        let columns = list(&surface.args[3]);
        let rows = [0, 1].map(|row| {
            columns
                .iter()
                .map(|column| point(&entities, &list(column)[row]))
                .collect::<Vec<_>>()
        });
        let multiplicities = list(&surface.args[8])
            .iter()
            .map(|value| value.parse::<usize>().expect("a multiplicity"))
            .collect::<Vec<_>>();
        let values = list(&surface.args[10])
            .iter()
            .map(|value| value.parse::<f64>().expect("a knot"))
            .collect::<Vec<_>>();
        let knots = values
            .iter()
            .zip(&multiplicities)
            .flat_map(|(value, count)| std::iter::repeat_n(*value, *count))
            .collect::<Vec<_>>();
        // The exact wall, from the ends of its two rails: the bottom rail
        // is the straight side between its ends, and the top the quarter
        // circle between its ends, walked counter-clockwise.
        let (a, b) = (rows[0][0], rows[0][rows[0].len() - 1]);
        let (c, d) = (rows[1][0], rows[1][rows[1].len() - 1]);
        assert!((a[2]).abs() < 1.0e-12 && (c[2] - height).abs() < 1.0e-12);
        let start = c[1].atan2(c[0]);
        let mut sweep = d[1].atan2(d[0]) - start;
        if sweep < 0.0 {
            sweep += 2.0 * PI;
        }
        assert!(
            (sweep - PI / 2.0).abs() < 1.0e-9,
            "a quarter circle: {sweep}"
        );
        let mut worst = 0.0_f64;
        for i in 0..=40 {
            let u = f64::from(i) / 40.0;
            let low = de_boor(&knots, &rows[0], u);
            let high = de_boor(&knots, &rows[1], u);
            let angle = start + sweep * u;
            let line = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * u);
            let arc = [radius * angle.cos(), radius * angle.sin(), height];
            for j in 0..=8 {
                let v = f64::from(j) / 8.0;
                for k in 0..3 {
                    let spline = (1.0 - v) * low[k] + v * high[k];
                    let exact = (1.0 - v) * line[k] + v * arc[k];
                    worst = worst.max((spline - exact).abs());
                }
            }
        }
        assert!(worst <= 1.0e-7, "the spline strays {worst:e} from the wall");
    }
}

// ---------------------------------------------------------------------------
// Scripting
// ---------------------------------------------------------------------------

const SCRIPT: &str = r#"
let base = sketch(on: plane(from: "XY", offset: 0), entities: [rect(width: 20, height: 20)], label: "base");
let top = sketch(on: plane(origin: [0, 0, 12], normal: [0, 0, 1], x_axis: [1, 0, 0]), entities: [circle(radius: 8)], label: "top");
let body = loft(sections: [base, top], operation: "new", label: "body");
"#;

#[test]
fn a_scripted_loft_runs_and_decompiles_to_itself() {
    let commands = compile_script(SCRIPT, &BTreeMap::new()).expect("the script compiles");
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, ApiCommand::Loft { .. })),
        "{commands:?}"
    );
    let mut session = Session::new();
    let token = CancellationToken::default();
    for command in commands.clone() {
        session.execute(command, &token).expect("the step runs");
    }
    let volume = session.snapshot.measures().volume;
    assert!(volume > 0.0);
    let report = &session.step_reports["body"];
    assert_eq!(report.rung.as_deref(), Some("loft/sections"));

    let written = artificer_kernel::api::decompile::decompile_journal(
        &session.journal,
        &artificer_kernel::api::decompile::DecompileOptions::default(),
    )
    .expect("the journal decompiles");
    let again = compile_script(&written, &BTreeMap::new()).expect("the decompiled script compiles");
    assert_eq!(again, commands, "{written}");
}

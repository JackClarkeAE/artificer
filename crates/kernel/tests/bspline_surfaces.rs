//! B-spline curves and surfaces (ADR 0050, K-B), pinned to answers computed
//! here without the kernel.
//!
//! A spline profile's area is Green's theorem, `½∮(x dy − y dx)`, evaluated
//! from the curve's own control points and knots: de Boor's recursion for
//! the point, the derivative spline for the rate, and a Gauss–Legendre rule
//! whose nodes this file finds itself. On each knot span the integrand is a
//! polynomial of degree `2p − 1`, which the 16-point rule integrates
//! exactly, so a straight extrusion's volume — that area times the height —
//! is pinned to rounding. A two-section loft between parallel sections has
//! its prismoidal formula, the middle section's area taken the same way
//! from the correspondence the loft is specified to make.
//!
//! A smooth loft has closed forms in two cases: one section repeated, whose
//! skinned walls are the prism through it, and sections that scale linearly
//! with height, whose walls are linear along the loft because interpolating
//! linear data reproduces it. Elsewhere a smooth loft is checked against its
//! own display facets — a volume that shares no code with the kernel's
//! measures — and its walls are checked to pass through every section and
//! to be smooth there, by difference quotients either side.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::commands::ApiCommand;
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::session::{Session, fit_point_spline};
use artificer_kernel::{
    CancellationToken, DisplaySurface, ExecutionOutcome, NativeKernel, Snapshot, SurfaceCounts,
};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, FaceExtrusionOperation, KernelCommand,
    LoftOperation, LoftSection, PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy, RequestId, RevolveAngle,
    RotationQuaternion, SimilarityTransform3, Tier, ValidationProfile, Vector3,
};

// ---------------------------------------------------------------------------
// Profiles and sections
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
            .map(|index| line(points[index], points[(index + 1) % points.len()]))
            .collect(),
    }
}

fn line(start: (f64, f64), end: (f64, f64)) -> PlanarCurve2 {
    PlanarCurve2::Line {
        start: Point2::new(start.0, start.1),
        end: Point2::new(end.0, end.1),
    }
}

/// A square of side `side` about the origin, first corner at its lower left.
fn square(side: f64) -> PlanarLoop2 {
    let half = side / 2.0;
    polygon(&[(-half, -half), (half, -half), (half, half), (-half, half)])
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

fn spline(degree: usize, points: &[(f64, f64)], knots: &[f64]) -> PlanarCurve2 {
    PlanarCurve2::Bspline {
        degree,
        control_points: points.iter().map(|(x, y)| Point2::new(*x, *y)).collect(),
        knots: knots.to_vec(),
        weights: None,
    }
}

/// The closed spline the fit-point tool draws through `points`.
fn closed_fit(points: &[(f64, f64)]) -> PlanarCurve2 {
    let points = points
        .iter()
        .map(|(x, y)| Point2::new(*x, *y))
        .collect::<Vec<_>>();
    fit_point_spline(&points, true).expect("the points fit")
}

/// A lopsided closed spline through six points, the profile most tests use.
const BLOB: [(f64, f64); 6] = [
    (18.0, 0.0),
    (9.0, 11.0),
    (-6.0, 13.0),
    (-17.0, 2.0),
    (-10.0, -10.0),
    (6.0, -12.0),
];

fn one_loop(curve: PlanarCurve2) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: vec![curve],
    }
}

fn profile(outer: PlanarLoop2, holes: Vec<PlanarLoop2>) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 { outer, holes }],
    }
}

fn section(frame: PlanarFrame3, outer: PlanarLoop2) -> LoftSection {
    LoftSection {
        frame,
        profile: profile(outer, Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Running the kernel
// ---------------------------------------------------------------------------

fn run(input: &Snapshot, command: KernelCommand) -> Result<ExecutionOutcome, Vec<String>> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("spline"),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::new()).map_err(|error| {
        error
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str().to_owned())
            .collect()
    })
}

/// A step that must build, exact, with the named rung and a valid body.
fn exact(input: &Snapshot, command: KernelCommand, rung: &str) -> Snapshot {
    let outcome = run(input, command).unwrap_or_else(|codes| panic!("refused: {codes:?}"));
    assert_eq!(outcome.report.rung.as_deref(), Some(rung));
    assert_eq!(outcome.report.tier(), Tier::Exact);
    assert!(
        outcome.report.warnings.is_empty(),
        "an exact step warns of nothing: {:?}",
        outcome.report.warnings
    );
    assert_valid(&outcome.snapshot);
    outcome.snapshot
}

fn refused(input: &Snapshot, command: KernelCommand) -> Vec<String> {
    match run(input, command) {
        Ok(outcome) => panic!(
            "the step should be refused, but built {:?}",
            outcome.snapshot.counts()
        ),
        Err(codes) => codes,
    }
}

fn extrude(frame: PlanarFrame3, profile: PlanarProfile2, distance: f64) -> KernelCommand {
    KernelCommand::ExtrudePlanarProfile {
        frame,
        profile,
        distance,
    }
}

fn loft(sections: Vec<LoftSection>) -> KernelCommand {
    KernelCommand::LoftPlanarSections {
        sections,
        operation: LoftOperation::New,
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

fn surfaces(snapshot: &Snapshot) -> SurfaceCounts {
    NativeKernel::surface_counts(snapshot)
}

/// The volume the display facets enclose, by the divergence theorem over
/// the triangles: an approximation within the facets' chord tolerance, and
/// independent of the kernel's exact measures.
fn facet_volume(snapshot: &Snapshot) -> f64 {
    NativeKernel::debug_scene(snapshot)
        .triangles
        .iter()
        .map(|triangle| {
            let [a, b, c] = triangle.vertices;
            (a.x * (b.y * c.z - b.z * c.y) - a.y * (b.x * c.z - b.z * c.x)
                + a.z * (b.x * c.y - b.y * c.x))
                / 6.0
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Independent spline arithmetic
// ---------------------------------------------------------------------------

/// `P_n(x)` and `P_n'(x)`, by the three-term recurrence.
fn legendre(n: usize, x: f64) -> (f64, f64) {
    let (mut previous, mut current) = (1.0, x);
    for k in 2..=n {
        let k = k as f64;
        let next = ((2.0 * k - 1.0) * x * current - (k - 1.0) * previous) / k;
        previous = current;
        current = next;
    }
    (current, n as f64 * (x * current - previous) / (x * x - 1.0))
}

/// The `n`-point Gauss–Legendre rule on `[−1, 1]`: each node polished from
/// the usual cosine guess by Newton's method on `P_n`.
fn gauss_legendre(n: usize) -> Vec<(f64, f64)> {
    (1..=n)
        .map(|index| {
            let mut x = (PI * (index as f64 - 0.25) / (n as f64 + 0.5)).cos();
            for _ in 0..100 {
                let (value, slope) = legendre(n, x);
                let step = value / slope;
                x -= step;
                if step.abs() < 1.0e-16 {
                    break;
                }
            }
            let (_, slope) = legendre(n, x);
            (x, 2.0 / ((1.0 - x * x) * slope * slope))
        })
        .collect()
}

/// A plane B-spline read from its protocol form.
#[derive(Clone, Debug)]
struct Spline {
    degree: usize,
    knots: Vec<f64>,
    points: Vec<[f64; 2]>,
}

impl Spline {
    fn of(curve: &PlanarCurve2) -> Self {
        let PlanarCurve2::Bspline {
            degree,
            control_points,
            knots,
            ..
        } = curve
        else {
            panic!("not a spline: {curve:?}");
        };
        Self {
            degree: *degree,
            knots: knots.clone(),
            points: control_points
                .iter()
                .map(|point| [point.x, point.y])
                .collect(),
        }
    }

    fn domain(&self) -> (f64, f64) {
        (self.knots[self.degree], self.knots[self.points.len()])
    }

    /// The nonempty knot spans of the domain.
    fn spans(&self) -> Vec<(f64, f64)> {
        (self.degree..self.points.len())
            .map(|index| (self.knots[index], self.knots[index + 1]))
            .filter(|(low, high)| high > low)
            .collect()
    }

    /// De Boor's recursion.
    fn point(&self, t: f64) -> [f64; 2] {
        let p = self.degree;
        let last = self.points.len() - 1;
        let span = (p..=last)
            .rev()
            .find(|&k| self.knots[k] <= t && self.knots[k] < self.knots[k + 1])
            .unwrap_or(p);
        let mut local = (0..=p)
            .map(|j| self.points[span - p + j])
            .collect::<Vec<_>>();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let index = span - p + j;
                let alpha =
                    (t - self.knots[index]) / (self.knots[index + p + 1 - r] - self.knots[index]);
                local[j] =
                    [0, 1].map(|axis| (1.0 - alpha) * local[j - 1][axis] + alpha * local[j][axis]);
            }
        }
        local[p]
    }

    /// The derivative, a spline of one degree less on the inner knots.
    fn derivative(&self) -> Self {
        let p = self.degree;
        let points = self
            .points
            .windows(2)
            .enumerate()
            .map(|(index, pair)| {
                let width = self.knots[index + p + 1] - self.knots[index + 1];
                [0, 1].map(|axis| p as f64 * (pair[1][axis] - pair[0][axis]) / width)
            })
            .collect();
        Self {
            degree: p - 1,
            knots: self.knots[1..self.knots.len() - 1].to_vec(),
            points,
        }
    }

    /// `∫ f(C(t), C'(t)) dt` over the domain, by the 16-point rule on
    /// `panels` equal pieces of every span.
    fn integrate(&self, panels: usize, f: &dyn Fn([f64; 2], [f64; 2]) -> f64) -> f64 {
        let rule = gauss_legendre(16);
        let rate = self.derivative();
        let mut sum = 0.0;
        for (low, high) in self.spans() {
            let width = (high - low) / panels as f64;
            for panel in 0..panels {
                let start = low + width * panel as f64;
                for (node, weight) in &rule {
                    let t = start + width * 0.5 * (node + 1.0);
                    sum += weight * width * 0.5 * f(self.point(t), rate.point(t));
                }
            }
        }
        sum
    }

    /// `½∫(x y' − y x')`, exact on every span.
    fn green(&self) -> f64 {
        self.integrate(1, &|[x, y], [dx, dy]| 0.5 * (x * dy - y * dx))
    }

    fn length(&self) -> f64 {
        self.integrate(64, &|_, [dx, dy]| dx.hypot(dy))
    }
}

/// `½(x₀y₁ − x₁y₀)`, a straight edge's share of Green's theorem.
fn green_line(start: (f64, f64), end: (f64, f64)) -> f64 {
    0.5 * (start.0 * end.1 - end.0 * start.1)
}

// ---------------------------------------------------------------------------
// Extruding spline profiles
// ---------------------------------------------------------------------------

#[test]
fn a_closed_fit_point_spline_extrudes_to_its_area_times_its_height() {
    let curve = closed_fit(&BLOB);
    let reference = Spline::of(&curve);

    // The fit is the documented one: cubic, through every point at its
    // chord-length parameter, closed on the first point with one tangent
    // either side of the seam.
    assert_eq!(reference.degree, 3);
    let mut chords = vec![0.0];
    for index in 0..BLOB.len() {
        let (a, b) = (BLOB[index], BLOB[(index + 1) % BLOB.len()]);
        chords.push(chords[index] + (b.0 - a.0).hypot(b.1 - a.1));
    }
    let total = chords[BLOB.len()];
    for (index, point) in BLOB.iter().enumerate() {
        let at = reference.point(chords[index] / total);
        assert!(
            (at[0] - point.0).hypot(at[1] - point.1) < 1.0e-9,
            "the fit misses point {index}: {at:?}"
        );
    }
    let (start, end) = reference.domain();
    let rate = reference.derivative();
    let (leaving, arriving) = (rate.point(start), rate.point(end));
    assert!(
        (leaving[0] - arriving[0]).hypot(leaving[1] - arriving[1])
            < 1.0e-9 * leaving[0].hypot(leaving[1]),
        "the seam is smooth: {leaving:?} {arriving:?}"
    );

    let area = reference.green().abs();
    let height = 7.5;
    // A frame turned off every axis and moved off the origin.
    let (sin, cos) = 20.0_f64.to_radians().sin_cos();
    let (tilt_sin, tilt_cos) = 40.0_f64.to_radians().sin_cos();
    let tilted = frame(
        [5.0, -3.0, 2.0],
        [cos, sin, 0.0],
        [-sin * tilt_cos, cos * tilt_cos, tilt_sin],
    );
    let solid = exact(
        &NativeKernel::empty(),
        extrude(tilted, profile(one_loop(curve), Vec::new()), height),
        "extrusion/spline-profile",
    );
    assert_relative(solid.measures().volume, area * height, 1.0e-9, "volume");
    // Two caps, and a wall whose area is the curve's length times the
    // height.
    assert_relative(
        solid.measures().surface_area,
        2.0 * area + reference.length() * height,
        1.0e-9,
        "surface area",
    );
    // The closed curve is walled in two halves.
    let counts = surfaces(&solid);
    assert_eq!((counts.planes, counts.bspline, counts.total()), (2, 2, 4));
}

#[test]
fn an_open_spline_closed_by_lines_extrudes_exactly() {
    // A cubic on uneven interior knots, from the right end of a slot's
    // floor to its left, over the top; three lines close it.
    let arch = spline(
        3,
        &[
            (30.0, 0.0),
            (26.0, 7.0),
            (19.0, -2.0),
            (12.0, 11.0),
            (4.0, 9.0),
            (0.0, 0.0),
        ],
        &[0.0, 0.0, 0.0, 0.0, 0.45, 0.7, 1.0, 1.0, 1.0, 1.0],
    );
    let corners = [(0.0, 0.0), (0.0, -12.0), (30.0, -12.0), (30.0, 0.0)];
    let area = (green_line(corners[0], corners[1])
        + green_line(corners[1], corners[2])
        + green_line(corners[2], corners[3])
        + Spline::of(&arch).green())
    .abs();
    let outer = PlanarLoop2 {
        curves: vec![
            line(corners[0], corners[1]),
            line(corners[1], corners[2]),
            line(corners[2], corners[3]),
            arch,
        ],
    };
    let height = 9.0;
    let solid = exact(
        &NativeKernel::empty(),
        extrude(level(-4.0), profile(outer, Vec::new()), height),
        "extrusion/spline-profile",
    );
    assert_relative(solid.measures().volume, area * height, 1.0e-9, "volume");
    let counts = surfaces(&solid);
    assert_eq!((counts.planes, counts.bspline), (5, 1));
}

#[test]
fn a_spline_hole_extrudes_through_a_plate() {
    let hole = closed_fit(&BLOB.map(|(x, y)| (x * 0.5, y * 0.5)));
    let area = 60.0 * 50.0 - Spline::of(&hole).green().abs();
    let solid = exact(
        &NativeKernel::empty(),
        extrude(
            level(0.0),
            profile(
                polygon(&[(-30.0, -25.0), (30.0, -25.0), (30.0, 25.0), (-30.0, 25.0)]),
                vec![one_loop(hole)],
            ),
            4.0,
        ),
        "extrusion/spline-profile",
    );
    assert_relative(solid.measures().volume, area * 4.0, 1.0e-9, "volume");
}

fn block() -> Snapshot {
    run(
        &NativeKernel::empty(),
        KernelCommand::MakeCuboid {
            origin: Point3::new(-20.0, -20.0, -20.0),
            size_x: 40.0,
            size_y: 40.0,
            size_z: 20.0,
        },
    )
    .expect("block")
    .snapshot
}

#[test]
fn a_spline_pocket_and_boss_answer_on_the_faceted_tier_and_say_so() {
    let body = block();
    let top = NativeKernel::describe_faces(&body)
        .values()
        .find(|face| face.normal.z > 0.5)
        .expect("a top face")
        .face;
    let support = NativeKernel::planar_face_support(&body, top).expect("the top is planar");
    let outline = closed_fit(&BLOB.map(|(x, y)| (x * 0.8, y * 0.8)));
    let area = Spline::of(&outline).green().abs();
    for (operation, distance, expected) in [
        (FaceExtrusionOperation::Cut, 6.0, 32_000.0 - area * 6.0),
        (FaceExtrusionOperation::Add, 4.0, 32_000.0 + area * 4.0),
    ] {
        let outcome = run(
            &body,
            KernelCommand::ExtrudeFacePlanarProfile {
                target_face: top,
                frame: support.frame,
                profile: profile(one_loop(outline.clone()), Vec::new()),
                distance,
                operation,
            },
        )
        .unwrap_or_else(|codes| panic!("{operation:?}: {codes:?}"));
        assert_eq!(
            outcome.report.rung.as_deref(),
            Some("face-feature/faceted"),
            "{operation:?}"
        );
        assert_eq!(outcome.report.tier(), Tier::Approximate);
        let codes = outcome
            .report
            .warnings
            .iter()
            .map(|warning| warning.code.as_str().to_owned())
            .collect::<Vec<_>>();
        assert!(
            codes
                .iter()
                .any(|code| code == "FACE_FEATURE_FACETED_APPROXIMATION"),
            "{codes:?}"
        );
        assert_valid(&outcome.snapshot);
        // The faceted answer is within its chord tolerance of the exact one.
        let volume = outcome.snapshot.measures().volume;
        assert!(
            (volume - expected).abs() < 0.01 * area * distance,
            "{operation:?}: {volume} against {expected}"
        );
    }
}

// ---------------------------------------------------------------------------
// Two-section lofts with a spline section
// ---------------------------------------------------------------------------

#[test]
fn a_square_lofts_to_a_spline_arch_by_the_prismoidal_formula() {
    // The top section is a square's lower three sides and a cubic arch over
    // them, walked counter-clockwise from its lower left corner like the
    // square below it; the loft pairs the four pieces in order, so the arch
    // is ruled to the square's top side, both run over the unit interval.
    let arch = [(8.0, 6.0), (4.0, 14.0), (-4.0, 14.0), (-8.0, 6.0)];
    let top = PlanarLoop2 {
        curves: vec![
            line((-8.0, -8.0), (8.0, -8.0)),
            line((8.0, -8.0), (8.0, 6.0)),
            spline(3, &arch, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]),
            line((-8.0, 6.0), (-8.0, -8.0)),
        ],
    };
    let height = 12.0;
    let solid = exact(
        &NativeKernel::empty(),
        loft(vec![
            section(level(0.0), square(20.0)),
            section(level(height), top),
        ]),
        "loft/sections",
    );
    let arch_spline = Spline {
        degree: 3,
        knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        points: arch.map(|(x, y)| [x, y]).to_vec(),
    };
    let top_area = green_line((-8.0, -8.0), (8.0, -8.0))
        + green_line((8.0, -8.0), (8.0, 6.0))
        + arch_spline.green()
        + green_line((-8.0, 6.0), (-8.0, -8.0));
    // Halfway up, each corner is the mean of its two, and the arch's piece
    // is the mean of the arch and the square's top side, point for point at
    // one parameter: a cubic again, whose control points are the means of
    // the arch's and of the side's, raised to degree three.
    let side = [
        (10.0, 10.0),
        (10.0 / 3.0, 10.0),
        (-10.0 / 3.0, 10.0),
        (-10.0, 10.0),
    ];
    let middle_arch = Spline {
        degree: 3,
        knots: arch_spline.knots.clone(),
        points: arch
            .iter()
            .zip(&side)
            .map(|(a, s)| [0.5 * (a.0 + s.0), 0.5 * (a.1 + s.1)])
            .collect(),
    };
    let mean = |a: (f64, f64), b: (f64, f64)| (0.5 * (a.0 + b.0), 0.5 * (a.1 + b.1));
    let corners = [
        mean((-10.0, -10.0), (-8.0, -8.0)),
        mean((10.0, -10.0), (8.0, -8.0)),
        mean((10.0, 10.0), (8.0, 6.0)),
        mean((-10.0, 10.0), (-8.0, 6.0)),
    ];
    let middle_area = green_line(corners[0], corners[1])
        + green_line(corners[1], corners[2])
        + middle_arch.green()
        + green_line(corners[3], corners[0]);
    let expected = height / 6.0 * (400.0 + 4.0 * middle_area + top_area);
    assert_relative(solid.measures().volume, expected, 1.0e-9, "volume");
    let counts = surfaces(&solid);
    assert_eq!((counts.bspline, counts.planes), (1, 5));
}

#[test]
fn a_circle_lofts_to_a_closed_spline_through_its_cubic_fit() {
    // The circle is cut where the closed spline's two halves meet, and its
    // halves are fitted by cubics well inside the linear agreement to rule
    // them to the spline's; the body is exact to that agreement and checked
    // here against its facets.
    let solid = exact(
        &NativeKernel::empty(),
        loft(vec![
            section(level(0.0), circle((0.0, 0.0), 10.0)),
            section(
                level(12.0),
                one_loop(closed_fit(&BLOB.map(|(x, y)| (x * 0.6, y * 0.6)))),
            ),
        ]),
        "loft/sections",
    );
    assert_relative(
        facet_volume(&solid),
        solid.measures().volume,
        2.0e-3,
        "facet volume",
    );
    assert_eq!(surfaces(&solid).bspline, 2);
}

// ---------------------------------------------------------------------------
// Smooth lofts through several sections
// ---------------------------------------------------------------------------

#[test]
fn a_smooth_loft_through_one_section_repeated_is_the_prism_of_it() {
    let radius = 7.0;
    let cylinder = exact(
        &NativeKernel::empty(),
        loft(
            [0.0, 10.0, 25.0]
                .iter()
                .map(|z| section(level(*z), circle((0.0, 0.0), radius)))
                .collect(),
        ),
        "loft/skinned",
    );
    // The circle is carried by its cubic fit, within the linear agreement.
    assert_relative(
        cylinder.measures().volume,
        PI * radius * radius * 25.0,
        1.0e-8,
        "cylinder",
    );
    let prism = exact(
        &NativeKernel::empty(),
        loft(
            [0.0, 4.0, 9.0, 15.0]
                .iter()
                .map(|z| section(level(*z), square(20.0)))
                .collect(),
        ),
        "loft/skinned",
    );
    assert_relative(prism.measures().volume, 400.0 * 15.0, 1.0e-12, "prism");
    // A hole is lofted the same way.
    let tube = exact(
        &NativeKernel::empty(),
        loft(
            [0.0, 6.0, 14.0]
                .iter()
                .map(|z| LoftSection {
                    frame: level(*z),
                    profile: profile(square(20.0), vec![circle((2.0, 1.0), 4.0)]),
                })
                .collect(),
        ),
        "loft/skinned",
    );
    assert_relative(
        tube.measures().volume,
        (400.0 - PI * 16.0) * 14.0,
        1.0e-8,
        "square tube",
    );
}

#[test]
fn sections_that_scale_linearly_loft_to_a_frustum() {
    let frustum =
        |height: f64, bottom: f64, top: f64| height / 3.0 * (bottom + top + (bottom * top).sqrt());
    for (levels, sides) in [
        (vec![0.0, 6.0, 12.0], vec![20.0, 15.0, 10.0]),
        (vec![0.0, 5.0, 10.0, 15.0], vec![20.0, 17.0, 14.0, 11.0]),
        (
            vec![0.0, 2.0, 7.0, 9.0, 15.0],
            vec![20.0, 19.2, 17.2, 16.4, 14.0],
        ),
    ] {
        let last = levels.len() - 1;
        let solid = exact(
            &NativeKernel::empty(),
            loft(
                levels
                    .iter()
                    .zip(&sides)
                    .map(|(z, side)| section(level(*z), square(*side)))
                    .collect(),
            ),
            "loft/skinned",
        );
        assert_relative(
            solid.measures().volume,
            frustum(levels[last], sides[0] * sides[0], sides[last] * sides[last]),
            1.0e-10,
            "frustum",
        );
    }
}

/// The smooth lofts whose shape has no closed form: a square to a circle
/// and back, and a square through a circle and a closed spline to a circle.
fn smooth_lofts() -> Vec<(Vec<f64>, Vec<LoftSection>)> {
    let waist = one_loop(closed_fit(&[
        (-6.0, -5.0),
        (6.0, -5.0),
        (6.0, 5.0),
        (-6.0, 5.0),
    ]));
    vec![
        (
            vec![0.0, 10.0, 20.0],
            vec![
                section(level(0.0), square(20.0)),
                section(level(10.0), circle((0.0, 0.0), 8.0)),
                section(level(20.0), square(20.0)),
            ],
        ),
        (
            vec![0.0, 12.0, 26.0, 34.0],
            vec![
                section(level(0.0), square(20.0)),
                section(level(12.0), circle((0.0, 0.0), 13.0)),
                section(level(26.0), waist),
                section(level(34.0), circle((0.0, 0.0), 9.0)),
            ],
        ),
    ]
}

#[test]
fn smooth_lofts_through_three_and_four_sections_are_valid_and_smooth_through_them() {
    for (levels, sections) in smooth_lofts() {
        let count = levels.len();
        let solid = exact(&NativeKernel::empty(), loft(sections), "loft/skinned");
        let counts = surfaces(&solid);
        assert_eq!(counts.planes, 2, "two caps");
        assert!(counts.bspline >= 4, "{counts:?}");
        assert_eq!(counts.total(), counts.planes + counts.bspline);
        assert_relative(
            facet_volume(&solid),
            solid.measures().volume,
            2.0e-3,
            "facet volume",
        );

        let carriers = NativeKernel::debug_scene(&solid)
            .carriers
            .into_iter()
            .filter(|carrier| matches!(carrier.surface, DisplaySurface::Bspline { .. }))
            .collect::<Vec<_>>();
        assert_eq!(carriers.len(), counts.bspline as usize);
        for carrier in carriers {
            let wall = carrier.surface;
            for (index, z) in levels.iter().enumerate() {
                // Where along the wall the section is: found by bisection
                // on the height of the wall's middle, which rises all the
                // way.
                let v = if index == 0 {
                    0.0
                } else if index + 1 == count {
                    1.0
                } else {
                    let (mut low, mut high) = (0.0, 1.0);
                    for _ in 0..80 {
                        let middle = 0.5 * (low + high);
                        if wall.evaluate(0.5, middle).z < *z {
                            low = middle;
                        } else {
                            high = middle;
                        }
                    }
                    0.5 * (low + high)
                };
                for step in 0..=8 {
                    let u = f64::from(step) / 8.0;
                    // The wall passes through every section: all of that
                    // row of it lies on the section's plane.
                    let on = wall.evaluate(u, v);
                    assert!((on.z - z).abs() < 1.0e-9, "section {index}: {on:?}");
                    if index == 0 || index + 1 == count {
                        continue;
                    }
                    // And it is smooth there: the rates just below and just
                    // above the section agree, as they would not at a crease.
                    let h = 1.0e-5;
                    let (below, above) = (wall.evaluate(u, v - h), wall.evaluate(u, v + h));
                    let lower = [
                        (on.x - below.x) / h,
                        (on.y - below.y) / h,
                        (on.z - below.z) / h,
                    ];
                    let upper = [
                        (above.x - on.x) / h,
                        (above.y - on.y) / h,
                        (above.z - on.z) / h,
                    ];
                    let gap = (0..3)
                        .map(|axis| (upper[axis] - lower[axis]).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    let size = upper.iter().map(|value| value * value).sum::<f64>().sqrt();
                    assert!(
                        gap < 1.0e-3 * size,
                        "a crease at section {index}, u = {u}: {lower:?} against {upper:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_spline_wall_draws_its_silhouette_where_it_turns_from_the_viewer() {
    // An upright wall seen along x turns away from the viewer where the
    // curve runs along x: at its highest and lowest points in y.
    let curve = closed_fit(&BLOB);
    let reference = Spline::of(&curve);
    let (start, end) = reference.domain();
    let ys = (0..=20_000)
        .map(|index| reference.point(start + (end - start) * f64::from(index) / 20_000.0)[1])
        .collect::<Vec<_>>();
    let low = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let high = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let solid = exact(
        &NativeKernel::empty(),
        extrude(level(0.0), profile(one_loop(curve), Vec::new()), 6.0),
        "extrusion/spline-profile",
    );
    let chords = NativeKernel::debug_scene(&solid)
        .carriers
        .into_iter()
        .filter(|carrier| matches!(carrier.surface, DisplaySurface::Bspline { .. }))
        .flat_map(|carrier| {
            carrier
                .surface
                .spline_silhouette(carrier.domain, [1.0, 0.0, 0.0])
        })
        .collect::<Vec<_>>();
    let (mut top, mut bottom) = (false, false);
    for [a, b] in chords {
        // Upright chords, at the curve's extremes.
        assert!((a.x - b.x).abs() < 1.0e-9 && (a.y - b.y).abs() < 1.0e-9);
        top |= (a.y - high).abs() < 1.0e-3;
        bottom |= (a.y - low).abs() < 1.0e-3;
        assert!(
            (a.y - high).abs() < 1.0e-3 || (a.y - low).abs() < 1.0e-3,
            "a silhouette away from the extremes: {a:?}"
        );
    }
    assert!(top && bottom, "both sides are outlined");
    // Seen from above the walls are edge-on everywhere, and turn from no
    // one.
    for carrier in NativeKernel::debug_scene(&solid).carriers {
        assert!(
            carrier
                .surface
                .spline_silhouette(carrier.domain, [0.0, 0.0, 1.0])
                .is_empty()
        );
    }
}

// ---------------------------------------------------------------------------
// Moving spline bodies
// ---------------------------------------------------------------------------

fn transformed(snapshot: &Snapshot, command: KernelCommand) -> Snapshot {
    let outcome = run(snapshot, command).unwrap_or_else(|codes| panic!("{codes:?}"));
    assert_valid(&outcome.snapshot);
    outcome.snapshot
}

#[test]
fn spline_bodies_keep_their_measures_when_moved_turned_mirrored_or_scaled() {
    let extrusion = exact(
        &NativeKernel::empty(),
        extrude(
            level(0.0),
            profile(one_loop(closed_fit(&BLOB)), Vec::new()),
            6.0,
        ),
        "extrusion/spline-profile",
    );
    let (_, sections) = smooth_lofts().remove(1);
    let skinned = exact(&NativeKernel::empty(), loft(sections), "loft/skinned");
    for solid in [extrusion, skinned] {
        let (volume, area) = (solid.measures().volume, solid.measures().surface_area);
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
        assert_eq!(surfaces(&mirrored), surfaces(&solid));
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
        assert_relative(
            scaled.measures().surface_area,
            4.0 * area,
            1.0e-9,
            "scaled area",
        );
    }
}

// ---------------------------------------------------------------------------
// STEP
// ---------------------------------------------------------------------------

#[test]
fn spline_bodies_are_written_to_step_as_b_spline_entities() {
    let extrusion = exact(
        &NativeKernel::empty(),
        extrude(
            level(0.0),
            profile(one_loop(closed_fit(&BLOB)), Vec::new()),
            6.0,
        ),
        "extrusion/spline-profile",
    );
    let step = NativeKernel::export_step(&extrusion, "extrusion").expect("the body exports");
    // Each half of the closed curve sweeps a cubic-by-linear wall, bounded
    // by the half at either end.
    assert_eq!(
        step.matches("B_SPLINE_SURFACE_WITH_KNOTS('',3,1,").count(),
        2
    );
    assert_eq!(step.matches("B_SPLINE_CURVE_WITH_KNOTS('',3,").count(), 4);

    let (_, sections) = smooth_lofts().remove(1);
    let skinned = exact(&NativeKernel::empty(), loft(sections), "loft/skinned");
    let step = NativeKernel::export_step(&skinned, "skinned").expect("the loft exports");
    let walls = surfaces(&skinned).bspline as usize;
    assert_eq!(step.matches("B_SPLINE_SURFACE_WITH_KNOTS(").count(), walls);
    // Four sections: every wall is cubic along the loft.
    assert_eq!(
        step.matches("B_SPLINE_SURFACE_WITH_KNOTS('',3,3,").count(),
        walls
    );
    assert!(step.contains("B_SPLINE_CURVE_WITH_KNOTS("));
}

// ---------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------

/// The digests of two bodies sharing spline content — the closed blob
/// extruded, and a smooth loft with the same blob as a section — built in
/// that order, or the other way round when `reversed`.
fn digests(reversed: bool) -> [String; 2] {
    let first = || {
        exact(
            &NativeKernel::empty(),
            extrude(
                level(0.0),
                profile(one_loop(closed_fit(&BLOB)), Vec::new()),
                6.0,
            ),
            "extrusion/spline-profile",
        )
        .semantic_digest()
        .to_string()
    };
    let second = || {
        exact(
            &NativeKernel::empty(),
            loft(vec![
                section(level(0.0), circle((0.0, 0.0), 22.0)),
                section(level(10.0), one_loop(closed_fit(&BLOB))),
                section(level(20.0), square(16.0)),
            ]),
            "loft/skinned",
        )
        .semantic_digest()
        .to_string()
    };
    if reversed {
        let later = second();
        [first(), later]
    } else {
        let earlier = first();
        [earlier, second()]
    }
}

const ORDER: &str = "ARTIFICER_SPLINE_DIGEST_ORDER";

/// A body's digest hashes its splines' content, never the handles the
/// process-wide store gave them, so it does not depend on which spline a
/// process happened to build first. Each order is built in a fresh process
/// of this test binary, where the store starts empty.
#[test]
fn a_digest_does_not_depend_on_which_spline_a_process_built_first() {
    if let Ok(order) = std::env::var(ORDER) {
        // The child: build in the order asked and print the digests.
        let [first, second] = digests(order == "reversed");
        println!("DIGEST {first} {second}");
        return;
    }
    let child = |order: &str| {
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "a_digest_does_not_depend_on_which_spline_a_process_built_first",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ORDER, order)
            .output()
            .expect("the child runs");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.lines()
            .find_map(|line| line.split_once("DIGEST ").map(|(_, digests)| digests))
            .unwrap_or_else(|| panic!("no digests from the child: {text}"))
            .to_owned()
    };
    let forward = child("forward");
    let reversed = child("reversed");
    assert_eq!(forward, reversed);
    let here = digests(false);
    assert_eq!(forward, format!("{} {}", here[0], here[1]));
    // And the same builds again in one process are the same bodies.
    assert_eq!(digests(true), here);
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[test]
fn what_cannot_be_certified_is_refused_by_name() {
    let empty = NativeKernel::empty();
    let arch = |curve: PlanarCurve2| {
        profile(
            PlanarLoop2 {
                curves: vec![curve, line((10.0, 0.0), (0.0, 0.0))],
            },
            Vec::new(),
        )
    };
    let points = [(0.0, 0.0), (3.0, 8.0), (7.0, 8.0), (10.0, 0.0)];
    let clamped = [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
    let cases: Vec<(PlanarCurve2, &str)> = vec![
        (
            PlanarCurve2::Bspline {
                degree: 3,
                control_points: points.iter().map(|(x, y)| Point2::new(*x, *y)).collect(),
                knots: clamped.to_vec(),
                weights: Some(vec![1.0, 2.0, 2.0, 1.0]),
            },
            "BSPLINE_RATIONAL_UNSUPPORTED",
        ),
        (
            spline(3, &points, &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]),
            "BSPLINE_UNCLAMPED_UNSUPPORTED",
        ),
        (
            spline(
                6,
                &[
                    (0.0, 0.0),
                    (1.0, 4.0),
                    (3.0, 8.0),
                    (5.0, 9.0),
                    (7.0, 8.0),
                    (9.0, 4.0),
                    (10.0, 0.0),
                ],
                &[0.0; 7]
                    .iter()
                    .chain(&[1.0; 7])
                    .copied()
                    .collect::<Vec<_>>(),
            ),
            "BSPLINE_DEGREE_UNSUPPORTED",
        ),
        (
            spline(3, &points, &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0]),
            "BSPLINE_KNOTS_INVALID",
        ),
        (
            // The first two control points coincide: the curve leaves its
            // start at no speed.
            spline(
                2,
                &[(0.0, 0.0), (0.0, 0.0), (5.0, 8.0), (10.0, 0.0)],
                &[0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0],
            ),
            "BSPLINE_CURVE_DEGENERATE",
        ),
        (
            // It dips through the closing line.
            spline(
                3,
                &[(0.0, 0.0), (3.0, -6.0), (7.0, 8.0), (10.0, 0.0)],
                &clamped,
            ),
            "EXTRUDE_PROFILE_SELF_INTERSECTING",
        ),
    ];
    for (curve, code) in cases {
        assert_eq!(
            refused(&empty, extrude(level(0.0), arch(curve), 5.0)),
            [code]
        );
    }

    // A draft offsets the section, and a spline's offset is no spline.
    assert_eq!(
        refused(
            &empty,
            KernelCommand::LoftPlanarProfileOffset {
                frame: level(0.0),
                profile: profile(one_loop(closed_fit(&BLOB)), Vec::new()),
                distance: 10.0,
                offset: -1.0,
            },
        ),
        ["LOFT_OFFSET_SPLINE_UNSUPPORTED"]
    );
    // With no draft it is the straight extrusion.
    exact(
        &empty,
        KernelCommand::LoftPlanarProfileOffset {
            frame: level(0.0),
            profile: profile(one_loop(closed_fit(&BLOB)), Vec::new()),
            distance: 10.0,
            offset: 0.0,
        },
        "loft/straight",
    );

    // The revolve has no B-spline carrier for a revolved spline.
    assert_eq!(
        refused(
            &empty,
            KernelCommand::RevolvePlanarProfile {
                frame: frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                profile: profile(
                    PlanarLoop2 {
                        curves: vec![
                            line((5.0, 0.0), (15.0, 0.0)),
                            spline(
                                3,
                                &[(15.0, 0.0), (15.0, 6.0), (8.0, 9.0), (5.0, 10.0)],
                                &clamped
                            ),
                            line((5.0, 10.0), (5.0, 0.0)),
                        ],
                    },
                    Vec::new(),
                ),
                axis: PlanarAxis2 {
                    start: Point2::new(0.0, 0.0),
                    end: Point2::new(0.0, 1.0),
                },
                angle: RevolveAngle::FullTurn,
            },
        ),
        ["PLANAR_PROFILE_SPLINE_UNSUPPORTED"]
    );

    // Three level sections whose centroids swing far sideways at the top:
    // by the chord lengths between them the middle section comes a fifth
    // of the way along, the wall rises past it and has to come back down
    // to the top, which is only half a millimetre higher.
    assert_eq!(
        refused(
            &empty,
            loft(vec![
                section(level(0.0), circle((0.0, 0.0), 5.0)),
                section(level(10.0), circle((0.0, 0.0), 5.0)),
                section(level(10.5), circle((40.0, 0.0), 5.0)),
            ]),
        ),
        ["LOFT_SKIN_FOLDS"]
    );
}

// ---------------------------------------------------------------------------
// Scripting
// ---------------------------------------------------------------------------

const SCRIPT: &str = r#"
let base = sketch(on: "XY", entities: [rect(width: 24, height: 24)], label: "base");
let belly = sketch(on: plane(from: "XY", offset: 15), entities: [spline(points: [[-14, -12], [14, -12], [14, 12], [-14, 12]], closed: true)], label: "belly");
let neck = sketch(on: plane(from: "XY", offset: 30), entities: [circle(radius: 6)], label: "neck");
let vase = loft(sections: [base, belly, neck], label: "vase");
let slot = sketch(on: plane(from: "XY", offset: 50), entities: [
    spline(control_points: [[10, 0], [8, 7], [-8, 7], [-10, 0]], degree: 2),
    line(start: [-10, 0], end: [10, 0]),
], label: "slot");
let bar = extrude(sketch: slot, distance: 5, label: "bar");
"#;

#[test]
fn a_scripted_spline_and_smooth_loft_run_and_decompile_to_themselves() {
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
    assert_eq!(
        session.step_reports["vase"].rung.as_deref(),
        Some("loft/skinned")
    );
    assert_eq!(
        session.step_reports["bar"].rung.as_deref(),
        Some("extrusion/spline-profile")
    );
    let report = serde_json::to_value(session.report()).expect("the report serializes");
    assert!(
        report["body"]["surfaces"]["bspline"].as_u64() > Some(0),
        "{}",
        report["body"]["surfaces"]
    );

    let written = artificer_kernel::api::decompile::decompile_journal(
        &session.journal,
        &artificer_kernel::api::decompile::DecompileOptions::default(),
    )
    .expect("the journal decompiles");
    assert!(written.contains("spline(points:"), "{written}");
    assert!(written.contains("spline(control_points:"), "{written}");
    let again = compile_script(&written, &BTreeMap::new()).expect("the decompiled script compiles");
    assert_eq!(again, commands, "{written}");
}

#[test]
fn the_spline_vase_example_answers_as_its_comments_say() {
    let source = include_str!("../examples/spline_vase.art");
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    assert_eq!(
        session.step_reports["vase"].rung.as_deref(),
        Some("loft/skinned")
    );
    assert_eq!(
        session.step_reports["pocket"].rung.as_deref(),
        Some("face-feature/faceted")
    );
}

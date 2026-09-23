//! Tensor-product B-spline surfaces, and fitting them to the scan
//! regions no analytic surface describes.
//!
//! The analytic vocabulary — plane, cylinder, sphere, cone, torus —
//! covers what a machinist makes, and everything upstream of here is
//! built to prefer it. What it cannot cover is a surface that was
//! *designed* free: a moulded crown, a blended organic cap, a styling
//! surface. Until the kernel could hold one, such a region either stayed
//! measured mesh or was carved into facets that each passed tolerance
//! and together described nothing. A B-spline patch is the honest
//! carrier for it.
//!
//! The fit follows the usual reverse-engineering recipe, each step for a
//! reason:
//!
//! - **A chart first.** A tensor-product patch is a map from a rectangle,
//!   so every sample needs a starting `(u, v)`. The samples are projected
//!   onto a *base surface* — the region's own PCA plane, or an unrolled
//!   cylinder or sphere fitted to it when the region curls too far for a
//!   plane — and the base that holds the region as a height field with
//!   the least distortion is taken. A region that folds over every base
//!   tried is refused by name: no single patch can carry it.
//! - **Regularized least squares.** The control net solves the sample
//!   residuals plus a thin-plate energy `∫∫ S_uu² + 2 S_uv² + S_vv²`,
//!   integrated exactly by Gauss quadrature over the knot cells, so the
//!   penalty means the same thing on a refined, non-uniform knot vector
//!   as on a uniform one. Its length scale is the sample spacing: shape
//!   at that scale is noise, shape at ten times it passes untouched.
//!   It is also what keeps control points with no data under them —
//!   the corners of the parameter rectangle, a scanner's dropout —
//!   quiet instead of free.
//! - **Parameter correction.** The chart's parameters are only a first
//!   guess: after each solve every sample is re-projected onto the new
//!   surface (Newton point inversion from its current parameters) and the
//!   net solved again. This is the optimisation after the fact — the fit
//!   moves the parameters to where the surface actually is, rather than
//!   where the base surface guessed.
//! - **Adaptive refinement.** The fit starts as one cubic Bézier patch
//!   (4 x 4 control points, more along a long side) and inserts knots
//!   only in the knot spans whose residual is still systematic — above
//!   the scan's own noise floor — until every span is at the floor, the
//!   control budget is spent, or a span would hold too few samples to
//!   carry more freedom.
//! - **Robust weighting.** Residuals are reweighted by Huber's function
//!   about a median-based scale, and trimmed well past it, so a
//!   scanner's spikes cannot drag the surface; how many were trimmed is
//!   reported with the fit.

use artificer_geometry::{Point3, Vector3};

use crate::fit::{DeviationStats, fit_cylinder, fit_sphere};
use crate::numeric::{BandedSpd, sym_eigen_3x3};
use crate::transform::{normalize, orthonormal_basis};

/// The highest degree evaluated here. Cubic is the default and the
/// industry's; nothing asks for more than quintic.
pub const MAX_DEGREE: usize = 5;

/// Basis values and their first two derivatives: `[order][function]`.
type Basis = [[f64; MAX_DEGREE + 1]; 3];

/// A clamped, non-rational tensor-product B-spline surface.
///
/// Parameters are in the millimetres of the chart the surface was fitted
/// over, so a knot span reads directly as a length on the part.
#[derive(Clone, Debug, PartialEq)]
pub struct BSplineSurface {
    pub degree_u: usize,
    pub degree_v: usize,
    /// Full knot vectors, each end repeated `degree + 1` times.
    pub knots_u: Vec<f64>,
    pub knots_v: Vec<f64>,
    pub count_u: usize,
    pub count_v: usize,
    /// Control points, row-major: `control[i * count_v + j]` is `P(i, j)`
    /// with `i` along u.
    pub control: Vec<Point3>,
}

/// A point on a surface with its partial derivatives.
#[derive(Clone, Copy, Debug)]
pub struct SurfacePoint {
    pub point: Point3,
    pub du: Vector3,
    pub dv: Vector3,
    pub duu: Vector3,
    pub duv: Vector3,
    pub dvv: Vector3,
}

/// Where a point lands on a surface.
#[derive(Clone, Copy, Debug)]
pub struct Projection {
    pub u: f64,
    pub v: f64,
    pub point: Point3,
    /// Distance from the surface, signed by its normal `S_u x S_v`.
    pub distance: f64,
}

/// A clamped uniform knot vector over `[start, end]` with `spans` spans.
pub fn clamped_knots(degree: usize, spans: usize, start: f64, end: f64) -> Vec<f64> {
    let spans = spans.max(1);
    let mut knots = vec![start; degree + 1];
    knots.extend((1..spans).map(|k| start + (end - start) * k as f64 / spans as f64));
    knots.extend(std::iter::repeat_n(end, degree + 1));
    knots
}

/// The knot span holding `t`: the index `k` with `knots[k] <= t <
/// knots[k + 1]`, clamped so the domain's far end falls in the last
/// non-empty span.
fn span_of(knots: &[f64], degree: usize, count: usize, t: f64) -> usize {
    knots
        .partition_point(|&knot| knot <= t)
        .saturating_sub(1)
        .clamp(degree, count - 1)
}

/// The non-zero basis functions of `degree` at `t` in `span`, and their
/// derivatives up to `order` (at most 2): `out[k][j]` is the `k`th
/// derivative of `N(span - degree + j)`. The NURBS Book's algorithm A2.3.
fn basis(knots: &[f64], degree: usize, span: usize, t: f64, order: usize) -> Basis {
    let p = degree;
    let mut ndu = [[0.0f64; MAX_DEGREE + 1]; MAX_DEGREE + 1];
    let mut left = [0.0f64; MAX_DEGREE + 1];
    let mut right = [0.0f64; MAX_DEGREE + 1];
    ndu[0][0] = 1.0;
    for j in 1..=p {
        left[j] = t - knots[span + 1 - j];
        right[j] = knots[span + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            ndu[j][r] = right[r + 1] + left[j - r];
            let temp = ndu[r][j - 1] / ndu[j][r];
            ndu[r][j] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        ndu[j][j] = saved;
    }
    let mut out: Basis = [[0.0; MAX_DEGREE + 1]; 3];
    for j in 0..=p {
        out[0][j] = ndu[j][p];
    }
    let order = order.min(p).min(2);
    let mut a = [[0.0f64; MAX_DEGREE + 1]; 2];
    for r in 0..=p {
        let (mut s1, mut s2) = (0usize, 1usize);
        a[0][0] = 1.0;
        for k in 1..=order {
            let mut d = 0.0;
            let rk = r as isize - k as isize;
            let pk = p - k;
            if rk >= 0 {
                a[s2][0] = a[s1][0] / ndu[pk + 1][rk as usize];
                d = a[s2][0] * ndu[rk as usize][pk];
            }
            let j1 = if rk >= -1 { 1 } else { (-rk) as usize };
            let j2 = if r as isize - 1 <= pk as isize {
                k - 1
            } else {
                p - r
            };
            for j in j1..=j2 {
                let index = (rk + j as isize) as usize;
                a[s2][j] = (a[s1][j] - a[s1][j - 1]) / ndu[pk + 1][index];
                d += a[s2][j] * ndu[index][pk];
            }
            if r <= pk {
                a[s2][k] = -a[s1][k - 1] / ndu[pk + 1][r];
                d += a[s2][k] * ndu[r][pk];
            }
            out[k][r] = d;
            std::mem::swap(&mut s1, &mut s2);
        }
    }
    let mut factor = p as f64;
    for (k, row) in out.iter_mut().enumerate().take(order + 1).skip(1) {
        for value in row.iter_mut().take(p + 1) {
            *value *= factor;
        }
        factor *= (p - k) as f64;
    }
    out
}

impl BSplineSurface {
    /// The parameter rectangle `((u0, u1), (v0, v1))`.
    pub fn domain(&self) -> ((f64, f64), (f64, f64)) {
        (
            (self.knots_u[self.degree_u], self.knots_u[self.count_u]),
            (self.knots_v[self.degree_v], self.knots_v[self.count_v]),
        )
    }

    fn clamp(&self, u: f64, v: f64) -> (f64, f64) {
        let ((u0, u1), (v0, v1)) = self.domain();
        (u.clamp(u0, u1), v.clamp(v0, v1))
    }

    /// The control-net size, `count_u x count_v`.
    pub fn net(&self) -> (usize, usize) {
        (self.count_u, self.count_v)
    }

    /// The distinct knot values strictly inside each domain, i.e. the
    /// span boundaries a refinement inserted.
    pub fn interior_knots(&self) -> (usize, usize) {
        (
            self.count_u - self.degree_u - 1,
            self.count_v - self.degree_v - 1,
        )
    }

    pub fn evaluate(&self, u: f64, v: f64) -> Point3 {
        self.derivatives(u, v, 0).point
    }

    /// The point at `(u, v)` (clamped to the domain) and its partial
    /// derivatives up to `order` (at most 2).
    // The tensor-product sums here and below index several basis rows by
    // one function index at once, which indices say more plainly than
    // zipped iterators.
    #[allow(clippy::needless_range_loop)]
    pub fn derivatives(&self, u: f64, v: f64, order: usize) -> SurfacePoint {
        let (u, v) = self.clamp(u, v);
        let (p, q) = (self.degree_u, self.degree_v);
        let span_u = span_of(&self.knots_u, p, self.count_u, u);
        let span_v = span_of(&self.knots_v, q, self.count_v, v);
        let order = order.min(2);
        let bu = basis(&self.knots_u, p, span_u, u, order);
        let bv = basis(&self.knots_v, q, span_v, v, order);
        let mut sum = [[Vector3::default(); 3]; 3];
        for a in 0..=p {
            let row = (span_u - p + a) * self.count_v;
            for b in 0..=q {
                let control = self.control[row + span_v - q + b] - Point3::default();
                for k in 0..=order {
                    for l in 0..=order - k {
                        sum[k][l] = sum[k][l] + control * (bu[k][a] * bv[l][b]);
                    }
                }
            }
        }
        SurfacePoint {
            point: Point3::default() + sum[0][0],
            du: sum[1][0],
            dv: sum[0][1],
            duu: sum[2][0],
            duv: sum[1][1],
            dvv: sum[0][2],
        }
    }

    /// The unit normal `S_u x S_v`, where the surface is regular.
    pub fn normal(&self, u: f64, v: f64) -> Option<Vector3> {
        let here = self.derivatives(u, v, 1);
        normalize(here.du.cross(here.dv))
    }

    /// Projects `target` onto the surface by Newton's method from `seed`
    /// (the NURBS Book's point inversion), with step halving so no
    /// iteration moves further from the target, and the parameters
    /// clamped to the domain.
    ///
    /// Where the full Hessian is not positive definite — far from the
    /// surface, across a fold of the distance function — the step falls
    /// back to Gauss–Newton, whose matrix is the first fundamental form
    /// and always a descent direction. For points within noise of the
    /// surface, which is every point the fit asks about, the two agree
    /// and a warm start converges in two or three steps.
    pub fn project(&self, target: Point3, seed: (f64, f64)) -> Projection {
        const ITERATIONS: usize = 24;
        let (mut u, mut v) = self.clamp(seed.0, seed.1);
        let mut here = self.derivatives(u, v, 2);
        let mut gap = here.point - target;
        let mut squared = gap.dot(gap);
        for _ in 0..ITERATIONS {
            let (a, b, c) = (
                here.du.dot(here.du),
                here.du.dot(here.dv),
                here.dv.dot(here.dv),
            );
            let (gu, gv) = (gap.dot(here.du), gap.dot(here.dv));
            let newton = (
                a + gap.dot(here.duu),
                b + gap.dot(here.duv),
                c + gap.dot(here.dvv),
            );
            let newton_determinant = newton.0 * newton.2 - newton.1 * newton.1;
            let (h00, h01, h11) = if newton.0 > 0.0 && newton_determinant > 1e-18 * (a * c) {
                newton
            } else {
                (a, b, c)
            };
            let determinant = h00 * h11 - h01 * h01;
            if determinant.is_nan() || determinant <= 1e-18 * (a * c).max(1e-300) {
                break;
            }
            let step_u = -(h11 * gu - h01 * gv) / determinant;
            let step_v = -(h00 * gv - h01 * gu) / determinant;
            let mut scale = 1.0;
            let mut moved = false;
            for _ in 0..8 {
                let (nu, nv) = self.clamp(u + scale * step_u, v + scale * step_v);
                let trial = self.derivatives(nu, nv, 2);
                let trial_gap = trial.point - target;
                let trial_squared = trial_gap.dot(trial_gap);
                if trial_squared <= squared {
                    let travel = ((nu - u) * (nu - u) * a + (nv - v) * (nv - v) * c).sqrt();
                    (u, v, here, gap, squared) = (nu, nv, trial, trial_gap, trial_squared);
                    moved = travel > 1e-10;
                    break;
                }
                scale *= 0.5;
            }
            if !moved {
                break;
            }
        }
        let normal = normalize(here.du.cross(here.dv));
        let offset = target - here.point;
        let distance = match normal {
            Some(normal) => offset.length().copysign(offset.dot(normal)),
            None => offset.length(),
        };
        Projection {
            u,
            v,
            point: here.point,
            distance,
        }
    }

    /// The closest point from anywhere: a coarse grid over the domain
    /// seeds the projection.
    pub fn closest_point(&self, target: Point3) -> Projection {
        const GRID: usize = 12;
        let ((u0, u1), (v0, v1)) = self.domain();
        let mut best = (f64::INFINITY, (u0, v0));
        for i in 0..=GRID {
            for j in 0..=GRID {
                let u = u0 + (u1 - u0) * i as f64 / GRID as f64;
                let v = v0 + (v1 - v0) * j as f64 / GRID as f64;
                let gap = self.evaluate(u, v) - target;
                let squared = gap.dot(gap);
                if squared < best.0 {
                    best = (squared, (u, v));
                }
            }
        }
        self.project(target, best.1)
    }
}

/// How a region's samples get their first parameters: a base surface
/// they are projected onto, in millimetres of that surface.
#[derive(Clone, Copy, Debug)]
pub enum Chart {
    /// Orthogonal projection onto the region's PCA plane, along its two
    /// principal directions.
    Plane {
        origin: Point3,
        u_axis: Vector3,
        v_axis: Vector3,
        normal: Vector3,
    },
    /// A cylinder unrolled: arc length at its radius from a cut placed
    /// in the region's widest angular gap, and height along its axis.
    Cylinder {
        axis_point: Point3,
        axis: Vector3,
        radius: f64,
        e1: Vector3,
        e2: Vector3,
        cut: f64,
    },
    /// A sphere mapped azimuthally-equidistant about a pole through the
    /// region: geodesic distance from the pole, in the pole's own frame.
    Sphere {
        center: Point3,
        radius: f64,
        pole: Vector3,
        e1: Vector3,
        e2: Vector3,
    },
}

impl Chart {
    /// The chart coordinates of a point, in millimetres.
    pub fn map(&self, point: Point3) -> (f64, f64) {
        match *self {
            Chart::Plane {
                origin,
                u_axis,
                v_axis,
                ..
            } => {
                let d = point - origin;
                (d.dot(u_axis), d.dot(v_axis))
            }
            Chart::Cylinder {
                axis_point,
                axis,
                radius,
                e1,
                e2,
                cut,
            } => {
                let d = point - axis_point;
                let theta = (d.dot(e2).atan2(d.dot(e1)) - cut).rem_euclid(std::f64::consts::TAU);
                (radius * theta, d.dot(axis))
            }
            Chart::Sphere {
                center,
                radius,
                pole,
                e1,
                e2,
            } => {
                let d = point - center;
                let length = d.length().max(1e-12);
                let polar = (d.dot(pole) / length).clamp(-1.0, 1.0).acos();
                let azimuth = d.dot(e2).atan2(d.dot(e1));
                (
                    radius * polar * azimuth.cos(),
                    radius * polar * azimuth.sin(),
                )
            }
        }
    }

    /// The chart's height direction at a point: what a sample's normal
    /// must roughly agree with for the region to be a height field.
    fn up(&self, point: Point3) -> Option<Vector3> {
        match *self {
            Chart::Plane { normal, .. } => Some(normal),
            Chart::Cylinder {
                axis_point, axis, ..
            } => {
                let d = point - axis_point;
                normalize(d - axis * d.dot(axis))
            }
            Chart::Sphere { center, .. } => normalize(point - center),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Chart::Plane { .. } => "plane",
            Chart::Cylinder { .. } => "cylinder",
            Chart::Sphere { .. } => "sphere",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Chart::Plane { normal, .. } => format!(
                "plane chart, normal ({:+.3} {:+.3} {:+.3})",
                normal.x, normal.y, normal.z
            ),
            Chart::Cylinder { radius, axis, .. } => format!(
                "cylinder chart r {radius:.2}, axis ({:+.3} {:+.3} {:+.3})",
                axis.x, axis.y, axis.z
            ),
            Chart::Sphere { radius, .. } => format!("sphere chart r {radius:.2}"),
        }
    }
}

/// Why a region got no patch. Each is a statement about the region, so
/// the report can say which regions stayed measured and why.
#[derive(Clone, Debug, PartialEq)]
pub enum SplineRefusal {
    /// Too few samples to carry even one patch without inventing shape.
    TooSmall { samples: usize, needed: usize },
    /// Folds over every base tried: a single tensor-product patch would
    /// have to double back on itself. The share of area that folds is
    /// given per base.
    NotAHeightField { tried: Vec<String> },
    /// The normal equations would not solve, or the chart collapsed.
    IllConditioned { reason: String },
    /// The control budget ran out before the patch reached tolerance.
    OutOfTolerance {
        rms: f64,
        max: f64,
        tolerance: f64,
        net: (usize, usize),
    },
}

impl std::fmt::Display for SplineRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SplineRefusal::TooSmall { samples, needed } => write!(
                f,
                "too small for a patch: {samples} samples where at least {needed} are needed"
            ),
            SplineRefusal::NotAHeightField { tried } => write!(
                f,
                "not a height field over any base tried ({})",
                tried.join("; ")
            ),
            SplineRefusal::IllConditioned { reason } => write!(f, "ill-conditioned: {reason}"),
            SplineRefusal::OutOfTolerance {
                rms,
                max,
                tolerance,
                net,
            } => write!(
                f,
                "out of tolerance at the control budget: rms {rms:.4} (max {max:.3}) against \
                 {tolerance:.3} with a {} x {} net",
                net.0, net.1
            ),
        }
    }
}

/// Knobs for [`fit_surface`].
#[derive(Clone, Copy, Debug)]
pub struct SplineFitOptions {
    /// Degree in both directions (2 to [`MAX_DEGREE`]).
    pub degree: usize,
    /// The control-point budget, `count_u * count_v`.
    pub max_control_points: usize,
    /// Knot-insertion rounds.
    pub max_rounds: usize,
    /// Parameter-correction (and reweighting) passes per round.
    pub corrections: usize,
    /// Thin-plate length scale, in sample spacings.
    pub smoothing: f64,
    /// Samples solved against; larger regions are strided down to this.
    pub sample_budget: usize,
}

impl Default for SplineFitOptions {
    fn default() -> Self {
        Self {
            degree: 3,
            max_control_points: 1024,
            max_rounds: 8,
            corrections: 3,
            smoothing: 1.0,
            sample_budget: 20_000,
        }
    }
}

/// A fitted patch and what it cost.
#[derive(Clone, Debug)]
pub struct SplineFit {
    pub surface: BSplineSurface,
    pub chart: Chart,
    /// Every sample against the surface, trimmed outliers included —
    /// the same measure the analytic fits report.
    pub deviation: DeviationStats,
    /// RMS over the samples the robust weighting kept.
    pub inlier_rms: f64,
    pub samples: usize,
    /// Samples trimmed as outliers.
    pub outliers: usize,
    /// Knot-insertion rounds run.
    pub rounds: usize,
    /// Parameter-correction passes run, over all rounds.
    pub corrections: usize,
}

/// Samples per knot cell below which a cell is not asked for more
/// freedom: it could not support it.
const MIN_CELL_SAMPLES: usize = 16;
/// A face steeper than this against the chart's height direction (its
/// cosine) folds over it.
const FOLD_COSINE: f64 = 0.17;
/// The share of area allowed to fold — a scanner-rounded rim bends away
/// from any chart, and a few percent of it is not a fold of the region.
const MAX_FOLDED: f64 = 0.03;
/// A curved base must beat the plane's mean alignment by this much to be
/// preferred: the plane is the simpler chart.
const CURVED_MARGIN: f64 = 0.02;
/// Outliers beyond this share and the samples are not the surface.
const MAX_OUTLIER_SHARE: f64 = 0.05;

/// Picks the base surface that holds the region as a height field with
/// the least distortion. `faces` are `(centroid, unit normal, area)`.
pub fn choose_chart(
    points: &[Point3],
    faces: &[(Point3, Vector3, f64)],
) -> Result<Chart, SplineRefusal> {
    let count = points.len().max(1) as f64;
    let centroid = Point3::default()
        + points
            .iter()
            .fold(Vector3::default(), |sum, p| sum + (*p - Point3::default()))
            / count;
    let mut covariance = [[0.0; 3]; 3];
    for p in points {
        let d = *p - centroid;
        let v = [d.x, d.y, d.z];
        for i in 0..3 {
            for j in 0..3 {
                covariance[i][j] += v[i] * v[j];
            }
        }
    }
    let (_, vectors) = sym_eigen_3x3(covariance);
    let axis = |row: usize| Vector3::new(vectors[row][0], vectors[row][1], vectors[row][2]);
    let extent = {
        let mut low = Vector3::new(f64::MAX, f64::MAX, f64::MAX);
        let mut high = Vector3::new(f64::MIN, f64::MIN, f64::MIN);
        for p in points {
            low = Vector3::new(low.x.min(p.x), low.y.min(p.y), low.z.min(p.z));
            high = Vector3::new(high.x.max(p.x), high.y.max(p.y), high.z.max(p.z));
        }
        (high - low).length()
    };
    let mut candidates: Vec<Chart> = Vec::new();
    if let (Some(u_axis), Some(normal)) = (normalize(axis(2)), normalize(axis(0))) {
        let v_axis = normal.cross(u_axis);
        candidates.push(Chart::Plane {
            origin: centroid,
            u_axis,
            v_axis,
            normal,
        });
    }
    let normals: Vec<(Vector3, f64)> = faces.iter().map(|&(_, n, a)| (n, a)).collect();
    if let Some(cylinder) = fit_cylinder(points, &normals)
        && cylinder.radius < 50.0 * extent
    {
        let (e1, e2) = orthonormal_basis(cylinder.axis);
        let mut angles: Vec<f64> = points
            .iter()
            .map(|p| {
                let d = *p - cylinder.axis_point;
                d.dot(e2).atan2(d.dot(e1))
            })
            .collect();
        angles.sort_by(f64::total_cmp);
        // The cut goes in the widest gap; a region with no real gap
        // wraps the axis and no unrolling holds it.
        let mut gap = (0.0f64, 0.0f64);
        for (index, &angle) in angles.iter().enumerate() {
            let next = if index + 1 < angles.len() {
                angles[index + 1]
            } else {
                angles[0] + std::f64::consts::TAU
            };
            if next - angle > gap.0 {
                gap = (next - angle, angle + (next - angle) / 2.0);
            }
        }
        if gap.0 > 20f64.to_radians() {
            candidates.push(Chart::Cylinder {
                axis_point: cylinder.axis_point,
                axis: cylinder.axis,
                radius: cylinder.radius,
                e1,
                e2,
                cut: gap.1,
            });
        }
    }
    if let Some(sphere) = fit_sphere(points)
        && sphere.radius < 50.0 * extent
        && let Some(pole) = normalize(centroid - sphere.center)
        && (centroid - sphere.center).length() > 0.2 * sphere.radius
    {
        let (e1, e2) = orthonormal_basis(pole);
        let reach = points
            .iter()
            .map(|p| {
                let d = *p - sphere.center;
                (d.dot(pole) / d.length().max(1e-12))
                    .clamp(-1.0, 1.0)
                    .acos()
            })
            .fold(0.0f64, f64::max);
        // The map is singular at the antipode.
        if reach < 0.9 * std::f64::consts::PI {
            candidates.push(Chart::Sphere {
                center: sphere.center,
                radius: sphere.radius,
                pole,
                e1,
                e2,
            });
        }
    }
    let mut tried: Vec<String> = Vec::new();
    let mut accepted: Vec<(Chart, f64)> = Vec::new();
    for chart in candidates {
        let (mut total, mut aligned_sum) = (0.0f64, 0.0f64);
        let mut cosines: Vec<(f64, f64)> = Vec::with_capacity(faces.len());
        for &(center, normal, area) in faces {
            let Some(up) = chart.up(center) else {
                continue;
            };
            let cosine = normal.dot(up);
            cosines.push((cosine, area));
            total += area;
            aligned_sum += area * cosine;
        }
        if total <= 0.0 {
            continue;
        }
        // Which side is "up" is the region's own business.
        let sense = if aligned_sum >= 0.0 { 1.0 } else { -1.0 };
        let (mut folded, mut mean) = (0.0f64, 0.0f64);
        for (cosine, area) in cosines {
            let aligned = sense * cosine;
            mean += area * aligned;
            if aligned < FOLD_COSINE {
                folded += area;
            }
        }
        let (folded, mean) = (folded / total, mean / total);
        tried.push(format!(
            "{} folds {:.1}% of its area",
            chart.kind(),
            100.0 * folded
        ));
        if folded <= MAX_FOLDED {
            accepted.push((chart, mean));
        }
    }
    let plane = accepted
        .iter()
        .find(|(chart, _)| matches!(chart, Chart::Plane { .. }))
        .copied();
    let curved = accepted
        .iter()
        .filter(|(chart, _)| !matches!(chart, Chart::Plane { .. }))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .copied();
    match (plane, curved) {
        (Some((plane, flat)), Some((curve, bent))) => Ok(if bent > flat + CURVED_MARGIN {
            curve
        } else {
            plane
        }),
        (Some((chart, _)), None) | (None, Some((chart, _))) => Ok(chart),
        (None, None) => Err(SplineRefusal::NotAHeightField { tried }),
    }
}

/// Gauss–Legendre nodes and weights on `[-1, 1]`: the roots of the
/// Legendre polynomial `P_n`, found by Newton's method from the usual
/// cosine estimates, with weights `2 / ((1 - x^2) P_n'(x)^2)`. Exact for
/// polynomials of degree `2n - 1`, which is what makes the thin-plate
/// energy of a degree-`p` patch exact at `n = p + 1`.
fn gauss_legendre(points: usize) -> Vec<(f64, f64)> {
    let n = points.max(1);
    // P_n(x) and P_n'(x) by the three-term recurrence.
    let legendre = |x: f64| -> (f64, f64) {
        let (mut previous, mut current) = (1.0, x);
        for k in 2..=n {
            let next = ((2 * k - 1) as f64 * x * current - (k - 1) as f64 * previous) / k as f64;
            previous = current;
            current = next;
        }
        if n == 1 {
            return (x, 1.0);
        }
        (current, n as f64 * (x * current - previous) / (x * x - 1.0))
    };
    (1..=n)
        .map(|i| {
            let mut x = (std::f64::consts::PI * (i as f64 - 0.25) / (n as f64 + 0.5)).cos();
            for _ in 0..100 {
                let (value, slope) = legendre(x);
                let step = value / slope;
                x -= step;
                if step.abs() < 1e-15 {
                    break;
                }
            }
            let (_, slope) = legendre(x);
            (x, 2.0 / ((1.0 - x * x) * slope * slope))
        })
        .collect()
}

/// Quadrature stations along one knot vector: `(span, basis, weight)`
/// for every Gauss point of every non-empty span.
fn stations(knots: &[f64], degree: usize, count: usize) -> Vec<(usize, Basis, f64)> {
    let rule = gauss_legendre(degree + 1);
    let mut out = Vec::new();
    for span in degree..count {
        let (a, b) = (knots[span], knots[span + 1]);
        if b <= a {
            continue;
        }
        let (middle, half) = ((a + b) / 2.0, (b - a) / 2.0);
        for &(node, weight) in &rule {
            let t = middle + half * node;
            out.push((span, basis(knots, degree, span, t, 2), half * weight));
        }
    }
    out
}

/// The thin-plate energy's quadratic form over the control net, scaled
/// by `weight`, in the net's band.
#[allow(clippy::needless_range_loop)]
fn thin_plate(
    knots_u: &[f64],
    knots_v: &[f64],
    degree: usize,
    count_u: usize,
    count_v: usize,
    weight: f64,
) -> BandedSpd {
    let p = degree;
    let band = p * count_v + p;
    let mut matrix = BandedSpd::new(count_u * count_v, band);
    let along_u = stations(knots_u, p, count_u);
    let along_v = stations(knots_v, p, count_v);
    let functions = (p + 1) * (p + 1);
    let mut index = vec![0usize; functions];
    let mut second = vec![[0.0f64; 3]; functions];
    for (span_u, bu, wu) in &along_u {
        for (span_v, bv, wv) in &along_v {
            let w = weight * wu * wv;
            for a in 0..=p {
                for b in 0..=p {
                    let slot = a * (p + 1) + b;
                    index[slot] = (span_u - p + a) * count_v + (span_v - p + b);
                    second[slot] = [
                        bu[2][a] * bv[0][b],
                        bu[1][a] * bv[1][b],
                        bu[0][a] * bv[2][b],
                    ];
                }
            }
            for s in 0..functions {
                for t in 0..=s {
                    let value = second[s][0] * second[t][0]
                        + 2.0 * second[s][1] * second[t][1]
                        + second[s][2] * second[t][2];
                    if value != 0.0 {
                        matrix.add(index[s], index[t], w * value);
                    }
                }
            }
        }
    }
    matrix
}

/// Everything one solve needs besides the knots.
struct Problem<'a> {
    degree: usize,
    samples: &'a [Point3],
    /// Subtracted before solving and added back, for conditioning.
    centroid: Point3,
}

impl Problem<'_> {
    /// Solves the weighted, regularized least-squares control net.
    #[allow(clippy::needless_range_loop)]
    fn solve(
        &self,
        knots_u: &[f64],
        knots_v: &[f64],
        params: &[(f64, f64)],
        weights: &[f64],
        smoothing: &BandedSpd,
    ) -> Result<BSplineSurface, SplineRefusal> {
        let p = self.degree;
        let count_u = knots_u.len() - p - 1;
        let count_v = knots_v.len() - p - 1;
        let mut matrix = smoothing.clone();
        let mut rhs = vec![[0.0f64; 3]; count_u * count_v];
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return Err(SplineRefusal::IllConditioned {
                reason: "every sample was trimmed".to_owned(),
            });
        }
        let functions = (p + 1) * (p + 1);
        let mut index = vec![0usize; functions];
        let mut value = vec![0.0f64; functions];
        for ((point, &(u, v)), &weight) in self.samples.iter().zip(params).zip(weights) {
            if weight <= 0.0 {
                continue;
            }
            let span_u = span_of(knots_u, p, count_u, u);
            let span_v = span_of(knots_v, p, count_v, v);
            let bu = basis(knots_u, p, span_u, u, 0);
            let bv = basis(knots_v, p, span_v, v, 0);
            let w = weight / total;
            let d = *point - self.centroid;
            for a in 0..=p {
                for b in 0..=p {
                    let slot = a * (p + 1) + b;
                    index[slot] = (span_u - p + a) * count_v + (span_v - p + b);
                    value[slot] = bu[0][a] * bv[0][b];
                }
            }
            for s in 0..functions {
                let ws = w * value[s];
                rhs[index[s]][0] += ws * d.x;
                rhs[index[s]][1] += ws * d.y;
                rhs[index[s]][2] += ws * d.z;
                for t in 0..=s {
                    matrix.add(index[s], index[t], ws * value[t]);
                }
            }
        }
        // A whisper of ridge so a net with an unsupported corner still
        // factors; far below anything the data or the smoothing says.
        let ridge = 1e-12 * matrix.max_diagonal();
        for i in 0..matrix.size() {
            matrix.add(i, i, ridge);
        }
        let solution = matrix
            .solve3(&rhs)
            .ok_or_else(|| SplineRefusal::IllConditioned {
                reason: format!(
                    "the {count_u} x {count_v} control net's normal equations are singular"
                ),
            })?;
        Ok(BSplineSurface {
            degree_u: p,
            degree_v: p,
            knots_u: knots_u.to_vec(),
            knots_v: knots_v.to_vec(),
            count_u,
            count_v,
            control: solution
                .into_iter()
                .map(|[x, y, z]| {
                    Point3::new(
                        self.centroid.x + x,
                        self.centroid.y + y,
                        self.centroid.z + z,
                    )
                })
                .collect(),
        })
    }
}

/// Re-projects every sample onto `surface` from its current parameters,
/// updating them in place, and returns the signed residuals.
fn correct_parameters(
    surface: &BSplineSurface,
    samples: &[Point3],
    params: &mut [(f64, f64)],
) -> Vec<f64> {
    samples
        .iter()
        .zip(params.iter_mut())
        .map(|(point, param)| {
            let landed = surface.project(*point, *param);
            *param = (landed.u, landed.v);
            landed.distance
        })
        .collect()
}

/// Huber weights about a median-based scale, with samples far past it
/// trimmed outright.
///
/// The scale is floored at the scan's noise, so on a clean region the
/// weighting does nothing; and the trim never cuts inside twice the
/// tolerance, so shape the current net has not yet resolved — which is
/// the whole bulk of the residual on an early, coarse round — is
/// down-weighted at most, never discarded.
fn robust_weights(residuals: &[f64], noise: f64, tolerance: f64) -> Vec<f64> {
    let mut magnitudes: Vec<f64> = residuals.iter().map(|r| r.abs()).collect();
    magnitudes.sort_by(f64::total_cmp);
    let median = magnitudes
        .get(magnitudes.len() / 2)
        .copied()
        .unwrap_or_default();
    let scale = (1.4826 * median).max(noise).max(1e-6);
    let huber = 2.0 * scale;
    let trim = (5.0 * scale).max(2.0 * tolerance);
    residuals
        .iter()
        .map(|r| {
            let r = r.abs();
            if r > trim {
                0.0
            } else if r > huber {
                huber / r
            } else {
                1.0
            }
        })
        .collect()
}

fn weighted_rms(residuals: &[f64], weights: &[f64]) -> f64 {
    let (mut squared, mut total) = (0.0, 0.0);
    for (r, w) in residuals.iter().zip(weights) {
        squared += w * r * r;
        total += w;
    }
    (squared / total.max(1e-300)).sqrt()
}

/// Fits a B-spline patch to a region.
///
/// `points` are the region's samples (its vertices), `faces` its faces
/// as `(centroid, unit normal, area)` for choosing the chart, `noise`
/// the scan's estimated noise sigma and `tolerance` the RMS a patch
/// must reach.
pub fn fit_surface(
    points: &[Point3],
    faces: &[(Point3, Vector3, f64)],
    tolerance: f64,
    noise: f64,
    options: &SplineFitOptions,
) -> Result<SplineFit, SplineRefusal> {
    let degree = options.degree.clamp(2, MAX_DEGREE);
    let needed = 4 * (degree + 1) * (degree + 1);
    if points.len() < needed {
        return Err(SplineRefusal::TooSmall {
            samples: points.len(),
            needed,
        });
    }
    let chart = choose_chart(points, faces)?;
    // The domain covers every point, not just the solved samples, so the
    // region's boundary lies inside it however the samples were strided.
    let (mut u0, mut u1, mut v0, mut v1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &point in points {
        let (u, v) = chart.map(point);
        (u0, u1, v0, v1) = (u0.min(u), u1.max(u), v0.min(v), v1.max(v));
    }
    let area: f64 = faces.iter().map(|f| f.2).sum();
    let (width, height) = (u1 - u0, v1 - v0);
    if !(width > 1e-6 && height > 1e-6 && area > 0.0) {
        return Err(SplineRefusal::IllConditioned {
            reason: format!("the region's chart collapses to {width:.2e} x {height:.2e} mm"),
        });
    }
    // A strip a couple of samples wide has nothing to say across itself:
    // every control row but the middle one would be set by the smoothing
    // alone. That is a curve with a width, not a surface.
    let point_spacing = (area / points.len() as f64).sqrt();
    let narrow = width.min(height).min(area / width.max(height));
    if narrow < 2.0 * point_spacing {
        return Err(SplineRefusal::IllConditioned {
            reason: format!(
                "a strip {narrow:.2} mm across at {point_spacing:.2} mm sample spacing is too \
                 narrow to carry a surface"
            ),
        });
    }
    let stride = points
        .len()
        .div_ceil(options.sample_budget.max(needed))
        .max(1);
    let samples: Vec<Point3> = points.iter().step_by(stride).copied().collect();
    let mut params: Vec<(f64, f64)> = samples.iter().map(|&p| chart.map(p)).collect();
    // Spacing of the solved samples: the finest scale they can speak to.
    let spacing = (area / samples.len() as f64).sqrt();
    let smoothing_length = options.smoothing * spacing;
    let smoothing_weight = smoothing_length.powi(4) / (width * height);
    // A span narrower than this could not hold enough samples to be
    // worth its freedom.
    let min_span = (MIN_CELL_SAMPLES as f64).sqrt() * spacing;
    // Systematic residual is anything the noise cannot explain.
    let target = (1.5 * noise).max(0.25 * tolerance);
    let aspect = width / height;
    let (mut spans_u, mut spans_v) = if aspect >= 1.0 {
        ((aspect.round() as usize).clamp(1, 4), 1)
    } else {
        (1, ((1.0 / aspect).round() as usize).clamp(1, 4))
    };
    let mut knots_u = clamped_knots(degree, spans_u, u0, u1);
    let mut knots_v = clamped_knots(degree, spans_v, v0, v1);
    let problem = Problem {
        degree,
        samples: &samples,
        centroid: Point3::default()
            + samples
                .iter()
                .fold(Vector3::default(), |sum, p| sum + (*p - Point3::default()))
                / samples.len() as f64,
    };
    let mut weights = vec![1.0f64; samples.len()];
    let (mut rounds, mut corrections) = (0usize, 0usize);
    let (surface, residuals) = loop {
        rounds += 1;
        let count_u = spans_u + degree;
        let count_v = spans_v + degree;
        let smoothing = thin_plate(
            &knots_u,
            &knots_v,
            degree,
            count_u,
            count_v,
            smoothing_weight,
        );
        let mut surface = problem.solve(&knots_u, &knots_v, &params, &weights, &smoothing)?;
        let mut residuals = correct_parameters(&surface, &samples, &mut params);
        let mut previous = weighted_rms(&residuals, &weights);
        for _ in 0..options.corrections {
            weights = robust_weights(&residuals, noise, tolerance);
            surface = problem.solve(&knots_u, &knots_v, &params, &weights, &smoothing)?;
            residuals = correct_parameters(&surface, &samples, &mut params);
            corrections += 1;
            let now = weighted_rms(&residuals, &weights);
            // Settled: the parameters have stopped moving the answer.
            if now > 0.995 * previous {
                break;
            }
            previous = now;
        }
        if rounds >= options.max_rounds {
            break (surface, residuals);
        }
        // Where is the residual still systematic?
        let mut cells = vec![(0.0f64, 0usize); spans_u * spans_v];
        for ((&(u, v), &r), &w) in params.iter().zip(&residuals).zip(&weights) {
            if w <= 0.0 {
                continue;
            }
            let cu = span_of(&knots_u, degree, count_u, u) - degree;
            let cv = span_of(&knots_v, degree, count_v, v) - degree;
            let cell = &mut cells[cu * spans_v + cv];
            cell.0 += r * r;
            cell.1 += 1;
        }
        let mut score_u = vec![0.0f64; spans_u];
        let mut score_v = vec![0.0f64; spans_v];
        for cu in 0..spans_u {
            for cv in 0..spans_v {
                let (squared, count) = cells[cu * spans_v + cv];
                if count < MIN_CELL_SAMPLES {
                    continue;
                }
                let rms = (squared / count as f64).sqrt();
                if rms > target {
                    score_u[cu] = score_u[cu].max(rms);
                    score_v[cv] = score_v[cv].max(rms);
                }
            }
        }
        // Split the worst spans first, while the budget and the sample
        // density both allow.
        let mut wanted: Vec<(f64, bool, usize)> = Vec::new();
        for (span, &score) in score_u.iter().enumerate() {
            let length = knots_u[degree + span + 1] - knots_u[degree + span];
            if score > 0.0 && length >= 2.0 * min_span {
                wanted.push((score, true, span));
            }
        }
        for (span, &score) in score_v.iter().enumerate() {
            let length = knots_v[degree + span + 1] - knots_v[degree + span];
            if score > 0.0 && length >= 2.0 * min_span {
                wanted.push((score, false, span));
            }
        }
        wanted.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let (mut add_u, mut add_v): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        for (_, along_u, span) in wanted {
            let (next_u, next_v) = if along_u {
                (count_u + add_u.len() + 1, count_v + add_v.len())
            } else {
                (count_u + add_u.len(), count_v + add_v.len() + 1)
            };
            if next_u * next_v > options.max_control_points {
                continue;
            }
            if along_u {
                add_u.push(span);
            } else {
                add_v.push(span);
            }
        }
        if add_u.is_empty() && add_v.is_empty() {
            break (surface, residuals);
        }
        let insert = |knots: &mut Vec<f64>, spans: &[usize]| {
            let mut middles: Vec<f64> = spans
                .iter()
                .map(|&span| (knots[degree + span] + knots[degree + span + 1]) / 2.0)
                .collect();
            knots.append(&mut middles);
            knots.sort_by(f64::total_cmp);
        };
        insert(&mut knots_u, &add_u);
        insert(&mut knots_v, &add_v);
        spans_u += add_u.len();
        spans_v += add_v.len();
    };
    let deviation = crate::fit::stats(residuals.iter().copied());
    let inliers: Vec<f64> = residuals
        .iter()
        .zip(&weights)
        .filter(|(_, w)| **w > 0.0)
        .map(|(r, _)| *r)
        .collect();
    let outliers = samples.len() - inliers.len();
    let inlier_rms = crate::fit::stats(inliers.into_iter()).rms;
    if inlier_rms > tolerance || outliers as f64 > MAX_OUTLIER_SHARE * samples.len() as f64 {
        return Err(SplineRefusal::OutOfTolerance {
            rms: inlier_rms,
            max: deviation.max_abs,
            tolerance,
            net: surface.net(),
        });
    }
    Ok(SplineFit {
        surface,
        chart,
        deviation,
        inlier_rms,
        samples: samples.len(),
        outliers,
        rounds,
        corrections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64 and Box–Muller: the same deterministic noise the
    /// simulator uses, local so the tests own their stream.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        }

        fn gaussian(&mut self) -> f64 {
            let (a, b) = (self.next().max(1e-300), self.next());
            (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
        }
    }

    /// `z = x^2 + y^2` over `[-1, 1]^2` is a quadratic Bézier patch
    /// exactly: the control heights along each direction are 1, -1, 1.
    fn paraboloid() -> BSplineSurface {
        let c = [1.0, -1.0, 1.0];
        let mut control = Vec::new();
        for i in 0..3 {
            for j in 0..3 {
                control.push(Point3::new(i as f64 - 1.0, j as f64 - 1.0, c[i] + c[j]));
            }
        }
        BSplineSurface {
            degree_u: 2,
            degree_v: 2,
            knots_u: vec![-1.0, -1.0, -1.0, 1.0, 1.0, 1.0],
            knots_v: vec![-1.0, -1.0, -1.0, 1.0, 1.0, 1.0],
            count_u: 3,
            count_v: 3,
            control,
        }
    }

    /// A height field sampled as a triangulated grid: vertices, and faces
    /// as `(centroid, normal, area)`.
    #[allow(clippy::type_complexity)]
    fn height_field(
        f: impl Fn(f64, f64) -> f64,
        half: (f64, f64),
        steps: (usize, usize),
        sigma: f64,
        seed: u64,
    ) -> (Vec<Point3>, Vec<(Point3, Vector3, f64)>) {
        let mut noise = Noise(seed);
        let mut points = Vec::new();
        for i in 0..=steps.0 {
            for j in 0..=steps.1 {
                let x = -half.0 + 2.0 * half.0 * i as f64 / steps.0 as f64;
                let y = -half.1 + 2.0 * half.1 * j as f64 / steps.1 as f64;
                points.push(Point3::new(x, y, f(x, y) + sigma * noise.gaussian()));
            }
        }
        let at = |i: usize, j: usize| points[i * (steps.1 + 1) + j];
        let mut faces = Vec::new();
        for i in 0..steps.0 {
            for j in 0..steps.1 {
                for [a, b, c] in [
                    [at(i, j), at(i + 1, j), at(i + 1, j + 1)],
                    [at(i, j), at(i + 1, j + 1), at(i, j + 1)],
                ] {
                    let cross = (b - a).cross(c - a);
                    let centroid = Point3::new(
                        (a.x + b.x + c.x) / 3.0,
                        (a.y + b.y + c.y) / 3.0,
                        (a.z + b.z + c.z) / 3.0,
                    );
                    faces.push((centroid, cross / cross.length(), cross.length() / 2.0));
                }
            }
        }
        (points, faces)
    }

    #[test]
    fn basis_functions_partition_unity_and_differentiate_correctly() {
        let knots = vec![0.0, 0.0, 0.0, 0.0, 0.3, 0.5, 0.9, 1.0, 1.0, 1.0, 1.0];
        let (degree, count) = (3, 7);
        for t in [0.0, 0.1, 0.3, 0.42, 0.77, 0.9, 1.0] {
            let span = span_of(&knots, degree, count, t);
            let b = basis(&knots, degree, span, t, 2);
            let sum: f64 = b[0][..=degree].iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-12,
                "partition of unity at {t}: {sum}"
            );
            // Derivatives sum to zero (the sum is constant) and match a
            // central difference taken inside the same span.
            let d1: f64 = b[1][..=degree].iter().sum();
            assert!(d1.abs() < 1e-9, "derivative sum {d1}");
            let h = 1e-6;
            let (lo, hi) = (
                (t - h).max(knots[span]),
                (t + h).min(knots[span + 1] - 1e-12),
            );
            let bl = basis(&knots, degree, span, lo, 0);
            let bh = basis(&knots, degree, span, hi, 0);
            for j in 0..=degree {
                // One-sided at the domain's ends, so the difference is
                // only first-order accurate there: judge it relatively.
                let numeric = (bh[0][j] - bl[0][j]) / (hi - lo);
                assert!(
                    (numeric - b[1][j]).abs() < 1e-4 * (1.0 + b[1][j].abs()),
                    "N'{j}({t}) = {} vs {numeric}",
                    b[1][j]
                );
            }
        }
    }

    #[test]
    fn gauss_legendre_integrates_polynomials_to_its_degree_exactly() {
        for n in 1..=6 {
            let rule = gauss_legendre(n);
            assert_eq!(rule.len(), n);
            // Exact through degree 2n - 1: every even power of x
            // integrates to 2 / (k + 1) over [-1, 1], every odd to zero.
            for k in 0..2 * n {
                let sum: f64 = rule.iter().map(|&(x, w)| w * x.powi(k as i32)).sum();
                let exact = if k % 2 == 0 {
                    2.0 / (k as f64 + 1.0)
                } else {
                    0.0
                };
                assert!(
                    (sum - exact).abs() < 1e-13,
                    "n {n}, x^{k}: {sum} vs {exact}"
                );
            }
        }
    }

    #[test]
    fn an_exact_net_evaluates_and_differentiates_its_surface() {
        let surface = paraboloid();
        for (u, v) in [(-1.0, -1.0), (0.3, -0.4), (0.0, 0.0), (0.9, 1.0)] {
            let here = surface.derivatives(u, v, 2);
            let (x, y) = (here.point.x, here.point.y);
            assert!((x - u).abs() < 1e-12 && (y - v).abs() < 1e-12);
            assert!((here.point.z - (u * u + v * v)).abs() < 1e-12);
            assert!((here.du.z - 2.0 * u).abs() < 1e-12);
            assert!((here.dv.z - 2.0 * v).abs() < 1e-12);
            assert!((here.duu.z - 2.0).abs() < 1e-12);
            assert!(here.duv.z.abs() < 1e-12);
            assert!((here.dvv.z - 2.0).abs() < 1e-12);
        }
    }

    #[test]
    fn projection_lands_on_the_foot_of_the_perpendicular() {
        let surface = paraboloid();
        // Stand off the surface along its normal at a known foot.
        let (u, v) = (0.35, -0.2);
        let foot = surface.evaluate(u, v);
        let normal = surface.normal(u, v).expect("regular");
        let target = foot + normal * 0.05;
        let landed = surface.project(target, (0.0, 0.0));
        assert!((landed.u - u).abs() < 1e-8 && (landed.v - v).abs() < 1e-8);
        assert!((landed.distance - 0.05).abs() < 1e-9);
        let below = surface.project(foot + normal * -0.05, (u, v));
        assert!((below.distance + 0.05).abs() < 1e-9, "signed by the normal");
        // From well off the surface — inside the bowl, where the
        // distance has a fold and Gauss–Newton alone crawls — the grid
        // seed and the Newton step still land on a foot of the
        // perpendicular, inside the domain.
        let target = Point3::new(0.3, -0.2, 1.5);
        let far = surface.closest_point(target);
        assert!(far.u.abs() < 0.99 && far.v.abs() < 0.99, "{far:?}");
        let gap = far.point - target;
        let here = surface.derivatives(far.u, far.v, 1);
        assert!(
            gap.dot(here.du).abs() < 1e-9 && gap.dot(here.dv).abs() < 1e-9,
            "{far:?}"
        );
        // It is the nearest of the candidates a dense scan would find.
        let mut nearest = f64::INFINITY;
        for i in 0..=200 {
            for j in 0..=200 {
                let (u, v) = (-1.0 + i as f64 / 100.0, -1.0 + j as f64 / 100.0);
                nearest = nearest.min((surface.evaluate(u, v) - target).length());
            }
        }
        assert!(far.distance.abs() <= nearest + 1e-9);
    }

    #[test]
    fn a_noisy_freeform_patch_fits_within_tolerance_of_its_true_surface() {
        // The bench's own freeform surface, noised the way the simulator
        // noises a scan, with a few scanner spikes on top.
        let truth = crate::synth::freeform_top_height;
        let (mut points, faces) = height_field(truth, (40.0, 30.0), (160, 120), 0.02, 7);
        let mut spikes = Noise(99);
        for index in (0..points.len()).step_by(997) {
            points[index].z += 0.8 + spikes.next();
        }
        let fit = fit_surface(&points, &faces, 0.12, 0.02, &SplineFitOptions::default())
            .expect("a smooth height field fits");
        assert!(
            matches!(fit.chart, Chart::Plane { .. }),
            "{}",
            fit.chart.describe()
        );
        // At the noise floor against the scan, spikes trimmed rather
        // than chased.
        assert!(fit.inlier_rms < 0.03, "inlier rms {:.4}", fit.inlier_rms);
        assert!(fit.outliers >= 15, "{} outliers trimmed", fit.outliers);
        assert!(fit.rounds > 1, "refined from the coarse start");
        // And within a stated tolerance of the surface the scan was taken
        // of: the noise is 0.02, so the fit must average it down, not
        // copy it.
        let mut worst = 0.0f64;
        let mut squared = 0.0;
        let mut count = 0usize;
        for i in 0..=60 {
            for j in 0..=40 {
                let (x, y) = (
                    -38.0 + 76.0 * i as f64 / 60.0,
                    -28.0 + 56.0 * j as f64 / 40.0,
                );
                let on = Point3::new(x, y, truth(x, y));
                let landed = fit.surface.closest_point(on);
                worst = worst.max(landed.distance.abs());
                squared += landed.distance * landed.distance;
                count += 1;
            }
        }
        let rms = (squared / count as f64).sqrt();
        println!(
            "net {:?} after {} round(s), {} correction(s): scan rms {:.4} (inliers {:.4}, \
             {} trimmed), truth rms {rms:.4} max {worst:.4}",
            fit.surface.net(),
            fit.rounds,
            fit.corrections,
            fit.deviation.rms,
            fit.inlier_rms,
            fit.outliers
        );
        assert!(rms < 0.01, "rms {rms:.4} from the true surface");
        assert!(worst < 0.05, "worst {worst:.4} from the true surface");
    }

    #[test]
    fn a_curled_patch_is_charted_on_its_cylinder() {
        // A 200 degree band of a ribbed cylinder: past the half-turn no
        // plane holds it, and unrolled it is a gentle height field.
        let (radius, arc) = (15.0, 200f64.to_radians());
        let mut points = Vec::new();
        let (steps_a, steps_h) = (120usize, 30usize);
        let at = |i: usize, j: usize| {
            let theta = -arc / 2.0 + arc * i as f64 / steps_a as f64;
            let h = 20.0 * j as f64 / steps_h as f64;
            let r = radius + 0.4 * (3.0 * theta).sin() * (h / 7.0).cos();
            Point3::new(r * theta.cos(), r * theta.sin(), h)
        };
        for i in 0..=steps_a {
            for j in 0..=steps_h {
                points.push(at(i, j));
            }
        }
        let mut faces = Vec::new();
        for i in 0..steps_a {
            for j in 0..steps_h {
                for [a, b, c] in [
                    [at(i, j), at(i + 1, j), at(i + 1, j + 1)],
                    [at(i, j), at(i + 1, j + 1), at(i, j + 1)],
                ] {
                    let cross = (b - a).cross(c - a);
                    let centroid = Point3::new(
                        (a.x + b.x + c.x) / 3.0,
                        (a.y + b.y + c.y) / 3.0,
                        (a.z + b.z + c.z) / 3.0,
                    );
                    faces.push((centroid, cross / cross.length(), cross.length() / 2.0));
                }
            }
        }
        let fit = fit_surface(&points, &faces, 0.05, 0.0, &SplineFitOptions::default())
            .expect("the unrolled band fits");
        assert!(
            matches!(fit.chart, Chart::Cylinder { .. }),
            "{}",
            fit.chart.describe()
        );
        assert!(fit.deviation.rms < 0.02, "rms {:.4}", fit.deviation.rms);
    }

    #[test]
    fn regions_no_patch_can_carry_are_refused_by_name() {
        // Too few samples.
        let (points, faces) = height_field(|_, _| 0.0, (1.0, 1.0), (3, 3), 0.0, 1);
        assert!(matches!(
            fit_surface(&points, &faces, 0.05, 0.0, &SplineFitOptions::default()),
            Err(SplineRefusal::TooSmall { .. })
        ));
        // A closed tube folds over every base: the plane sees its back,
        // the cylinder has no gap to cut, the sphere no pole.
        let mut points = Vec::new();
        let mut faces = Vec::new();
        for i in 0..72 {
            for j in 0..=10 {
                let theta = std::f64::consts::TAU * i as f64 / 72.0;
                let normal = Vector3::new(theta.cos(), theta.sin(), 0.0);
                let point = Point3::new(10.0 * theta.cos(), 10.0 * theta.sin(), j as f64);
                points.push(point);
                faces.push((point, normal, 1.0));
            }
        }
        let refusal = fit_surface(&points, &faces, 0.05, 0.0, &SplineFitOptions::default())
            .expect_err("a tube is not a height field");
        assert!(
            matches!(refusal, SplineRefusal::NotAHeightField { .. }),
            "{refusal}"
        );
        assert!(refusal.to_string().contains("plane folds"), "{refusal}");
        // A strip two samples wide is a curve with a width: nothing
        // across it for a surface to be fitted to.
        let (points, faces) = height_field(|x, _| 0.01 * x * x, (60.0, 0.4), (150, 1), 0.0, 5);
        let refusal = fit_surface(&points, &faces, 0.05, 0.0, &SplineFitOptions::default())
            .expect_err("too narrow");
        assert!(
            matches!(refusal, SplineRefusal::IllConditioned { .. }),
            "{refusal}"
        );
        // Shape finer than the budget allows is out of tolerance, and
        // says at what net.
        let options = SplineFitOptions {
            max_control_points: 36,
            ..SplineFitOptions::default()
        };
        let (points, faces) = height_field(
            |x, y| 2.0 * (x * 1.3).sin() * (y * 1.1).cos(),
            (20.0, 20.0),
            (100, 100),
            0.0,
            3,
        );
        let refusal = fit_surface(&points, &faces, 0.05, 0.0, &options).expect_err("budget");
        assert!(
            matches!(refusal, SplineRefusal::OutOfTolerance { .. }),
            "{refusal}"
        );
    }

    #[test]
    fn the_fit_is_deterministic() {
        let (points, faces) = height_field(
            crate::synth::freeform_top_height,
            (20.0, 15.0),
            (60, 45),
            0.03,
            11,
        );
        let options = SplineFitOptions::default();
        let first = fit_surface(&points, &faces, 0.15, 0.03, &options).expect("fits");
        let second = fit_surface(&points, &faces, 0.15, 0.03, &options).expect("fits");
        assert_eq!(first.surface, second.surface);
    }
}

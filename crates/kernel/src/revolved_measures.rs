//! Exact measures of a face on a carrier of revolution over any parameter
//! region (ADR 0056, Track B5).
//!
//! The validator's shell measures integrate every face of revolution over
//! its parameter rectangle, which is every face the builders make. The
//! general Boolean makes others: a torus band with a bore through it, a
//! sphere cut by a plane and a bore, a cone face left L-shaped by two
//! rings and a meridian. Each of the integrals those measures need —
//! `∬ F(a)·G(v) du dv` over the region, with `F` a trigonometric polynomial
//! in the azimuth angle `a = s·u` and `G` a polynomial or a trigonometric
//! polynomial in `v` — is turned by Green's theorem into a contour
//! integral `−∮ F(a)·𝒢(v) du` along the face's loops, where `𝒢` is an
//! antiderivative of `G`. Along a ring the contour term is closed form,
//! along a meridian it vanishes, and along any other pcurve — a numerically
//! traced B-spline, a harmonic, a trace — it is Gauss–Legendre quadrature of
//! an integrand analytic on every span, the standing ADR 0026 gave such
//! integrals.
//!
//! The surface is `P(u, v) = O + ρ(v)·r̂(a) + z(v)·A` with `r̂(a) = cos a·U +
//! sin a·V`, and its unnormalised normal is `σ·ρ·(z'·r̂ − ρ'·A)` with `σ` the
//! frame's handedness times the angular sign. Everything below follows from
//! that one form.

use crate::bspline::SplineCurve2;
use crate::revolved::Revolved;
use crate::topology::{Curve2, Face, Point3, Topology, Vector3};

/// A polynomial in one variable, rising powers.
#[derive(Clone, Debug)]
struct Poly(Vec<f64>);

impl Poly {
    fn constant(value: f64) -> Self {
        Self(vec![value])
    }

    fn linear(constant: f64, slope: f64) -> Self {
        Self(vec![constant, slope])
    }

    fn times(&self, other: &Self) -> Self {
        let mut product = vec![0.0; self.0.len() + other.0.len() - 1];
        for (i, a) in self.0.iter().enumerate() {
            for (j, b) in other.0.iter().enumerate() {
                product[i + j] = a.mul_add(*b, product[i + j]);
            }
        }
        Self(product)
    }

    fn plus(&self, other: &Self) -> Self {
        let mut sum = vec![0.0; self.0.len().max(other.0.len())];
        for (index, value) in self.0.iter().enumerate() {
            sum[index] += value;
        }
        for (index, value) in other.0.iter().enumerate() {
            sum[index] += value;
        }
        Self(sum)
    }

    fn scaled(&self, factor: f64) -> Self {
        Self(self.0.iter().map(|value| value * factor).collect())
    }

    /// The antiderivative vanishing at zero, evaluated at `v`.
    fn antiderivative(&self, v: f64) -> f64 {
        self.0
            .iter()
            .enumerate()
            .rev()
            .fold(0.0_f64, |total, (power, coefficient)| {
                total.mul_add(v, coefficient / (power as f64 + 1.0))
            })
            * v
    }
}

/// A polynomial in `cos θ` and `sin θ`, as terms `(coefficient, cosines,
/// sines)`.
#[derive(Clone, Debug)]
struct Trig(Vec<(f64, u32, u32)>);

impl Trig {
    fn constant(value: f64) -> Self {
        Self(vec![(value, 0, 0)])
    }

    fn cosine() -> Self {
        Self(vec![(1.0, 1, 0)])
    }

    fn sine() -> Self {
        Self(vec![(1.0, 0, 1)])
    }

    fn times(&self, other: &Self) -> Self {
        let mut terms = Vec::with_capacity(self.0.len() * other.0.len());
        for (left, left_cos, left_sin) in &self.0 {
            for (right, right_cos, right_sin) in &other.0 {
                terms.push((left * right, left_cos + right_cos, left_sin + right_sin));
            }
        }
        Self(terms)
    }

    fn plus(&self, other: &Self) -> Self {
        let mut terms = self.0.clone();
        terms.extend(other.0.iter().copied());
        Self(terms)
    }

    fn scaled(&self, factor: f64) -> Self {
        Self(
            self.0
                .iter()
                .map(|(coefficient, cosines, sines)| (coefficient * factor, *cosines, *sines))
                .collect(),
        )
    }

    fn evaluate(&self, angle: f64) -> f64 {
        let (sin, cos) = angle.sin_cos();
        self.0
            .iter()
            .map(|(coefficient, cosines, sines)| {
                coefficient * cos.powi(*cosines as i32) * sin.powi(*sines as i32)
            })
            .sum()
    }

    /// The antiderivative vanishing at zero, evaluated at `angle`, by the
    /// classical reduction of `∫ cosᵃ sinᵇ`.
    fn antiderivative(&self, angle: f64) -> f64 {
        self.0
            .iter()
            .map(|(coefficient, cosines, sines)| {
                coefficient
                    * (trig_power_antiderivative(*cosines, *sines, angle)
                        - trig_power_antiderivative(*cosines, *sines, 0.0))
            })
            .sum()
    }
}

fn trig_power_antiderivative(a: u32, b: u32, t: f64) -> f64 {
    let (sin, cos) = t.sin_cos();
    let total = f64::from(a + b);
    match (a, b) {
        (0, 0) => t,
        (1, 0) => sin,
        (0, 1) => -cos,
        (1, 1) => sin * sin / 2.0,
        (_, b) if b >= 2 => {
            -cos.powi(a as i32 + 1) * sin.powi(b as i32 - 1) / total
                + f64::from(b - 1) / total * trig_power_antiderivative(a, b - 2, t)
        }
        _ => {
            cos.powi(a as i32 - 1) * sin.powi(b as i32 + 1) / total
                + f64::from(a - 1) / total * trig_power_antiderivative(a - 2, b, t)
        }
    }
}

/// A function of `v`: a polynomial for a cylinder or a cone, a trigonometric
/// polynomial for a sphere or a torus.
#[derive(Clone, Debug)]
enum Latitude {
    Poly(Poly),
    Trig(Trig),
}

impl Latitude {
    fn antiderivative(&self, v: f64) -> f64 {
        match self {
            Self::Poly(poly) => poly.antiderivative(v),
            Self::Trig(trig) => trig.antiderivative(v),
        }
    }

    fn times(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Poly(a), Self::Poly(b)) => Self::Poly(a.times(b)),
            (Self::Trig(a), Self::Trig(b)) => Self::Trig(a.times(b)),
            _ => unreachable!("one carrier has one kind of latitude"),
        }
    }

    fn plus(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Poly(a), Self::Poly(b)) => Self::Poly(a.plus(b)),
            (Self::Trig(a), Self::Trig(b)) => Self::Trig(a.plus(b)),
            _ => unreachable!("one carrier has one kind of latitude"),
        }
    }

    fn scaled(&self, factor: f64) -> Self {
        match self {
            Self::Poly(poly) => Self::Poly(poly.scaled(factor)),
            Self::Trig(trig) => Self::Trig(trig.scaled(factor)),
        }
    }
}

/// The meridian profile of a carrier as functions of `v`: `ρ`, `z`, `ρ'`,
/// `z'`, the area density `ρ·√(ρ'² + z'²)`, and the constant kind.
struct Meridian {
    rho: Latitude,
    z: Latitude,
    rho_prime: Latitude,
    z_prime: Latitude,
    density: Latitude,
    one: Latitude,
}

fn meridian(revolved: Revolved) -> Meridian {
    match revolved {
        Revolved::Cylinder(cylinder) => Meridian {
            rho: Latitude::Poly(Poly::constant(cylinder.radius)),
            z: Latitude::Poly(Poly::linear(0.0, 1.0)),
            rho_prime: Latitude::Poly(Poly::constant(0.0)),
            z_prime: Latitude::Poly(Poly::constant(1.0)),
            density: Latitude::Poly(Poly::constant(cylinder.radius)),
            one: Latitude::Poly(Poly::constant(1.0)),
        },
        Revolved::Cone(cone) => Meridian {
            rho: Latitude::Poly(Poly::linear(cone.base_radius, cone.slope)),
            z: Latitude::Poly(Poly::linear(0.0, 1.0)),
            rho_prime: Latitude::Poly(Poly::constant(cone.slope)),
            z_prime: Latitude::Poly(Poly::constant(1.0)),
            density: Latitude::Poly(
                Poly::linear(cone.base_radius, cone.slope)
                    .scaled(cone.slope.mul_add(cone.slope, 1.0).sqrt()),
            ),
            one: Latitude::Poly(Poly::constant(1.0)),
        },
        Revolved::Sphere(sphere) => {
            let radius = sphere.radius;
            Meridian {
                rho: Latitude::Trig(Trig::cosine().scaled(radius)),
                z: Latitude::Trig(Trig::sine().scaled(radius)),
                rho_prime: Latitude::Trig(Trig::sine().scaled(-radius)),
                z_prime: Latitude::Trig(Trig::cosine().scaled(radius)),
                density: Latitude::Trig(Trig::cosine().scaled(radius * radius)),
                one: Latitude::Trig(Trig::constant(1.0)),
            }
        }
        Revolved::Torus(torus) => {
            let (major, minor) = (torus.major_radius, torus.minor_radius);
            let rho = Trig::constant(major).plus(&Trig::cosine().scaled(minor));
            Meridian {
                rho: Latitude::Trig(rho.clone()),
                z: Latitude::Trig(Trig::sine().scaled(minor)),
                rho_prime: Latitude::Trig(Trig::sine().scaled(-minor)),
                z_prime: Latitude::Trig(Trig::cosine().scaled(minor)),
                density: Latitude::Trig(rho.scaled(minor)),
                one: Latitude::Trig(Trig::constant(1.0)),
            }
        }
    }
}

/// `∬ F(a)·G(v) du dv` over the face's region, by Green's theorem along
/// its loops: `−∮ F(a)·𝒢(v) du`. A B-spline pcurve's quadrature samples
/// are kept in `samples`, by the coedge's position in the face's loops, so
/// the several integrals one face needs evaluate the spline once.
///
/// A loop closes by its vertices, not in the parameter plane: two meridians
/// meet at a pole a stretch of `u` apart, and a coedge may start a whole
/// turn from where the last one ended. The walk is lifted onto one sheet
/// of the plane — each coedge carried by the turns that bring its start to
/// the last end, which changes no integrand, every one being periodic —
/// and what is still open between two coedges is closed by the straight
/// segment between them, a ring along the pole. A loop whose end comes back
/// a turn from its start winds round the axis and encloses a pole on its
/// left: it is closed along that pole's ring.
fn region_integral(
    topology: &Topology,
    face: &Face,
    revolved: Revolved,
    sign: f64,
    azimuth: &Trig,
    latitude: &Latitude,
    samples: &mut Vec<Option<Samples>>,
) -> Option<f64> {
    let tau = std::f64::consts::TAU;
    let v_periodic = revolved.v_periodic();
    let mut total = 0.0;
    let mut position = 0;
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        let coedges = loop_record
            .value
            .coedges
            .iter()
            .map(|key| topology.coedge(*key).map(|record| record.value))
            .collect::<Option<Vec<_>>>()?;
        let Some(first) = coedges.first() else {
            continue;
        };
        let first_start = first.pcurve_endpoints()[0];
        let first_start = [first_start.x, first_start.y];
        let mut offset = [0.0_f64; 2];
        for (index, coedge) in coedges.iter().enumerate() {
            let slot = position;
            position += 1;
            if samples.len() <= slot {
                samples.resize_with(slot + 1, || None);
            }
            total +=
                coedge_contribution(coedge, offset, sign, azimuth, latitude, &mut samples[slot]);
            let end = coedge.pcurve_endpoints()[1];
            let end = [end.x + offset[0], end.y + offset[1]];
            let last = index + 1 == coedges.len();
            let mut next = if last {
                first_start
            } else {
                let start = coedges[index + 1].pcurve_endpoints()[0];
                [start.x + offset[0], start.y + offset[1]]
            };
            let turns_u = ((end[0] - next[0]) / tau).round();
            let turns_v = if v_periodic {
                ((end[1] - next[1]) / tau).round()
            } else {
                0.0
            };
            next[0] += turns_u * tau;
            next[1] += turns_v * tau;
            if !last {
                offset[0] += turns_u * tau;
                offset[1] += turns_v * tau;
            }
            let scale = end[0].abs().max(end[1].abs()).max(1.0);
            if (end[0] - next[0]).abs() > 1.0e-9 * scale
                || (end[1] - next[1]).abs() > 1.0e-9 * scale
            {
                total += line_contribution(end, next, sign, azimuth, latitude);
            }
            if last && (turns_u != 0.0 || turns_v != 0.0) {
                // The loop winds: closed along the pole on its left, reached
                // and left along meridians, which contribute nothing.
                if turns_v != 0.0 {
                    return None;
                }
                let pole = pole_latitude(revolved, next[1], turns_u > 0.0)?;
                total += line_contribution(
                    [next[0], pole],
                    [first_start[0], pole],
                    sign,
                    azimuth,
                    latitude,
                );
            }
        }
    }
    total.is_finite().then_some(total)
}

/// The `v` of the pole a winding loop encloses: above `from` when the
/// region is on the rising side, below it otherwise. A sphere's poles are
/// its latitudes `±π/2`; a cone's is its apex, where its ring closes; a
/// cylinder and a torus have none.
fn pole_latitude(revolved: Revolved, from: f64, above: bool) -> Option<f64> {
    let pole = match revolved {
        Revolved::Sphere(_) => {
            if above {
                std::f64::consts::FRAC_PI_2
            } else {
                -std::f64::consts::FRAC_PI_2
            }
        }
        Revolved::Cone(cone) if cone.slope != 0.0 => -cone.base_radius / cone.slope,
        Revolved::Cylinder(_) | Revolved::Cone(_) | Revolved::Torus(_) => return None,
    };
    ((above && pole >= from) || (!above && pole <= from)).then_some(pole)
}

/// One coedge's share of the contour integral, its pcurve carried by
/// `offset` onto the loop's sheet.
fn coedge_contribution(
    coedge: &crate::topology::Coedge,
    offset: [f64; 2],
    sign: f64,
    azimuth: &Trig,
    latitude: &Latitude,
    samples: &mut Option<Samples>,
) -> f64 {
    let range = coedge.parameter_range;
    let [start, end] = coedge.pcurve_endpoints();
    match coedge.pcurve {
        Curve2::Line { .. } => line_contribution(
            [start.x + offset[0], start.y + offset[1]],
            [end.x + offset[0], end.y + offset[1]],
            sign,
            azimuth,
            latitude,
        ),
        Curve2::Bspline { curve } => {
            let samples =
                samples.get_or_insert_with(|| spline_samples(curve, range.start, range.end));
            samples.integrate(&|point, rate| {
                -azimuth.evaluate(sign * (point[0] + offset[0]))
                    * latitude.antiderivative(point[1] + offset[1])
                    * rate[0]
            })
        }
        Curve2::Circle { .. }
        | Curve2::Harmonic { .. }
        | Curve2::Ellipse { .. }
        | Curve2::Trace { .. } => {
            let integrand = |t: f64| {
                let point = coedge.pcurve.evaluate(t);
                let rate = coedge.pcurve.derivative(t);
                -azimuth.evaluate(sign * (point.x + offset[0]))
                    * latitude.antiderivative(point.y + offset[1])
                    * rate.x
            };
            crate::cylinder_trace::integrate(range.start, range.end, &integrand)
        }
    }
}

/// The contour term along a straight segment of the parameter plane: closed
/// form along a ring, where `𝒢` is constant and `∫F(s·u) du` is known;
/// nothing along a meridian, where `du = 0`; quadrature otherwise.
fn line_contribution(
    from: [f64; 2],
    to: [f64; 2],
    sign: f64,
    azimuth: &Trig,
    latitude: &Latitude,
) -> f64 {
    let (du, dv) = (to[0] - from[0], to[1] - from[1]);
    let scale = du.abs().max(dv.abs()).max(1.0);
    if dv.abs() <= 1.0e-12 * scale {
        -latitude.antiderivative(from[1])
            * (azimuth.antiderivative(sign * to[0]) - azimuth.antiderivative(sign * from[0]))
            / sign
    } else if du.abs() <= 1.0e-12 * scale {
        0.0
    } else {
        let integrand = |t: f64| {
            -azimuth.evaluate(sign * du.mul_add(t, from[0]))
                * latitude.antiderivative(dv.mul_add(t, from[1]))
                * du
        };
        crate::cylinder_trace::integrate(0.0, 1.0, &integrand)
    }
}

/// The five-point Gauss–Legendre rule on `[−1, 1]`.
const GAUSS_FIVE: [(f64, f64); 5] = [
    (-0.906_179_845_938_664, 0.236_926_885_056_189_1),
    (-0.538_469_310_105_683_1, 0.478_628_670_499_366_5),
    (0.0, 0.568_888_888_888_888_9),
    (0.538_469_310_105_683_1, 0.478_628_670_499_366_5),
    (0.906_179_845_938_664, 0.236_926_885_056_189_1),
];

/// The quadrature samples of a B-spline pcurve over a coedge's range: each
/// node's weight, with the span's half-width and the walk's direction
/// folded in, and the curve's point and rate there.
struct Samples(Vec<(f64, [f64; 2], [f64; 2])>);

impl Samples {
    /// `∫ f(point, rate) dt` over the range the samples were taken on.
    fn integrate(&self, integrand: &dyn Fn([f64; 2], [f64; 2]) -> f64) -> f64 {
        self.0
            .iter()
            .map(|(weight, point, rate)| weight * integrand(*point, *rate))
            .sum()
    }
}

/// The samples of `[from, to]` (walked backwards when `to < from`) by a
/// Gauss rule on each knot span. A curve of few spans takes the ten-point
/// rule on each span halved, the rule the spline's own measures use, where
/// every integrand a measure needs is analytic; a numerically traced curve
/// of thousands of spans, whose integrands are smooth but not polynomial,
/// takes the five-point rule once per span, which on spans that short is
/// exact to rounding and four times cheaper.
fn spline_samples(curve: SplineCurve2, from: f64, to: f64) -> Samples {
    let spans = curve.spans(from, to);
    let direction = if to < from { -1.0 } else { 1.0 };
    let mut samples = Vec::new();
    if spans.len() > 256 {
        for (low, high) in spans {
            let half = 0.5 * (high - low);
            let centre = 0.5 * (low + high);
            for (node, weight) in GAUSS_FIVE {
                let t = half.mul_add(node, centre);
                let [point, rate, _] = curve.derivatives(t);
                samples.push((direction * weight * half, point, rate));
            }
        }
    } else {
        for (low, high) in spans {
            let middle = 0.5 * (low + high);
            for (a, b) in [(low, middle), (middle, high)] {
                let half = 0.5 * (b - a);
                let centre = 0.5 * (a + b);
                for (node, weight) in crate::ruled::GAUSS_NODES {
                    let t = half.mul_add(node, centre);
                    let [point, rate, _] = curve.derivatives(t);
                    samples.push((direction * weight * half, point, rate));
                }
            }
        }
    }
    Samples(samples)
}

/// One face's exact area, flux `∮ (x − p)·n dA` and first moment
/// `½∮ |x − p|² n dA` about `anchor`, over whatever region its loops bound.
pub(crate) fn face_contribution(
    topology: &Topology,
    face: &Face,
    revolved: Revolved,
    anchor: Point3,
) -> Option<(f64, f64, Vector3)> {
    let axis = revolved.axis();
    let (u, v) = (revolved.radial_u(), revolved.radial_v());
    let sign = revolved.angular_sign();
    let handedness = u.cross(v).dot(axis).signum();
    let sigma = handedness * sign;
    let offset = revolved.origin() - anchor;
    let (along_u, along_v, axial) = (offset.dot(u), offset.dot(v), offset.dot(axis));
    let profile = meridian(revolved);

    let one = Trig::constant(1.0);
    let cosine = Trig::cosine();
    let sine = Trig::sine();
    // `(O − p)·r̂(a)`.
    let radial_offset = cosine.scaled(along_u).plus(&sine.scaled(along_v));
    let mut samples = Vec::new();
    let mut integral = |azimuth: &Trig, latitude: &Latitude| {
        region_integral(
            topology,
            face,
            revolved,
            sign,
            azimuth,
            latitude,
            &mut samples,
        )
    };

    let area = integral(&one, &profile.density)?.abs();

    // (x − p)·n dA = σ [ρz'(off·r̂) + ρ²z' − ρρ'(off·A) − ρρ'z].
    let rho_z_prime = profile.rho.times(&profile.z_prime);
    let rho2_z_prime = rho_z_prime.times(&profile.rho);
    let rho_rho_prime = profile.rho.times(&profile.rho_prime);
    let rho_rho_prime_z = rho_rho_prime.times(&profile.z);
    let flux = sigma
        * (integral(&radial_offset, &rho_z_prime)? + integral(&one, &rho2_z_prime)?
            - axial * integral(&one, &rho_rho_prime)?
            - integral(&one, &rho_rho_prime_z)?);

    // |x − p|² = K(v) + 2ρ(off·r̂), K = |off|² + ρ² + z² + 2z(off·A), and
    // the moment is ½∬ |x − p|² σρ(z'r̂ − ρ'A) du dv.
    let k = profile
        .one
        .scaled(offset.dot(offset))
        .plus(&profile.rho.times(&profile.rho))
        .plus(&profile.z.times(&profile.z))
        .plus(&profile.z.scaled(2.0 * axial));
    let k_rho_z_prime = k.times(&rho_z_prime);
    let k_rho_rho_prime = k.times(&rho_rho_prime);
    let rho2_rho_prime = rho_rho_prime.times(&profile.rho);
    let radial_cosine = radial_offset.times(&cosine);
    let radial_sine = radial_offset.times(&sine);
    let along_u_moment =
        integral(&cosine, &k_rho_z_prime)? + 2.0 * integral(&radial_cosine, &rho2_z_prime)?;
    let along_v_moment =
        integral(&sine, &k_rho_z_prime)? + 2.0 * integral(&radial_sine, &rho2_z_prime)?;
    let axial_moment =
        integral(&one, &k_rho_rho_prime)? + 2.0 * integral(&radial_offset, &rho2_rho_prime)?;
    let moment = (u * along_u_moment + v * along_v_moment - axis * axial_moment) * (0.5 * sigma);
    (flux.is_finite() && moment.is_finite()).then_some((area, flux, moment))
}

/// Whether a face's loops are more than one rectangle of rings and
/// meridians: an inner loop, a pcurve that is not a line, or an outer loop
/// that is not four iso-lines. Such a face is measured here; the rectangle
/// keeps the validator's closed forms.
pub(crate) fn needs_general_measures(topology: &Topology, face: &Face) -> bool {
    if !face.inner_loops.is_empty() {
        return true;
    }
    let Some(outer) = topology.loop_record(face.outer_loop) else {
        return true;
    };
    if outer.value.coedges.len() != 4 {
        return true;
    }
    outer.value.coedges.iter().any(|coedge_key| {
        let Some(coedge) = topology.coedge(*coedge_key) else {
            return true;
        };
        if !matches!(coedge.value.pcurve, Curve2::Line { .. }) {
            return true;
        }
        let [start, end] = coedge.value.pcurve_endpoints();
        let scale = start.x.abs().max(start.y.abs()).max(1.0);
        (start.x - end.x).abs() > 1.0e-12 * scale && (start.y - end.y).abs() > 1.0e-12 * scale
    })
}

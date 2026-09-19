//! The curve where any two cylinders meet.
//!
//! ADR 0025 drew the intersection matrix around pairs whose curve is a line, a
//! circle or an ellipse, and two cylinders qualify only when they are coaxial,
//! parallel, or of equal radius on crossing axes. Everything else — a bore
//! crossing a bore of another diameter, a slot cut from a sloped face across a
//! bore, any two round features that are not siblings — met in what the matrix
//! called "a genuine space quartic" and was refused by name.
//!
//! It is a quartic, and it is also closed form. Write a point of cylinder `A`
//! in `A`'s own parameters as `P(x, y) = origin + r·radial(x) + y·axis`, and
//! substitute it into `B`'s implicit equation `|w|² − (w·e)² = r_B²`. Because
//! `radial ⟂ axis`, the `y` terms collect into a quadratic whose coefficients
//! are trigonometric polynomials in `x`:
//!
//! ```text
//! a·y² + b(x)·y + c(x) = 0,    a = |n|² − (n·e)²
//! b(x) = b₀ + b₁cos x + b₂sin x
//! c(x) = c₀ + c₁cos x + c₂sin x + c₃cos 2x + c₄sin 2x
//! ```
//!
//! so the trace is `y(x) = (−b(x) ± √D(x)) / 2a` with `D = b² − 4ac` itself a
//! second-order trigonometric polynomial. Two branches, each exact to the last
//! bit the arithmetic carries, meeting where `D` vanishes.
//!
//! ## Which cylinder holds the parameter
//!
//! `y(x)` has a vertical tangent where `D(x) = 0`: the branches meet there and
//! the azimuth stops being a good parameter, the way it does at the ends of a
//! semicircle drawn as a graph over its diameter. The curve is perfectly
//! smooth there — only this description of it is not.
//!
//! The same curve written on the *other* cylinder has its own branch points,
//! and they are elsewhere: on `A` they sit where the trace runs along `A`'s
//! generators, on `B` where it runs along `B`'s. The trace therefore records
//! which cylinder holds its parameter, and the pair picks that host once, by
//! a canonical order, so that both faces read the curve the same way.
//!
//! ## What is exact here
//!
//! The evaluation, the derivative and the branch points are closed forms. The
//! arc length and the area a trace bounds are not — no elementary antiderivative
//! exists for `√(trig polynomial)` — and are integrated by Gauss–Legendre
//! quadrature over spans the branch points split, where the integrand is
//! analytic and convergence is exponential. That is the same standing the
//! kernel already gives an ellipse's arc length, which has no closed form
//! either.

use crate::topology::{Cylinder, Point2, Point3, Vector3, seam_snapped_sin_cos};

/// The curve two cylinders share, read in one of their parameter spaces.
///
/// `host` holds the parameter — the curve is a graph `y(x)` over `host`'s
/// azimuth — and `branch` picks one of the two roots. The pair `(host, other)`
/// is not symmetric: the same curve read from the other side is a different
/// `CylinderTrace` with the same locus, which is what
/// [`CylinderTrace::flipped`] produces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CylinderTrace {
    pub(crate) host: Cylinder,
    pub(crate) other: Cylinder,
    /// `+1` or `−1`: which root of the quadratic in height.
    pub(crate) branch: f64,
}

/// The quadratic's coefficients, resolved once per evaluation batch.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TraceCoefficients {
    /// The `y²` coefficient, which does not vary with the azimuth.
    pub(crate) quadratic: f64,
    /// `b₀, b₁, b₂` of `b(x) = b₀ + b₁cos x + b₂sin x`.
    pub(crate) linear: [f64; 3],
    /// `d₀ … d₄` of `D(x) = d₀ + d₁cos x + d₂sin x + d₃cos 2x + d₄sin 2x`.
    pub(crate) discriminant: [f64; 5],
}

impl TraceCoefficients {
    /// `b(x)`.
    fn linear_at(self, x: f64) -> f64 {
        let (sin, cos) = seam_snapped_sin_cos(x);
        self.linear[1].mul_add(cos, self.linear[2].mul_add(sin, self.linear[0]))
    }

    /// `b′(x)`.
    fn linear_slope_at(self, x: f64) -> f64 {
        let (sin, cos) = seam_snapped_sin_cos(x);
        self.linear[2].mul_add(cos, -(self.linear[1] * sin))
    }

    /// `D(x)`.
    pub(crate) fn discriminant_at(self, x: f64) -> f64 {
        let (sin, cos) = seam_snapped_sin_cos(x);
        let (sin2, cos2) = seam_snapped_sin_cos(2.0 * x);
        self.discriminant[1].mul_add(
            cos,
            self.discriminant[2].mul_add(
                sin,
                self.discriminant[3].mul_add(
                    cos2,
                    self.discriminant[4].mul_add(sin2, self.discriminant[0]),
                ),
            ),
        )
    }

    /// `D′(x)`.
    fn discriminant_slope_at(self, x: f64) -> f64 {
        let (sin, cos) = seam_snapped_sin_cos(x);
        let (sin2, cos2) = seam_snapped_sin_cos(2.0 * x);
        2.0f64.mul_add(
            self.discriminant[4].mul_add(cos2, -(self.discriminant[3] * sin2)),
            self.discriminant[2].mul_add(cos, -(self.discriminant[1] * sin)),
        )
    }
}

/// Ten-point Gauss–Legendre nodes and weights on `[−1, 1]`.
///
/// A rule of this order integrates a polynomial of degree nineteen exactly
/// and an analytic integrand to machine precision over a short span, so the
/// composite below is exact in the sense ADR 0026 made normative for the
/// elliptic integrals an ellipse's arc length already needs: it evaluates a
/// transcendental exactly; it does not approximate the geometry.
const GAUSS_NODES: [(f64, f64); 10] = [
    (-0.973_906_528_517_171_7, 0.066_671_344_308_688_1),
    (-0.865_063_366_688_984_5, 0.149_451_349_150_580_6),
    (-0.679_409_568_299_024_4, 0.219_086_362_515_982),
    (-0.433_395_394_129_247_2, 0.269_266_719_309_996_3),
    (-0.148_874_338_981_631_2, 0.295_524_224_714_752_9),
    (0.148_874_338_981_631_2, 0.295_524_224_714_752_9),
    (0.433_395_394_129_247_2, 0.269_266_719_309_996_3),
    (0.679_409_568_299_024_4, 0.219_086_362_515_982),
    (0.865_063_366_688_984_5, 0.149_451_349_150_580_6),
    (0.973_906_528_517_171_7, 0.066_671_344_308_688_1),
];

/// Integrates an analytic function over a span, composite Gauss–Legendre.
///
/// The span is cut into pieces no longer than a fifth of a radian so the rule
/// sees a nearly polynomial integrand on each, which is where its convergence
/// is exponential. The caller is responsible for splitting at branch points
/// first: across one the integrand has a square-root cusp and no quadrature
/// of fixed order is exact.
pub(crate) fn integrate(from: f64, to: f64, integrand: &dyn Fn(f64) -> f64) -> f64 {
    let span = to - from;
    if !span.is_finite() || span == 0.0 {
        return 0.0;
    }
    let pieces = ((span.abs() / 0.2).ceil() as usize).clamp(1, 512);
    let step = span / pieces as f64;
    let mut total = 0.0;
    for piece in 0..pieces {
        let low = step.mul_add(piece as f64, from);
        let (half, middle) = (0.5 * step, step.mul_add(0.5, low));
        for (node, weight) in GAUSS_NODES {
            total += weight * integrand(half.mul_add(node, middle));
        }
        // The half-width is the Jacobian of the map onto [-1, 1].
    }
    total * 0.5 * step
}

/// How near zero a discriminant counts as a branch point, relative to the
/// scale its own coefficients set.
fn discriminant_floor(coefficients: TraceCoefficients) -> f64 {
    let scale = coefficients
        .discriminant
        .iter()
        .fold(0.0_f64, |worst, term| worst.max(term.abs()));
    scale.max(1.0) * 1.0e-12
}

impl CylinderTrace {
    /// The same locus read from the other cylinder's parameter space.
    ///
    /// Used by the reparameterization that hands one face's trace to the
    /// other; kept beside the rest of the curve's algebra.
    ///
    /// Which branch it becomes is not knowable from the coefficients alone —
    /// the two descriptions number their roots independently — so the caller
    /// picks it by a point the two must agree on.
    pub(crate) fn flipped(self, branch: f64) -> Self {
        Self {
            host: self.other,
            other: self.host,
            branch,
        }
    }

    /// The quadratic in the host's height whose roots are this trace.
    pub(crate) fn coefficients(self) -> Option<TraceCoefficients> {
        let axis = self.host.axis;
        let axis_length_squared = axis.dot(axis);
        let other_length = self.other.axis.length();
        if axis_length_squared <= f64::EPSILON || other_length <= f64::EPSILON {
            return None;
        }
        let e = self.other.axis / other_length;
        let m = self.host.origin - self.other.origin;
        let k = axis.dot(e);
        let quadratic = axis_length_squared - k * k;
        if quadratic.abs() <= f64::EPSILON {
            // Parallel axes: the height falls out and the trace is a pair of
            // generators, which the intersection matrix already names.
            return None;
        }
        let sign = self.host.angular_sign;
        let radius = self.host.radius;
        // `radial(x) = u·cos x + v·sin(σx)`, so folding the sign into the `v`
        // terms lets every coefficient below be written over the plain
        // azimuth.
        let u = self.host.radial_u;
        let v = self.host.radial_v * sign;
        let (p, q) = (u.dot(e), v.dot(e));
        let (s, t) = (m.dot(u), m.dot(v));
        let (m_e, m_n) = (m.dot(e), m.dot(axis));

        let linear = [
            2.0 * k.mul_add(-m_e, m_n),
            -2.0 * k * radius * p,
            -2.0 * k * radius * q,
        ];
        let quadratic_c = [
            0.5 * radius * radius * (q * q - p * p),
            -(radius * radius * p * q),
        ];
        let constant = radius.mul_add(radius, m.dot(m))
            - m_e.mul_add(m_e, self.other.radius * self.other.radius)
            - 0.5 * radius * radius * p.mul_add(p, q * q);
        let c = [
            constant,
            2.0 * radius * m_e.mul_add(-p, s),
            2.0 * radius * m_e.mul_add(-q, t),
            quadratic_c[0],
            quadratic_c[1],
        ];

        // `D = b² − 4ac`, with `b²` reduced to the same harmonics.
        let [b0, b1, b2] = linear;
        let four_a = 4.0 * quadratic;
        let discriminant = [
            b0.mul_add(b0, 0.5 * b1.mul_add(b1, b2 * b2)) - four_a * c[0],
            2.0f64.mul_add(b0 * b1, -(four_a * c[1])),
            2.0f64.mul_add(b0 * b2, -(four_a * c[2])),
            0.5f64.mul_add(b1.mul_add(b1, -(b2 * b2)), -(four_a * c[3])),
            (b1 * b2).mul_add(1.0, -(four_a * c[4])),
        ];
        Some(TraceCoefficients {
            quadratic,
            linear,
            discriminant,
        })
    }

    /// The host height at an azimuth, or `None` past a branch point where the
    /// two cylinders no longer meet.
    pub(crate) fn height_at(self, x: f64) -> Option<f64> {
        let coefficients = self.coefficients()?;
        let discriminant = coefficients.discriminant_at(x);
        // A hair below zero at a branch point is the arithmetic, not the
        // shape: the root is the double one.
        let root = if discriminant < 0.0 {
            if discriminant < -discriminant_floor(coefficients) {
                return None;
            }
            0.0
        } else {
            discriminant.sqrt()
        };
        Some(
            self.branch
                .mul_add(root, -coefficients.linear_at(x))
                .mul_add(0.5 / coefficients.quadratic, 0.0),
        )
    }

    /// The point in the host's parameter space.
    #[cfg(test)]
    pub(crate) fn evaluate(self, x: f64) -> Option<Point2> {
        self.height_at(x).map(|height| Point2::new(x, height))
    }

    /// The point in space, or nothing past a branch point.
    #[cfg(test)]
    pub(crate) fn point_at(self, x: f64) -> Option<Point3> {
        self.evaluate(x).map(|point| self.host.evaluate(point))
    }

    /// The host height at an azimuth, with the discriminant held at zero
    /// past a branch point.
    ///
    /// Evaluation has to be total — a curve is asked for its point at a
    /// parameter, not asked whether it has one — and the curve's own domain
    /// ends at its branch points. Clamping returns the branch point itself,
    /// which is the nearest point of the curve and is where the parameter was
    /// heading.
    pub(crate) fn height_clamped(self, x: f64) -> f64 {
        let Some(coefficients) = self.coefficients() else {
            return 0.0;
        };
        let root = coefficients.discriminant_at(x).max(0.0).sqrt();
        self.branch
            .mul_add(root, -coefficients.linear_at(x))
            .mul_add(0.5 / coefficients.quadratic, 0.0)
    }

    /// The point in space at any azimuth, clamped as [`Self::height_clamped`].
    pub(crate) fn point_clamped(self, x: f64) -> Point3 {
        self.host.evaluate(Point2::new(x, self.height_clamped(x)))
    }

    /// The tangent in space, taken a hair inside the domain where the
    /// azimuth's own slope has run away.
    ///
    /// At a branch point the direction is still well defined — the curve is
    /// smooth — but this parameterization reaches it with infinite speed, so
    /// the value is read just inside and is finite by construction.
    pub(crate) fn tangent_clamped(self, x: f64) -> Vector3 {
        if let Some(tangent) = self.tangent_at(x) {
            return tangent;
        }
        let Some(coefficients) = self.coefficients() else {
            return Vector3::new(0.0, 0.0, 0.0);
        };
        let step = 1.0e-7;
        let inward = if coefficients.discriminant_at(x + step) >= coefficients.discriminant_at(x) {
            step
        } else {
            -step
        };
        self.tangent_at(x + inward)
            .unwrap_or_else(|| Vector3::new(0.0, 0.0, 0.0))
    }

    /// This point of the curve in the other cylinder's parameter space, with
    /// the azimuth carried onto the branch `turns` whole turns from the
    /// principal one.
    ///
    /// The two faces either side of a trace must read it over one parameter,
    /// or the sewer's midpoint weld compares two different points of the same
    /// curve and leaves the bodies apart. So the azimuth here is the host's
    /// throughout, and this is only the mapping into the other face's own
    /// coordinates.
    pub(crate) fn on_other(self, x: f64, turns: f64) -> Point2 {
        let point = self.point_clamped(x);
        let cylinder = self.other;
        let axis_length_squared = cylinder.axis.dot(cylinder.axis);
        if axis_length_squared <= f64::EPSILON {
            return Point2::new(0.0, 0.0);
        }
        let offset = point - cylinder.origin;
        let height = offset.dot(cylinder.axis) / axis_length_squared;
        let radial = offset - cylinder.axis * height;
        let azimuth = cylinder.angular_sign
            * radial
                .dot(cylinder.radial_v)
                .atan2(radial.dot(cylinder.radial_u));
        Point2::new(turns.mul_add(std::f64::consts::TAU, azimuth), height)
    }

    /// The rate of [`Self::on_other`] with the host's azimuth.
    pub(crate) fn on_other_rate(self, x: f64) -> Point2 {
        let tangent = self.tangent_clamped(x);
        let cylinder = self.other;
        let axis_length_squared = cylinder.axis.dot(cylinder.axis);
        if axis_length_squared <= f64::EPSILON {
            return Point2::new(0.0, 0.0);
        }
        let height_rate = tangent.dot(cylinder.axis) / axis_length_squared;
        let offset = self.point_clamped(x) - cylinder.origin;
        let height = offset.dot(cylinder.axis) / axis_length_squared;
        let radial = offset - cylinder.axis * height;
        let radial_rate = tangent - cylinder.axis * height_rate;
        let (along, across) = (radial.dot(cylinder.radial_u), radial.dot(cylinder.radial_v));
        let (along_rate, across_rate) = (
            radial_rate.dot(cylinder.radial_u),
            radial_rate.dot(cylinder.radial_v),
        );
        let square = along.mul_add(along, across * across);
        let azimuth_rate = if square <= f64::EPSILON {
            0.0
        } else {
            cylinder.angular_sign * along.mul_add(across_rate, -(across * along_rate)) / square
        };
        Point2::new(azimuth_rate, height_rate)
    }

    /// `dy/dx` in the host's parameter space, or `None` at a branch point,
    /// where the azimuth stops being a parameter of this curve.
    pub(crate) fn slope_at(self, x: f64) -> Option<f64> {
        let coefficients = self.coefficients()?;
        let discriminant = coefficients.discriminant_at(x);
        let floor = discriminant_floor(coefficients);
        if discriminant <= floor {
            return None;
        }
        let root = discriminant.sqrt();
        Some(
            self.branch
                .mul_add(
                    coefficients.discriminant_slope_at(x) / (2.0 * root),
                    -coefficients.linear_slope_at(x),
                )
                .mul_add(0.5 / coefficients.quadratic, 0.0),
        )
    }

    /// The tangent in space, which stays finite where the parameter's own
    /// slope does not: the curve is smooth even where this reading of it is
    /// singular.
    pub(crate) fn tangent_at(self, x: f64) -> Option<Vector3> {
        let slope = self.slope_at(x)?;
        let sign = self.host.angular_sign;
        let (sin, cos) = seam_snapped_sin_cos(sign * x);
        let radial_rate =
            (self.host.radial_v * cos - self.host.radial_u * sin) * (self.host.radius * sign);
        Some(radial_rate + self.host.axis * slope)
    }

    /// The length of the curve between two azimuths.
    ///
    /// `|dP/dx| = √((r·dθ/dx)² + |axis|²·y′²)`, integrated over the span. The
    /// caller splits at branch points, where `y′` is unbounded and the
    /// integrand has a cusp; between them it is analytic.
    pub(crate) fn arc_length(self, from: f64, to: f64) -> f64 {
        let radial_speed = self.host.radius * self.host.angular_sign.abs();
        let axis_length_squared = self.host.axis.dot(self.host.axis);
        let speed = |x: f64| {
            let slope = self
                .slope_at(x)
                .unwrap_or_else(|| self.slope_towards_domain(x));
            axis_length_squared
                .mul_add(slope * slope, radial_speed * radial_speed)
                .sqrt()
        };
        integrate(from, to, &speed)
    }

    /// The slope read a hair inside the domain, for the two parameters at the
    /// very ends of a span where the exact one is unbounded.
    fn slope_towards_domain(self, x: f64) -> f64 {
        let Some(coefficients) = self.coefficients() else {
            return 0.0;
        };
        let step = 1.0e-9;
        let inward = if coefficients.discriminant_at(x + step) >= coefficients.discriminant_at(x) {
            step
        } else {
            -step
        };
        self.slope_at(x + inward).unwrap_or(0.0)
    }

    /// `a·y² + b(x)·y + c(x)`, which vanishes exactly on the curve.
    ///
    /// Two cylinders meeting is a quadratic in the host's height, so the
    /// quadratic itself is the curve's implicit form in the host's parameter
    /// space: exact, cheap, and signed either side. The pipeline's crossing
    /// finder wants a signed distance from a carrier, and this is that
    /// carrier's own equation rather than a sampled stand-in.
    pub(crate) fn implicit_at(self, point: Point2) -> f64 {
        let Some(coefficients) = self.coefficients() else {
            return f64::NAN;
        };
        let linear = coefficients.linear_at(point.x);
        let discriminant = coefficients.discriminant_at(point.x);
        // `c = (b² − D) / 4a`, which keeps the coefficients in one place.
        let constant = linear.mul_add(linear, -discriminant) / (4.0 * coefficients.quadratic);
        coefficients
            .quadratic
            .mul_add(point.y * point.y, linear.mul_add(point.y, constant))
    }

    /// The height both branches share at a branch point: the double root.
    ///
    /// At `D = 0` the two roots are one, and the arithmetic that reaches it
    /// from either side does not land on the same bits — `√D` is a few
    /// ulps either way. Evaluating the double root directly gives both
    /// branches the very same endpoint, which is what lets the weld close
    /// the loop they make between them rather than leaving it open by a
    /// millionth.
    pub(crate) fn double_root_at(self, x: f64) -> f64 {
        let Some(coefficients) = self.coefficients() else {
            return 0.0;
        };
        -coefficients.linear_at(x) / (2.0 * coefficients.quadratic)
    }

    /// A piece's endpoint in whichever face's parameters it is written in,
    /// taking the double root where the azimuth is a branch point.
    pub(crate) fn endpoint(self, x: f64, at_branch: bool, on_other: bool) -> Point2 {
        let height = if at_branch {
            self.double_root_at(x)
        } else {
            self.height_clamped(x)
        };
        if !on_other {
            return Point2::new(x, height);
        }
        let point = self.host.evaluate(Point2::new(x, height));
        let cylinder = self.other;
        let axis_length_squared = cylinder.axis.dot(cylinder.axis);
        if axis_length_squared <= f64::EPSILON {
            return Point2::new(0.0, 0.0);
        }
        let offset = point - cylinder.origin;
        let along = offset.dot(cylinder.axis) / axis_length_squared;
        let radial = offset - cylinder.axis * along;
        let azimuth = cylinder.angular_sign
            * radial
                .dot(cylinder.radial_v)
                .atan2(radial.dot(cylinder.radial_u));
        Point2::new(azimuth, along)
    }

    /// The largest the discriminant gets over a whole turn.
    ///
    /// Two cylinders that never meet have a discriminant that stays negative,
    /// and there is no curve to name: the pair is empty rather than outside
    /// the vocabulary. Two that graze touch zero at a single azimuth. The
    /// sampling is refined at every bracket and at every extremum, so the
    /// number it reports is the true maximum and not the best of a grid.
    pub(crate) fn peak_discriminant(self) -> Option<f64> {
        let coefficients = self.coefficients()?;
        const SAMPLES: usize = 96;
        let tau = std::f64::consts::TAU;
        let mut peak = f64::NEG_INFINITY;
        let mut previous_slope = coefficients.discriminant_slope_at(0.0);
        for index in 0..=SAMPLES {
            let x = tau * index as f64 / SAMPLES as f64;
            peak = peak.max(coefficients.discriminant_at(x));
            let slope = coefficients.discriminant_slope_at(x);
            if index > 0 && (previous_slope > 0.0) != (slope > 0.0) {
                // A turning point between the two samples: bisect on the
                // slope and read the discriminant there.
                let (mut low, mut high) = (tau * (index - 1) as f64 / SAMPLES as f64, x);
                let rising = previous_slope > 0.0;
                for _ in 0..60 {
                    let middle = 0.5 * (low + high);
                    if (coefficients.discriminant_slope_at(middle) > 0.0) == rising {
                        low = middle;
                    } else {
                        high = middle;
                    }
                }
                peak = peak.max(coefficients.discriminant_at(0.5 * (low + high)));
            }
            previous_slope = slope;
        }
        Some(peak)
    }

    /// Every azimuth in `[from, to]` where the two branches meet.
    ///
    /// These are the roots of `D`, and they are where a span has to be split
    /// before it can be read as a graph over the azimuth. Sign changes are
    /// bracketed and bisected; a double root — the tangency of ADR 0045 —
    /// shows as a minimum that touches zero and is refined by the same
    /// bisection on `D′`.
    pub(crate) fn branch_points(self, from: f64, to: f64) -> Vec<f64> {
        let Some(coefficients) = self.coefficients() else {
            return Vec::new();
        };
        let floor = discriminant_floor(coefficients);
        // `D` has at most four roots in a turn, so a sampling this fine
        // cannot skip a sign change, and the refinement below is what makes
        // the answer exact rather than the sampling.
        const SAMPLES: usize = 96;
        let span = to - from;
        if !span.is_finite() || span.abs() <= f64::EPSILON {
            return Vec::new();
        }
        let at = |index: usize| span.mul_add(index as f64 / SAMPLES as f64, from);
        let mut found: Vec<f64> = Vec::new();
        let push = |candidate: f64, found: &mut Vec<f64>| {
            if coefficients.discriminant_at(candidate).abs() <= floor * 1.0e3
                && !found
                    .iter()
                    .any(|held: &f64| (held - candidate).abs() <= 1.0e-9)
            {
                found.push(candidate);
            }
        };
        let bisect = |value: &dyn Fn(f64) -> f64, mut low: f64, mut high: f64| -> f64 {
            let low_value = value(low);
            for _ in 0..80 {
                let middle = 0.5 * (low + high);
                if (value(middle) < 0.0) == (low_value < 0.0) {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            0.5 * (low + high)
        };
        let discriminant = |x: f64| coefficients.discriminant_at(x);
        let slope = |x: f64| coefficients.discriminant_slope_at(x);
        let mut previous = (at(0), discriminant(at(0)), slope(at(0)));
        if previous.1.abs() <= floor {
            push(previous.0, &mut found);
        }
        for index in 1..=SAMPLES {
            let x = at(index);
            let current = (x, discriminant(x), slope(x));
            if (previous.1 < 0.0) != (current.1 < 0.0) {
                push(bisect(&discriminant, previous.0, current.0), &mut found);
            } else if (previous.2 < 0.0) != (current.2 < 0.0) {
                // An extremum: a double root if it sits on zero.
                let extremum = bisect(&slope, previous.0, current.0);
                push(extremum, &mut found);
            } else if current.1.abs() <= floor {
                push(current.0, &mut found);
            }
            previous = current;
        }
        found.sort_by(f64::total_cmp);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::Point3;

    fn cylinder(origin: [f64; 3], axis: [f64; 3], radius: f64) -> Cylinder {
        let axis = Vector3::new(axis[0], axis[1], axis[2]);
        let length = axis.length();
        let unit = axis / length;
        // Any frame square to the axis; the trace must not depend on which.
        let away = if unit.x.abs() <= 0.5 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let radial_u = {
            let tangent = away - unit * away.dot(unit);
            tangent / tangent.length()
        };
        Cylinder {
            origin: Point3::new(origin[0], origin[1], origin[2]),
            axis: unit,
            radial_u,
            radial_v: unit.cross(radial_u),
            radius,
            angular_sign: 1.0,
        }
    }

    /// The defining property, and the only one worth testing directly: every
    /// point the trace names is on both cylinders.
    fn assert_on_both(trace: CylinderTrace, tolerance: f64) {
        let mut sampled = 0;
        for step in 0..512 {
            let x = std::f64::consts::TAU * f64::from(step) / 512.0;
            let Some(point) = trace.point_at(x) else {
                continue;
            };
            sampled += 1;
            for cylinder in [trace.host, trace.other] {
                let axis = cylinder.axis / cylinder.axis.length();
                let offset = point - cylinder.origin;
                let radial = offset - axis * offset.dot(axis);
                assert!(
                    (radial.length() - cylinder.radius).abs() < tolerance,
                    "off the cylinder at {x}: {} vs {}",
                    radial.length(),
                    cylinder.radius
                );
            }
        }
        assert!(sampled > 64, "the trace should exist over much of the turn");
    }

    #[test]
    fn two_bores_of_unequal_radius_crossing_at_a_right_angle() {
        let host = cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 8.0);
        let other = cylinder([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 5.0);
        for branch in [1.0, -1.0] {
            assert_on_both(
                CylinderTrace {
                    host,
                    other,
                    branch,
                },
                1.0e-9,
            );
        }
    }

    #[test]
    fn two_bores_on_skew_axes_of_unequal_radius() {
        let host = cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 9.0);
        let other = cylinder([3.0, 1.0, 4.0], [1.0, 0.4, 0.25], 6.0);
        for branch in [1.0, -1.0] {
            assert_on_both(
                CylinderTrace {
                    host,
                    other,
                    branch,
                },
                1.0e-9,
            );
        }
    }

    /// The case the old matrix did carry: equal radii on crossing axes, where
    /// the trace degenerates to the two straight harmonics of the Steinmetz
    /// seam. The general form has to agree with it.
    #[test]
    fn equal_radii_on_crossing_axes_are_the_steinmetz_seam() {
        let radius = 7.0;
        let host = cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], radius);
        let other = cylinder([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], radius);
        let trace = CylinderTrace {
            host,
            other,
            branch: 1.0,
        };
        for step in 0..256 {
            let x = std::f64::consts::TAU * f64::from(step) / 256.0;
            let Some(point) = trace.evaluate(x) else {
                continue;
            };
            // On a cylinder about z of radius r, crossed by one about x of the
            // same radius: z = ±r·cos(azimuth measured from x).
            let expected = radius * x.cos();
            assert!(
                (point.y.abs() - expected.abs()).abs() < 1.0e-9,
                "the Steinmetz height at {x}: {} vs {expected}",
                point.y
            );
        }
    }

    /// The branch points are where the trace runs along the host's own
    /// generators. For two equal perpendicular bores through one another that
    /// is the pair of azimuths square to the other axis.
    #[test]
    fn the_branches_meet_where_the_trace_turns_along_a_generator() {
        let host = cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 8.0);
        let other = cylinder([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 5.0);
        let trace = CylinderTrace {
            host,
            other,
            branch: 1.0,
        };
        let points = trace.branch_points(0.0, std::f64::consts::TAU);
        assert!(
            points.len() >= 2,
            "a bore crossed by a narrower one meets it in two arcs: {points:?}"
        );
        for point in &points {
            let discriminant = trace
                .coefficients()
                .expect("crossing axes")
                .discriminant_at(*point);
            assert!(
                discriminant.abs() < 1.0e-6,
                "a branch point is a root of the discriminant: {discriminant}"
            );
        }
    }

    /// The slope is the derivative of the height, and it runs away at a branch
    /// point while the tangent in space does not.
    #[test]
    fn the_slope_matches_a_difference_and_the_space_tangent_stays_finite() {
        let host = cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 9.0);
        let other = cylinder([2.0, 1.0, 3.0], [1.0, 0.3, 0.2], 6.0);
        let trace = CylinderTrace {
            host,
            other,
            branch: -1.0,
        };
        let mut checked = 0;
        for step in 0..128 {
            let x = std::f64::consts::TAU * f64::from(step) / 128.0;
            let step_size = 1.0e-6;
            let (Some(slope), Some(ahead), Some(behind)) = (
                trace.slope_at(x),
                trace.height_at(x + step_size),
                trace.height_at(x - step_size),
            ) else {
                continue;
            };
            let numeric = (ahead - behind) / (2.0 * step_size);
            if slope.abs() > 1.0e3 {
                continue;
            }
            checked += 1;
            assert!(
                (slope - numeric).abs() < 1.0e-4 * slope.abs().max(1.0),
                "the slope at {x}: {slope} against {numeric}"
            );
            let tangent = trace.tangent_at(x).expect("a regular point has a tangent");
            assert!(tangent.is_finite() && tangent.length() > 0.0);
        }
        assert!(checked > 32, "most of the turn should be regular");
    }
}

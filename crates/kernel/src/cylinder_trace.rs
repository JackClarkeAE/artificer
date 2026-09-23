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
    /// The host height the quadratic's own height is measured from: the foot
    /// of the two axes' common perpendicular on the host's axis. A root `y′`
    /// of the quadratic is the height `offset + y′`.
    pub(crate) offset: f64,
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

    /// How far from zero `D(x)` can land at a branch point by rounding alone.
    ///
    /// Not only this evaluation's rounding: the coefficients themselves are
    /// re-derived whenever the body moves — a mirror, a placement — from
    /// cylinders whose frames were rounded in the move, and the branch point
    /// an edge ends at was found against the coefficients as they were. That
    /// leaves the discriminant at the edge's end a few hundred ulps of its
    /// terms from zero rather than a few, so the floor is set well above
    /// both, at a millionth of a millionth of the terms. Read as zero, only
    /// azimuths within about as far of a root snap to it, and the point they
    /// snap to is the branch point itself, which is on the curve.
    fn rounding(self) -> f64 {
        1.0e-12
            * self
                .discriminant
                .iter()
                .fold(0.0_f64, |total, term| total + term.abs())
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

/// Integrates a function over a span, composite Gauss–Legendre.
///
/// The span is cut into pieces no longer than a fifth of a radian so the rule
/// sees a nearly polynomial integrand on each, which is where its convergence
/// is exponential. The caller splits at branch points, so no piece has one
/// inside it; but a piece routinely *ends* at one, and there the integrand
/// has a square-root cusp — the height moves like `√(x − x*)` and its rate
/// like `1/√(x − x*)` — which no rule of fixed order integrates exactly.
///
/// So the span is walked through `x = a + (b − a)(3t² − 2t³)`, whose rate
/// vanishes at both ends. Near an end `x − a` grows like `t²`, the cusp's
/// `√(x − a)` becomes a multiple of `t`, and a `1/√(x − a)` times the map's
/// own rate becomes smooth: the integrand the rule sees is analytic again,
/// whether or not the span ends at a branch point, and the convergence is
/// exponential either way.
pub(crate) fn integrate(from: f64, to: f64, integrand: &dyn Fn(f64) -> f64) -> f64 {
    let span = to - from;
    if !span.is_finite() || span == 0.0 {
        return 0.0;
    }
    let pieces = ((span.abs() / 0.2).ceil() as usize).clamp(2, 512);
    let step = 1.0 / pieces as f64;
    let mut total = 0.0;
    for piece in 0..pieces {
        let low = step * piece as f64;
        let (half, middle) = (0.5 * step, step.mul_add(0.5, low));
        for (node, weight) in GAUSS_NODES {
            let t = half.mul_add(node, middle);
            let x = span.mul_add(t * t * 2.0f64.mul_add(-t, 3.0), from);
            let rate = 6.0 * span * t * (1.0 - t);
            total += weight * rate * integrand(x);
        }
    }
    // The half-width of each piece is the Jacobian of its map onto [−1, 1].
    total * 0.5 * step
}

/// One stretch of the curve read as a graph over the host's azimuth, from one
/// of its landmarks to the next.
///
/// `start` and `end` are the stretch's exact ends in the host's parameters.
/// Where the stretch ends at one of the host's own branch points that end is
/// the double root, and where it ends at one of the other cylinder's it is
/// that point carried across — never `height(x)` re-evaluated, because two
/// stretches that share an end have to share it to the bit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TraceArc {
    /// Which root: `+1` the upper, `−1` the lower.
    pub(crate) branch: f64,
    /// The host azimuths the stretch runs between.
    pub(crate) from: f64,
    pub(crate) to: f64,
    pub(crate) start: Point2,
    pub(crate) end: Point2,
}

/// A cubic B-spline standing in for a stretch of the curve, for a file format
/// that has no entity for the curve itself.
///
/// It is the Bézier pieces of a Hermite interpolation laid end to end: every
/// interior knot is doubled, so the spline is C¹ and each piece is its own
/// cubic between two points of the curve.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TraceSpline {
    /// The control polygon; the first and last are the stretch's two ends.
    pub(crate) control_points: Vec<Point3>,
    /// The distinct knots, rising, each with its multiplicity.
    pub(crate) knots: Vec<(f64, usize)>,
}

/// A point's parameters on a cylinder it lies on: the azimuth on its principal
/// branch, and the height along the axis in the axis's own units.
pub(crate) fn cylinder_local(cylinder: Cylinder, point: Point3) -> Point2 {
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
    Point2::new(azimuth, height)
}

/// `value` moved by whole turns to within half a turn of `near`.
pub(crate) fn nearest_turn(value: f64, near: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    tau.mul_add(((near - value) / tau).round(), value)
}

/// Whether two cylinder records describe one carrier, whatever frame each
/// walks it in: same radius, parallel axes, and each origin on the other's
/// axis line — to the agreement the engine uses, not to the last bit.
pub(crate) fn same_carrier(left: Cylinder, right: Cylinder) -> bool {
    let scale = left.radius.abs().max(right.radius.abs()).max(1.0);
    let tolerance = scale * 1.0e-9;
    let (left_length, right_length) = (left.axis.length(), right.axis.length());
    if left_length <= f64::EPSILON || right_length <= f64::EPSILON {
        return false;
    }
    let (left_axis, right_axis) = (left.axis / left_length, right.axis / right_length);
    (left.radius - right.radius).abs() <= tolerance
        && left_axis.cross(right_axis).length() <= 1.0e-9
        && (right.origin - left.origin).cross(left_axis).length() <= tolerance
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
        let k = axis.dot(e);
        let quadratic = axis_length_squared - k * k;
        if quadratic.abs() <= f64::EPSILON {
            // Parallel axes: the height falls out and the trace is a pair of
            // generators, which the intersection matrix already names.
            return None;
        }
        // Both origins are moved along their own axes to the two feet of the
        // axes' common perpendicular. The other cylinder is the same surface
        // from any point of its axis, and the host's heights are simply
        // measured from its foot and moved back by `offset`. It matters: a
        // cut's tool keeps its origin at the far end of its sweep, a thousand
        // away, and written from there the constant terms are a million that
        // cancel to a hundred — leaving the discriminant's zeros, the branch
        // points, a few picoradians adrift, which the square root turns into
        // a hundred-thousandth of height. From the feet, the terms are the
        // size of the axes' distance and the radii, and nothing cancels.
        let between = self.host.origin - self.other.origin;
        let (along_host, along_other) = (between.dot(axis), between.dot(e));
        let offset = k.mul_add(along_other, -along_host) / quadratic;
        let other_foot = along_other.mul_add(1.0, k * offset);
        let m = between + axis * offset - e * other_foot;
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
            offset,
        })
    }

    /// The host height at an azimuth, or `None` past a branch point where the
    /// two cylinders no longer meet.
    #[cfg(test)]
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
                .mul_add(0.5 / coefficients.quadratic, coefficients.offset),
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
        // A discriminant within its own rounding of zero is zero: at a
        // branch point the arithmetic lands a few ulps either side, and the
        // square root turns those ulps into a millionth of height. Read as
        // zero, every float that is the branch point evaluates to the double
        // root, which is the point both branches share.
        let discriminant = coefficients.discriminant_at(x);
        let root = if discriminant <= coefficients.rounding() {
            0.0
        } else {
            discriminant.sqrt()
        };
        self.branch
            .mul_add(root, -coefficients.linear_at(x))
            .mul_add(0.5 / coefficients.quadratic, coefficients.offset)
    }

    /// The point in space at any azimuth, clamped as [`Self::height_clamped`].
    pub(crate) fn point_clamped(self, x: f64) -> Point3 {
        self.host.evaluate(Point2::new(x, self.height_clamped(x)))
    }

    /// The tangent in space, with the height's rate read a hair inside the
    /// domain where the azimuth's own slope has run away.
    ///
    /// At a branch point the direction is still well defined — the curve is
    /// smooth — but this parameterization reaches it with infinite speed, so
    /// the rate is read just inside and is finite by construction. The
    /// radial part is the host's own at `x`, so a pcurve that reports
    /// `(1, slope_clamped(x))` maps onto exactly this vector.
    pub(crate) fn tangent_clamped(self, x: f64) -> Vector3 {
        let sign = self.host.angular_sign;
        let (sin, cos) = seam_snapped_sin_cos(sign * x);
        let radial_rate =
            (self.host.radial_v * cos - self.host.radial_u * sin) * (self.host.radius * sign);
        radial_rate + self.host.axis * self.slope_clamped(x)
    }

    /// `dy/dx`, read a hair inside the domain at a branch point, where the
    /// exact slope is unbounded.
    pub(crate) fn slope_clamped(self, x: f64) -> f64 {
        self.slope_at(x)
            .unwrap_or_else(|| self.slope_towards_domain(x))
    }

    /// This point of the curve in the other cylinder's parameter space, with
    /// the azimuth carried to within half a turn of `near`.
    ///
    /// The two faces either side of a trace read it over one parameter, or
    /// the sewer's midpoint weld compares two different points of the same
    /// curve and leaves the bodies apart. So the parameter here is the
    /// host's azimuth, and this is only the mapping into the other face's
    /// own coordinates. The arctangent behind that mapping jumps a whole
    /// turn once round; a piece of a face never spans a whole turn, so
    /// carrying every value near one azimuth of the face's own window keeps
    /// the piece continuous wherever the jump falls.
    pub(crate) fn on_other_near(self, x: f64, near: f64) -> Point2 {
        let local = cylinder_local(self.other, self.point_clamped(x));
        Point2::new(nearest_turn(local.x, near), local.y)
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
    #[cfg(test)]
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

    /// A cubic B-spline within `tolerance` of the curve from `from` to `to`,
    /// running from the point at `from` to the point at `to`.
    ///
    /// The azimuth is a poor parameter to interpolate over where the stretch
    /// ends at a branch point: the height moves like `√(x − x*)` there, which
    /// no polynomial follows. So the stretch is walked by a `t` in `[0, 1]`
    /// whose azimuth moves like `t²` towards such an end — the trick
    /// [`integrate`] plays — and the curve is analytic in `t` all the way.
    /// The pieces are Hermite cubics through the curve's points and rates in
    /// `t`, halved until each is within `tolerance` of the curve at its
    /// quarter points; neighbours share a point and a rate.
    ///
    /// `None` without coefficients, or when a few thousand pieces still do
    /// not meet the tolerance.
    pub(crate) fn spline(self, from: f64, to: f64, tolerance: f64) -> Option<TraceSpline> {
        let coefficients = self.coefficients()?;
        let span = to - from;
        if !span.is_finite() || span == 0.0 || tolerance.is_nan() || tolerance <= 0.0 {
            return None;
        }
        // How fast the discriminant opens into the stretch at an end that is
        // a branch point. An end where it does not open — a double root of
        // the discriminant, where the branches cross rather than turn — is
        // smooth in the azimuth and walked like any other.
        let opening = |x: f64, inward: f64| {
            let rate = coefficients.discriminant_slope_at(x) * inward;
            (coefficients.discriminant_at(x) <= coefficients.rounding()
                && rate > coefficients.rounding())
            .then_some(rate)
        };
        let (opens_at_start, opens_at_end) = (opening(from, span), opening(to, -span));
        // The azimuth and its rate at `t`. Towards a branch end `x − x*`
        // goes like `c·span·t²`, with `c` the constant each walk has there.
        let walk = |t: f64| -> (f64, f64) {
            let (x, rate) = match (opens_at_start.is_some(), opens_at_end.is_some()) {
                (true, true) => (
                    span.mul_add(t * t * 2.0f64.mul_add(-t, 3.0), from),
                    6.0 * span * t * (1.0 - t),
                ),
                (true, false) => (span.mul_add(t * t, from), 2.0 * span * t),
                (false, true) => (span.mul_add(t * (2.0 - t), from), 2.0 * span * (1.0 - t)),
                (false, false) => (span.mul_add(t, from), span),
            };
            // The ends are the stretch's own, to the bit.
            let x = if t <= 0.0 {
                from
            } else if t >= 1.0 {
                to
            } else {
                x
            };
            (x, rate)
        };
        let c = if opens_at_start.is_some() && opens_at_end.is_some() {
            3.0
        } else {
            1.0
        };
        let sample = |t: f64| -> (f64, Point3, Vector3) {
            let (x, rate) = walk(t);
            let point = self.point_clamped(x);
            let velocity = if rate == 0.0 {
                // At a branch end the azimuth stands still and only the
                // height moves: `√D` goes like `√(c·opening)·t` from the
                // start, and like that in `1 − t` towards the end.
                let (opening, towards) = if t <= 0.0 {
                    (opens_at_start.unwrap_or(0.0), 1.0)
                } else {
                    (opens_at_end.unwrap_or(0.0), -1.0)
                };
                self.host.axis
                    * (self.branch * towards * (c * opening).sqrt()
                        / (2.0 * coefficients.quadratic))
            } else {
                self.tangent_clamped(x) * rate
            };
            (t, point, velocity)
        };
        // The Hermite cubic between two samples at `u` of the way across.
        // Its point weights sum to one, so `P₀` plus the second's weight
        // times `P₁ − P₀` is the pair of them.
        let at = |left: (f64, Point3, Vector3), right: (f64, Point3, Vector3), u: f64| {
            let h = right.0 - left.0;
            let (u2, u3) = (u * u, u * u * u);
            let to_right = (-2.0f64).mul_add(u3, 3.0 * u2);
            let left_rate = 2.0f64.mul_add(-u2, u3) + u;
            let right_rate = u3 - u2;
            left.1
                + ((right.1 - left.1) * to_right
                    + left.2 * (h * left_rate)
                    + right.2 * (h * right_rate))
        };
        let initial = ((span.abs() / 0.25).ceil() as usize).clamp(2, 64);
        let mut pending: Vec<(f64, Point3, Vector3)> = (1..=initial)
            .rev()
            .map(|index| sample(index as f64 / initial as f64))
            .collect();
        let mut left = sample(0.0);
        let mut accepted = vec![left];
        while let Some(right) = pending.pop() {
            let within = [0.25, 0.5, 0.75].iter().all(|&u| {
                let t = (right.0 - left.0).mul_add(u, left.0);
                let (x, _) = walk(t);
                self.point_clamped(x).distance(at(left, right, u)) <= tolerance
            });
            if within {
                accepted.push(right);
                left = right;
                continue;
            }
            if accepted.len() + pending.len() > 4096 || right.0 - left.0 < 1.0e-9 {
                return None;
            }
            pending.push(right);
            pending.push(sample(0.5 * (left.0 + right.0)));
        }
        let mut control_points = vec![accepted[0].1];
        let mut knots = vec![(accepted[0].0, 4)];
        for pair in accepted.windows(2) {
            let ((t0, p0, v0), (t1, p1, v1)) = (pair[0], pair[1]);
            let third = (t1 - t0) / 3.0;
            control_points.push(p0 + v0 * third);
            control_points.push(p1 + v1 * -third);
            knots.push((t1, 2));
        }
        control_points.push(accepted[accepted.len() - 1].1);
        if let Some(last) = knots.last_mut() {
            last.1 = 4;
        }
        Some(TraceSpline {
            control_points,
            knots,
        })
    }

    /// The slope read a hair inside the domain, for the two parameters at the
    /// very ends of a span where the exact one is unbounded.
    ///
    /// Inside is wherever the discriminant rises; the step widens until it
    /// clears the floor below which a discriminant counts as zero.
    fn slope_towards_domain(self, x: f64) -> f64 {
        let Some(coefficients) = self.coefficients() else {
            return 0.0;
        };
        for step in [1.0e-9, 1.0e-8, 1.0e-7, 1.0e-6] {
            let inward = if coefficients.discriminant_at(x + step)
                >= coefficients.discriminant_at(x - step)
            {
                step
            } else {
                -step
            };
            if let Some(slope) = self.slope_at(x + inward) {
                return slope;
            }
        }
        0.0
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
        // The very arithmetic `height_clamped` does with the root at zero,
        // so an end written as the double root and the curve evaluated at
        // that end are one point to the bit.
        (-coefficients.linear_at(x)).mul_add(0.5 / coefficients.quadratic, coefficients.offset)
    }

    /// Which root a point of the host's parameter space lies on: `+1` above
    /// the double root, `−1` below it, and `None` where the two roots are
    /// too close together to tell — at a branch point, where both are one.
    ///
    /// The quadratic's leading coefficient `|n|² − (n·e)²` is never negative,
    /// so the upper root is always the `+√D` one.
    pub(crate) fn branch_at(self, point: Point2) -> Option<f64> {
        let coefficients = self.coefficients()?;
        let middle = self.double_root_at(point.x);
        let half_gap =
            coefficients.discriminant_at(point.x).max(0.0).sqrt() / (2.0 * coefficients.quadratic);
        let scale = self.host.radius.abs().max(self.other.radius.abs()).max(1.0);
        if half_gap <= scale * 1.0e-9 {
            return None;
        }
        Some(if point.y >= middle { 1.0 } else { -1.0 })
    }

    /// The branch points of this reading over one turn, each once, with the
    /// turn's two ends counted as the one azimuth they are.
    fn branch_points_once(self) -> Vec<f64> {
        let tau = std::f64::consts::TAU;
        // A root found at the far end of the turn is the one at its start;
        // it is found again there, not moved there by subtracting a turn,
        // which would hand back a float the discriminant is positive at.
        let mut roots: Vec<f64> = self
            .branch_points(0.0, tau)
            .into_iter()
            .map(|root| {
                if root >= tau - 1.0e-9 {
                    self.branch_point_at(root - tau).unwrap_or(root - tau)
                } else {
                    root
                }
            })
            .collect();
        roots.sort_by(f64::total_cmp);
        roots.dedup_by(|later, earlier| (*later - *earlier).abs() <= 1.0e-9);
        roots
    }

    /// The whole curve — both branches — as graphs over the host's azimuth,
    /// cut at every landmark.
    ///
    /// A landmark is a point where either cylinder's azimuth stops being a
    /// parameter of the curve: one of the host's branch points, where the
    /// curve runs along the host's generators, or one of the other's. They
    /// belong to the pair, not to whichever face is asking, and they are
    /// computed the same way whichever cylinder is the host here — the host's
    /// from this reading and the other's from the flipped one — so the two
    /// faces a curve separates cut it at the same points in space. That is
    /// what lets their pieces weld, and it is also what makes every piece a
    /// graph over *both* azimuths, so either face can re-read any piece.
    ///
    /// Nothing else cuts the curve: not a whole turn, not an azimuth zero.
    /// Those would be points of one face's frame, and the other face would
    /// have no reason to cut there.
    ///
    /// `None` refuses a curve with no landmark on some loop: one that winds
    /// round both cylinders at once. Two unequal cylinders cannot do that —
    /// the narrower one's shadow along the wider one's axis is a strip too
    /// thin to go round it — and two equal ones that do meet in the ellipses
    /// the intersection matrix names instead.
    pub(crate) fn arcs(self) -> Option<Vec<TraceArc>> {
        let tau = std::f64::consts::TAU;
        let coefficients = self.coefficients()?;
        let reverse = self.flipped(1.0);
        reverse.coefficients()?;
        let roots = self.branch_points_once();

        // The stretches of the host's azimuth where the cylinders meet: each
        // bounded by two branch points, or the whole turn when the curve
        // winds round the host and has none.
        let mut spans: Vec<(f64, f64)> = Vec::new();
        let winds = roots.is_empty();
        if winds {
            if coefficients.discriminant_at(0.0) <= 0.0 {
                return Some(Vec::new());
            }
            spans.push((0.0, tau));
        } else {
            for (index, from) in roots.iter().copied().enumerate() {
                let to = roots.get(index + 1).copied().unwrap_or_else(|| {
                    self.branch_point_at(roots[0] + tau)
                        .unwrap_or(roots[0] + tau)
                });
                if to - from > 1.0e-9 && coefficients.discriminant_at(0.5 * (from + to)) > 0.0 {
                    spans.push((from, to));
                }
            }
        }

        // The other reading's branch points, carried into this one, each on
        // the root it belongs to. One that is also a branch point here is a
        // tangency of the two readings and is already an end.
        let mut landmarks: Vec<(Point2, f64)> = Vec::new();
        for azimuth in reverse.branch_points_once() {
            let point = self
                .other
                .evaluate(Point2::new(azimuth, reverse.double_root_at(azimuth)));
            let local = cylinder_local(self.host, point);
            if let Some(branch) = self.branch_at(local) {
                landmarks.push((local, branch));
            }
        }

        let mut arcs = Vec::new();
        for (from, to) in spans {
            for branch in [1.0, -1.0] {
                let mut cuts: Vec<Point2> = landmarks
                    .iter()
                    .filter(|(_, on)| *on == branch)
                    .map(|(point, _)| {
                        let x = if winds {
                            point.x.rem_euclid(tau)
                        } else {
                            nearest_turn(point.x, 0.5 * (from + to))
                        };
                        Point2::new(x, point.y)
                    })
                    .filter(|point| winds || (point.x > from + 1.0e-9 && point.x < to - 1.0e-9))
                    .collect();
                cuts.sort_by(|left, right| left.x.total_cmp(&right.x));
                cuts.dedup_by(|later, earlier| (later.x - earlier.x).abs() <= 1.0e-9);
                let ends: Vec<Point2> = if winds {
                    // A loop round the host: from each landmark to the next,
                    // and from the last round to the first a turn on.
                    let first = *cuts.first()?;
                    cuts.push(Point2::new(first.x + tau, first.y));
                    cuts
                } else {
                    // Both roots share the double root at a branch point, so
                    // the two branches of a span meet there to the bit.
                    let mut ends = vec![Point2::new(from, self.double_root_at(from))];
                    ends.extend(cuts);
                    ends.push(Point2::new(to, self.double_root_at(to)));
                    ends
                };
                for pair in ends.windows(2) {
                    arcs.push(TraceArc {
                        branch,
                        from: pair[0].x,
                        to: pair[1].x,
                        start: pair[0],
                        end: pair[1],
                    });
                }
            }
        }
        Some(arcs)
    }

    /// A stretch of this curve read over another cylinder's azimuth.
    ///
    /// `target` is one of the two cylinders the curve lies on, in whatever
    /// frame: this reading's host in another record, or its other. The
    /// stretch comes back as a graph over `target`'s azimuth, with the
    /// remaining cylinder as its other. `from`/`to` are this reading's
    /// azimuths and `ends` the stretch's exact ends in space; the interior is
    /// sampled, carried onto one continuous branch of the target's azimuth,
    /// and the root it lies on is read off the middle.
    ///
    /// The target's azimuth has to run one way along the whole stretch — it
    /// does between landmarks, which is where every piece is cut — and
    /// `None` says it did not, rather than folding a stretch that turns back
    /// into a graph it is not.
    pub(crate) fn read_on(
        self,
        target: Cylinder,
        from: f64,
        to: f64,
        ends: [Point3; 2],
    ) -> Option<(Self, TraceArc)> {
        const SAMPLES: usize = 16;
        let remaining = if same_carrier(target, self.other) {
            self.host
        } else if same_carrier(target, self.host) {
            self.other
        } else {
            return None;
        };
        let reading = Self {
            host: target,
            other: remaining,
            branch: 1.0,
        };
        reading.coefficients()?;
        let mut samples: Vec<Point2> = Vec::with_capacity(SAMPLES + 1);
        for index in 0..=SAMPLES {
            let point = match index {
                0 => ends[0],
                SAMPLES => ends[1],
                _ => self.point_clamped((to - from).mul_add(index as f64 / SAMPLES as f64, from)),
            };
            let mut local = cylinder_local(target, point);
            if let Some(previous) = samples.last() {
                local.x = nearest_turn(local.x, previous.x);
            }
            samples.push(local);
        }
        let rising = samples[SAMPLES].x > samples[0].x;
        if samples
            .windows(2)
            .any(|pair| pair[1].x == pair[0].x || (pair[1].x > pair[0].x) != rising)
        {
            return None;
        }
        // The middle of a stretch is never a branch point of either reading,
        // but a stretch that grazes one near its middle is asked at the
        // quarters too before giving up.
        let branch = [SAMPLES / 2, SAMPLES / 4, 3 * SAMPLES / 4]
            .into_iter()
            .find_map(|index| reading.branch_at(samples[index]))?;
        // An end at one of the target's own branch points has come round by
        // arithmetic, and is put back on the branch point exactly: there the
        // new parameter is singular, and the curve read a few ulps off it is
        // a hundred-thousandth away from the vertex it has to meet.
        //
        // Whether an end *is* the branch point is asked in space, not in the
        // azimuth. One ulp of azimuth from a branch point already parts the
        // two roots by a hundred-thousandth, so neither the gap between them
        // nor the azimuth's own distance can tell an end that arrived a few
        // ulps off from one that really lies a little way along the curve.
        // The distance in space can: the branch point is taken when it is the
        // same point to within the weld every vertex is joined by.
        let settle = |end: Point2, point: Point3| -> Point2 {
            let Some(x) = reading.branch_point_at(end.x) else {
                return end;
            };
            let at = Point2::new(x, reading.double_root_at(x));
            let scale = 1.0 + point.x.abs().max(point.y.abs()).max(point.z.abs());
            if (target.evaluate(at) - point).length() <= 32.0e-9 * scale {
                at
            } else {
                end
            }
        };
        let (start, end) = (
            settle(samples[0], ends[0]),
            settle(samples[SAMPLES], ends[1]),
        );
        Some((
            Self { branch, ..reading },
            TraceArc {
                branch,
                from: start.x,
                to: end.x,
                start,
                end,
            },
        ))
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
        let bisect = |value: &dyn Fn(f64) -> f64, mut low: f64, mut high: f64| -> (f64, f64) {
            let low_value = value(low);
            for _ in 0..80 {
                let middle = 0.5 * (low + high);
                if (value(middle) < 0.0) == (low_value < 0.0) {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            (low, high)
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
                // The bracket closes on two neighbouring floats, one either
                // side of the root; the one where the discriminant is not
                // positive is taken. Evaluated there the root is the double
                // one to the bit, where a float a hair inside would put the
                // point `√D` along the curve — a millionth, not an ulp,
                // because the azimuth is a singular parameter here.
                let (low, high) = bisect(&discriminant, previous.0, current.0);
                push(
                    if discriminant(low) <= 0.0 { low } else { high },
                    &mut found,
                );
            } else if (previous.2 < 0.0) != (current.2 < 0.0) {
                // An extremum: a double root if it sits on zero.
                let (low, high) = bisect(&slope, previous.0, current.0);
                push(0.5 * (low + high), &mut found);
            } else if current.1.abs() <= floor {
                push(current.0, &mut found);
            }
            previous = current;
        }
        found.sort_by(f64::total_cmp);
        found
    }

    /// The azimuth of the branch point `x` sits at, when it sits at one.
    ///
    /// An end that should be a branch point but arrives by another route — a
    /// point mapped across from the other cylinder and back — lands a few
    /// ulps off it, and there a few ulps of azimuth are a millionth of
    /// height. So an end whose two roots cannot be told apart is moved onto
    /// the neighbouring float where the discriminant is not positive, which
    /// evaluates to the double root exactly. `None` when `x` is not at a
    /// branch point.
    pub(crate) fn branch_point_at(self, x: f64) -> Option<f64> {
        let coefficients = self.coefficients()?;
        let reach = 1.0e-6;
        self.branch_points(x - reach, x + reach)
            .into_iter()
            .filter(|root| coefficients.discriminant_at(*root) <= 0.0)
            .min_by(|left, right| (left - x).abs().total_cmp(&(right - x).abs()))
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

    fn distance_from(cylinder: Cylinder, point: Point3) -> f64 {
        let axis = cylinder.axis / cylinder.axis.length();
        let offset = point - cylinder.origin;
        ((offset - axis * offset.dot(axis)).length() - cylinder.radius).abs()
    }

    /// The pairs the tests below walk: a narrower bore straight through a
    /// wider one, where the wider's reading has branch points, and a skew
    /// pair lying far from both origins.
    fn pairs() -> Vec<(Cylinder, Cylinder)> {
        vec![
            (
                cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 8.0),
                cylinder([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 5.0),
            ),
            (
                cylinder([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 5.0),
                cylinder([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 8.0),
            ),
            (
                cylinder([400.0, -3.0, 2.0], [0.0, 0.0, 1.0], 9.0),
                cylinder([192.0, -700.0, -169.0], [0.3, 1.0, 0.25], 6.0),
            ),
        ]
    }

    /// Every stretch between landmarks is a piece of the curve that shares
    /// its ends with its neighbours, and together they are the whole of it.
    #[test]
    fn the_arcs_are_the_curve_cut_at_its_landmarks() {
        for (host, other) in pairs() {
            let arcs = CylinderTrace {
                host,
                other,
                branch: 1.0,
            }
            .arcs()
            .expect("crossing axes");
            assert!(arcs.len() >= 2, "{arcs:?}");
            for arc in &arcs {
                let trace = CylinderTrace {
                    host,
                    other,
                    branch: arc.branch,
                };
                for (x, end) in [(arc.from, arc.start), (arc.to, arc.end)] {
                    assert_eq!(end.x, x);
                    assert!((trace.height_clamped(x) - end.y).abs() < 1.0e-9);
                    assert!(distance_from(other, host.evaluate(end)) < 1.0e-9);
                }
                // Every end is another arc's end too: the stretches close up.
                for end in [arc.start, arc.end] {
                    let shared = arcs
                        .iter()
                        .filter(|neighbour| {
                            [neighbour.start, neighbour.end].iter().any(|point| {
                                host.evaluate(*point).distance(host.evaluate(end)) < 1.0e-9
                            })
                        })
                        .count();
                    assert!(shared >= 2, "an arc end no other arc reaches: {end:?}");
                }
            }
        }
    }

    /// A stretch read over the other cylinder's azimuth is the same set of
    /// points, and reading it back gives the stretch it started as.
    #[test]
    fn a_stretch_read_on_the_other_cylinder_is_the_same_curve() {
        let mut read_back = 0;
        for (host, other) in pairs() {
            let arcs = CylinderTrace {
                host,
                other,
                branch: 1.0,
            }
            .arcs()
            .expect("crossing axes");
            for arc in arcs {
                let trace = CylinderTrace {
                    host,
                    other,
                    branch: arc.branch,
                };
                let ends = [host.evaluate(arc.start), host.evaluate(arc.end)];
                let Some((read, piece)) = trace.read_on(other, arc.from, arc.to, ends) else {
                    // The other's azimuth turns back inside this stretch; the
                    // callers cut there first, so there is nothing to read.
                    continue;
                };
                assert!(same_carrier(read.host, other));
                assert!(other.evaluate(piece.start).distance(ends[0]) < 1.0e-9);
                assert!(other.evaluate(piece.end).distance(ends[1]) < 1.0e-9);
                for step in 1..8 {
                    let x = (piece.to - piece.from).mul_add(f64::from(step) / 8.0, piece.from);
                    let point = read.point_clamped(x);
                    assert!(distance_from(host, point) < 1.0e-9);
                    assert!(distance_from(other, point) < 1.0e-9);
                }
                let (back, again) = read
                    .read_on(host, piece.from, piece.to, ends)
                    .expect("a stretch reads back onto its own host");
                assert_eq!(back.branch, arc.branch);
                // On the principal branch of the host's azimuth, which is
                // the stretch's own window a whole turn away or not at all.
                let turn = nearest_turn(again.from, arc.from) - again.from;
                assert!((again.from + turn - arc.from).abs() < 1.0e-9);
                assert!((again.to + turn - arc.to).abs() < 1.0e-9);
                read_back += 1;
            }
        }
        assert!(read_back >= 6, "only {read_back} stretches read across");
    }

    /// A cubic B-spline's point at `t`, by de Boor's recursion.
    fn de_boor(knots: &[f64], points: &[Point3], t: f64) -> Point3 {
        let last = points.len() - 1;
        let span = (3..=last)
            .rev()
            .find(|&k| knots[k] <= t && knots[k] < knots[k + 1])
            .unwrap_or(3);
        let mut local: Vec<Point3> = (0..=3).map(|j| points[span - 3 + j]).collect();
        for r in 1..=3 {
            for j in (r..=3).rev() {
                let index = span - 3 + j;
                let alpha = (t - knots[index]) / (knots[index + 4 - r] - knots[index]);
                local[j] = local[j - 1] + (local[j] - local[j - 1]) * alpha;
            }
        }
        local[3]
    }

    /// The spline a file format carries in place of the curve stays on both
    /// cylinders to its tolerance, from end to end — including the ends at
    /// branch points, where the azimuth is no parameter to interpolate over —
    /// and begins and ends on the curve's own points.
    #[test]
    fn the_spline_stays_on_both_cylinders_to_its_tolerance() {
        let tolerance = 1.0e-7;
        for (host, other) in pairs() {
            let arcs = CylinderTrace {
                host,
                other,
                branch: 1.0,
            }
            .arcs()
            .expect("crossing axes");
            for arc in arcs {
                let trace = CylinderTrace {
                    host,
                    other,
                    branch: arc.branch,
                };
                // Both ways round: an edge's range can run either way.
                for (from, to) in [(arc.from, arc.to), (arc.to, arc.from)] {
                    let spline = trace
                        .spline(from, to, tolerance)
                        .expect("the stretch is fitted");
                    let points = &spline.control_points;
                    assert!(points.len() < 1000, "{} control points", points.len());
                    assert_eq!(points[0], trace.point_clamped(from));
                    assert_eq!(points[points.len() - 1], trace.point_clamped(to));
                    let knots: Vec<f64> = spline
                        .knots
                        .iter()
                        .flat_map(|(knot, count)| std::iter::repeat_n(*knot, *count))
                        .collect();
                    assert_eq!(knots.len(), points.len() + 4);
                    for window in spline.knots.windows(2) {
                        for step in 0..=24 {
                            let t = (window[1].0 - window[0].0)
                                .mul_add(f64::from(step) / 24.0, window[0].0);
                            let point = de_boor(&knots, points, t);
                            for cylinder in [host, other] {
                                let off = distance_from(cylinder, point);
                                assert!(off < 1.5 * tolerance, "{off} off at {t} of {from}..{to}");
                            }
                        }
                    }
                }
            }
        }
    }
}

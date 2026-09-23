//! The ruled surface (ADR 0049): the straight lines between two exact rails.
//!
//! `S(u, v) = (1 − v)·C₀(u) + v·C₁(u)` for `u, v ∈ [0, 1]`, where each rail
//! `Cᵢ` is a line, circular arc or elliptical arc of the edge vocabulary and
//! `u` runs linearly over the rail's own parameter range. Evaluation, both
//! partial derivatives and the normal `∂S/∂u × ∂S/∂v` are closed forms of
//! the rails. What has no closed form is kept here and bounded: inversion is
//! Newton's method with a fixed iteration limit that refuses rather than
//! guesses, the face integrals are composite Gauss–Legendre under the policy
//! ADR 0026 made normative for an ellipse's arc length, and the spline STEP
//! needs beside the surface is fitted to a stated tolerance.

use crate::bspline::settled_length;
use crate::topology::{Curve3, ParameterRange, Point2, Point3, Vector2, Vector3};

/// The curve a ruled surface is spanned from: the conics of the edge
/// vocabulary. A trace is not a rail; nothing lofts from one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RailCurve {
    Line {
        endpoints: [Point3; 2],
    },
    Circle {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
    },
    Ellipse {
        center: Point3,
        u: Vector3,
        v: Vector3,
        major_radius: f64,
        minor_radius: f64,
    },
}

impl RailCurve {
    /// The same curve as an edge carries it.
    pub(crate) const fn curve(self) -> Curve3 {
        match self {
            Self::Line { endpoints } => Curve3::Line { endpoints },
            Self::Circle {
                center,
                u,
                v,
                radius,
            } => Curve3::Circle {
                center,
                u,
                v,
                radius,
            },
            Self::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            } => Curve3::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            },
        }
    }

    /// The second derivative with respect to the curve's own parameter.
    fn second_derivative(self, parameter: f64) -> Vector3 {
        match self {
            Self::Line { .. } => Vector3::new(0.0, 0.0, 0.0),
            Self::Circle { u, v, radius, .. } => {
                let (sin, cos) = parameter.sin_cos();
                (u * (radius * cos) + v * (radius * sin)) * -1.0
            }
            Self::Ellipse {
                u,
                v,
                major_radius,
                minor_radius,
                ..
            } => {
                let (sin, cos) = parameter.sin_cos();
                (u * (major_radius * cos) + v * (minor_radius * sin)) * -1.0
            }
        }
    }

    /// How tightly the curve can bend: its radius, or an ellipse's major
    /// radius, which bounds the sagitta of any chord; a line never bends.
    pub(crate) const fn bending_radius(self) -> Option<f64> {
        match self {
            Self::Line { .. } => None,
            Self::Circle { radius, .. } => Some(radius),
            Self::Ellipse { major_radius, .. } => Some(major_radius),
        }
    }

    pub(crate) fn is_finite(self) -> bool {
        self.curve().is_finite()
    }
}

/// One rail: an exact curve and the stretch of it the surface spans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuledRail {
    pub(crate) curve: RailCurve,
    pub(crate) range: ParameterRange,
}

impl RuledRail {
    /// The curve's own parameter at `u` of the way along the rail, in the
    /// one rounding an edge sampled at the same fraction uses.
    pub(crate) fn parameter(self, u: f64) -> f64 {
        (self.range.end - self.range.start).mul_add(u, self.range.start)
    }

    fn span(self) -> f64 {
        self.range.end - self.range.start
    }

    pub(crate) fn point(self, u: f64) -> Point3 {
        self.curve.curve().evaluate(self.parameter(u))
    }

    /// `dC/du`, the rail's rate in the surface's own parameter.
    pub(crate) fn rate(self, u: f64) -> Vector3 {
        self.curve.curve().derivative(self.parameter(u)) * self.span()
    }

    /// `d²C/du²`.
    fn acceleration(self, u: f64) -> Vector3 {
        let span = self.span();
        self.curve.second_derivative(self.parameter(u)) * (span * span)
    }

    /// The same stretch walked the other way.
    pub(crate) const fn reversed(self) -> Self {
        Self {
            curve: self.curve,
            range: self.range.reversed(),
        }
    }

    /// The angle the rail turns through, zero for a line.
    pub(crate) fn sweep(self) -> f64 {
        match self.curve {
            RailCurve::Line { .. } => 0.0,
            RailCurve::Circle { .. } | RailCurve::Ellipse { .. } => self.span().abs(),
        }
    }

    pub(crate) fn is_finite(self) -> bool {
        self.curve.is_finite() && self.range.is_finite()
    }
}

/// A surface ruled between two rails, parameterised over the unit square.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuledSurface {
    pub(crate) rails: [RuledRail; 2],
}

impl RuledSurface {
    /// `S(u, v)`. The two rails are returned as the rails evaluate them,
    /// to the bit, so an edge along either rail and the surface agree on
    /// every point they share.
    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        let low = self.rails[0].point(point.x);
        if point.y == 0.0 {
            return low;
        }
        let high = self.rails[1].point(point.x);
        if point.y == 1.0 {
            return high;
        }
        low + (high - low) * point.y
    }

    /// `S`, `∂S/∂u` and `∂S/∂v` at one parameter pair.
    pub(crate) fn frame(self, point: Point2) -> (Point3, Vector3, Vector3) {
        let low = self.rails[0].point(point.x);
        let high = self.rails[1].point(point.x);
        let rung = high - low;
        let along =
            self.rails[0].rate(point.x) * (1.0 - point.y) + self.rails[1].rate(point.x) * point.y;
        (self.evaluate(point), along, rung)
    }

    /// `∂²S/∂u²` and `∂²S/∂u∂v`; `∂²S/∂v²` is zero, which is what makes the
    /// surface ruled.
    fn second(self, point: Point2) -> (Vector3, Vector3) {
        let bend = self.rails[0].acceleration(point.x) * (1.0 - point.y)
            + self.rails[1].acceleration(point.x) * point.y;
        let twist = self.rails[1].rate(point.x) - self.rails[0].rate(point.x);
        (bend, twist)
    }

    /// The unnormalised normal `∂S/∂u × ∂S/∂v`, which points out of the
    /// material by the builder's choice of rail order.
    pub(crate) fn normal(self, point: Point2) -> Vector3 {
        let (_, along, rung) = self.frame(point);
        along.cross(rung)
    }

    pub(crate) fn unit_normal(self, point: Point2) -> Option<Vector3> {
        let normal = self.normal(point);
        let length = normal.length();
        (length.is_finite() && length > f64::EPSILON).then(|| normal / length)
    }

    pub(crate) fn map_tangent(self, point: Point2, tangent: Vector2) -> Vector3 {
        let (_, along, rung) = self.frame(point);
        along * tangent.x + rung * tangent.y
    }

    pub(crate) fn is_finite(self) -> bool {
        self.rails.iter().all(|rail| rail.is_finite())
    }

    /// The same surface with `u` walked the other way: `S'(u, v) = S(1 − u,
    /// v)`, whose normal is the opposite one.
    pub(crate) const fn reversed_u(self) -> Self {
        Self {
            rails: [self.rails[0].reversed(), self.rails[1].reversed()],
        }
    }

    /// A length the surface is the size of, for scaling tolerances.
    pub(crate) fn scale(self) -> f64 {
        let corners = [
            self.evaluate(Point2::new(0.0, 0.0)),
            self.evaluate(Point2::new(1.0, 0.0)),
            self.evaluate(Point2::new(1.0, 1.0)),
            self.evaluate(Point2::new(0.0, 1.0)),
        ];
        let mut scale = 0.0_f64;
        for (index, corner) in corners.iter().enumerate() {
            scale = scale.max(corner.distance(corners[(index + 1) % 4]));
            scale = scale.max(corner.distance(corners[(index + 2) % 4]));
        }
        for rail in self.rails {
            if let Some(radius) = rail.curve.bending_radius() {
                scale = scale.max(radius.abs());
            }
        }
        scale
    }

    /// The parameters of the point of the surface nearest `target`.
    ///
    /// Newton's method on the squared distance with its exact Hessian, from
    /// `seed` or, without one, from the best of a sweep along `u` in which
    /// each rung's own nearest point is exact — the surface is linear along
    /// a rung. A step that would move away is halved, parameters stay within
    /// half the domain again of it, and a walk that has not settled within
    /// the iteration limit is refused rather than returned.
    pub(crate) fn invert(self, target: Point3, seed: Option<Point2>) -> Option<Point2> {
        if !target.is_finite() {
            return None;
        }
        let distance_squared = |point: Point2| {
            let offset = self.evaluate(point) - target;
            offset.dot(offset)
        };
        let mut current = match seed {
            Some(seed) if seed.is_finite() => seed,
            _ => self.coarse_seed(target)?,
        };
        let mut value = distance_squared(current);
        const ITERATIONS: usize = 64;
        const LIMIT: (f64, f64) = (-0.5, 1.5);
        for _ in 0..ITERATIONS {
            let (point, along, rung) = self.frame(current);
            let (bend, twist) = self.second(current);
            let offset = point - target;
            let gradient = [offset.dot(along), offset.dot(rung)];
            let uu = along.dot(along) + offset.dot(bend);
            let uv = along.dot(rung) + offset.dot(twist);
            let vv = rung.dot(rung);
            let determinant = uu.mul_add(vv, -(uv * uv));
            let step = if determinant.is_finite() && determinant > 0.0 && uu > 0.0 {
                Point2::new(
                    (vv * gradient[0] - uv * gradient[1]) / determinant,
                    (uu * gradient[1] - uv * gradient[0]) / determinant,
                )
            } else {
                // Away from a minimum the Hessian may not be positive; a
                // scaled gradient step still goes downhill.
                let scale = along.dot(along).max(vv).max(f64::MIN_POSITIVE);
                Point2::new(gradient[0] / scale, gradient[1] / scale)
            };
            if !step.is_finite() {
                return None;
            }
            let mut length = 1.0;
            let mut accepted = None;
            for _ in 0..32 {
                let candidate = Point2::new(
                    (current.x - step.x * length).clamp(LIMIT.0, LIMIT.1),
                    (current.y - step.y * length).clamp(LIMIT.0, LIMIT.1),
                );
                let candidate_value = distance_squared(candidate);
                if candidate_value <= value {
                    accepted = Some((candidate, candidate_value));
                    break;
                }
                length *= 0.5;
            }
            let Some((next, next_value)) = accepted else {
                // No step reduces the distance: the walk is at the minimum to
                // the precision the arithmetic carries.
                return Some(current);
            };
            let moved = (next.x - current.x).abs().max((next.y - current.y).abs());
            // Far from the origin the point is only known to the rounding of
            // its coordinates, and a step within that is noise: the walk has
            // settled there as surely as where the parameters stop moving,
            // or where a step no longer brings the point nearer.
            let carried = (along * (next.x - current.x) + rung * (next.y - current.y)).length();
            let stalled = next_value >= value;
            current = next;
            value = next_value;
            if moved <= 4.0 * f64::EPSILON || stalled || carried <= settled_length(point) {
                return Some(current);
            }
        }
        None
    }

    /// The best of a sweep of rungs, each with its own exact nearest point.
    fn coarse_seed(self, target: Point3) -> Option<Point2> {
        const SAMPLES: usize = 64;
        let mut best = None::<(f64, Point2)>;
        for index in 0..=SAMPLES {
            let u = index as f64 / SAMPLES as f64;
            let low = self.rails[0].point(u);
            let rung = self.rails[1].point(u) - low;
            let length_squared = rung.dot(rung);
            let v = if length_squared > f64::MIN_POSITIVE {
                ((target - low).dot(rung) / length_squared).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let offset = low + rung * v - target;
            let distance = offset.dot(offset);
            if distance.is_finite() && best.is_none_or(|(least, _)| distance < least) {
                best = Some((distance, Point2::new(u, v)));
            }
        }
        best.map(|(_, point)| point)
    }

    /// How far `target` sits from the surface, found by inversion.
    pub(crate) fn distance_to(self, target: Point3, seed: Option<Point2>) -> Option<f64> {
        let found = self.invert(target, seed)?;
        let distance = self.evaluate(found).distance(target);
        distance.is_finite().then_some(distance)
    }

    /// The smallest length the normal reaches on a grid over the domain,
    /// relative to the square of the surface's own scale. Zero where a wall
    /// pinches: a rung that shrinks to a point, or two rails whose tangents
    /// line up with the rung between them.
    pub(crate) fn least_normal(self, domain: (f64, f64, f64, f64)) -> f64 {
        const ROWS: usize = 8;
        const COLUMNS: usize = 32;
        let (u_min, u_max, v_min, v_max) = domain;
        let mut least = f64::INFINITY;
        for column in 0..=COLUMNS {
            let u = (u_max - u_min).mul_add(column as f64 / COLUMNS as f64, u_min);
            for row in 0..=ROWS {
                let v = (v_max - v_min).mul_add(row as f64 / ROWS as f64, v_min);
                least = least.min(self.normal(Point2::new(u, v)).length());
            }
        }
        least
    }

    /// Area, flux and first moment of the face covering the parameter
    /// rectangle `domain`, measured from `anchor`.
    ///
    /// With `N = ∂S/∂u × ∂S/∂v` the area is `∫∫|N|`, the flux of `x − p`
    /// that three times a volume sums is `∫∫(S − p)·N`, and the moment a
    /// centroid needs is `∫∫½|S − p|²·N`, all over the rectangle. Along `v`
    /// the last two are polynomials of degree two and three, and the rule
    /// is exact for them; the area's integrand is the length of a normal
    /// linear in `v`, and every integrand is analytic along `u`, where the
    /// composite rule converges exponentially. ADR 0026 counts an integral
    /// so evaluated as a closed form.
    pub(crate) fn measures(self, domain: (f64, f64, f64, f64), anchor: Point3) -> RuledMeasures {
        let (u_min, u_max, v_min, v_max) = domain;
        let sweep = self.rails[0].sweep().max(self.rails[1].sweep());
        let u_pieces = ((sweep * (u_max - u_min).abs() / 0.2).ceil() as usize).clamp(4, 512);
        let v_pieces = 4;
        let mut area = 0.0;
        let mut flux = 0.0;
        let mut moment = Vector3::new(0.0, 0.0, 0.0);
        let u_step = (u_max - u_min) / u_pieces as f64;
        let v_step = (v_max - v_min) / v_pieces as f64;
        for u_piece in 0..u_pieces {
            let u_middle = u_step.mul_add(u_piece as f64 + 0.5, u_min);
            for (u_node, u_weight) in GAUSS_NODES {
                let u = (0.5 * u_step).mul_add(u_node, u_middle);
                for v_piece in 0..v_pieces {
                    let v_middle = v_step.mul_add(v_piece as f64 + 0.5, v_min);
                    for (v_node, v_weight) in GAUSS_NODES {
                        let v = (0.5 * v_step).mul_add(v_node, v_middle);
                        let (point, along, rung) = self.frame(Point2::new(u, v));
                        let normal = along.cross(rung);
                        let offset = point - anchor;
                        let weight = u_weight * v_weight;
                        area += weight * normal.length();
                        flux += weight * offset.dot(normal);
                        moment = moment + normal * (weight * 0.5 * offset.dot(offset));
                    }
                }
            }
        }
        // Each piece's half-widths are the Jacobians of its maps onto the
        // reference square.
        let jacobian = 0.25 * u_step * v_step;
        RuledMeasures {
            area: area * jacobian.abs(),
            flux: flux * jacobian,
            moment: moment * jacobian,
        }
    }

    /// Cubic fits of both rails on one knot vector, each within `tolerance`
    /// of its rail at every sample the adaptive walk checks: the B-spline
    /// surface of degree three in `u` and one in `v` whose rows are these
    /// fits lies within `tolerance` of the ruled surface everywhere, because
    /// both are linear in `v`.
    ///
    /// The fits are C¹ chains of Hermite cubics through the rails' own points
    /// and rates, halved where either misses at a quarter point, laid end to
    /// end with every interior knot doubled — the construction the trace's
    /// spline uses (ADR 0047).
    pub(crate) fn rail_splines(self, tolerance: f64) -> Option<RailSplines> {
        if !(tolerance.is_finite() && tolerance > 0.0) {
            return None;
        }
        let sample = |u: f64| -> (f64, [Point3; 2], [Vector3; 2]) {
            let point = |index: usize| {
                if u <= 0.0 {
                    self.rails[index].point(0.0)
                } else if u >= 1.0 {
                    self.rails[index].point(1.0)
                } else {
                    self.rails[index].point(u)
                }
            };
            (
                u,
                [point(0), point(1)],
                [self.rails[0].rate(u), self.rails[1].rate(u)],
            )
        };
        let hermite = |left: &(f64, [Point3; 2], [Vector3; 2]),
                       right: &(f64, [Point3; 2], [Vector3; 2]),
                       rail: usize,
                       t: f64| {
            let h = right.0 - left.0;
            let (t2, t3) = (t * t, t * t * t);
            let to_right = (-2.0f64).mul_add(t3, 3.0 * t2);
            let left_rate = 2.0f64.mul_add(-t2, t3) + t;
            let right_rate = t3 - t2;
            left.1[rail]
                + ((right.1[rail] - left.1[rail]) * to_right
                    + left.2[rail] * (h * left_rate)
                    + right.2[rail] * (h * right_rate))
        };
        let sweep = self.rails[0].sweep().max(self.rails[1].sweep());
        let initial = ((sweep / 0.25).ceil() as usize).clamp(1, 64);
        let mut pending = (1..=initial)
            .rev()
            .map(|index| sample(index as f64 / initial as f64))
            .collect::<Vec<_>>();
        let mut left = sample(0.0);
        let mut accepted = vec![left];
        while let Some(right) = pending.pop() {
            let within = [0.25, 0.5, 0.75].iter().all(|&t| {
                let u = (right.0 - left.0).mul_add(t, left.0);
                (0..2).all(|rail| {
                    self.rails[rail]
                        .point(u)
                        .distance(hermite(&left, &right, rail, t))
                        <= tolerance
                })
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
        let mut rows = [vec![accepted[0].1[0]], vec![accepted[0].1[1]]];
        let mut knots = vec![(accepted[0].0, 4)];
        for pair in accepted.windows(2) {
            let (left, right) = (&pair[0], &pair[1]);
            let third = (right.0 - left.0) / 3.0;
            for (rail, row) in rows.iter_mut().enumerate() {
                row.push(left.1[rail] + left.2[rail] * third);
                row.push(right.1[rail] + right.2[rail] * -third);
            }
            knots.push((right.0, 2));
        }
        let last = accepted[accepted.len() - 1];
        rows[0].push(last.1[0]);
        rows[1].push(last.1[1]);
        if let Some(knot) = knots.last_mut() {
            knot.1 = 4;
        }
        Some(RailSplines { rows, knots })
    }
}

/// The face integrals of one ruled face, see [`RuledSurface::measures`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RuledMeasures {
    pub(crate) area: f64,
    pub(crate) flux: f64,
    pub(crate) moment: Vector3,
}

/// Two cubic B-spline rows on one knot vector: the control net of the
/// B-spline surface that stands for a ruled surface in a STEP file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RailSplines {
    /// The control points of the fit to rail 0 and to rail 1.
    pub(crate) rows: [Vec<Point3>; 2],
    /// The distinct knots, rising, each with its multiplicity.
    pub(crate) knots: Vec<(f64, usize)>,
}

/// Ten-point Gauss–Legendre nodes and weights on `[−1, 1]`, the rule the
/// trace's integrals use.
pub(crate) const GAUSS_NODES: [(f64, f64); 10] = [
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(a: [f64; 3], b: [f64; 3]) -> RuledRail {
        RuledRail {
            curve: RailCurve::Line {
                endpoints: [Point3::new(a[0], a[1], a[2]), Point3::new(b[0], b[1], b[2])],
            },
            range: ParameterRange::new(0.0, 1.0),
        }
    }

    fn arc(center: [f64; 3], radius: f64, from: f64, to: f64) -> RuledRail {
        RuledRail {
            curve: RailCurve::Circle {
                center: Point3::new(center[0], center[1], center[2]),
                u: Vector3::new(1.0, 0.0, 0.0),
                v: Vector3::new(0.0, 1.0, 0.0),
                radius,
            },
            range: ParameterRange::new(from, to),
        }
    }

    #[test]
    fn inversion_recovers_the_parameters_of_its_own_points() {
        let surface = RuledSurface {
            rails: [
                line([10.0, -10.0, 0.0], [10.0, 10.0, 0.0]),
                arc(
                    [0.0, 0.0, 20.0],
                    6.0,
                    -0.25 * std::f64::consts::PI,
                    0.25 * std::f64::consts::PI,
                ),
            ],
        };
        for (u, v) in [(0.0, 0.0), (0.3, 0.7), (1.0, 1.0), (0.5, 0.5), (0.9, 0.1)] {
            let point = surface.evaluate(Point2::new(u, v));
            let found = surface.invert(point, None).expect("converges");
            assert!(
                (found.x - u).abs() < 1.0e-9 && (found.y - v).abs() < 1.0e-9,
                "{found:?}"
            );
            assert!(surface.distance_to(point, None).expect("converges") < 1.0e-12);
        }
        // A point off the surface finds its foot and reports the gap.
        let above = surface.evaluate(Point2::new(0.4, 0.5))
            + surface.unit_normal(Point2::new(0.4, 0.5)).expect("regular") * 0.25;
        let gap = surface.distance_to(above, None).expect("converges");
        assert!((gap - 0.25).abs() < 1.0e-9, "{gap}");
    }

    #[test]
    fn a_twisted_quadrilateral_measures_in_closed_form() {
        // The saddle z = x·y over the unit square, ruled between two of its
        // skew edges: S = (u, v, uv), N = S_u × S_v = (−v, −u, 1). Measured
        // from one below the origin, (S − p)·N = 1 − uv, whose integral is
        // three quarters; the rule is exact on it.
        let surface = RuledSurface {
            rails: [
                line([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                line([0.0, 1.0, 0.0], [1.0, 1.0, 1.0]),
            ],
        };
        let measures = surface.measures((0.0, 1.0, 0.0, 1.0), Point3::new(0.0, 0.0, -1.0));
        assert!((measures.flux - 0.75).abs() < 1.0e-14, "{}", measures.flux);
        // The area ∫∫√(1 + u² + v²) against a fine midpoint sum.
        let n = 2000;
        let mut expected = 0.0;
        for i in 0..n {
            for j in 0..n {
                let u = (f64::from(i) + 0.5) / f64::from(n);
                let v = (f64::from(j) + 0.5) / f64::from(n);
                expected += u.mul_add(u, v.mul_add(v, 1.0)).sqrt();
            }
        }
        expected /= f64::from(n * n);
        assert!(
            (measures.area - expected).abs() < 1.0e-7,
            "{} {expected}",
            measures.area
        );
    }
}

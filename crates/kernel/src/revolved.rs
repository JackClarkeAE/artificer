//! The surfaces of revolution as one family, for the general Boolean (ADR
//! 0056, Track B1).
//!
//! A cylinder, a cone, a sphere and a torus are each
//! `P(u, v) = origin + ρ(v)·r̂(s·u) + z(v)·axis`, with `r̂` the unit radial
//! direction at an azimuth and `(ρ, z)` the meridian profile in the surface's
//! own `v`. Everything the analytic engine needs from them — the chords an
//! in-matrix intersection curve leaves in their parameter space, a piece of
//! one face re-expressed on another, the crossings of a ray, and the curve a
//! boundary line carries — reduces to that profile, so it is written once
//! here rather than once per class, as the validator's measures already are.
//!
//! Only lines in parameter space arise on these carriers from the published
//! matrix: a ring at a fixed `v` (a latitude circle, a rim, a cone's ring)
//! and a meridian at a fixed `u` (a cone's generator, a sphere's or torus's
//! minor circle). Anything else the matrix names between these classes is a
//! curve the parameter space cannot carry with lines, and refuses here so the
//! numerical rung can take it.

use crate::analytic_extrusion::Segment;
use crate::surface_intersection::IntersectionCurve;
use crate::topology::{
    Cone, Curve3, Cylinder, ParameterRange, Plane, Point2, Point3, Sphere, Surface, Torus, Vector3,
    seam_snapped_sin_cos,
};

/// One of the four carriers of revolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Revolved {
    Cylinder(Cylinder),
    Cone(Cone),
    Sphere(Sphere),
    Torus(Torus),
}

/// The meridian profile at one `v`: the ring radius and height.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Profile {
    pub(crate) rho: f64,
    pub(crate) z: f64,
}

impl Revolved {
    pub(crate) fn of(surface: Surface) -> Option<Self> {
        match surface {
            Surface::Cylinder(cylinder) => Some(Self::Cylinder(cylinder)),
            Surface::Cone(cone) => Some(Self::Cone(cone)),
            Surface::Sphere(sphere) => Some(Self::Sphere(sphere)),
            Surface::Torus(torus) => Some(Self::Torus(torus)),
            Surface::Plane(_) | Surface::Ruled(_) | Surface::Bspline(_) => None,
        }
    }

    /// Whether the surface is a cone, a sphere or a torus: the classes the
    /// engine admitted with Track B1, beyond the planes and cylinders it
    /// carried before.
    pub(crate) fn is_general(surface: Surface) -> bool {
        matches!(
            surface,
            Surface::Cone(_) | Surface::Sphere(_) | Surface::Torus(_)
        )
    }

    pub(crate) fn surface(self) -> Surface {
        match self {
            Self::Cylinder(cylinder) => Surface::Cylinder(cylinder),
            Self::Cone(cone) => Surface::Cone(cone),
            Self::Sphere(sphere) => Surface::Sphere(sphere),
            Self::Torus(torus) => Surface::Torus(torus),
        }
    }

    pub(crate) fn origin(self) -> Point3 {
        match self {
            Self::Cylinder(cylinder) => cylinder.origin,
            Self::Cone(cone) => cone.origin,
            Self::Sphere(sphere) => sphere.origin,
            Self::Torus(torus) => torus.origin,
        }
    }

    /// The unit axis.
    pub(crate) fn axis(self) -> Vector3 {
        let axis = match self {
            Self::Cylinder(cylinder) => cylinder.axis,
            Self::Cone(cone) => cone.axis,
            Self::Sphere(sphere) => sphere.axis,
            Self::Torus(torus) => torus.axis,
        };
        let length = axis.length();
        if length.is_finite() && length > f64::EPSILON {
            axis / length
        } else {
            axis
        }
    }

    pub(crate) fn radial_u(self) -> Vector3 {
        match self {
            Self::Cylinder(cylinder) => cylinder.radial_u,
            Self::Cone(cone) => cone.radial_u,
            Self::Sphere(sphere) => sphere.radial_u,
            Self::Torus(torus) => torus.radial_u,
        }
    }

    pub(crate) fn radial_v(self) -> Vector3 {
        match self {
            Self::Cylinder(cylinder) => cylinder.radial_v,
            Self::Cone(cone) => cone.radial_v,
            Self::Sphere(sphere) => sphere.radial_v,
            Self::Torus(torus) => torus.radial_v,
        }
    }

    pub(crate) fn angular_sign(self) -> f64 {
        match self {
            Self::Cylinder(cylinder) => cylinder.angular_sign,
            Self::Cone(cone) => cone.angular_sign,
            Self::Sphere(sphere) => sphere.angular_sign,
            Self::Torus(torus) => torus.angular_sign,
        }
    }

    /// A length the surface is the size of, for tolerances.
    pub(crate) fn scale(self) -> f64 {
        match self {
            Self::Cylinder(cylinder) => cylinder.radius.abs(),
            Self::Cone(cone) => cone.base_radius.abs(),
            Self::Sphere(sphere) => sphere.radius.abs(),
            Self::Torus(torus) => torus.major_radius.abs() + torus.minor_radius.abs(),
        }
        .max(1.0)
    }

    /// Whether `v` is an angle that wraps: only a torus's minor angle does.
    pub(crate) fn v_periodic(self) -> bool {
        matches!(self, Self::Torus(_))
    }

    pub(crate) fn profile(self, v: f64) -> Profile {
        match self {
            Self::Cylinder(cylinder) => Profile {
                rho: cylinder.radius,
                z: v,
            },
            Self::Cone(cone) => Profile {
                rho: cone.ring_radius(v),
                z: v,
            },
            Self::Sphere(sphere) => {
                let (sin, cos) = seam_snapped_sin_cos(v);
                Profile {
                    rho: sphere.radius * cos,
                    z: sphere.radius * sin,
                }
            }
            Self::Torus(torus) => {
                let (sin, cos) = seam_snapped_sin_cos(v);
                Profile {
                    rho: torus.minor_radius.mul_add(cos, torus.major_radius),
                    z: torus.minor_radius * sin,
                }
            }
        }
    }

    /// The unit radial direction at an azimuth.
    pub(crate) fn radial(self, u: f64) -> Vector3 {
        let (sin, cos) = seam_snapped_sin_cos(self.angular_sign() * u);
        self.radial_u() * cos + self.radial_v() * sin
    }

    pub(crate) fn evaluate(self, point: Point2) -> Point3 {
        self.surface().evaluate(point)
    }

    /// The azimuth on its principal branch and the profile parameter of a
    /// point on the carrier. A point on the axis has no azimuth and takes
    /// zero.
    pub(crate) fn local(self, point: Point3) -> Point2 {
        let axis = self.axis();
        let relative = point - self.origin();
        let height = relative.dot(axis);
        let radial = relative - axis * height;
        let reach = radial.length();
        let azimuth = if reach > 0.0 {
            self.angular_sign()
                * radial
                    .dot(self.radial_v())
                    .atan2(radial.dot(self.radial_u()))
        } else {
            0.0
        };
        let v = match self {
            Self::Cylinder(_) | Self::Cone(_) => height,
            Self::Sphere(_) => height.atan2(reach),
            Self::Torus(torus) => height.atan2(reach - torus.major_radius),
        };
        Point2::new(azimuth, v)
    }

    /// A signed distance-like implicit function and its gradient at a point
    /// near the carrier: zero on it, growing outward.
    pub(crate) fn implicit(self, point: Point3) -> (f64, Vector3) {
        let axis = self.axis();
        let relative = point - self.origin();
        let height = relative.dot(axis);
        let radial = relative - axis * height;
        let reach = radial.length();
        let outward = if reach > 0.0 {
            radial / reach
        } else {
            self.radial_u()
        };
        match self {
            Self::Cylinder(cylinder) => (reach - cylinder.radius, outward),
            Self::Cone(cone) => (
                reach - cone.ring_radius(height),
                outward - axis * cone.slope,
            ),
            Self::Sphere(sphere) => {
                let distance = relative.length();
                if distance > 0.0 {
                    (distance - sphere.radius, relative / distance)
                } else {
                    (-sphere.radius, axis)
                }
            }
            Self::Torus(torus) => {
                let ring = reach - torus.major_radius;
                let tube = ring.hypot(height);
                if tube > 0.0 {
                    (
                        tube - torus.minor_radius,
                        (outward * ring + axis * height) / tube,
                    )
                } else {
                    (-torus.minor_radius, outward)
                }
            }
        }
    }

    /// The `v` at which the carrier's ring has `radius` at `height` above
    /// the origin's plane, if it has one there.
    pub(crate) fn ring_parameter(self, radius: f64, height: f64, tolerance: f64) -> Option<f64> {
        let v = match self {
            Self::Cylinder(cylinder) => {
                if (radius - cylinder.radius).abs() > tolerance {
                    return None;
                }
                height
            }
            Self::Cone(cone) => {
                if (radius - cone.ring_radius(height)).abs() > tolerance {
                    return None;
                }
                height
            }
            Self::Sphere(sphere) => {
                if (radius.hypot(height) - sphere.radius).abs() > tolerance {
                    return None;
                }
                height.atan2(radius)
            }
            Self::Torus(torus) => {
                let ring = radius - torus.major_radius;
                if (ring.hypot(height) - torus.minor_radius).abs() > tolerance {
                    return None;
                }
                height.atan2(ring)
            }
        };
        v.is_finite().then_some(v)
    }
}

// ---------------------------------------------------------------------------
// Chords of in-matrix curves
// ---------------------------------------------------------------------------

/// An in-matrix intersection curve as chords in a cone's, sphere's or torus's
/// parameter space, long enough to cross the face whose parameter box is
/// `(middle, reach)`.
///
/// A ring is a horizontal line offered on every turn a bounded window might
/// use, as a cylinder's ring is; a meridian is a vertical line across the
/// face's `v` range. A circle that is neither — a great circle tilted off the
/// sphere's own axis — has no line for it here and returns `None`.
pub(crate) fn curve_chords(
    revolved: Revolved,
    curve: IntersectionCurve,
    middle: Point2,
    reach: f64,
) -> Option<Vec<Segment>> {
    let tau = std::f64::consts::TAU;
    let tolerance = 1.0e-9 * revolved.scale();
    let axis = revolved.axis();
    match curve {
        IntersectionCurve::Circle {
            center,
            u,
            v,
            radius,
        } => {
            let normal = u.cross(v);
            let normal_length = normal.length();
            if normal_length <= f64::EPSILON {
                return None;
            }
            let normal = normal / normal_length;
            let offset = center - revolved.origin();
            let height = offset.dot(axis);
            let across = offset - axis * height;
            if normal.cross(axis).length() <= 1.0e-9 && across.length() <= tolerance {
                // A ring: constant `v`.
                let level = revolved.ring_parameter(radius, height, tolerance)?;
                return Some(vec![
                    Segment::Line {
                        start: Point2::new(-tau, level),
                        end: Point2::new(0.0, level),
                    },
                    Segment::Line {
                        start: Point2::new(0.0, level),
                        end: Point2::new(tau, level),
                    },
                ]);
            }
            if normal.dot(axis).abs() <= 1.0e-9 {
                // A meridian: the circle's plane contains the axis direction,
                // and its centre sits where this carrier's minor circle does.
                let azimuths = match revolved {
                    Revolved::Sphere(sphere) => {
                        if (radius - sphere.radius).abs() > tolerance || offset.length() > tolerance
                        {
                            return None;
                        }
                        // A great circle through the poles: two meridians,
                        // half a turn apart.
                        let direction = axis.cross(normal);
                        let first = revolved.local(revolved.origin() + direction).x;
                        vec![first, first + std::f64::consts::PI]
                    }
                    Revolved::Torus(torus) => {
                        if (radius - torus.minor_radius).abs() > tolerance
                            || height.abs() > tolerance
                            || (across.length() - torus.major_radius).abs() > tolerance
                        {
                            return None;
                        }
                        vec![revolved.local(center).x]
                    }
                    Revolved::Cylinder(_) | Revolved::Cone(_) => return None,
                };
                let half = 2.0 * reach;
                let mut chords = Vec::new();
                for azimuth in azimuths {
                    for turns in [-1.0_f64, 0.0, 1.0] {
                        let at = turns.mul_add(tau, azimuth);
                        chords.push(Segment::Line {
                            start: Point2::new(at, middle.y - half),
                            end: Point2::new(at, middle.y + half),
                        });
                    }
                }
                return Some(chords);
            }
            None
        }
        IntersectionCurve::Line { origin, direction } => {
            // A generator of a cone: the line runs up the slant at one
            // azimuth.
            let Revolved::Cone(cone) = revolved else {
                return None;
            };
            let length = direction.length();
            if length <= f64::EPSILON {
                return None;
            }
            let direction = direction / length;
            let along = direction.dot(axis);
            let radial = direction - axis * along;
            let sideways = radial.length();
            if along.abs() <= 1.0e-9 || (sideways - cone.slope * along).abs() > 1.0e-9 {
                return None;
            }
            let base = revolved.local(origin);
            if (revolved.evaluate(base) - origin).length() > tolerance {
                return None;
            }
            let half = 2.0 * reach;
            Some(
                [-1.0, 0.0, 1.0]
                    .into_iter()
                    .map(|turns: f64| {
                        let at = turns.mul_add(tau, base.x);
                        Segment::Line {
                            start: Point2::new(at, middle.y - half),
                            end: Point2::new(at, middle.y + half),
                        }
                    })
                    .collect(),
            )
        }
        IntersectionCurve::Ellipse { .. } | IntersectionCurve::Trace(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Re-expressing a piece on another carrier
// ---------------------------------------------------------------------------

/// A boundary piece as the curve it is in space: a straight stretch or an
/// arc of a circle with a right-handed frame.
#[derive(Clone, Copy, Debug)]
enum SpacePiece {
    Line {
        start: Point3,
        end: Point3,
    },
    Arc {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
        start_angle: f64,
        sweep: f64,
    },
}

impl SpacePiece {
    fn point(self, fraction: f64) -> Point3 {
        match self {
            Self::Line { start, end } => start + (end - start) * fraction,
            Self::Arc {
                center,
                u,
                v,
                radius,
                start_angle,
                sweep,
            } => {
                let angle = sweep.mul_add(fraction, start_angle);
                center + u * (radius * angle.cos()) + v * (radius * angle.sin())
            }
        }
    }
}

/// A piece of one face's boundary lifted into space, when it is a line or a
/// circular arc there.
fn lift(from: &Surface, piece: Segment) -> Option<SpacePiece> {
    match (from, piece) {
        (Surface::Plane(plane), Segment::Line { start, end }) => Some(SpacePiece::Line {
            start: plane.evaluate(start),
            end: plane.evaluate(end),
        }),
        (
            Surface::Plane(plane),
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            },
        ) => Some(SpacePiece::Arc {
            center: plane.evaluate(center),
            u: plane.u,
            v: plane.v,
            radius,
            start_angle,
            sweep,
        }),
        (surface, Segment::Line { start, end }) => {
            let revolved = Revolved::of(*surface)?;
            let level =
                (start.y - end.y).abs() <= 1.0e-12 * start.y.abs().max(end.y.abs()).max(1.0);
            let upright =
                (start.x - end.x).abs() <= 1.0e-12 * start.x.abs().max(end.x.abs()).max(1.0);
            if level {
                // A ring at a fixed `v`.
                let profile = revolved.profile(start.y);
                let sign = revolved.angular_sign();
                Some(SpacePiece::Arc {
                    center: revolved.origin() + revolved.axis() * profile.z,
                    u: revolved.radial_u(),
                    v: revolved.radial_v(),
                    radius: profile.rho,
                    start_angle: sign * start.x,
                    sweep: sign * (end.x - start.x),
                })
            } else if upright {
                match revolved {
                    Revolved::Cylinder(_) | Revolved::Cone(_) => Some(SpacePiece::Line {
                        start: revolved.evaluate(start),
                        end: revolved.evaluate(end),
                    }),
                    Revolved::Sphere(sphere) => Some(SpacePiece::Arc {
                        center: sphere.origin,
                        u: revolved.radial(start.x),
                        v: revolved.axis(),
                        radius: sphere.radius,
                        start_angle: start.y,
                        sweep: end.y - start.y,
                    }),
                    Revolved::Torus(torus) => {
                        let radial = revolved.radial(start.x);
                        Some(SpacePiece::Arc {
                            center: torus.origin + radial * torus.major_radius,
                            u: radial,
                            v: revolved.axis(),
                            radius: torus.minor_radius,
                            start_angle: start.y,
                            sweep: end.y - start.y,
                        })
                    }
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A curve in space as a piece in a carrier's parameter space, or `None`
/// where the carrier cannot hold it as a line or an arc.
fn lower(to: &Surface, piece: SpacePiece) -> Option<Segment> {
    match to {
        Surface::Plane(plane) => {
            let local = |point: Point3| plane.project(point);
            match piece {
                SpacePiece::Line { start, end } => Some(Segment::Line {
                    start: local(start),
                    end: local(end),
                }),
                SpacePiece::Arc {
                    center,
                    u,
                    v,
                    radius,
                    sweep,
                    ..
                } => {
                    let normal = u.cross(v);
                    if normal.cross(plane.normal).length() > 1.0e-9 * normal.length() {
                        return None;
                    }
                    let local_center = local(center);
                    let local_start = local(piece.point(0.0));
                    let orientation = normal.dot(plane.normal).signum();
                    Some(Segment::Arc {
                        center: local_center,
                        start: local_start,
                        end: local(piece.point(1.0)),
                        radius,
                        start_angle: (local_start.y - local_center.y)
                            .atan2(local_start.x - local_center.x),
                        sweep: orientation * sweep,
                    })
                }
            }
        }
        surface => {
            let revolved = Revolved::of(*surface)?;
            let axis = revolved.axis();
            let tolerance = 1.0e-9 * revolved.scale();
            match piece {
                SpacePiece::Line { start, end } => {
                    // A straight piece lands on a cylinder or a cone only as
                    // a generator.
                    if !matches!(revolved, Revolved::Cylinder(_) | Revolved::Cone(_)) {
                        return None;
                    }
                    let a = revolved.local(start);
                    let b = revolved.local(end);
                    if (revolved.evaluate(a) - start).length() > tolerance
                        || (revolved.evaluate(b) - end).length() > tolerance
                    {
                        return None;
                    }
                    let bx = nearest_turn(b.x, a.x);
                    if (a.x - bx).abs() > 1.0e-9 {
                        return None;
                    }
                    Some(Segment::Line {
                        start: a,
                        end: Point2::new(a.x, b.y),
                    })
                }
                SpacePiece::Arc {
                    center,
                    u,
                    v,
                    radius,
                    sweep,
                    ..
                } => {
                    let normal = u.cross(v);
                    let offset = center - revolved.origin();
                    let height = offset.dot(axis);
                    let across = offset - axis * height;
                    if normal.cross(axis).length() <= 1.0e-9 * normal.length()
                        && across.length() <= tolerance
                    {
                        // A ring: constant `v`, the azimuth read off the ends
                        // and the middle so a half turn is not mistaken for
                        // its complement.
                        let level = revolved.ring_parameter(radius, height, tolerance)?;
                        let a = revolved.local(piece.point(0.0));
                        let middle = nearest_turn(revolved.local(piece.point(0.5)).x, a.x);
                        let b = revolved.local(piece.point(1.0));
                        let bx = nearest_turn(b.x, a.x + 2.0 * (middle - a.x));
                        return Some(Segment::Line {
                            start: Point2::new(a.x, level),
                            end: Point2::new(bx, level),
                        });
                    }
                    if normal.dot(axis).abs() <= 1.0e-9 * normal.length() {
                        // A meridian on a sphere or a torus: constant `u`.
                        let expected_radius = match revolved {
                            Revolved::Sphere(sphere) => {
                                if offset.length() > tolerance {
                                    return None;
                                }
                                sphere.radius
                            }
                            Revolved::Torus(torus) => {
                                if height.abs() > tolerance
                                    || (across.length() - torus.major_radius).abs() > tolerance
                                {
                                    return None;
                                }
                                torus.minor_radius
                            }
                            Revolved::Cylinder(_) | Revolved::Cone(_) => return None,
                        };
                        if (radius - expected_radius).abs() > tolerance {
                            return None;
                        }
                        let a = revolved.local(piece.point(0.0));
                        let middle = revolved.local(piece.point(0.5));
                        let b = revolved.local(piece.point(1.0));
                        let same = |first: f64, second: f64| {
                            (nearest_turn(second, first) - first).abs() <= 1.0e-9
                        };
                        // An arc that crosses a pole changes meridian; it is
                        // not one line here.
                        if !same(a.x, middle.x) || !same(a.x, b.x) {
                            return None;
                        }
                        // The minor angle's branch: continuous through the
                        // middle.
                        let middle_v = nearest_turn(middle.y, a.y);
                        let bv = nearest_turn(b.y, a.y + 2.0 * (middle_v - a.y));
                        let _ = sweep;
                        return Some(Segment::Line {
                            start: a,
                            end: Point2::new(a.x, bv),
                        });
                    }
                    None
                }
            }
        }
    }
}

fn nearest_turn(value: f64, target: f64) -> f64 {
    let tau = std::f64::consts::TAU;
    value + ((target - value) / tau).round() * tau
}

/// Re-expresses a chord piece from one carrier's parameter space into
/// another's, where at least one of them is a cone, a sphere or a torus.
pub(crate) fn reparameterize(from: &Surface, piece: Segment, to: &Surface) -> Option<Segment> {
    lower(to, lift(from, piece)?)
}

// ---------------------------------------------------------------------------
// The curve a boundary line carries
// ---------------------------------------------------------------------------

/// The 3D curve and parameter range of a line in a cone's, sphere's or
/// torus's parameter space: a ring, a meridian, a cone's generator, or the
/// degenerate line that stands in for a pole.
pub(crate) fn line_curve(
    revolved: Revolved,
    start: Point2,
    end: Point2,
) -> Option<(Curve3, ParameterRange)> {
    let level = (start.y - end.y).abs() <= 1.0e-12 * start.y.abs().max(end.y.abs()).max(1.0);
    let upright = (start.x - end.x).abs() <= 1.0e-12 * start.x.abs().max(end.x.abs()).max(1.0);
    let sign = revolved.angular_sign();
    if level {
        let profile = revolved.profile(start.y);
        if profile.rho.abs() <= 1.0e-9 * revolved.scale() {
            // A pole: every azimuth is the one point, and the edge that
            // stands for it has both ends there, to the bit.
            let pole = revolved.origin() + revolved.axis() * profile.z;
            return Some(Curve3::line_segment([pole, pole]));
        }
        return Some((
            Curve3::Circle {
                center: revolved.origin() + revolved.axis() * profile.z,
                u: revolved.radial_u(),
                v: revolved.radial_v(),
                radius: profile.rho,
            },
            ParameterRange::new(sign * start.x, sign * end.x),
        ));
    }
    if upright {
        return Some(match revolved {
            Revolved::Cylinder(_) | Revolved::Cone(_) => {
                Curve3::line_segment([revolved.evaluate(start), revolved.evaluate(end)])
            }
            Revolved::Sphere(sphere) => (
                Curve3::Circle {
                    center: sphere.origin,
                    u: revolved.radial(start.x),
                    v: revolved.axis(),
                    radius: sphere.radius,
                },
                ParameterRange::new(start.y, end.y),
            ),
            Revolved::Torus(torus) => {
                let radial = revolved.radial(start.x);
                (
                    Curve3::Circle {
                        center: torus.origin + radial * torus.major_radius,
                        u: radial,
                        v: revolved.axis(),
                        radius: torus.minor_radius,
                    },
                    ParameterRange::new(start.y, end.y),
                )
            }
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Ray casting
// ---------------------------------------------------------------------------

/// The parameters at which a ray `point + t·direction` meets the carrier,
/// with the rate of the carrier's implicit polynomial there, rising order.
/// Only forward hits past `guard` are reported.
///
/// A sphere and a cone are quadrics; a torus is a quartic in the ray
/// parameter, whose real roots are isolated by the roots of its derivatives.
pub(crate) fn ray_hits(
    revolved: Revolved,
    point: Point3,
    direction: Vector3,
    guard: f64,
) -> Option<Vec<f64>> {
    let axis = revolved.axis();
    let relative = point - revolved.origin();
    let (h0, h1) = (relative.dot(axis), direction.dot(axis));
    let (q0, q1, q2) = (
        relative.dot(relative),
        2.0 * relative.dot(direction),
        direction.dot(direction),
    );
    // |radial|² = |w|² − h² as a polynomial in t.
    let radial_square = [q0 - h0 * h0, q1 - 2.0 * h0 * h1, q2 - h1 * h1];
    let polynomial: Vec<f64> = match revolved {
        Revolved::Cylinder(cylinder) => vec![
            radial_square[0] - cylinder.radius * cylinder.radius,
            radial_square[1],
            radial_square[2],
        ],
        Revolved::Sphere(sphere) => vec![q0 - sphere.radius * sphere.radius, q1, q2],
        Revolved::Cone(cone) => {
            // |radial|² − (b + m·h)², with h = h0 + h1·t.
            let ring = [cone.base_radius + cone.slope * h0, cone.slope * h1];
            vec![
                radial_square[0] - ring[0] * ring[0],
                radial_square[1] - 2.0 * ring[0] * ring[1],
                radial_square[2] - ring[1] * ring[1],
            ]
        }
        Revolved::Torus(torus) => {
            // (|w|² + R² − r²)² − 4R²·|radial|².
            let major = torus.major_radius;
            let minor = torus.minor_radius;
            let a = [q0 + major * major - minor * minor, q1, q2];
            let square = multiply(&a, &a);
            let mut quartic = square;
            for (index, term) in radial_square.iter().enumerate() {
                quartic[index] -= 4.0 * major * major * term;
            }
            quartic
        }
    };
    let scale = polynomial
        .iter()
        .fold(0.0_f64, |scale, coefficient| scale.max(coefficient.abs()));
    if !scale.is_finite() || scale == 0.0 {
        return None;
    }
    let mut hits = Vec::new();
    for root in real_roots(&polynomial) {
        if root <= guard {
            continue;
        }
        // A grazing hit — a double root — cannot be counted: the ray touches
        // the carrier and the parity does not change. Another direction is
        // asked instead.
        let rate = derivative_at(&polynomial, root);
        if rate.abs() <= 1.0e-9 * scale {
            return None;
        }
        hits.push(root);
    }
    Some(hits)
}

fn multiply(first: &[f64], second: &[f64]) -> Vec<f64> {
    let mut product = vec![0.0; first.len() + second.len() - 1];
    for (i, a) in first.iter().enumerate() {
        for (j, b) in second.iter().enumerate() {
            product[i + j] = a.mul_add(*b, product[i + j]);
        }
    }
    product
}

fn evaluate_at(polynomial: &[f64], t: f64) -> f64 {
    polynomial
        .iter()
        .rev()
        .fold(0.0, |total, coefficient| total.mul_add(t, *coefficient))
}

fn derivative(polynomial: &[f64]) -> Vec<f64> {
    polynomial
        .iter()
        .enumerate()
        .skip(1)
        .map(|(power, coefficient)| coefficient * power as f64)
        .collect()
}

fn derivative_at(polynomial: &[f64], t: f64) -> f64 {
    evaluate_at(&derivative(polynomial), t)
}

/// Every real root of a polynomial with rising coefficients, in rising
/// order, by isolating them between the roots of the derivative and
/// bisecting. Deterministic, and sound for any degree the caller reaches.
pub(crate) fn real_roots(polynomial: &[f64]) -> Vec<f64> {
    // Drop vanishing leading coefficients.
    let mut trimmed = polynomial.to_vec();
    let scale = trimmed
        .iter()
        .fold(0.0_f64, |scale, coefficient| scale.max(coefficient.abs()));
    while trimmed
        .last()
        .is_some_and(|leading| leading.abs() <= 1.0e-14 * scale)
        && trimmed.len() > 1
    {
        trimmed.pop();
    }
    let degree = trimmed.len().saturating_sub(1);
    if degree == 0 {
        return Vec::new();
    }
    let leading = trimmed[degree];
    if degree == 1 {
        return vec![-trimmed[0] / leading];
    }
    // Cauchy's bound: every root lies within 1 + max|aᵢ/aₙ|.
    let bound = 1.0
        + trimmed[..degree]
            .iter()
            .fold(0.0_f64, |bound, coefficient| {
                bound.max((coefficient / leading).abs())
            });
    let mut stations = vec![-bound];
    stations.extend(real_roots(&derivative(&trimmed)));
    stations.push(bound);
    let mut roots = Vec::new();
    let value = |t: f64| evaluate_at(&trimmed, t);
    for pair in stations.windows(2) {
        let (mut low, mut high) = (pair[0], pair[1]);
        if high <= low {
            continue;
        }
        let (low_value, high_value) = (value(low), value(high));
        if low_value == 0.0 {
            roots.push(low);
            continue;
        }
        if (low_value < 0.0) == (high_value < 0.0) {
            // A double root at a station: the polynomial touches zero there.
            if high_value == 0.0 || high_value.abs() <= 1.0e-13 * scale * bound.powi(degree as i32)
            {
                roots.push(high);
            }
            continue;
        }
        for _ in 0..200 {
            let middle = 0.5 * (low + high);
            if middle <= low || middle >= high {
                break;
            }
            if (value(middle) < 0.0) == (low_value < 0.0) {
                low = middle;
            } else {
                high = middle;
            }
        }
        roots.push(0.5 * (low + high));
    }
    roots.sort_by(f64::total_cmp);
    roots.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-12 * bound);
    roots
}

/// The number of times a ray crosses one face of a cone, a sphere or a torus,
/// or `None` where a hit is too near the face's boundary or grazes the
/// carrier. `inside` answers whether a parameter point lies in the face's
/// region, or `None` where it is too near the boundary to say.
pub(crate) fn ray_face_crossings(
    revolved: Revolved,
    point: Point3,
    direction: Vector3,
    guard: f64,
    inside: &dyn Fn(Point2) -> Option<usize>,
) -> Option<usize> {
    let tau = std::f64::consts::TAU;
    let mut crossings = 0;
    for t in ray_hits(revolved, point, direction, guard)? {
        let hit = point + direction * t;
        let local = revolved.local(hit);
        // The face's window may sit on any whole turn of the azimuth, and
        // of a torus's minor angle too.
        let v_turns: &[f64] = if revolved.v_periodic() {
            &[-1.0, 0.0, 1.0]
        } else {
            &[0.0]
        };
        let mut counted = false;
        'branches: for u_turn in [-1.0, 0.0, 1.0] {
            for v_turn in v_turns {
                let candidate =
                    Point2::new(tau.mul_add(u_turn, local.x), tau.mul_add(*v_turn, local.y));
                match inside(candidate) {
                    Some(1) => {
                        counted = true;
                        break 'branches;
                    }
                    Some(_) => {}
                    None => return None,
                }
            }
        }
        if counted {
            crossings += 1;
        }
    }
    Some(crossings)
}

/// A box in model space a face on one of these carriers cannot leave: the
/// whole drum, ball or ring the carrier occupies over the face's `v` range.
pub(crate) fn extent(revolved: Revolved, v_low: f64, v_high: f64) -> Option<(Point3, Point3)> {
    let axis = revolved.axis();
    let reach = |radius: f64| {
        let (u, v) = (revolved.radial_u(), revolved.radial_v());
        Vector3::new(
            radius * u.x.hypot(v.x),
            radius * u.y.hypot(v.y),
            radius * u.z.hypot(v.z),
        )
    };
    let mut min = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut max = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut grow = |on_axis: Point3, radius: f64| {
        let across = reach(radius);
        min = Point3::new(
            min.x.min(on_axis.x - across.x),
            min.y.min(on_axis.y - across.y),
            min.z.min(on_axis.z - across.z),
        );
        max = Point3::new(
            max.x.max(on_axis.x + across.x),
            max.y.max(on_axis.y + across.y),
            max.z.max(on_axis.z + across.z),
        );
    };
    match revolved {
        Revolved::Cylinder(cylinder) => {
            for height in [v_low, v_high] {
                grow(cylinder.origin + axis * height, cylinder.radius.abs());
            }
        }
        Revolved::Cone(cone) => {
            for height in [v_low, v_high] {
                grow(cone.origin + axis * height, cone.ring_radius(height).abs());
            }
        }
        Revolved::Sphere(sphere) => {
            // The ball: the latitude band's reach along the axis and across
            // it, taken whole for simplicity.
            for sign in [-1.0, 1.0] {
                grow(sphere.origin + axis * (sign * sphere.radius), sphere.radius);
            }
        }
        Revolved::Torus(torus) => {
            let outer = torus.major_radius + torus.minor_radius;
            for sign in [-1.0, 1.0] {
                grow(torus.origin + axis * (sign * torus.minor_radius), outer);
            }
        }
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

/// The same carrier facing the other way: the azimuth's sense reversed,
/// which is the convention every reversal in the kernel keeps.
pub(crate) fn reversed_surface(surface: Surface) -> Option<Surface> {
    Some(match surface {
        Surface::Cone(cone) => Surface::Cone(Cone {
            angular_sign: -cone.angular_sign,
            ..cone
        }),
        Surface::Sphere(sphere) => Surface::Sphere(Sphere {
            angular_sign: -sphere.angular_sign,
            ..sphere
        }),
        Surface::Torus(torus) => Surface::Torus(Torus {
            angular_sign: -torus.angular_sign,
            ..torus
        }),
        Surface::Cylinder(cylinder) => Surface::Cylinder(Cylinder {
            angular_sign: -cylinder.angular_sign,
            ..cylinder
        }),
        Surface::Plane(plane) => Surface::Plane(Plane::new(plane.origin, plane.v, plane.u)),
        Surface::Ruled(_) | Surface::Bspline(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn torus() -> Revolved {
        Revolved::Torus(Torus {
            origin: Point3::new(1.0, 2.0, 3.0),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            major_radius: 6.0,
            minor_radius: 2.0,
            angular_sign: 1.0,
        })
    }

    #[test]
    fn a_polynomial_gives_back_its_roots() {
        // (t − 1)(t + 2)(t − 3.5)(t − 0.25)
        let factors = [[-1.0, 1.0], [2.0, 1.0], [-3.5, 1.0], [-0.25, 1.0]];
        let mut polynomial = vec![1.0];
        for factor in factors {
            polynomial = multiply(&polynomial, &factor);
        }
        let roots = real_roots(&polynomial);
        let expected = [-2.0, 0.25, 1.0, 3.5];
        assert_eq!(roots.len(), 4, "{roots:?}");
        for (found, wanted) in roots.iter().zip(expected) {
            assert!((found - wanted).abs() < 1.0e-12, "{found} is not {wanted}");
        }
    }

    #[test]
    fn local_inverts_evaluate_on_every_carrier() {
        let torus = torus();
        for (u, v) in [(0.3, 0.7), (2.5, -1.2), (-1.0, 2.9)] {
            let point = torus.evaluate(Point2::new(u, v));
            let back = torus.local(point);
            assert!((back.x - u).abs() < 1.0e-12 && (back.y - v).abs() < 1.0e-12);
            let (distance, _) = torus.implicit(point);
            assert!(distance.abs() < 1.0e-12);
        }
    }

    #[test]
    fn a_ray_through_a_torus_meets_it_four_times() {
        let torus = torus();
        let hits = ray_hits(
            torus,
            Point3::new(-20.0, 2.0, 3.0),
            Vector3::new(1.0, 0.0, 0.0),
            1.0e-9,
        )
        .expect("a transverse ray");
        assert_eq!(hits.len(), 4, "{hits:?}");
        for t in hits {
            let point = Point3::new(-20.0 + t, 2.0, 3.0);
            assert!(torus.implicit(point).0.abs() < 1.0e-9);
        }
    }
}

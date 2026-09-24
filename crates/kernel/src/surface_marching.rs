//! Numerical surface–surface intersection (ADR 0056, Track B2–B4 as one
//! route).
//!
//! Where the published matrix has no closed form for the curve two carriers
//! share — a bore through a torus off its axis, a sphere cut by an off-centre
//! bore, a cone cut by an oblique plane, two tori crossing — the curve is
//! traced numerically: start points are found by projecting a grid of one
//! surface onto the other, the curve is marched along `n₁ × n₂` with a
//! Newton corrector and step control, closed loops and separate branches are
//! told apart, and the marched points are interpolated by a B-spline in space
//! and by one in each carrier's parameter space.
//!
//! Nothing here is presented as exact. The fit's departure from both carriers
//! is measured after every refinement and reported with the result, and the
//! points are refined until the three descriptions of the curve — the space
//! curve and the two pcurves read through their surfaces — agree to
//! [`INTERSECTION_TOLERANCE`], which is inside what the solid validator's
//! locus proof demands of every edge. A curve that cannot be brought within
//! it is refused, never published.

use crate::bspline::{SplineCurve2, SplineCurve3, array2, array3, interpolation_knots};
use crate::revolved::Revolved;
use crate::topology::{Point2, Point3, Surface, Vector2, Vector3};

/// The agreement the fitted curve is brought to, in model units: a tenth of
/// the validator's linear tolerance, so every sample the locus proof takes
/// of the fitted edge lands inside it.
pub(crate) const INTERSECTION_TOLERANCE: f64 = 5.0e-10;

/// The most marched points one curve is allowed before the fit is given up.
const MOST_POINTS: usize = 16_384;

/// Why a pair could not be traced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TraceError {
    /// A carrier the tracer has no oracle for.
    Unsupported,
    /// The curve could not be fitted within [`INTERSECTION_TOLERANCE`]
    /// with the points allowed.
    ToleranceUnmet,
}

/// One carrier's side of a traced curve.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TracedSide {
    pub(crate) carrier: Surface,
    /// The curve in this carrier's parameter space, over the same parameter
    /// as the space curve. On a periodic carrier the azimuth runs on
    /// continuously and may leave `[0, 2π)`.
    pub(crate) pcurve: SplineCurve2,
}

/// The curve two carriers share, traced and fitted.
#[derive(Clone, Debug)]
pub(crate) struct TracedCurve {
    /// Over `[0, 1]`, by normalised chord length.
    pub(crate) curve: SplineCurve3,
    pub(crate) sides: [TracedSide; 2],
    /// Whether the curve closes on itself: its two ends are one point.
    pub(crate) closed: bool,
    /// The worst departure measured between any two of the three
    /// descriptions, and from either carrier.
    pub(crate) deviation: f64,
}

/// A parameter window a traced curve is kept inside: the parameter box of
/// the faces on one carrier, widened a little. An azimuth window wider than
/// a turn is every azimuth.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Window {
    pub(crate) u: (f64, f64),
    pub(crate) v: (f64, f64),
    pub(crate) u_periodic: bool,
    pub(crate) v_periodic: bool,
}

impl Window {
    fn contains(&self, point: Point2) -> bool {
        let inside = |value: f64, (low, high): (f64, f64), periodic: bool| {
            if !periodic {
                return value >= low && value <= high;
            }
            let tau = std::f64::consts::TAU;
            if high - low >= tau {
                return true;
            }
            let middle = 0.5 * (low + high);
            let brought = value + ((middle - value) / tau).round() * tau;
            brought >= low && brought <= high
        };
        inside(point.x, self.u, self.u_periodic) && inside(point.y, self.v, self.v_periodic)
    }
}

/// What the tracer asks of a carrier: a signed distance and its gradient, the
/// parameters of a point on it, and the point at parameters.
#[derive(Clone, Copy, Debug)]
struct Oracle {
    surface: Surface,
}

impl Oracle {
    fn new(surface: Surface) -> Option<Self> {
        match surface {
            Surface::Plane(_)
            | Surface::Cylinder(_)
            | Surface::Cone(_)
            | Surface::Sphere(_)
            | Surface::Torus(_) => Some(Self { surface }),
            Surface::Ruled(_) | Surface::Bspline(_) => None,
        }
    }

    /// A signed distance to the carrier (exact for a plane, a cylinder, a
    /// sphere and a torus; proportional for a cone) and its gradient.
    fn distance(&self, point: Point3) -> (f64, Vector3) {
        match self.surface {
            Surface::Plane(plane) => {
                let normal = plane.normal / plane.normal.length();
                ((point - plane.origin).dot(normal), normal)
            }
            _ => Revolved::of(self.surface)
                .map(|revolved| revolved.implicit(point))
                .unwrap_or((f64::INFINITY, Vector3::new(0.0, 0.0, 1.0))),
        }
    }

    fn local(&self, point: Point3) -> Point2 {
        match self.surface {
            Surface::Plane(plane) => plane.project(point),
            _ => Revolved::of(self.surface)
                .map(|revolved| revolved.local(point))
                .unwrap_or(Point2::new(0.0, 0.0)),
        }
    }

    fn evaluate(&self, parameters: Point2) -> Point3 {
        self.surface.evaluate(parameters)
    }

    fn u_periodic(&self) -> bool {
        Revolved::of(self.surface).is_some()
    }

    fn v_periodic(&self) -> bool {
        Revolved::of(self.surface).is_some_and(Revolved::v_periodic)
    }

    /// The smallest curvature radius the carrier has inside a window, for
    /// step control and sagitta bounds. A cone's is its narrowest ring in
    /// the window (its curvature radius along a ring is the ring's radius
    /// times the slant factor, so the ring radius alone is the lower bound),
    /// and zero where the window holds the apex.
    fn least_radius(&self, window: Window) -> f64 {
        match self.surface {
            Surface::Plane(_) => f64::INFINITY,
            Surface::Cylinder(cylinder) => cylinder.radius.abs(),
            Surface::Cone(cone) => {
                let (low, high) = (cone.ring_radius(window.v.0), cone.ring_radius(window.v.1));
                if low.signum() != high.signum() {
                    0.0
                } else {
                    low.abs().min(high.abs())
                }
            }
            Surface::Sphere(sphere) => sphere.radius.abs(),
            Surface::Torus(torus) => torus.minor_radius.abs(),
            Surface::Ruled(_) | Surface::Bspline(_) => 1.0,
        }
    }
}

/// Newton's method onto the curve both carriers share, from a point near
/// it: the minimum-norm step of the two implicit equations. `None` where the
/// gradients are parallel — a tangency — or the iteration does not settle.
fn project(a: &Oracle, b: &Oracle, start: Point3, scale: f64) -> Option<Point3> {
    let mut point = start;
    let settle = 1.0e-14 * scale;
    for _ in 0..40 {
        let (fa, ga) = a.distance(point);
        let (fb, gb) = b.distance(point);
        let (aa, ab, bb) = (ga.dot(ga), ga.dot(gb), gb.dot(gb));
        let determinant = aa.mul_add(bb, -(ab * ab));
        if !determinant.is_finite() || determinant <= 1.0e-10 * aa * bb {
            return None;
        }
        let la = (-fa).mul_add(bb, fb * ab) / determinant;
        let lb = (-fb).mul_add(aa, fa * ab) / determinant;
        let step = ga * la + gb * lb;
        point = point + step;
        if step.length() <= settle {
            break;
        }
    }
    let (fa, _) = a.distance(point);
    let (fb, _) = b.distance(point);
    (fa.abs() <= 1.0e-11 * scale && fb.abs() <= 1.0e-11 * scale && point.is_finite())
        .then_some(point)
}

/// The unit tangent of the curve at a point on it, or `None` at a tangency.
fn tangent(a: &Oracle, b: &Oracle, point: Point3) -> Option<Vector3> {
    let (_, ga) = a.distance(point);
    let (_, gb) = b.distance(point);
    let cross = ga.cross(gb);
    let length = cross.length();
    (length > 1.0e-6 * ga.length() * gb.length()).then(|| cross / length)
}

/// Where one marched branch stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// Back at its start: a closed loop.
    Closed,
    /// Out of both faces' windows.
    Left,
    /// A tangency, or the step floor: the branch ends inside the windows.
    Stalled,
}

/// Marches from `start` in the direction of `direction` until the curve
/// closes, leaves both windows, or stalls.
fn march(
    a: &Oracle,
    b: &Oracle,
    windows: [Window; 2],
    start: Point3,
    direction: f64,
    scale: f64,
) -> (Vec<Point3>, Stop) {
    let mut points = vec![start];
    // The tightest bend either carrier has bounds the step; a carrier with
    // an apex in the window bounds nothing, and the turn control below
    // keeps the step honest there.
    let least_radius = a
        .least_radius(windows[0])
        .min(b.least_radius(windows[1]))
        .max(scale * 1.0e-2);
    let largest = (scale / 32.0).min(least_radius / 4.0).max(scale * 1.0e-4);
    let floor = scale * 1.0e-7;
    let mut step = largest / 4.0;
    let mut point = start;
    let Some(mut heading) = tangent(a, b, point) else {
        return (points, Stop::Stalled);
    };
    heading = heading * direction;
    let mut travelled = 0.0;
    let mut stop = Stop::Stalled;
    let inside =
        |point: Point3| windows[0].contains(a.local(point)) && windows[1].contains(b.local(point));
    while points.len() < MOST_POINTS {
        let predicted = point + heading * step;
        let Some(next) = project(a, b, predicted, scale) else {
            if step <= floor {
                break;
            }
            step *= 0.5;
            continue;
        };
        let Some(ahead) = tangent(a, b, next) else {
            break;
        };
        let ahead = if ahead.dot(heading) < 0.0 {
            ahead * -1.0
        } else {
            ahead
        };
        let turn = heading.dot(ahead).clamp(-1.0, 1.0).acos();
        let moved = (next - point).length();
        if (turn > 0.15 || moved > 2.0 * step || (next - predicted).length() > 0.5 * step)
            && step > floor
        {
            step *= 0.5;
            continue;
        }
        points.push(next);
        travelled += moved;
        point = next;
        heading = ahead;
        if turn < 0.03 {
            step = (step * 1.5).min(largest);
        }
        // Closed when back within a step of the start after going round.
        if travelled > 4.0 * largest && (point - start).length() <= step {
            points.push(start);
            stop = Stop::Closed;
            break;
        }
        if !inside(point) {
            stop = Stop::Left;
            break;
        }
    }
    (points, stop)
}

/// Grid parameters over a window, for start points.
fn grid(window: Window, steps: usize) -> Vec<Point2> {
    let mut samples = Vec::with_capacity((steps + 1) * (steps + 1));
    for i in 0..=steps {
        let u = (window.u.1 - window.u.0).mul_add(i as f64 / steps as f64, window.u.0);
        for j in 0..=steps {
            let v = (window.v.1 - window.v.0).mul_add(j as f64 / steps as f64, window.v.0);
            samples.push(Point2::new(u, v));
        }
    }
    samples
}

/// The distance from a point to a polyline.
fn polyline_distance(point: Point3, polyline: &[Point3]) -> f64 {
    let mut least = f64::INFINITY;
    for pair in polyline.windows(2) {
        let direction = pair[1] - pair[0];
        let square = direction.dot(direction);
        let along = if square > 0.0 {
            ((point - pair[0]).dot(direction) / square).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let foot = pair[0] + direction * along;
        least = least.min((point - foot).length());
    }
    if let Some(only) = polyline.first().filter(|_| polyline.len() == 1) {
        least = least.min((point - *only).length());
    }
    least
}

/// Traces every branch of the curve two carriers share inside the two
/// windows, fitted and measured. An empty answer means the tracer found no
/// crossing; `Unresolved` means it found the surfaces within reach of one
/// another but no curve, which is refused rather than read as apart.
pub(crate) fn trace_carriers(
    first: Surface,
    first_window: Window,
    second: Surface,
    second_window: Window,
) -> Result<Vec<TracedCurve>, TraceError> {
    let a = Oracle::new(first).ok_or(TraceError::Unsupported)?;
    let b = Oracle::new(second).ok_or(TraceError::Unsupported)?;
    let windows = [first_window, second_window];
    let scale = window_scale(&a, first_window)
        .max(window_scale(&b, second_window))
        .max(1.0);
    // Every grid point of either window, projected onto the curve.
    let mut starts: Vec<Point3> = Vec::new();
    for (oracle, window) in [(&a, first_window), (&b, second_window)] {
        for parameters in grid(window, 24) {
            let seed = oracle.evaluate(parameters);
            if let Some(point) = project(&a, &b, seed, scale) {
                starts.push(point);
            }
        }
    }
    let mut curves: Vec<TracedCurve> = Vec::new();
    let mut polylines: Vec<(Vec<Point3>, f64)> = Vec::new();
    for start in starts {
        if !windows[0].contains(a.local(start)) || !windows[1].contains(b.local(start)) {
            continue;
        }
        if polylines
            .iter()
            .any(|(polyline, reach)| polyline_distance(start, polyline) <= *reach)
        {
            continue;
        }
        let (forward, stop) = march(&a, &b, windows, start, 1.0, scale);
        let (points, closed) = match stop {
            Stop::Closed => (forward, true),
            Stop::Left | Stop::Stalled => {
                let (backward, _) = march(&a, &b, windows, start, -1.0, scale);
                let mut points: Vec<Point3> = backward.into_iter().skip(1).rev().collect();
                points.extend(forward);
                (points, false)
            }
        };
        if points.len() < 3 {
            continue;
        }
        let reach = points
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).length())
            .fold(0.0_f64, f64::max)
            * 1.5;
        polylines.push((points.clone(), reach));
        if let Some(curve) = fit(&a, &b, points, closed, scale)? {
            curves.push(curve);
        }
    }
    Ok(curves)
}

fn window_scale(oracle: &Oracle, window: Window) -> f64 {
    let corners = [
        Point2::new(window.u.0, window.v.0),
        Point2::new(window.u.1, window.v.0),
        Point2::new(window.u.0, window.v.1),
        Point2::new(window.u.1, window.v.1),
    ]
    .map(|corner| oracle.evaluate(corner));
    let mut scale = 0.0_f64;
    for corner in &corners {
        for other in &corners {
            scale = scale.max((*corner - *other).length());
        }
    }
    scale.max(oracle.least_radius(window).min(1.0e3))
}

/// The parameters of the marched points on one carrier, continuous across
/// the seam: each azimuth brought within half a turn of the one before.
fn unwrapped(oracle: &Oracle, points: &[Point3]) -> Vec<[f64; 2]> {
    let tau = std::f64::consts::TAU;
    let mut previous: Option<Point2> = None;
    points
        .iter()
        .map(|point| {
            let mut local = oracle.local(*point);
            if let Some(last) = previous {
                if oracle.u_periodic() {
                    local.x += ((last.x - local.x) / tau).round() * tau;
                }
                if oracle.v_periodic() {
                    local.y += ((last.y - local.y) / tau).round() * tau;
                }
            }
            previous = Some(local);
            array2(local)
        })
        .collect()
}

/// Interpolates the curve through samples spaced evenly by chord length
/// along the marched points, doubling the sampling until the three
/// descriptions agree to the tolerance.
///
/// Even spacing is what keeps the interpolant's rate converging: a quintic
/// through points spaced unevenly by refinement keeps the error of its
/// coarsest neighbours, however finely the rest is cut.
fn fit(
    a: &Oracle,
    b: &Oracle,
    points: Vec<Point3>,
    closed: bool,
    scale: f64,
) -> Result<Option<TracedCurve>, TraceError> {
    // The sampling starts coarse whatever the march took: the marched
    // points only lay out the curve, and the fit's own doubling finds the
    // count it needs. Too fine a start would not converge better — the
    // rate of a spline with very short spans amplifies the rounding of its
    // control points — so the first count under tolerance is the one kept.
    let mut count = 32;
    while count <= MOST_POINTS {
        let samples = resample(a, b, &points, count, scale);
        let Some(candidate) = interpolate_all(a, b, &samples, closed) else {
            return Err(TraceError::ToleranceUnmet);
        };
        let deviation = measure(a, b, &candidate);
        if deviation <= INTERSECTION_TOLERANCE {
            return Ok(Some(TracedCurve {
                deviation,
                ..candidate
            }));
        }
        count *= 2;
    }
    Err(TraceError::ToleranceUnmet)
}

/// `count + 1` points of the curve at even chord-length steps along the
/// marched polyline, each projected back onto the curve; the two ends are
/// the polyline's own.
fn resample(a: &Oracle, b: &Oracle, points: &[Point3], count: usize, scale: f64) -> Vec<Point3> {
    let mut lengths = Vec::with_capacity(points.len());
    let mut running = 0.0;
    lengths.push(0.0);
    for pair in points.windows(2) {
        running += (pair[1] - pair[0]).length();
        lengths.push(running);
    }
    let total = running;
    let mut samples = Vec::with_capacity(count + 1);
    samples.push(points[0]);
    let mut cursor = 0;
    for step in 1..count {
        let target = total * step as f64 / count as f64;
        while cursor + 2 < lengths.len() && lengths[cursor + 1] < target {
            cursor += 1;
        }
        let span = lengths[cursor + 1] - lengths[cursor];
        let fraction = if span > 0.0 {
            ((target - lengths[cursor]) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let guess = points[cursor] + (points[cursor + 1] - points[cursor]) * fraction;
        samples.push(project(a, b, guess, scale).unwrap_or(guess));
    }
    samples.push(points[points.len() - 1]);
    samples
}

/// The chord-length parameters of a point list, on `[0, 1]`.
fn chord_parameters(points: &[Point3]) -> Option<Vec<f64>> {
    let mut parameters = Vec::with_capacity(points.len());
    let mut running = 0.0;
    parameters.push(0.0);
    for pair in points.windows(2) {
        let chord = (pair[1] - pair[0]).length();
        // A repeated point, or a NaN, has no chord to parameterise by.
        if chord.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        running += chord;
        parameters.push(running);
    }
    if running.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    for parameter in &mut parameters {
        *parameter /= running;
    }
    let last = parameters.len() - 1;
    parameters[last] = 1.0;
    Some(parameters)
}

fn interpolate_all(a: &Oracle, b: &Oracle, points: &[Point3], closed: bool) -> Option<TracedCurve> {
    let parameters = chord_parameters(points)?;
    let degree = (points.len() - 1).min(5);
    let knots = interpolation_knots(&parameters, degree);
    let data: Vec<[f64; 3]> = points.iter().map(|point| array3(*point)).collect();
    let curve = crate::bspline::interpolating_curve(degree, &knots, &parameters, &data)?;
    let side = |oracle: &Oracle| -> Option<TracedSide> {
        let pcurve = match oracle.surface {
            // On a plane the pcurve is the space curve itself, projected:
            // the same B-spline, which the validator's identity proof
            // recognises control point for control point.
            Surface::Plane(plane) => crate::bspline::plane_pcurve(curve, plane)?,
            _ => {
                let traced = unwrapped(oracle, points);
                crate::bspline::interpolating_curve(degree, &knots, &parameters, &traced)?
            }
        };
        Some(TracedSide {
            carrier: oracle.surface,
            pcurve,
        })
    };
    Some(TracedCurve {
        curve,
        sides: [side(a)?, side(b)?],
        closed,
        deviation: f64::INFINITY,
    })
}

/// The worst disagreement between the three descriptions of the curve and
/// the two carriers, sampled on every knot span in position and in rate: the
/// very comparison the validator's locus proof makes of an edge, with the
/// rate taken over the whole curve, which is the widest range any piece of
/// it can be walked over.
fn measure(a: &Oracle, b: &Oracle, traced: &TracedCurve) -> f64 {
    let probe = |t: f64| -> f64 {
        let point = traced.curve.point(t);
        let rate = traced.curve.tangent(t);
        let mut error = 0.0_f64;
        for (oracle, side) in [(a, &traced.sides[0]), (b, &traced.sides[1])] {
            let local = side.pcurve.point(t);
            let local_rate = side.pcurve.tangent(t);
            let on_surface = oracle.evaluate(local);
            error = error.max((on_surface - point).length());
            error = error.max(oracle.distance(point).0.abs());
            let mapped = oracle
                .surface
                .map_tangent(local, Vector2::new(local_rate.x, local_rate.y));
            error = error.max((mapped - rate).length());
        }
        error
    };
    let mut overall = 0.0_f64;
    for (low, high) in traced.curve.spans(0.0, 1.0) {
        for fraction in [0.0, 0.25, 0.5, 0.75] {
            overall = overall.max(probe((high - low).mul_add(fraction, low)));
        }
    }
    overall.max(probe(1.0))
}

/// Whether two bounded patches are certainly apart: every sample of one lies
/// farther from the other's carrier than the sampling can hide.
///
/// A patch is sampled on a grid over its window; between neighbouring
/// samples the surface departs from the chord by at most the sagitta its
/// least curvature radius allows, so every point of the patch is within
/// half the sample spacing plus that sagitta of a sample. If every sample
/// is farther than that from the other carrier, no point of the patch
/// touches it, and the pair cannot meet. Either patch may show it.
pub(crate) fn patches_apart(
    first: Surface,
    first_window: Window,
    second: Surface,
    second_window: Window,
) -> bool {
    let (Some(a), Some(b)) = (Oracle::new(first), Oracle::new(second)) else {
        return false;
    };
    let apart = |patch: &Oracle, window: Window, carrier: &Oracle| -> bool {
        const STEPS: usize = 32;
        let samples = grid(window, STEPS);
        let mut spacing = 0.0_f64;
        for i in 0..=STEPS {
            for j in 0..=STEPS {
                let here = patch.evaluate(samples[i * (STEPS + 1) + j]);
                if i < STEPS {
                    let next = patch.evaluate(samples[(i + 1) * (STEPS + 1) + j]);
                    spacing = spacing.max((next - here).length());
                }
                if j < STEPS {
                    let next = patch.evaluate(samples[i * (STEPS + 1) + j + 1]);
                    spacing = spacing.max((next - here).length());
                }
            }
        }
        let diagonal = spacing * std::f64::consts::SQRT_2;
        let sagitta = diagonal * diagonal / (8.0 * patch.least_radius(window));
        let cover = 0.5 * diagonal + sagitta + 1.0e-6;
        samples.iter().all(|parameters| {
            let point = patch.evaluate(*parameters);
            carrier_distance(carrier, point) > cover
        })
    };
    apart(&a, first_window, &b) || apart(&b, second_window, &a)
}

/// A lower bound on the distance from a point to a carrier: the implicit
/// value, scaled down for a cone whose implicit is measured across rather
/// than perpendicular to its wall.
fn carrier_distance(oracle: &Oracle, point: Point3) -> f64 {
    let (distance, _) = oracle.distance(point);
    match oracle.surface {
        Surface::Cone(cone) => distance.abs() / cone.slope.mul_add(cone.slope, 1.0).sqrt(),
        _ => distance.abs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{Cylinder, Sphere, Torus};

    fn upright_cylinder(x: f64, radius: f64) -> Surface {
        Surface::Cylinder(Cylinder {
            origin: Point3::new(x, 0.0, 0.0),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            radius,
            angular_sign: 1.0,
        })
    }

    fn window(u: (f64, f64), v: (f64, f64), periodic: bool) -> Window {
        Window {
            u,
            v,
            u_periodic: periodic,
            v_periodic: false,
        }
    }

    /// A sphere of radius 5 cut by a bore of radius 1.5 whose axis passes 2.5
    /// from the centre: two closed loops, one on each side of the equator,
    /// each fitted to within the tolerance.
    #[test]
    fn an_off_centre_bore_through_a_sphere_traces_two_closed_loops() {
        let sphere = Surface::Sphere(Sphere {
            origin: Point3::new(0.0, 0.0, 0.0),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            radius: 5.0,
            angular_sign: 1.0,
        });
        let bore = upright_cylinder(2.5, 1.5);
        let curves = trace_carriers(
            sphere,
            window((0.0, std::f64::consts::TAU), (-1.6, 1.6), true),
            bore,
            window((0.0, std::f64::consts::TAU), (-6.0, 6.0), true),
        )
        .expect("the pair traces");
        assert_eq!(curves.len(), 2, "one loop above the equator, one below");
        for curve in &curves {
            assert!(curve.closed);
            assert!(
                curve.deviation <= INTERSECTION_TOLERANCE * 10.0,
                "deviation {}",
                curve.deviation
            );
            for step in 0..=64 {
                let t = f64::from(step) / 64.0;
                let point = curve.curve.point(t);
                assert!(((point - Point3::new(0.0, 0.0, 0.0)).length() - 5.0).abs() < 1.0e-9);
                assert!(((point.x - 2.5).hypot(point.y) - 1.5).abs() < 1.0e-9);
            }
        }
    }

    /// A torus band and a bore that stays inside its hole are apart, and the
    /// sampled separation says so.
    #[test]
    fn a_bore_inside_a_torus_hole_is_apart() {
        let torus = Surface::Torus(Torus {
            origin: Point3::new(0.0, 0.0, 0.0),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            major_radius: 20.0,
            minor_radius: 3.0,
            angular_sign: 1.0,
        });
        let bore = upright_cylinder(8.0, 3.0);
        assert!(patches_apart(
            torus,
            window((0.0, std::f64::consts::PI), (0.0, 1.6), true),
            bore,
            window((0.0, std::f64::consts::TAU), (-40.0, 40.0), true),
        ));
        let near = upright_cylinder(15.0, 3.0);
        assert!(!patches_apart(
            torus,
            window((0.0, std::f64::consts::PI), (0.0, 1.6), true),
            near,
            window((0.0, std::f64::consts::TAU), (-40.0, 40.0), true),
        ));
    }
}

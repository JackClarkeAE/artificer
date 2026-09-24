//! Planar helpers over the protocol's loop vocabulary: lines and circular
//! arcs, closed into loops, nested into regions.
//!
//! Everything CAM reasons about in two dimensions — a lathe section in
//! `(r, z)`, a milled level's outline in `(x, y)`, a tool's swept region —
//! is a [`PlanarLoop2`] of lines and arcs. The kernel owns the hard parts
//! (offsets and Booleans, ADR 0057 §2.1); this module owns the easy ones:
//! winding, bounds, sampling, ray casting, and the union of regions that
//! only touch along shared boundaries, which is what the levels of a 2.5D
//! part do.

use std::f64::consts::{PI, TAU};

use artificer_protocol::{ArcDirection, PlanarCurve2, PlanarLoop2, PlanarRegion2, Point2};

/// Tolerance for two points to be one point, in millimetres.
pub const POINT_AGREEMENT: f64 = 1.0e-9;

#[must_use]
pub const fn point(x: f64, y: f64) -> Point2 {
    Point2::new(x, y)
}

#[must_use]
pub fn distance(a: Point2, b: Point2) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

#[must_use]
pub fn same_point(a: Point2, b: Point2) -> bool {
    distance(a, b) <= POINT_AGREEMENT
}

/// A closed polygon as a loop of lines.
#[must_use]
pub fn polygon(points: &[Point2]) -> PlanarLoop2 {
    PlanarLoop2::from_polygon(points)
}

/// An axis-aligned rectangle as a counter-clockwise loop.
#[must_use]
pub fn rectangle(min: Point2, max: Point2) -> PlanarLoop2 {
    polygon(&[min, point(max.x, min.y), max, point(min.x, max.y)])
}

/// The loop with every whole circle split into two half arcs, so every curve
/// has distinct ends and a direction.
#[must_use]
pub fn normalised(source: &PlanarLoop2) -> PlanarLoop2 {
    let mut curves = Vec::with_capacity(source.curves.len() + 1);
    for curve in &source.curves {
        match curve {
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            } => {
                let right = point(center.x + radius, center.y);
                let left = point(center.x - radius, center.y);
                curves.push(PlanarCurve2::CircularArc {
                    center: *center,
                    start: right,
                    end: left,
                    direction: *direction,
                });
                curves.push(PlanarCurve2::CircularArc {
                    center: *center,
                    start: left,
                    end: right,
                    direction: *direction,
                });
            }
            other => curves.push(other.clone()),
        }
    }
    PlanarLoop2 { curves }
}

#[must_use]
pub fn curve_start(curve: &PlanarCurve2) -> Point2 {
    match curve {
        PlanarCurve2::Line { start, .. } | PlanarCurve2::CircularArc { start, .. } => *start,
        PlanarCurve2::Circle { center, radius, .. } => point(center.x + radius, center.y),
        PlanarCurve2::Bspline { control_points, .. } => {
            control_points.first().copied().unwrap_or_default()
        }
    }
}

#[must_use]
pub fn curve_end(curve: &PlanarCurve2) -> Point2 {
    match curve {
        PlanarCurve2::Line { end, .. } | PlanarCurve2::CircularArc { end, .. } => *end,
        PlanarCurve2::Circle { center, radius, .. } => point(center.x + radius, center.y),
        PlanarCurve2::Bspline { control_points, .. } => {
            control_points.last().copied().unwrap_or_default()
        }
    }
}

/// An arc's radius, start angle and signed sweep: positive counter-clockwise,
/// always the unique turn below one revolution.
#[must_use]
pub fn arc_parameters(
    center: Point2,
    start: Point2,
    end: Point2,
    direction: ArcDirection,
) -> (f64, f64, f64) {
    let radius = distance(center, start);
    let start_angle = (start.y - center.y).atan2(start.x - center.x);
    let end_angle = (end.y - center.y).atan2(end.x - center.x);
    let mut sweep = match direction {
        ArcDirection::CounterClockwise => (end_angle - start_angle).rem_euclid(TAU),
        ArcDirection::Clockwise => -((start_angle - end_angle).rem_euclid(TAU)),
    };
    if sweep.abs() < 1.0e-12 {
        // Coincident ends name a full turn, the way a whole circle would.
        sweep = match direction {
            ArcDirection::CounterClockwise => TAU,
            ArcDirection::Clockwise => -TAU,
        };
    }
    (radius, start_angle, sweep)
}

/// A point at `angle` on a circle.
#[must_use]
pub fn on_circle(center: Point2, radius: f64, angle: f64) -> Point2 {
    point(
        radius.mul_add(angle.cos(), center.x),
        radius.mul_add(angle.sin(), center.y),
    )
}

#[must_use]
pub fn curve_length(curve: &PlanarCurve2) -> f64 {
    match curve {
        PlanarCurve2::Line { start, end } => distance(*start, *end),
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (radius, _, sweep) = arc_parameters(*center, *start, *end, *direction);
            radius * sweep.abs()
        }
        PlanarCurve2::Circle { radius, .. } => TAU * radius,
        PlanarCurve2::Bspline { control_points, .. } => control_points
            .windows(2)
            .map(|pair| distance(pair[0], pair[1]))
            .sum(),
    }
}

#[must_use]
pub fn loop_length(source: &PlanarLoop2) -> f64 {
    source.curves.iter().map(curve_length).sum()
}

/// The signed area: positive for a counter-clockwise loop.
#[must_use]
pub fn signed_area(source: &PlanarLoop2) -> f64 {
    let mut area = 0.0;
    for curve in &normalised(source).curves {
        let start = curve_start(curve);
        let end = curve_end(curve);
        area += start.x.mul_add(end.y, -(start.y * end.x)) / 2.0;
        if let PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } = curve
        {
            let (radius, _, sweep) = arc_parameters(*center, *start, *end, *direction);
            area += 0.5 * radius * radius * (sweep - sweep.sin());
        }
    }
    area
}

#[must_use]
pub fn region_area(region: &PlanarRegion2) -> f64 {
    signed_area(&region.outer).abs()
        - region
            .holes
            .iter()
            .map(|hole| signed_area(hole).abs())
            .sum::<f64>()
}

#[must_use]
pub fn reversed_curve(curve: &PlanarCurve2) -> PlanarCurve2 {
    match curve {
        PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
            start: *end,
            end: *start,
        },
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => PlanarCurve2::CircularArc {
            center: *center,
            start: *end,
            end: *start,
            direction: match direction {
                ArcDirection::CounterClockwise => ArcDirection::Clockwise,
                ArcDirection::Clockwise => ArcDirection::CounterClockwise,
            },
        },
        PlanarCurve2::Circle {
            center,
            radius,
            direction,
        } => PlanarCurve2::Circle {
            center: *center,
            radius: *radius,
            direction: match direction {
                ArcDirection::CounterClockwise => ArcDirection::Clockwise,
                ArcDirection::Clockwise => ArcDirection::CounterClockwise,
            },
        },
        PlanarCurve2::Bspline {
            degree,
            control_points,
            knots,
            weights,
        } => PlanarCurve2::Bspline {
            degree: *degree,
            control_points: control_points.iter().rev().copied().collect(),
            knots: knots.clone(),
            weights: weights.clone(),
        },
    }
}

#[must_use]
pub fn reversed(source: &PlanarLoop2) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: source.curves.iter().rev().map(reversed_curve).collect(),
    }
}

/// The loop wound counter-clockwise.
#[must_use]
pub fn counter_clockwise(source: &PlanarLoop2) -> PlanarLoop2 {
    if signed_area(source) < 0.0 {
        reversed(source)
    } else {
        source.clone()
    }
}

/// The loop wound clockwise.
#[must_use]
pub fn clockwise(source: &PlanarLoop2) -> PlanarLoop2 {
    if signed_area(source) > 0.0 {
        reversed(source)
    } else {
        source.clone()
    }
}

/// A region with its outer loop counter-clockwise and its holes clockwise.
#[must_use]
pub fn oriented_region(region: &PlanarRegion2) -> PlanarRegion2 {
    PlanarRegion2 {
        outer: counter_clockwise(&region.outer),
        holes: region.holes.iter().map(clockwise).collect(),
    }
}

/// The axis-aligned bounds of a loop, arcs included.
#[must_use]
pub fn bounds(source: &PlanarLoop2) -> Option<(Point2, Point2)> {
    let mut min = point(f64::INFINITY, f64::INFINITY);
    let mut max = point(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut include = |p: Point2| {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    };
    for curve in &normalised(source).curves {
        include(curve_start(curve));
        include(curve_end(curve));
        if let PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } = curve
        {
            let (radius, start_angle, sweep) = arc_parameters(*center, *start, *end, *direction);
            for quadrant in 0..4 {
                let angle = f64::from(quadrant) * PI / 2.0;
                if angle_in_sweep(angle, start_angle, sweep) {
                    include(on_circle(*center, radius, angle));
                }
            }
        }
    }
    (min.x.is_finite() && max.x.is_finite()).then_some((min, max))
}

#[must_use]
pub fn region_bounds(region: &PlanarRegion2) -> Option<(Point2, Point2)> {
    bounds(&region.outer)
}

#[must_use]
pub fn bounds_union(
    first: Option<(Point2, Point2)>,
    second: Option<(Point2, Point2)>,
) -> Option<(Point2, Point2)> {
    match (first, second) {
        (Some((a_min, a_max)), Some((b_min, b_max))) => Some((
            point(a_min.x.min(b_min.x), a_min.y.min(b_min.y)),
            point(a_max.x.max(b_max.x), a_max.y.max(b_max.y)),
        )),
        (Some(bounds), None) | (None, Some(bounds)) => Some(bounds),
        (None, None) => None,
    }
}

/// Whether `angle` lies on the arc from `start_angle` over `sweep`.
#[must_use]
pub fn angle_in_sweep(angle: f64, start_angle: f64, sweep: f64) -> bool {
    let tolerance = 1.0e-12;
    if sweep >= 0.0 {
        (angle - start_angle).rem_euclid(TAU) <= sweep + tolerance
    } else {
        (start_angle - angle).rem_euclid(TAU) <= -sweep + tolerance
    }
}

/// The curve sampled as a polyline whose chords stay within `tolerance` of
/// the arc. Includes both ends.
#[must_use]
pub fn sample_curve(curve: &PlanarCurve2, tolerance: f64) -> Vec<Point2> {
    match curve {
        PlanarCurve2::Line { start, end } => vec![*start, *end],
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (radius, start_angle, sweep) = arc_parameters(*center, *start, *end, *direction);
            let steps = arc_steps(radius, sweep, tolerance);
            let mut points = Vec::with_capacity(steps + 1);
            points.push(*start);
            for step in 1..steps {
                let angle = sweep.mul_add(step as f64 / steps as f64, start_angle);
                points.push(on_circle(*center, radius, angle));
            }
            points.push(*end);
            points
        }
        PlanarCurve2::Circle {
            center,
            radius,
            direction,
        } => {
            let sweep = match direction {
                ArcDirection::CounterClockwise => TAU,
                ArcDirection::Clockwise => -TAU,
            };
            let steps = arc_steps(*radius, sweep, tolerance);
            (0..=steps)
                .map(|step| on_circle(*center, *radius, sweep * step as f64 / steps as f64))
                .collect()
        }
        PlanarCurve2::Bspline { control_points, .. } => control_points.clone(),
    }
}

fn arc_steps(radius: f64, sweep: f64, tolerance: f64) -> usize {
    if radius <= 0.0 || !radius.is_finite() {
        return 1;
    }
    let tolerance = tolerance.max(1.0e-6).min(radius);
    let angle = 2.0 * (1.0 - tolerance / radius).clamp(-1.0, 1.0).acos();
    let steps = if angle > 0.0 {
        (sweep.abs() / angle).ceil()
    } else {
        1.0
    };
    (steps as usize).clamp(1, 4096)
}

/// The loop as a closed polyline (the first point is not repeated).
#[must_use]
pub fn sample_loop(source: &PlanarLoop2, tolerance: f64) -> Vec<Point2> {
    let mut points: Vec<Point2> = Vec::new();
    for curve in &normalised(source).curves {
        let sampled = sample_curve(curve, tolerance);
        for (index, p) in sampled.iter().enumerate() {
            if index + 1 == sampled.len() {
                continue;
            }
            if points.last().is_some_and(|last| same_point(*last, *p)) {
                continue;
            }
            points.push(*p);
        }
    }
    if points.len() > 1 && same_point(points[0], *points.last().expect("non-empty")) {
        points.pop();
    }
    points
}

/// Where a curve crosses the horizontal line at `y`, with the half-open
/// convention that makes a loop's crossing count even: a crossing at a
/// vertex counts only when the curve is strictly above the line on the side
/// adjacent to that vertex; a tangency counts as nothing.
pub fn horizontal_crossings(curve: &PlanarCurve2, y: f64, out: &mut Vec<f64>) {
    match curve {
        PlanarCurve2::Line { start, end } => {
            if (start.y > y) != (end.y > y) {
                let t = (y - start.y) / (end.y - start.y);
                out.push((end.x - start.x).mul_add(t, start.x));
            }
        }
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (radius, start_angle, sweep) = arc_parameters(*center, *start, *end, *direction);
            let dy = y - center.y;
            if dy.abs() >= radius {
                return;
            }
            let dx = (radius * radius - dy * dy).sqrt();
            let sense = sweep.signum();
            for candidate in [center.x + dx, center.x - dx] {
                let angle = dy.atan2(candidate - center.x);
                if !angle_in_sweep(angle, start_angle, sweep) {
                    continue;
                }
                // The derivative of y along the arc's own direction.
                let rising = angle.cos() * sense;
                let at_start = same_point(on_circle(*center, radius, angle), *start);
                let at_end = same_point(on_circle(*center, radius, angle), *end);
                let counted = if at_start && at_end {
                    false
                } else if at_start {
                    rising > 0.0
                } else if at_end {
                    rising < 0.0
                } else {
                    rising != 0.0
                };
                if counted {
                    out.push(candidate);
                }
            }
        }
        PlanarCurve2::Circle { .. } | PlanarCurve2::Bspline { .. } => {
            for piece in normalised(&PlanarLoop2 {
                curves: vec![curve.clone()],
            })
            .curves
            {
                if !matches!(piece, PlanarCurve2::Circle { .. }) {
                    horizontal_crossings(&piece, y, out);
                }
            }
        }
    }
}

/// The sorted x positions where the loop crosses the horizontal line at `y`.
#[must_use]
pub fn loop_crossings(source: &PlanarLoop2, y: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for curve in &source.curves {
        horizontal_crossings(curve, y, &mut out);
    }
    out.sort_by(f64::total_cmp);
    out
}

/// Whether a point lies inside a loop, by the parity of a ray cast towards
/// `+x`. Points on the boundary are not reliably either.
#[must_use]
pub fn point_in_loop(source: &PlanarLoop2, p: Point2) -> bool {
    loop_crossings(source, p.y)
        .into_iter()
        .filter(|x| *x > p.x)
        .count()
        % 2
        == 1
}

#[must_use]
pub fn point_in_region(region: &PlanarRegion2, p: Point2) -> bool {
    point_in_loop(&region.outer, p) && !region.holes.iter().any(|hole| point_in_loop(hole, p))
}

/// A point strictly inside the loop, found by probing the midpoints of its
/// curves a little to the left of travel (the interior side of a
/// counter-clockwise loop).
#[must_use]
pub fn interior_point(source: &PlanarLoop2) -> Option<Point2> {
    let ccw = counter_clockwise(source);
    let scale = bounds(&ccw).map_or(1.0, |(min, max)| {
        (max.x - min.x).max(max.y - min.y).max(1.0e-6)
    });
    for probe_scale in [1.0e-6, 1.0e-4, 1.0e-2] {
        let step = scale * probe_scale;
        for curve in &normalised(&ccw).curves {
            let (mid, tangent) = match curve {
                PlanarCurve2::Line { start, end } => (
                    point((start.x + end.x) / 2.0, (start.y + end.y) / 2.0),
                    point(end.x - start.x, end.y - start.y),
                ),
                PlanarCurve2::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => {
                    let (radius, start_angle, sweep) =
                        arc_parameters(*center, *start, *end, *direction);
                    let angle = sweep.mul_add(0.5, start_angle);
                    let sense = sweep.signum();
                    (
                        on_circle(*center, radius, angle),
                        point(-angle.sin() * sense, angle.cos() * sense),
                    )
                }
                _ => continue,
            };
            let length = tangent.x.hypot(tangent.y);
            if length <= 0.0 {
                continue;
            }
            let candidate = point(
                mid.x - tangent.y / length * step,
                mid.y + tangent.x / length * step,
            );
            if point_in_loop(&ccw, candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// The convex hull of a point set, counter-clockwise, by monotone chain.
#[must_use]
pub fn convex_hull(points: &[Point2]) -> Vec<Point2> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    sorted.dedup_by(|a, b| same_point(*a, *b));
    if sorted.len() < 3 {
        return sorted;
    }
    let cross = |o: Point2, a: Point2, b: Point2| {
        (a.x - o.x).mul_add(b.y - o.y, -((a.y - o.y) * (b.x - o.x)))
    };
    let mut lower: Vec<Point2> = Vec::new();
    for p in &sorted {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Point2> = Vec::new();
    for p in sorted.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

// ---------------------------------------------------------------------------
// Union of touching regions
// ---------------------------------------------------------------------------

/// One carrier a boundary piece lies on: a line named by its unit direction
/// and offset, or a circle by its centre and radius.
#[derive(Clone, Copy, Debug)]
enum Carrier {
    Line { direction: Point2, offset: f64 },
    Circle { center: Point2, radius: f64 },
}

impl Carrier {
    fn of(curve: &PlanarCurve2) -> Option<Self> {
        match curve {
            PlanarCurve2::Line { start, end } => {
                let length = distance(*start, *end);
                if length <= POINT_AGREEMENT {
                    return None;
                }
                let mut direction = point((end.x - start.x) / length, (end.y - start.y) / length);
                // One canonical direction per line, whichever way it is used.
                if direction.x < -1.0e-12 || (direction.x.abs() <= 1.0e-12 && direction.y < 0.0) {
                    direction = point(-direction.x, -direction.y);
                }
                let offset = (-direction.y).mul_add(start.x, direction.x * start.y);
                Some(Self::Line { direction, offset })
            }
            PlanarCurve2::CircularArc { center, start, .. } => Some(Self::Circle {
                center: *center,
                radius: distance(*center, *start),
            }),
            _ => None,
        }
    }

    fn matches(self, other: Self, scale: f64) -> bool {
        let tolerance = 1.0e-9 * scale.max(1.0);
        match (self, other) {
            (
                Self::Line { direction, offset },
                Self::Line {
                    direction: other_direction,
                    offset: other_offset,
                },
            ) => {
                distance(direction, other_direction) <= 1.0e-9
                    && (offset - other_offset).abs() <= tolerance
            }
            (
                Self::Circle { center, radius },
                Self::Circle {
                    center: other_center,
                    radius: other_radius,
                },
            ) => {
                distance(center, other_center) <= tolerance
                    && (radius - other_radius).abs() <= tolerance
            }
            _ => false,
        }
    }

    /// The carrier parameter of a point on it: distance along a line, angle
    /// on a circle.
    fn parameter(self, p: Point2) -> f64 {
        match self {
            Self::Line { direction, .. } => direction.x.mul_add(p.x, direction.y * p.y),
            Self::Circle { center, .. } => (p.y - center.y).atan2(p.x - center.x),
        }
    }

    fn point_at(self, parameter: f64) -> Point2 {
        match self {
            Self::Line { direction, offset } => point(
                direction.x.mul_add(parameter, -direction.y * offset),
                direction.y.mul_add(parameter, direction.x * offset),
            ),
            Self::Circle { center, radius } => on_circle(center, radius, parameter),
        }
    }
}

/// A boundary piece on a carrier: the parameter interval it covers, in the
/// carrier's own increasing direction, and whether the curve ran that way.
#[derive(Clone, Copy, Debug)]
struct Piece {
    low: f64,
    high: f64,
    forward: bool,
}

/// The union of regions that never overlap and only touch along shared
/// boundary pieces: the levels of a 2.5D part seen from above, a pocket's
/// floor filling the hole in the face above it. Shared pieces cancel; what
/// remains is chained into loops and nested into regions.
///
/// Refused when two regions overlap in area, which shows as a piece used
/// twice the same way.
pub fn merge_touching(regions: &[PlanarRegion2]) -> Result<Vec<PlanarRegion2>, String> {
    let mut curves: Vec<PlanarCurve2> = Vec::new();
    let mut scale: f64 = 1.0;
    for region in regions {
        let region = oriented_region(region);
        if let Some((min, max)) = region_bounds(&region) {
            scale = scale.max((max.x - min.x).abs()).max((max.y - min.y).abs());
            scale = scale
                .max(min.x.abs())
                .max(min.y.abs())
                .max(max.x.abs())
                .max(max.y.abs());
        }
        curves.extend(normalised(&region.outer).curves);
        for hole in &region.holes {
            curves.extend(normalised(hole).curves);
        }
    }
    // Group by carrier.
    let mut groups: Vec<(Carrier, Vec<Piece>)> = Vec::new();
    for curve in &curves {
        let Some(carrier) = Carrier::of(curve) else {
            return Err("a level outline carries a curve that is not a line or an arc".to_owned());
        };
        let group = match groups
            .iter()
            .position(|(existing, _)| existing.matches(carrier, scale))
        {
            Some(index) => index,
            None => {
                groups.push((carrier, Vec::new()));
                groups.len() - 1
            }
        };
        let carrier = groups[group].0;
        let start = curve_start(curve);
        let end = curve_end(curve);
        let piece = match (carrier, curve) {
            (Carrier::Line { .. }, _) => {
                let a = carrier.parameter(start);
                let b = carrier.parameter(end);
                Piece {
                    low: a.min(b),
                    high: a.max(b),
                    forward: b > a,
                }
            }
            (
                Carrier::Circle { .. },
                PlanarCurve2::CircularArc {
                    center, direction, ..
                },
            ) => {
                let (_, start_angle, sweep) = arc_parameters(*center, start, end, *direction);
                let (low, forward) = if sweep >= 0.0 {
                    (start_angle, true)
                } else {
                    (start_angle + sweep, false)
                };
                let low = low.rem_euclid(TAU);
                Piece {
                    low,
                    high: low + sweep.abs(),
                    forward,
                }
            }
            _ => return Err("a level outline carries an unexpected curve".to_owned()),
        };
        groups[group].1.push(piece);
    }

    let tolerance = 1.0e-9 * scale.max(1.0);
    let mut remaining: Vec<PlanarCurve2> = Vec::new();
    for (carrier, pieces) in &groups {
        let circular = matches!(carrier, Carrier::Circle { .. });
        // Every endpoint splits every piece it falls inside.
        // A circle's parameters are folded into one turn, `[0, 2π]`, so each
        // elementary arc is visited exactly once; a piece that crosses the
        // seam is covered by testing a midpoint and its turned copy.
        let mut cuts: Vec<f64> = Vec::new();
        for piece in pieces {
            if circular {
                cuts.push(piece.low.rem_euclid(TAU));
                cuts.push(piece.high.rem_euclid(TAU));
            } else {
                cuts.push(piece.low);
                cuts.push(piece.high);
            }
        }
        if circular {
            cuts.push(0.0);
            cuts.push(TAU);
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-9);
        // Net direction per elementary interval.
        let mut intervals: Vec<(f64, f64, i32)> = Vec::new();
        for window in cuts.windows(2) {
            let (low, high) = (window[0], window[1]);
            if high - low <= 1.0e-9 {
                continue;
            }
            let mid = (low + high) / 2.0;
            let mut net: i32 = 0;
            for piece in pieces {
                let covers = if circular {
                    [mid, mid + TAU]
                        .iter()
                        .any(|m| *m > piece.low + 1.0e-12 && *m < piece.high - 1.0e-12)
                } else {
                    mid > piece.low + 1.0e-12 && mid < piece.high - 1.0e-12
                };
                if covers {
                    net += if piece.forward { 1 } else { -1 };
                }
            }
            if net.abs() > 1 {
                return Err("two level outlines overlap in area rather than touching".to_owned());
            }
            if net != 0 {
                intervals.push((low, high, net));
            }
        }
        for (low, high, net) in intervals {
            let (a, b) = if net > 0 { (low, high) } else { (high, low) };
            let start = carrier.point_at(a);
            let end = carrier.point_at(b);
            let curve = match carrier {
                Carrier::Line { .. } => PlanarCurve2::Line { start, end },
                Carrier::Circle { center, .. } => PlanarCurve2::CircularArc {
                    center: *center,
                    start,
                    end,
                    direction: if net > 0 {
                        ArcDirection::CounterClockwise
                    } else {
                        ArcDirection::Clockwise
                    },
                },
            };
            if curve_length(&curve) > tolerance {
                remaining.push(curve);
            }
        }
    }
    let loops = chain_curves(remaining, tolerance)?;
    Ok(nest_loops(loops))
}

/// Chains curves into closed loops by endpoint identity.
pub fn chain_curves(
    mut curves: Vec<PlanarCurve2>,
    tolerance: f64,
) -> Result<Vec<PlanarLoop2>, String> {
    let mut loops = Vec::new();
    while let Some(first) = curves.pop() {
        let head = curve_start(&first);
        let mut tail = curve_end(&first);
        let mut chain = vec![first];
        while distance(tail, head) > tolerance {
            let Some(index) = curves
                .iter()
                .position(|curve| distance(curve_start(curve), tail) <= tolerance)
            else {
                return Err("a level outline does not close".to_owned());
            };
            let next = curves.remove(index);
            tail = curve_end(&next);
            chain.push(next);
        }
        loops.push(PlanarLoop2 { curves: chain });
    }
    Ok(loops)
}

/// Nests loops into regions by containment: a loop inside an even number of
/// others is an outer boundary, inside an odd number a hole of the smallest
/// loop that contains it.
#[must_use]
pub fn nest_loops(loops: Vec<PlanarLoop2>) -> Vec<PlanarRegion2> {
    let mut entries = loops
        .into_iter()
        .map(|source| {
            let area = signed_area(&source).abs();
            let probe = interior_point(&source);
            (source, area, probe)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.total_cmp(&a.1));
    let count = entries.len();
    let mut depth = vec![0_usize; count];
    let mut parent: Vec<Option<usize>> = vec![None; count];
    for index in 0..count {
        let Some(probe) = entries[index].2 else {
            continue;
        };
        for other in 0..count {
            if other == index || entries[other].1 <= entries[index].1 {
                continue;
            }
            if point_in_loop(&entries[other].0, probe) {
                depth[index] += 1;
                // The smallest containing loop is the immediate parent.
                match parent[index] {
                    Some(existing) if entries[existing].1 <= entries[other].1 => {}
                    _ => parent[index] = Some(other),
                }
            }
        }
    }
    let mut regions: Vec<(usize, PlanarRegion2)> = Vec::new();
    for index in 0..count {
        if depth[index] % 2 == 0 {
            regions.push((
                index,
                PlanarRegion2 {
                    outer: counter_clockwise(&entries[index].0),
                    holes: Vec::new(),
                },
            ));
        }
    }
    for index in 0..count {
        if depth[index] % 2 == 1
            && let Some(parent_index) = parent[index]
            && let Some((_, region)) = regions.iter_mut().find(|(owner, _)| *owner == parent_index)
        {
            region.holes.push(clockwise(&entries[index].0));
        }
    }
    regions.into_iter().map(|(_, region)| region).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(min: f64, max: f64) -> PlanarLoop2 {
        rectangle(point(min, min), point(max, max))
    }

    #[test]
    fn areas_and_winding_read_correctly() {
        let unit = square(0.0, 1.0);
        assert!((signed_area(&unit) - 1.0).abs() < 1.0e-12);
        assert!((signed_area(&reversed(&unit)) + 1.0).abs() < 1.0e-12);
        let circle = PlanarLoop2 {
            curves: vec![PlanarCurve2::Circle {
                center: point(0.0, 0.0),
                radius: 2.0,
                direction: ArcDirection::CounterClockwise,
            }],
        };
        assert!((signed_area(&circle) - PI * 4.0).abs() < 1.0e-9);
        assert!(point_in_loop(&circle, point(1.0, 1.0)));
        assert!(!point_in_loop(&circle, point(2.0, 1.0)));
        let (min, max) = bounds(&circle).unwrap();
        assert!((min.x + 2.0).abs() < 1.0e-12 && (max.y - 2.0).abs() < 1.0e-12);
    }

    #[test]
    fn a_pocket_floor_fills_the_hole_in_the_face_above_it() {
        let top = PlanarRegion2 {
            outer: square(0.0, 10.0),
            holes: vec![clockwise(&square(3.0, 6.0))],
        };
        let floor = PlanarRegion2 {
            outer: square(3.0, 6.0),
            holes: Vec::new(),
        };
        let merged = merge_touching(&[top, floor]).unwrap();
        assert_eq!(merged.len(), 1);
        assert!(merged[0].holes.is_empty());
        assert!((region_area(&merged[0]) - 100.0).abs() < 1.0e-9);
    }

    #[test]
    fn a_boss_inside_a_pocket_stays_an_island() {
        let top = PlanarRegion2 {
            outer: square(0.0, 10.0),
            holes: vec![clockwise(&square(2.0, 8.0))],
        };
        let boss = PlanarRegion2 {
            outer: square(4.0, 6.0),
            holes: Vec::new(),
        };
        let merged = merge_touching(&[top, boss]).unwrap();
        assert_eq!(merged.len(), 2, "{merged:?}");
        let total = merged.iter().map(region_area).sum::<f64>();
        assert!((total - (100.0 - 36.0 + 4.0)).abs() < 1.0e-9);
    }

    #[test]
    fn touching_circles_cancel_their_shared_rim() {
        let ring = PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: point(0.0, 0.0),
                    radius: 5.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: point(0.0, 0.0),
                    radius: 2.0,
                    direction: ArcDirection::Clockwise,
                }],
            }],
        };
        let disc = PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: point(0.0, 0.0),
                    radius: 2.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: Vec::new(),
        };
        let merged = merge_touching(&[ring, disc]).unwrap();
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(merged[0].holes.is_empty(), "{merged:?}");
        assert!((region_area(&merged[0]) - PI * 25.0).abs() < 1.0e-9);
    }

    #[test]
    fn hull_is_counter_clockwise_and_drops_interior_points() {
        let hull = convex_hull(&[
            point(0.0, 0.0),
            point(2.0, 0.0),
            point(1.0, 1.0),
            point(2.0, 2.0),
            point(0.0, 2.0),
        ]);
        assert_eq!(hull.len(), 4);
        assert!(signed_area(&polygon(&hull)) > 0.0);
    }
}

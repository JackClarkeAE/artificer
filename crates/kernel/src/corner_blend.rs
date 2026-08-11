//! Certified two-dimensional corner blends shared by every exact finish path.
//!
//! An exact blend is a planar corner operation followed by an exact rebuild:
//! `edge_finish` rounds a rectangle corner and re-extrudes, `revolve` replaces
//! a rim corner of the (r, z) section and re-revolves, and the prism paths do
//! the same on a recovered profile. This module owns that corner operation so
//! every caller shares one certified implementation.
//!
//! Nothing here guesses. A configuration that admits several blends, or none,
//! is an error rather than a choice: interactive callers resolve ambiguity by
//! changing the selection or the distance, never by trusting a heuristic.

use artificer_protocol::{EdgeFinishKind, PrecisionPolicy};

use crate::analytic_extrusion::Segment;
use crate::topology::Point2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CornerBlendError {
    /// The junction is tangent-continuous, a cusp, or the seam of one logical
    /// carrier: there is no corner to blend.
    NoCorner,
    /// The blend consumes more of a neighbour than the neighbour can give.
    TrimTooLarge,
    /// No candidate satisfies tangency inside both neighbours.
    NoSolution,
    /// Several candidates survive; the caller must disambiguate.
    Ambiguous,
}

/// One corner rewritten: both neighbours shortened, joined by a connector.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CornerBlend {
    pub(crate) trimmed_incoming: Segment,
    pub(crate) trimmed_outgoing: Segment,
    /// `Arc` for a fillet, `Line` for a chamfer.
    pub(crate) connector: Segment,
    /// Whether the blend consumed a neighbour exactly, so its trimmed form
    /// closed to a point rather than keeping a usable remnant.
    ///
    /// Reported rather than refused because the two are different facts. A
    /// blend that eats a neighbour *exactly* is legitimate — filleting both
    /// rims of a cylinder at its own radius consumes the caps and the wall
    /// and leaves a sphere. A blend that overshoots is not, and never
    /// certifies: the tangency foot falls off the neighbour entirely. Callers
    /// that can drop a collapsed piece accept these; callers that cannot must
    /// reject them.
    pub(crate) consumed: ConsumedNeighbours,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConsumedNeighbours {
    pub(crate) incoming: bool,
    pub(crate) outgoing: bool,
}

impl ConsumedNeighbours {
    pub(crate) const fn any(self) -> bool {
        self.incoming || self.outgoing
    }
}

/// Blends the corner where `incoming` ends and `outgoing` starts.
///
/// `material_probe` reports whether a planar point lies in the solid's
/// material, which decides the branch a fillet takes at a convex versus a
/// concave corner. `distance` is the fillet radius or the chamfer setback.
pub(crate) fn corner_blend(
    incoming: Segment,
    outgoing: Segment,
    kind: EdgeFinishKind,
    distance: f64,
    material_probe: &dyn Fn(Point2) -> bool,
    precision: PrecisionPolicy,
) -> Result<CornerBlend, CornerBlendError> {
    if !distance.is_finite() || distance < precision.min_feature_size {
        return Err(CornerBlendError::TrimTooLarge);
    }
    let corner = incoming.end();
    if !points_agree(corner, outgoing.start(), precision) {
        return Err(CornerBlendError::NoCorner);
    }
    if incoming.shares_side_carrier(outgoing) {
        return Err(CornerBlendError::NoCorner);
    }
    let incoming_tangent = end_tangent(incoming)?;
    let outgoing_tangent = start_tangent(outgoing)?;
    let turn = signed_turn(incoming_tangent, outgoing_tangent);
    if turn.abs() <= precision.angular_agreement_radians
        || (std::f64::consts::PI - turn.abs()) <= precision.angular_agreement_radians
    {
        return Err(CornerBlendError::NoCorner);
    }

    match kind {
        EdgeFinishKind::Chamfer => build_chamfer(incoming, outgoing, distance, precision),
        EdgeFinishKind::Fillet => {
            build_fillet(incoming, outgoing, distance, material_probe, precision)
        }
    }
}

// ---------------------------------------------------------------------------
// Chamfer
// ---------------------------------------------------------------------------

fn build_chamfer(
    incoming: Segment,
    outgoing: Segment,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<CornerBlend, CornerBlendError> {
    // Set back by arc length along each neighbour, so a chamfer on an arc
    // removes the same length of material as one on a line.
    let start = point_at_arc_length_from_end(incoming, distance, precision)?;
    let end = point_at_arc_length_from_start(outgoing, distance, precision)?;
    let trimmed_incoming = retarget_end(incoming, start)?;
    let trimmed_outgoing = retarget_start(outgoing, end)?;
    let connector = Segment::Line { start, end };
    if segment_length(connector) < precision.min_feature_size {
        return Err(CornerBlendError::TrimTooLarge);
    }
    Ok(CornerBlend {
        trimmed_incoming,
        trimmed_outgoing,
        connector,
        // A chamfer sets back by arc length, so consuming a neighbour exactly
        // would leave the connector spanning the whole piece; that stays out
        // of the certified domain until a case needs it.
        consumed: ConsumedNeighbours::default(),
    })
}

// ---------------------------------------------------------------------------
// Fillet
// ---------------------------------------------------------------------------

/// One offset carrier of a neighbour: the locus of fillet centres tangent to
/// it at the requested radius.
#[derive(Clone, Copy, Debug)]
enum OffsetLocus {
    /// Line through `point` with unit direction `direction`.
    Line {
        point: Point2,
        direction: Vector2,
    },
    Circle {
        center: Point2,
        radius: f64,
    },
}

#[derive(Clone, Copy, Debug)]
struct Vector2 {
    x: f64,
    y: f64,
}

impl Vector2 {
    const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn dot(self, other: Self) -> f64 {
        self.x.mul_add(other.x, self.y * other.y)
    }

    fn cross(self, other: Self) -> f64 {
        self.x.mul_add(other.y, -(self.y * other.x))
    }

    fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    fn normalized(self) -> Option<Self> {
        let length = self.length();
        (length.is_finite() && length > 0.0).then(|| Self::new(self.x / length, self.y / length))
    }

    const fn perpendicular(self) -> Self {
        Self::new(-self.y, self.x)
    }

    fn scaled(self, factor: f64) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

fn offset(point: Point2, vector: Vector2) -> Point2 {
    Point2::new(point.x + vector.x, point.y + vector.y)
}

fn between(from: Point2, to: Point2) -> Vector2 {
    Vector2::new(to.x - from.x, to.y - from.y)
}

fn build_fillet(
    incoming: Segment,
    outgoing: Segment,
    radius: f64,
    material_probe: &dyn Fn(Point2) -> bool,
    precision: PrecisionPolicy,
) -> Result<CornerBlend, CornerBlendError> {
    let mut candidates = Vec::new();
    for incoming_locus in offset_loci(incoming, radius, precision) {
        for outgoing_locus in offset_loci(outgoing, radius, precision) {
            for center in locus_intersections(incoming_locus, outgoing_locus, precision) {
                if let Some(candidate) =
                    certify_candidate(incoming, outgoing, center, radius, precision)
                {
                    candidates.push(candidate);
                }
            }
        }
    }
    canonicalize(&mut candidates, precision);

    // A convex corner removes material at the connector midpoint, a concave
    // corner adds it. Both are legal; the probe only has to be consistent, so
    // when several geometric candidates survive it selects the one whose
    // midpoint agrees with the corner's own convexity.
    let corner_is_convex = !material_probe(corner_probe_point(incoming, outgoing, precision));
    let matching = candidates
        .iter()
        .filter(|candidate| {
            let midpoint = arc_midpoint(candidate.center, candidate.start, candidate.end, radius);
            material_probe(midpoint) != corner_is_convex
        })
        .copied()
        .collect::<Vec<_>>();
    let surviving = if matching.is_empty() {
        candidates
    } else {
        matching
    };

    match surviving.len() {
        0 => Err(CornerBlendError::NoSolution),
        1 => finish_fillet(incoming, outgoing, surviving[0], radius, precision),
        _ => Err(CornerBlendError::Ambiguous),
    }
}

#[derive(Clone, Copy, Debug)]
struct FilletCandidate {
    center: Point2,
    start: Point2,
    end: Point2,
    consumed: ConsumedNeighbours,
}

fn offset_loci(segment: Segment, radius: f64, precision: PrecisionPolicy) -> Vec<OffsetLocus> {
    match segment {
        Segment::Line { start, end } => {
            let Some(direction) = between(start, end).normalized() else {
                return Vec::new();
            };
            let normal = direction.perpendicular();
            [radius, -radius]
                .into_iter()
                .map(|signed| OffsetLocus::Line {
                    point: offset(start, normal.scaled(signed)),
                    direction,
                })
                .collect()
        }
        Segment::Arc {
            center, radius: r, ..
        } => {
            let mut loci = Vec::new();
            if r - radius >= precision.min_feature_size {
                loci.push(OffsetLocus::Circle {
                    center,
                    radius: r - radius,
                });
            }
            loci.push(OffsetLocus::Circle {
                center,
                radius: r + radius,
            });
            loci
        }
    }
}

fn locus_intersections(
    first: OffsetLocus,
    second: OffsetLocus,
    precision: PrecisionPolicy,
) -> Vec<Point2> {
    match (first, second) {
        (
            OffsetLocus::Line {
                point: first_point,
                direction: first_direction,
            },
            OffsetLocus::Line {
                point: second_point,
                direction: second_direction,
            },
        ) => {
            let determinant = first_direction.cross(second_direction);
            if determinant.abs() <= precision.angular_agreement_radians {
                return Vec::new();
            }
            let delta = between(first_point, second_point);
            let travel = delta.cross(second_direction) / determinant;
            vec![offset(first_point, first_direction.scaled(travel))]
        }
        (OffsetLocus::Line { point, direction }, OffsetLocus::Circle { center, radius })
        | (OffsetLocus::Circle { center, radius }, OffsetLocus::Line { point, direction }) => {
            line_circle_intersections(point, direction, center, radius)
        }
        (
            OffsetLocus::Circle {
                center: first_center,
                radius: first_radius,
            },
            OffsetLocus::Circle {
                center: second_center,
                radius: second_radius,
            },
        ) => circle_circle_intersections(first_center, first_radius, second_center, second_radius),
    }
}

fn line_circle_intersections(
    point: Point2,
    direction: Vector2,
    center: Point2,
    radius: f64,
) -> Vec<Point2> {
    let to_center = between(point, center);
    let projection = to_center.dot(direction);
    let foot = offset(point, direction.scaled(projection));
    let gap_squared = radius.mul_add(radius, -between(center, foot).dot(between(center, foot)));
    if gap_squared < 0.0 {
        return Vec::new();
    }
    let half_chord = gap_squared.sqrt();
    if half_chord == 0.0 {
        return vec![foot];
    }
    vec![
        offset(foot, direction.scaled(half_chord)),
        offset(foot, direction.scaled(-half_chord)),
    ]
}

fn circle_circle_intersections(
    first_center: Point2,
    first_radius: f64,
    second_center: Point2,
    second_radius: f64,
) -> Vec<Point2> {
    let axis = between(first_center, second_center);
    let separation = axis.length();
    if separation == 0.0 || !separation.is_finite() {
        return Vec::new();
    }
    let along = (separation.mul_add(
        separation,
        first_radius.mul_add(first_radius, -(second_radius * second_radius)),
    )) / (2.0 * separation);
    let height_squared = first_radius.mul_add(first_radius, -(along * along));
    if height_squared < 0.0 {
        return Vec::new();
    }
    let Some(direction) = axis.normalized() else {
        return Vec::new();
    };
    let base = offset(first_center, direction.scaled(along));
    let height = height_squared.sqrt();
    if height == 0.0 {
        return vec![base];
    }
    let normal = direction.perpendicular();
    vec![
        offset(base, normal.scaled(height)),
        offset(base, normal.scaled(-height)),
    ]
}

/// Accepts a centre only when both tangency feet land strictly inside their
/// neighbours and the centre really is `radius` from each carrier.
fn certify_candidate(
    incoming: Segment,
    outgoing: Segment,
    center: Point2,
    radius: f64,
    precision: PrecisionPolicy,
) -> Option<FilletCandidate> {
    let tolerance = tangency_tolerance(radius, precision);
    let start = tangency_foot(incoming, center)?;
    let end = tangency_foot(outgoing, center)?;
    if (between(center, start).length() - radius).abs() > tolerance
        || (between(center, end).length() - radius).abs() > tolerance
    {
        return None;
    }
    // The foot must lie on its neighbour — `arc_length_from_start` already
    // refuses a foot that falls off either end, so overshoot never reaches
    // here. What remains is how much of the neighbour survives: a usable
    // remnant, or nothing at all because the blend consumed it exactly.
    // Anything between those is an unusable sliver and still refuses.
    let incoming_remaining = arc_length_from_start(incoming, start, precision)?;
    let outgoing_remaining = arc_length_from_end(outgoing, end, precision)?;
    let classify = |remaining: f64, length: f64| {
        if remaining <= tangency_tolerance(length, precision) {
            Some(true)
        } else if remaining < precision.min_feature_size {
            None
        } else {
            Some(false)
        }
    };
    let consumed = ConsumedNeighbours {
        incoming: classify(incoming_remaining, segment_length(incoming))?,
        outgoing: classify(outgoing_remaining, segment_length(outgoing))?,
    };
    Some(FilletCandidate {
        center,
        start,
        end,
        consumed,
    })
}

fn finish_fillet(
    incoming: Segment,
    outgoing: Segment,
    candidate: FilletCandidate,
    radius: f64,
    precision: PrecisionPolicy,
) -> Result<CornerBlend, CornerBlendError> {
    let trimmed_incoming = retarget_end(incoming, candidate.start)?;
    let trimmed_outgoing = retarget_start(outgoing, candidate.end)?;
    let start_angle = angle_of(candidate.center, candidate.start);
    let end_angle = angle_of(candidate.center, candidate.end);

    // The connector must leave the incoming tangent continuously: pick the
    // sweep direction whose initial motion matches it. Read that tangent off
    // the incoming *carrier* at the tangency foot rather than off the trimmed
    // remnant — the two agree wherever a remnant survives, and the carrier
    // still answers when the blend consumed its neighbour outright.
    let incoming_tangent = carrier_tangent_at(incoming, candidate.start)?;
    let radial = between(candidate.center, candidate.start);
    let counter_clockwise_tangent = radial.perpendicular();
    let sweep_sign = if counter_clockwise_tangent.dot(incoming_tangent) >= 0.0 {
        1.0
    } else {
        -1.0
    };
    let mut sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    if sweep_sign < 0.0 {
        sweep -= std::f64::consts::TAU;
    }
    if sweep.abs() < precision.angular_agreement_radians
        || sweep.abs() >= std::f64::consts::PI + precision.angular_agreement_radians
    {
        // A blend arc sweeping half a turn or more means the neighbours were
        // resolved on the wrong branch; reject rather than emit a reflex arc.
        return Err(CornerBlendError::NoSolution);
    }
    let connector = Segment::Arc {
        center: candidate.center,
        start: candidate.start,
        end: candidate.end,
        radius,
        start_angle,
        sweep,
    };
    Ok(CornerBlend {
        trimmed_incoming,
        trimmed_outgoing,
        connector,
        consumed: candidate.consumed,
    })
}

fn canonicalize(candidates: &mut Vec<FilletCandidate>, precision: PrecisionPolicy) {
    candidates.sort_by(|left, right| {
        left.center
            .x
            .total_cmp(&right.center.x)
            .then_with(|| left.center.y.total_cmp(&right.center.y))
    });
    let tolerance = precision.linear_agreement.max(f64::EPSILON);
    candidates.dedup_by(|left, right| {
        (left.center.x - right.center.x).abs() <= tolerance
            && (left.center.y - right.center.y).abs() <= tolerance
    });
}

/// A point just inside the corner along the angle bisector, used to decide
/// whether the corner itself is convex.
fn corner_probe_point(incoming: Segment, outgoing: Segment, precision: PrecisionPolicy) -> Point2 {
    let corner = incoming.end();
    let Ok(incoming_tangent) = end_tangent(incoming) else {
        return corner;
    };
    let Ok(outgoing_tangent) = start_tangent(outgoing) else {
        return corner;
    };
    // Directions pointing away from the corner along each neighbour.
    let back = incoming_tangent.scaled(-1.0);
    let forward = outgoing_tangent;
    let Some(bisector) = Vector2::new(back.x + forward.x, back.y + forward.y).normalized() else {
        return corner;
    };
    offset(
        corner,
        bisector.scaled(precision.min_feature_size.max(1.0e-7)),
    )
}

fn arc_midpoint(center: Point2, start: Point2, end: Point2, radius: f64) -> Point2 {
    let start_angle = angle_of(center, start);
    let end_angle = angle_of(center, end);
    let mut sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    if sweep > std::f64::consts::PI {
        sweep -= std::f64::consts::TAU;
    }
    let middle = start_angle + sweep * 0.5;
    Point2::new(
        radius.mul_add(middle.cos(), center.x),
        radius.mul_add(middle.sin(), center.y),
    )
}

// ---------------------------------------------------------------------------
// Segment helpers
// ---------------------------------------------------------------------------

fn tangency_tolerance(radius: f64, precision: PrecisionPolicy) -> f64 {
    precision
        .linear_agreement
        .max(radius.abs() * 1.0e-12)
        .max(f64::EPSILON * 16.0)
}

fn points_agree(first: Point2, second: Point2, precision: PrecisionPolicy) -> bool {
    let scale = 1.0
        + first
            .x
            .abs()
            .max(first.y.abs())
            .max(second.x.abs())
            .max(second.y.abs());
    between(first, second).length() <= precision.linear_agreement.max(1.0e-12) * scale
}

fn angle_of(center: Point2, point: Point2) -> f64 {
    (point.y - center.y).atan2(point.x - center.x)
}

fn start_tangent(segment: Segment) -> Result<Vector2, CornerBlendError> {
    match segment {
        Segment::Line { start, end } => between(start, end)
            .normalized()
            .ok_or(CornerBlendError::NoCorner),
        Segment::Arc {
            center,
            start,
            sweep,
            ..
        } => {
            let radial = between(center, start)
                .normalized()
                .ok_or(CornerBlendError::NoCorner)?;
            Ok(radial.perpendicular().scaled(sweep.signum()))
        }
    }
}

/// The carrier's unit tangent at a point on it, in the segment's direction of
/// travel. Unlike `end_tangent` this never depends on the segment's extent, so
/// it stays defined for a piece trimmed to nothing.
fn carrier_tangent_at(segment: Segment, point: Point2) -> Result<Vector2, CornerBlendError> {
    match segment {
        Segment::Line { start, end } => between(start, end)
            .normalized()
            .ok_or(CornerBlendError::NoCorner),
        Segment::Arc { center, sweep, .. } => {
            let radial = between(center, point)
                .normalized()
                .ok_or(CornerBlendError::NoCorner)?;
            Ok(radial.perpendicular().scaled(sweep.signum()))
        }
    }
}

fn end_tangent(segment: Segment) -> Result<Vector2, CornerBlendError> {
    match segment {
        Segment::Line { start, end } => between(start, end)
            .normalized()
            .ok_or(CornerBlendError::NoCorner),
        Segment::Arc {
            center, end, sweep, ..
        } => {
            let radial = between(center, end)
                .normalized()
                .ok_or(CornerBlendError::NoCorner)?;
            Ok(radial.perpendicular().scaled(sweep.signum()))
        }
    }
}

fn signed_turn(incoming: Vector2, outgoing: Vector2) -> f64 {
    incoming.cross(outgoing).atan2(incoming.dot(outgoing))
}

pub(crate) fn segment_length(segment: Segment) -> f64 {
    match segment {
        Segment::Line { start, end } => between(start, end).length(),
        Segment::Arc { radius, sweep, .. } => radius * sweep.abs(),
    }
}

/// The point on `segment` at `distance` measured back from its end.
fn point_at_arc_length_from_end(
    segment: Segment,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Point2, CornerBlendError> {
    let length = segment_length(segment);
    if distance > length - precision.min_feature_size {
        return Err(CornerBlendError::TrimTooLarge);
    }
    Ok(match segment {
        Segment::Line { start, end } => {
            let direction = between(end, start)
                .normalized()
                .ok_or(CornerBlendError::NoCorner)?;
            offset(end, direction.scaled(distance))
        }
        Segment::Arc {
            center,
            end,
            radius,
            sweep,
            ..
        } => {
            let end_angle = angle_of(center, end);
            let angle = end_angle - sweep.signum() * (distance / radius);
            Point2::new(
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            )
        }
    })
}

/// The point on `segment` at `distance` measured forward from its start.
fn point_at_arc_length_from_start(
    segment: Segment,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Point2, CornerBlendError> {
    let length = segment_length(segment);
    if distance > length - precision.min_feature_size {
        return Err(CornerBlendError::TrimTooLarge);
    }
    Ok(match segment {
        Segment::Line { start, end } => {
            let direction = between(start, end)
                .normalized()
                .ok_or(CornerBlendError::NoCorner)?;
            offset(start, direction.scaled(distance))
        }
        Segment::Arc {
            center,
            start,
            radius,
            sweep,
            ..
        } => {
            let start_angle = angle_of(center, start);
            let angle = start_angle + sweep.signum() * (distance / radius);
            Point2::new(
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            )
        }
    })
}

/// Arc length from the segment's start to `point`, or `None` when the point is
/// not on the segment's retained span.
fn arc_length_from_start(
    segment: Segment,
    point: Point2,
    precision: PrecisionPolicy,
) -> Option<f64> {
    match segment {
        Segment::Line { start, end } => {
            let direction = between(start, end).normalized()?;
            let travel = between(start, point).dot(direction);
            let length = between(start, end).length();
            let deviation = between(offset(start, direction.scaled(travel)), point).length();
            (deviation <= tangency_tolerance(length, precision)
                && travel >= 0.0
                && travel <= length)
                .then_some(travel)
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let distance = between(center, point).length();
            if (distance - radius).abs() > tangency_tolerance(radius, precision) {
                return None;
            }
            let angle = angle_of(center, point);
            let progress = if sweep > 0.0 {
                (angle - start_angle).rem_euclid(std::f64::consts::TAU) / sweep
            } else {
                (start_angle - angle).rem_euclid(std::f64::consts::TAU) / -sweep
            };
            (0.0..=1.0)
                .contains(&progress)
                .then(|| progress * radius * sweep.abs())
        }
    }
}

/// Arc length from `point` to the segment's end.
fn arc_length_from_end(segment: Segment, point: Point2, precision: PrecisionPolicy) -> Option<f64> {
    let travelled = arc_length_from_start(segment, point, precision)?;
    Some(segment_length(segment) - travelled)
}

/// Foot of the perpendicular (line) or the radial projection (arc) from
/// `center` onto the segment's carrier.
fn tangency_foot(segment: Segment, center: Point2) -> Option<Point2> {
    match segment {
        Segment::Line { start, end } => {
            let direction = between(start, end).normalized()?;
            let travel = between(start, center).dot(direction);
            Some(offset(start, direction.scaled(travel)))
        }
        Segment::Arc {
            center: arc_center,
            radius,
            ..
        } => {
            let radial = between(arc_center, center).normalized()?;
            Some(offset(arc_center, radial.scaled(radius)))
        }
    }
}

/// Rebuilds `segment` ending at `point`, preserving its carrier.
pub(crate) fn retarget_end(segment: Segment, point: Point2) -> Result<Segment, CornerBlendError> {
    Ok(match segment {
        Segment::Line { start, .. } => Segment::Line { start, end: point },
        Segment::Arc {
            center,
            start,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let end_angle = angle_of(center, point);
            let new_sweep = if sweep > 0.0 {
                (end_angle - start_angle).rem_euclid(std::f64::consts::TAU)
            } else {
                -((start_angle - end_angle).rem_euclid(std::f64::consts::TAU))
            };
            Segment::Arc {
                center,
                start,
                end: point,
                radius,
                start_angle,
                sweep: new_sweep,
            }
        }
    })
}

/// Rebuilds `segment` starting at `point`, preserving its carrier.
pub(crate) fn retarget_start(segment: Segment, point: Point2) -> Result<Segment, CornerBlendError> {
    Ok(match segment {
        Segment::Line { end, .. } => Segment::Line { start: point, end },
        Segment::Arc {
            center,
            end,
            radius,
            sweep,
            ..
        } => {
            let start_angle = angle_of(center, point);
            let end_angle = angle_of(center, end);
            let new_sweep = if sweep > 0.0 {
                (end_angle - start_angle).rem_euclid(std::f64::consts::TAU)
            } else {
                -((start_angle - end_angle).rem_euclid(std::f64::consts::TAU))
            };
            Segment::Arc {
                center,
                start: point,
                end,
                radius,
                start_angle,
                sweep: new_sweep,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn precision() -> PrecisionPolicy {
        PrecisionPolicy::default()
    }

    fn line(start: (f64, f64), end: (f64, f64)) -> Segment {
        Segment::Line {
            start: Point2::new(start.0, start.1),
            end: Point2::new(end.0, end.1),
        }
    }

    #[test]
    fn right_angle_line_corner_matches_the_closed_form() {
        // Material is the lower-left quadrant interior; the corner at (1,0)
        // turns left, so it is convex.
        let incoming = line((0.0, 0.0), (1.0, 0.0));
        let outgoing = line((1.0, 0.0), (1.0, 1.0));
        let radius = 0.25;
        let probe = |point: Point2| point.x < 1.0 && point.y > 0.0;
        let blend = corner_blend(
            incoming,
            outgoing,
            EdgeFinishKind::Fillet,
            radius,
            &probe,
            precision(),
        )
        .expect("a right-angle fillet is exact");

        // t = r / tan(45 deg) = r.
        assert!((blend.trimmed_incoming.end().x - (1.0 - radius)).abs() < 1.0e-12);
        assert!((blend.trimmed_outgoing.start().y - radius).abs() < 1.0e-12);
        match blend.connector {
            Segment::Arc {
                center,
                radius: arc_radius,
                sweep,
                ..
            } => {
                assert!((center.x - (1.0 - radius)).abs() < 1.0e-12);
                assert!((center.y - radius).abs() < 1.0e-12);
                assert!((arc_radius - radius).abs() < 1.0e-12);
                assert!((sweep.abs() - std::f64::consts::FRAC_PI_2).abs() < 1.0e-12);
            }
            Segment::Line { .. } => panic!("a fillet connector is an arc"),
        }
    }

    #[test]
    fn chamfer_sets_back_by_arc_length_on_both_neighbours() {
        let incoming = line((0.0, 0.0), (1.0, 0.0));
        let outgoing = line((1.0, 0.0), (1.0, 1.0));
        let probe = |point: Point2| point.x < 1.0 && point.y > 0.0;
        let blend = corner_blend(
            incoming,
            outgoing,
            EdgeFinishKind::Chamfer,
            0.25,
            &probe,
            precision(),
        )
        .expect("a right-angle chamfer is exact");
        assert!((blend.trimmed_incoming.end().x - 0.75).abs() < 1.0e-12);
        assert!((blend.trimmed_outgoing.start().y - 0.25).abs() < 1.0e-12);
        assert!(matches!(blend.connector, Segment::Line { .. }));
        assert!(
            (segment_length(blend.connector) - 0.25 * std::f64::consts::SQRT_2).abs() < 1.0e-12
        );
    }

    #[test]
    fn a_tangent_junction_has_no_corner_to_blend() {
        let incoming = line((0.0, 0.0), (1.0, 0.0));
        let outgoing = line((1.0, 0.0), (2.0, 0.0));
        let probe = |_: Point2| false;
        assert_eq!(
            corner_blend(
                incoming,
                outgoing,
                EdgeFinishKind::Fillet,
                0.1,
                &probe,
                precision(),
            )
            .err(),
            Some(CornerBlendError::NoCorner)
        );
    }

    #[test]
    fn an_oversized_chamfer_rejects_rather_than_inverting_a_neighbour() {
        let incoming = line((0.0, 0.0), (1.0, 0.0));
        let outgoing = line((1.0, 0.0), (1.0, 1.0));
        let probe = |point: Point2| point.x < 1.0 && point.y > 0.0;
        assert_eq!(
            corner_blend(
                incoming,
                outgoing,
                EdgeFinishKind::Chamfer,
                2.0,
                &probe,
                precision(),
            )
            .err(),
            Some(CornerBlendError::TrimTooLarge)
        );
    }

    #[test]
    fn a_line_to_arc_corner_resolves_one_certified_fillet() {
        // A quarter pie slice: radial line out to (4,0), arc back to (0,4).
        let radius = 4.0_f64;
        let incoming = line((0.0, 0.0), (radius, 0.0));
        let outgoing = Segment::Arc {
            center: Point2::new(0.0, 0.0),
            start: Point2::new(radius, 0.0),
            end: Point2::new(0.0, radius),
            radius,
            start_angle: 0.0,
            sweep: std::f64::consts::FRAC_PI_2,
        };
        let fillet = 1.0;
        let probe = |point: Point2| {
            let distance = point.x.hypot(point.y);
            distance < radius && point.y > 0.0 && point.x > 0.0
        };
        let blend = corner_blend(
            incoming,
            outgoing,
            EdgeFinishKind::Fillet,
            fillet,
            &probe,
            precision(),
        )
        .expect("a line/arc corner admits one exact fillet");
        match blend.connector {
            Segment::Arc { center, .. } => {
                // Centre lies at height f above the line and at radius R - f
                // from the arc centre.
                assert!((center.y - fillet).abs() < 1.0e-12);
                assert!((center.x.hypot(center.y) - (radius - fillet)).abs() < 1.0e-12);
            }
            Segment::Line { .. } => panic!("a fillet connector is an arc"),
        }
    }
}

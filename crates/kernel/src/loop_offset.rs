//! Certified inward offset of a closed planar loop.
//!
//! A rim-loop fillet of radius `f` sweeps a ball along the loop: the ball's
//! centre path — the spine — is the loop offset inward by `f`, mitred at sharp
//! convex corners because the ball cannot round a corner without passing
//! through the far wall. The shrunk cap of the finished body is exactly that
//! spine, so this offset is load-bearing geometry rather than a helper.
//!
//! Offsetting is where a blend radius stops being feasible: an arc can invert,
//! a segment can vanish between two mitres, and a narrow neck can make the
//! spine cross itself. Each of those is certified here and rejected, so the
//! caller never has to guess whether a radius fits.

use artificer_protocol::PrecisionPolicy;

use crate::analytic_extrusion::{Segment, segment_clearance};
use crate::corner_blend::segment_length;
use crate::topology::Point2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoopOffsetError {
    /// The radius exceeds a convex arc, or leaves a segment shorter than the
    /// minimum feature size once both its ends are mitred.
    RadiusTooLarge,
    /// The offset loop crosses itself: a neck closed up.
    SelfIntersects,
    /// A sharp concave corner: the two blend bands would meet in a quartic
    /// curve, outside the analytic vocabulary.
    ReflexSharpCorner,
    /// The loop is malformed or the offset is numerically indeterminate.
    Degenerate,
}

/// How the spine treats one source vertex.
///
/// The corner measurements are recorded for the blend builders that will
/// set back a corner patch; today's consumers decide by kind alone.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum SpineVertexKind {
    /// The neighbours were tangent-continuous, so their offsets already meet.
    Tangent,
    /// A sharp convex corner: the offsets meet at a mitre point, and each
    /// neighbour gives up `trim` of arc length reaching it.
    SharpConvex {
        interior_angle: f64,
        trim_in: f64,
        trim_out: f64,
    },
    /// A sharp reflex corner between two straight runs: the offsets are
    /// extended until they meet, so the spine keeps a corner there.
    SharpReflex { interior_angle: f64 },
}

/// What a sharp reflex corner may become on the spine.
///
/// A fillet cannot round a reflex corner: the two blend bands would meet in a
/// quartic curve, outside the analytic vocabulary. A chamfer between two
/// straight runs can, because two slant planes meet in a straight mitre line,
/// so a chamfer asks for `MitreLines` and a fillet for `Refuse`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReflexPolicy {
    Refuse,
    MitreLines,
}

/// The offset loop plus the per-vertex metadata a blend builder needs.
pub(crate) struct SpineLoop {
    pub(crate) segments: Vec<Segment>,
    pub(crate) vertices: Vec<SpineVertexKind>,
}

/// Offsets a counter-clockwise loop inward by `distance`.
///
/// The loop's material lies to the left of travel, so the inward normal is the
/// left normal. Vertex `index` of the result corresponds to the junction that
/// starts source segment `index`.
pub(crate) fn mitred_inward_offset(
    source: &[Segment],
    distance: f64,
    reflex: ReflexPolicy,
    precision: PrecisionPolicy,
) -> Result<SpineLoop, LoopOffsetError> {
    if !distance.is_finite() || distance <= 0.0 {
        return Err(LoopOffsetError::Degenerate);
    }
    mitred_offset(source, distance, reflex, precision)
}

/// Offsets a counter-clockwise loop by a signed `distance`: inward when
/// positive, outward when negative.
///
/// Outward offsetting mirrors the corner cases. A convex corner's outward
/// offsets diverge and are extended until they meet, exactly as a reflex
/// corner's inward offsets are; a reflex corner's outward offsets overlap and
/// are trimmed at their crossing, as a convex corner's inward offsets are.
/// `reflex` governs whichever corners need extending.
pub(crate) fn mitred_offset(
    source: &[Segment],
    distance: f64,
    reflex: ReflexPolicy,
    precision: PrecisionPolicy,
) -> Result<SpineLoop, LoopOffsetError> {
    if source.len() < 2 || !distance.is_finite() || distance == 0.0 {
        return Err(LoopOffsetError::Degenerate);
    }
    let outward = distance < 0.0;
    let count = source.len();

    // Offset each carrier independently first; corners reconcile afterwards.
    let mut offsets = Vec::with_capacity(count);
    for segment in source {
        offsets.push(offset_carrier(*segment, distance, precision)?);
    }

    let mut vertices = Vec::with_capacity(count);
    let mut starts = vec![Point2::new(0.0, 0.0); count];
    let mut ends = vec![Point2::new(0.0, 0.0); count];
    for index in 0..count {
        let previous = (index + count - 1) % count;
        let incoming = source[previous];
        let outgoing = source[index];
        let turn = turn_angle(incoming, outgoing)?;
        if turn.abs() <= precision.angular_agreement_radians {
            // Tangent junction: both offsets already end at the same point.
            let meeting = offsets[previous].end;
            ends[previous] = meeting;
            starts[index] = offsets[index].start;
            vertices.push(SpineVertexKind::Tangent);
            continue;
        }
        // A right turn on a counter-clockwise loop is a reflex corner, whose
        // inward offsets diverge; offsetting outward, it is the convex
        // corners whose offsets diverge instead.
        let diverging = if outward { turn > 0.0 } else { turn < 0.0 };
        if diverging {
            let both_straight = matches!(
                (offsets[previous].kind, offsets[index].kind),
                (CarrierKind::Line { .. }, CarrierKind::Line { .. })
            );
            if reflex == ReflexPolicy::Refuse || !both_straight {
                return Err(LoopOffsetError::ReflexSharpCorner);
            }
            // Two offset lines always meet once; extending both to that point
            // keeps the corner sharp on the spine.
            let mitre = mitre_point(&offsets[previous], &offsets[index], precision)
                .ok_or(LoopOffsetError::Degenerate)?;
            ends[previous] = mitre;
            starts[index] = mitre;
            vertices.push(SpineVertexKind::SharpReflex {
                interior_angle: std::f64::consts::PI - turn,
            });
            continue;
        }
        let mitre = mitre_point(&offsets[previous], &offsets[index], precision)
            .ok_or(LoopOffsetError::RadiusTooLarge)?;
        let trim_in = carrier_arc_length(&offsets[previous], offsets[previous].start, mitre)
            .ok_or(LoopOffsetError::RadiusTooLarge)?;
        let trim_out = carrier_arc_length(&offsets[index], mitre, offsets[index].end)
            .ok_or(LoopOffsetError::RadiusTooLarge)?;
        ends[previous] = mitre;
        starts[index] = mitre;
        let interior_angle = std::f64::consts::PI - turn;
        vertices.push(SpineVertexKind::SharpConvex {
            interior_angle,
            trim_in: full_length(&offsets[previous]) - trim_in,
            trim_out: full_length(&offsets[index]) - trim_out,
        });
    }

    // Assemble and certify.
    let mut segments = Vec::with_capacity(count);
    for index in 0..count {
        let trimmed = retarget(&offsets[index], starts[index], ends[index])?;
        if segment_length(trimmed) < precision.min_feature_size
            || consumed(&offsets[index], trimmed, source[index], precision)
        {
            return Err(LoopOffsetError::RadiusTooLarge);
        }
        segments.push(trimmed);
    }
    certify(&segments, signed_area(source).signum(), precision)?;
    Ok(SpineLoop { segments, vertices })
}

/// Whether trimming consumed the whole carrier: the two mitres crossed, so
/// the piece left between them runs against its source. That is a collapsed
/// section even when the loop as a whole still winds the right way — a
/// square offset past its centre comes back as a smaller square with every
/// side reversed, and the winding test alone would pass it.
fn consumed(
    carrier: &OffsetCarrier,
    trimmed: Segment,
    source: Segment,
    precision: PrecisionPolicy,
) -> bool {
    match (carrier.kind, trimmed, source) {
        (CarrierKind::Line { direction }, Segment::Line { start, end }, _) => {
            between(start, end).dot(direction) <= 0.0
        }
        (
            CarrierKind::Arc { .. },
            Segment::Arc { sweep, .. },
            Segment::Arc {
                sweep: source_sweep,
                ..
            },
        ) => sweep.abs() > source_sweep.abs() + precision.angular_agreement_radians,
        _ => false,
    }
}

/// One offset carrier, still untrimmed.
#[derive(Clone, Copy, Debug)]
struct OffsetCarrier {
    kind: CarrierKind,
    start: Point2,
    end: Point2,
}

#[derive(Clone, Copy, Debug)]
enum CarrierKind {
    Line {
        direction: Vector,
    },
    Arc {
        center: Point2,
        radius: f64,
        sweep_sign: f64,
    },
}

#[derive(Clone, Copy, Debug)]
struct Vector {
    x: f64,
    y: f64,
}

impl Vector {
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
    /// The left normal, which points into the material on a CCW loop.
    const fn left_normal(self) -> Self {
        Self::new(-self.y, self.x)
    }
    fn scaled(self, factor: f64) -> Self {
        Self::new(self.x * factor, self.y * factor)
    }
}

fn shift(point: Point2, vector: Vector) -> Point2 {
    Point2::new(point.x + vector.x, point.y + vector.y)
}

fn between(from: Point2, to: Point2) -> Vector {
    Vector::new(to.x - from.x, to.y - from.y)
}

fn offset_carrier(
    segment: Segment,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<OffsetCarrier, LoopOffsetError> {
    match segment {
        Segment::Line { start, end } => {
            let direction = between(start, end)
                .normalized()
                .ok_or(LoopOffsetError::Degenerate)?;
            let inward = direction.left_normal().scaled(distance);
            Ok(OffsetCarrier {
                kind: CarrierKind::Line { direction },
                start: shift(start, inward),
                end: shift(end, inward),
            })
        }
        Segment::Arc {
            center,
            start,
            end,
            radius,
            sweep,
            ..
        } => {
            // On a counter-clockwise loop a positive sweep is a convex arc, so
            // its inward offset shrinks; a negative sweep is concave and grows.
            let offset_radius = if sweep >= 0.0 {
                radius - distance
            } else {
                radius + distance
            };
            if offset_radius < precision.min_feature_size {
                return Err(LoopOffsetError::RadiusTooLarge);
            }
            let scale = offset_radius / radius;
            Ok(OffsetCarrier {
                kind: CarrierKind::Arc {
                    center,
                    radius: offset_radius,
                    sweep_sign: sweep.signum(),
                },
                start: scale_about(center, start, scale),
                end: scale_about(center, end, scale),
            })
        }
    }
}

fn scale_about(center: Point2, point: Point2, scale: f64) -> Point2 {
    Point2::new(
        (point.x - center.x).mul_add(scale, center.x),
        (point.y - center.y).mul_add(scale, center.y),
    )
}

fn turn_angle(incoming: Segment, outgoing: Segment) -> Result<f64, LoopOffsetError> {
    let leaving = end_direction(incoming)?;
    let entering = start_direction(outgoing)?;
    Ok(leaving.cross(entering).atan2(leaving.dot(entering)))
}

fn start_direction(segment: Segment) -> Result<Vector, LoopOffsetError> {
    match segment {
        Segment::Line { start, end } => between(start, end)
            .normalized()
            .ok_or(LoopOffsetError::Degenerate),
        Segment::Arc {
            center,
            start,
            sweep,
            ..
        } => Ok(between(center, start)
            .normalized()
            .ok_or(LoopOffsetError::Degenerate)?
            .left_normal()
            .scaled(sweep.signum())),
    }
}

fn end_direction(segment: Segment) -> Result<Vector, LoopOffsetError> {
    match segment {
        Segment::Line { start, end } => between(start, end)
            .normalized()
            .ok_or(LoopOffsetError::Degenerate),
        Segment::Arc {
            center, end, sweep, ..
        } => Ok(between(center, end)
            .normalized()
            .ok_or(LoopOffsetError::Degenerate)?
            .left_normal()
            .scaled(sweep.signum())),
    }
}

/// Where two adjacent offset carriers meet, choosing the intersection nearest
/// the corner the source neighbours shared.
fn mitre_point(
    incoming: &OffsetCarrier,
    outgoing: &OffsetCarrier,
    precision: PrecisionPolicy,
) -> Option<Point2> {
    let candidates = match (incoming.kind, outgoing.kind) {
        (
            CarrierKind::Line {
                direction: first_direction,
            },
            CarrierKind::Line {
                direction: second_direction,
            },
        ) => {
            let determinant = first_direction.cross(second_direction);
            if determinant.abs() <= precision.angular_agreement_radians {
                return None;
            }
            let delta = between(incoming.start, outgoing.start);
            let travel = delta.cross(second_direction) / determinant;
            vec![shift(incoming.start, first_direction.scaled(travel))]
        }
        (CarrierKind::Line { direction }, CarrierKind::Arc { center, radius, .. }) => {
            line_circle(incoming.start, direction, center, radius)
        }
        (CarrierKind::Arc { center, radius, .. }, CarrierKind::Line { direction }) => {
            line_circle(outgoing.start, direction, center, radius)
        }
        (
            CarrierKind::Arc {
                center: first_center,
                radius: first_radius,
                ..
            },
            CarrierKind::Arc {
                center: second_center,
                radius: second_radius,
                ..
            },
        ) => circle_circle(first_center, first_radius, second_center, second_radius),
    };
    // The mitre is the candidate closest to where the untrimmed offsets end.
    let anchor = incoming.end;
    candidates
        .into_iter()
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .min_by(|left, right| {
            between(anchor, *left)
                .length()
                .total_cmp(&between(anchor, *right).length())
        })
}

fn line_circle(point: Point2, direction: Vector, center: Point2, radius: f64) -> Vec<Point2> {
    let projection = between(point, center).dot(direction);
    let foot = shift(point, direction.scaled(projection));
    let gap = radius.mul_add(radius, -between(center, foot).dot(between(center, foot)));
    if gap < 0.0 {
        return Vec::new();
    }
    let half = gap.sqrt();
    vec![
        shift(foot, direction.scaled(half)),
        shift(foot, direction.scaled(-half)),
    ]
}

fn circle_circle(
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
    let along = separation.mul_add(
        separation,
        first_radius.mul_add(first_radius, -(second_radius * second_radius)),
    ) / (2.0 * separation);
    let height = first_radius.mul_add(first_radius, -(along * along));
    if height < 0.0 {
        return Vec::new();
    }
    let Some(direction) = axis.normalized() else {
        return Vec::new();
    };
    let base = shift(first_center, direction.scaled(along));
    let offset = height.sqrt();
    let normal = direction.left_normal();
    vec![
        shift(base, normal.scaled(offset)),
        shift(base, normal.scaled(-offset)),
    ]
}

fn full_length(carrier: &OffsetCarrier) -> f64 {
    match carrier.kind {
        CarrierKind::Line { .. } => between(carrier.start, carrier.end).length(),
        CarrierKind::Arc { center, radius, .. } => {
            let start = angle_of(center, carrier.start);
            let end = angle_of(center, carrier.end);
            radius * (end - start).abs()
        }
    }
}

fn carrier_arc_length(carrier: &OffsetCarrier, from: Point2, to: Point2) -> Option<f64> {
    match carrier.kind {
        CarrierKind::Line { direction } => {
            let travel = between(from, to).dot(direction);
            travel.is_finite().then_some(travel.abs())
        }
        CarrierKind::Arc { center, radius, .. } => {
            let start = angle_of(center, from);
            let end = angle_of(center, to);
            Some(radius * (end - start).abs())
        }
    }
}

fn angle_of(center: Point2, point: Point2) -> f64 {
    (point.y - center.y).atan2(point.x - center.x)
}

fn retarget(
    carrier: &OffsetCarrier,
    start: Point2,
    end: Point2,
) -> Result<Segment, LoopOffsetError> {
    Ok(match carrier.kind {
        CarrierKind::Line { .. } => Segment::Line { start, end },
        CarrierKind::Arc {
            center,
            radius,
            sweep_sign,
        } => {
            let start_angle = angle_of(center, start);
            let end_angle = angle_of(center, end);
            let sweep = if sweep_sign >= 0.0 {
                (end_angle - start_angle).rem_euclid(std::f64::consts::TAU)
            } else {
                -((start_angle - end_angle).rem_euclid(std::f64::consts::TAU))
            };
            Segment::Arc {
                center,
                start,
                end,
                radius,
                start_angle,
                sweep,
            }
        }
    })
}

/// Rejects a spine that crosses itself or encloses no material. `sense` is
/// the sign of the source loop's signed area: an outer boundary runs
/// counter-clockwise, a hole clockwise, and the spine must keep its source's
/// winding rather than invert through a collapse.
fn certify(
    segments: &[Segment],
    sense: f64,
    precision: PrecisionPolicy,
) -> Result<(), LoopOffsetError> {
    let count = segments.len();
    for first in 0..count {
        for second in first + 1..count {
            let adjacent = second == first + 1 || (first == 0 && second == count - 1);
            if adjacent {
                continue;
            }
            if segment_clearance(segments[first], segments[second]) < precision.min_feature_size {
                return Err(LoopOffsetError::SelfIntersects);
            }
        }
    }
    let area = signed_area(segments) * sense;
    if !area.is_finite() || area <= precision.min_feature_size * precision.min_feature_size {
        return Err(LoopOffsetError::SelfIntersects);
    }
    Ok(())
}

fn signed_area(segments: &[Segment]) -> f64 {
    let mut area = 0.0;
    for segment in segments {
        let start = segment.start();
        let end = segment.end();
        area += start.x.mul_add(end.y, -(start.y * end.x)) / 2.0;
        if let Segment::Arc { radius, sweep, .. } = *segment {
            area += 0.5 * radius * radius * (sweep - sweep.sin());
        }
    }
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    fn precision() -> PrecisionPolicy {
        PrecisionPolicy::default()
    }

    fn rectangle(width: f64, height: f64) -> Vec<Segment> {
        let corners = [
            Point2::new(0.0, 0.0),
            Point2::new(width, 0.0),
            Point2::new(width, height),
            Point2::new(0.0, height),
        ];
        (0..4)
            .map(|index| Segment::Line {
                start: corners[index],
                end: corners[(index + 1) % 4],
            })
            .collect()
    }

    #[test]
    fn a_rectangle_offsets_to_a_mitred_rectangle() {
        let spine = mitred_inward_offset(
            &rectangle(10.0, 6.0),
            1.0,
            ReflexPolicy::Refuse,
            precision(),
        )
        .expect("a rectangle offsets cleanly");
        assert_eq!(spine.segments.len(), 4);
        assert!(matches!(
            spine.vertices[0],
            SpineVertexKind::SharpConvex { .. }
        ));
        // The offset rectangle is 8 x 4 inset by one unit on every side.
        let area = signed_area(&spine.segments);
        assert!((area - 8.0 * 4.0).abs() < 1.0e-12, "spine area was {area}");
    }

    #[test]
    fn an_oversized_offset_collapses_and_rejects() {
        // Half the short side leaves nothing behind.
        let result = mitred_inward_offset(
            &rectangle(10.0, 6.0),
            3.0,
            ReflexPolicy::Refuse,
            precision(),
        );
        assert!(
            matches!(
                result,
                Err(LoopOffsetError::SelfIntersects | LoopOffsetError::RadiusTooLarge)
            ),
            "an offset that consumes the loop must reject"
        );
    }

    #[test]
    fn a_reflex_corner_rejects() {
        // L-shape: the vertex at (6,4) turns right on a CCW loop.
        let corners = [
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 4.0),
            Point2::new(6.0, 4.0),
            Point2::new(6.0, 9.0),
            Point2::new(0.0, 9.0),
        ];
        let loop_: Vec<Segment> = (0..corners.len())
            .map(|index| Segment::Line {
                start: corners[index],
                end: corners[(index + 1) % corners.len()],
            })
            .collect();
        assert_eq!(
            mitred_inward_offset(&loop_, 1.0, ReflexPolicy::Refuse, precision()).err(),
            Some(LoopOffsetError::ReflexSharpCorner)
        );
        // A chamfer may mitre it: the offsets of the two runs meeting at (6,4)
        // are extended to their intersection at (5,3), one unit inside both.
        let spine = mitred_inward_offset(&loop_, 1.0, ReflexPolicy::MitreLines, precision())
            .expect("straight reflex corners mitre for a chamfer");
        assert_eq!(spine.segments.len(), 6);
        assert!(matches!(
            spine.vertices[3],
            SpineVertexKind::SharpReflex { .. }
        ));
        let corner = spine.segments[3].start();
        assert!((corner.x - 5.0).abs() < 1.0e-12 && (corner.y - 3.0).abs() < 1.0e-12);
        // The inset L keeps the outer L's shape: 8 x 2 plus 4 x 5.
        let area = signed_area(&spine.segments);
        assert!(
            (area - (8.0 * 2.0 + 4.0 * 5.0)).abs() < 1.0e-12,
            "spine area was {area}"
        );
    }

    #[test]
    fn a_convex_arc_smaller_than_the_radius_rejects() {
        // Stadium: two lines joined by tangent semicircles of radius 2.
        let radius = 2.0;
        let loop_ = vec![
            Segment::Line {
                start: Point2::new(0.0, 0.0),
                end: Point2::new(6.0, 0.0),
            },
            Segment::Arc {
                center: Point2::new(6.0, radius),
                start: Point2::new(6.0, 0.0),
                end: Point2::new(6.0, 2.0 * radius),
                radius,
                start_angle: -std::f64::consts::FRAC_PI_2,
                sweep: std::f64::consts::PI,
            },
            Segment::Line {
                start: Point2::new(6.0, 2.0 * radius),
                end: Point2::new(0.0, 2.0 * radius),
            },
            Segment::Arc {
                center: Point2::new(0.0, radius),
                start: Point2::new(0.0, 2.0 * radius),
                end: Point2::new(0.0, 0.0),
                radius,
                start_angle: std::f64::consts::FRAC_PI_2,
                sweep: std::f64::consts::PI,
            },
        ];
        // A radius equal to the arc radius leaves a degenerate carrier.
        assert_eq!(
            mitred_inward_offset(&loop_, radius, ReflexPolicy::Refuse, precision()).err(),
            Some(LoopOffsetError::RadiusTooLarge)
        );
        // A smaller radius is fine, and every junction stays tangent.
        let spine = mitred_inward_offset(&loop_, 0.5, ReflexPolicy::Refuse, precision())
            .expect("a tangent stadium offsets cleanly");
        assert!(
            spine
                .vertices
                .iter()
                .all(|vertex| matches!(vertex, SpineVertexKind::Tangent)),
            "a stadium has no sharp corners"
        );
    }
}

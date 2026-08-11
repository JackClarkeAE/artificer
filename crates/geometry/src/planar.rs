//! Certified planar entities and relations built on the exact predicate ladder.

use crate::{Direction2, Orientation2, Point2, Segment2, Vector2, orient2d};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line2 {
    pub origin: Point2,
    pub direction: Direction2,
}

impl Line2 {
    pub fn new(origin: Point2, direction: Vector2) -> Option<Self> {
        origin.is_finite().then_some(Self {
            origin,
            direction: Direction2::new(direction)?,
        })
    }

    #[must_use]
    pub fn evaluate(self, parameter: f64) -> Point2 {
        let direction = self.direction.vector();
        Point2::new(
            self.origin.x + direction.x * parameter,
            self.origin.y + direction.y * parameter,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle2 {
    pub center: Point2,
    pub radius: f64,
}

impl Circle2 {
    pub fn new(center: Point2, radius: f64) -> Option<Self> {
        (center.is_finite() && radius.is_finite() && radius > 0.0)
            .then_some(Self { center, radius })
    }

    #[must_use]
    pub fn evaluate(self, radians: f64) -> Point2 {
        Point2::new(
            self.center.x + self.radius * radians.cos(),
            self.center.y + self.radius * radians.sin(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arc2 {
    pub circle: Circle2,
    pub start_radians: f64,
    pub sweep_radians: f64,
}

impl Arc2 {
    pub fn new(circle: Circle2, start_radians: f64, sweep_radians: f64) -> Option<Self> {
        (start_radians.is_finite()
            && sweep_radians.is_finite()
            && sweep_radians != 0.0
            && sweep_radians.abs() <= std::f64::consts::TAU)
            .then_some(Self {
                circle,
                start_radians,
                sweep_radians,
            })
    }

    #[must_use]
    pub fn evaluate(self, normalized_parameter: f64) -> Point2 {
        self.circle.evaluate(
            self.start_radians + self.sweep_radians * normalized_parameter.clamp(0.0, 1.0),
        )
    }
}

/// Topological relation of two closed finite segments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentRelation {
    Disjoint,
    ProperIntersection,
    EndpointTouch,
    CollinearOverlap,
    Indeterminate,
}

#[must_use]
pub fn classify_segment_relation(first: Segment2, second: Segment2) -> SegmentRelation {
    if !first.is_finite() || !second.is_finite() {
        return SegmentRelation::Indeterminate;
    }
    if bounds_disjoint(first, second) {
        return SegmentRelation::Disjoint;
    }
    let orientations = [
        orient2d(first.start, first.end, second.start),
        orient2d(first.start, first.end, second.end),
        orient2d(second.start, second.end, first.start),
        orient2d(second.start, second.end, first.end),
    ];
    if orientations.contains(&Orientation2::Indeterminate) {
        return SegmentRelation::Indeterminate;
    }
    if opposite(orientations[0], orientations[1]) && opposite(orientations[2], orientations[3]) {
        return SegmentRelation::ProperIntersection;
    }
    let contacts = [
        (orientations[0], second.start, first),
        (orientations[1], second.end, first),
        (orientations[2], first.start, second),
        (orientations[3], first.end, second),
    ];
    let collinear_contacts = contacts
        .into_iter()
        .filter(|(orientation, point, segment)| {
            *orientation == Orientation2::Collinear && in_bounds(*point, *segment)
        })
        .count();
    match collinear_contacts {
        0 => SegmentRelation::Disjoint,
        1 => SegmentRelation::EndpointTouch,
        _ if orientations
            .iter()
            .all(|value| *value == Orientation2::Collinear) =>
        {
            let shared_endpoint_only = [first.start, first.end]
                .into_iter()
                .any(|point| point == second.start || point == second.end)
                && collinear_contacts == 2;
            if shared_endpoint_only {
                SegmentRelation::EndpointTouch
            } else {
                SegmentRelation::CollinearOverlap
            }
        }
        _ => SegmentRelation::EndpointTouch,
    }
}

fn opposite(first: Orientation2, second: Orientation2) -> bool {
    matches!(
        (first, second),
        (Orientation2::Clockwise, Orientation2::CounterClockwise)
            | (Orientation2::CounterClockwise, Orientation2::Clockwise)
    )
}

fn bounds_disjoint(first: Segment2, second: Segment2) -> bool {
    first.start.x.min(first.end.x) > second.start.x.max(second.end.x)
        || second.start.x.min(second.end.x) > first.start.x.max(first.end.x)
        || first.start.y.min(first.end.y) > second.start.y.max(second.end.y)
        || second.start.y.min(second.end.y) > first.start.y.max(first.end.y)
}

fn in_bounds(point: Point2, segment: Segment2) -> bool {
    (segment.start.x.min(segment.end.x)..=segment.start.x.max(segment.end.x)).contains(&point.x)
        && (segment.start.y.min(segment.end.y)..=segment.start.y.max(segment.end.y))
            .contains(&point.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certified_segment_relations_cover_cross_touch_overlap_and_disjoint() {
        let segment = |a, b| Segment2::new(Point2::new(a, 0.0), Point2::new(b, 0.0));
        assert_eq!(
            classify_segment_relation(
                Segment2::new(Point2::new(0.0, 0.0), Point2::new(2.0, 2.0)),
                Segment2::new(Point2::new(0.0, 2.0), Point2::new(2.0, 0.0)),
            ),
            SegmentRelation::ProperIntersection
        );
        assert_eq!(
            classify_segment_relation(segment(0.0, 1.0), segment(1.0, 2.0)),
            SegmentRelation::EndpointTouch
        );
        assert_eq!(
            classify_segment_relation(segment(0.0, 2.0), segment(1.0, 3.0)),
            SegmentRelation::CollinearOverlap
        );
        assert_eq!(
            classify_segment_relation(segment(0.0, 1.0), segment(2.0, 3.0)),
            SegmentRelation::Disjoint
        );
    }
}

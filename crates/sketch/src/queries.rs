//! Parameter-aware sketch hit testing and semantic snapping.
//!
//! Queries operate exclusively on evaluated analytic curves. Callers convert
//! their pixel radius to sketch units before entering this module; screen-space
//! tolerances never become modeling tolerances.

use artificer_protocol::PrecisionPolicy;

use crate::{
    CurveIntersections, IntersectionBranch, SketchCurve2, SketchDefinition, SketchEntityId,
    SketchPoint2, SketchPointId, intersect_entities,
};

/// Stable semantic identity of one snap candidate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SketchSnapKey {
    Endpoint {
        entity: SketchEntityId,
        point: SketchPointId,
    },
    Intersection {
        first_entity: SketchEntityId,
        second_entity: SketchEntityId,
        branch: IntersectionBranch,
    },
    Center {
        entity: SketchEntityId,
    },
    Midpoint {
        entity: SketchEntityId,
    },
    Quadrant {
        entity: SketchEntityId,
        index: u8,
    },
}

impl SketchSnapKey {
    const fn priority(&self) -> u8 {
        match self {
            Self::Endpoint { .. } => 0,
            Self::Intersection { .. } => 1,
            Self::Center { .. } => 2,
            Self::Midpoint { .. } => 3,
            Self::Quadrant { .. } => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SketchSnapCandidate {
    pub key: SketchSnapKey,
    pub point: SketchPoint2,
    pub distance_squared: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchCurveHit {
    pub entity: SketchEntityId,
    pub parameter: f64,
    pub point: SketchPoint2,
    pub distance_squared: f64,
}

/// Returns visible active curves beneath a model-space pick radius, nearest
/// first with stable entity-ID tie breaking.
#[must_use]
pub fn hit_test_curves(
    definition: &SketchDefinition,
    pointer: SketchPoint2,
    radius: f64,
) -> Vec<SketchCurveHit> {
    if !pointer.is_finite() || !radius.is_finite() || radius < 0.0 {
        return Vec::new();
    }
    let radius_squared = radius * radius;
    let mut hits = definition
        .active_entities()
        .filter(|entity| entity.visible)
        .filter_map(|entity| {
            let curve = definition.evaluated_curve(entity.id).ok()?;
            if !curve.bounds().expanded(radius).contains(pointer) {
                return None;
            }
            let parameter = curve.closest_parameter(pointer);
            let point = curve.evaluate(parameter).ok()?;
            let distance_squared = point.distance_squared(pointer);
            (distance_squared <= radius_squared).then_some(SketchCurveHit {
                entity: entity.id,
                parameter,
                point,
                distance_squared,
            })
        })
        .collect::<Vec<_>>();
    hits.sort_by(|first, second| {
        first
            .distance_squared
            .total_cmp(&second.distance_squared)
            .then_with(|| first.entity.cmp(&second.entity))
            .then_with(|| first.parameter.total_cmp(&second.parameter))
    });
    hits
}

/// Collects typed snap candidates near `pointer`. The output is bounded before
/// it reaches the UI and deterministically ordered by semantic priority,
/// distance, and stable key.
#[must_use]
pub fn query_snap_candidates(
    definition: &SketchDefinition,
    pointer: SketchPoint2,
    radius: f64,
    precision: &PrecisionPolicy,
    max_candidates: usize,
) -> Vec<SketchSnapCandidate> {
    if max_candidates == 0 || !pointer.is_finite() || !radius.is_finite() || radius < 0.0 {
        return Vec::new();
    }
    let radius_squared = radius * radius;
    let nearby = definition
        .active_entities()
        .filter(|entity| entity.visible)
        .filter_map(|entity| {
            let curve = definition.evaluated_curve(entity.id).ok()?;
            curve
                .bounds()
                .expanded(radius)
                .contains(pointer)
                .then_some((entity, curve))
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();

    for (entity, curve) in &nearby {
        match entity.geometry {
            SketchCurve2::Line { start, end } | SketchCurve2::CircularArc { start, end, .. } => {
                let Some((start_position, end_position)) = curve.endpoints() else {
                    continue;
                };
                push_candidate(
                    &mut candidates,
                    SketchSnapKey::Endpoint {
                        entity: entity.id,
                        point: start,
                    },
                    start_position,
                    pointer,
                    radius_squared,
                );
                push_candidate(
                    &mut candidates,
                    SketchSnapKey::Endpoint {
                        entity: entity.id,
                        point: end,
                    },
                    end_position,
                    pointer,
                    radius_squared,
                );
            }
            SketchCurve2::Circle { .. } => {}
        }

        if let Some(center) = curve.center() {
            push_candidate(
                &mut candidates,
                SketchSnapKey::Center { entity: entity.id },
                center,
                pointer,
                radius_squared,
            );
        }
        if !curve.is_periodic()
            && let Ok(midpoint) = curve.evaluate(0.5)
        {
            push_candidate(
                &mut candidates,
                SketchSnapKey::Midpoint { entity: entity.id },
                midpoint,
                pointer,
                radius_squared,
            );
        }
        if let (Some(center), Some(radius_value)) = (curve.center(), curve.radius()) {
            for (index, (u, v)) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)]
                .into_iter()
                .enumerate()
            {
                let expected = SketchPoint2::new(
                    radius_value.mul_add(u, center.u),
                    radius_value.mul_add(v, center.v),
                );
                let parameter = curve.closest_parameter(expected);
                let Ok(actual) = curve.evaluate(parameter) else {
                    continue;
                };
                if actual.distance(expected)
                    <= precision
                        .linear_agreement
                        .max(f64::EPSILON * radius_value * 32.0)
                {
                    push_candidate(
                        &mut candidates,
                        SketchSnapKey::Quadrant {
                            entity: entity.id,
                            index: index as u8,
                        },
                        actual,
                        pointer,
                        radius_squared,
                    );
                }
            }
        }
    }

    for first_index in 0..nearby.len() {
        for second_index in first_index + 1..nearby.len() {
            let (first_entity, first_curve) = nearby[first_index];
            let (second_entity, second_curve) = nearby[second_index];
            let intersections = intersect_entities(
                first_entity.id,
                first_curve,
                second_entity.id,
                second_curve,
                precision,
            );
            if let CurveIntersections::Points {
                intersections: points,
            } = intersections.result
            {
                for (branch, intersection) in points.into_iter().enumerate() {
                    push_candidate(
                        &mut candidates,
                        SketchSnapKey::Intersection {
                            first_entity: intersections.first_entity,
                            second_entity: intersections.second_entity,
                            branch: IntersectionBranch(branch as u16),
                        },
                        intersection.point,
                        pointer,
                        radius_squared,
                    );
                }
            }
        }
    }

    candidates.sort_by(|first, second| {
        first
            .key
            .priority()
            .cmp(&second.key.priority())
            .then_with(|| first.distance_squared.total_cmp(&second.distance_squared))
            .then_with(|| first.key.cmp(&second.key))
    });
    candidates.dedup_by(|second, first| second.key == first.key);
    candidates.truncate(max_candidates);
    candidates
}

fn push_candidate(
    candidates: &mut Vec<SketchSnapCandidate>,
    key: SketchSnapKey,
    point: SketchPoint2,
    pointer: SketchPoint2,
    radius_squared: f64,
) {
    let distance_squared = point.distance_squared(pointer);
    if distance_squared <= radius_squared {
        candidates.push(SketchSnapCandidate {
            key,
            point,
            distance_squared,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfirmationSource, PointInput, SketchRecipe};

    fn point(u: f64, v: f64) -> PointInput {
        PointInput::Position(SketchPoint2::new(u, v))
    }

    fn add(definition: &mut SketchDefinition, recipe: SketchRecipe) {
        let transaction = definition.stage(recipe, "query fixture").unwrap();
        definition
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
    }

    #[test]
    fn hit_testing_returns_exact_curve_parameters() {
        let mut definition = SketchDefinition::new();
        add(
            &mut definition,
            SketchRecipe::Line {
                start: point(0.0, 0.0),
                end: point(10.0, 0.0),
            },
        );
        let hits = hit_test_curves(&definition, SketchPoint2::new(4.0, 0.1), 0.2);
        assert_eq!(hits.len(), 1);
        assert!((hits[0].parameter - 0.4).abs() < 1.0e-12);
        assert_eq!(hits[0].point, SketchPoint2::new(4.0, 0.0));
    }

    #[test]
    fn endpoint_midpoint_and_intersection_candidates_are_typed() {
        let mut definition = SketchDefinition::new();
        add(
            &mut definition,
            SketchRecipe::Line {
                start: point(-2.0, 0.0),
                end: point(2.0, 0.0),
            },
        );
        add(
            &mut definition,
            SketchRecipe::Line {
                start: point(0.0, -2.0),
                end: point(0.0, 2.0),
            },
        );
        let candidates = query_snap_candidates(
            &definition,
            SketchPoint2::new(0.0, 0.0),
            0.1,
            &PrecisionPolicy::default(),
            16,
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| matches!(candidate.key, SketchSnapKey::Intersection { .. }))
        );
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| matches!(candidate.key, SketchSnapKey::Midpoint { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn circle_candidates_include_center_and_four_quadrants() {
        let mut definition = SketchDefinition::new();
        add(
            &mut definition,
            SketchRecipe::TwoPointCircle {
                first_diameter_point: point(-2.0, 0.0),
                second_diameter_point: point(2.0, 0.0),
                direction: crate::CurveDirection::CounterClockwise,
            },
        );
        let center = query_snap_candidates(
            &definition,
            SketchPoint2::new(0.0, 0.0),
            0.01,
            &PrecisionPolicy::default(),
            8,
        );
        assert!(
            center
                .iter()
                .any(|candidate| matches!(candidate.key, SketchSnapKey::Center { .. }))
        );

        for point in [
            SketchPoint2::new(2.0, 0.0),
            SketchPoint2::new(0.0, 2.0),
            SketchPoint2::new(-2.0, 0.0),
            SketchPoint2::new(0.0, -2.0),
        ] {
            let candidates =
                query_snap_candidates(&definition, point, 0.01, &PrecisionPolicy::default(), 8);
            assert!(
                candidates
                    .iter()
                    .any(|candidate| matches!(candidate.key, SketchSnapKey::Quadrant { .. }))
            );
        }
    }

    #[test]
    fn candidate_count_is_bounded_and_deterministic() {
        let mut definition = SketchDefinition::new();
        for offset in 0..4 {
            add(
                &mut definition,
                SketchRecipe::Line {
                    start: point(-1.0, f64::from(offset)),
                    end: point(1.0, f64::from(offset)),
                },
            );
        }
        let first = query_snap_candidates(
            &definition,
            SketchPoint2::new(0.0, 1.5),
            2.0,
            &PrecisionPolicy::default(),
            3,
        );
        let second = query_snap_candidates(
            &definition,
            SketchPoint2::new(0.0, 1.5),
            2.0,
            &PrecisionPolicy::default(),
            3,
        );
        assert_eq!(first, second);
        assert_eq!(first.len(), 3);
    }
}

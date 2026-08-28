//! Exact adjacent-span selection for Trim.

use artificer_protocol::PrecisionPolicy;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CurveIntersections, EvaluatedCurve2, IntersectionBranch, JunctionKey, SketchEntityId,
    SketchPoint2, SourceInterval, intersect_entities,
};

#[derive(Clone, Debug, PartialEq)]
pub struct TrimCurve {
    pub entity: SketchEntityId,
    pub curve: EvaluatedCurve2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrimJunction {
    pub parameter: f64,
    pub key: JunctionKey,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrimFragment {
    pub curve: EvaluatedCurve2,
    pub source_interval: SourceInterval,
    pub start_limit: Option<JunctionKey>,
    pub end_limit: Option<JunctionKey>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrimSpanSelection {
    pub target_entity: SketchEntityId,
    pub click_parameter: f64,
    pub removed: TrimFragment,
    pub retained: Vec<TrimFragment>,
    pub ordered_junctions: Vec<TrimJunction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrimError {
    #[error("trim target geometry is invalid")]
    InvalidTarget,
    #[error("no unique trim span exists because a limit overlaps the target")]
    NoUniqueSpan { limit_entity: SketchEntityId },
    #[error("a target/limit intersection could not be certified")]
    IndeterminateLimit { limit_entity: SketchEntityId },
    #[error("the target has no finite span; delete the entity instead")]
    RecommendDelete,
    #[error("a complete circle requires at least two distinct trim limits")]
    CircleNeedsTwoLimits,
    #[error("the click is on a trim junction; choose an adjacent span")]
    ClickAtJunction { junctions: Vec<JunctionKey> },
    #[error("trim would exceed the configured event limit")]
    EventLimitExceeded { limit: usize },
    #[error("the selected exact subcurve could not be constructed")]
    InvalidSpan,
}

/// Resolves the exact target span beneath `click`. `limits` must already be
/// filtered by the profile/construction/reference role policy of the caller.
pub fn select_trim_span(
    target: TrimCurve,
    limits: &[TrimCurve],
    click: SketchPoint2,
    precision: &PrecisionPolicy,
    max_events: usize,
) -> Result<TrimSpanSelection, TrimError> {
    target
        .curve
        .validate(precision)
        .map_err(|_| TrimError::InvalidTarget)?;
    let mut junctions = Vec::new();
    for limit in limits {
        if limit.entity == target.entity {
            continue;
        }
        let intersections = intersect_entities(
            target.entity,
            target.curve.clone(),
            limit.entity,
            limit.curve.clone(),
            precision,
        );
        match intersections.result {
            CurveIntersections::Disjoint => {}
            CurveIntersections::Points {
                intersections: points,
            } => {
                for (branch, event) in points.into_iter().enumerate() {
                    let parameter = if target.entity <= limit.entity {
                        event.first_parameter
                    } else {
                        event.second_parameter
                    };
                    junctions.push(TrimJunction {
                        parameter: normalize_trim_parameter(parameter, target.curve.is_periodic()),
                        key: JunctionKey::Intersection {
                            first_entity: target.entity.min(limit.entity),
                            second_entity: target.entity.max(limit.entity),
                            branch: IntersectionBranch(branch as u16),
                        },
                    });
                    if junctions.len() > max_events {
                        return Err(TrimError::EventLimitExceeded { limit: max_events });
                    }
                }
            }
            CurveIntersections::CoincidentFull | CurveIntersections::Overlap { .. } => {
                return Err(TrimError::NoUniqueSpan {
                    limit_entity: limit.entity,
                });
            }
            CurveIntersections::Indeterminate { .. } => {
                return Err(TrimError::IndeterminateLimit {
                    limit_entity: limit.entity,
                });
            }
        }
    }

    if junctions.is_empty() {
        return Err(if target.curve.is_periodic() {
            TrimError::CircleNeedsTwoLimits
        } else {
            TrimError::RecommendDelete
        });
    }

    junctions.sort_by(|first, second| {
        first
            .parameter
            .total_cmp(&second.parameter)
            .then_with(|| first.key.cmp(&second.key))
    });

    let parameter_tolerance = parameter_uncertainty(&target.curve, precision);
    let mut clustered = Vec::<TrimJunction>::with_capacity(junctions.len());
    for junction in junctions {
        if let Some(last) = clustered.last_mut()
            && (junction.parameter - last.parameter).abs() <= parameter_tolerance
        {
            // The first canonical branch key survives clustering. When
            // intersecting lines produce identical parameter positions,
            // the complete arrangement retains the full cluster key.
            if junction.key < last.key {
                last.key = junction.key;
            }
            continue;
        }
        clustered.push(junction);
    }
    if target.curve.is_periodic()
        && clustered.len() > 1
        && parameter_distance(
            clustered[0].parameter,
            clustered.last().expect("non-empty").parameter,
            true,
        ) <= parameter_tolerance
    {
        let last = clustered.pop().expect("len > 1");
        if last.key < clustered[0].key {
            clustered[0].key = last.key;
        }
    }

    if target.curve.is_periodic() {
        if clustered.len() < 2 {
            return Err(TrimError::CircleNeedsTwoLimits);
        }
    } else if clustered.is_empty() {
        return Err(TrimError::RecommendDelete);
    }

    let click_parameter = target.curve.closest_parameter(click);
    let uncertainty = parameter_uncertainty(&target.curve, precision);
    let target_periodic = target.curve.is_periodic();
    let at_click: Vec<_> = clustered
        .iter()
        .filter(|junction| {
            parameter_distance(
                click_parameter,
                junction.parameter,
                target_periodic,
            ) <= uncertainty
        })
        .map(|junction| junction.key.clone())
        .collect();
    if !at_click.is_empty() {
        return Err(TrimError::ClickAtJunction {
            junctions: at_click,
        });
    }

    let spans = if target.curve.is_periodic() {
        periodic_spans(&clustered)
    } else {
        open_spans(&clustered)
    };
    let removed_index = spans
        .iter()
        .position(|span| span_contains(*span, click_parameter, parameter_tolerance))
        .ok_or(TrimError::InvalidSpan)?;
    let mut fragments = Vec::with_capacity(spans.len());
    for span in &spans {
        fragments.push(fragment_for_span(&target.curve, *span, &clustered)?);
    }
    let removed = fragments.remove(removed_index);
    Ok(TrimSpanSelection {
        target_entity: target.entity,
        click_parameter,
        removed,
        retained: fragments,
        ordered_junctions: clustered,
    })
}

#[derive(Clone, Copy, Debug)]
struct TrimSpan {
    start: f64,
    end: f64,
    start_limit: Option<usize>,
    end_limit: Option<usize>,
    wraps: bool,
}

impl TrimSpan {
    const fn new(start: f64, end: f64, start_limit: usize, end_limit: usize) -> Self {
        Self {
            start,
            end,
            start_limit: Some(start_limit),
            end_limit: Some(end_limit),
            wraps: false,
        }
    }
}

fn open_spans(junctions: &[TrimJunction]) -> Vec<TrimSpan> {
    let mut spans = Vec::with_capacity(junctions.len() + 1);
    if let Some(first) = junctions.first()
        && first.parameter > 0.0
    {
        spans.push(TrimSpan {
            start: 0.0,
            end: first.parameter,
            start_limit: None,
            end_limit: Some(0),
            wraps: false,
        });
    }
    for index in 0..junctions.len().saturating_sub(1) {
        spans.push(TrimSpan::new(
            junctions[index].parameter,
            junctions[index + 1].parameter,
            index,
            index + 1,
        ));
    }
    if let Some(last) = junctions.last()
        && last.parameter < 1.0
    {
        spans.push(TrimSpan {
            start: last.parameter,
            end: 1.0,
            start_limit: Some(junctions.len() - 1),
            end_limit: None,
            wraps: false,
        });
    }
    spans
}

fn periodic_spans(junctions: &[TrimJunction]) -> Vec<TrimSpan> {
    let mut spans = Vec::with_capacity(junctions.len());
    for index in 0..junctions.len() {
        let next_index = (index + 1) % junctions.len();
        let wraps = index + 1 == junctions.len();
        spans.push(TrimSpan {
            start: junctions[index].parameter,
            end: junctions[next_index].parameter,
            start_limit: Some(index),
            end_limit: Some(next_index),
            wraps,
        });
    }
    spans
}

fn fragment_for_span(
    curve: &EvaluatedCurve2,
    span: TrimSpan,
    junctions: &[TrimJunction],
) -> Result<TrimFragment, TrimError> {
    let result = if span.wraps {
        let start = curve
            .evaluate(span.start)
            .map_err(|_| TrimError::InvalidSpan)?;
        let end = curve
            .evaluate(span.end)
            .map_err(|_| TrimError::InvalidSpan)?;
        let (center, direction) = match curve {
            EvaluatedCurve2::Circle {
                center, direction, ..
            } => (*center, *direction),
            _ => return Err(TrimError::InvalidSpan),
        };
        EvaluatedCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        }
    } else {
        curve
            .subcurve(span.start, span.end)
            .map_err(|_| TrimError::InvalidSpan)?
    };
    Ok(TrimFragment {
        curve: result,
        source_interval: SourceInterval {
            start: span.start,
            end: span.end,
            wraps_periodic_seam: span.wraps,
        },
        start_limit: span.start_limit.map(|index| junctions[index].key.clone()),
        end_limit: span.end_limit.map(|index| junctions[index].key.clone()),
    })
}

fn span_contains(span: TrimSpan, parameter: f64, tolerance: f64) -> bool {
    if span.wraps {
        parameter > span.start + tolerance || parameter < span.end - tolerance
    } else {
        (span.start == 0.0 || parameter > span.start + tolerance)
            && (span.end == 1.0 || parameter < span.end - tolerance)
            && parameter >= span.start - tolerance
            && parameter <= span.end + tolerance
    }
}

fn parameter_uncertainty(curve: &EvaluatedCurve2, precision: &PrecisionPolicy) -> f64 {
    let length = curve.arc_length().max(precision.min_feature_size);
    precision
        .parameter_resolution
        .max(precision.modeling_resolution / length)
}

fn normalize_trim_parameter(parameter: f64, periodic: bool) -> f64 {
    if periodic {
        parameter.rem_euclid(1.0)
    } else {
        parameter.clamp(0.0, 1.0)
    }
}

fn parameter_distance(first: f64, second: f64, periodic: bool) -> f64 {
    let direct = (first - second).abs();
    if periodic {
        direct.min(1.0 - direct)
    } else {
        direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CurveDirection;

    fn eid(raw: u64) -> SketchEntityId {
        SketchEntityId::new(raw).unwrap()
    }

    fn line(entity: u64, start: (f64, f64), end: (f64, f64)) -> TrimCurve {
        TrimCurve {
            entity: eid(entity),
            curve: EvaluatedCurve2::Line {
                start: SketchPoint2::new(start.0, start.1),
                end: SketchPoint2::new(end.0, end.1),
            },
        }
    }

    #[test]
    fn twice_crossed_line_selects_only_middle_span() {
        let precision = PrecisionPolicy::default();
        let target = line(1, (-3.0, 0.0), (3.0, 0.0));
        let limits = [
            line(2, (-1.0, -1.0), (-1.0, 1.0)),
            line(3, (1.0, -1.0), (1.0, 1.0)),
        ];
        let selection =
            select_trim_span(target, &limits, SketchPoint2::new(0.0, 0.0), &precision, 64).unwrap();
        assert_eq!(selection.retained.len(), 2);
        assert!((selection.removed.source_interval.start - 1.0 / 3.0).abs() < 1.0e-9);
        assert!((selection.removed.source_interval.end - 2.0 / 3.0).abs() < 1.0e-9);
    }

    #[test]
    fn periodic_circle_selects_wraparound_span() {
        let precision = PrecisionPolicy::default();
        let target = TrimCurve {
            entity: eid(1),
            curve: EvaluatedCurve2::Circle {
                center: SketchPoint2::new(0.0, 0.0),
                radius: 2.0,
                direction: CurveDirection::CounterClockwise,
            },
        };
        let limits = [line(2, (0.0, -3.0), (0.0, 3.0))];
        let selection =
            select_trim_span(target, &limits, SketchPoint2::new(2.0, 0.0), &precision, 64).unwrap();
        assert!(selection.removed.source_interval.wraps_periodic_seam);
        assert_eq!(selection.retained.len(), 1);
    }

    #[test]
    fn circle_with_one_limit_recommends_another_limit() {
        let precision = PrecisionPolicy::default();
        let target = TrimCurve {
            entity: eid(1),
            curve: EvaluatedCurve2::Circle {
                center: SketchPoint2::new(0.0, 0.0),
                radius: 2.0,
                direction: CurveDirection::CounterClockwise,
            },
        };
        let tangent = [line(2, (-3.0, 2.0), (3.0, 2.0))];
        assert!(matches!(
            select_trim_span(
                target,
                &tangent,
                SketchPoint2::new(2.0, 0.0),
                &precision,
                64
            ),
            Err(TrimError::CircleNeedsTwoLimits)
        ));
    }
}

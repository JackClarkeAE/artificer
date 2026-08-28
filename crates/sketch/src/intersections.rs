//! Symmetric analytic intersections for line, circular-arc, and circle uses.
//!
//! The routines in this module never sample curves.  Results carry normalized
//! parameters on both operands and retain overlap/coincidence as explicit
//! outcomes so callers cannot accidentally treat them as unique junctions.

use artificer_protocol::PrecisionPolicy;
use serde::{Deserialize, Serialize};

use crate::{
    EvaluatedCurve2, SketchEntityId, SketchPoint2, SketchPointId, angle_of, angle_on_directed_arc,
    parameter_for_arc_angle, parameter_for_circle_angle,
};

/// Deterministic solution index after canonical operand ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntersectionBranch(pub u16);

/// Semantic identity of an arrangement junction.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum JunctionKey {
    Endpoint(SketchPointId),
    Intersection {
        first_entity: SketchEntityId,
        second_entity: SketchEntityId,
        branch: IntersectionBranch,
    },
    /// The synthetic antipode a circle receives when exactly one authored
    /// junction lies on it. A closed curve needs two junctions to become two
    /// non-degenerate fragments; this one is derived from the single real
    /// event, so it is as stable as that event.
    PeriodicSplit {
        source_entity: SketchEntityId,
    },
}

/// A canonical set of semantic events which evaluate to one point.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JunctionClusterKey {
    keys: Vec<JunctionKey>,
}

impl JunctionClusterKey {
    #[must_use]
    pub fn new(mut keys: Vec<JunctionKey>) -> Option<Self> {
        keys.sort();
        keys.dedup();
        (!keys.is_empty()).then_some(Self { keys })
    }

    #[must_use]
    pub fn keys(&self) -> &[JunctionKey] {
        &self.keys
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntersectionClass {
    ProperCrossing,
    Tangent,
    EndpointEndpoint,
    EndpointInterior,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurveIntersection {
    pub point: SketchPoint2,
    pub first_parameter: f64,
    pub second_parameter: f64,
    pub class: IntersectionClass,
    /// True when the two carrier tangents are parallel at this event. Kept
    /// separate from endpoint ownership so G1 endpoint joins can be accepted
    /// while an interior kissing contact remains diagnosable.
    pub is_tangent: bool,
}

impl CurveIntersection {
    #[must_use]
    pub fn reversed(self) -> Self {
        Self {
            point: self.point,
            first_parameter: self.second_parameter,
            second_parameter: self.first_parameter,
            class: self.class,
            is_tangent: self.is_tangent,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParameterInterval {
    pub start: f64,
    pub end: f64,
}

impl ParameterInterval {
    #[must_use]
    pub fn ordered(first: f64, second: f64) -> Self {
        Self {
            start: first.min(second),
            end: first.max(second),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlapInterval {
    pub first: ParameterInterval,
    pub second: ParameterInterval,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CurveIntersections {
    Disjoint,
    Points {
        intersections: Vec<CurveIntersection>,
    },
    Overlap {
        intervals: Vec<OverlapInterval>,
    },
    CoincidentFull,
    Indeterminate {
        reason: IntersectionIndeterminate,
    },
}

impl CurveIntersections {
    #[must_use]
    pub fn reversed(self) -> Self {
        match self {
            Self::Points { intersections } => Self::Points {
                intersections: {
                    let mut reversed: Vec<_> = intersections
                        .into_iter()
                        .map(CurveIntersection::reversed)
                        .collect();
                    reversed.sort_by(|first, second| {
                        first
                            .first_parameter
                            .total_cmp(&second.first_parameter)
                            .then_with(|| {
                                first.second_parameter.total_cmp(&second.second_parameter)
                            })
                            .then_with(|| first.point.total_cmp(&second.point))
                    });
                    reversed
                },
            },
            Self::Overlap { intervals } => Self::Overlap {
                intervals: intervals
                    .into_iter()
                    .map(|interval| OverlapInterval {
                        first: interval.second,
                        second: interval.first,
                    })
                    .collect(),
            },
            other => other,
        }
    }

    #[must_use]
    pub fn unique_points(&self) -> &[CurveIntersection] {
        match self {
            Self::Points { intersections } => intersections,
            _ => &[],
        }
    }

    #[must_use]
    pub fn has_non_unique_contact(&self) -> bool {
        matches!(self, Self::Overlap { .. } | Self::CoincidentFull)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntersectionIndeterminate {
    NonFiniteInput,
    DegenerateInput,
    UncertifiedParallelism,
    UncertifiedDiscriminant,
    UncertifiedCarrierRelation,
    EventBudgetExceeded,
}

/// Intersection result whose operands are always in entity-ID order. Stable
/// branches are assigned only after this canonicalization.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalEntityIntersections {
    pub first_entity: SketchEntityId,
    pub second_entity: SketchEntityId,
    pub result: CurveIntersections,
}

impl CanonicalEntityIntersections {
    #[must_use]
    pub fn junction_keys(&self) -> Vec<(JunctionKey, CurveIntersection)> {
        self.result
            .unique_points()
            .iter()
            .copied()
            .enumerate()
            .map(|(index, intersection)| {
                (
                    JunctionKey::Intersection {
                        first_entity: self.first_entity,
                        second_entity: self.second_entity,
                        branch: IntersectionBranch(index as u16),
                    },
                    intersection,
                )
            })
            .collect()
    }
}

#[must_use]
pub fn intersect_entities(
    first_entity: SketchEntityId,
    first: EvaluatedCurve2,
    second_entity: SketchEntityId,
    second: EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> CanonicalEntityIntersections {
    if first_entity <= second_entity {
        CanonicalEntityIntersections {
            first_entity,
            second_entity,
            result: intersect_curves(first, second, precision),
        }
    } else {
        CanonicalEntityIntersections {
            first_entity: second_entity,
            second_entity: first_entity,
            result: intersect_curves(second, first, precision),
        }
    }
}

/// Intersects two bounded curve uses. Returned points are deterministically
/// ordered by the first parameter, then second parameter and coordinates.
#[must_use]
pub fn intersect_curves(
    first: EvaluatedCurve2,
    second: EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    if first.validate(precision).is_err() || second.validate(precision).is_err() {
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::DegenerateInput,
        };
    }
    if !first
        .bounds()
        .expanded(linear_tolerance(precision, &first, &second))
        .intersects(
            second
                .bounds()
                .expanded(linear_tolerance(precision, &first, &second)),
        )
    {
        return CurveIntersections::Disjoint;
    }

    let mut result = match (&first, &second) {
        (
            EvaluatedCurve2::Line {
                start: first_start,
                end: first_end,
            },
            EvaluatedCurve2::Line {
                start: second_start,
                end: second_end,
            },
        ) => intersect_line_line(
            *first_start,
            *first_end,
            *second_start,
            *second_end,
            precision,
        ),
        (
            EvaluatedCurve2::Line { start, end },
            circular @ (EvaluatedCurve2::CircularArc { .. } | EvaluatedCurve2::Circle { .. }),
        ) => intersect_line_circular(*start, *end, circular.clone(), precision),
        (
            circular @ (EvaluatedCurve2::CircularArc { .. } | EvaluatedCurve2::Circle { .. }),
            EvaluatedCurve2::Line { start, end },
        ) => intersect_line_circular(*start, *end, circular.clone(), precision).reversed(),
        (
            first_circular @ (EvaluatedCurve2::CircularArc { .. } | EvaluatedCurve2::Circle { .. }),
            second_circular
            @ (EvaluatedCurve2::CircularArc { .. } | EvaluatedCurve2::Circle { .. }),
        ) => {
            intersect_circular_circular(first_circular.clone(), second_circular.clone(), precision)
        }
        _ => intersect_general_curves(&first, &second, precision),
    };
    canonicalize_points(&mut result, precision);
    result
}

fn intersect_general_curves(
    first: &EvaluatedCurve2,
    second: &EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    let n_samples = 32;
    let m_samples = 32;
    let mut intersections = Vec::new();

    for i in 0..n_samples {
        let t1_start = i as f64 / n_samples as f64;
        let t1_end = (i + 1) as f64 / n_samples as f64;
        let Ok(p1_start) = first.evaluate(t1_start) else {
            continue;
        };
        let Ok(p1_end) = first.evaluate(t1_end) else {
            continue;
        };

        for j in 0..m_samples {
            let t2_start = j as f64 / m_samples as f64;
            let t2_end = (j + 1) as f64 / m_samples as f64;
            let Ok(p2_start) = second.evaluate(t2_start) else {
                continue;
            };
            let Ok(p2_end) = second.evaluate(t2_end) else {
                continue;
            };

            let tol = precision.linear_agreement;
            let b1 = crate::geometry::Aabb2::from_points(p1_start, p1_end).expanded(tol);
            let b2 = crate::geometry::Aabb2::from_points(p2_start, p2_end).expanded(tol);
            if !b1.intersects(b2) {
                continue;
            }

            let v1 = p1_end - p1_start;
            let v2 = p2_end - p2_start;
            let denom = v1.cross(v2);
            if denom.abs() < 1.0e-12 {
                continue;
            }
            let s1 = (p2_start - p1_start).cross(v2) / denom;
            let s2 = (p2_start - p1_start).cross(v1) / denom;
            if !(0.0..=1.0).contains(&s1) || !(0.0..=1.0).contains(&s2) {
                continue;
            }

            let mut u = t1_start + s1 * (t1_end - t1_start);
            let mut v = t2_start + s2 * (t2_end - t2_start);

            for _ in 0..10 {
                let Ok(pt1) = first.evaluate(u) else { break };
                let Ok(pt2) = second.evaluate(v) else { break };
                let delta = pt1 - pt2;
                if delta.length() < precision.linear_agreement {
                    break;
                }
                let Ok(tan1) = first.tangent(u) else { break };
                let Ok(tan2) = second.tangent(v) else { break };
                let det = -tan1.u * tan2.v + tan1.v * tan2.u;
                if det.abs() < 1.0e-14 {
                    break;
                }
                let du = (-tan2.v * delta.u + tan2.u * delta.v) / det;
                let dv = (-tan1.v * delta.u + tan1.u * delta.v) / det;
                u = (u - du).clamp(0.0, 1.0);
                v = (v - dv).clamp(0.0, 1.0);
            }

            let Ok(final_pt1) = first.evaluate(u) else {
                continue;
            };
            let Ok(final_pt2) = second.evaluate(v) else {
                continue;
            };
            if final_pt1.distance(final_pt2) <= precision.linear_agreement * 2.0 {
                let point = final_pt1;
                if !intersections.iter().any(|ix: &CurveIntersection| {
                    (ix.first_parameter - u).abs() < 1.0e-4
                        && (ix.second_parameter - v).abs() < 1.0e-4
                }) {
                    let is_tangent = if let (Ok(t1), Ok(t2)) = (first.tangent(u), second.tangent(v))
                    {
                        if let (Some(n1), Some(n2)) = (t1.normalized(), t2.normalized()) {
                            n1.cross(n2).abs() < 1.0e-3
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    intersections.push(CurveIntersection {
                        point,
                        first_parameter: u,
                        second_parameter: v,
                        class: contact_class(
                            u,
                            v,
                            is_tangent,
                            first.is_periodic(),
                            second.is_periodic(),
                            precision,
                        ),
                        is_tangent,
                    });
                }
            }
        }
    }

    if intersections.is_empty() {
        CurveIntersections::Disjoint
    } else {
        CurveIntersections::Points { intersections }
    }
}

fn intersect_line_line(
    first_start: SketchPoint2,
    first_end: SketchPoint2,
    second_start: SketchPoint2,
    second_end: SketchPoint2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    let first_vector = first_end - first_start;
    let second_vector = second_end - second_start;
    let offset = second_start - first_start;
    let denominator = first_vector.cross(second_vector);
    let determinant_uncertainty = f64::EPSILON
        * (first_vector.u * second_vector.v)
            .abs()
            .max((first_vector.v * second_vector.u).abs())
            .max(1.0)
        * 32.0;

    if denominator.abs() <= determinant_uncertainty {
        let carrier_distance_numerator = offset.cross(first_vector).abs();
        let carrier_uncertainty = f64::EPSILON
            * (offset.u * first_vector.v)
                .abs()
                .max((offset.v * first_vector.u).abs())
                .max(1.0)
            * 32.0;
        if carrier_distance_numerator > carrier_uncertainty {
            return CurveIntersections::Disjoint;
        }
        if denominator != 0.0 || carrier_distance_numerator != 0.0 {
            return CurveIntersections::Indeterminate {
                reason: IntersectionIndeterminate::UncertifiedParallelism,
            };
        }
        return collinear_line_overlap(first_start, first_end, second_start, second_end, precision);
    }

    let first_parameter = offset.cross(second_vector) / denominator;
    let second_parameter = offset.cross(first_vector) / denominator;
    let parameter_tolerance = precision.parameter_resolution.max(f64::EPSILON * 32.0);
    if !within_unit(first_parameter, parameter_tolerance)
        || !within_unit(second_parameter, parameter_tolerance)
    {
        return CurveIntersections::Disjoint;
    }
    let first_parameter = snap_unit(first_parameter, parameter_tolerance);
    let second_parameter = snap_unit(second_parameter, parameter_tolerance);
    let point = first_start + first_vector * first_parameter;
    CurveIntersections::Points {
        intersections: vec![CurveIntersection {
            point,
            first_parameter,
            second_parameter,
            class: contact_class(
                first_parameter,
                second_parameter,
                false,
                false,
                false,
                precision,
            ),
            is_tangent: false,
        }],
    }
}

fn collinear_line_overlap(
    first_start: SketchPoint2,
    first_end: SketchPoint2,
    second_start: SketchPoint2,
    second_end: SketchPoint2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    let first_vector = first_end - first_start;
    let denominator = first_vector.length_squared();
    if denominator == 0.0 {
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::DegenerateInput,
        };
    }
    let second_on_first_start = (second_start - first_start).dot(first_vector) / denominator;
    let second_on_first_end = (second_end - first_start).dot(first_vector) / denominator;
    let overlap_start = second_on_first_start.min(second_on_first_end).max(0.0);
    let overlap_end = second_on_first_start.max(second_on_first_end).min(1.0);
    let parameter_tolerance = precision.parameter_resolution.max(f64::EPSILON * 32.0);
    if overlap_end < overlap_start - parameter_tolerance {
        return CurveIntersections::Disjoint;
    }
    if (overlap_end - overlap_start).abs() <= parameter_tolerance {
        let first_parameter = snap_unit((overlap_start + overlap_end) * 0.5, parameter_tolerance);
        let point = first_start + first_vector * first_parameter;
        let second_vector = second_end - second_start;
        let second_parameter = ((point - second_start).dot(second_vector)
            / second_vector.length_squared())
        .clamp(0.0, 1.0);
        return CurveIntersections::Points {
            intersections: vec![CurveIntersection {
                point,
                first_parameter,
                second_parameter: snap_unit(second_parameter, parameter_tolerance),
                class: contact_class(
                    first_parameter,
                    second_parameter,
                    false,
                    false,
                    false,
                    precision,
                ),
                is_tangent: false,
            }],
        };
    }

    let start_point = first_start + first_vector * overlap_start;
    let end_point = first_start + first_vector * overlap_end;
    let second_vector = second_end - second_start;
    let second_denominator = second_vector.length_squared();
    let second_a = (start_point - second_start).dot(second_vector) / second_denominator;
    let second_b = (end_point - second_start).dot(second_vector) / second_denominator;
    CurveIntersections::Overlap {
        intervals: vec![OverlapInterval {
            first: ParameterInterval::ordered(overlap_start, overlap_end),
            second: ParameterInterval::ordered(second_a, second_b),
        }],
    }
}

fn intersect_line_circular(
    line_start: SketchPoint2,
    line_end: SketchPoint2,
    circular: EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    let Some(center) = circular.center() else {
        unreachable!("caller provides a circular curve");
    };
    let Some(radius) = circular.radius() else {
        unreachable!("caller provides a circular curve");
    };
    let direction = line_end - line_start;
    let offset = line_start - center;
    let a = direction.length_squared();
    let b = 2.0 * offset.dot(direction);
    let c = offset.length_squared() - radius * radius;
    let discriminant = b.mul_add(b, -4.0 * a * c);
    if !discriminant.is_finite() {
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::NonFiniteInput,
        };
    }
    let scale = (b * b).abs().max((4.0 * a * c).abs()).max(1.0);
    let discriminant_roundoff = f64::EPSILON * scale * 128.0;
    // Near tangency, changing the perpendicular line/carrier separation by
    // `delta` changes the quadratic discriminant by approximately
    // `8 * |d|^2 * radius * delta`.
    let resolution_band = linear_tolerance_values(precision, radius.max(direction.length()))
        * a
        * radius.max(1.0)
        * 8.0;
    if discriminant != 0.0 && discriminant.abs() <= discriminant_roundoff.max(resolution_band) {
        // A rigid/similarity transform can perturb the carrier discriminant
        // even though an authored line and arc still share the exact same
        // endpoint coordinate.  That endpoint identity certifies the
        // coalesced tangent root without weakening the deliberately
        // indeterminate policy for merely near-tangent, unrelated carriers.
        if let Some(intersection) =
            certified_line_arc_tangent_endpoint(line_start, line_end, circular, precision)
        {
            return CurveIntersections::Points {
                intersections: vec![intersection],
            };
        }
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::UncertifiedDiscriminant,
        };
    }
    if discriminant < 0.0 {
        return CurveIntersections::Disjoint;
    }
    let tangent = discriminant == 0.0;
    let roots: Vec<f64> = if tangent {
        vec![-b / (2.0 * a)]
    } else {
        let root = discriminant.sqrt();
        // The compensated quadratic form avoids losing the smaller root.
        let q = -0.5 * (b + root.copysign(b));
        if q == 0.0 {
            vec![(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)]
        } else {
            vec![q / a, c / q]
        }
    };

    let parameter_tolerance = precision.parameter_resolution.max(f64::EPSILON * 64.0);
    let mut intersections = Vec::new();
    for root in roots {
        if !within_unit(root, parameter_tolerance) {
            continue;
        }
        let line_parameter = snap_unit(root, parameter_tolerance);
        let point = line_start + direction * line_parameter;
        let Some(circular_parameter) = parameter_on_circular(&circular, point, precision) else {
            continue;
        };
        intersections.push(CurveIntersection {
            point,
            first_parameter: line_parameter,
            second_parameter: circular_parameter,
            class: contact_class(
                line_parameter,
                circular_parameter,
                tangent,
                false,
                circular.is_periodic(),
                precision,
            ),
            is_tangent: tangent,
        });
    }
    points_or_disjoint(intersections)
}

fn certified_line_arc_tangent_endpoint(
    line_start: SketchPoint2,
    line_end: SketchPoint2,
    circular: EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> Option<CurveIntersection> {
    let EvaluatedCurve2::CircularArc {
        center,
        start: arc_start,
        end: arc_end,
        direction,
    } = circular
    else {
        return None;
    };
    let candidates = [
        (line_start, 0.0, arc_start, 0.0),
        (line_start, 0.0, arc_end, 1.0),
        (line_end, 1.0, arc_start, 0.0),
        (line_end, 1.0, arc_end, 1.0),
    ];
    let line_tangent = line_end - line_start;
    for (line_point, line_parameter, arc_point, arc_parameter) in candidates {
        if line_point != arc_point {
            continue;
        }
        let radial = arc_point - center;
        let arc_tangent = radial.left_normal()
            * match direction {
                crate::CurveDirection::CounterClockwise => 1.0,
                crate::CurveDirection::Clockwise => -1.0,
            };
        let denominator = line_tangent.length() * arc_tangent.length();
        if denominator == 0.0 {
            return None;
        }
        let normalized_cross = line_tangent.cross(arc_tangent).abs() / denominator;
        let angular_tolerance = precision
            .angular_agreement_radians
            .max(f64::EPSILON * 128.0);
        if normalized_cross <= angular_tolerance {
            return Some(CurveIntersection {
                point: line_point,
                first_parameter: line_parameter,
                second_parameter: arc_parameter,
                class: IntersectionClass::EndpointEndpoint,
                is_tangent: true,
            });
        }
    }
    None
}

fn intersect_circular_circular(
    first: EvaluatedCurve2,
    second: EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    let first_center = first.center().expect("circular curve");
    let second_center = second.center().expect("circular curve");
    let first_radius = first.radius().expect("circular curve");
    let second_radius = second.radius().expect("circular curve");
    let center_delta = second_center - first_center;
    let distance = center_delta.length();
    let tolerance =
        linear_tolerance_values(precision, first_radius.max(second_radius).max(distance));
    let arithmetic_carrier_uncertainty = f64::EPSILON
        * first_radius
            .max(second_radius)
            .max(first_center.u.abs())
            .max(first_center.v.abs())
            .max(second_center.u.abs())
            .max(second_center.v.abs())
            .max(1.0)
        * 64.0;

    if first_center == second_center
        && (first_radius - second_radius).abs() <= arithmetic_carrier_uncertainty
    {
        return coincident_circular_overlap(first, second, precision);
    }
    if distance <= tolerance {
        if first_center == second_center && (first_radius - second_radius).abs() > tolerance {
            return CurveIntersections::Disjoint;
        }
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::UncertifiedCarrierRelation,
        };
    }
    let external_gap = distance - (first_radius + second_radius);
    let internal_gap = distance - (first_radius - second_radius).abs();
    if (external_gap != 0.0 && external_gap.abs() <= tolerance)
        || (internal_gap != 0.0 && internal_gap.abs() <= tolerance)
    {
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::UncertifiedCarrierRelation,
        };
    }
    if external_gap > 0.0 || internal_gap < 0.0 {
        return CurveIntersections::Disjoint;
    }

    let along = (first_radius * first_radius - second_radius * second_radius + distance * distance)
        / (2.0 * distance);
    let height_squared = first_radius.mul_add(first_radius, -(along * along));
    let height_tolerance = tolerance * first_radius.max(1.0) * 4.0;
    if height_squared != 0.0 && height_squared.abs() <= height_tolerance {
        return CurveIntersections::Indeterminate {
            reason: IntersectionIndeterminate::UncertifiedDiscriminant,
        };
    }
    if height_squared < 0.0 {
        return CurveIntersections::Disjoint;
    }
    let tangent = height_squared == 0.0;
    let unit = center_delta / distance;
    let base = first_center + unit * along;
    let candidates = if tangent {
        vec![base]
    } else {
        let perpendicular = unit.left_normal() * height_squared.sqrt();
        vec![base + perpendicular, base + -perpendicular]
    };

    let mut intersections = Vec::new();
    for point in candidates {
        let Some(first_parameter) = parameter_on_circular(&first, point, precision) else {
            continue;
        };
        let Some(second_parameter) = parameter_on_circular(&second, point, precision) else {
            continue;
        };
        intersections.push(CurveIntersection {
            point,
            first_parameter,
            second_parameter,
            class: contact_class(
                first_parameter,
                second_parameter,
                tangent,
                first.is_periodic(),
                second.is_periodic(),
                precision,
            ),
            is_tangent: tangent,
        });
    }
    points_or_disjoint(intersections)
}

fn coincident_circular_overlap(
    first: EvaluatedCurve2,
    second: EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> CurveIntersections {
    if first.is_periodic() && second.is_periodic() {
        return CurveIntersections::CoincidentFull;
    }

    // Build intervals by cutting the shared carrier at both arcs' endpoints.
    let mut first_cuts = vec![0.0, 1.0];
    let mut second_cuts = vec![0.0, 1.0];
    if let Some((start, end)) = second.endpoints() {
        if let Some(parameter) = parameter_on_circular(&first, start, precision) {
            first_cuts.push(parameter);
        }
        if let Some(parameter) = parameter_on_circular(&first, end, precision) {
            first_cuts.push(parameter);
        }
    }
    if let Some((start, end)) = first.endpoints() {
        if let Some(parameter) = parameter_on_circular(&second, start, precision) {
            second_cuts.push(parameter);
        }
        if let Some(parameter) = parameter_on_circular(&second, end, precision) {
            second_cuts.push(parameter);
        }
    }
    sort_dedup_parameters(&mut first_cuts, precision.parameter_resolution);
    sort_dedup_parameters(&mut second_cuts, precision.parameter_resolution);

    let mut intervals = Vec::new();
    for pair in first_cuts.windows(2) {
        if pair[1] - pair[0] <= precision.parameter_resolution {
            continue;
        }
        let midpoint = (pair[0] + pair[1]) * 0.5;
        let eval_parameter = if first.is_periodic() && midpoint == 1.0 {
            0.0
        } else {
            midpoint
        };
        let Ok(point) = first.evaluate(eval_parameter) else {
            continue;
        };
        let Some(second_midpoint) = parameter_on_circular(&second, point, precision) else {
            continue;
        };
        let start_point = evaluate_allow_seam(&first, pair[0]);
        let end_point = evaluate_allow_seam(&first, pair[1]);
        let (Ok(start_point), Ok(end_point)) = (start_point, end_point) else {
            continue;
        };
        let Some(second_start) = parameter_on_circular(&second, start_point, precision) else {
            continue;
        };
        let Some(second_end) = parameter_on_circular(&second, end_point, precision) else {
            continue;
        };
        if second_midpoint >= -precision.parameter_resolution
            && second_midpoint <= 1.0 + precision.parameter_resolution
        {
            intervals.push(OverlapInterval {
                first: ParameterInterval::ordered(pair[0], pair[1]),
                second: ParameterInterval::ordered(second_start, second_end),
            });
        }
    }
    if intervals.is_empty() {
        // Coincident carriers can still touch at one arc endpoint.
        let mut points = Vec::new();
        if let Some((start, end)) = first.endpoints() {
            for (parameter, point) in [(0.0, start), (1.0, end)] {
                if let Some(other_parameter) = parameter_on_circular(&second, point, precision) {
                    points.push(CurveIntersection {
                        point,
                        first_parameter: parameter,
                        second_parameter: other_parameter,
                        class: contact_class(
                            parameter,
                            other_parameter,
                            true,
                            first.is_periodic(),
                            second.is_periodic(),
                            precision,
                        ),
                        is_tangent: true,
                    });
                }
            }
        }
        points_or_disjoint(points)
    } else {
        CurveIntersections::Overlap { intervals }
    }
}

fn parameter_on_circular(
    curve: &EvaluatedCurve2,
    point: SketchPoint2,
    precision: &PrecisionPolicy,
) -> Option<f64> {
    match curve {
        EvaluatedCurve2::Circle {
            center, direction, ..
        } => Some(parameter_for_circle_angle(
            angle_of(point - *center),
            *direction,
        )),
        EvaluatedCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let start_angle = angle_of(*start - *center);
            let end_angle = angle_of(*end - *center);
            let angle = angle_of(point - *center);
            let radius = center.distance(*start).max(precision.min_feature_size);
            let angular_tolerance = precision
                .angular_agreement_radians
                .max(precision.modeling_resolution / radius);
            if angle_on_directed_arc(angle, start_angle, end_angle, *direction, angular_tolerance) {
                let parameter = parameter_for_arc_angle(angle, start_angle, end_angle, *direction);
                Some(snap_unit(
                    parameter,
                    precision.parameter_resolution.max(f64::EPSILON * 64.0),
                ))
            } else {
                None
            }
        }
        EvaluatedCurve2::Line { .. } | EvaluatedCurve2::Bspline { .. } => None,
    }
}

fn evaluate_allow_seam(
    curve: &EvaluatedCurve2,
    parameter: f64,
) -> Result<SketchPoint2, crate::CurveGeometryError> {
    curve.evaluate(if curve.is_periodic() && parameter == 1.0 {
        0.0
    } else {
        parameter
    })
}

fn contact_class(
    first_parameter: f64,
    second_parameter: f64,
    tangent: bool,
    first_periodic: bool,
    second_periodic: bool,
    precision: &PrecisionPolicy,
) -> IntersectionClass {
    let tolerance = precision.parameter_resolution.max(f64::EPSILON * 64.0);
    let first_endpoint = !first_periodic && parameter_is_endpoint(first_parameter, tolerance);
    let second_endpoint = !second_periodic && parameter_is_endpoint(second_parameter, tolerance);
    if first_endpoint && second_endpoint {
        IntersectionClass::EndpointEndpoint
    } else if first_endpoint || second_endpoint {
        IntersectionClass::EndpointInterior
    } else if tangent {
        IntersectionClass::Tangent
    } else {
        IntersectionClass::ProperCrossing
    }
}

fn points_or_disjoint(intersections: Vec<CurveIntersection>) -> CurveIntersections {
    if intersections.is_empty() {
        CurveIntersections::Disjoint
    } else {
        CurveIntersections::Points { intersections }
    }
}

fn canonicalize_points(result: &mut CurveIntersections, precision: &PrecisionPolicy) {
    let CurveIntersections::Points { intersections } = result else {
        return;
    };
    intersections.sort_by(|first, second| {
        first
            .first_parameter
            .total_cmp(&second.first_parameter)
            .then_with(|| first.second_parameter.total_cmp(&second.second_parameter))
            .then_with(|| first.point.total_cmp(&second.point))
    });
    let tolerance = precision.parameter_resolution.max(f64::EPSILON * 64.0);
    intersections.dedup_by(|second, first| {
        (first.first_parameter - second.first_parameter).abs() <= tolerance
            && (first.second_parameter - second.second_parameter).abs() <= tolerance
    });
}

fn sort_dedup_parameters(parameters: &mut Vec<f64>, tolerance: f64) {
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|second, first| (*first - *second).abs() <= tolerance);
}

fn linear_tolerance(
    precision: &PrecisionPolicy,
    first: &EvaluatedCurve2,
    second: &EvaluatedCurve2,
) -> f64 {
    let first_bounds = first.bounds();
    let second_bounds = second.bounds();
    let scale = [
        first_bounds.min.u.abs(),
        first_bounds.min.v.abs(),
        first_bounds.max.u.abs(),
        first_bounds.max.v.abs(),
        second_bounds.min.u.abs(),
        second_bounds.min.v.abs(),
        second_bounds.max.u.abs(),
        second_bounds.max.v.abs(),
    ]
    .into_iter()
    .fold(1.0, f64::max);
    linear_tolerance_values(precision, scale)
}

fn linear_tolerance_values(precision: &PrecisionPolicy, scale: f64) -> f64 {
    precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(f64::EPSILON * scale * 64.0)
}

fn within_unit(parameter: f64, tolerance: f64) -> bool {
    parameter.is_finite() && parameter >= -tolerance && parameter <= 1.0 + tolerance
}

fn snap_unit(parameter: f64, tolerance: f64) -> f64 {
    if parameter.abs() <= tolerance {
        0.0
    } else if (parameter - 1.0).abs() <= tolerance {
        1.0
    } else {
        parameter
    }
}

fn parameter_is_endpoint(parameter: f64, tolerance: f64) -> bool {
    parameter.abs() <= tolerance || (parameter - 1.0).abs() <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CurveDirection;

    #[test]
    fn junction_keys_round_trip_through_json_for_region_persistence() {
        let keys = [
            JunctionKey::Endpoint(SketchPointId::new(7).unwrap()),
            JunctionKey::Intersection {
                first_entity: SketchEntityId::new(2).unwrap(),
                second_entity: SketchEntityId::new(9).unwrap(),
                branch: IntersectionBranch(1),
            },
        ];
        for key in keys {
            let encoded = serde_json::to_string(&key).expect("junction key should serialize");
            let decoded: JunctionKey =
                serde_json::from_str(&encoded).expect("junction key should deserialize");
            assert_eq!(decoded, key);
        }
    }

    fn line(a: (f64, f64), b: (f64, f64)) -> EvaluatedCurve2 {
        EvaluatedCurve2::Line {
            start: SketchPoint2::new(a.0, a.1),
            end: SketchPoint2::new(b.0, b.1),
        }
    }

    fn circle(center: (f64, f64), radius: f64) -> EvaluatedCurve2 {
        EvaluatedCurve2::Circle {
            center: SketchPoint2::new(center.0, center.1),
            radius,
            direction: CurveDirection::CounterClockwise,
        }
    }

    #[test]
    fn line_line_classifies_cross_endpoint_and_overlap() {
        let precision = PrecisionPolicy::default();
        let crossing = intersect_curves(
            line((-1.0, 0.0), (1.0, 0.0)),
            line((0.0, -1.0), (0.0, 1.0)),
            &precision,
        );
        assert_eq!(
            crossing.unique_points()[0].class,
            IntersectionClass::ProperCrossing
        );
        let endpoint = intersect_curves(
            line((0.0, 0.0), (1.0, 0.0)),
            line((1.0, 0.0), (1.0, 1.0)),
            &precision,
        );
        assert_eq!(
            endpoint.unique_points()[0].class,
            IntersectionClass::EndpointEndpoint
        );
        assert!(matches!(
            intersect_curves(
                line((0.0, 0.0), (2.0, 0.0)),
                line((1.0, 0.0), (3.0, 0.0)),
                &precision
            ),
            CurveIntersections::Overlap { .. }
        ));
    }

    #[test]
    fn line_circle_has_two_ordered_symmetric_events() {
        let precision = PrecisionPolicy::default();
        let first = line((-2.0, 0.0), (2.0, 0.0));
        let second = circle((0.0, 0.0), 1.0);
        let forward = intersect_curves(first.clone(), second.clone(), &precision);
        let reverse = intersect_curves(second, first, &precision).reversed();
        assert_eq!(forward, reverse);
        assert_eq!(forward.unique_points().len(), 2);
        assert!(
            forward.unique_points()[0].first_parameter < forward.unique_points()[1].first_parameter
        );
    }

    #[test]
    fn arc_domain_excludes_carrier_solution() {
        let precision = PrecisionPolicy::default();
        let upper_arc = EvaluatedCurve2::CircularArc {
            center: SketchPoint2::new(0.0, 0.0),
            start: SketchPoint2::new(1.0, 0.0),
            end: SketchPoint2::new(-1.0, 0.0),
            direction: CurveDirection::CounterClockwise,
        };
        let vertical = line((0.0, -2.0), (0.0, 2.0));
        let result = intersect_curves(vertical, upper_arc, &precision);
        assert_eq!(result.unique_points().len(), 1);
        assert!(result.unique_points()[0].point.v > 0.0);
    }

    #[test]
    fn circles_classify_tangent_two_point_and_coincident() {
        let precision = PrecisionPolicy::default();
        let tangent =
            intersect_curves(circle((0.0, 0.0), 1.0), circle((2.0, 0.0), 1.0), &precision);
        assert_eq!(tangent.unique_points()[0].class, IntersectionClass::Tangent);
        assert_eq!(
            intersect_curves(circle((0.0, 0.0), 2.0), circle((2.0, 0.0), 2.0), &precision)
                .unique_points()
                .len(),
            2
        );
        assert_eq!(
            intersect_curves(circle((0.0, 0.0), 2.0), circle((0.0, 0.0), 2.0), &precision),
            CurveIntersections::CoincidentFull
        );
    }
}

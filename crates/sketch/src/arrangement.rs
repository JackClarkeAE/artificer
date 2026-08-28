//! Bounded analytic planar arrangement and stable region identities.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::f64::consts::TAU;

use artificer_compute::ComputePool;
use artificer_protocol::PrecisionPolicy;
use serde::{Deserialize, Serialize};

use crate::{
    CurveDirection, CurveGeometryError, CurveIntersections, EvaluatedCurve2, JunctionClusterKey,
    JunctionKey, SketchEntityId, SketchPoint2, SketchPointId, intersect_entities,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementInputCurve {
    pub entity: SketchEntityId,
    pub curve: EvaluatedCurve2,
    pub start_point: Option<SketchPointId>,
    pub end_point: Option<SketchPointId>,
}

impl ArrangementInputCurve {
    #[must_use]
    pub const fn line(
        entity: SketchEntityId,
        start_point: SketchPointId,
        end_point: SketchPointId,
        start: SketchPoint2,
        end: SketchPoint2,
    ) -> Self {
        Self {
            entity,
            curve: EvaluatedCurve2::Line { start, end },
            start_point: Some(start_point),
            end_point: Some(end_point),
        }
    }

    #[must_use]
    pub const fn circular_arc(
        entity: SketchEntityId,
        center: SketchPoint2,
        start_point: SketchPointId,
        end_point: SketchPointId,
        start: SketchPoint2,
        end: SketchPoint2,
        direction: CurveDirection,
    ) -> Self {
        Self {
            entity,
            curve: EvaluatedCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            },
            start_point: Some(start_point),
            end_point: Some(end_point),
        }
    }

    #[must_use]
    pub const fn circle(
        entity: SketchEntityId,
        center: SketchPoint2,
        radius: f64,
        direction: CurveDirection,
    ) -> Self {
        Self {
            entity,
            curve: EvaluatedCurve2::Circle {
                center,
                radius,
                direction,
            },
            start_point: None,
            end_point: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArrangementLimits {
    pub max_curves: usize,
    pub max_intersection_events: usize,
    pub max_fragments: usize,
}

impl Default for ArrangementLimits {
    fn default() -> Self {
        Self {
            max_curves: 1_024,
            max_intersection_events: 16_384,
            max_fragments: 34_816,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FragmentDirection {
    Forward,
    Reverse,
}

impl FragmentDirection {
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Reverse,
            Self::Reverse => Self::Forward,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FragmentEndpointKey {
    Junction(JunctionClusterKey),
    PeriodicSeam { source_entity: SketchEntityId },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FragmentKey {
    pub source_entity: SketchEntityId,
    pub start: FragmentEndpointKey,
    pub end: FragmentEndpointKey,
    pub direction: FragmentDirection,
    /// Which way round its carrier a fragment of a *circle* runs. Two
    /// junctions cut a circle into two arcs with the same endpoints; without
    /// this the second arc's key was the first arc's reversal, so the pair
    /// cancelled as a shared boundary and read as a dangling bridge. Straight
    /// and open-arc fragments cannot repeat an endpoint pair and carry `None`,
    /// as does the seam key of an unsplit circle, so their keys are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sense: Option<CurveDirection>,
}

impl FragmentKey {
    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            source_entity: self.source_entity,
            start: self.end.clone(),
            end: self.start.clone(),
            direction: self.direction.reversed(),
            sense: self.sense.map(|sense| match sense {
                CurveDirection::CounterClockwise => CurveDirection::Clockwise,
                CurveDirection::Clockwise => CurveDirection::CounterClockwise,
            }),
        }
    }

    /// The key as written before `sense` existed. Signatures persisted by
    /// earlier releases resolve through this view.
    fn without_sense(&self) -> Self {
        Self {
            sense: None,
            ..self.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementFragment {
    pub key: FragmentKey,
    pub curve: EvaluatedCurve2,
    pub start_junction: usize,
    pub end_junction: usize,
    pub source_interval: SourceInterval,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceInterval {
    pub start: f64,
    pub end: f64,
    pub wraps_periodic_seam: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementJunction {
    pub key: JunctionClusterKey,
    pub point: SketchPoint2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementHalfEdge {
    pub fragment: usize,
    pub origin: usize,
    pub destination: usize,
    pub twin: usize,
    pub next: Option<usize>,
    pub curve: EvaluatedCurve2,
    pub key: FragmentKey,
}

/// Stable semantic identity of one minimal bounded cell.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RegionSignature {
    pub outer: Vec<FragmentKey>,
    pub holes: Vec<Vec<FragmentKey>>,
}

impl RegionSignature {
    fn without_sense(&self) -> Self {
        Self {
            outer: self.outer.iter().map(FragmentKey::without_sense).collect(),
            holes: self
                .holes
                .iter()
                .map(|hole| hole.iter().map(FragmentKey::without_sense).collect())
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementLoop {
    pub half_edges: Vec<usize>,
    pub curves: Vec<EvaluatedCurve2>,
    pub fragment_keys: Vec<FragmentKey>,
    pub signed_area: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrangementCell {
    pub signature: RegionSignature,
    pub outer: ArrangementLoop,
    pub holes: Vec<ArrangementLoop>,
    pub signed_area: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SketchArrangement {
    pub junctions: Vec<ArrangementJunction>,
    pub fragments: Vec<ArrangementFragment>,
    pub half_edges: Vec<ArrangementHalfEdge>,
    pub cells: Vec<ArrangementCell>,
    pub diagnostics: Vec<ArrangementDiagnostic>,
}

impl SketchArrangement {
    /// Resolves a signature to its cell. A signature written before circle
    /// fragments carried [`FragmentKey::sense`] still names one cell: with the
    /// sense stripped from both sides the earlier scheme was unambiguous
    /// within any one loop, so that comparison is tried when the exact one
    /// finds nothing.
    #[must_use]
    pub fn cell(&self, signature: &RegionSignature) -> Option<&ArrangementCell> {
        self.cells
            .iter()
            .find(|cell| &cell.signature == signature)
            .or_else(|| {
                if signature.outer.iter().any(|key| key.sense.is_some())
                    || signature
                        .holes
                        .iter()
                        .flatten()
                        .any(|key| key.sense.is_some())
                {
                    return None;
                }
                self.cells
                    .iter()
                    .find(|cell| &cell.signature.without_sense() == signature)
            })
    }

    /// Whether `point` lies within `tolerance` of any arrangement fragment.
    ///
    /// Region anchors must be strictly interior: a point that has drifted onto
    /// a boundary belongs to no cell in particular, and resolving it would
    /// pick a neighbour arbitrarily.
    #[must_use]
    pub fn point_near_boundary(&self, point: SketchPoint2, tolerance: f64) -> bool {
        self.fragments.iter().any(|fragment| {
            let parameter = fragment.curve.closest_parameter(point);
            fragment
                .curve
                .evaluate(parameter)
                .is_ok_and(|nearest| nearest.distance(point) <= tolerance)
        })
    }

    /// An exact interior point of `cell`, suitable as a stable region anchor.
    ///
    /// The sample is stepped inward from the cell's own boundary rather than
    /// taken from a centroid, so it stays inside concave and annular cells
    /// whose centroid falls in a hole or outside the loop entirely.
    #[must_use]
    pub fn cell_interior_sample(
        &self,
        cell: &ArrangementCell,
        precision: &PrecisionPolicy,
    ) -> Option<SketchPoint2> {
        for curve in &cell.outer.curves {
            for parameter in [0.5, 0.25, 0.75] {
                let Ok(boundary) = curve.evaluate(parameter) else {
                    continue;
                };
                let Some(inward) = curve
                    .tangent(parameter)
                    .ok()
                    .and_then(|tangent| tangent.normalized())
                    .map(|tangent| tangent.left_normal())
                else {
                    continue;
                };
                let base_offset = precision
                    .modeling_resolution
                    .max(precision.linear_agreement)
                    .max(curve.arc_length() * 1.0e-5);
                for multiple in [4.0, 16.0, 1.0] {
                    let candidate = boundary + inward * (base_offset * multiple);
                    if self
                        .cell_at_point(candidate, precision)
                        .is_some_and(|owner| owner.signature == cell.signature)
                        && !self.point_near_boundary(candidate, precision.modeling_resolution)
                    {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    /// Resolves the minimal bounded cell containing a sketch-plane point.
    /// This is analytic region selection; renderer fill meshes are not used.
    #[must_use]
    pub fn cell_at_point(
        &self,
        point: SketchPoint2,
        precision: &PrecisionPolicy,
    ) -> Option<&ArrangementCell> {
        self.cells
            .iter()
            .filter(|cell| {
                point_in_loop(point, &cell.outer, precision)
                    && !cell
                        .holes
                        .iter()
                        .any(|hole| point_in_loop(point, hole, precision))
            })
            .min_by(|first, second| first.signed_area.total_cmp(&second.signed_area))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ArrangementDiagnostic {
    CurveLimitExceeded {
        limit: usize,
        actual: usize,
    },
    EventLimitExceeded {
        limit: usize,
    },
    FragmentLimitExceeded {
        limit: usize,
    },
    DuplicateEntity {
        entity: SketchEntityId,
    },
    InvalidCurve {
        entity: SketchEntityId,
    },
    CoincidentOrOverlapping {
        first: SketchEntityId,
        second: SketchEntityId,
    },
    KissingJunction {
        junction: JunctionClusterKey,
    },
    IndeterminateIntersection {
        first: SketchEntityId,
        second: SketchEntityId,
    },
    ZeroAreaCycle,
    AmbiguousJunctionOrder,
}

#[derive(Clone, Debug)]
struct RawEvent {
    curve_index: usize,
    parameter: f64,
    point: SketchPoint2,
    key: JunctionKey,
}

#[derive(Clone, Debug)]
struct Cluster {
    point: SketchPoint2,
    keys: Vec<JunctionKey>,
    event_indices: Vec<usize>,
}

#[derive(Clone, Debug)]
struct CurveEvent {
    parameter: f64,
    junction: usize,
}

/// Builds a bounded arrangement. Invalid/ambiguous curve pairs are isolated:
/// unrelated valid closed geometry still publishes its cells.
#[must_use]
pub fn build_arrangement(
    curves: &[ArrangementInputCurve],
    precision: &PrecisionPolicy,
    limits: ArrangementLimits,
) -> SketchArrangement {
    build_arrangement_with_pool(ComputePool::global(), curves, precision, limits)
}

/// Builds the same canonical arrangement using a caller-selected compute
/// pool. This supports controlled benchmarks and deterministic test modes.
pub fn build_arrangement_with_pool(
    compute: &ComputePool,
    curves: &[ArrangementInputCurve],
    precision: &PrecisionPolicy,
    limits: ArrangementLimits,
) -> SketchArrangement {
    let mut arrangement = SketchArrangement {
        junctions: Vec::new(),
        fragments: Vec::new(),
        half_edges: Vec::new(),
        cells: Vec::new(),
        diagnostics: Vec::new(),
    };
    if curves.len() > limits.max_curves {
        arrangement
            .diagnostics
            .push(ArrangementDiagnostic::CurveLimitExceeded {
                limit: limits.max_curves,
                actual: curves.len(),
            });
        return arrangement;
    }

    let mut entity_indices = BTreeMap::new();
    let mut invalid_curves = BTreeSet::new();
    for (index, input) in curves.iter().enumerate() {
        if entity_indices.insert(input.entity, index).is_some() {
            arrangement
                .diagnostics
                .push(ArrangementDiagnostic::DuplicateEntity {
                    entity: input.entity,
                });
            invalid_curves.insert(index);
        }
        if input.curve.validate(precision).is_err() {
            arrangement
                .diagnostics
                .push(ArrangementDiagnostic::InvalidCurve {
                    entity: input.entity,
                });
            invalid_curves.insert(index);
        }
    }

    let mut raw_events = Vec::new();
    for (index, input) in curves.iter().enumerate() {
        if invalid_curves.contains(&index) {
            continue;
        }
        if let (Some(point_id), Some((start, _))) = (input.start_point, input.curve.endpoints()) {
            raw_events.push(RawEvent {
                curve_index: index,
                parameter: 0.0,
                point: start,
                key: JunctionKey::Endpoint(point_id),
            });
        }
        if let (Some(point_id), Some((_, end))) = (input.end_point, input.curve.endpoints()) {
            raw_events.push(RawEvent {
                curve_index: index,
                parameter: 1.0,
                point: end,
                key: JunctionKey::Endpoint(point_id),
            });
        }
    }

    let mut unique_event_count = 0usize;
    let candidate_pairs = broad_phase_pairs(curves, precision);
    // Intersection evaluation is immutable and usually dominates dense
    // arrangements. Results remain indexed by the broad-phase pair order;
    // diagnostics, event limits, and topology publication stay serial below.
    let pair_intersections = compute.map(
        "sketch.arrangement.intersections",
        &candidate_pairs,
        |_, &(first_index, second_index)| {
            if invalid_curves.contains(&first_index) || invalid_curves.contains(&second_index) {
                return None;
            }
            let first = &curves[first_index];
            let second = &curves[second_index];
            Some(intersect_entities(
                first.entity,
                first.curve.clone(),
                second.entity,
                second.curve.clone(),
                precision,
            ))
        },
    );
    'pairs: for ((first_index, second_index), intersections) in
        candidate_pairs.into_iter().zip(pair_intersections)
    {
        let Some(intersections) = intersections else {
            continue;
        };
        let first = &curves[first_index];
        let second = &curves[second_index];
        match &intersections.result {
            CurveIntersections::Disjoint => {}
            CurveIntersections::Points {
                intersections: points,
            } => {
                unique_event_count = unique_event_count.saturating_add(points.len());
                if unique_event_count > limits.max_intersection_events {
                    arrangement
                        .diagnostics
                        .push(ArrangementDiagnostic::EventLimitExceeded {
                            limit: limits.max_intersection_events,
                        });
                    break 'pairs;
                }
                for (key, canonical_event) in intersections.junction_keys() {
                    let (first_parameter, second_parameter) = if first.entity <= second.entity {
                        (
                            canonical_event.first_parameter,
                            canonical_event.second_parameter,
                        )
                    } else {
                        (
                            canonical_event.second_parameter,
                            canonical_event.first_parameter,
                        )
                    };
                    // A tangent contact is an ordinary junction: both curves
                    // split there, and `link_half_edges` orders the parallel
                    // departures by curvature. A circle resting on a square's
                    // side therefore yields the disc and the surrounding cell
                    // instead of no cells at all.
                    raw_events.push(RawEvent {
                        curve_index: first_index,
                        parameter: first_parameter,
                        point: canonical_event.point,
                        key: key.clone(),
                    });
                    raw_events.push(RawEvent {
                        curve_index: second_index,
                        parameter: second_parameter,
                        point: canonical_event.point,
                        key,
                    });
                }
            }
            CurveIntersections::Overlap { .. } | CurveIntersections::CoincidentFull => {
                arrangement
                    .diagnostics
                    .push(ArrangementDiagnostic::CoincidentOrOverlapping {
                        first: first.entity.min(second.entity),
                        second: first.entity.max(second.entity),
                    });
                invalid_curves.insert(first_index);
                invalid_curves.insert(second_index);
            }
            CurveIntersections::Indeterminate { .. } => {
                arrangement
                    .diagnostics
                    .push(ArrangementDiagnostic::IndeterminateIntersection {
                        first: first.entity.min(second.entity),
                        second: first.entity.max(second.entity),
                    });
                invalid_curves.insert(first_index);
                invalid_curves.insert(second_index);
            }
        }
    }
    if arrangement
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic, ArrangementDiagnostic::EventLimitExceeded { .. }))
    {
        return arrangement;
    }

    let invalid_entities: BTreeSet<_> = invalid_curves
        .iter()
        .map(|index| curves[*index].entity)
        .collect();
    raw_events.retain(|event| {
        !invalid_curves.contains(&event.curve_index)
            && match &event.key {
                JunctionKey::Endpoint(_) | JunctionKey::PeriodicSplit { .. } => true,
                JunctionKey::Intersection {
                    first_entity,
                    second_entity,
                    ..
                } => {
                    !invalid_entities.contains(first_entity)
                        && !invalid_entities.contains(second_entity)
                }
            }
    });

    let (junctions, event_junctions) = cluster_events(&raw_events, precision);
    arrangement.junctions = junctions;
    let mut events_by_curve = vec![Vec::new(); curves.len()];
    for (event_index, event) in raw_events.iter().enumerate() {
        if invalid_curves.contains(&event.curve_index) {
            continue;
        }
        events_by_curve[event.curve_index].push(CurveEvent {
            parameter: event.parameter,
            junction: event_junctions[event_index],
        });
    }
    for events in &mut events_by_curve {
        events.sort_by(|first, second| {
            first
                .parameter
                .total_cmp(&second.parameter)
                .then_with(|| first.junction.cmp(&second.junction))
        });
        let tolerance = precision.parameter_resolution.max(f64::EPSILON * 64.0);
        events.dedup_by(|second, first| {
            (first.parameter - second.parameter).abs() <= tolerance
                && first.junction == second.junction
        });
    }
    split_single_junction_circles(
        curves,
        &invalid_curves,
        &mut events_by_curve,
        &mut arrangement,
    );

    let mut unsplit_circles = Vec::new();
    for (curve_index, input) in curves.iter().enumerate() {
        if invalid_curves.contains(&curve_index) {
            continue;
        }
        let events = &events_by_curve[curve_index];
        if input.curve.is_periodic() && events.is_empty() {
            unsplit_circles.push(input.clone());
            continue;
        }
        add_curve_fragments(
            input,
            events,
            &arrangement.junctions,
            precision,
            &mut arrangement.fragments,
        );
        if arrangement.fragments.len() > limits.max_fragments {
            arrangement
                .diagnostics
                .push(ArrangementDiagnostic::FragmentLimitExceeded {
                    limit: limits.max_fragments,
                });
            arrangement.fragments.clear();
            return arrangement;
        }
    }

    arrangement.half_edges = build_half_edges(&arrangement.fragments);
    link_half_edges(
        &mut arrangement.half_edges,
        &arrangement.junctions,
        &mut arrangement.diagnostics,
    );
    let mut positive_loops = walk_positive_loops(
        &arrangement.half_edges,
        precision,
        &mut arrangement.diagnostics,
    );

    for input in unsplit_circles {
        let seam = FragmentEndpointKey::PeriodicSeam {
            source_entity: input.entity,
        };
        let key = FragmentKey {
            source_entity: input.entity,
            start: seam.clone(),
            end: seam,
            direction: FragmentDirection::Forward,
            sense: None,
        };
        let curve = match &input.curve {
            EvaluatedCurve2::Circle {
                direction: CurveDirection::CounterClockwise,
                ..
            } => input.curve.clone(),
            _ => input.curve.reverse(),
        };
        let area = curve.signed_area_contribution();
        positive_loops.push(ArrangementLoop {
            half_edges: Vec::new(),
            curves: vec![curve],
            fragment_keys: vec![key],
            signed_area: area,
        });
    }

    arrangement.cells = build_cells(positive_loops, precision);
    arrangement
        .cells
        .sort_by(|first, second| first.signature.cmp(&second.signature));
    arrangement
}

/// Deterministic sweep-and-prune broad phase. It bounds the narrow-phase work
/// for ordinary sparse sketches while dense adversarial inputs still reach the
/// explicit event ceiling instead of growing without a product limit.
fn broad_phase_pairs(
    curves: &[ArrangementInputCurve],
    precision: &PrecisionPolicy,
) -> Vec<(usize, usize)> {
    let expansion = precision
        .modeling_resolution
        .max(precision.linear_agreement);
    let bounds: Vec<_> = curves
        .iter()
        .map(|input| input.curve.bounds().expanded(expansion))
        .collect();
    let mut order: Vec<_> = (0..curves.len()).collect();
    order.sort_by(|&first, &second| {
        bounds[first]
            .min
            .u
            .total_cmp(&bounds[second].min.u)
            .then_with(|| curves[first].entity.cmp(&curves[second].entity))
    });
    let mut active = Vec::<usize>::new();
    let mut pairs = Vec::new();
    for index in order {
        active.retain(|candidate| bounds[*candidate].max.u >= bounds[index].min.u);
        for candidate in &active {
            if bounds[*candidate].intersects(bounds[index]) {
                pairs.push(((*candidate).min(index), (*candidate).max(index)));
            }
        }
        active.push(index);
    }
    pairs.sort_by(|(first_a, second_a), (first_b, second_b)| {
        curves[*first_a]
            .entity
            .min(curves[*second_a].entity)
            .cmp(&curves[*first_b].entity.min(curves[*second_b].entity))
            .then_with(|| {
                curves[*first_a]
                    .entity
                    .max(curves[*second_a].entity)
                    .cmp(&curves[*first_b].entity.max(curves[*second_b].entity))
            })
    });
    pairs
}

/// A circle carrying exactly one junction — a line ending on its rim, or a
/// side it rests against — cannot become fragments on its own: one split of
/// a closed curve is a full-turn arc, which is degenerate. Give it a second,
/// synthetic junction at the antipode of the real one so it splits into two
/// half-turn arcs and the disc stays a bounded cell.
fn split_single_junction_circles(
    curves: &[ArrangementInputCurve],
    invalid_curves: &BTreeSet<usize>,
    events_by_curve: &mut [Vec<CurveEvent>],
    arrangement: &mut SketchArrangement,
) {
    for (curve_index, input) in curves.iter().enumerate() {
        if invalid_curves.contains(&curve_index) || !input.curve.is_periodic() {
            continue;
        }
        let [only] = events_by_curve[curve_index].as_slice() else {
            continue;
        };
        let parameter = (only.parameter + 0.5).rem_euclid(1.0);
        let Ok(point) = input.curve.evaluate(parameter) else {
            continue;
        };
        let key = JunctionClusterKey::new(vec![JunctionKey::PeriodicSplit {
            source_entity: input.entity,
        }])
        .expect("one key is a non-empty cluster");
        let junction = arrangement.junctions.len();
        arrangement
            .junctions
            .push(ArrangementJunction { key, point });
        let events = &mut events_by_curve[curve_index];
        events.push(CurveEvent {
            parameter,
            junction,
        });
        events.sort_by(|first, second| first.parameter.total_cmp(&second.parameter));
    }
}

fn cluster_events(
    events: &[RawEvent],
    precision: &PrecisionPolicy,
) -> (Vec<ArrangementJunction>, Vec<usize>) {
    let mut order: Vec<_> = (0..events.len()).collect();
    order.sort_by(|&first, &second| {
        events[first]
            .point
            .total_cmp(&events[second].point)
            .then_with(|| events[first].key.cmp(&events[second].key))
            .then_with(|| events[first].curve_index.cmp(&events[second].curve_index))
            .then_with(|| events[first].parameter.total_cmp(&events[second].parameter))
    });
    let tolerance = precision
        .modeling_resolution
        .max(precision.linear_agreement);
    let mut clusters: Vec<Cluster> = Vec::new();
    for event_index in order {
        let event = &events[event_index];
        if let Some(cluster) = clusters
            .iter_mut()
            .find(|cluster| cluster.point.distance(event.point) <= tolerance)
        {
            cluster.keys.push(event.key.clone());
            cluster.event_indices.push(event_index);
        } else {
            clusters.push(Cluster {
                point: event.point,
                keys: vec![event.key.clone()],
                event_indices: vec![event_index],
            });
        }
    }
    clusters.sort_by(|first, second| {
        let first_key = JunctionClusterKey::new(first.keys.clone()).expect("non-empty cluster");
        let second_key = JunctionClusterKey::new(second.keys.clone()).expect("non-empty cluster");
        first_key
            .cmp(&second_key)
            .then_with(|| first.point.total_cmp(&second.point))
    });

    let mut event_junctions = vec![0; events.len()];
    let junctions = clusters
        .into_iter()
        .enumerate()
        .map(|(junction_index, cluster)| {
            for event_index in cluster.event_indices {
                event_junctions[event_index] = junction_index;
            }
            ArrangementJunction {
                key: JunctionClusterKey::new(cluster.keys).expect("non-empty cluster"),
                point: cluster.point,
            }
        })
        .collect();
    (junctions, event_junctions)
}

fn add_curve_fragments(
    input: &ArrangementInputCurve,
    events: &[CurveEvent],
    junctions: &[ArrangementJunction],
    precision: &PrecisionPolicy,
    fragments: &mut Vec<ArrangementFragment>,
) {
    if input.curve.is_periodic() {
        if events.len() < 2 {
            return;
        }
        for index in 0..events.len() {
            let start = &events[index];
            let end = &events[(index + 1) % events.len()];
            let wraps = index + 1 == events.len();
            let curve = periodic_fragment(&input.curve, start.parameter, end.parameter, wraps);
            if let Ok(curve) = curve {
                let sense = match &curve {
                    EvaluatedCurve2::CircularArc { direction, .. }
                    | EvaluatedCurve2::Circle { direction, .. } => Some(*direction),
                    EvaluatedCurve2::Line { .. } | EvaluatedCurve2::Bspline { .. } => None,
                };
                push_fragment(
                    input.entity,
                    (start, end),
                    curve,
                    sense,
                    wraps,
                    junctions,
                    fragments,
                );
            }
        }
        return;
    }

    for pair in events.windows(2) {
        if pair[1].parameter - pair[0].parameter
            <= precision.parameter_resolution.max(f64::EPSILON * 64.0)
        {
            continue;
        }
        if let Ok(curve) = input.curve.subcurve(pair[0].parameter, pair[1].parameter) {
            push_fragment(
                input.entity,
                (&pair[0], &pair[1]),
                curve,
                None,
                false,
                junctions,
                fragments,
            );
        }
    }
}

fn periodic_fragment(
    circle: &EvaluatedCurve2,
    start_parameter: f64,
    end_parameter: f64,
    wraps: bool,
) -> Result<EvaluatedCurve2, CurveGeometryError> {
    if !wraps {
        return circle.subcurve(start_parameter, end_parameter);
    }
    let start = circle.evaluate(start_parameter)?;
    let end = circle.evaluate(end_parameter)?;
    let EvaluatedCurve2::Circle {
        center, direction, ..
    } = circle
    else {
        unreachable!("periodic sketch curve is a circle");
    };
    Ok(EvaluatedCurve2::CircularArc {
        center: *center,
        start,
        end,
        direction: *direction,
    })
}

fn push_fragment(
    entity: SketchEntityId,
    (start, end): (&CurveEvent, &CurveEvent),
    curve: EvaluatedCurve2,
    sense: Option<CurveDirection>,
    wraps: bool,
    junctions: &[ArrangementJunction],
    fragments: &mut Vec<ArrangementFragment>,
) {
    let start_key = FragmentEndpointKey::Junction(junctions[start.junction].key.clone());
    let end_key = FragmentEndpointKey::Junction(junctions[end.junction].key.clone());
    let semantic_direction = if start_key <= end_key {
        FragmentDirection::Forward
    } else {
        FragmentDirection::Reverse
    };
    // Every fragment meeting at a junction ends on the junction's one point.
    // An evaluated arc endpoint is trigonometry and a line's is arithmetic,
    // so left to themselves the two agree only to rounding — and the kernel
    // reads a profile whose uses do not chain bit-exactly as open. The move
    // is at most the junction cluster tolerance, which is also what an arc's
    // radius agreement allows.
    let curve = retarget_endpoints(
        curve,
        junctions[start.junction].point,
        junctions[end.junction].point,
    );
    fragments.push(ArrangementFragment {
        key: FragmentKey {
            source_entity: entity,
            start: start_key,
            end: end_key,
            direction: semantic_direction,
            sense,
        },
        curve,
        start_junction: start.junction,
        end_junction: end.junction,
        source_interval: SourceInterval {
            start: start.parameter,
            end: end.parameter,
            wraps_periodic_seam: wraps,
        },
    });
}

fn retarget_endpoints(
    curve: EvaluatedCurve2,
    start: SketchPoint2,
    end: SketchPoint2,
) -> EvaluatedCurve2 {
    match curve {
        EvaluatedCurve2::Line { .. } => EvaluatedCurve2::Line { start, end },
        EvaluatedCurve2::CircularArc {
            center, direction, ..
        } => EvaluatedCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        },
        EvaluatedCurve2::Circle { .. } | EvaluatedCurve2::Bspline { .. } => curve,
    }
}

fn build_half_edges(fragments: &[ArrangementFragment]) -> Vec<ArrangementHalfEdge> {
    let mut half_edges = Vec::with_capacity(fragments.len() * 2);
    for (fragment_index, fragment) in fragments.iter().enumerate() {
        let forward_index = half_edges.len();
        let reverse_index = forward_index + 1;
        half_edges.push(ArrangementHalfEdge {
            fragment: fragment_index,
            origin: fragment.start_junction,
            destination: fragment.end_junction,
            twin: reverse_index,
            next: None,
            curve: fragment.curve.clone(),
            key: fragment.key.clone(),
        });
        half_edges.push(ArrangementHalfEdge {
            fragment: fragment_index,
            origin: fragment.end_junction,
            destination: fragment.start_junction,
            twin: forward_index,
            next: None,
            curve: fragment.curve.reverse(),
            key: fragment.key.reversed(),
        });
    }
    half_edges
}

fn link_half_edges(
    half_edges: &mut [ArrangementHalfEdge],
    junctions: &[ArrangementJunction],
    diagnostics: &mut Vec<ArrangementDiagnostic>,
) {
    let mut outgoing: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, edge) in half_edges.iter().enumerate() {
        outgoing.entry(edge.origin).or_default().push(index);
    }
    let mut ambiguous_junctions = BTreeSet::new();
    for (junction, edges) in &mut outgoing {
        sort_departures(edges, half_edges);
        for pair in edges.windows(2) {
            if departures_coincide(&half_edges[pair[0]], &half_edges[pair[1]])
                && half_edges[pair[0]].key.source_entity != half_edges[pair[1]].key.source_entity
            {
                diagnostics.push(ArrangementDiagnostic::AmbiguousJunctionOrder);
                ambiguous_junctions.insert(*junction);
            }
        }
        let has_authored_endpoint = junctions[*junction]
            .key
            .keys()
            .iter()
            .any(|key| matches!(key, JunctionKey::Endpoint(_)));
        if edges.len() >= 4 && has_authored_endpoint {
            diagnostics.push(ArrangementDiagnostic::KissingJunction {
                junction: junctions[*junction].key.clone(),
            });
            ambiguous_junctions.insert(*junction);
        }
    }
    for edge in half_edges.iter_mut() {
        let twin = edge.twin;
        let destination = edge.destination;
        if ambiguous_junctions.contains(&edge.origin) || ambiguous_junctions.contains(&destination)
        {
            continue;
        }
        let Some(at_destination) = outgoing.get(&destination) else {
            continue;
        };
        let Some(twin_position) = at_destination
            .iter()
            .position(|candidate| *candidate == twin)
        else {
            continue;
        };
        let next_position = if twin_position == 0 {
            at_destination.len() - 1
        } else {
            twin_position - 1
        };
        edge.next = Some(at_destination[next_position]);
    }
}

const DEPARTURE_ANGLE_TOLERANCE: f64 = f64::EPSILON * 64.0;

/// Orders the half-edges leaving one junction counter-clockwise.
///
/// The primary key is the departure tangent. Two curves that touch
/// tangentially leave along the same tangent, so a run of equal angles is
/// ordered by signed curvature: after an infinitesimal advance the departure
/// bending left has turned further counter-clockwise than a straight one,
/// which in turn is further round than one bending right. Only equal tangent
/// *and* equal curvature is a genuine tie, and that is a coincident carrier.
fn sort_departures(edges: &mut [usize], half_edges: &[ArrangementHalfEdge]) {
    edges.sort_by(|&first, &second| {
        departure_angle(&half_edges[first])
            .total_cmp(&departure_angle(&half_edges[second]))
            .then_with(|| half_edges[first].key.cmp(&half_edges[second].key))
    });
    let mut start = 0;
    while start < edges.len() {
        let mut end = start + 1;
        while end < edges.len()
            && (departure_angle(&half_edges[edges[end]])
                - departure_angle(&half_edges[edges[end - 1]]))
            .abs()
                <= DEPARTURE_ANGLE_TOLERANCE
        {
            end += 1;
        }
        if end - start > 1 {
            edges[start..end].sort_by(|&first, &second| {
                signed_curvature(&half_edges[first])
                    .total_cmp(&signed_curvature(&half_edges[second]))
                    .then_with(|| half_edges[first].key.cmp(&half_edges[second].key))
            });
        }
        start = end;
    }
}

/// Whether two departures from one junction cannot be told apart to second
/// order: same tangent direction and same signed curvature.
fn departures_coincide(first: &ArrangementHalfEdge, second: &ArrangementHalfEdge) -> bool {
    let first_curvature = signed_curvature(first);
    let second_curvature = signed_curvature(second);
    (departure_angle(first) - departure_angle(second)).abs() <= DEPARTURE_ANGLE_TOLERANCE
        && (first_curvature - second_curvature).abs()
            <= f64::EPSILON * 64.0 * (1.0 + first_curvature.abs() + second_curvature.abs())
}

/// The departure tangent angle with the seam folded: an angle within
/// tolerance below `2π` reads as slightly negative, so tangent departures
/// that straddle the `0`/`2π` seam still sort adjacent.
fn departure_angle(edge: &ArrangementHalfEdge) -> f64 {
    let angle = tangent_angle(edge);
    if angle >= TAU - DEPARTURE_ANGLE_TOLERANCE {
        angle - TAU
    } else {
        angle
    }
}

/// Signed curvature at the departure: positive bends left (counter-clockwise),
/// negative bends right, zero for a straight carrier.
fn signed_curvature(edge: &ArrangementHalfEdge) -> f64 {
    match &edge.curve {
        EvaluatedCurve2::Line { .. } => 0.0,
        EvaluatedCurve2::CircularArc { direction, .. }
        | EvaluatedCurve2::Circle { direction, .. } => {
            let radius = edge
                .curve
                .radius()
                .expect("a circular carrier has a radius");
            match direction {
                CurveDirection::CounterClockwise => 1.0 / radius,
                CurveDirection::Clockwise => -1.0 / radius,
            }
        }
        EvaluatedCurve2::Bspline { .. } => edge.curve.curvature(0.0).unwrap_or(0.0),
    }
}

fn tangent_angle(edge: &ArrangementHalfEdge) -> f64 {
    let tangent = edge
        .curve
        .tangent(0.0)
        .expect("validated fragment has a start tangent");
    tangent.v.atan2(tangent.u).rem_euclid(TAU)
}

fn walk_positive_loops(
    half_edges: &[ArrangementHalfEdge],
    precision: &PrecisionPolicy,
    diagnostics: &mut Vec<ArrangementDiagnostic>,
) -> Vec<ArrangementLoop> {
    let mut visited = vec![false; half_edges.len()];
    let mut loops = Vec::new();
    for start in 0..half_edges.len() {
        if visited[start] {
            continue;
        }
        let mut edge_indices = Vec::new();
        let mut current = start;
        let mut closed = false;
        for _ in 0..=half_edges.len() {
            if visited[current] && current != start {
                break;
            }
            visited[current] = true;
            edge_indices.push(current);
            let Some(next) = half_edges[current].next else {
                break;
            };
            current = next;
            if current == start {
                closed = true;
                break;
            }
        }
        if !closed {
            continue;
        }
        let mut boundary_uses: Vec<_> = edge_indices
            .iter()
            .map(|index| {
                (
                    *index,
                    half_edges[*index].key.clone(),
                    half_edges[*index].curve.clone(),
                )
            })
            .collect();
        remove_bridge_backtracks(&mut boundary_uses);
        let curves: Vec<_> = boundary_uses
            .iter()
            .map(|(_, _, curve)| curve.clone())
            .collect();
        let signed_area: f64 = curves
            .iter()
            .map(|curve| curve.signed_area_contribution())
            .sum();
        if signed_area.abs() <= precision.min_feature_size * precision.min_feature_size {
            diagnostics.push(ArrangementDiagnostic::ZeroAreaCycle);
            continue;
        }
        if signed_area > 0.0 {
            loops.push(ArrangementLoop {
                half_edges: boundary_uses
                    .iter()
                    .map(|(edge_index, _, _)| *edge_index)
                    .collect(),
                curves,
                fragment_keys: boundary_uses
                    .iter()
                    .map(|(_, key, _)| key.clone())
                    .collect(),
                signed_area,
            });
        }
    }
    loops
}

/// A dangling bridge is traversed once in each direction while walking the
/// surrounding face. Remove those adjacent inverse uses so open geometry does
/// not become part of a bounded region signature or exported boundary.
fn remove_bridge_backtracks(uses: &mut Vec<(usize, FragmentKey, EvaluatedCurve2)>) {
    loop {
        if uses.len() < 2 {
            return;
        }
        let mut removed = false;
        for index in 0..uses.len() {
            let next = (index + 1) % uses.len();
            if uses[index].1.reversed() == uses[next].1 {
                if next == 0 {
                    uses.remove(index);
                    uses.remove(0);
                } else {
                    uses.remove(next);
                    uses.remove(index);
                }
                removed = true;
                break;
            }
        }
        if !removed {
            return;
        }
    }
}

fn build_cells(
    mut loops: Vec<ArrangementLoop>,
    precision: &PrecisionPolicy,
) -> Vec<ArrangementCell> {
    loops.sort_by(|first, second| {
        second
            .signed_area
            .total_cmp(&first.signed_area)
            .then_with(|| {
                canonical_cycle(&first.fragment_keys).cmp(&canonical_cycle(&second.fragment_keys))
            })
    });
    let sample_points: Vec<_> = loops
        .iter()
        .map(|profile_loop| interior_sample(profile_loop, precision))
        .collect();
    let mut parent = vec![None; loops.len()];
    for child in 0..loops.len() {
        let Some(sample) = sample_points[child] else {
            continue;
        };
        let mut best: Option<(usize, f64)> = None;
        for candidate in 0..loops.len() {
            if candidate == child || loops[candidate].signed_area <= loops[child].signed_area {
                continue;
            }
            if point_in_loop(sample, &loops[candidate], precision) {
                let area = loops[candidate].signed_area;
                if best.is_none_or(|(_, best_area)| area < best_area) {
                    best = Some((candidate, area));
                }
            }
        }
        parent[child] = best.map(|(index, _)| index);
    }

    let mut cells = Vec::with_capacity(loops.len());
    for outer_index in 0..loops.len() {
        let hole_indices: Vec<_> = parent
            .iter()
            .enumerate()
            .filter_map(|(index, loop_parent)| (*loop_parent == Some(outer_index)).then_some(index))
            .collect();
        let holes: Vec<_> = hole_indices
            .iter()
            .map(|index| reverse_loop(&loops[*index]))
            .collect();
        let outer_signature = canonical_cycle(&loops[outer_index].fragment_keys);
        let mut hole_signatures: Vec<_> = holes
            .iter()
            .map(|hole| canonical_cycle(&hole.fragment_keys))
            .collect();
        hole_signatures.sort();
        let signed_area = loops[outer_index].signed_area
            - hole_indices
                .iter()
                .map(|index| loops[*index].signed_area)
                .sum::<f64>();
        cells.push(ArrangementCell {
            signature: RegionSignature {
                outer: outer_signature,
                holes: hole_signatures,
            },
            outer: loops[outer_index].clone(),
            holes,
            signed_area,
        });
    }
    cells
}

fn reverse_loop(profile_loop: &ArrangementLoop) -> ArrangementLoop {
    ArrangementLoop {
        half_edges: profile_loop.half_edges.iter().rev().copied().collect(),
        curves: profile_loop
            .curves
            .iter()
            .rev()
            .map(|curve| curve.reverse())
            .collect(),
        fragment_keys: profile_loop
            .fragment_keys
            .iter()
            .rev()
            .map(FragmentKey::reversed)
            .collect(),
        signed_area: -profile_loop.signed_area,
    }
}

fn canonical_cycle(keys: &[FragmentKey]) -> Vec<FragmentKey> {
    if keys.is_empty() {
        return Vec::new();
    }
    let forward = minimal_rotation(keys.to_vec());
    let reverse = minimal_rotation(keys.iter().rev().map(FragmentKey::reversed).collect());
    forward.min(reverse)
}

fn minimal_rotation(keys: Vec<FragmentKey>) -> Vec<FragmentKey> {
    let mut best = keys.clone();
    for offset in 1..keys.len() {
        let candidate: Vec<_> = keys[offset..]
            .iter()
            .chain(keys[..offset].iter())
            .cloned()
            .collect();
        if candidate < best {
            best = candidate;
        }
    }
    best
}

fn interior_sample(
    profile_loop: &ArrangementLoop,
    precision: &PrecisionPolicy,
) -> Option<SketchPoint2> {
    let curve = profile_loop.curves.first()?;
    let midpoint_parameter = if curve.is_periodic() { 0.125 } else { 0.5 };
    let point = curve.evaluate(midpoint_parameter).ok()?;
    let tangent = curve.tangent(midpoint_parameter).ok()?.normalized()?;
    let offset = precision
        .min_feature_size
        .max(precision.modeling_resolution * 8.0);
    Some(point + tangent.left_normal() * offset)
}

/// Slope of the parity ray in [`point_in_loop`]: an unremarkable number, so
/// that no authored direction is likely to run along it.
const RAY_LEAN: f64 = 0.003_713;

fn point_in_loop(
    point: SketchPoint2,
    profile_loop: &ArrangementLoop,
    precision: &PrecisionPolicy,
) -> bool {
    let max_u = profile_loop
        .curves
        .iter()
        .map(|curve| curve.bounds().max.u)
        .fold(point.u + 1.0, f64::max);
    // The ray leans a little rather than running level: sketches are full of
    // horizontal sides and quadrant points, and a level ray from a snapped
    // point runs straight through them, where a crossing and a touch count
    // the same. The lean is small enough that the far end still clears the
    // loop's extent in `u`.
    let reach = max_u + (max_u - point.u).abs().max(1.0) * 2.0 - point.u;
    let ray_end = SketchPoint2::new(point.u + reach, point.v + reach * RAY_LEAN);
    let ray = EvaluatedCurve2::Line {
        start: point,
        end: ray_end,
    };
    let mut parameters = Vec::new();
    for curve in &profile_loop.curves {
        if let CurveIntersections::Points { intersections } =
            crate::intersect_curves(ray.clone(), curve.clone(), precision)
        {
            for intersection in intersections {
                if intersection.first_parameter > precision.parameter_resolution {
                    parameters.push(intersection.first_parameter);
                }
            }
        }
    }
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|second, first| {
        (*first - *second).abs() <= precision.parameter_resolution.max(f64::EPSILON * 64.0)
    });
    parameters.len() % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(raw: u64) -> SketchPointId {
        SketchPointId::new(raw).unwrap()
    }

    fn entity(raw: u64) -> SketchEntityId {
        SketchEntityId::new(raw).unwrap()
    }

    fn rectangle(
        base_entity: u64,
        base_point: u64,
        min: (f64, f64),
        max: (f64, f64),
    ) -> Vec<ArrangementInputCurve> {
        let coordinates = [
            SketchPoint2::new(min.0, min.1),
            SketchPoint2::new(max.0, min.1),
            SketchPoint2::new(max.0, max.1),
            SketchPoint2::new(min.0, max.1),
        ];
        (0..4)
            .map(|index| {
                ArrangementInputCurve::line(
                    entity(base_entity + index as u64),
                    point(base_point + index as u64),
                    point(base_point + ((index + 1) % 4) as u64),
                    coordinates[index],
                    coordinates[(index + 1) % 4],
                )
            })
            .collect()
    }

    #[test]
    fn rectangle_produces_one_canonical_cell() {
        let curves = rectangle(1, 1, (0.0, 0.0), (4.0, 3.0));
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 1, "{:?}", arrangement.diagnostics);
        assert!((arrangement.cells[0].signed_area - 12.0).abs() < 1.0e-9);
        assert_eq!(arrangement.cells[0].outer.curves.len(), 4);
    }

    #[test]
    fn open_geometry_does_not_suppress_rectangle() {
        let mut curves = rectangle(1, 1, (0.0, 0.0), (4.0, 3.0));
        curves.push(ArrangementInputCurve::line(
            entity(10),
            point(10),
            point(11),
            SketchPoint2::new(8.0, 0.0),
            SketchPoint2::new(9.0, 2.0),
        ));
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 1);
    }

    #[test]
    fn complete_circle_is_preserved_as_one_analytic_cell() {
        let curves = [ArrangementInputCurve::circle(
            entity(1),
            SketchPoint2::new(2.0, 3.0),
            5.0,
            CurveDirection::Clockwise,
        )];
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 1);
        assert!(matches!(
            arrangement.cells[0].outer.curves[0],
            EvaluatedCurve2::Circle {
                direction: CurveDirection::CounterClockwise,
                ..
            }
        ));
    }

    #[test]
    fn nested_circles_create_annulus_and_inner_disk_cells() {
        let curves = [
            ArrangementInputCurve::circle(
                entity(1),
                SketchPoint2::new(0.0, 0.0),
                5.0,
                CurveDirection::CounterClockwise,
            ),
            ArrangementInputCurve::circle(
                entity(2),
                SketchPoint2::new(0.0, 0.0),
                2.0,
                CurveDirection::CounterClockwise,
            ),
        ];
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 2);
        assert!(arrangement.cells.iter().any(|cell| cell.holes.len() == 1));
    }

    fn cell_areas(arrangement: &SketchArrangement) -> Vec<f64> {
        let mut areas: Vec<_> = arrangement
            .cells
            .iter()
            .map(|cell| cell.signed_area)
            .collect();
        areas.sort_by(f64::total_cmp);
        areas
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1.0e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn a_circle_resting_on_a_square_side_yields_the_disc_and_the_surround() {
        // With grid snap on, a circle drawn inside a square lands exactly
        // tangent to a side more often than not. That is two regions, not
        // an unusable sketch.
        let mut curves = rectangle(1, 1, (-2.0, -2.0), (2.0, 2.0));
        curves.push(ArrangementInputCurve::circle(
            entity(10),
            SketchPoint2::new(0.0, -1.0),
            1.0,
            CurveDirection::CounterClockwise,
        ));
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        assert!(
            arrangement.diagnostics.is_empty(),
            "{:?}",
            arrangement.diagnostics
        );
        let areas = cell_areas(&arrangement);
        assert_close(areas[0], std::f64::consts::PI);
        assert_close(areas[1], 16.0 - std::f64::consts::PI);
        // The tangent point pinches the surround into one loop; the disc is
        // no hole of it, so a point in the disc resolves to the disc alone.
        let disc = arrangement
            .cell_at_point(SketchPoint2::new(0.3, -1.2), &PrecisionPolicy::default())
            .expect("the disc is a cell");
        assert_close(disc.signed_area, std::f64::consts::PI);
        let surround = arrangement
            .cell_at_point(SketchPoint2::new(1.5, 1.5), &PrecisionPolicy::default())
            .expect("the surround is a cell");
        assert_close(surround.signed_area, 16.0 - std::f64::consts::PI);
    }

    #[test]
    fn an_inscribed_circle_pinches_the_surround_into_four_corner_cells() {
        // Four tangent points leave four corner regions that meet the disc
        // only at points; each is its own minimal cell.
        let mut curves = rectangle(1, 1, (-2.0, -2.0), (2.0, 2.0));
        curves.push(ArrangementInputCurve::circle(
            entity(10),
            SketchPoint2::new(0.0, 0.0),
            2.0,
            CurveDirection::CounterClockwise,
        ));
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 5, "{:?}", arrangement.diagnostics);
        assert!(
            arrangement.diagnostics.is_empty(),
            "{:?}",
            arrangement.diagnostics
        );
        let areas = cell_areas(&arrangement);
        for corner in &areas[..4] {
            assert_close(*corner, 4.0 - std::f64::consts::PI);
        }
        assert_close(areas[4], 4.0 * std::f64::consts::PI);
    }

    #[test]
    fn the_two_arcs_of_a_chord_split_circle_have_distinct_keys() {
        let curves = [
            ArrangementInputCurve::circle(
                entity(1),
                SketchPoint2::new(0.0, 0.0),
                2.0,
                CurveDirection::CounterClockwise,
            ),
            ArrangementInputCurve::line(
                entity(2),
                point(1),
                point(2),
                SketchPoint2::new(-3.0, 0.5),
                SketchPoint2::new(3.0, 0.5),
            ),
        ];
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        let arcs: Vec<_> = arrangement
            .fragments
            .iter()
            .filter(|fragment| fragment.key.source_entity == entity(1))
            .map(|fragment| fragment.key.clone())
            .collect();
        assert_eq!(arcs.len(), 2);
        assert_ne!(arcs[0], arcs[1]);
        assert_ne!(arcs[0].reversed(), arcs[1]);
        assert!(arcs.iter().all(|key| key.sense.is_some()));
        // Straight fragments and unsplit circles keep their earlier keys.
        assert!(
            arrangement
                .fragments
                .iter()
                .filter(|fragment| fragment.key.source_entity == entity(2))
                .all(|fragment| fragment.key.sense.is_none())
        );
        // A signature written before `sense` existed still resolves.
        let legacy = RegionSignature {
            outer: arrangement.cells[0]
                .signature
                .outer
                .iter()
                .map(FragmentKey::without_sense)
                .collect(),
            holes: Vec::new(),
        };
        assert_eq!(
            arrangement.cell(&legacy).map(|cell| &cell.signature),
            Some(&arrangement.cells[0].signature)
        );
    }

    #[test]
    fn a_line_ending_on_a_circle_leaves_the_disc_as_a_cell() {
        // One junction on a closed curve used to drop the circle entirely.
        for line_start in [SketchPoint2::new(0.0, 0.0), SketchPoint2::new(5.0, 0.0)] {
            let curves = [
                ArrangementInputCurve::circle(
                    entity(1),
                    SketchPoint2::new(0.0, 0.0),
                    2.0,
                    CurveDirection::CounterClockwise,
                ),
                ArrangementInputCurve::line(
                    entity(2),
                    point(1),
                    point(2),
                    line_start,
                    SketchPoint2::new(2.0, 0.0),
                ),
            ];
            let arrangement = build_arrangement(
                &curves,
                &PrecisionPolicy::default(),
                ArrangementLimits::default(),
            );
            assert_eq!(arrangement.cells.len(), 1, "{:?}", arrangement.diagnostics);
            assert_close(arrangement.cells[0].signed_area, 4.0 * std::f64::consts::PI);
            assert_eq!(arrangement.cells[0].outer.curves.len(), 2);
            assert!(arrangement.junctions.iter().any(|junction| {
                junction
                    .key
                    .keys()
                    .iter()
                    .any(|key| matches!(key, JunctionKey::PeriodicSplit { .. }))
            }));
        }
    }

    #[test]
    fn two_lines_tangent_to_a_circle_close_a_teardrop_beside_the_disc() {
        // Apex (0, 4), circle radius 2 at the origin: the tangent points are
        // at (±√3, 1) and both lines meet the rim tangentially at their ends.
        let root = 3.0_f64.sqrt();
        let curves = [
            ArrangementInputCurve::circle(
                entity(1),
                SketchPoint2::new(0.0, 0.0),
                2.0,
                CurveDirection::CounterClockwise,
            ),
            ArrangementInputCurve::line(
                entity(2),
                point(1),
                point(2),
                SketchPoint2::new(0.0, 4.0),
                SketchPoint2::new(-root, 1.0),
            ),
            ArrangementInputCurve::line(
                entity(3),
                point(1),
                point(3),
                SketchPoint2::new(0.0, 4.0),
                SketchPoint2::new(root, 1.0),
            ),
        ];
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        let areas = cell_areas(&arrangement);
        // Kite (0,4)-(√3,1)-(0,0)-(-√3,1) minus the 120° sector.
        let kite = 4.0 * root;
        let sector = 4.0 * std::f64::consts::PI / 3.0;
        assert_close(areas[0], kite - sector);
        assert_close(areas[1], 4.0 * std::f64::consts::PI);
    }

    #[test]
    fn externally_tangent_circles_are_two_separate_discs() {
        let curves = [
            ArrangementInputCurve::circle(
                entity(1),
                SketchPoint2::new(0.0, 0.0),
                1.0,
                CurveDirection::CounterClockwise,
            ),
            ArrangementInputCurve::circle(
                entity(2),
                SketchPoint2::new(3.0, 0.0),
                2.0,
                CurveDirection::CounterClockwise,
            ),
        ];
        let arrangement = build_arrangement(
            &curves,
            &PrecisionPolicy::default(),
            ArrangementLimits::default(),
        );
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        let areas = cell_areas(&arrangement);
        assert_close(areas[0], std::f64::consts::PI);
        assert_close(areas[1], 4.0 * std::f64::consts::PI);
        assert!(arrangement.cells.iter().all(|cell| cell.holes.is_empty()));
    }
}

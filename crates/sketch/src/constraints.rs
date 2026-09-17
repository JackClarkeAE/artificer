//! Persisted geometric constraints and a deterministic bounded projection solver.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{SketchConstraintId, SketchPoint2, SketchPointId};

pub const MAX_SKETCH_CONSTRAINTS: usize = 2_048;
const MAX_SOLVER_ITERATIONS: usize = 192;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SketchConstraintKind {
    Fixed {
        point: SketchPointId,
        position: SketchPoint2,
    },
    Coincident {
        first: SketchPointId,
        second: SketchPointId,
    },
    Horizontal {
        first: SketchPointId,
        second: SketchPointId,
    },
    Vertical {
        first: SketchPointId,
        second: SketchPointId,
    },
    Distance {
        first: SketchPointId,
        second: SketchPointId,
        distance: f64,
    },
    Parallel {
        first_start: SketchPointId,
        first_end: SketchPointId,
        second_start: SketchPointId,
        second_end: SketchPointId,
    },
    Perpendicular {
        first_start: SketchPointId,
        first_end: SketchPointId,
        second_start: SketchPointId,
        second_end: SketchPointId,
    },
    EqualLength {
        first_start: SketchPointId,
        first_end: SketchPointId,
        second_start: SketchPointId,
        second_end: SketchPointId,
    },
    Tangent {
        first_start: SketchPointId,
        first_end: SketchPointId,
        second_start: SketchPointId,
        second_end: SketchPointId,
    },
    Collinear {
        first: SketchPointId,
        second: SketchPointId,
        third: SketchPointId,
    },
    /// The line through `start` and `end` touches the circle about `center`
    /// of the given radius. The radius is a literal because a circle's is.
    LineTangentToCircle {
        start: SketchPointId,
        end: SketchPointId,
        center: SketchPointId,
        radius: f64,
    },
    /// The line through `start` and `end` touches the circle about `center`
    /// that passes through `rim`, which is how an arc carries its radius.
    LineTangentToArc {
        start: SketchPointId,
        end: SketchPointId,
        center: SketchPointId,
        rim: SketchPointId,
    },
    /// `point` stands `distance` away from the line through `start` and `end`,
    /// measured along that line's normal.
    ///
    /// This is the ordinate a drawing is mostly made of — how far a hole sits
    /// from an edge — and it is a different statement from a distance to that
    /// edge's corner. A distance to a corner is a radius, so it leaves the
    /// point anywhere on a circle and pairs of them meet in two places or in
    /// none. An offset from the line leaves the point anywhere along it, and
    /// two offsets from two edges meet in exactly one place.
    ///
    /// It is the *line* that is measured from, not the segment: a dimension to
    /// an edge does not stop being meaningful where the point is past the end
    /// of it, and the foot of the perpendicular is where the witness line is
    /// drawn to.
    PointToLineDistance {
        point: SketchPointId,
        start: SketchPointId,
        end: SketchPointId,
        distance: f64,
    },
    /// `point` stands `distance` away from the midpoint of `start` and `end`.
    ///
    /// The midpoint of an edge is the feature a drawing centres things on, and
    /// it is not a point the sketch owns — an edge carries its two ends and
    /// nothing between them — so the relation names the ends and measures to
    /// the middle of them.
    PointToMidpointDistance {
        point: SketchPointId,
        start: SketchPointId,
        end: SketchPointId,
        distance: f64,
    },
    /// The line through `first_start`/`first_end` and the line through
    /// `second_start`/`second_end` stand `distance` apart.
    ///
    /// Two lines only have a distance when they are parallel; anywhere else
    /// they meet, and the number would depend on where along them it was
    /// measured. The relation holds the separation and leaves the parallelism
    /// to a parallel relation, so that staging one on lines that are not
    /// parallel is refused by name rather than answered with a number that
    /// means nothing.
    LineToLineDistance {
        first_start: SketchPointId,
        first_end: SketchPointId,
        second_start: SketchPointId,
        second_end: SketchPointId,
        distance: f64,
    },
}

impl SketchConstraintKind {
    #[must_use]
    pub fn referenced_points(&self) -> Vec<SketchPointId> {
        match *self {
            Self::Fixed { point, .. } => vec![point],
            Self::Coincident { first, second }
            | Self::Horizontal { first, second }
            | Self::Vertical { first, second }
            | Self::Distance { first, second, .. } => vec![first, second],
            Self::Collinear {
                first,
                second,
                third,
            } => vec![first, second, third],
            Self::LineTangentToCircle {
                start, end, center, ..
            } => vec![start, end, center],
            Self::LineTangentToArc {
                start,
                end,
                center,
                rim,
            } => vec![start, end, center, rim],
            Self::PointToLineDistance {
                point, start, end, ..
            }
            | Self::PointToMidpointDistance {
                point, start, end, ..
            } => vec![point, start, end],
            Self::LineToLineDistance {
                first_start,
                first_end,
                second_start,
                second_end,
                ..
            } => vec![first_start, first_end, second_start, second_end],
            Self::Parallel {
                first_start,
                first_end,
                second_start,
                second_end,
            }
            | Self::Perpendicular {
                first_start,
                first_end,
                second_start,
                second_end,
            }
            | Self::EqualLength {
                first_start,
                first_end,
                second_start,
                second_end,
            }
            | Self::Tangent {
                first_start,
                first_end,
                second_start,
                second_end,
            } => {
                vec![first_start, first_end, second_start, second_end]
            }
        }
    }

    /// The number this relation holds, for the kinds that hold one.
    ///
    /// A relation either states a measurement the user chose — a distance, a
    /// tangent circle's radius — or states a pure relationship between points.
    /// Only the first sort can be drawn as a dimension and typed into, and this
    /// is what tells them apart.
    #[must_use]
    pub const fn measurement(&self) -> Option<f64> {
        match *self {
            Self::Distance { distance, .. }
            | Self::PointToLineDistance { distance, .. }
            | Self::PointToMidpointDistance { distance, .. }
            | Self::LineToLineDistance { distance, .. } => Some(distance),
            Self::LineTangentToCircle { radius, .. } => Some(radius),
            _ => None,
        }
    }

    /// The points a retype has to hold still for the new number to mean what
    /// the user just typed.
    ///
    /// A distance between two points has no datum — either end may be the one
    /// that moves, and the caller says which by naming it. A distance measured
    /// *from* something does: an offset is from an edge, so retyping it must
    /// move the point and leave the edge alone. Holding one end of that edge is
    /// not enough, because the projection is free to share the correction with
    /// the other end and tilt it; the edge has to be held whole.
    #[must_use]
    pub fn datum_points(&self) -> Vec<SketchPointId> {
        match *self {
            Self::PointToLineDistance { start, end, .. }
            | Self::PointToMidpointDistance { start, end, .. } => vec![start, end],
            Self::LineToLineDistance {
                first_start,
                first_end,
                ..
            } => vec![first_start, first_end],
            _ => Vec::new(),
        }
    }

    /// The same relation, restated to hold `measurement` instead.
    ///
    /// Returns nothing for a relation that holds no number, so a caller cannot
    /// quietly turn one kind of relation into another by typing into it.
    #[must_use]
    pub const fn with_measurement(&self, measurement: f64) -> Option<Self> {
        match *self {
            Self::Distance { first, second, .. } => Some(Self::Distance {
                first,
                second,
                distance: measurement,
            }),
            Self::LineTangentToCircle {
                start, end, center, ..
            } => Some(Self::LineTangentToCircle {
                start,
                end,
                center,
                radius: measurement,
            }),
            Self::PointToLineDistance {
                point, start, end, ..
            } => Some(Self::PointToLineDistance {
                point,
                start,
                end,
                distance: measurement,
            }),
            Self::PointToMidpointDistance {
                point, start, end, ..
            } => Some(Self::PointToMidpointDistance {
                point,
                start,
                end,
                distance: measurement,
            }),
            Self::LineToLineDistance {
                first_start,
                first_end,
                second_start,
                second_end,
                ..
            } => Some(Self::LineToLineDistance {
                first_start,
                first_end,
                second_start,
                second_end,
                distance: measurement,
            }),
            _ => None,
        }
    }

    #[must_use]
    pub const fn equation_count(&self) -> usize {
        match self {
            Self::Fixed { .. } | Self::Coincident { .. } => 2,
            Self::Horizontal { .. }
            | Self::Vertical { .. }
            | Self::Distance { .. }
            | Self::Parallel { .. }
            | Self::Perpendicular { .. }
            | Self::EqualLength { .. }
            | Self::Tangent { .. }
            | Self::Collinear { .. }
            | Self::LineTangentToCircle { .. }
            | Self::LineTangentToArc { .. }
            | Self::PointToLineDistance { .. }
            | Self::PointToMidpointDistance { .. }
            | Self::LineToLineDistance { .. } => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchConstraintRecord {
    pub id: SketchConstraintId,
    pub kind: SketchConstraintKind,
    #[serde(default = "constraint_enabled")]
    pub enabled: bool,
}

const fn constraint_enabled() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintSolveStatus {
    FullyConstrained,
    UnderConstrained { remaining_degrees_of_freedom: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConstraintSolution {
    pub positions: BTreeMap<SketchPointId, SketchPoint2>,
    pub status: ConstraintSolveStatus,
    pub iterations: usize,
    pub maximum_residual: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConstraintError {
    NonFiniteValue,
    NonPositiveDistance,
    MissingPoint(SketchPointId),
    InactivePoint(SketchPointId),
    DuplicatePoint(SketchPointId),
    MissingConstraint(SketchConstraintId),
    Conflicting { maximum_residual: f64 },
    IdSpaceExhausted,
}

impl fmt::Display for ConstraintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteValue => formatter.write_str("constraint values must be finite"),
            Self::NonPositiveDistance => {
                formatter.write_str("constraint distance must be positive")
            }
            Self::MissingPoint(point) => {
                write!(formatter, "constraint point {point} does not exist")
            }
            Self::InactivePoint(point) => write!(formatter, "constraint point {point} is inactive"),
            Self::DuplicatePoint(point) => write!(formatter, "constraint repeats point {point}"),
            Self::MissingConstraint(constraint) => {
                write!(formatter, "relation {constraint} does not exist")
            }
            Self::Conflicting { maximum_residual } => write!(
                formatter,
                "constraint system is conflicting (residual {maximum_residual:.3e})"
            ),
            Self::IdSpaceExhausted => formatter.write_str("constraint ID space is exhausted"),
        }
    }
}

impl std::error::Error for ConstraintError {}

pub(crate) fn validate_constraint(kind: &SketchConstraintKind) -> Result<(), ConstraintError> {
    let points = kind.referenced_points();
    let mut unique = BTreeSet::new();
    for point in points {
        if !unique.insert(point) {
            return Err(ConstraintError::DuplicatePoint(point));
        }
    }
    match kind {
        SketchConstraintKind::Fixed { position, .. } if !position.is_finite() => {
            Err(ConstraintError::NonFiniteValue)
        }
        SketchConstraintKind::Distance { distance, .. } if !distance.is_finite() => {
            Err(ConstraintError::NonFiniteValue)
        }
        SketchConstraintKind::Distance { distance, .. } if *distance <= 0.0 => {
            Err(ConstraintError::NonPositiveDistance)
        }
        SketchConstraintKind::LineTangentToCircle { radius, .. } if !radius.is_finite() => {
            Err(ConstraintError::NonFiniteValue)
        }
        SketchConstraintKind::LineTangentToCircle { radius, .. } if *radius <= 0.0 => {
            Err(ConstraintError::NonPositiveDistance)
        }
        SketchConstraintKind::PointToLineDistance { distance, .. }
        | SketchConstraintKind::PointToMidpointDistance { distance, .. }
        | SketchConstraintKind::LineToLineDistance { distance, .. }
            if !distance.is_finite() =>
        {
            Err(ConstraintError::NonFiniteValue)
        }
        // An offset of zero is a point *on* the line, which is what a collinear
        // relation says; a separation of zero is two lines on top of one
        // another. Neither is a dimension, and both leave the normal these
        // project along undefined.
        SketchConstraintKind::PointToLineDistance { distance, .. }
        | SketchConstraintKind::PointToMidpointDistance { distance, .. }
        | SketchConstraintKind::LineToLineDistance { distance, .. }
            if *distance <= 0.0 =>
        {
            Err(ConstraintError::NonPositiveDistance)
        }
        _ => Ok(()),
    }
}

/// Solves the constraint system, holding `anchored` points where their seeds
/// already put them.
///
/// Anchoring exists because a relation and a deliberate edit are not equal
/// authorities. Two coincident points that are both free meet in the middle,
/// which is the fair answer when nothing distinguishes them; but when the user
/// has just dragged one of them, or typed the angle that placed it, that point
/// is where they said it should be and the other one is what has to give. The
/// editing paths anchor the points the edit authored and let everything else
/// follow. `Fixed` still outranks an anchor: a pinned point is pinned whoever
/// is pulling at it.
pub(crate) fn solve(
    seeds: &BTreeMap<SketchPointId, SketchPoint2>,
    constraints: impl Iterator<Item = SketchConstraintRecord>,
    tolerance: f64,
    anchored: &BTreeSet<SketchPointId>,
) -> Result<ConstraintSolution, ConstraintError> {
    let constraints = constraints
        .filter(|record| record.enabled)
        .collect::<Vec<_>>();
    let mut positions = seeds.clone();
    let mut pinned = anchored
        .iter()
        .filter_map(|point| seeds.get(point).map(|position| (*point, *position)))
        .collect::<BTreeMap<_, _>>();
    pinned.extend(constraints.iter().filter_map(|record| match record.kind {
        SketchConstraintKind::Fixed { point, position } => Some((point, position)),
        _ => None,
    }));
    for (point, position) in &pinned {
        if positions.contains_key(point) {
            positions.insert(*point, *position);
        }
    }

    let threshold = tolerance.max(1.0e-10);
    for iteration in 0..MAX_SOLVER_ITERATIONS {
        for record in &constraints {
            if record
                .kind
                .referenced_points()
                .iter()
                .any(|id| !positions.contains_key(id))
            {
                continue;
            }
            project(&mut positions, &pinned, &record.kind, threshold);
        }
        for (point, position) in &pinned {
            positions.insert(*point, *position);
        }
        let maximum_residual = constraints
            .iter()
            .filter(|record| {
                record
                    .kind
                    .referenced_points()
                    .iter()
                    .all(|id| positions.contains_key(id))
            })
            .map(|record| residual(&positions, &record.kind))
            .fold(0.0_f64, f64::max);
        if maximum_residual <= threshold {
            let equations = constraints
                .iter()
                .map(|record| record.kind.equation_count())
                .sum::<usize>();
            let remaining = positions.len().saturating_mul(2).saturating_sub(equations);
            return Ok(ConstraintSolution {
                positions,
                status: if remaining == 0 {
                    ConstraintSolveStatus::FullyConstrained
                } else {
                    ConstraintSolveStatus::UnderConstrained {
                        remaining_degrees_of_freedom: remaining,
                    }
                },
                iterations: iteration + 1,
                maximum_residual,
            });
        }
    }
    let maximum_residual = constraints
        .iter()
        .filter(|record| {
            record
                .kind
                .referenced_points()
                .iter()
                .all(|id| positions.contains_key(id))
        })
        .map(|record| residual(&positions, &record.kind))
        .fold(0.0_f64, f64::max);
    Err(ConstraintError::Conflicting { maximum_residual })
}

fn point(positions: &BTreeMap<SketchPointId, SketchPoint2>, id: SketchPointId) -> SketchPoint2 {
    positions[&id]
}

fn movable(pinned: &BTreeMap<SketchPointId, SketchPoint2>, id: SketchPointId) -> bool {
    !pinned.contains_key(&id)
}

fn set_pair_coordinate(
    positions: &mut BTreeMap<SketchPointId, SketchPoint2>,
    pinned: &BTreeMap<SketchPointId, SketchPoint2>,
    first: SketchPointId,
    second: SketchPointId,
    horizontal: bool,
) {
    let a = point(positions, first);
    let b = point(positions, second);
    let target = if horizontal {
        (a.v + b.v) * 0.5
    } else {
        (a.u + b.u) * 0.5
    };
    let first_movable = movable(pinned, first);
    let second_movable = movable(pinned, second);
    if first_movable {
        let value = if second_movable {
            target
        } else if horizontal {
            b.v
        } else {
            b.u
        };
        let p = positions.get_mut(&first).expect("checked");
        if horizontal {
            p.v = value;
        } else {
            p.u = value;
        }
    }
    if second_movable {
        let value = if first_movable {
            target
        } else if horizontal {
            a.v
        } else {
            a.u
        };
        let p = positions.get_mut(&second).expect("checked");
        if horizontal {
            p.v = value;
        } else {
            p.u = value;
        }
    }
}

fn set_segment(
    positions: &mut BTreeMap<SketchPointId, SketchPoint2>,
    pinned: &BTreeMap<SketchPointId, SketchPoint2>,
    start: SketchPointId,
    end: SketchPointId,
    direction: (f64, f64),
    length: f64,
) {
    let a = point(positions, start);
    let b = point(positions, end);
    let start_movable = movable(pinned, start);
    let end_movable = movable(pinned, end);
    if start_movable && end_movable {
        let mid = SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5);
        let half = length * 0.5;
        positions.insert(
            start,
            SketchPoint2::new(mid.u - direction.0 * half, mid.v - direction.1 * half),
        );
        positions.insert(
            end,
            SketchPoint2::new(mid.u + direction.0 * half, mid.v + direction.1 * half),
        );
    } else if start_movable {
        positions.insert(
            start,
            SketchPoint2::new(b.u - direction.0 * length, b.v - direction.1 * length),
        );
    } else if end_movable {
        positions.insert(
            end,
            SketchPoint2::new(a.u + direction.0 * length, a.v + direction.1 * length),
        );
    }
}

fn normalized(delta: (f64, f64), fallback: (f64, f64)) -> (f64, f64) {
    let length = delta.0.hypot(delta.1);
    if length > 1.0e-14 {
        (delta.0 / length, delta.1 / length)
    } else {
        fallback
    }
}

fn project(
    positions: &mut BTreeMap<SketchPointId, SketchPoint2>,
    pinned: &BTreeMap<SketchPointId, SketchPoint2>,
    kind: &SketchConstraintKind,
    tolerance: f64,
) {
    match *kind {
        SketchConstraintKind::Fixed { point, position } => {
            positions.insert(point, position);
        }
        SketchConstraintKind::Coincident { first, second } => {
            let a = point(positions, first);
            let b = point(positions, second);
            let first_movable = movable(pinned, first);
            let second_movable = movable(pinned, second);
            if first_movable && second_movable {
                let mid = SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5);
                positions.insert(first, mid);
                positions.insert(second, mid);
            } else if first_movable {
                positions.insert(first, b);
            } else if second_movable {
                positions.insert(second, a);
            }
        }
        SketchConstraintKind::Horizontal { first, second } => {
            set_pair_coordinate(positions, pinned, first, second, true)
        }
        SketchConstraintKind::Vertical { first, second } => {
            set_pair_coordinate(positions, pinned, first, second, false)
        }
        SketchConstraintKind::PointToLineDistance {
            point: held,
            start,
            end,
            distance,
        } => project_point_off_line(positions, pinned, start, end, held, distance),
        SketchConstraintKind::PointToMidpointDistance {
            point: held,
            start,
            end,
            distance,
        } => {
            // The midpoint is not a point anything can move, so the correction
            // goes to the point when it can move and to the edge's two ends
            // together when it cannot — moving both ends the same way carries
            // the midpoint with them and leaves the edge's length and
            // direction alone.
            let p = point(positions, held);
            let (a, b) = (point(positions, start), point(positions, end));
            let mid = SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5);
            let away = p - mid;
            let length = away.length();
            let direction = if length <= 1.0e-14 {
                (1.0, 0.0)
            } else {
                (away.u / length, away.v / length)
            };
            let delta = distance - length;
            if movable(pinned, held) {
                positions.insert(
                    held,
                    SketchPoint2::new(p.u + direction.0 * delta, p.v + direction.1 * delta),
                );
            } else if movable(pinned, start) && movable(pinned, end) {
                for (id, at) in [(start, a), (end, b)] {
                    positions.insert(
                        id,
                        SketchPoint2::new(at.u - direction.0 * delta, at.v - direction.1 * delta),
                    );
                }
            }
        }
        SketchConstraintKind::LineToLineDistance {
            first_start,
            first_end,
            second_start,
            second_end,
            distance,
        } => {
            // Hold each end of the second line the same offset off the first.
            // Two parallel lines then stand the asked distance apart; two that
            // are not parallel are refused when the relation is staged, so the
            // projection never has to decide what their distance would mean.
            for held in [second_start, second_end] {
                project_point_off_line(positions, pinned, first_start, first_end, held, distance);
            }
        }
        SketchConstraintKind::Distance {
            first,
            second,
            distance,
        } => {
            let a = point(positions, first);
            let b = point(positions, second);
            let direction = normalized((b.u - a.u, b.v - a.v), (1.0, 0.0));
            set_segment(
                positions,
                pinned,
                first,
                second,
                direction,
                distance.max(tolerance),
            );
        }
        SketchConstraintKind::Parallel {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_start);
            let b = point(positions, first_end);
            let c = point(positions, second_start);
            let d = point(positions, second_end);
            let direction = normalized((b.u - a.u, b.v - a.v), (1.0, 0.0));
            set_segment(
                positions,
                pinned,
                second_start,
                second_end,
                direction,
                c.distance(d),
            );
        }
        SketchConstraintKind::Perpendicular {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_start);
            let b = point(positions, first_end);
            let c = point(positions, second_start);
            let d = point(positions, second_end);
            let direction = normalized((-(b.v - a.v), b.u - a.u), (0.0, 1.0));
            set_segment(
                positions,
                pinned,
                second_start,
                second_end,
                direction,
                c.distance(d),
            );
        }
        SketchConstraintKind::EqualLength {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_start);
            let b = point(positions, first_end);
            let c = point(positions, second_start);
            let d = point(positions, second_end);
            let direction = normalized((d.u - c.u, d.v - c.v), (1.0, 0.0));
            set_segment(
                positions,
                pinned,
                second_start,
                second_end,
                direction,
                a.distance(b),
            );
        }
        SketchConstraintKind::Tangent {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_start);
            let b = point(positions, first_end);
            let c = point(positions, second_start);
            let d = point(positions, second_end);
            let direction = normalized((b.u - a.u, b.v - a.v), (1.0, 0.0));
            set_segment(
                positions,
                pinned,
                second_start,
                second_end,
                direction,
                c.distance(d),
            );
        }
        SketchConstraintKind::Collinear {
            first,
            second,
            third,
        } => {
            let a = point(positions, first);
            let c = point(positions, third);
            let b = point(positions, second);
            let ac = c - a;
            let len_sq = ac.length_squared();
            // The point goes to the infinite line, not the span between the
            // other two: collinear lines lie end to end, not on top of each
            // other, and clamping used to drag the second line onto the
            // first.
            if len_sq > 1.0e-14 && !pinned.contains_key(&second) {
                let t = (b - a).dot(ac) / len_sq;
                positions.insert(second, a + ac * t);
            }
        }
        SketchConstraintKind::LineTangentToCircle {
            start,
            end,
            center,
            radius,
        } => project_line_tangent(positions, pinned, start, end, center, radius),
        SketchConstraintKind::LineTangentToArc {
            start,
            end,
            center,
            rim,
        } => {
            let radius = point(positions, center).distance(point(positions, rim));
            project_line_tangent(positions, pinned, start, end, center, radius);
        }
    }
}

/// The signed distance from `center` to the line through `a` and `b`, and
/// the unit normal it is measured along.
fn line_offset(
    a: SketchPoint2,
    b: SketchPoint2,
    center: SketchPoint2,
) -> Option<(f64, (f64, f64))> {
    let direction = b - a;
    let length = direction.length();
    if length <= 1.0e-14 {
        return None;
    }
    let normal = (-direction.v / length, direction.u / length);
    let offset = (center - a).u * normal.0 + (center - a).v * normal.1;
    Some((offset, normal))
}

/// Where a dimension's witness line runs: the two points whose separation is
/// the number the relation holds.
///
/// Every relation that carries a measurement can say this, and saying it is
/// what lets one drawing path serve all of them. For a distance between two
/// points it is those points; for an offset from a line it is the point and
/// the foot of its perpendicular; for two lines it is a point on one and its
/// foot on the other.
#[must_use]
pub fn dimension_span(
    kind: &SketchConstraintKind,
    positions: &BTreeMap<SketchPointId, SketchPoint2>,
) -> Option<(SketchPoint2, SketchPoint2)> {
    let known = |id: SketchPointId| positions.get(&id).copied();
    match *kind {
        SketchConstraintKind::Distance { first, second, .. } => {
            Some((known(first)?, known(second)?))
        }
        SketchConstraintKind::PointToLineDistance {
            point, start, end, ..
        } => {
            let (p, a, b) = (known(point)?, known(start)?, known(end)?);
            let (offset, normal) = line_offset(a, b, p)?;
            Some((
                p,
                SketchPoint2::new(p.u - normal.0 * offset, p.v - normal.1 * offset),
            ))
        }
        SketchConstraintKind::PointToMidpointDistance {
            point, start, end, ..
        } => {
            let (p, a, b) = (known(point)?, known(start)?, known(end)?);
            Some((p, SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5)))
        }
        SketchConstraintKind::LineToLineDistance {
            first_start,
            first_end,
            second_start,
            second_end,
            ..
        } => {
            let (a, b) = (known(first_start)?, known(first_end)?);
            let c = known(second_start)?;
            let _ = known(second_end)?;
            let (offset, normal) = line_offset(a, b, c)?;
            Some((
                c,
                SketchPoint2::new(c.u - normal.0 * offset, c.v - normal.1 * offset),
            ))
        }
        SketchConstraintKind::LineTangentToCircle {
            start, end, center, ..
        } => {
            let (a, b, c) = (known(start)?, known(end)?, known(center)?);
            let (offset, normal) = line_offset(a, b, c)?;
            Some((
                c,
                SketchPoint2::new(c.u - normal.0 * offset, c.v - normal.1 * offset),
            ))
        }
        _ => None,
    }
}

/// Moves `point` onto the line's normal at `distance`, keeping the side it is
/// already on, and sharing the movement with the line when the line can move.
///
/// This is the same operation as holding a line tangent to a circle — the
/// circle's centre is a point held one radius off the line — so the two share
/// their projection.
fn project_point_off_line(
    positions: &mut BTreeMap<SketchPointId, SketchPoint2>,
    pinned: &BTreeMap<SketchPointId, SketchPoint2>,
    start: SketchPointId,
    end: SketchPointId,
    point: SketchPointId,
    distance: f64,
) {
    project_line_tangent(positions, pinned, start, end, point, distance);
}

/// Slides the line, or failing that the centre, along the line's normal
/// until the centre sits one radius away from it. The circle stays on the
/// side it is already on, so a line tangent to a circle never flips through
/// it to the other side.
fn project_line_tangent(
    positions: &mut BTreeMap<SketchPointId, SketchPoint2>,
    pinned: &BTreeMap<SketchPointId, SketchPoint2>,
    start: SketchPointId,
    end: SketchPointId,
    center: SketchPointId,
    radius: f64,
) {
    let a = point(positions, start);
    let b = point(positions, end);
    let c = point(positions, center);
    let Some((offset, normal)) = line_offset(a, b, c) else {
        return;
    };
    let target = if offset < 0.0 { -radius } else { radius };
    let delta = target - offset;
    let line_movable = movable(pinned, start) || movable(pinned, end);
    let center_movable = movable(pinned, center);
    let (line_share, center_share) = match (line_movable, center_movable) {
        (true, true) => (0.5, 0.5),
        (true, false) => (1.0, 0.0),
        (false, true) => (0.0, 1.0),
        (false, false) => return,
    };
    // Moving the line away from the centre by `delta` lowers the offset.
    let shift = |p: SketchPoint2, amount: f64| {
        SketchPoint2::new(p.u + normal.0 * amount, p.v + normal.1 * amount)
    };
    if movable(pinned, start) {
        positions.insert(start, shift(a, -delta * line_share));
    }
    if movable(pinned, end) {
        positions.insert(end, shift(b, -delta * line_share));
    }
    if center_movable {
        positions.insert(center, shift(c, delta * center_share));
    }
}

fn residual(positions: &BTreeMap<SketchPointId, SketchPoint2>, kind: &SketchConstraintKind) -> f64 {
    match *kind {
        SketchConstraintKind::Fixed {
            point: id,
            position,
        } => point(positions, id).distance(position),
        SketchConstraintKind::Coincident { first, second } => {
            point(positions, first).distance(point(positions, second))
        }
        SketchConstraintKind::Horizontal { first, second } => {
            (point(positions, first).v - point(positions, second).v).abs()
        }
        SketchConstraintKind::Vertical { first, second } => {
            (point(positions, first).u - point(positions, second).u).abs()
        }
        SketchConstraintKind::Distance {
            first,
            second,
            distance,
        } => (point(positions, first).distance(point(positions, second)) - distance).abs(),
        SketchConstraintKind::PointToLineDistance {
            point: held,
            start,
            end,
            distance,
        } => line_offset(
            point(positions, start),
            point(positions, end),
            point(positions, held),
        )
        .map_or(f64::INFINITY, |(offset, _)| (offset.abs() - distance).abs()),
        SketchConstraintKind::PointToMidpointDistance {
            point: held,
            start,
            end,
            distance,
        } => {
            let (a, b) = (point(positions, start), point(positions, end));
            (point(positions, held)
                .distance(SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5))
                - distance)
                .abs()
        }
        SketchConstraintKind::LineToLineDistance {
            first_start,
            first_end,
            second_start,
            second_end,
            distance,
        } => {
            let (a, b) = (point(positions, first_start), point(positions, first_end));
            [second_start, second_end]
                .into_iter()
                .map(|id| {
                    line_offset(a, b, point(positions, id))
                        .map_or(f64::INFINITY, |(offset, _)| (offset.abs() - distance).abs())
                })
                .fold(0.0_f64, f64::max)
        }
        SketchConstraintKind::Parallel {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_end) - point(positions, first_start);
            let b = point(positions, second_end) - point(positions, second_start);
            a.cross(b).abs() / (a.length() * b.length()).max(1.0e-14)
        }
        SketchConstraintKind::Perpendicular {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_end) - point(positions, first_start);
            let b = point(positions, second_end) - point(positions, second_start);
            a.dot(b).abs() / (a.length() * b.length()).max(1.0e-14)
        }
        SketchConstraintKind::EqualLength {
            first_start,
            first_end,
            second_start,
            second_end,
        } => (point(positions, first_start).distance(point(positions, first_end))
            - point(positions, second_start).distance(point(positions, second_end)))
        .abs(),
        SketchConstraintKind::Tangent {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = point(positions, first_end) - point(positions, first_start);
            let b = point(positions, second_end) - point(positions, second_start);
            a.cross(b).abs() / (a.length() * b.length()).max(1.0e-14)
        }
        SketchConstraintKind::Collinear {
            first,
            second,
            third,
        } => {
            let a = point(positions, first);
            let b = point(positions, second);
            let c = point(positions, third);
            let ab = b - a;
            let ac = c - a;
            ab.cross(ac).abs() / (ac.length()).max(1.0e-14)
        }
        SketchConstraintKind::LineTangentToCircle {
            start,
            end,
            center,
            radius,
        } => line_offset(
            point(positions, start),
            point(positions, end),
            point(positions, center),
        )
        .map_or(radius, |(offset, _)| (offset.abs() - radius).abs()),
        SketchConstraintKind::LineTangentToArc {
            start,
            end,
            center,
            rim,
        } => {
            let radius = point(positions, center).distance(point(positions, rim));
            line_offset(
                point(positions, start),
                point(positions, end),
                point(positions, center),
            )
            .map_or(radius, |(offset, _)| (offset.abs() - radius).abs())
        }
    }
}

#[cfg(test)]
mod tests {
    use artificer_protocol::PrecisionPolicy;

    use super::*;
    use crate::{
        ConfirmationSource, PointInput, SignedLength, SketchDefinition, SketchInputValues,
        SketchRecipe, SketchValue,
    };

    fn rectangle() -> SketchDefinition {
        let definition = SketchDefinition::new();
        let transaction = definition
            .stage_with_inputs(
                SketchRecipe::TwoPointRectangle {
                    first_corner: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
                    width: SketchValue::Literal(SignedLength::new(8.0).expect("width")),
                    height: SketchValue::Literal(SignedLength::new(3.0).expect("height")),
                },
                "rectangle",
                &SketchInputValues::default(),
                PrecisionPolicy::default(),
            )
            .expect("stage rectangle");
        let mut committed = definition;
        committed
            .commit(transaction, ConfirmationSource::BareEnter)
            .expect("commit rectangle");
        committed
    }

    #[test]
    fn fixed_distance_and_horizontal_constraints_drive_evaluated_geometry() {
        let mut definition = rectangle();
        let ids = definition
            .active_points()
            .map(|point| point.id)
            .collect::<Vec<_>>();
        definition
            .add_constraint(
                SketchConstraintKind::Fixed {
                    point: ids[0],
                    position: SketchPoint2::new(0.0, 0.0),
                },
                PrecisionPolicy::default(),
            )
            .expect("fix first point");
        definition
            .add_constraint(
                SketchConstraintKind::Horizontal {
                    first: ids[0],
                    second: ids[1],
                },
                PrecisionPolicy::default(),
            )
            .expect("horizontal pair");
        definition
            .add_constraint(
                SketchConstraintKind::Distance {
                    first: ids[0],
                    second: ids[1],
                    distance: 5.0,
                },
                PrecisionPolicy::default(),
            )
            .expect("distance pair");

        let solution = definition
            .solve_constraints(PrecisionPolicy::default())
            .expect("solve");
        let first = solution.positions[&ids[0]];
        let second = solution.positions[&ids[1]];
        assert!((first.v - second.v).abs() < 1.0e-9);
        assert!((first.distance(second) - 5.0).abs() < 1.0e-9);
        assert!(definition.validate(PrecisionPolicy::default()).is_ok());
    }

    #[test]
    fn a_line_slides_until_it_touches_the_circle_it_is_made_tangent_to() {
        let mut definition = rectangle();
        let ids = definition
            .active_points()
            .map(|point| point.id)
            .collect::<Vec<_>>();
        // The rectangle's first side runs along v = 0; a circle of radius 2
        // centred a little above it, at the fixed origin, wants that side
        // two units away.
        definition
            .add_constraint(
                SketchConstraintKind::Fixed {
                    point: ids[0],
                    position: SketchPoint2::new(0.0, 0.0),
                },
                PrecisionPolicy::default(),
            )
            .expect("fix the corner");
        let others = ids
            .iter()
            .copied()
            .filter(|id| *id != ids[0])
            .collect::<Vec<_>>();
        definition
            .add_constraint(
                SketchConstraintKind::LineTangentToCircle {
                    start: others[0],
                    end: others[1],
                    center: ids[0],
                    radius: 2.0,
                },
                PrecisionPolicy::default(),
            )
            .expect("tangent line");
        let solution = definition
            .solve_constraints(PrecisionPolicy::default())
            .expect("solve");
        let a = solution.positions[&others[0]];
        let b = solution.positions[&others[1]];
        let c = solution.positions[&ids[0]];
        let (offset, _) = line_offset(a, b, c).expect("the side keeps its length");
        assert!((offset.abs() - 2.0).abs() < 1.0e-9, "offset {offset}");
        assert!(solution.maximum_residual <= 1.0e-9);
    }

    #[test]
    fn collinear_moves_a_point_onto_the_infinite_line_not_the_span() {
        let mut positions = BTreeMap::new();
        let ids = (1..=3)
            .map(|index| SketchPointId::new(index).expect("id"))
            .collect::<Vec<_>>();
        positions.insert(ids[0], SketchPoint2::new(0.0, 0.0));
        positions.insert(ids[1], SketchPoint2::new(1.0, 0.0));
        positions.insert(ids[2], SketchPoint2::new(5.0, 1.0));
        let record = SketchConstraintRecord {
            id: SketchConstraintId::new(1).expect("id"),
            kind: SketchConstraintKind::Collinear {
                first: ids[0],
                second: ids[2],
                third: ids[1],
            },
            enabled: true,
        };
        let solved =
            solve(&positions, [record].into_iter(), 1.0e-9, &BTreeSet::new()).expect("solve");
        let moved = solved.positions[&ids[2]];
        assert!((moved.v).abs() < 1.0e-9);
        assert!(
            (moved.u - 5.0).abs() < 1.0e-9,
            "the point stays beyond the span: {moved:?}"
        );
    }

    #[test]
    fn conflicting_fixed_constraints_are_rejected_without_publishing() {
        let mut definition = rectangle();
        let point = definition.active_points().next().expect("point").id;
        definition
            .add_constraint(
                SketchConstraintKind::Fixed {
                    point,
                    position: SketchPoint2::new(0.0, 0.0),
                },
                PrecisionPolicy::default(),
            )
            .expect("first fixed constraint");
        let before = definition.constraints().len();
        let result = definition.add_constraint(
            SketchConstraintKind::Fixed {
                point,
                position: SketchPoint2::new(1.0, 0.0),
            },
            PrecisionPolicy::default(),
        );
        assert!(matches!(result, Err(ConstraintError::Conflicting { .. })));
        assert_eq!(definition.constraints().len(), before);
    }

    #[test]
    fn constraint_ids_and_graph_survive_json_round_trip() {
        let mut definition = rectangle();
        let points = definition
            .active_points()
            .map(|point| point.id)
            .collect::<Vec<_>>();
        let id = definition
            .add_constraint(
                SketchConstraintKind::Horizontal {
                    first: points[0],
                    second: points[1],
                },
                PrecisionPolicy::default(),
            )
            .expect("horizontal");
        let json = serde_json::to_string(&definition).expect("serialize");
        let decoded: SketchDefinition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            decoded.constraints().get(&id),
            definition.constraints().get(&id)
        );
        assert!(decoded.high_water_marks().constraint() >= id.get());
    }
}

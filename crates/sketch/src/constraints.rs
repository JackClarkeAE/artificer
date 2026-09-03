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
    /// The perpendicular distance from `point` to the infinite line through
    /// `line_start` and `line_end`, signed along that line's left normal.
    ///
    /// This is the dimension from an edge to a centre: the one a drawer
    /// reaches for to place a hole in a plate.
    PointToLineDistance {
        point: SketchPointId,
        line_start: SketchPointId,
        line_end: SketchPointId,
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
            Self::PointToLineDistance {
                point,
                line_start,
                line_end,
                ..
            } => vec![point, line_start, line_end],
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

    /// The value this relation holds, when it holds one.
    ///
    /// A relation that holds a measurement is a dimension, and a dimension is
    /// something a drawer retypes. The ones that hold no number — coincident,
    /// parallel, and the rest — answer `None`.
    #[must_use]
    pub const fn value(&self) -> Option<f64> {
        match self {
            Self::Distance { distance, .. } | Self::PointToLineDistance { distance, .. } => {
                Some(*distance)
            }
            _ => None,
        }
    }

    /// The same relation holding a different value, or `None` when it holds no
    /// value to change. The operands are untouched: retyping a dimension must
    /// not silently re-aim it.
    #[must_use]
    pub fn with_value(&self, value: f64) -> Option<Self> {
        let mut updated = self.clone();
        match &mut updated {
            Self::Distance { distance, .. } | Self::PointToLineDistance { distance, .. } => {
                *distance = value;
            }
            _ => return None,
        }
        Some(updated)
    }

    #[must_use]
    pub const fn equation_count(&self) -> usize {
        match self {
            Self::Fixed { .. } | Self::Coincident { .. } => 2,
            Self::Horizontal { .. }
            | Self::Vertical { .. }
            | Self::Distance { .. }
            | Self::PointToLineDistance { .. }
            | Self::Parallel { .. }
            | Self::Perpendicular { .. }
            | Self::EqualLength { .. }
            | Self::Tangent { .. }
            | Self::Collinear { .. }
            | Self::LineTangentToCircle { .. }
            | Self::LineTangentToArc { .. } => 1,
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
    /// Every point the relation names belongs to one shape-owning feature.
    /// The solver moves such a feature as a body, so the relation could only
    /// be satisfied by deforming it.
    WithinOneShape,
    MissingConstraint(SketchConstraintId),
    /// The relation holds no value, so there is nothing to retype.
    NotADimension,
    Conflicting {
        maximum_residual: f64,
    },
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
            Self::WithinOneShape => formatter.write_str(
                "that relation names points of one feature, which the solver moves as a body; set its size with its own dimensions instead",
            ),
            Self::MissingConstraint(id) => write!(formatter, "relation {id} does not exist"),
            Self::NotADimension => {
                formatter.write_str("that relation holds no value to change")
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
    // A dimension's magnitude is what it holds; the sign, where it holds it
    // from. Zero is the one value no dimension may take: it names no side and
    // is the relation that puts two things in the same place.
    if let Some(value) = kind.value() {
        if !value.is_finite() {
            return Err(ConstraintError::NonFiniteValue);
        }
        // A point-to-line dimension is signed: the sign is the side it was
        // taken from. Every other value is a magnitude.
        let signed = matches!(kind, SketchConstraintKind::PointToLineDistance { .. });
        if signed && value == 0.0 || !signed && value <= 0.0 {
            return Err(ConstraintError::NonPositiveDistance);
        }
    }
    match kind {
        SketchConstraintKind::Fixed { position, .. } if !position.is_finite() => {
            Err(ConstraintError::NonFiniteValue)
        }
        SketchConstraintKind::LineTangentToCircle { radius, .. } if !radius.is_finite() => {
            Err(ConstraintError::NonFiniteValue)
        }
        SketchConstraintKind::LineTangentToCircle { radius, .. } if *radius <= 0.0 => {
            Err(ConstraintError::NonPositiveDistance)
        }
        _ => Ok(()),
    }
}

/// Which points move together.
///
/// A shape-owning recipe contributes one group: the solver may translate it,
/// never shear it. Every other point is a group of one, which is the behaviour
/// the solver has always had for lines and their endpoints.
#[derive(Clone, Debug, Default)]
pub struct RigidPointGroups {
    group_of: BTreeMap<SketchPointId, usize>,
    members: Vec<Vec<SketchPointId>>,
}

impl RigidPointGroups {
    /// Puts `points` in one group. Points named twice keep their first group,
    /// so a caller cannot accidentally split a body in half.
    pub fn insert_group(&mut self, points: impl IntoIterator<Item = SketchPointId>) {
        let index = self.members.len();
        let mut members = Vec::new();
        for point in points {
            if self.group_of.contains_key(&point) {
                continue;
            }
            self.group_of.insert(point, index);
            members.push(point);
        }
        if members.is_empty() {
            return;
        }
        self.members.push(members);
    }

    /// Calls `visit` for every point that moves with `point`, itself included.
    ///
    /// A point in no group is a body of one, so this always visits at least
    /// the point it was given.
    fn for_each_member(&self, point: SketchPointId, mut visit: impl FnMut(SketchPointId)) {
        match self
            .group_of
            .get(&point)
            .and_then(|index| self.members.get(*index))
        {
            Some(members) => members.iter().copied().for_each(visit),
            None => visit(point),
        }
    }

    /// Whether two points are held by the same body, which is the case a
    /// relation between them cannot resolve by translation.
    #[must_use]
    pub fn share_a_body(&self, first: SketchPointId, second: SketchPointId) -> bool {
        match (self.group_of.get(&first), self.group_of.get(&second)) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }

    /// How many independent bodies the given points amount to, which is what
    /// the degrees-of-freedom count is over.
    fn body_count(&self, points: impl Iterator<Item = SketchPointId>) -> usize {
        let mut bodies = BTreeSet::new();
        let mut loose = 0;
        for point in points {
            match self.group_of.get(&point) {
                Some(index) => {
                    bodies.insert(*index);
                }
                None => loose += 1,
            }
        }
        bodies.len() + loose
    }
}

/// The solver's view of the sketch while it iterates: where every point is,
/// which are pinned, and which move together.
///
/// Every write goes through here, because a write to one point of a body is a
/// translation of the whole body. The projections below therefore say what
/// they mean — "put this point there" — and rigidity is honoured underneath
/// them rather than remembered at each call site.
struct Motion<'a> {
    positions: &'a mut BTreeMap<SketchPointId, SketchPoint2>,
    pinned: &'a BTreeMap<SketchPointId, SketchPoint2>,
    groups: &'a RigidPointGroups,
}

impl Motion<'_> {
    fn at(&self, id: SketchPointId) -> SketchPoint2 {
        point(self.positions, id)
    }

    /// A point is movable when nothing in its body is pinned.
    fn movable(&self, id: SketchPointId) -> bool {
        let mut movable = true;
        self.groups.for_each_member(id, |member| {
            movable &= !self.pinned.contains_key(&member);
        });
        movable
    }

    /// Translates the body holding `id` so that `id` lands on `target`.
    fn set(&mut self, id: SketchPointId, target: SketchPoint2) {
        let current = self.at(id);
        self.shift(id, (target.u - current.u, target.v - current.v));
    }

    /// Translates the body holding `id` by `delta`.
    fn shift(&mut self, id: SketchPointId, delta: (f64, f64)) {
        if delta.0 == 0.0 && delta.1 == 0.0 {
            return;
        }
        let mut moving = Vec::new();
        self.groups
            .for_each_member(id, |member| moving.push(member));
        for member in moving {
            if let Some(position) = self.positions.get_mut(&member) {
                *position = SketchPoint2::new(position.u + delta.0, position.v + delta.1);
            }
        }
    }
}

pub(crate) fn solve(
    seeds: &BTreeMap<SketchPointId, SketchPoint2>,
    constraints: impl Iterator<Item = SketchConstraintRecord>,
    groups: &RigidPointGroups,
    tolerance: f64,
) -> Result<ConstraintSolution, ConstraintError> {
    let constraints = constraints
        .filter(|record| record.enabled)
        .collect::<Vec<_>>();
    let mut positions = seeds.clone();
    // Pinning one point of a body pins the body: a rectangle with a fixed
    // corner cannot translate, because translating it would move that corner.
    let mut pinned = BTreeMap::new();
    for record in &constraints {
        let SketchConstraintKind::Fixed { point, position } = record.kind else {
            continue;
        };
        pinned.insert(point, position);
        let mut held = Vec::new();
        groups.for_each_member(point, |member| held.push(member));
        for member in held {
            if member == point {
                continue;
            }
            if let Some(place) = positions.get(&member) {
                pinned.insert(member, *place);
            }
        }
    }
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
            let mut motion = Motion {
                positions: &mut positions,
                pinned: &pinned,
                groups,
            };
            project(&mut motion, &record.kind, threshold);
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
            // Freedom is counted over bodies, not points: a rectangle the
            // solver may only translate has two degrees of freedom however
            // many corners it happens to own.
            let remaining = groups
                .body_count(positions.keys().copied())
                .saturating_mul(2)
                .saturating_sub(equations);
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

fn set_pair_coordinate(
    motion: &mut Motion<'_>,
    first: SketchPointId,
    second: SketchPointId,
    horizontal: bool,
) {
    let a = motion.at(first);
    let b = motion.at(second);
    let (first_value, second_value) = if horizontal { (a.v, b.v) } else { (a.u, b.u) };
    let target = (first_value + second_value) * 0.5;
    let first_movable = motion.movable(first);
    let second_movable = motion.movable(second);
    let delta = |from: f64, to: f64| {
        if horizontal {
            (0.0, to - from)
        } else {
            (to - from, 0.0)
        }
    };
    if first_movable {
        let value = if second_movable { target } else { second_value };
        motion.shift(first, delta(first_value, value));
    }
    if second_movable {
        let value = if first_movable { target } else { first_value };
        motion.shift(second, delta(second_value, value));
    }
}

fn set_segment(
    motion: &mut Motion<'_>,
    start: SketchPointId,
    end: SketchPointId,
    direction: (f64, f64),
    length: f64,
) {
    let a = motion.at(start);
    let b = motion.at(end);
    let start_movable = motion.movable(start);
    let end_movable = motion.movable(end);
    if start_movable && end_movable {
        let mid = SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5);
        let half = length * 0.5;
        motion.set(
            start,
            SketchPoint2::new(mid.u - direction.0 * half, mid.v - direction.1 * half),
        );
        motion.set(
            end,
            SketchPoint2::new(mid.u + direction.0 * half, mid.v + direction.1 * half),
        );
    } else if start_movable {
        motion.set(
            start,
            SketchPoint2::new(b.u - direction.0 * length, b.v - direction.1 * length),
        );
    } else if end_movable {
        motion.set(
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

fn project(motion: &mut Motion<'_>, kind: &SketchConstraintKind, tolerance: f64) {
    match *kind {
        SketchConstraintKind::Fixed { point, position } => {
            motion.set(point, position);
        }
        SketchConstraintKind::Coincident { first, second } => {
            let a = motion.at(first);
            let b = motion.at(second);
            let first_movable = motion.movable(first);
            let second_movable = motion.movable(second);
            if first_movable && second_movable {
                let mid = SketchPoint2::new((a.u + b.u) * 0.5, (a.v + b.v) * 0.5);
                motion.set(first, mid);
                motion.set(second, mid);
            } else if first_movable {
                motion.set(first, b);
            } else if second_movable {
                motion.set(second, a);
            }
        }
        SketchConstraintKind::Horizontal { first, second } => {
            set_pair_coordinate(motion, first, second, true)
        }
        SketchConstraintKind::Vertical { first, second } => {
            set_pair_coordinate(motion, first, second, false)
        }
        SketchConstraintKind::Distance {
            first,
            second,
            distance,
        } => {
            let a = motion.at(first);
            let b = motion.at(second);
            let direction = normalized((b.u - a.u, b.v - a.v), (1.0, 0.0));
            set_segment(motion, first, second, direction, distance.max(tolerance));
        }
        SketchConstraintKind::PointToLineDistance {
            point,
            line_start,
            line_end,
            distance,
        } => project_point_to_line(motion, point, line_start, line_end, distance),
        SketchConstraintKind::Parallel {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = motion.at(first_start);
            let b = motion.at(first_end);
            let c = motion.at(second_start);
            let d = motion.at(second_end);
            let direction = normalized((b.u - a.u, b.v - a.v), (1.0, 0.0));
            set_segment(motion, second_start, second_end, direction, c.distance(d));
        }
        SketchConstraintKind::Perpendicular {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = motion.at(first_start);
            let b = motion.at(first_end);
            let c = motion.at(second_start);
            let d = motion.at(second_end);
            let direction = normalized((-(b.v - a.v), b.u - a.u), (0.0, 1.0));
            set_segment(motion, second_start, second_end, direction, c.distance(d));
        }
        SketchConstraintKind::EqualLength {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = motion.at(first_start);
            let b = motion.at(first_end);
            let c = motion.at(second_start);
            let d = motion.at(second_end);
            let direction = normalized((d.u - c.u, d.v - c.v), (1.0, 0.0));
            set_segment(motion, second_start, second_end, direction, a.distance(b));
        }
        SketchConstraintKind::Tangent {
            first_start,
            first_end,
            second_start,
            second_end,
        } => {
            let a = motion.at(first_start);
            let b = motion.at(first_end);
            let c = motion.at(second_start);
            let d = motion.at(second_end);
            let direction = normalized((b.u - a.u, b.v - a.v), (1.0, 0.0));
            set_segment(motion, second_start, second_end, direction, c.distance(d));
        }
        SketchConstraintKind::Collinear {
            first,
            second,
            third,
        } => {
            let a = motion.at(first);
            let c = motion.at(third);
            let b = motion.at(second);
            let ac = c - a;
            let len_sq = ac.length_squared();
            // The point goes to the infinite line, not the span between the
            // other two: collinear lines lie end to end, not on top of each
            // other, and clamping used to drag the second line onto the
            // first.
            if len_sq > 1.0e-14 && motion.movable(second) {
                let t = (b - a).dot(ac) / len_sq;
                motion.set(second, a + ac * t);
            }
        }
        SketchConstraintKind::LineTangentToCircle {
            start,
            end,
            center,
            radius,
        } => project_line_tangent(motion, start, end, center, radius),
        SketchConstraintKind::LineTangentToArc {
            start,
            end,
            center,
            rim,
        } => {
            let radius = motion.at(center).distance(motion.at(rim));
            project_line_tangent(motion, start, end, center, radius);
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

/// Slides the line, or failing that the centre, along the line's normal
/// until the centre sits one radius away from it. The circle stays on the
/// side it is already on, so a line tangent to a circle never flips through
/// it to the other side.
fn project_line_tangent(
    motion: &mut Motion<'_>,
    start: SketchPointId,
    end: SketchPointId,
    center: SketchPointId,
    radius: f64,
) {
    let a = motion.at(start);
    let b = motion.at(end);
    let c = motion.at(center);
    let Some((offset, normal)) = line_offset(a, b, c) else {
        return;
    };
    let target = if offset < 0.0 { -radius } else { radius };
    project_across_line(
        motion,
        start,
        end,
        center,
        normal,
        target - offset,
        Share::Between,
    );
}

/// Holds a point at a signed distance from the line through `line_start` and
/// `line_end`, measured along that line's left normal.
///
/// The sign is the constraint's, not the current pose's: a dimension taken
/// from one side of an edge stays on that side however the value is retyped.
fn project_point_to_line(
    motion: &mut Motion<'_>,
    subject: SketchPointId,
    line_start: SketchPointId,
    line_end: SketchPointId,
    distance: f64,
) {
    let a = motion.at(line_start);
    let b = motion.at(line_end);
    let c = motion.at(subject);
    let Some((offset, normal)) = line_offset(a, b, c) else {
        return;
    };
    project_across_line(
        motion,
        line_start,
        line_end,
        subject,
        normal,
        distance - offset,
        Share::SubjectFirst,
    );
}

/// Who gives way when both the line and the point could move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Share {
    /// Half each, which is how a tangency settles: neither the line nor the
    /// circle is the reference.
    Between,
    /// All of it to the point. A dimension is taken *from* an edge, so the
    /// edge is the reference and holds still.
    SubjectFirst,
}

/// Shares `delta` of separation along `normal` between a line and a point,
/// giving it all to whichever end can move.
#[allow(clippy::too_many_arguments)]
fn project_across_line(
    motion: &mut Motion<'_>,
    start: SketchPointId,
    end: SketchPointId,
    subject: SketchPointId,
    normal: (f64, f64),
    delta: f64,
    share: Share,
) {
    let (a, b, c) = (motion.at(start), motion.at(end), motion.at(subject));
    let line_movable = motion.movable(start) || motion.movable(end);
    let subject_movable = motion.movable(subject);
    let (line_share, subject_share) = match (line_movable, subject_movable, share) {
        (true, true, Share::Between) => (0.5, 0.5),
        (_, true, Share::SubjectFirst) => (0.0, 1.0),
        (true, false, _) => (1.0, 0.0),
        (false, true, _) => (0.0, 1.0),
        (false, false, _) => return,
    };
    // Moving the line away from the point by `delta` lowers the offset.
    let shift = |p: SketchPoint2, amount: f64| {
        SketchPoint2::new(p.u + normal.0 * amount, p.v + normal.1 * amount)
    };
    if motion.movable(start) {
        motion.set(start, shift(a, -delta * line_share));
    }
    if motion.movable(end) {
        motion.set(end, shift(b, -delta * line_share));
    }
    if subject_movable {
        motion.set(subject, shift(c, delta * subject_share));
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
            point: subject,
            line_start,
            line_end,
            distance,
        } => {
            let a = point(positions, line_start);
            let b = point(positions, line_end);
            let c = point(positions, subject);
            // A line with no length names no direction to measure along, so
            // the relation is as unsatisfied as it can be rather than
            // accidentally satisfied.
            line_offset(a, b, c)
                .map_or_else(|| distance.abs(), |(offset, _)| (offset - distance).abs())
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

    /// Two lines: four points a relation may move independently, which is what
    /// the solver's own behaviour is stated over. A rectangle's corners are not
    /// that — the solver moves them as one body — so a solver test that wants
    /// loose points has to draw loose points.
    fn two_lines() -> SketchDefinition {
        let mut definition = SketchDefinition::new();
        for (start, end) in [((0.0, 0.0), (8.0, 0.0)), ((0.0, 3.0), (8.0, 3.0))] {
            let transaction = definition
                .stage_with_inputs(
                    SketchRecipe::Line {
                        start: PointInput::Position(SketchPoint2::new(start.0, start.1)),
                        end: PointInput::Position(SketchPoint2::new(end.0, end.1)),
                    },
                    "line",
                    &SketchInputValues::default(),
                    PrecisionPolicy::default(),
                )
                .expect("stage line");
            definition
                .commit(transaction, ConfirmationSource::BareEnter)
                .expect("commit line");
        }
        definition
    }

    /// A relation over the points of one shape is refused rather than shearing
    /// it. Before the solver moved shapes as bodies, this pulled two corners of
    /// a rectangle together and left the other two where they were, which made
    /// a bowtie out of a rectangle without a word of complaint.
    #[test]
    fn a_relation_within_one_shape_is_refused_by_name() {
        let mut definition = rectangle();
        let ids = definition
            .active_points()
            .map(|point| point.id)
            .collect::<Vec<_>>();
        assert_eq!(
            definition.add_constraint(
                SketchConstraintKind::Distance {
                    first: ids[0],
                    second: ids[1],
                    distance: 4.0,
                },
                PrecisionPolicy::default(),
            ),
            Err(ConstraintError::WithinOneShape)
        );
        // Pinning one point of a shape is not a relation over the shape: it
        // says where the shape is, and remains allowed.
        definition
            .add_constraint(
                SketchConstraintKind::Fixed {
                    point: ids[0],
                    position: SketchPoint2::new(1.0, 1.0),
                },
                PrecisionPolicy::default(),
            )
            .expect("a shape can still be pinned");
    }

    #[test]
    fn fixed_distance_and_horizontal_constraints_drive_evaluated_geometry() {
        let mut definition = two_lines();
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
        let mut definition = two_lines();
        let ids = definition
            .active_points()
            .map(|point| point.id)
            .collect::<Vec<_>>();
        // The first line runs along v = 0; a circle of radius 2 centred at
        // the fixed origin wants the second line two units away from it.
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
        let solved = solve(
            &positions,
            [record].into_iter(),
            &RigidPointGroups::default(),
            1.0e-9,
        )
        .expect("solve");
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
        let mut definition = two_lines();
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

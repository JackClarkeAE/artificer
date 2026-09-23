//! Open, tangent-continuous chains of sketch curves: the path a sweep
//! follows (ADR 0055).
//!
//! A chain is read out of a sketch by entity id. The curves are put end to
//! end whatever order they were named in, each turned to run the way the
//! chain does, and the chain is refused where it could not be a path: a gap
//! between two curves, three curves meeting at one point, two meeting at a
//! corner rather than tangentially, a chain that closes on itself, or a full
//! circle, which has no ends to join.

use std::collections::BTreeSet;

use crate::{
    CurveDirection, EvaluatedCurve2, SketchDefinition, SketchEntityId, SketchPoint2,
    SketchValidationError,
};

/// How close two curve ends must be to meet, as a fraction of the chain's
/// size.
const MEET_AGREEMENT: f64 = 1.0e-9;

/// How closely two tangents must agree, in radians, for a junction to be
/// smooth rather than a corner.
const TANGENT_AGREEMENT: f64 = 1.0e-6;

/// One curve of a chain, turned to run the way the chain does.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainCurve {
    pub entity: SketchEntityId,
    pub curve: EvaluatedCurve2,
    /// Whether the chain runs against the curve's own direction, from its
    /// end to its start.
    pub reversed: bool,
}

impl ChainCurve {
    /// Where the chain enters this curve.
    #[must_use]
    pub fn start(&self) -> SketchPoint2 {
        let (start, end) = ends(&self.curve).expect("a chain holds curves with ends");
        if self.reversed { end } else { start }
    }

    /// Where the chain leaves this curve.
    #[must_use]
    pub fn end(&self) -> SketchPoint2 {
        let (start, end) = ends(&self.curve).expect("a chain holds curves with ends");
        if self.reversed { start } else { end }
    }
}

/// Why a set of curves is not one open, smooth chain.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    #[error("a path needs at least one curve")]
    Empty,
    #[error("a path names curve {entity} twice")]
    Repeated { entity: SketchEntityId },
    #[error("curve {entity} is not an active curve of the sketch")]
    MissingCurve { entity: SketchEntityId },
    #[error("the sketch's constraints do not solve, so its curves have no positions")]
    Unsolved,
    #[error("curve {entity} is a full circle, which has no ends to join a path by")]
    ClosedCurve { entity: SketchEntityId },
    #[error(
        "curve {entity} is not a clamped spline, so its ends are not its first and last points"
    )]
    UnclampedSpline { entity: SketchEntityId },
    #[error("the path's curves do not all join end to end; curve {entity} is apart from the rest")]
    Gap { entity: SketchEntityId },
    #[error("three or more of the path's curves meet at one point, at an end of curve {entity}")]
    Branch { entity: SketchEntityId },
    #[error(
        "the path turns a corner where curves {first} and {second} meet; a sweep needs them tangent"
    )]
    Corner {
        first: SketchEntityId,
        second: SketchEntityId,
    },
    #[error("the path closes on itself; a sweep's path must be open")]
    Closed,
}

impl SketchDefinition {
    /// Puts `entities` end to end as one open, tangent-continuous chain,
    /// each curve turned to run the way the chain does. The chain runs from
    /// the free end of the curve named first.
    pub fn ordered_chain(
        &self,
        entities: &[SketchEntityId],
    ) -> Result<Vec<ChainCurve>, ChainError> {
        if entities.is_empty() {
            return Err(ChainError::Empty);
        }
        let mut seen = BTreeSet::new();
        for entity in entities {
            if !seen.insert(*entity) {
                return Err(ChainError::Repeated { entity: *entity });
            }
        }
        let curves = self
            .evaluated_curves(entities)
            .map_err(|error| match error {
                SketchValidationError::MissingEntity { entity } => {
                    ChainError::MissingCurve { entity }
                }
                _ => ChainError::Unsolved,
            })?;
        let pieces = entities
            .iter()
            .copied()
            .zip(curves)
            .map(|(entity, curve)| Piece::new(entity, curve))
            .collect::<Result<Vec<_>, _>>()?;
        let agreement = MEET_AGREEMENT * chain_size(&pieces);

        // Every end meets at most one other; an end that meets none is one
        // of the chain's two free ends.
        let meets = |a: SketchPoint2, b: SketchPoint2| (a.u - b.u).hypot(a.v - b.v) <= agreement;
        let partners = |index: usize, point: SketchPoint2| {
            pieces
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .filter(|(_, piece)| meets(piece.start, point) || meets(piece.end, point))
                .count()
        };
        let mut free_ends = Vec::new();
        for (index, piece) in pieces.iter().enumerate() {
            for (at_start, point) in [(true, piece.start), (false, piece.end)] {
                match partners(index, point) {
                    0 => free_ends.push((index, at_start)),
                    1 => {}
                    _ => {
                        return Err(ChainError::Branch {
                            entity: piece.entity,
                        });
                    }
                }
            }
        }
        if free_ends.is_empty() {
            return Err(ChainError::Closed);
        }
        // Start from the free end of the first curve named, or the first free
        // end at all.
        let (mut index, at_start) = free_ends
            .iter()
            .copied()
            .find(|(index, _)| *index == 0)
            .unwrap_or(free_ends[0]);
        let mut reversed = !at_start;
        let mut used = vec![false; pieces.len()];
        let mut chain = Vec::with_capacity(pieces.len());
        loop {
            used[index] = true;
            let piece = &pieces[index];
            chain.push(ChainCurve {
                entity: piece.entity,
                curve: piece.curve.clone(),
                reversed,
            });
            let (exit, exit_tangent) = if reversed {
                (piece.start, negated(piece.start_tangent))
            } else {
                (piece.end, piece.end_tangent)
            };
            let Some((next, next_reversed)) =
                pieces.iter().enumerate().find_map(|(other, candidate)| {
                    if used[other] {
                        return None;
                    }
                    if meets(candidate.start, exit) {
                        Some((other, false))
                    } else if meets(candidate.end, exit) {
                        Some((other, true))
                    } else {
                        None
                    }
                })
            else {
                break;
            };
            let candidate = &pieces[next];
            let entry_tangent = if next_reversed {
                negated(candidate.end_tangent)
            } else {
                candidate.start_tangent
            };
            if angle_between(exit_tangent, entry_tangent) > TANGENT_AGREEMENT {
                return Err(ChainError::Corner {
                    first: piece.entity,
                    second: candidate.entity,
                });
            }
            index = next;
            reversed = next_reversed;
        }
        if let Some(apart) = pieces
            .iter()
            .zip(&used)
            .find_map(|(piece, used)| (!used).then_some(piece.entity))
        {
            return Err(ChainError::Gap { entity: apart });
        }
        Ok(chain)
    }

    /// The tangent-continuous chain through `entity`: it and every curve
    /// that continues it smoothly at either end, stopping at a free end, a
    /// corner or a branch. Only curves that can be part of a path — lines,
    /// arcs and clamped splines of the sketch's profile or construction
    /// geometry — are followed. The result is in chain order.
    pub fn tangent_chain_through(&self, entity: SketchEntityId) -> Vec<SketchEntityId> {
        let candidates = self
            .active_entities()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        let Ok(curves) = self.evaluated_curves(&candidates) else {
            return Vec::new();
        };
        let pieces = candidates
            .into_iter()
            .zip(curves)
            .filter_map(|(id, curve)| Piece::new(id, curve).ok())
            .collect::<Vec<_>>();
        let Some(seed) = pieces.iter().position(|piece| piece.entity == entity) else {
            return Vec::new();
        };
        let agreement = MEET_AGREEMENT * chain_size(&pieces);
        let meets = |a: SketchPoint2, b: SketchPoint2| (a.u - b.u).hypot(a.v - b.v) <= agreement;

        // Walks one way from the seed, returning the curves it passes.
        let walk = |forward: bool, taken: &mut Vec<bool>| {
            let mut found = Vec::new();
            let mut index = seed;
            let mut reversed = !forward;
            loop {
                let piece = &pieces[index];
                let (exit, exit_tangent) = if reversed {
                    (piece.start, negated(piece.start_tangent))
                } else {
                    (piece.end, piece.end_tangent)
                };
                let touching = pieces
                    .iter()
                    .enumerate()
                    .filter(|(other, candidate)| {
                        *other != index
                            && (meets(candidate.start, exit) || meets(candidate.end, exit))
                    })
                    .collect::<Vec<_>>();
                // A branch or a free end stops the walk.
                let [(next, candidate)] = touching.as_slice() else {
                    break;
                };
                if taken[*next] {
                    break;
                }
                let next_reversed = !meets(candidate.start, exit);
                let entry_tangent = if next_reversed {
                    negated(candidate.end_tangent)
                } else {
                    candidate.start_tangent
                };
                if angle_between(exit_tangent, entry_tangent) > TANGENT_AGREEMENT {
                    break;
                }
                taken[*next] = true;
                found.push(candidate.entity);
                index = *next;
                reversed = next_reversed;
            }
            found
        };
        let mut taken = vec![false; pieces.len()];
        taken[seed] = true;
        let ahead = walk(true, &mut taken);
        let behind = walk(false, &mut taken);
        behind
            .into_iter()
            .rev()
            .chain(std::iter::once(entity))
            .chain(ahead)
            .collect()
    }
}

/// A curve with its ends and the tangent directions there, both taken the
/// way the curve runs.
struct Piece {
    entity: SketchEntityId,
    curve: EvaluatedCurve2,
    start: SketchPoint2,
    end: SketchPoint2,
    start_tangent: (f64, f64),
    end_tangent: (f64, f64),
}

impl Piece {
    fn new(entity: SketchEntityId, curve: EvaluatedCurve2) -> Result<Self, ChainError> {
        let (start, end) = ends(&curve).ok_or(match curve {
            EvaluatedCurve2::Circle { .. } => ChainError::ClosedCurve { entity },
            _ => ChainError::UnclampedSpline { entity },
        })?;
        let (start_tangent, end_tangent) = match &curve {
            EvaluatedCurve2::Line { start, end } => {
                let direction = (end.u - start.u, end.v - start.v);
                (direction, direction)
            }
            EvaluatedCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                // Round the centre the way the arc turns.
                let turn = |point: &SketchPoint2| {
                    let (x, y) = (point.u - center.u, point.v - center.v);
                    match direction {
                        CurveDirection::CounterClockwise => (-y, x),
                        CurveDirection::Clockwise => (y, -x),
                    }
                };
                (turn(start), turn(end))
            }
            EvaluatedCurve2::Bspline { control_points, .. } => {
                let count = control_points.len();
                let along = |from: &SketchPoint2, to: &SketchPoint2| (to.u - from.u, to.v - from.v);
                (
                    along(&control_points[0], &control_points[1]),
                    along(&control_points[count - 2], &control_points[count - 1]),
                )
            }
            EvaluatedCurve2::Circle { .. } => unreachable!("a circle has no ends"),
        };
        Ok(Self {
            entity,
            curve,
            start,
            end,
            start_tangent,
            end_tangent,
        })
    }
}

/// A curve's first and last points, or none for a curve without ends: a
/// full circle, or a spline that is not clamped to its end points.
fn ends(curve: &EvaluatedCurve2) -> Option<(SketchPoint2, SketchPoint2)> {
    match curve {
        EvaluatedCurve2::Line { start, end } | EvaluatedCurve2::CircularArc { start, end, .. } => {
            Some((*start, *end))
        }
        EvaluatedCurve2::Circle { .. } => None,
        EvaluatedCurve2::Bspline {
            control_points,
            degree,
            knots,
            weights,
        } => {
            let count = control_points.len();
            let clamped = count >= 2
                && knots.len() == count + degree + 1
                && knots[..=*degree].iter().all(|knot| *knot == knots[0])
                && knots[count..].iter().all(|knot| *knot == knots[count])
                && weights
                    .as_ref()
                    .is_none_or(|weights| weights.iter().all(|weight| *weight > 0.0));
            clamped.then(|| (control_points[0], control_points[count - 1]))
        }
    }
}

/// The largest coordinate any curve end reaches, and at least one: the
/// scale end-to-end agreement is judged against.
fn chain_size(pieces: &[Piece]) -> f64 {
    pieces
        .iter()
        .flat_map(|piece| [piece.start, piece.end])
        .map(|point| point.u.abs().max(point.v.abs()))
        .fold(1.0, f64::max)
}

const fn negated(vector: (f64, f64)) -> (f64, f64) {
    (-vector.0, -vector.1)
}

fn angle_between(first: (f64, f64), second: (f64, f64)) -> f64 {
    let cross = first.0 * second.1 - first.1 * second.0;
    let dot = first.0 * second.0 + first.1 * second.1;
    cross.atan2(dot).abs()
}

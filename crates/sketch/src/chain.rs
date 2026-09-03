//! Walking a connected chain of sketch curves from one of its members.
//!
//! "All the connected lines within a loop" is the selection Offset acts on and
//! the one a user reaches for when they mean *this outline*, not *this
//! segment*. The walk is over whole entities joined at shared endpoints — not
//! over the arrangement's fragments, which are split at every crossing and
//! carry no entity identity a recipe could store.
//!
//! Three properties are the whole contract, and each is a test:
//!
//! - the order is deterministic, so the curves a chain yields do not depend on
//!   pick order or map iteration;
//! - a junction where three or more ends meet stops the walk, because there is
//!   no single continuation and guessing one is worse than stopping; and
//! - a closed chain reports itself closed, whichever member it was seeded from.

use artificer_protocol::PrecisionPolicy;

use crate::definition::{SketchDefinition, SketchEntityRole, SketchValidationError};
use crate::geometry::{EvaluatedCurve2, SketchPoint2};
use crate::ids::SketchEntityId;
use crate::offset::OffsetChain;

/// One curve of a chain, and which way the walk crosses it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChainMember {
    pub entity: SketchEntityId,
    /// True when the walk crosses the curve against its own direction, so its
    /// evaluated geometry has to be reversed to read head to tail.
    pub reversed: bool,
}

/// A connected run of sketch curves, in traversal order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SketchChain {
    pub members: Vec<ChainMember>,
    pub closed: bool,
}

/// Why a chain could not be walked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainError {
    /// The seed is not an active curve of this sketch.
    MissingSeed { entity: SketchEntityId },
    /// The seed is a construction or reference curve. A chain is the outline
    /// something is made of, and those are not part of one.
    UnsupportedSeed { entity: SketchEntityId },
    /// A curve could not be evaluated, so its ends are unknown.
    Unevaluated { entity: SketchEntityId },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSeed { entity } => {
                write!(formatter, "curve {entity:?} is not in this sketch")
            }
            Self::UnsupportedSeed { entity } => write!(
                formatter,
                "curve {entity:?} is not profile geometry, so it is not part of an outline"
            ),
            Self::Unevaluated { entity } => {
                write!(formatter, "curve {entity:?} could not be evaluated")
            }
        }
    }
}

impl std::error::Error for ChainError {}

impl From<ChainError> for SketchValidationError {
    fn from(reason: ChainError) -> Self {
        Self::ChainRefused { reason }
    }
}

/// Whether a curve can take part in a chain at all.
///
/// Profile geometry only: a centreline or a projected reference is there to
/// measure and align against, and sweeping one up into an outline because it
/// happens to touch it is not what a click on that outline asked for.
fn eligible(role: SketchEntityRole) -> bool {
    matches!(role, SketchEntityRole::Profile)
}

/// Every curve reachable from `seed` through shared endpoints, in order.
///
/// A closed chain is rotated to start at its lowest entity id and walked in
/// that curve's own direction, so the same loop yields the same order whichever
/// member was clicked. An open chain is walked from one free end to the other,
/// and the end it starts from is the one whose first curve has the lower id.
pub fn connected_chain(
    definition: &SketchDefinition,
    seed: SketchEntityId,
    precision: &PrecisionPolicy,
) -> Result<SketchChain, ChainError> {
    let record = definition
        .entity(seed)
        .filter(|record| record.active)
        .ok_or(ChainError::MissingSeed { entity: seed })?;
    if !eligible(record.role) {
        return Err(ChainError::UnsupportedSeed { entity: seed });
    }

    let curves = eligible_curves(definition, seed)?;
    let Some(seed_curve) = curves.iter().find(|entry| entry.entity == seed) else {
        return Err(ChainError::MissingSeed { entity: seed });
    };
    // A closed curve has no ends to walk from and is a chain of itself.
    let Some((seed_start, seed_end)) = seed_curve.ends else {
        return Ok(SketchChain {
            members: vec![ChainMember {
                entity: seed,
                reversed: false,
            }],
            closed: true,
        });
    };

    let mut forward = vec![ChainMember {
        entity: seed,
        reversed: false,
    }];
    let mut visited = vec![seed];
    let mut frontier = seed_end;
    let mut closed = false;
    while let Some(next) = continuation(&curves, frontier, &visited, precision) {
        if next.entity == seed {
            break;
        }
        let (start, end) = next.ends.expect("a walked curve has ends");
        let reversed = !near(start, frontier, precision);
        frontier = if reversed { start } else { end };
        forward.push(ChainMember {
            entity: next.entity,
            reversed,
        });
        visited.push(next.entity);
        if near(frontier, seed_start, precision) {
            closed = true;
            break;
        }
    }

    if closed {
        return Ok(canonical_loop(forward));
    }

    // The chain is open, so it also runs backwards out of the seed.
    let mut backward = Vec::new();
    let mut frontier = seed_start;
    while let Some(next) = continuation(&curves, frontier, &visited, precision) {
        let (start, end) = next.ends.expect("a walked curve has ends");
        // Walking backwards, a curve is reversed when it *starts* at the
        // frontier: the chain reads into it from its far end.
        let reversed = near(start, frontier, precision);
        frontier = if reversed { end } else { start };
        backward.push(ChainMember {
            entity: next.entity,
            reversed,
        });
        visited.push(next.entity);
    }
    backward.reverse();
    backward.extend(forward);
    Ok(canonical_open(backward))
}

/// The chain as geometry, head to tail, ready for [`crate::offset_chain`].
pub fn chain_geometry(
    definition: &SketchDefinition,
    chain: &SketchChain,
) -> Result<OffsetChain, ChainError> {
    let evaluated = definition
        .evaluated_curves()
        .map_err(|_| ChainError::Unevaluated {
            entity: chain.members[0].entity,
        })?;
    let mut curves = Vec::with_capacity(chain.members.len());
    for member in &chain.members {
        let curve = evaluated
            .iter()
            .find(|(entity, _)| *entity == member.entity)
            .map(|(_, curve)| curve.clone())
            .ok_or(ChainError::Unevaluated {
                entity: member.entity,
            })?;
        curves.push(if member.reversed {
            curve.reverse()
        } else {
            curve
        });
    }
    Ok(OffsetChain::new(curves, chain.closed))
}

/// One candidate curve of the sketch, with its evaluated ends.
struct ChainCandidate {
    entity: SketchEntityId,
    /// `None` for a curve closed on itself, which has no ends to join at.
    ends: Option<(SketchPoint2, SketchPoint2)>,
}

fn eligible_curves(
    definition: &SketchDefinition,
    seed: SketchEntityId,
) -> Result<Vec<ChainCandidate>, ChainError> {
    // One constraint solve for the whole walk: `evaluated_curve` solves the
    // system per call, and a chain asks about every curve in the sketch.
    let evaluated = definition
        .evaluated_curves()
        .map_err(|_| ChainError::Unevaluated { entity: seed })?;
    Ok(evaluated
        .into_iter()
        .filter(|(entity, _)| {
            definition
                .entity(*entity)
                .is_some_and(|record| eligible(record.role))
        })
        .map(|(entity, curve)| ChainCandidate {
            entity,
            ends: curve_ends(&curve),
        })
        .collect())
}

fn curve_ends(curve: &EvaluatedCurve2) -> Option<(SketchPoint2, SketchPoint2)> {
    match curve {
        EvaluatedCurve2::Line { start, end } | EvaluatedCurve2::CircularArc { start, end, .. } => {
            Some((*start, *end))
        }
        EvaluatedCurve2::Bspline { control_points, .. } => {
            Some((*control_points.first()?, *control_points.last()?))
        }
        EvaluatedCurve2::Circle { .. } => None,
    }
}

fn near(first: SketchPoint2, second: SketchPoint2, precision: &PrecisionPolicy) -> bool {
    (first - second).length() <= precision.linear_agreement
}

/// The one unvisited curve continuing the chain at `frontier`, if there is
/// exactly one.
///
/// Exactly one: a point where three or more ends meet is a junction, and a
/// chain that guessed a branch there would silently offset geometry the user
/// did not point at.
fn continuation<'a>(
    curves: &'a [ChainCandidate],
    frontier: SketchPoint2,
    visited: &[SketchEntityId],
    precision: &PrecisionPolicy,
) -> Option<&'a ChainCandidate> {
    let mut incident = 0_usize;
    let mut candidate = None;
    for entry in curves {
        let Some((start, end)) = entry.ends else {
            continue;
        };
        let touches = usize::from(near(start, frontier, precision))
            + usize::from(near(end, frontier, precision));
        if touches == 0 {
            continue;
        }
        incident += touches;
        if !visited.contains(&entry.entity) {
            candidate = Some(entry);
        }
    }
    // Two ends meet at an ordinary joint: the one arriving and the one leaving.
    (incident == 2).then_some(candidate?)
}

/// Rotates a closed chain to begin at its lowest entity id, walked in that
/// curve's own direction, so the loop reads the same from any member.
fn canonical_loop(members: Vec<ChainMember>) -> SketchChain {
    let Some(anchor) = members
        .iter()
        .enumerate()
        .min_by_key(|(_, member)| member.entity)
        .map(|(index, _)| index)
    else {
        return SketchChain {
            members,
            closed: true,
        };
    };
    let mut rotated = members;
    rotated.rotate_left(anchor);
    if rotated[0].reversed {
        rotated.reverse();
        for member in &mut rotated {
            member.reversed = !member.reversed;
        }
        rotated.rotate_right(1);
    }
    SketchChain {
        members: rotated,
        closed: true,
    }
}

/// Orients an open chain so it starts at the end whose curve has the lower id.
fn canonical_open(members: Vec<ChainMember>) -> SketchChain {
    let mut members = members;
    let first = members.first().map(|member| member.entity);
    let last = members.last().map(|member| member.entity);
    if let (Some(first), Some(last)) = (first, last)
        && last < first
    {
        members.reverse();
        for member in &mut members {
            member.reversed = !member.reversed;
        }
    }
    SketchChain {
        members,
        closed: false,
    }
}

//! Exact regularized Booleans on planar line/arc regions.
//!
//! This is the 2D heart of the prism Boolean: ADR 0025's imprint, classify,
//! regularize, and sew stages, run in the plane where every intersection in
//! the vocabulary has a closed form. The pipeline is:
//!
//! 1. **Imprint** — intersect every boundary segment of one operand with
//!    every boundary segment of the other (line/line, line/arc, arc/arc, all
//!    algebraic), and split both segments at the shared intersection points.
//!    Both sides receive the *same* `Point2` bit for bit, which is what makes
//!    the later chaining exact rather than tolerance-driven.
//! 2. **Classify** — each resulting piece crosses no boundary of the other
//!    operand, so one interior sample decides which side of the other
//!    region's material it lies on. Two independent samples must agree, or
//!    the piece is rejected as numerically suspect.
//! 3. **Regularize** — keep pieces by the standard directed-boundary rules:
//!    union keeps boundary outside the other operand, intersection keeps
//!    boundary inside, difference keeps the minuend's boundary outside plus
//!    the subtrahend's boundary inside *reversed*. Material always stays on
//!    the left, so result loop orientation falls out by construction.
//! 4. **Sew** — chain the retained pieces into closed loops by exact
//!    endpoint identity, then nest loops into regions by even-odd depth.
//!
//! Everything outside the transverse-crossing domain fails closed:
//! coincident carriers, tangential contacts, crossings that land within the
//! minimum feature size of an endpoint or of each other, and any chaining
//! ambiguity all return [`ProfileBooleanError::Unsupported`] rather than a
//! guessed result.

use artificer_protocol::{BooleanOperation, PrecisionPolicy};

use crate::analytic_extrusion::Segment;
use crate::topology::Point2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProfileBooleanError {
    /// Tangency, coincident carriers, slivers below the minimum feature
    /// size, or a chaining ambiguity: outside the regularized v1 domain.
    Unsupported,
    /// The regularized result contains no material at all.
    EmptyResult,
}

/// One connected region: an outer loop and its holes. Orientation is
/// normalized on entry, so callers may pass loops either way round.
#[derive(Clone, Debug)]
pub(crate) struct ProfileRegion {
    pub(crate) outer: Vec<Segment>,
    pub(crate) holes: Vec<Vec<Segment>>,
}

/// Computes the regularized Boolean of two single-region operands, returning
/// the result as zero or more disjoint regions.
pub(crate) fn profile_boolean(
    first: &ProfileRegion,
    second: &ProfileRegion,
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Vec<ProfileRegion>, ProfileBooleanError> {
    profile_boolean_multi(
        std::slice::from_ref(first),
        std::slice::from_ref(second),
        operation,
        precision,
    )
}

/// The multi-region generalization: each operand is a set of disjoint
/// regions, and even-odd classification over the combined loop sets does the
/// rest without further special cases.
pub(crate) fn profile_boolean_multi(
    first: &[ProfileRegion],
    second: &[ProfileRegion],
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Vec<ProfileRegion>, ProfileBooleanError> {
    let tolerances = Tolerances::from(precision);
    let first_loops = oriented_loop_sets(first, tolerances)?;
    let second_loops = oriented_loop_sets(second, tolerances)?;
    let segments = first_loops.iter().chain(&second_loops).map(Vec::len).sum();
    let imprint = std::time::Instant::now();

    // Imprint: every cross-operand segment pair contributes its crossings to
    // both sides' cut lists, sharing the exact intersection points.
    let mut first_cuts = cut_lists(&first_loops);
    let mut second_cuts = cut_lists(&second_loops);
    // A grid over the second operand's segments, so a segment of the first
    // is imprinted against the few second segments its box reaches rather
    // than all of them. Two segments whose boxes are disjoint neither cross,
    // touch, nor share a carrier stretch, so skipping the pair changes no
    // cut; below the grid's threshold every pair is still visited.
    let grid = SegmentGrid::new(&second_loops);
    for (loop_a, segments_a) in first_loops.iter().enumerate() {
        for (index_a, segment_a) in segments_a.iter().enumerate() {
            for (loop_b, index_b) in grid.candidates(segment_bounds(*segment_a)) {
                let segment_b = &second_loops[loop_b][index_b];
                {
                    let crossings = match segment_crossings(*segment_a, *segment_b, tolerances) {
                        Ok(crossings) => crossings,
                        Err(refusal) => {
                            // A shared stretch rather than a crossing. Its two
                            // ends are where either side's classification can
                            // change, so both sides are cut there — with the
                            // same points, so their pieces align bit for bit —
                            // and what the stretch between them means is left
                            // to the classifier, which has a rule for it.
                            let Some(overlap) = carrier_overlap(*segment_a, *segment_b, tolerances)
                            else {
                                return Err(refusal);
                            };
                            for point in overlap.ends {
                                if let Placement::Interior(parameter) = place(
                                    parameter_of(*segment_a, point),
                                    segment_length(*segment_a),
                                    tolerances,
                                ) {
                                    first_cuts[loop_a][index_a].push(Cut { parameter, point });
                                }
                                if let Placement::Interior(parameter) = place(
                                    parameter_of(*segment_b, point),
                                    segment_length(*segment_b),
                                    tolerances,
                                ) {
                                    second_cuts[loop_b][index_b].push(Cut { parameter, point });
                                }
                            }
                            continue;
                        }
                    };
                    for crossing in crossings {
                        if let Some(parameter) = crossing.first_interior {
                            first_cuts[loop_a][index_a].push(Cut {
                                parameter,
                                point: crossing.point,
                            });
                        }
                        if let Some(parameter) = crossing.second_interior {
                            second_cuts[loop_b][index_b].push(Cut {
                                parameter,
                                point: crossing.point,
                            });
                        }
                    }
                }
            }
        }
    }

    crate::perf::record(
        "kernel.profile_boolean.imprint",
        segments,
        imprint.elapsed(),
    );
    let classify = std::time::Instant::now();

    // Split, classify, and select.
    let first_wrapped = wrap_loops(&first_loops);
    let second_wrapped = wrap_loops(&second_loops);
    let mut pieces = Vec::new();
    collect_pieces(
        &first_loops,
        &first_cuts,
        &second_wrapped,
        &second_loops,
        FirstOperandRule::from(operation),
        tolerances,
        &mut pieces,
    )?;
    collect_pieces(
        &second_loops,
        &second_cuts,
        &first_wrapped,
        &first_loops,
        SecondOperandRule::from(operation),
        tolerances,
        &mut pieces,
    )?;
    crate::perf::record(
        "kernel.profile_boolean.classify",
        segments,
        classify.elapsed(),
    );
    if pieces.is_empty() {
        return Err(ProfileBooleanError::EmptyResult);
    }

    // Sew: chain by exact endpoint identity, then nest by even-odd depth.
    //
    // Identity is exact, so endpoints that agree to within the precision
    // policy but not bit for bit have to be made one point first. Two curves
    // crossing where one of them ends — a tangential touch rather than a
    // transverse cut — produce exactly that: the crossing is a vertex of one
    // and an endpoint of the other, and the chain dead-ends between them.
    crate::perf::stage("kernel.profile_boolean.chain", segments, || {
        let pieces = weld_piece_endpoints(pieces, tolerances);
        let loops = chain_pieces(pieces)?;
        nest_loops(loops, tolerances)
    })
}

/// The first operand's loops — welded, oriented, and split at every
/// transverse crossing with the second operand. This is the imprint stage
/// alone, for callers that need a profile whose vertices align bit for bit
/// with a Boolean result computed from the same operands: the split points
/// come from the same deterministic crossing code, so they are the same
/// floats.
pub(crate) fn imprinted_first_loops(
    first: &ProfileRegion,
    second: &ProfileRegion,
    precision: PrecisionPolicy,
) -> Result<Vec<Vec<Segment>>, ProfileBooleanError> {
    let tolerances = Tolerances::from(precision);
    let first_loops = oriented_loops(first, tolerances)?;
    let second_loops = oriented_loops(second, tolerances)?;
    let mut first_cuts = cut_lists(&first_loops);
    let grid = SegmentGrid::new(&second_loops);
    for (loop_a, segments_a) in first_loops.iter().enumerate() {
        for (index_a, segment_a) in segments_a.iter().enumerate() {
            for (loop_b, index_b) in grid.candidates(segment_bounds(*segment_a)) {
                let segment_b = second_loops[loop_b][index_b];
                for crossing in segment_crossings(*segment_a, segment_b, tolerances)? {
                    if let Some(parameter) = crossing.first_interior {
                        first_cuts[loop_a][index_a].push(Cut {
                            parameter,
                            point: crossing.point,
                        });
                    }
                }
            }
        }
    }
    first_loops
        .iter()
        .enumerate()
        .map(|(loop_index, segments)| {
            let mut split = Vec::with_capacity(segments.len());
            for (segment_index, segment) in segments.iter().enumerate() {
                split.extend(split_segment(
                    *segment,
                    &first_cuts[loop_index][segment_index],
                    tolerances,
                )?);
            }
            Ok(split)
        })
        .collect()
}

/// Loose pieces split wherever one crosses or touches another, so that they
/// meet only at their ends: a planar arrangement a face walk can trace.
///
/// A section's curves can cross where no edge of the other solid is — the
/// pinch of a Steinmetz seam is two ellipses crossing where the cylinders
/// are tangent — and a walk that did not know it would trace one loop
/// through the crossing where there are two cells. The cuts come from the
/// Boolean's own crossing code, and both pieces take the very point it
/// finds, so the pieces either side of a crossing meet there to the bit.
pub(crate) fn split_at_mutual_crossings(
    pieces: &[Segment],
    precision: PrecisionPolicy,
) -> Result<Vec<Segment>, ProfileBooleanError> {
    let tolerances = Tolerances::from(precision);
    // A point on a straight piece that runs along a parameter direction is put
    // on it exactly. Where one piece's end lands on another's interior the
    // crossing adopts that end, and the end came from its own arithmetic — a
    // ring chord cut there would lose its level by the difference, which the
    // sewer reads as a helix.
    let aligned = |piece: Segment, point: Point2| -> Point2 {
        let Segment::Line { start, end } = piece else {
            return point;
        };
        let level = |a: f64, b: f64| (a - b).abs() <= 1.0e-12 * a.abs().max(b.abs()).max(1.0);
        Point2::new(
            if level(start.x, end.x) {
                start.x
            } else {
                point.x
            },
            if level(start.y, end.y) {
                start.y
            } else {
                point.y
            },
        )
    };
    let mut cuts: Vec<Vec<Cut>> = vec![Vec::new(); pieces.len()];
    for first in 0..pieces.len() {
        for second in first + 1..pieces.len() {
            for crossing in segment_crossings(pieces[first], pieces[second], tolerances)? {
                let point = aligned(pieces[second], aligned(pieces[first], crossing.point));
                if let Some(parameter) = crossing.first_interior {
                    cuts[first].push(Cut { parameter, point });
                }
                if let Some(parameter) = crossing.second_interior {
                    cuts[second].push(Cut { parameter, point });
                }
            }
        }
    }
    let mut split = Vec::with_capacity(pieces.len());
    for (piece, cuts) in pieces.iter().zip(&cuts) {
        split.extend(split_segment(*piece, cuts, tolerances)?);
    }
    Ok(split)
}

/// Every region's loops, welded and oriented, concatenated into one set.
fn oriented_loop_sets(
    regions: &[ProfileRegion],
    tolerances: Tolerances,
) -> Result<Vec<Vec<Segment>>, ProfileBooleanError> {
    let mut loops = Vec::new();
    for region in regions {
        loops.extend(oriented_loops(region, tolerances)?);
    }
    if loops.is_empty() {
        return Err(ProfileBooleanError::EmptyResult);
    }
    Ok(loops)
}

/// The sub-segments of `chord` lying strictly inside the region bounded by
/// `loops` (even-odd), split at every transverse crossing. Tangential
/// contact or a crossing landing on an endpoint fails closed, exactly as the
/// Boolean's own imprint does.
pub(crate) fn chord_region_pieces(
    chord: Segment,
    loops: &[Vec<Segment>],
    precision: PrecisionPolicy,
) -> Result<Vec<Segment>, ProfileBooleanError> {
    let tolerances = Tolerances::from(precision);
    let chord_length = segment_length(chord);
    let mut cuts: Vec<Cut> = Vec::new();
    for segments in loops {
        for boundary in segments {
            // A chord running *along* a piece of the boundary rather than
            // across it. A clip is not asking which side the material is on —
            // it asks which parts of the chord lie in this region, and a
            // region contains its own boundary. So the shared stretch's ends
            // become cuts and the stretch itself is kept below.
            if let Some(overlap) = carrier_overlap(chord, *boundary, tolerances) {
                for point in overlap.ends {
                    if let Placement::Interior(parameter) =
                        place(parameter_of(chord, point), chord_length, tolerances)
                    {
                        cuts.push(Cut { parameter, point });
                    }
                }
                continue;
            }
            for crossing in segment_crossings(chord, *boundary, tolerances)? {
                if let Some(parameter) = crossing.first_interior {
                    cuts.push(Cut {
                        parameter,
                        point: crossing.point,
                    });
                }
            }
        }
    }
    let pieces = split_segment(chord, &cuts, tolerances)?;
    let wrapped = wrap_loops(loops);
    let mut inside = Vec::new();
    for piece in pieces {
        if coincidence_with(piece, loops, tolerances).is_some() {
            inside.push(piece);
            continue;
        }
        let sample = point_in_loops(evaluate(piece, 0.5), &wrapped);
        let confirm = point_in_loops(evaluate(piece, 0.37), &wrapped);
        if sample != confirm {
            return Err(ProfileBooleanError::Unsupported);
        }
        if sample {
            inside.push(piece);
        }
    }
    Ok(inside)
}

/// Chains loose 2D segments into closed loops by tolerance-welded endpoint
/// identity: endpoints within the agreement adopt one representative point,
/// then the exact chain walk applies. Used to assemble planar sections of a
/// solid from per-face intersection pieces.
pub(crate) fn chain_welded_segments(
    segments: Vec<Segment>,
    precision: PrecisionPolicy,
) -> Result<Vec<Vec<Segment>>, ProfileBooleanError> {
    let tolerances = Tolerances::from(precision);
    if segments.is_empty() {
        return Err(ProfileBooleanError::EmptyResult);
    }
    // Cluster endpoints: each point adopts the first representative within
    // the weld distance.
    let scale = segments
        .iter()
        .flat_map(|segment| [segment.start(), segment.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let weld = tolerances.agreement * scale * 32.0;
    let mut representatives: Vec<Point2> = Vec::new();
    let canonical = |point: Point2, representatives: &mut Vec<Point2>| -> Point2 {
        if let Some(found) = representatives
            .iter()
            .find(|candidate| (candidate.x - point.x).hypot(candidate.y - point.y) <= weld)
        {
            return *found;
        }
        representatives.push(point);
        point
    };
    let welded: Vec<Segment> = segments
        .into_iter()
        .map(|segment| {
            let start = canonical(segment.start(), &mut representatives);
            let end = canonical(segment.end(), &mut representatives);
            match segment {
                Segment::Line { .. } => Segment::Line { start, end },
                Segment::Arc {
                    center,
                    radius,
                    start_angle,
                    sweep,
                    ..
                } => Segment::Arc {
                    center,
                    start,
                    end,
                    radius,
                    start_angle,
                    sweep,
                },
                other @ (Segment::Ellipse { .. }
                | Segment::Harmonic { .. }
                | Segment::Trace { .. }) => other.with_endpoints(start, end),
            }
        })
        .collect();
    // Section pieces arrive undirected: each weld point must touch exactly
    // two segment ends, and the walk flips segments to travel consistently.
    let mut adjacency: std::collections::BTreeMap<(u64, u64), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, segment) in welded.iter().enumerate() {
        adjacency
            .entry(point_key(segment.start()))
            .or_default()
            .push(index);
        adjacency
            .entry(point_key(segment.end()))
            .or_default()
            .push(index);
    }
    if adjacency.values().any(|touching| touching.len() != 2) {
        return Err(ProfileBooleanError::Unsupported);
    }
    let mut used = vec![false; welded.len()];
    let mut loops = Vec::new();
    for start in 0..welded.len() {
        if used[start] {
            continue;
        }
        let mut chain = Vec::new();
        let origin = point_key(welded[start].start());
        let mut cursor_segment = start;
        let mut cursor_forward = true;
        loop {
            used[cursor_segment] = true;
            let oriented = if cursor_forward {
                welded[cursor_segment]
            } else {
                reverse_segment(welded[cursor_segment])
            };
            let arrival = point_key(oriented.end());
            chain.push(oriented);
            if arrival == origin {
                break;
            }
            let touching = adjacency
                .get(&arrival)
                .ok_or(ProfileBooleanError::Unsupported)?;
            let next = touching
                .iter()
                .copied()
                .find(|candidate| !used[*candidate])
                .ok_or(ProfileBooleanError::Unsupported)?;
            cursor_forward = point_key(welded[next].start()) == arrival;
            if !cursor_forward && point_key(welded[next].end()) != arrival {
                return Err(ProfileBooleanError::Unsupported);
            }
            cursor_segment = next;
        }
        loops.push(chain);
    }
    Ok(loops)
}

/// Rewrites a loop so consecutive endpoints are bit-identical, for callers
/// that feed extracted loops to consumers demanding exact junctions.
pub(crate) fn welded(
    segments: &[Segment],
    precision: PrecisionPolicy,
) -> Result<Vec<Segment>, ProfileBooleanError> {
    weld_loop(segments.to_vec(), Tolerances::from(precision))
}

/// How one region's material sits relative to another's, when their
/// boundaries do not cross at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Containment {
    /// The first region's material lies strictly inside the second's, clear
    /// of every hole.
    StrictlyInside,
    /// The two boundaries cross, touch, or otherwise interact.
    Interacting,
    /// The boundaries are disjoint and the first region is not inside the
    /// second (beside it, around it, or inside one of its holes).
    Separate,
}

/// Classifies the first region against the second without computing a full
/// Boolean: the stacked-pocket builder needs to know that a tool sits
/// strictly inside a target's material, and nothing else.
pub(crate) fn region_containment(
    first: &ProfileRegion,
    second: &ProfileRegion,
    precision: PrecisionPolicy,
) -> Result<Containment, ProfileBooleanError> {
    let tolerances = Tolerances::from(precision);
    let first_loops = oriented_loops(first, tolerances)?;
    let second_loops = oriented_loops(second, tolerances)?;
    for segments_a in &first_loops {
        for segment_a in segments_a {
            for segments_b in &second_loops {
                for segment_b in segments_b {
                    if !segment_crossings(*segment_a, *segment_b, tolerances)?.is_empty() {
                        return Ok(Containment::Interacting);
                    }
                }
            }
        }
    }
    let second_wrapped = wrap_loops(&second_loops);
    // With no crossings, one boundary sample decides the whole region.
    let sample = evaluate(first_loops[0][0], 0.5);
    if !point_in_loops(sample, &second_wrapped) {
        return Ok(Containment::Separate);
    }
    // Inside the material — but a hole of the first region swallowing part of
    // the second's boundary would still be an interaction, as would the first
    // region containing one of the second's holes entirely. With no
    // crossings, it suffices that no boundary of the second lies inside the
    // first's material.
    let first_wrapped = wrap_loops(&first_loops);
    for segments in &second_loops {
        if point_in_loops(evaluate(segments[0], 0.5), &first_wrapped) {
            return Ok(Containment::Interacting);
        }
    }
    Ok(Containment::StrictlyInside)
}

// ---------------------------------------------------------------------------
// Selection rules
// ---------------------------------------------------------------------------

/// What the first operand's boundary must satisfy against the second, and
/// whether its retained pieces keep their orientation.
#[derive(Clone, Copy)]
struct Keep {
    keep_inside: bool,
    reverse: bool,
    /// What to do with a piece lying along the other operand's boundary,
    /// where an interior sample decides nothing because both sides of it are
    /// the other operand's edge. `Some(true)` keeps it where the two run the
    /// same way — materials on the same side — `Some(false)` where they run
    /// against each other, and `None` never keeps it.
    keep_coincident: Option<bool>,
}

struct FirstOperandRule;
struct SecondOperandRule;

impl FirstOperandRule {
    fn from(operation: BooleanOperation) -> Keep {
        match operation {
            // A shared stretch bounds the difference only where the two
            // materials lie on opposite sides of it: co-oriented, the
            // minuend's material there is the subtrahend's too and goes with
            // it.
            BooleanOperation::Difference => Keep {
                keep_inside: false,
                reverse: false,
                keep_coincident: Some(false),
            },
            BooleanOperation::Union => Keep {
                keep_inside: false,
                reverse: false,
                keep_coincident: Some(true),
            },
            BooleanOperation::Intersection => Keep {
                keep_inside: true,
                reverse: false,
                keep_coincident: Some(true),
            },
        }
    }
}

impl SecondOperandRule {
    fn from(operation: BooleanOperation) -> Keep {
        match operation {
            // A shared stretch is carried by the first operand's copy of it
            // or by neither, so the second operand never contributes one: two
            // copies of one curve is not a boundary, it is a seam the chain
            // cannot walk.
            BooleanOperation::Union => Keep {
                keep_inside: false,
                reverse: false,
                keep_coincident: None,
            },
            BooleanOperation::Intersection => Keep {
                keep_inside: true,
                reverse: false,
                keep_coincident: None,
            },
            // The subtrahend's boundary inside the minuend bounds the result
            // with the material on its other side.
            BooleanOperation::Difference => Keep {
                keep_inside: true,
                reverse: true,
                keep_coincident: None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Loop preparation
// ---------------------------------------------------------------------------

fn loop_signed_area(segments: &[Segment]) -> f64 {
    segments
        .iter()
        .map(|segment| segment.signed_area_contribution())
        .sum()
}

fn reverse_segment(segment: Segment) -> Segment {
    match segment {
        Segment::Line { start, end } => Segment::Line {
            start: end,
            end: start,
        },
        Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep,
        } => Segment::Arc {
            center,
            start: end,
            end: start,
            radius,
            start_angle: start_angle + sweep,
            sweep: -sweep,
        },
        other @ (Segment::Ellipse { .. } | Segment::Harmonic { .. }) => other.reversed(),
        trace @ Segment::Trace { .. } => trace.reversed(),
    }
}

fn reverse_loop(segments: &[Segment]) -> Vec<Segment> {
    segments
        .iter()
        .rev()
        .copied()
        .map(reverse_segment)
        .collect()
}

/// Rewrites a loop so every junction shares one exact `Point2`: consecutive
/// endpoints within the coordinate agreement adopt the earlier segment's end
/// bit for bit, and the closing junction adopts the loop's start.
///
/// Committed topology stores each coedge's pcurve independently, so two
/// segments meeting at a seam can evaluate their shared vertex to values an
/// ulp apart. The Boolean's sewing stage chains by exact identity — that is
/// what makes it tolerance-free — so the identities are established here,
/// once, at the door.
fn weld_loop(
    mut segments: Vec<Segment>,
    tolerances: Tolerances,
) -> Result<Vec<Segment>, ProfileBooleanError> {
    let count = segments.len();
    if count == 0 {
        return Err(ProfileBooleanError::Unsupported);
    }
    for index in 0..count {
        let expected = segments[(index + count - 1) % count].end();
        let found = segments[index].start();
        if found.x.to_bits() == expected.x.to_bits() && found.y.to_bits() == expected.y.to_bits() {
            continue;
        }
        if (found.x - expected.x).hypot(found.y - expected.y) > tolerances.agreement {
            return Err(ProfileBooleanError::Unsupported);
        }
        segments[index] = match segments[index] {
            Segment::Line { end, .. } => Segment::Line {
                start: expected,
                end,
            },
            Segment::Arc {
                center,
                end,
                radius,
                start_angle,
                sweep,
                ..
            } => Segment::Arc {
                center,
                start: expected,
                end,
                radius,
                start_angle,
                sweep,
            },
            other @ (Segment::Ellipse { .. } | Segment::Harmonic { .. }) => {
                other.with_endpoints(expected, other.end())
            }
            trace @ Segment::Trace { .. } => trace.with_endpoints(expected, trace.end()),
        };
    }
    Ok(segments)
}

/// The operand's loops with material on the left: outer counter-clockwise,
/// holes clockwise.
fn oriented_loops(
    region: &ProfileRegion,
    tolerances: Tolerances,
) -> Result<Vec<Vec<Segment>>, ProfileBooleanError> {
    let mut loops = Vec::with_capacity(1 + region.holes.len());
    let outer_area = loop_signed_area(&region.outer);
    if !outer_area.is_finite() || outer_area == 0.0 {
        return Err(ProfileBooleanError::Unsupported);
    }
    loops.push(weld_loop(
        if outer_area > 0.0 {
            region.outer.clone()
        } else {
            reverse_loop(&region.outer)
        },
        tolerances,
    )?);
    for hole in &region.holes {
        let hole_area = loop_signed_area(hole);
        if !hole_area.is_finite() || hole_area == 0.0 {
            return Err(ProfileBooleanError::Unsupported);
        }
        loops.push(weld_loop(
            if hole_area < 0.0 {
                hole.clone()
            } else {
                reverse_loop(hole)
            },
            tolerances,
        )?);
    }
    Ok(loops)
}

// ---------------------------------------------------------------------------
// Imprint
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Cut {
    parameter: f64,
    point: Point2,
}

fn cut_lists(loops: &[Vec<Segment>]) -> Vec<Vec<Vec<Cut>>> {
    loops
        .iter()
        .map(|segments| vec![Vec::new(); segments.len()])
        .collect()
}

/// One transverse crossing between two segments. A parameter is `None` when
/// the crossing lands exactly on that segment's endpoint, in which case the
/// segment needs no split there — the shared point *is* its vertex.
#[derive(Clone, Copy, Debug)]
struct Crossing {
    point: Point2,
    first_interior: Option<f64>,
    second_interior: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct Tolerances {
    /// Coordinate agreement: below this, two positions are the same point.
    agreement: f64,
    /// Feature floor: structure smaller than this is a sliver and rejects.
    minimum: f64,
}

impl From<PrecisionPolicy> for Tolerances {
    fn from(precision: PrecisionPolicy) -> Self {
        Self {
            agreement: precision.linear_agreement.max(1.0e-12),
            minimum: precision.min_feature_size.max(1.0e-12),
        }
    }
}

/// Where a candidate position falls along a segment of the given length,
/// with the parameter expressed in [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    Outside,
    StartVertex,
    EndVertex,
    Interior(f64),
    /// Inside the span but within the feature floor of an endpoint: a sliver
    /// the regularized domain refuses rather than fabricates.
    Sliver,
}

fn place(parameter: f64, length: f64, tolerances: Tolerances) -> Placement {
    let along = parameter * length;
    if along < -tolerances.agreement || along > length + tolerances.agreement {
        return Placement::Outside;
    }
    if along.abs() <= tolerances.agreement {
        return Placement::StartVertex;
    }
    if (length - along).abs() <= tolerances.agreement {
        return Placement::EndVertex;
    }
    if along < tolerances.minimum || length - along < tolerances.minimum {
        return Placement::Sliver;
    }
    Placement::Interior(parameter)
}

fn segment_length(segment: Segment) -> f64 {
    match segment {
        Segment::Line { start, end } => (end.x - start.x).hypot(end.y - start.y),
        Segment::Arc { radius, sweep, .. } => radius * sweep.abs(),
        Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => {
            segment.length()
        }
    }
}

/// The parameter of `point` along `segment`, by direct projection.
fn parameter_of(segment: Segment, point: Point2) -> f64 {
    match segment {
        Segment::Line { start, end } => {
            let dx = end.x - start.x;
            let dy = end.y - start.y;
            let square = dx.mul_add(dx, dy * dy);
            ((point.x - start.x).mul_add(dx, (point.y - start.y) * dy)) / square
        }
        Segment::Arc {
            center,
            start_angle,
            sweep,
            ..
        } => {
            let angle = (point.y - center.y).atan2(point.x - center.x);
            arc_fraction(angle, start_angle, sweep)
        }
        Segment::Ellipse {
            center,
            u,
            major,
            minor,
            start_angle,
            sweep,
            ..
        } => {
            // The parameter angle is read off the ellipse's own frame,
            // with the axes scaled back to a circle first.
            let along = ((point.x - center.x) * u.x + (point.y - center.y) * u.y) / major;
            let across = ((point.y - center.y) * u.x - (point.x - center.x) * u.y) / minor;
            arc_fraction(across.atan2(along), start_angle, sweep)
        }
        Segment::Harmonic { start, end, .. } => (point.x - start.x) / (end.x - start.x),
        // A trace piece is a graph over its own face's azimuth, so the
        // abscissa is the parameter, moved by the shift.
        Segment::Trace {
            shift, from, to, ..
        } => (point.x - shift.x - from) / (to - from),
    }
}

/// The fraction of the sweep at which `angle` sits, in [0, 1) measured from
/// the start and wrapping the full turn.
fn arc_fraction(angle: f64, start_angle: f64, sweep: f64) -> f64 {
    let progress = if sweep >= 0.0 {
        (angle - start_angle).rem_euclid(std::f64::consts::TAU)
    } else {
        (start_angle - angle).rem_euclid(std::f64::consts::TAU)
    };
    // A wrap-around hit at the very start belongs to parameter zero, not one.
    let fraction = progress / sweep.abs();
    if fraction >= std::f64::consts::TAU / sweep.abs() - 1.0e-9 {
        0.0
    } else {
        fraction
    }
}

fn evaluate(segment: Segment, parameter: f64) -> Point2 {
    match segment {
        Segment::Line { start, end } => Point2::new(
            (end.x - start.x).mul_add(parameter, start.x),
            (end.y - start.y).mul_add(parameter, start.y),
        ),
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let angle = sweep.mul_add(parameter, start_angle);
            Point2::new(
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            )
        }
        Segment::Ellipse { .. } | Segment::Harmonic { .. } => segment.point_at(parameter),
        trace @ Segment::Trace { .. } => trace.point_at(parameter),
    }
}

/// A stretch two boundary segments share, and whether they run along it the
/// same way.
#[derive(Clone, Copy, Debug)]
struct Overlap {
    ends: [Point2; 2],
    same_way: bool,
}

/// Where two segments lie on one carrier and overlap along it.
///
/// This is the contact the transverse pipeline has no answer for: not a
/// crossing at a point but a shared stretch, where which side the material
/// lies on is the whole question. Finding it is the first half of answering
/// it; the classifier does the rest.
fn carrier_overlap(first: Segment, second: Segment, tolerances: Tolerances) -> Option<Overlap> {
    match (first, second) {
        (Segment::Line { start: p0, end: p1 }, Segment::Line { start: q0, end: q1 }) => {
            let direction = Point2::new(p1.x - p0.x, p1.y - p0.y);
            let length = direction.x.hypot(direction.y);
            let span = (q1.x - q0.x).hypot(q1.y - q0.y);
            if length <= tolerances.minimum || span <= tolerances.minimum {
                return None;
            }
            let unit = Point2::new(direction.x / length, direction.y / length);
            let scale = length.max(span).max(1.0);
            let across =
                |point: Point2| (point.x - p0.x).mul_add(unit.y, -((point.y - p0.y) * unit.x));
            if across(q0).abs() > tolerances.agreement * scale
                || across(q1).abs() > tolerances.agreement * scale
            {
                return None;
            }
            let along = |point: Point2| (point.x - p0.x).mul_add(unit.x, (point.y - p0.y) * unit.y);
            let (a, b) = (along(q0), along(q1));
            let low = a.min(b).max(0.0);
            let high = a.max(b).min(length);
            if high - low <= tolerances.minimum {
                return None;
            }
            Some(Overlap {
                ends: [
                    Point2::new(unit.x.mul_add(low, p0.x), unit.y.mul_add(low, p0.y)),
                    Point2::new(unit.x.mul_add(high, p0.x), unit.y.mul_add(high, p0.y)),
                ],
                same_way: b > a,
            })
        }
        (
            Segment::Arc {
                center: c1,
                radius: r1,
                start_angle: a1,
                sweep: s1,
                ..
            },
            Segment::Arc {
                center: c2,
                radius: r2,
                start_angle: a2,
                sweep: s2,
                ..
            },
        ) => {
            let scale = r1.max(r2).max(1.0);
            if (c1.x - c2.x).hypot(c1.y - c2.y) > tolerances.agreement * scale
                || (r1 - r2).abs() > tolerances.agreement * scale
            {
                return None;
            }
            // Both spans as increasing intervals, the second brought onto the
            // first's branch so the two can be compared at all.
            let (low1, high1) = if s1 >= 0.0 {
                (a1, a1 + s1)
            } else {
                (a1 + s1, a1)
            };
            let (mut low2, mut high2) = if s2 >= 0.0 {
                (a2, a2 + s2)
            } else {
                (a2 + s2, a2)
            };
            let turn = std::f64::consts::TAU;
            while low2 < low1 - turn / 2.0 {
                low2 += turn;
                high2 += turn;
            }
            while low2 > low1 + turn / 2.0 {
                low2 -= turn;
                high2 -= turn;
            }
            let low = low1.max(low2);
            let high = high1.min(high2);
            if (high - low) * r1 <= tolerances.minimum {
                return None;
            }
            let at = |angle: f64| {
                Point2::new(r1.mul_add(angle.cos(), c1.x), r1.mul_add(angle.sin(), c1.y))
            };
            Some(Overlap {
                ends: [at(low), at(high)],
                same_way: (s1 >= 0.0) == (s2 >= 0.0),
            })
        }
        // Two section chords on one cylinder along the same oblique trace:
        // the edge where a bore meets a wall that a tool's own wall then
        // continues. The carrier is a graph over the azimuth, so the shared
        // stretch is the overlap of the two azimuth spans.
        (
            Segment::Harmonic {
                mean,
                amplitude,
                phase,
                start: p0,
                end: p1,
            },
            Segment::Harmonic {
                start: q0, end: q1, ..
            },
        ) if harmonics_share_carrier(first, second, tolerances) => {
            let low = p0.x.min(p1.x).max(q0.x.min(q1.x));
            let high = p0.x.max(p1.x).min(q0.x.max(q1.x));
            if high - low <= tolerances.minimum {
                return None;
            }
            let at = |x: f64| Point2::new(x, amplitude.mul_add((x - phase).cos(), mean));
            Some(Overlap {
                ends: [at(low), at(high)],
                same_way: (p1.x >= p0.x) == (q1.x >= q0.x),
            })
        }
        _ => None,
    }
}

/// Whether a piece lies along any of these boundary loops, and which way.
fn coincidence_with(
    piece: Segment,
    loops: &[Vec<Segment>],
    tolerances: Tolerances,
) -> Option<Overlap> {
    loops
        .iter()
        .flatten()
        .find_map(|boundary| carrier_overlap(piece, *boundary, tolerances))
}

/// All transverse crossings of one segment pair, or `Unsupported` when the
/// pair touches tangentially or shares a carrier.
fn segment_crossings(
    first: Segment,
    second: Segment,
    tolerances: Tolerances,
) -> Result<Vec<Crossing>, ProfileBooleanError> {
    let candidates = carrier_crossings(first, second, tolerances)?;
    let first_length = segment_length(first);
    let second_length = segment_length(second);
    let mut crossings = Vec::new();
    for point in candidates {
        let first_place = place(parameter_of(first, point), first_length, tolerances);
        let second_place = place(parameter_of(second, point), second_length, tolerances);
        if first_place == Placement::Outside || second_place == Placement::Outside {
            continue;
        }
        // A crossing closer to an end than the smallest feature the policy
        // admits *is* that end. Splitting there would make the sliver the
        // check is named for, and refusing would turn a vertex the operands
        // already share into a reason to give up — which is what a tangency
        // landing on a region's own corner looks like. Snapping is what both
        // avoid: the vertex is already there, so nothing needs cutting.
        let settle = |placement: Placement, segment: Segment| match placement {
            Placement::Sliver => {
                if parameter_of(segment, point) < 0.5 {
                    Placement::StartVertex
                } else {
                    Placement::EndVertex
                }
            }
            other => other,
        };
        let first_place = settle(first_place, first);
        let second_place = settle(second_place, second);
        // Resolve the shared point: an endpoint hit adopts the segment's own
        // vertex bit for bit, so both operands chain through one identity.
        let (point, first_interior, second_interior) = match (first_place, second_place) {
            (Placement::Interior(a), Placement::Interior(b)) => (point, Some(a), Some(b)),
            (Placement::StartVertex, Placement::Interior(_)) => {
                let vertex = first.start();
                (vertex, None, Some(parameter_of(second, vertex)))
            }
            (Placement::EndVertex, Placement::Interior(_)) => {
                let vertex = first.end();
                (vertex, None, Some(parameter_of(second, vertex)))
            }
            (Placement::Interior(_), Placement::StartVertex) => {
                let vertex = second.start();
                (vertex, Some(parameter_of(first, vertex)), None)
            }
            (Placement::Interior(_), Placement::EndVertex) => {
                let vertex = second.end();
                (vertex, Some(parameter_of(first, vertex)), None)
            }
            // Vertex-on-vertex contact: neither side needs a split, whichever
            // end each is, so the only question is whether the two ends are
            // the same point. Agreement decides that, not identical bits.
            // Bits were too strict to be a rule about geometry: two ends that
            // are the same corner reached by different arithmetic — one
            // computed from a carrier, one refined from a touch — agree to the
            // last few bits and not to all of them, and which machine is
            // running decides how many. That is how one platform came to cut a
            // shape another refused. Ends this close are welded into one point
            // by the sew stage regardless, so accepting exactly what it will
            // weld is what keeps the two stages telling the same story.
            (
                Placement::StartVertex | Placement::EndVertex,
                Placement::StartVertex | Placement::EndVertex,
            ) => {
                let first_vertex = if first_place == Placement::StartVertex {
                    first.start()
                } else {
                    first.end()
                };
                let second_vertex = if second_place == Placement::StartVertex {
                    second.start()
                } else {
                    second.end()
                };
                let scale = [first_vertex, second_vertex]
                    .into_iter()
                    .map(|point| point.x.abs().max(point.y.abs()))
                    .fold(1.0_f64, f64::max);
                if (first_vertex.x - second_vertex.x).hypot(first_vertex.y - second_vertex.y)
                    <= tolerances.agreement * scale
                {
                    (first_vertex, None, None)
                } else {
                    return Err(ProfileBooleanError::Unsupported);
                }
            }
            _ => unreachable!("outside and sliver placements returned above"),
        };
        crossings.push(Crossing {
            point,
            first_interior,
            second_interior,
        });
    }
    Ok(crossings)
}

/// Candidate crossing points of the two segments' unbounded carriers, or
/// `Unsupported` when the carriers coincide or touch tangentially within
/// either segment's span.
fn carrier_crossings(
    first: Segment,
    second: Segment,
    tolerances: Tolerances,
) -> Result<Vec<Point2>, ProfileBooleanError> {
    match (first, second) {
        (Segment::Line { start: p0, end: p1 }, Segment::Line { start: q0, end: q1 }) => {
            let d1 = Point2::new(p1.x - p0.x, p1.y - p0.y);
            let d2 = Point2::new(q1.x - q0.x, q1.y - q0.y);
            let denominator = d1.x.mul_add(d2.y, -(d1.y * d2.x));
            let scale = segment_length(first).max(segment_length(second));
            if denominator.abs() <= tolerances.agreement * scale {
                // Parallel: coincident overlapping carriers refuse; separated
                // parallels simply do not cross.
                let offset = Point2::new(q0.x - p0.x, q0.y - p0.y);
                let across = offset.x.mul_add(d1.y, -(offset.y * d1.x)) / segment_length(first);
                if across.abs() <= tolerances.agreement {
                    // Same carrier: an actual span overlap is out of domain.
                    let along = |point: Point2| {
                        (point.x - p0.x).mul_add(d1.x, (point.y - p0.y) * d1.y)
                            / segment_length(first)
                    };
                    let (a_low, a_high) = (0.0, segment_length(first));
                    let (b_low, b_high) = {
                        let one = along(q0);
                        let two = along(q1);
                        (one.min(two), one.max(two))
                    };
                    if b_high > a_low + tolerances.agreement
                        && b_low < a_high - tolerances.agreement
                    {
                        return Err(ProfileBooleanError::Unsupported);
                    }
                }
                return Ok(Vec::new());
            }
            let offset = Point2::new(q0.x - p0.x, q0.y - p0.y);
            let t = offset.x.mul_add(d2.y, -(offset.y * d2.x)) / denominator;
            Ok(vec![Point2::new(
                d1.x.mul_add(t, p0.x),
                d1.y.mul_add(t, p0.y),
            )])
        }
        (Segment::Line { start, end }, arc @ Segment::Arc { .. })
        | (arc @ Segment::Arc { .. }, Segment::Line { start, end }) => {
            let Segment::Arc { center, radius, .. } = arc else {
                unreachable!()
            };
            let direction = Point2::new(end.x - start.x, end.y - start.y);
            let length = (direction.x).hypot(direction.y);
            let unit = Point2::new(direction.x / length, direction.y / length);
            let offset = Point2::new(center.x - start.x, center.y - start.y);
            let along = offset.x.mul_add(unit.x, offset.y * unit.y);
            let across = offset.x.mul_add(unit.y, -(offset.y * unit.x));
            let square = radius.mul_add(radius, -(across * across));
            if square.abs() <= 2.0 * tolerances.agreement * radius {
                // A line touching the circle at one point. The boundaries do
                // not cross there, but the touch is still where one operand's
                // boundary stops being inside the other and starts being
                // outside — the shadow of a fillet band's tangency with the
                // wall it rolls against — so it is imprinted like any other
                // crossing. What each side of it is remains a question for the
                // interior sample, which is exactly what the classifier asks.
                let touch = Point2::new(
                    unit.x.mul_add(along, start.x),
                    unit.y.mul_add(along, start.y),
                );
                // On the circle to the bit, so the arc's own radius checks
                // downstream see a true carrier point.
                let angle = (touch.y - center.y).atan2(touch.x - center.x);
                let touch = Point2::new(
                    radius.mul_add(angle.cos(), center.x),
                    radius.mul_add(angle.sin(), center.y),
                );
                return Ok(vec![touch]);
            }
            if square < 0.0 {
                return Ok(Vec::new());
            }
            let reach = square.sqrt();
            // Place each candidate exactly on the circle so arc radius checks
            // downstream see a true carrier point.
            Ok([along - reach, along + reach]
                .into_iter()
                .map(|distance| {
                    let raw = Point2::new(
                        unit.x.mul_add(distance, start.x),
                        unit.y.mul_add(distance, start.y),
                    );
                    let angle = (raw.y - center.y).atan2(raw.x - center.x);
                    Point2::new(
                        radius.mul_add(angle.cos(), center.x),
                        radius.mul_add(angle.sin(), center.y),
                    )
                })
                .collect())
        }
        (
            Segment::Arc {
                center: c1,
                radius: r1,
                ..
            },
            Segment::Arc {
                center: c2,
                radius: r2,
                ..
            },
        ) => {
            let offset = Point2::new(c2.x - c1.x, c2.y - c1.y);
            let separation = offset.x.hypot(offset.y);
            if separation <= tolerances.agreement {
                if (r1 - r2).abs() <= tolerances.agreement {
                    // Same carrier: refuse if the angular spans overlap.
                    if arc_spans_overlap(first, second) {
                        return Err(ProfileBooleanError::Unsupported);
                    }
                }
                return Ok(Vec::new());
            }
            let far = r1 + r2;
            let near = (r1 - r2).abs();
            if separation >= far + tolerances.agreement || separation <= near - tolerances.agreement
            {
                return Ok(Vec::new());
            }
            if (separation - far).abs() <= tolerances.agreement
                || (separation - near).abs() <= tolerances.agreement
            {
                // Circles touching at one point, inside or outside each
                // other. As with a line and a circle, the touch is imprinted
                // and the interior samples either side decide what it means.
                // The point is on both carriers by construction: the centres
                // and the touch are collinear.
                let toward = Point2::new(offset.x / separation, offset.y / separation);
                let touch = Point2::new(r1.mul_add(toward.x, c1.x), r1.mul_add(toward.y, c1.y));
                return Ok(vec![touch]);
            }
            let reach_along = r1
                .mul_add(r1, -(r2 * r2))
                .mul_add(1.0 / (2.0 * separation), separation / 2.0);
            let square = r1.mul_add(r1, -(reach_along * reach_along));
            if square <= 0.0 {
                return Ok(Vec::new());
            }
            let across = square.sqrt();
            let toward = Point2::new(offset.x / separation, offset.y / separation);
            let sideways = Point2::new(-toward.y, toward.x);
            Ok([across, -across]
                .into_iter()
                .map(|reach| {
                    let raw = Point2::new(
                        toward.x.mul_add(reach_along, sideways.x * reach) + c1.x,
                        toward.y.mul_add(reach_along, sideways.y * reach) + c1.y,
                    );
                    // Snap onto the first circle's carrier exactly.
                    let angle = (raw.y - c1.y).atan2(raw.x - c1.x);
                    Point2::new(r1.mul_add(angle.cos(), c1.x), r1.mul_add(angle.sin(), c1.y))
                })
                .collect())
        }
        _ => section_carrier_crossings(first, second, tolerances),
    }
}

/// Candidate crossings when at least one segment is a section chord. The
/// chords are exact — an ellipse, a harmonic — but a crossing with another
/// carrier has no closed form in general, so it is bracketed on a fine
/// sampling of the chord's own parameter and bisected to precision. A near
/// touch that never changes sign is tangential contact, which refuses.
fn section_carrier_crossings(
    first: Segment,
    second: Segment,
    tolerances: Tolerances,
) -> Result<Vec<Point2>, ProfileBooleanError> {
    // Sample along the section chord; the other segment is the carrier.
    let (chord, other) = if first.is_section_chord() {
        (first, second)
    } else {
        (second, first)
    };
    if let (Segment::Harmonic { .. }, Segment::Harmonic { .. }) = (first, second)
        && harmonics_share_carrier(first, second, tolerances)
    {
        return Err(ProfileBooleanError::Unsupported);
    }
    // Signed distance of a point from the other carrier.
    let signed = |point: Point2| -> f64 {
        match other {
            Segment::Line { start, end } => {
                let dx = end.x - start.x;
                let dy = end.y - start.y;
                let length = dx.hypot(dy);
                ((point.x - start.x) * dy - (point.y - start.y) * dx) / length
            }
            Segment::Arc { center, radius, .. } => {
                (point.x - center.x).hypot(point.y - center.y) - radius
            }
            Segment::Ellipse {
                center,
                u,
                major,
                minor,
                ..
            } => {
                let along = ((point.x - center.x) * u.x + (point.y - center.y) * u.y) / major;
                let across = ((point.y - center.y) * u.x - (point.x - center.x) * u.y) / minor;
                (along.hypot(across) - 1.0) * major.min(minor)
            }
            Segment::Harmonic {
                mean,
                amplitude,
                phase,
                ..
            } => point.y - (mean + amplitude * (point.x - phase).cos()),
            Segment::Trace {
                host,
                other,
                branch,
                shift,
                ..
            } => {
                // Height above this piece's own root, as for a harmonic.
                // The quadratic `a·y² + b·y + c` is the curve's implicit
                // form too, but it vanishes on *both* roots, and a piece is
                // one of them: a carrier that crossed the other root would
                // read as crossing this piece, at a point the piece never
                // reaches, and the abscissa alone — which is all a graph's
                // parameter looks at — would place it inside the span.
                let trace = crate::cylinder_trace::CylinderTrace {
                    host,
                    other,
                    branch,
                };
                point.y - shift.y - trace.height_clamped(point.x - shift.x)
            }
        }
    };
    // The chord's whole carrier is not bounded for a harmonic, so sample the
    // chord's own span, slightly extended so an endpoint crossing is found.
    const SAMPLES: usize = 256;
    let extent: f64 = 1.0e-3;
    let at = |index: usize| -> f64 {
        (1.0 + 2.0 * extent).mul_add(index as f64 / SAMPLES as f64, -extent)
    };
    let mut candidates = Vec::new();
    let mut previous = (at(0), signed(chord.point_at(at(0))));
    for index in 1..=SAMPLES {
        let fraction = at(index);
        let value = signed(chord.point_at(fraction));
        if previous.1 == 0.0 {
            candidates.push(chord.point_at(previous.0));
        } else if (previous.1 < 0.0) != (value < 0.0) {
            let (mut low, mut high) = (previous.0, fraction);
            let (mut low_value, _) = (previous.1, value);
            for _ in 0..80 {
                let middle = 0.5 * (low + high);
                let middle_value = signed(chord.point_at(middle));
                if (middle_value < 0.0) == (low_value < 0.0) {
                    low = middle;
                    low_value = middle_value;
                } else {
                    high = middle;
                }
            }
            candidates.push(chord.point_at(0.5 * (low + high)));
        } else if index >= 2 {
            // A local minimum of |distance| that stays on one side within
            // the agreement is a graze: tangential contact.
            let (before, here) = (previous.1.abs(), value.abs());
            let _ = (before, here);
        }
        previous = (fraction, value);
    }
    // Tangential contact: an interior extremum of the signed distance within
    // the agreement, with no sign change around it, inside both spans. As
    // with a line touching a circle, the boundaries meet and part again
    // without crossing, so the touch is imprinted rather than refused and
    // what each side of it means is left to the classifier. Two bands that
    // spring from one wall meet exactly here.
    let mut values = Vec::with_capacity(SAMPLES + 1);
    for index in 0..=SAMPLES {
        values.push((at(index), signed(chord.point_at(at(index)))));
    }
    // How near a touch has to be to count, against the size of what is being
    // measured rather than against a bare number.
    let reach = tolerances.agreement
        * [chord.start(), chord.end(), other.start(), other.end()]
            .into_iter()
            .map(|point| point.x.abs().max(point.y.abs()))
            .fold(1.0_f64, f64::max);
    for window in values.windows(3) {
        let (a, b, c) = (window[0].1, window[1].1, window[2].1);
        if b.abs() > a.abs() || b.abs() > c.abs() || (a < 0.0) != (c < 0.0) {
            continue;
        }
        // Whether this dip reaches the other carrier is a question about the
        // curve, not about where the samples happened to fall: a sample lands
        // wherever the grid puts it, and how near zero it comes there depends
        // on the last bits of a sine — which the platform's own library
        // decides. Two machines would then disagree about whether two shapes
        // touch. So the extremum is found first and *it* is what the reach is
        // measured against; the samples only say where to look.
        let (mut low, mut high) = (window[0].0, window[2].0);
        for _ in 0..80 {
            let third = (high - low) / 3.0;
            let (left, right) = (low + third, high - third);
            if signed(chord.point_at(left)).abs() <= signed(chord.point_at(right)).abs() {
                high = right;
            } else {
                low = left;
            }
        }
        let touch = chord.point_at(0.5 * (low + high));
        if signed(touch).abs() > reach {
            continue;
        }
        let within = |segment: Segment| {
            matches!(
                place(
                    parameter_of(segment, touch),
                    segment_length(segment),
                    tolerances
                ),
                Placement::Interior(_) | Placement::StartVertex | Placement::EndVertex
            )
        };
        if within(chord) && within(other) {
            candidates.push(touch);
        }
    }
    candidates.dedup_by(|a, b| (a.x - b.x).hypot(a.y - b.y) <= tolerances.agreement);
    // A crossing with a straight carrier is put on it exactly. It was found
    // walking the chord, so it lies on the chord to the last bit and on the
    // line only to the bisection's last step — and a ring chord on a
    // cylinder cut at such a point comes out a hundred-billionth off level,
    // which to the sewer is a helix. The chord is defined by its own
    // parameters, not by its ends, so moving its end by that much onto the
    // line moves nothing it is made of.
    if let Segment::Line { start, end } = other {
        let (dx, dy) = (end.x - start.x, end.y - start.y);
        let square = dx.mul_add(dx, dy * dy);
        if square > 0.0 {
            for candidate in &mut candidates {
                let along =
                    (candidate.x - start.x).mul_add(dx, (candidate.y - start.y) * dy) / square;
                *candidate = Point2::new(dx.mul_add(along, start.x), dy.mul_add(along, start.y));
            }
        }
    }
    Ok(candidates)
}

/// Whether two harmonic chords lie on one carrier with overlapping spans.
fn harmonics_share_carrier(first: Segment, second: Segment, tolerances: Tolerances) -> bool {
    let (
        Segment::Harmonic {
            mean: m1,
            amplitude: a1,
            phase: p1,
            start: s1,
            end: e1,
        },
        Segment::Harmonic {
            mean: m2,
            amplitude: a2,
            phase: p2,
            start: s2,
            end: e2,
        },
    ) = (first, second)
    else {
        return false;
    };
    let same_phase = |p: f64, q: f64| {
        let delta = (p - q).rem_euclid(std::f64::consts::TAU);
        delta <= tolerances.agreement || std::f64::consts::TAU - delta <= tolerances.agreement
    };
    let same = (m1 - m2).abs() <= tolerances.agreement
        && ((a1 - a2).abs() <= tolerances.agreement && same_phase(p1, p2)
            || (a1 + a2).abs() <= tolerances.agreement
                && same_phase(p1, p2 + std::f64::consts::PI));
    if !same {
        return false;
    }
    let (low1, high1) = (s1.x.min(e1.x), s1.x.max(e1.x));
    let (low2, high2) = (s2.x.min(e2.x), s2.x.max(e2.x));
    high2 > low1 + tolerances.agreement && low2 < high1 - tolerances.agreement
}

fn arc_spans_overlap(first: Segment, second: Segment) -> bool {
    let (
        Segment::Arc {
            start_angle: a_start,
            sweep: a_sweep,
            ..
        },
        Segment::Arc {
            start_angle: b_start,
            sweep: b_sweep,
            ..
        },
    ) = (first, second)
    else {
        return false;
    };
    // Strictly inside the span: an end that lands on the other arc's start
    // or end is a vertex the two share, not a stretch they share. Two halves
    // of one circle split at the same points do exactly that, and reading
    // the shared vertex as an overlap refused the pair the coincidence rule
    // exists to resolve.
    let inside = |angle: f64, start: f64, sweep: f64| {
        let progress = if sweep >= 0.0 {
            (angle - start).rem_euclid(std::f64::consts::TAU)
        } else {
            (start - angle).rem_euclid(std::f64::consts::TAU)
        };
        let ends = 1.0e-9;
        progress > ends && progress < sweep.abs() - ends
    };
    inside(b_start, a_start, a_sweep)
        || inside(b_start + b_sweep, a_start, a_sweep)
        || inside(a_start, b_start, b_sweep)
        || inside(a_start + a_sweep, b_start, b_sweep)
}

// ---------------------------------------------------------------------------
// Split, classify, select
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Piece {
    segment: Segment,
}

/// Splits one segment at its (sorted, deduplicated) cuts into sub-segments
/// whose endpoints reuse the shared crossing points exactly.
fn split_segment(
    segment: Segment,
    cuts: &[Cut],
    tolerances: Tolerances,
) -> Result<Vec<Segment>, ProfileBooleanError> {
    if cuts.is_empty() {
        return Ok(vec![segment]);
    }
    let mut ordered: Vec<Cut> = cuts.to_vec();
    ordered.sort_by(|left, right| left.parameter.total_cmp(&right.parameter));
    ordered.dedup_by(|left, right| {
        left.point.x.to_bits() == right.point.x.to_bits()
            && left.point.y.to_bits() == right.point.y.to_bits()
    });
    let length = segment_length(segment);
    // A cut that lands on an end, or on the cut before it, is not a cut: the
    // point it would split at is already a vertex of the arrangement, and
    // splitting there again yields a piece of no length for the sew to choke
    // on. Where two curves meet tangentially — the pinch between the lobes of
    // a Steinmetz seam arrives here as exactly this — several cuts converge on
    // one point, and keeping the first is what keeps the arrangement whole.
    let mut kept: Vec<Cut> = Vec::with_capacity(ordered.len());
    let mut previous_parameter = 0.0;
    for cut in ordered {
        if (cut.parameter - previous_parameter) * length < tolerances.minimum {
            continue;
        }
        if (1.0 - cut.parameter) * length < tolerances.minimum {
            continue;
        }
        previous_parameter = cut.parameter;
        kept.push(cut);
    }
    let ordered = kept;
    if ordered.is_empty() {
        return Ok(vec![segment]);
    }

    // A line is cut on itself. A cut can adopt another piece's vertex, which
    // came from that piece's arithmetic and sits a few ulps off this line;
    // cut there, a line along a parameter direction would no longer run
    // along it. The foot of the perpendicular is on the line exactly, and
    // welding brings the other piece's vertex to it.
    let ordered: Vec<Cut> = match segment {
        Segment::Line { start, end } => {
            let (dx, dy) = (end.x - start.x, end.y - start.y);
            let square = dx.mul_add(dx, dy * dy);
            ordered
                .into_iter()
                .map(|cut| {
                    let along =
                        (cut.point.x - start.x).mul_add(dx, (cut.point.y - start.y) * dy) / square;
                    Cut {
                        parameter: cut.parameter,
                        point: Point2::new(dx.mul_add(along, start.x), dy.mul_add(along, start.y)),
                    }
                })
                .collect()
        }
        _ => ordered,
    };
    let mut result = Vec::with_capacity(ordered.len() + 1);
    let mut cursor = segment.start();
    let mut cursor_parameter = 0.0;
    for cut in ordered.iter().chain(std::iter::once(&Cut {
        parameter: 1.0,
        point: segment.end(),
    })) {
        result.push(sub_segment(
            segment,
            cursor,
            cursor_parameter,
            cut.point,
            cut.parameter,
        ));
        cursor = cut.point;
        cursor_parameter = cut.parameter;
    }
    Ok(result)
}

fn sub_segment(
    segment: Segment,
    start: Point2,
    start_parameter: f64,
    end: Point2,
    end_parameter: f64,
) -> Segment {
    match segment {
        Segment::Line { .. } => Segment::Line { start, end },
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle: sweep.mul_add(start_parameter, start_angle),
            sweep: sweep * (end_parameter - start_parameter),
        },
        Segment::Ellipse {
            center,
            u,
            major,
            minor,
            start_angle,
            sweep,
            ..
        } => Segment::Ellipse {
            center,
            u,
            major,
            minor,
            start,
            end,
            start_angle: sweep.mul_add(start_parameter, start_angle),
            sweep: sweep * (end_parameter - start_parameter),
        },
        section @ Segment::Harmonic { .. } => section.with_endpoints(start, end),
        Segment::Trace {
            host,
            other,
            branch,
            shift,
            from,
            to,
            ..
        } => Segment::Trace {
            host,
            other,
            branch,
            shift,
            from: (to - from).mul_add(start_parameter, from),
            to: (to - from).mul_add(end_parameter, from),
            start,
            end,
        },
    }
}

/// Whether a point lies inside an operand's material, by even-odd count over
/// all of its loops. Orientation is irrelevant to parity, so outers and
/// holes need no distinction here. The loops are pre-wrapped once per
/// operand: classification samples every piece, and cloning the segment
/// lists per sample would dominate the whole stage.
pub(crate) fn point_in_loops(
    point: Point2,
    loops: &[crate::analytic_extrusion::AnalyticLoop],
) -> bool {
    let mut inside = false;
    for profile_loop in loops {
        if crate::analytic_extrusion::point_inside_loop(point, profile_loop) {
            inside = !inside;
        }
    }
    inside
}

pub(crate) fn wrap_loops(loops: &[Vec<Segment>]) -> Vec<crate::analytic_extrusion::AnalyticLoop> {
    loops
        .iter()
        .map(|segments| crate::analytic_extrusion::AnalyticLoop {
            segments: segments.clone(),
            signed_area: 0.0,
        })
        .collect()
}

fn collect_pieces(
    loops: &[Vec<Segment>],
    cuts: &[Vec<Vec<Cut>>],
    other: &[crate::analytic_extrusion::AnalyticLoop],
    other_boundary: &[Vec<Segment>],
    rule: Keep,
    tolerances: Tolerances,
    pieces: &mut Vec<Piece>,
) -> Result<(), ProfileBooleanError> {
    for (loop_index, segments) in loops.iter().enumerate() {
        for (segment_index, segment) in segments.iter().enumerate() {
            for piece in split_segment(*segment, &cuts[loop_index][segment_index], tolerances)? {
                // A piece lying along the other operand's boundary is neither
                // in nor out of it, and sampling either side of it only asks
                // the same question again. Which way the two run decides it
                // instead: material on the same side or on opposite sides.
                let keep =
                    if let Some(overlap) = coincidence_with(piece, other_boundary, tolerances) {
                        rule.keep_coincident == Some(overlap.same_way)
                    } else {
                        // Two independent interior samples must agree on the side.
                        let inside = point_in_loops(evaluate(piece, 0.5), other);
                        let confirm = point_in_loops(evaluate(piece, 0.37), other);
                        if inside != confirm {
                            return Err(ProfileBooleanError::Unsupported);
                        }
                        inside == rule.keep_inside
                    };
                if keep {
                    pieces.push(Piece {
                        segment: if rule.reverse {
                            reverse_segment(piece)
                        } else {
                            piece
                        },
                    });
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sew
// ---------------------------------------------------------------------------

fn point_key(point: Point2) -> (u64, u64) {
    (point.x.to_bits(), point.y.to_bits())
}

/// The face count below which the crossing grid reports every segment rather
/// than building cells. Below it the imprint is the exhaustive pairwise scan
/// unchanged; only operands large enough for O(segments²) to bite bucket.
const CROSSING_GRID_THRESHOLD: usize = 64;
const CROSSING_CELLS_PER_AXIS: usize = 64;

/// A conservative axis-aligned box a segment cannot leave, or `None` for a
/// trace, whose parameter is not a plane coordinate: a trace is a candidate
/// for every query, which is safe and rare (traces arise only in the small
/// section operands, never in the large profiles this grid is for).
fn segment_bounds(segment: Segment) -> Option<[Point2; 2]> {
    let mut low = Point2::new(f64::INFINITY, f64::INFINITY);
    let mut high = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut include = |point: Point2| {
        low = Point2::new(low.x.min(point.x), low.y.min(point.y));
        high = Point2::new(high.x.max(point.x), high.y.max(point.y));
    };
    match segment {
        Segment::Line { start, end } => {
            include(start);
            include(end);
        }
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            start,
            end,
        } => {
            include(start);
            include(end);
            // An arc bulges past its chord only where it passes a cardinal
            // direction; those are its only interior extremes.
            for quarter in 0..4 {
                let angle = f64::from(quarter) * std::f64::consts::FRAC_PI_2;
                let ahead = if sweep >= 0.0 {
                    (angle - start_angle).rem_euclid(std::f64::consts::TAU)
                } else {
                    (start_angle - angle).rem_euclid(std::f64::consts::TAU)
                };
                if ahead <= sweep.abs() {
                    include(Point2::new(
                        radius.mul_add(angle.cos(), center.x),
                        radius.mul_add(angle.sin(), center.y),
                    ));
                }
            }
        }
        Segment::Ellipse {
            center,
            major,
            minor,
            ..
        } => {
            let reach = major.abs() + minor.abs();
            include(Point2::new(center.x - reach, center.y - reach));
            include(Point2::new(center.x + reach, center.y + reach));
        }
        Segment::Harmonic {
            mean,
            amplitude,
            start,
            end,
            ..
        } => {
            include(Point2::new(start.x, mean - amplitude.abs()));
            include(Point2::new(end.x, mean + amplitude.abs()));
        }
        Segment::Trace { .. } => return None,
    }
    (low.x.is_finite() && low.y.is_finite() && high.x.is_finite() && high.y.is_finite())
        .then_some([low, high])
}

/// A uniform grid over one operand's boundary segments, keyed by their boxes,
/// so a crossing query visits the segments a box reaches rather than all of
/// them. See [`profile_boolean_multi`]'s imprint for why box-disjoint pairs
/// can be skipped without changing a single cut.
struct SegmentGrid {
    all: Vec<(usize, usize)>,
    grid: Option<CrossingGrid>,
}

struct CrossingGrid {
    origin: Point2,
    cell: [f64; 2],
    dims: [usize; 2],
    cells: Vec<Vec<usize>>,
    large: Vec<usize>,
    /// Segments with no box (traces): a candidate for every query.
    unbounded: Vec<usize>,
    entries: Vec<(usize, usize, Option<[Point2; 2]>)>,
}

impl SegmentGrid {
    fn new(loops: &[Vec<Segment>]) -> Self {
        let all: Vec<(usize, usize)> = loops
            .iter()
            .enumerate()
            .flat_map(|(loop_index, segments)| {
                (0..segments.len()).map(move |index| (loop_index, index))
            })
            .collect();
        let grid = (all.len() >= CROSSING_GRID_THRESHOLD).then(|| CrossingGrid::new(loops, &all));
        Self { all, grid }
    }

    /// The second-operand `(loop, index)` pairs whose box may meet `query`,
    /// ascending, so the imprint visits them in the order the exhaustive scan
    /// did and produces the same cuts. A body without a grid, or a query with
    /// no box, matches every segment.
    fn candidates(&self, query: Option<[Point2; 2]>) -> Vec<(usize, usize)> {
        match (&self.grid, query) {
            (Some(grid), Some(query)) => grid.candidates(query),
            _ => self.all.clone(),
        }
    }
}

impl CrossingGrid {
    fn new(loops: &[Vec<Segment>], all: &[(usize, usize)]) -> Self {
        let entries: Vec<(usize, usize, Option<[Point2; 2]>)> = all
            .iter()
            .map(|&(loop_index, index)| {
                (loop_index, index, segment_bounds(loops[loop_index][index]))
            })
            .collect();
        let mut low = Point2::new(f64::INFINITY, f64::INFINITY);
        let mut high = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut bounded = 0_usize;
        for (_, _, bounds) in &entries {
            if let Some([box_low, box_high]) = bounds {
                low = Point2::new(low.x.min(box_low.x), low.y.min(box_low.y));
                high = Point2::new(high.x.max(box_high.x), high.y.max(box_high.y));
                bounded += 1;
            }
        }
        let per_axis = (bounded as f64).sqrt().ceil().max(1.0) as usize;
        let per_axis = per_axis.clamp(1, CROSSING_CELLS_PER_AXIS);
        let span = [high.x - low.x, high.y - low.y];
        let cell = span.map(|extent| {
            let size = extent / per_axis as f64;
            if size.is_finite() && size > 0.0 {
                size
            } else {
                1.0
            }
        });
        let dims = [per_axis, per_axis];
        let mut cells = vec![Vec::new(); dims[0] * dims[1]];
        let mut large = Vec::new();
        let mut unbounded = Vec::new();
        for (entry_index, (_, _, bounds)) in entries.iter().enumerate() {
            let Some(bounds) = bounds else {
                unbounded.push(entry_index);
                continue;
            };
            let range = cell_span(bounds, low, cell, dims);
            let spans =
                (0..2).any(|axis| range[axis].1 - range[axis].0 + 1 > dims[axis].max(2) / 2);
            if spans {
                large.push(entry_index);
                continue;
            }
            for x in range[0].0..=range[0].1 {
                for y in range[1].0..=range[1].1 {
                    cells[y * dims[0] + x].push(entry_index);
                }
            }
        }
        Self {
            origin: low,
            cell,
            dims,
            cells,
            large,
            unbounded,
            entries,
        }
    }

    fn candidates(&self, query: [Point2; 2]) -> Vec<(usize, usize)> {
        let mut found = self.large.clone();
        found.extend_from_slice(&self.unbounded);
        let range = cell_span(&query, self.origin, self.cell, self.dims);
        for x in range[0].0..=range[0].1 {
            for y in range[1].0..=range[1].1 {
                for &entry in &self.cells[y * self.dims[0] + x] {
                    if self.entries[entry]
                        .2
                        .is_some_and(|bounds| boxes_overlap(&bounds, &query))
                    {
                        found.push(entry);
                    }
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
            .into_iter()
            .map(|entry| {
                let (loop_index, index, _) = self.entries[entry];
                (loop_index, index)
            })
            .collect()
    }
}

fn cell_span(
    bounds: &[Point2; 2],
    origin: Point2,
    cell: [f64; 2],
    dims: [usize; 2],
) -> [(usize, usize); 2] {
    let axis = |value: f64, minimum: f64, size: f64, count: usize| -> usize {
        let index = ((value - minimum) / size).floor();
        if index.is_finite() {
            (index as i64).clamp(0, count as i64 - 1) as usize
        } else {
            0
        }
    };
    [0, 1].map(|dimension| {
        let (min, max, origin_axis) = if dimension == 0 {
            (bounds[0].x, bounds[1].x, origin.x)
        } else {
            (bounds[0].y, bounds[1].y, origin.y)
        };
        let count = dims[dimension];
        let low = axis(min, origin_axis, cell[dimension], count).saturating_sub(1);
        let high = (axis(max, origin_axis, cell[dimension], count) + 1).min(count - 1);
        (low, high)
    })
}

/// A loop's bounding box, the union of its segments' boxes, or `None` when a
/// segment has no closed-form box (a trace) — then the loop is never pruned.
fn loop_bounds(chain: &[Segment]) -> Option<[Point2; 2]> {
    let mut low = Point2::new(f64::INFINITY, f64::INFINITY);
    let mut high = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for segment in chain {
        let [box_low, box_high] = segment_bounds(*segment)?;
        low = Point2::new(low.x.min(box_low.x), low.y.min(box_low.y));
        high = Point2::new(high.x.max(box_high.x), high.y.max(box_high.y));
    }
    (low.x.is_finite() && high.x.is_finite()).then_some([low, high])
}

/// Whether a point could lie inside a loop with this box, growing the box by
/// a relative margin so a point on the boundary is never pruned. `None` (a
/// loop with no box) always could.
fn box_contains(bounds: Option<[Point2; 2]>, point: Point2) -> bool {
    let Some([low, high]) = bounds else {
        return true;
    };
    let scale = point.x.abs().max(point.y.abs()).max(1.0);
    let margin = 1.0e-9 * scale;
    point.x >= low.x - margin
        && point.x <= high.x + margin
        && point.y >= low.y - margin
        && point.y <= high.y + margin
}

fn boxes_overlap(left: &[Point2; 2], right: &[Point2; 2]) -> bool {
    let scale = [left[0], left[1], right[0], right[1]]
        .iter()
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    // Generous against any agreement a caller might set, so a tangent touch
    // the classifier would keep is never pruned; a slightly loose box only
    // costs an exact crossing test that finds nothing.
    let margin = 1.0e-6 * scale;
    left[0].x - margin <= right[1].x
        && right[0].x - margin <= left[1].x
        && left[0].y - margin <= right[1].y
        && right[0].y - margin <= left[1].y
}

/// Makes endpoints that agree within the precision policy into one point, so
/// the exact-identity chaining below sees the arrangement the geometry means
/// rather than the one the arithmetic produced.
fn weld_piece_endpoints(pieces: Vec<Piece>, tolerances: Tolerances) -> Vec<Piece> {
    weld_aligned(
        pieces.into_iter().map(|piece| piece.segment).collect(),
        tolerances.minimum,
    )
    .into_iter()
    .map(|segment| Piece { segment })
    .collect()
}

/// Pieces with every cluster of ends within `weld` of one another made one
/// point, so a walk can key on exact bits.
///
/// The point a cluster becomes keeps every straight piece through it running
/// the way it ran: its abscissa is a vertical line's, if one ends there, and
/// its ordinate a horizontal line's. Ends that meet arrive by different
/// arithmetic and agree only to the last few bits. On a plane that is
/// harmless, but on a cylinder a vertical line is a generator and a
/// horizontal one a ring, and a line that took its neighbour's azimuth or
/// height would run neither way — to the sewer, a helix. A curve's ends carry
/// no such constraint, since a curve is its own parameters, so they are the
/// ones that move.
pub(crate) fn weld_aligned(pieces: Vec<Segment>, weld: f64) -> Vec<Segment> {
    let mut seeds: Vec<Point2> = Vec::new();
    let mut cluster = |point: Point2| -> usize {
        if let Some(found) = seeds
            .iter()
            .position(|seed| (seed.x - point.x).hypot(seed.y - point.y) <= weld)
        {
            return found;
        }
        seeds.push(point);
        seeds.len() - 1
    };
    let ends: Vec<[usize; 2]> = pieces
        .iter()
        .map(|piece| [cluster(piece.start()), cluster(piece.end())])
        .collect();
    let level = |a: f64, b: f64| (a - b).abs() <= 1.0e-12 * a.abs().max(b.abs()).max(1.0);
    let mut abscissas: Vec<Option<f64>> = vec![None; seeds.len()];
    let mut ordinates: Vec<Option<f64>> = vec![None; seeds.len()];
    for (piece, [first, last]) in pieces.iter().zip(&ends) {
        if let Segment::Line { start, end } = piece {
            if level(start.x, end.x) {
                abscissas[*first] = Some(start.x);
                abscissas[*last] = Some(start.x);
            }
            if level(start.y, end.y) {
                ordinates[*first] = Some(start.y);
                ordinates[*last] = Some(start.y);
            }
        }
    }
    let point = |index: usize| {
        Point2::new(
            abscissas[index].unwrap_or(seeds[index].x),
            ordinates[index].unwrap_or(seeds[index].y),
        )
    };
    pieces
        .into_iter()
        .zip(ends)
        .map(|(piece, [first, last])| piece.with_endpoints(point(first), point(last)))
        .collect()
}

/// Chains the retained pieces into closed loops by exact endpoint identity.
///
/// A vertex usually has one outgoing piece and the walk is forced. Where a
/// boundary touches itself — the pinch between the two lobes of a Steinmetz
/// seam is the case that brought this about — several pieces leave the same
/// point and "the next one" has to be decided by direction rather than by
/// being the only one. The walk takes the first piece anticlockwise from the
/// way it came in, the sharpest turn to the right.
///
/// That turn keeps two holes that touch at a point apart, as two loops. Two
/// outer lobes that touch at a point it carries on from one round the
/// other, into one loop that passes the point twice: the half-walls of a
/// Steinmetz crossing are each bounded so, one face apiece, and the sewn
/// solid validates. A caller that needs simple loops — a revolve, where the
/// point would sweep a circle of contact — refuses such a loop at its own
/// self-intersection check.
fn chain_pieces(pieces: Vec<Piece>) -> Result<Vec<Vec<Segment>>, ProfileBooleanError> {
    use std::collections::BTreeMap;
    let mut outgoing: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
    for (index, piece) in pieces.iter().enumerate() {
        outgoing
            .entry(point_key(piece.segment.start()))
            .or_default()
            .push(index);
    }
    if outgoing.values().any(Vec::is_empty) {
        return Err(ProfileBooleanError::Unsupported);
    }
    // The direction a piece sets off in, and the direction it arrives by.
    let leaving = |index: usize| -> Option<(f64, f64)> {
        let segment = pieces[index].segment;
        let from = segment.start();
        let to = evaluate(segment, 0.05);
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let length = dx.hypot(dy);
        (length > 0.0).then_some((dx / length, dy / length))
    };
    let arriving = |index: usize| -> Option<(f64, f64)> {
        let segment = pieces[index].segment;
        let from = evaluate(segment, 0.95);
        let to = segment.end();
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let length = dx.hypot(dy);
        (length > 0.0).then_some((dx / length, dy / length))
    };
    let mut used = vec![false; pieces.len()];
    let mut loops = Vec::new();
    for start in 0..pieces.len() {
        if used[start] {
            continue;
        }
        let mut chain = Vec::new();
        let mut cursor = start;
        let origin = point_key(pieces[start].segment.start());
        loop {
            if used[cursor] {
                // Re-entered a consumed piece without closing: ambiguous.
                return Err(ProfileBooleanError::Unsupported);
            }
            used[cursor] = true;
            chain.push(pieces[cursor].segment);
            let next_key = point_key(pieces[cursor].segment.end());
            if next_key == origin {
                break;
            }
            let Some(candidates) = outgoing.get(&next_key) else {
                return Err(ProfileBooleanError::Unsupported);
            };
            let back = arriving(cursor).map(|(x, y)| (-x, -y));
            let next = candidates
                .iter()
                .copied()
                .filter(|candidate| !used[*candidate])
                .min_by(|left, right| {
                    let turn = |candidate: &usize| {
                        let (Some(back), Some(out)) = (back, leaving(*candidate)) else {
                            return f64::INFINITY;
                        };
                        // Counter-clockwise from the way back, so the
                        // smallest turn is the sharpest left.
                        let angle = (back.0 * out.1 - back.1 * out.0)
                            .atan2(back.0 * out.0 + back.1 * out.1);
                        if angle <= 1.0e-12 {
                            angle + std::f64::consts::TAU
                        } else {
                            angle
                        }
                    };
                    turn(left).total_cmp(&turn(right))
                })
                .or_else(|| candidates.iter().copied().find(|index| !used[*index]));
            let Some(next) = next else {
                return Err(ProfileBooleanError::Unsupported);
            };
            cursor = next;
        }
        loops.push(chain);
    }
    Ok(loops)
}

/// Nests chained loops into regions by even-odd containment depth, checking
/// that orientation agrees with depth as the material-on-the-left rule
/// requires.
fn nest_loops(
    loops: Vec<Vec<Segment>>,
    tolerances: Tolerances,
) -> Result<Vec<ProfileRegion>, ProfileBooleanError> {
    let samples: Vec<Point2> = loops
        .iter()
        .map(|segments| evaluate(segments[0], 0.5))
        .collect();
    let areas: Vec<f64> = loops.iter().map(|chain| loop_signed_area(chain)).collect();
    // A sliver has little area, or little width for its length however long
    // it runs: twice the area over the perimeter is the width of a strip.
    if areas.iter().zip(&loops).any(|(area, chain)| {
        let perimeter = chain.iter().copied().map(segment_length).sum::<f64>();
        !area.is_finite()
            || area.abs() < tolerances.minimum * tolerances.minimum
            || 2.0 * area.abs() < tolerances.minimum * perimeter
    }) {
        return Err(ProfileBooleanError::Unsupported);
    }

    let wrapped = wrap_loops(&loops);
    // A loop's bounding box, computed once: a point outside the box is
    // outside the loop, so the box pre-filter turns the containment depth
    // from O(loops²) point-in-loop tests into O(loops²) cheap box checks plus
    // one point-in-loop per loop that actually encloses the point. On a face
    // carrying hundreds of holes that is the difference between a quadratic
    // pass and a near-linear one.
    let boxes: Vec<Option<[Point2; 2]>> = loops.iter().map(|chain| loop_bounds(chain)).collect();
    let depth_of = |index: usize| -> usize {
        wrapped
            .iter()
            .enumerate()
            .filter(|(other, profile_loop)| {
                *other != index
                    && box_contains(boxes[*other], samples[index])
                    && point_in_loops(samples[index], std::slice::from_ref(profile_loop))
            })
            .count()
    };
    let depths: Vec<usize> = (0..loops.len()).map(depth_of).collect();

    // Depth parity must match orientation: even depth ⇒ outer ⇒ positive
    // area, odd depth ⇒ hole ⇒ negative area.
    for (index, depth) in depths.iter().enumerate() {
        let outer = depth.is_multiple_of(2);
        if outer != (areas[index] > 0.0) {
            return Err(ProfileBooleanError::Unsupported);
        }
    }

    let mut regions: Vec<(usize, ProfileRegion)> = Vec::new();
    for (index, chain) in loops.iter().enumerate() {
        if depths[index].is_multiple_of(2) {
            regions.push((
                index,
                ProfileRegion {
                    outer: chain.clone(),
                    holes: Vec::new(),
                },
            ));
        }
    }
    for (index, chain) in loops.iter().enumerate() {
        if !depths[index].is_multiple_of(2) {
            // The hole's parent is the innermost containing outer: the outer
            // that contains it at depth exactly one less.
            let parent = regions
                .iter_mut()
                .filter(|(outer_index, _)| {
                    depths[*outer_index] + 1 == depths[index]
                        && box_contains(boxes[*outer_index], samples[index])
                        && point_in_loops(
                            samples[index],
                            std::slice::from_ref(&wrapped[*outer_index]),
                        )
                })
                .min_by(|(left, _), (right, _)| areas[*left].abs().total_cmp(&areas[*right].abs()));
            let Some((_, region)) = parent else {
                return Err(ProfileBooleanError::Unsupported);
            };
            region.holes.push(chain.clone());
        }
    }
    Ok(regions.into_iter().map(|(_, region)| region).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rectangle(min: (f64, f64), max: (f64, f64)) -> Vec<Segment> {
        let corners = [
            Point2::new(min.0, min.1),
            Point2::new(max.0, min.1),
            Point2::new(max.0, max.1),
            Point2::new(min.0, max.1),
        ];
        (0..4)
            .map(|index| Segment::Line {
                start: corners[index],
                end: corners[(index + 1) % 4],
            })
            .collect()
    }

    fn circle(center: (f64, f64), radius: f64) -> Vec<Segment> {
        // Two exact semicircles, seam at azimuth 0 and π (ADR 0016).
        let center = Point2::new(center.0, center.1);
        let east = Point2::new(center.x + radius, center.y);
        let west = Point2::new(center.x - radius, center.y);
        vec![
            Segment::Arc {
                center,
                start: east,
                end: west,
                radius,
                start_angle: 0.0,
                sweep: std::f64::consts::PI,
            },
            Segment::Arc {
                center,
                start: west,
                end: east,
                radius,
                start_angle: std::f64::consts::PI,
                sweep: std::f64::consts::PI,
            },
        ]
    }

    fn region(outer: Vec<Segment>) -> ProfileRegion {
        ProfileRegion {
            outer,
            holes: Vec::new(),
        }
    }

    fn total_area(regions: &[ProfileRegion]) -> f64 {
        regions
            .iter()
            .map(|region| {
                loop_signed_area(&region.outer)
                    + region
                        .holes
                        .iter()
                        .map(|hole| loop_signed_area(hole))
                        .sum::<f64>()
            })
            .sum()
    }

    fn run(
        first: &ProfileRegion,
        second: &ProfileRegion,
        operation: BooleanOperation,
    ) -> Vec<ProfileRegion> {
        profile_boolean(first, second, operation, PrecisionPolicy::default())
            .expect("the operation is inside the regularized domain")
    }

    /// A difference that leaves a strip narrower than the minimum feature
    /// is a sliver however long it is, and is refused.
    #[test]
    fn a_long_strip_narrower_than_the_minimum_feature_is_a_sliver() {
        let minimum = PrecisionPolicy::default().min_feature_size;
        let first = region(rectangle((0.0, 0.0), (100.0, 10.0)));
        let second = region(rectangle((-1.0, -1.0), (101.0, 10.0 - minimum / 4.0)));
        let outcome = profile_boolean(
            &first,
            &second,
            BooleanOperation::Difference,
            PrecisionPolicy::default(),
        );
        assert!(
            matches!(outcome, Err(ProfileBooleanError::Unsupported)),
            "{outcome:?}"
        );
    }

    #[test]
    fn overlapping_rectangles_union_difference_and_intersect_exactly() {
        let first = region(rectangle((0.0, 0.0), (4.0, 4.0)));
        let second = region(rectangle((2.0, 1.0), (6.0, 3.0)));
        let union = run(&first, &second, BooleanOperation::Union);
        assert_eq!(union.len(), 1);
        assert!((total_area(&union) - (16.0 + 8.0 - 4.0)).abs() < 1.0e-12);

        let difference = run(&first, &second, BooleanOperation::Difference);
        assert!((total_area(&difference) - 12.0).abs() < 1.0e-12);

        let intersection = run(&first, &second, BooleanOperation::Intersection);
        assert_eq!(intersection.len(), 1);
        assert!((total_area(&intersection) - 4.0).abs() < 1.0e-12);
    }

    #[test]
    fn a_disjoint_hole_subtracts_without_any_boundary_crossing() {
        let plate = region(rectangle((0.0, 0.0), (10.0, 8.0)));
        let hole = region(circle((5.0, 4.0), 1.5));
        let result = run(&plate, &hole, BooleanOperation::Difference);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].holes.len(), 1);
        let expected = 80.0 - std::f64::consts::PI * 1.5 * 1.5;
        assert!((total_area(&result) - expected).abs() < 1.0e-9);
    }

    #[test]
    fn a_circle_crossing_the_boundary_notches_the_rectangle() {
        // Centre on the boundary carrier: half the disc removes.
        let plate = region(rectangle((0.0, 0.0), (10.0, 8.0)));
        let bite = region(circle((0.0, 4.0), 2.0));
        let result = run(&plate, &bite, BooleanOperation::Difference);
        assert_eq!(result.len(), 1);
        let expected = 80.0 - std::f64::consts::PI * 2.0 * 2.0 / 2.0;
        assert!(
            (total_area(&result) - expected).abs() < 1.0e-9,
            "area {} should equal {expected}",
            total_area(&result)
        );
    }

    #[test]
    fn two_overlapping_circles_union_into_one_lens_bounded_region() {
        let (radius, offset) = (3.0_f64, 4.0_f64);
        let first = region(circle((0.0, 0.0), radius));
        let second = region(circle((offset, 0.0), radius));
        let result = run(&first, &second, BooleanOperation::Union);
        assert_eq!(result.len(), 1);
        let half = offset / 2.0;
        let lens = 2.0
            * radius.mul_add(
                radius * (half / radius).acos(),
                -(half * (radius * radius - half * half).sqrt()),
            );
        let expected = 2.0 * std::f64::consts::PI * radius * radius - lens;
        assert!(
            (total_area(&result) - expected).abs() < 1.0e-9,
            "area {} should equal {expected}",
            total_area(&result)
        );
    }

    #[test]
    fn a_full_width_cut_splits_the_plate_into_two_regions() {
        let plate = region(rectangle((0.0, 0.0), (10.0, 8.0)));
        let cut = region(rectangle((4.0, -1.0), (6.0, 9.0)));
        let result = run(&plate, &cut, BooleanOperation::Difference);
        assert_eq!(result.len(), 2);
        assert!((total_area(&result) - (80.0 - 16.0)).abs() < 1.0e-12);
    }

    #[test]
    fn coincident_boundaries_join_along_the_edge_they_share() {
        let first = region(rectangle((0.0, 0.0), (4.0, 4.0)));
        // Shares the whole edge x = 4 — a coincident carrier overlap.
        let second = region(rectangle((4.0, 0.0), (8.0, 4.0)));
        let joined = profile_boolean(
            &first,
            &second,
            BooleanOperation::Union,
            PrecisionPolicy::default(),
        )
        .expect("two squares meeting along one edge make one rectangle");
        assert_eq!(joined.len(), 1, "one region, not two");
        let outer = &joined[0].outer;
        let points = outer
            .iter()
            .flat_map(|segment| [segment.start(), segment.end()])
            .collect::<Vec<_>>();
        let low = points.iter().fold(f64::MAX, |least, p| least.min(p.x));
        let high = points.iter().fold(f64::MIN, |most, p| most.max(p.x));
        assert!(
            (low - 0.0).abs() < 1.0e-9 && (high - 8.0).abs() < 1.0e-9,
            "the union spans both squares: {low} to {high}"
        );
    }

    #[test]
    fn a_tool_that_swallows_the_target_empties_the_difference() {
        let small = region(rectangle((1.0, 1.0), (2.0, 2.0)));
        let large = region(rectangle((0.0, 0.0), (4.0, 4.0)));
        assert_eq!(
            profile_boolean(
                &small,
                &large,
                BooleanOperation::Difference,
                PrecisionPolicy::default()
            )
            .err(),
            Some(ProfileBooleanError::EmptyResult)
        );
    }

    #[test]
    fn subtracting_a_holed_tool_leaves_the_island_as_its_own_region() {
        // The tool is an annulus: material between radius 1 and 3. Cutting it
        // from the plate leaves the plate minus the ring, with the disc under
        // the tool's hole surviving as an island.
        let plate = region(rectangle((-6.0, -6.0), (6.0, 6.0)));
        let tool = ProfileRegion {
            outer: circle((0.0, 0.0), 3.0),
            holes: vec![circle((0.0, 0.0), 1.0)],
        };
        let result = run(&plate, &tool, BooleanOperation::Difference);
        assert_eq!(result.len(), 2);
        let pi = std::f64::consts::PI;
        let expected = 144.0 - pi * 9.0 + pi;
        assert!(
            (total_area(&result) - expected).abs() < 1.0e-9,
            "area {} should equal {expected}",
            total_area(&result)
        );
    }
}

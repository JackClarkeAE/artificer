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

    // Imprint: every cross-operand segment pair contributes its crossings to
    // both sides' cut lists, sharing the exact intersection points.
    let mut first_cuts = cut_lists(&first_loops);
    let mut second_cuts = cut_lists(&second_loops);
    for (loop_a, segments_a) in first_loops.iter().enumerate() {
        for (index_a, segment_a) in segments_a.iter().enumerate() {
            for (loop_b, segments_b) in second_loops.iter().enumerate() {
                for (index_b, segment_b) in segments_b.iter().enumerate() {
                    for crossing in segment_crossings(*segment_a, *segment_b, tolerances)? {
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

    // Split, classify, and select.
    let first_wrapped = wrap_loops(&first_loops);
    let second_wrapped = wrap_loops(&second_loops);
    let mut pieces = Vec::new();
    collect_pieces(
        &first_loops,
        &first_cuts,
        &second_wrapped,
        FirstOperandRule::from(operation),
        tolerances,
        &mut pieces,
    )?;
    collect_pieces(
        &second_loops,
        &second_cuts,
        &first_wrapped,
        SecondOperandRule::from(operation),
        tolerances,
        &mut pieces,
    )?;
    if pieces.is_empty() {
        return Err(ProfileBooleanError::EmptyResult);
    }

    // Sew: chain by exact endpoint identity, then nest by even-odd depth.
    let loops = chain_pieces(pieces)?;
    nest_loops(loops, tolerances)
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
    for (loop_a, segments_a) in first_loops.iter().enumerate() {
        for (index_a, segment_a) in segments_a.iter().enumerate() {
            for segments_b in &second_loops {
                for segment_b in segments_b {
                    for crossing in segment_crossings(*segment_a, *segment_b, tolerances)? {
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
    let mut cuts: Vec<Cut> = Vec::new();
    for segments in loops {
        for boundary in segments {
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
}

struct FirstOperandRule;
struct SecondOperandRule;

impl FirstOperandRule {
    fn from(operation: BooleanOperation) -> Keep {
        match operation {
            BooleanOperation::Union | BooleanOperation::Difference => Keep {
                keep_inside: false,
                reverse: false,
            },
            BooleanOperation::Intersection => Keep {
                keep_inside: true,
                reverse: false,
            },
        }
    }
}

impl SecondOperandRule {
    fn from(operation: BooleanOperation) -> Keep {
        match operation {
            BooleanOperation::Union => Keep {
                keep_inside: false,
                reverse: false,
            },
            BooleanOperation::Intersection => Keep {
                keep_inside: true,
                reverse: false,
            },
            // The subtrahend's boundary inside the minuend bounds the result
            // with the material on its other side.
            BooleanOperation::Difference => Keep {
                keep_inside: true,
                reverse: true,
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
    }
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
        if first_place == Placement::Sliver || second_place == Placement::Sliver {
            return Err(ProfileBooleanError::Unsupported);
        }
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
            // Vertex-on-vertex contact: legal only when the two vertices are
            // the same bits, in which case neither side needs a split.
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
                if first_vertex.x.to_bits() == second_vertex.x.to_bits()
                    && first_vertex.y.to_bits() == second_vertex.y.to_bits()
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
                // Tangential contact: refuse only if the touch is within both
                // spans; a distant graze is no crossing at all.
                let touch = Point2::new(
                    unit.x.mul_add(along, start.x),
                    unit.y.mul_add(along, start.y),
                );
                let line = Segment::Line { start, end };
                let on_line = matches!(
                    place(parameter_of(line, touch), length, tolerances),
                    Placement::Interior(_) | Placement::StartVertex | Placement::EndVertex
                );
                let on_arc = matches!(
                    place(parameter_of(arc, touch), segment_length(arc), tolerances),
                    Placement::Interior(_) | Placement::StartVertex | Placement::EndVertex
                );
                if on_line && on_arc {
                    return Err(ProfileBooleanError::Unsupported);
                }
                return Ok(Vec::new());
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
                // Tangent circles: refuse if the touch lies within both spans.
                let toward = Point2::new(offset.x / separation, offset.y / separation);
                let touch = Point2::new(r1.mul_add(toward.x, c1.x), r1.mul_add(toward.y, c1.y));
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
                if within(first) && within(second) {
                    return Err(ProfileBooleanError::Unsupported);
                }
                return Ok(Vec::new());
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
    }
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
    let inside = |angle: f64, start: f64, sweep: f64| {
        let progress = if sweep >= 0.0 {
            (angle - start).rem_euclid(std::f64::consts::TAU)
        } else {
            (start - angle).rem_euclid(std::f64::consts::TAU)
        };
        progress < sweep.abs()
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
    let mut previous_parameter = 0.0;
    for cut in &ordered {
        if (cut.parameter - previous_parameter) * length < tolerances.minimum {
            return Err(ProfileBooleanError::Unsupported);
        }
        previous_parameter = cut.parameter;
    }
    if (1.0 - previous_parameter) * length < tolerances.minimum {
        return Err(ProfileBooleanError::Unsupported);
    }

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
    }
}

/// Whether a point lies inside an operand's material, by even-odd count over
/// all of its loops. Orientation is irrelevant to parity, so outers and
/// holes need no distinction here. The loops are pre-wrapped once per
/// operand: classification samples every piece, and cloning the segment
/// lists per sample would dominate the whole stage.
fn point_in_loops(point: Point2, loops: &[crate::analytic_extrusion::AnalyticLoop]) -> bool {
    let mut inside = false;
    for profile_loop in loops {
        if crate::analytic_extrusion::point_inside_loop(point, profile_loop) {
            inside = !inside;
        }
    }
    inside
}

fn wrap_loops(loops: &[Vec<Segment>]) -> Vec<crate::analytic_extrusion::AnalyticLoop> {
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
    rule: Keep,
    tolerances: Tolerances,
    pieces: &mut Vec<Piece>,
) -> Result<(), ProfileBooleanError> {
    for (loop_index, segments) in loops.iter().enumerate() {
        for (segment_index, segment) in segments.iter().enumerate() {
            for piece in split_segment(*segment, &cuts[loop_index][segment_index], tolerances)? {
                // Two independent interior samples must agree on the side.
                let inside = point_in_loops(evaluate(piece, 0.5), other);
                let confirm = point_in_loops(evaluate(piece, 0.37), other);
                if inside != confirm {
                    return Err(ProfileBooleanError::Unsupported);
                }
                if inside == rule.keep_inside {
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

/// Chains the retained pieces into closed loops by exact endpoint identity.
/// Every vertex must have exactly one outgoing piece, or the arrangement is
/// ambiguous and the whole operation refuses.
fn chain_pieces(pieces: Vec<Piece>) -> Result<Vec<Vec<Segment>>, ProfileBooleanError> {
    use std::collections::BTreeMap;
    let mut outgoing: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
    for (index, piece) in pieces.iter().enumerate() {
        outgoing
            .entry(point_key(piece.segment.start()))
            .or_default()
            .push(index);
    }
    if outgoing.values().any(|candidates| candidates.len() != 1) {
        return Err(ProfileBooleanError::Unsupported);
    }
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
            cursor = candidates[0];
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
    if areas
        .iter()
        .any(|area| !area.is_finite() || area.abs() < tolerances.minimum * tolerances.minimum)
    {
        return Err(ProfileBooleanError::Unsupported);
    }

    let wrapped = wrap_loops(&loops);
    let depth_of = |index: usize| -> usize {
        wrapped
            .iter()
            .enumerate()
            .filter(|(other, profile_loop)| {
                *other != index
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
    fn coincident_boundaries_refuse_rather_than_guess() {
        let first = region(rectangle((0.0, 0.0), (4.0, 4.0)));
        // Shares the whole edge x = 4 — a coincident carrier overlap.
        let second = region(rectangle((4.0, 0.0), (8.0, 4.0)));
        assert_eq!(
            profile_boolean(
                &first,
                &second,
                BooleanOperation::Union,
                PrecisionPolicy::default()
            )
            .err(),
            Some(ProfileBooleanError::Unsupported)
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

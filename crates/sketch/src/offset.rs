//! Exact offset of a connected chain of analytic curves.
//!
//! One chain in, one chain out, at a signed distance measured to the left of
//! travel. The work is entirely local to the chain: each curve gets its own
//! exact offset, and each corner between two of them is where those two exact
//! offsets meet. Nothing here reads the sketch, the arrangement, or the
//! document — the caller supplies the chain and this module answers with
//! geometry or a named refusal.
//!
//! The result matches the topology of what it came from: four lines offset to
//! four lines, so a rectangle offsets to a rectangle. That is what makes an
//! offset something you can dimension, constrain and offset again, and it is
//! what every offset in this module is derived from — the parent curves, and
//! nothing added to them.
//!
//! The exact domain is lines, circular arcs and circles. A B-spline is refused
//! by name rather than approximated: the offset of a degree-*n* B-spline is not
//! a B-spline, and an approximation here would be the one place in this crate
//! where display-grade geometry became authored geometry.
//!
//! [The sketch offset plan](../../../docs/architecture/geometry-kernel/sketch-offset-plan.md)
//! is the specification, including the self-intersection pruning this pass
//! deliberately refuses instead of attempting.

use artificer_protocol::PrecisionPolicy;

use crate::geometry::{
    CurveDirection, EvaluatedCurve2, SketchPoint2, SketchVector2, angle_of,
    directed_sweep_allow_zero, direction_sign,
};

/// A connected run of curves, in traversal order.
///
/// Each curve begins where the previous one ended; a closed chain additionally
/// has its last curve ending where the first begins. Orientation is the
/// caller's job, because only the caller knows which way round the chain was
/// walked, and which side the offset lands on depends on it.
#[derive(Clone, Debug, PartialEq)]
pub struct OffsetChain {
    pub curves: Vec<EvaluatedCurve2>,
    pub closed: bool,
}

impl OffsetChain {
    #[must_use]
    pub const fn new(curves: Vec<EvaluatedCurve2>, closed: bool) -> Self {
        Self { curves, closed }
    }

    /// The same chain walked the other way, which swaps which side of it
    /// "left of travel" names.
    #[must_use]
    pub fn reversed(&self) -> Self {
        Self {
            curves: self
                .curves
                .iter()
                .rev()
                .map(EvaluatedCurve2::reverse)
                .collect(),
            closed: self.closed,
        }
    }
}

/// Where one curve of the result came from.
///
/// Carried out of the offset so a caller can give each generated curve a stable
/// identity: an offset curve is keyed on the source it came from, a join arc on
/// the corner it fills, and both survive a distance edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OffsetOrigin {
    /// The offset of the source curve at this index in the input chain.
    Source(usize),
    /// A join arc filling the corner that follows the source at this index.
    Join(usize),
}

/// One curve of the offset chain, with its provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct OffsetCurve {
    pub curve: EvaluatedCurve2,
    pub origin: OffsetOrigin,
}

/// Why an offset could not be produced.
///
/// Every variant names one curve or one corner, because "the offset failed" is
/// not something a user can act on and "the corner after the third curve
/// collapses" is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OffsetError {
    /// No curves were supplied.
    EmptyChain,
    /// A B-spline, which has no exact circular-arc offset.
    UnsupportedSource { index: usize },
    /// The curve at this index does not begin where the previous one ended.
    ChainNotConnected { index: usize },
    /// A closed chain whose last curve does not return to the first.
    ChainNotClosed,
    /// The distance is zero, non-finite, or below the modelling floor.
    DistanceTooSmall,
    /// An arc or circle whose offset radius reaches zero: at this distance the
    /// curve has no offset, only a point.
    CurveCollapses { index: usize },
    /// A concave corner whose two offsets no longer reach each other. The
    /// distance has eaten one of the curves that formed it.
    CornerCollapses { corner: usize },
}

impl std::fmt::Display for OffsetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyChain => formatter.write_str("offset needs at least one curve"),
            Self::UnsupportedSource { index } => write!(
                formatter,
                "curve {index} is a spline, which has no exact offset"
            ),
            Self::ChainNotConnected { index } => {
                write!(formatter, "curve {index} does not join the one before it")
            }
            Self::ChainNotClosed => {
                formatter.write_str("a closed chain must return to where it started")
            }
            Self::DistanceTooSmall => {
                formatter.write_str("the offset distance is below the modelling resolution")
            }
            Self::CurveCollapses { index } => write!(
                formatter,
                "curve {index} collapses to a point at this offset distance"
            ),
            Self::CornerCollapses { corner } => write!(
                formatter,
                "the corner after curve {corner} collapses at this offset distance"
            ),
        }
    }
}

impl std::error::Error for OffsetError {}

/// Offsets `chain` by `distance`, positive to the left of travel.
///
/// The result reads in the same order as the input: the offset of source 0,
/// then whatever join its corner needs, then the offset of source 1, and so on.
/// A closed chain's last corner is between its last and first curves and is
/// emitted last.
pub fn offset_chain(
    chain: &OffsetChain,
    distance: f64,
    precision: &PrecisionPolicy,
) -> Result<Vec<OffsetCurve>, OffsetError> {
    if chain.curves.is_empty() {
        return Err(OffsetError::EmptyChain);
    }
    if !distance.is_finite() || distance.abs() < precision.min_feature_size {
        return Err(OffsetError::DistanceTooSmall);
    }
    validate_chain(chain, precision)?;

    let mut offsets = Vec::with_capacity(chain.curves.len());
    for (index, curve) in chain.curves.iter().enumerate() {
        offsets.push(offset_one(curve, distance, index, precision)?);
    }

    let corners = corner_count(chain);
    let mut joins: Vec<Option<EvaluatedCurve2>> = vec![None; chain.curves.len()];
    for corner in 0..corners {
        let next = (corner + 1) % chain.curves.len();
        match corner_turn(
            &chain.curves[corner],
            &chain.curves[next],
            distance,
            precision,
        ) {
            CornerTurn::Tangent => {
                // The two offsets already meet, exactly, in exact arithmetic.
                // Make them meet exactly in this one too, so the chain stays
                // watertight through the arrangement's endpoint tolerance.
                let meeting = curve_end(&offsets[corner]);
                set_curve_start(&mut offsets[next], meeting);
            }
            // Both kinds of corner do the same thing: the two offsets meet
            // where their carriers meet. A concave corner reaches that point by
            // trimming and a convex one by extending, which is the difference
            // between them and the whole of it.
            turn @ (CornerTurn::Convex { .. } | CornerTurn::Concave) => {
                let pivot = curve_end(&chain.curves[corner]);
                let Some(meeting) =
                    nearest_carrier_meeting(&offsets[corner], &offsets[next], pivot)
                else {
                    // Carriers that never meet — two arcs whose offset circles
                    // have separated, a line that misses one — leave a gap no
                    // extension can close. At a convex corner the arc on the
                    // source corner is the one curve that closes it and stays
                    // exact; at a concave one the corner is simply gone.
                    let CornerTurn::Convex { left } = turn else {
                        return Err(OffsetError::CornerCollapses { corner });
                    };
                    let from = curve_end(&offsets[corner]);
                    let to = curve_start(&offsets[next]);
                    joins[corner] = round_join(pivot, from, to, left, precision);
                    continue;
                };
                let (before_first, before_second) =
                    (offsets[corner].clone(), offsets[next].clone());
                set_curve_end(&mut offsets[corner], meeting);
                set_curve_start(&mut offsets[next], meeting);
                // Meeting at the carriers must never turn a curve round. That is
                // a curve the distance has consumed, and removing it to rejoin
                // its neighbours is the self-intersection pass this one
                // deliberately does not attempt — so it refuses, and says where.
                if !survives_trim(&before_first, &offsets[corner], precision)
                    || !survives_trim(&before_second, &offsets[next], precision)
                {
                    return Err(OffsetError::CornerCollapses { corner });
                }
            }
        }
    }

    let mut result = Vec::with_capacity(offsets.len() + corners);
    for (index, curve) in offsets.into_iter().enumerate() {
        result.push(OffsetCurve {
            curve,
            origin: OffsetOrigin::Source(index),
        });
        if let Some(join) = joins[index].take() {
            result.push(OffsetCurve {
                curve: join,
                origin: OffsetOrigin::Join(index),
            });
        }
    }
    Ok(result)
}

/// How many corners a chain has: one between each consecutive pair, plus the
/// closing one. A chain of one closed curve — a circle — closes on itself and
/// has none.
const fn corner_count(chain: &OffsetChain) -> usize {
    let curves = chain.curves.len();
    if chain.closed {
        if curves < 2 { 0 } else { curves }
    } else {
        curves - 1
    }
}

fn validate_chain(chain: &OffsetChain, precision: &PrecisionPolicy) -> Result<(), OffsetError> {
    for (index, curve) in chain.curves.iter().enumerate() {
        match curve {
            EvaluatedCurve2::Bspline { .. } => {
                return Err(OffsetError::UnsupportedSource { index });
            }
            // A circle has no ends, so it can only be a chain of one.
            EvaluatedCurve2::Circle { .. } if chain.curves.len() > 1 => {
                return Err(OffsetError::ChainNotConnected { index });
            }
            _ => {}
        }
    }
    for index in 1..chain.curves.len() {
        if !joined(
            curve_end(&chain.curves[index - 1]),
            curve_start(&chain.curves[index]),
            precision,
        ) {
            return Err(OffsetError::ChainNotConnected { index });
        }
    }
    if chain.closed
        && chain.curves.len() > 1
        && !joined(
            curve_end(chain.curves.last().expect("non-empty")),
            curve_start(&chain.curves[0]),
            precision,
        )
    {
        return Err(OffsetError::ChainNotClosed);
    }
    Ok(())
}

fn joined(first: SketchPoint2, second: SketchPoint2, precision: &PrecisionPolicy) -> bool {
    (first - second).length() <= precision.linear_agreement
}

/// The exact offset of one curve.
///
/// A line moves along its own left normal. An arc or circle keeps its centre
/// and changes radius: the left of travel is inward for a counter-clockwise
/// curve and outward for a clockwise one, which is the whole of
/// `radius - sign * distance`.
fn offset_one(
    curve: &EvaluatedCurve2,
    distance: f64,
    index: usize,
    precision: &PrecisionPolicy,
) -> Result<EvaluatedCurve2, OffsetError> {
    match curve {
        EvaluatedCurve2::Line { start, end } => {
            let shift = (*end - *start)
                .normalized()
                .ok_or(OffsetError::CurveCollapses { index })?
                .left_normal()
                * distance;
            Ok(EvaluatedCurve2::Line {
                start: *start + shift,
                end: *end + shift,
            })
        }
        EvaluatedCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let radius = (*start - *center).length();
            let offset_radius = direction_sign(*direction).mul_add(-distance, radius);
            if offset_radius < precision.min_feature_size {
                return Err(OffsetError::CurveCollapses { index });
            }
            Ok(EvaluatedCurve2::CircularArc {
                center: *center,
                start: radial(*center, *start, offset_radius)
                    .ok_or(OffsetError::CurveCollapses { index })?,
                end: radial(*center, *end, offset_radius)
                    .ok_or(OffsetError::CurveCollapses { index })?,
                direction: *direction,
            })
        }
        EvaluatedCurve2::Circle {
            center,
            radius,
            direction,
        } => {
            let offset_radius = direction_sign(*direction).mul_add(-distance, *radius);
            if offset_radius < precision.min_feature_size {
                return Err(OffsetError::CurveCollapses { index });
            }
            Ok(EvaluatedCurve2::Circle {
                center: *center,
                radius: offset_radius,
                direction: *direction,
            })
        }
        EvaluatedCurve2::Bspline { .. } => Err(OffsetError::UnsupportedSource { index }),
    }
}

/// The point at `radius` from `center` in the direction of `through`.
fn radial(center: SketchPoint2, through: SketchPoint2, radius: f64) -> Option<SketchPoint2> {
    Some(center + (through - center).normalized()? * radius)
}

/// What a corner does to the offset that turns it.
///
/// Which one it is decides whether the two offsets have to be extended or
/// trimmed to reach the point where their carriers meet; it does not change
/// that they meet there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CornerTurn {
    /// The curves meet tangentially; so do their offsets.
    Tangent,
    /// The corner turns away from the offset side, opening a gap. `left` is
    /// the handedness of the turn, which is the sense a join arc would sweep
    /// in on the corners where no extension can close the gap.
    Convex { left: bool },
    /// The corner turns towards the offset side, so the two offsets overlap.
    Concave,
}

/// Classifies a corner against the side the offset lands on.
///
/// The turn's handedness comes from the cross product of the two source
/// tangents; whether that opens a gap or an overlap depends on which side the
/// offset went, which is the sign of the distance.
fn corner_turn(
    first: &EvaluatedCurve2,
    second: &EvaluatedCurve2,
    distance: f64,
    precision: &PrecisionPolicy,
) -> CornerTurn {
    let (Some(outgoing), Some(incoming)) = (
        first.tangent(1.0).ok().and_then(SketchVector2::normalized),
        second.tangent(0.0).ok().and_then(SketchVector2::normalized),
    ) else {
        return CornerTurn::Tangent;
    };
    let cross = outgoing.cross(incoming);
    if cross.abs() <= precision.angular_agreement_radians.sin() && outgoing.dot(incoming) > 0.0 {
        return CornerTurn::Tangent;
    }
    if cross * distance < 0.0 {
        CornerTurn::Convex { left: cross > 0.0 }
    } else {
        CornerTurn::Concave
    }
}

fn curve_start(curve: &EvaluatedCurve2) -> SketchPoint2 {
    match curve {
        EvaluatedCurve2::Line { start, .. } | EvaluatedCurve2::CircularArc { start, .. } => *start,
        EvaluatedCurve2::Circle { center, .. } => *center,
        EvaluatedCurve2::Bspline { control_points, .. } => {
            control_points.first().copied().unwrap_or_default()
        }
    }
}

fn curve_end(curve: &EvaluatedCurve2) -> SketchPoint2 {
    match curve {
        EvaluatedCurve2::Line { end, .. } | EvaluatedCurve2::CircularArc { end, .. } => *end,
        EvaluatedCurve2::Circle { center, .. } => *center,
        EvaluatedCurve2::Bspline { control_points, .. } => {
            control_points.last().copied().unwrap_or_default()
        }
    }
}

/// Moves a curve's start, keeping an arc's own circle authoritative: the new
/// endpoint is the given point projected back onto it, never a point off it,
/// because an arc whose ends disagree about the radius is not an arc.
fn set_curve_start(curve: &mut EvaluatedCurve2, point: SketchPoint2) {
    match curve {
        EvaluatedCurve2::Line { start, .. } => *start = point,
        EvaluatedCurve2::CircularArc { center, start, .. } => {
            let radius = (*start - *center).length();
            if let Some(on_circle) = radial(*center, point, radius) {
                *start = on_circle;
            }
        }
        EvaluatedCurve2::Circle { .. } | EvaluatedCurve2::Bspline { .. } => {}
    }
}

fn set_curve_end(curve: &mut EvaluatedCurve2, point: SketchPoint2) {
    match curve {
        EvaluatedCurve2::Line { end, .. } => *end = point,
        EvaluatedCurve2::CircularArc { center, end, .. } => {
            let radius = (*end - *center).length();
            if let Some(on_circle) = radial(*center, point, radius) {
                *end = on_circle;
            }
        }
        EvaluatedCurve2::Circle { .. } | EvaluatedCurve2::Bspline { .. } => {}
    }
}

/// Whether a curve is still itself after a corner trimmed or extended it.
///
/// Long enough to exist, and pointing the same way it did. A line whose
/// direction reversed, or an arc whose sweep wrapped past a full turn, is not a
/// shorter version of the curve — it is the corner having eaten the whole of
/// it, and the neighbours meeting somewhere the chain does not go.
fn survives_trim(
    before: &EvaluatedCurve2,
    after: &EvaluatedCurve2,
    precision: &PrecisionPolicy,
) -> bool {
    match (before, after) {
        (
            EvaluatedCurve2::Line {
                start: was_start,
                end: was_end,
            },
            EvaluatedCurve2::Line { start, end },
        ) => {
            let span = *end - *start;
            span.length() >= precision.min_feature_size && span.dot(*was_end - *was_start) > 0.0
        }
        (
            EvaluatedCurve2::CircularArc { .. },
            EvaluatedCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            },
        ) => {
            if (*end - *start).length() < precision.min_feature_size {
                return false;
            }
            let sweep = directed_sweep_allow_zero(
                angle_of(*start - *center),
                angle_of(*end - *center),
                *direction,
            );
            sweep > precision.angular_agreement_radians
                && sweep < std::f64::consts::TAU - precision.angular_agreement_radians
        }
        _ => true,
    }
}

/// The arc that fills a convex corner whose carriers never meet: centred on the
/// source corner, at the offset radius, from one offset's end to the next's
/// start.
///
/// This is the fallback, not the rule. Extending to the carriers is what keeps
/// the offset's topology the same as its parent's — a rectangle offsets to a
/// rectangle — and it is what a corner of two lines, or of a line and an arc
/// that still reach each other, always gets. Two offset arcs whose circles have
/// separated leave a gap no extension can close, and this arc is the only curve
/// that closes it and stays exact.
fn round_join(
    pivot: SketchPoint2,
    from: SketchPoint2,
    to: SketchPoint2,
    left: bool,
    precision: &PrecisionPolicy,
) -> Option<EvaluatedCurve2> {
    if (to - from).length() < precision.min_feature_size {
        return None;
    }
    Some(EvaluatedCurve2::CircularArc {
        center: pivot,
        start: from,
        end: to,
        direction: if left {
            CurveDirection::CounterClockwise
        } else {
            CurveDirection::Clockwise
        },
    })
}

/// Where two offsets' *carriers* meet, nearest the corner they came from.
///
/// Carriers, not the bounded curves. A convex corner's meeting point always
/// lies past both offsets' ends — that is what makes it convex — and a concave
/// one's routinely lies past a short neighbour's. Whether reaching it is
/// legitimate is [`survives_trim`]'s question, not this one's.
fn nearest_carrier_meeting(
    first: &EvaluatedCurve2,
    second: &EvaluatedCurve2,
    pivot: SketchPoint2,
) -> Option<SketchPoint2> {
    carrier_meetings(first, second)
        .into_iter()
        .min_by(|left, right| {
            (*left - pivot)
                .length_squared()
                .total_cmp(&(*right - pivot).length_squared())
        })
}

/// Every point where the unbounded carriers of two offset curves meet.
fn carrier_meetings(first: &EvaluatedCurve2, second: &EvaluatedCurve2) -> Vec<SketchPoint2> {
    match (carrier(first), carrier(second)) {
        (
            Carrier::Line { point, direction },
            Carrier::Line {
                point: other,
                direction: along,
            },
        ) => {
            let denominator = direction.cross(along);
            if denominator == 0.0 {
                return Vec::new();
            }
            let parameter = (other - point).cross(along) / denominator;
            vec![point + direction * parameter]
        }
        (Carrier::Line { point, direction }, Carrier::Circle { center, radius })
        | (Carrier::Circle { center, radius }, Carrier::Line { point, direction }) => {
            line_circle_meetings(point, direction, center, radius)
        }
        (
            Carrier::Circle {
                center: first_center,
                radius: first_radius,
            },
            Carrier::Circle {
                center: second_center,
                radius: second_radius,
            },
        ) => circle_circle_meetings(first_center, first_radius, second_center, second_radius),
        _ => Vec::new(),
    }
}

/// The unbounded geometry one offset curve lies on.
enum Carrier {
    Line {
        point: SketchPoint2,
        direction: SketchVector2,
    },
    Circle {
        center: SketchPoint2,
        radius: f64,
    },
    /// A curve with no carrier this module reasons about.
    None,
}

fn carrier(curve: &EvaluatedCurve2) -> Carrier {
    match curve {
        EvaluatedCurve2::Line { start, end } => {
            (*end - *start)
                .normalized()
                .map_or(Carrier::None, |direction| Carrier::Line {
                    point: *start,
                    direction,
                })
        }
        EvaluatedCurve2::CircularArc { center, start, .. } => Carrier::Circle {
            center: *center,
            radius: (*start - *center).length(),
        },
        EvaluatedCurve2::Circle { center, radius, .. } => Carrier::Circle {
            center: *center,
            radius: *radius,
        },
        EvaluatedCurve2::Bspline { .. } => Carrier::None,
    }
}

fn line_circle_meetings(
    point: SketchPoint2,
    direction: SketchVector2,
    center: SketchPoint2,
    radius: f64,
) -> Vec<SketchPoint2> {
    // `direction` is normalised, so the quadratic reduces to a foot and a
    // half-chord about it.
    let to_center = center - point;
    let foot = to_center.dot(direction);
    let half_chord_squared = radius.mul_add(radius, -(to_center.length_squared() - foot * foot));
    if half_chord_squared < 0.0 {
        return Vec::new();
    }
    let half_chord = half_chord_squared.sqrt();
    if half_chord == 0.0 {
        return vec![point + direction * foot];
    }
    vec![
        point + direction * (foot - half_chord),
        point + direction * (foot + half_chord),
    ]
}

fn circle_circle_meetings(
    first_center: SketchPoint2,
    first_radius: f64,
    second_center: SketchPoint2,
    second_radius: f64,
) -> Vec<SketchPoint2> {
    let between = second_center - first_center;
    let separation = between.length();
    if separation == 0.0
        || separation > first_radius + second_radius
        || separation < (first_radius - second_radius).abs()
    {
        return Vec::new();
    }
    let Some(along) = between.normalized() else {
        return Vec::new();
    };
    let foot = second_radius.mul_add(
        -second_radius,
        first_radius.mul_add(first_radius, separation * separation),
    ) / (2.0 * separation);
    let half_chord_squared = first_radius.mul_add(first_radius, -(foot * foot));
    let base = first_center + along * foot;
    if half_chord_squared <= 0.0 {
        return vec![base];
    }
    let half_chord = along.left_normal() * half_chord_squared.sqrt();
    vec![base + half_chord, base + (-half_chord)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(u: f64, v: f64) -> SketchPoint2 {
        SketchPoint2::new(u, v)
    }

    fn line(start: (f64, f64), end: (f64, f64)) -> EvaluatedCurve2 {
        EvaluatedCurve2::Line {
            start: point(start.0, start.1),
            end: point(end.0, end.1),
        }
    }

    /// A 10 × 10 square walked counter-clockwise from the origin.
    fn square() -> OffsetChain {
        OffsetChain::new(
            vec![
                line((0.0, 0.0), (10.0, 0.0)),
                line((10.0, 0.0), (10.0, 10.0)),
                line((10.0, 10.0), (0.0, 10.0)),
                line((0.0, 10.0), (0.0, 0.0)),
            ],
            true,
        )
    }

    fn close_to(first: SketchPoint2, second: SketchPoint2) -> bool {
        (first - second).length() < 1.0e-9
    }

    #[track_caller]
    fn expect_line(curve: &EvaluatedCurve2) -> (SketchPoint2, SketchPoint2) {
        match curve {
            EvaluatedCurve2::Line { start, end } => (*start, *end),
            other => panic!("expected a line, got {other:?}"),
        }
    }

    /// The chain is watertight: every curve begins where the last one ended,
    /// and a closed one returns to its start. An offset that leaves a gap is
    /// not a profile, whatever it looks like.
    #[track_caller]
    fn assert_watertight(offset: &[OffsetCurve], closed: bool) {
        for pair in offset.windows(2) {
            assert!(
                close_to(curve_end(&pair[0].curve), curve_start(&pair[1].curve)),
                "gap between {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
        if closed {
            assert!(close_to(
                curve_end(&offset.last().expect("non-empty").curve),
                curve_start(&offset[0].curve)
            ));
        }
    }

    #[test]
    fn a_square_offsets_outward_into_a_larger_square() {
        // Walked counter-clockwise, outward is to the right of travel. Four
        // lines in, four lines out: the offset keeps the topology of what it
        // came from, which is what lets it be dimensioned and offset again.
        let offset = offset_chain(&square(), -2.0, &PrecisionPolicy::default())
            .expect("a square offsets outward");
        assert_eq!(offset.len(), 4);
        assert_watertight(&offset, true);
        assert!(
            offset
                .iter()
                .all(|entry| matches!(entry.origin, OffsetOrigin::Source(_))),
            "no corner needs a curve of its own"
        );

        let corners = offset
            .iter()
            .map(|entry| expect_line(&entry.curve).0)
            .collect::<Vec<_>>();
        for (actual, expected) in corners.iter().zip([
            point(-2.0, -2.0),
            point(12.0, -2.0),
            point(12.0, 12.0),
            point(-2.0, 12.0),
        ]) {
            assert!(close_to(*actual, expected), "{actual:?} vs {expected:?}");
        }
    }

    #[test]
    fn the_same_square_offsets_inward_to_a_smaller_square_with_sharp_corners() {
        let offset = offset_chain(&square(), 2.0, &PrecisionPolicy::default())
            .expect("a square offsets inward");
        // Concave corners trim to their intersection: four sides, no joins.
        assert_eq!(offset.len(), 4);
        assert_watertight(&offset, true);
        assert!(
            offset
                .iter()
                .all(|entry| matches!(entry.origin, OffsetOrigin::Source(_)))
        );

        let corners = offset
            .iter()
            .map(|entry| expect_line(&entry.curve).0)
            .collect::<Vec<_>>();
        for (actual, expected) in corners.iter().zip([
            point(2.0, 2.0),
            point(8.0, 2.0),
            point(8.0, 8.0),
            point(2.0, 8.0),
        ]) {
            assert!(close_to(*actual, expected), "{actual:?} vs {expected:?}");
        }
    }

    #[test]
    fn walking_the_chain_the_other_way_swaps_the_side_the_offset_lands_on() {
        let precision = PrecisionPolicy::default();
        let outward = offset_chain(&square(), -2.0, &precision).expect("outward");
        let reversed = offset_chain(&square().reversed(), 2.0, &precision).expect("reversed");
        // The same four lines, walked the other way: the same larger square,
        // whichever direction the chain was taken in.
        let extent = |offset: &[OffsetCurve]| {
            offset
                .iter()
                .flat_map(|entry| {
                    let (start, end) = expect_line(&entry.curve);
                    [start.u, end.u, start.v, end.v]
                })
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| {
                    (low.min(value), high.max(value))
                })
        };
        assert_eq!(outward.len(), reversed.len());
        let (low, high) = extent(&outward);
        assert!(close_to(point(low, high), point(-2.0, 12.0)));
        assert_eq!(extent(&reversed), (low, high));
    }

    #[test]
    fn a_tangent_join_inserts_nothing_and_still_meets_exactly() {
        // A line running into a quarter arc it is tangent to, then out again:
        // one rounded corner of a filleted outline.
        let chain = OffsetChain::new(
            vec![
                line((0.0, 0.0), (8.0, 0.0)),
                EvaluatedCurve2::CircularArc {
                    center: point(8.0, 2.0),
                    start: point(8.0, 0.0),
                    end: point(10.0, 2.0),
                    direction: CurveDirection::CounterClockwise,
                },
                line((10.0, 2.0), (10.0, 10.0)),
            ],
            false,
        );
        let offset =
            offset_chain(&chain, -1.0, &PrecisionPolicy::default()).expect("a tangent chain");
        assert_eq!(offset.len(), 3, "a tangent corner needs no join");
        assert_watertight(&offset, false);

        let EvaluatedCurve2::CircularArc { center, start, .. } = &offset[1].curve else {
            panic!("the arc stays an arc");
        };
        // Outward of a counter-clockwise arc is the larger radius, about the
        // same centre.
        assert!(close_to(*center, point(8.0, 2.0)));
        assert!(((*start - *center).length() - 3.0).abs() < 1.0e-12);
    }

    #[test]
    fn an_arc_offset_keeps_its_centre_and_changes_only_its_radius() {
        let precision = PrecisionPolicy::default();
        let arc = EvaluatedCurve2::CircularArc {
            center: point(0.0, 0.0),
            start: point(5.0, 0.0),
            end: point(0.0, 5.0),
            direction: CurveDirection::CounterClockwise,
        };
        let chain = OffsetChain::new(vec![arc], false);
        for (distance, expected) in [(1.0, 4.0), (-1.5, 6.5)] {
            let offset = offset_chain(&chain, distance, &precision).expect("an arc offsets");
            assert_eq!(offset.len(), 1);
            let EvaluatedCurve2::CircularArc { center, start, .. } = &offset[0].curve else {
                panic!("an offset arc is an arc");
            };
            assert!(close_to(*center, point(0.0, 0.0)));
            assert!(((*start - *center).length() - expected).abs() < 1.0e-12);
        }
    }

    #[test]
    fn a_circle_offset_inward_past_its_own_radius_is_refused_by_name() {
        let precision = PrecisionPolicy::default();
        let chain = OffsetChain::new(
            vec![EvaluatedCurve2::Circle {
                center: point(0.0, 0.0),
                radius: 3.0,
                direction: CurveDirection::CounterClockwise,
            }],
            true,
        );
        assert_eq!(
            offset_chain(&chain, 3.5, &precision),
            Err(OffsetError::CurveCollapses { index: 0 })
        );
        // Outward is always available, and a circle has no corner to join.
        let outward = offset_chain(&chain, -3.5, &precision).expect("a circle offsets outward");
        assert_eq!(outward.len(), 1);
        assert!(matches!(
            outward[0].curve,
            EvaluatedCurve2::Circle { radius, .. } if (radius - 6.5).abs() < 1.0e-12
        ));
    }

    #[test]
    fn a_spline_in_the_chain_is_refused_by_name_rather_than_approximated() {
        let chain = OffsetChain::new(
            vec![EvaluatedCurve2::Bspline {
                control_points: vec![point(0.0, 0.0), point(1.0, 2.0), point(3.0, 0.0)],
                degree: 2,
                knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                weights: None,
            }],
            false,
        );
        assert_eq!(
            offset_chain(&chain, 1.0, &PrecisionPolicy::default()),
            Err(OffsetError::UnsupportedSource { index: 0 })
        );
    }

    /// A step tall enough to survive the offset, taken on the inside: the
    /// first corner is concave and trims to its meeting, the second is convex
    /// and extends to its own. One chain, both directions, and no curve added
    /// to either.
    #[test]
    fn a_step_taken_on_the_inside_trims_one_corner_and_extends_the_other() {
        let chain = OffsetChain::new(
            vec![
                line((0.0, 0.0), (10.0, 0.0)),
                line((10.0, 0.0), (10.0, 3.0)),
                line((10.0, 3.0), (20.0, 3.0)),
            ],
            false,
        );
        let offset =
            offset_chain(&chain, 2.0, &PrecisionPolicy::default()).expect("a 3 mm step survives 2");
        assert_eq!(offset.len(), 3);
        assert_watertight(&offset, false);

        // The riser's offset was trimmed at one end and extended at the other,
        // and still runs the way it was drawn.
        assert_eq!(expect_line(&offset[0].curve).1, point(8.0, 2.0));
        assert_eq!(
            expect_line(&offset[1].curve),
            (point(8.0, 2.0), point(8.0, 5.0))
        );
        assert_eq!(expect_line(&offset[2].curve).0, point(8.0, 5.0));
    }

    #[test]
    fn an_inward_offset_that_eats_a_short_side_names_the_corner_it_lost() {
        // A 1 mm step cannot survive a 2 mm inward offset: the riser's own
        // offset would have to run backwards to reach the corner. Removing it
        // and rejoining its neighbours is the self-intersection pass this one
        // does not attempt, so it refuses, and says where.
        let chain = OffsetChain::new(
            vec![
                line((0.0, 0.0), (10.0, 0.0)),
                line((10.0, 0.0), (10.0, 1.0)),
                line((10.0, 1.0), (20.0, 1.0)),
            ],
            false,
        );
        assert_eq!(
            offset_chain(&chain, 2.0, &PrecisionPolicy::default()),
            Err(OffsetError::CornerCollapses { corner: 0 })
        );
    }

    #[test]
    fn a_broken_chain_and_a_zero_distance_are_both_refused_before_any_geometry() {
        let precision = PrecisionPolicy::default();
        let broken = OffsetChain::new(
            vec![line((0.0, 0.0), (1.0, 0.0)), line((5.0, 0.0), (6.0, 0.0))],
            false,
        );
        assert_eq!(
            offset_chain(&broken, 1.0, &precision),
            Err(OffsetError::ChainNotConnected { index: 1 })
        );
        assert_eq!(
            offset_chain(&square(), 0.0, &precision),
            Err(OffsetError::DistanceTooSmall)
        );
        assert_eq!(
            offset_chain(&OffsetChain::new(Vec::new(), false), 1.0, &precision),
            Err(OffsetError::EmptyChain)
        );
    }
}

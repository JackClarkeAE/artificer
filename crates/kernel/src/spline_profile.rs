//! Planar profiles with B-spline curves (ADR 0050), certified and extruded.
//!
//! A sketch spline arrives as a `PlanarCurve2::Bspline`: a degree, a knot
//! vector, control points and optional weights. It is admitted when it is a
//! curve the kernel carries — non-rational, clamped, of degree one to five —
//! and refused by name otherwise. A loop may mix splines with lines and arcs,
//! or be one spline that closes on itself, which is cut in two at the middle
//! of its domain the way a whole circle is cut into two semicircles, so that
//! every loop has at least two vertices and every wall two rungs.
//!
//! The certification is the one every profile gets — closed loops, outer
//! loops counter-clockwise and holes clockwise, holes inside and clear of
//! their outer loop, regions clear of each other, nothing below the feature
//! floor — with one difference in how it is proved where a spline takes
//! part. A spline has no closed-form intersection with anything, so two
//! pieces are tested by subdivision: each piece is Bézier segments, arcs of
//! at most a quarter turn or chords, every one inside a box and within a
//! known distance of its own chord, and a pair is split until either the
//! boxes are further apart than the floor, or both are flat enough that the
//! distance between their chords, less and plus their flatness, falls
//! wholly on one side of it. Nothing is sampled: every bound is the convex
//! hull's. A pair that never resolves within the subdivision limit is
//! refused rather than guessed.
//!
//! An extruded spline sweeps a B-spline wall of degree `p` by one, its two
//! rows the spline's control points on the bottom and top planes. The spline
//! is the bottom and top edge, and its ends' straight rungs are the wall's
//! other two; lines and arcs sweep planes and cylinders as they always have.

use artificer_protocol::{
    MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, Point2 as ProtocolPoint2, PrecisionPolicy,
};

use crate::analytic_extrusion::{
    BoundaryUse, Frame, Segment, adjacent_has_extra_contact, allocate_id, cap_pcurve,
    merge_topologies, normalize_frame, parse_curve, parse_loop, push_boundary_edge, push_cap_face,
    push_edge, push_loop, push_side_face, push_vertex, segment_clearance,
};
use crate::bspline::{SplineCurve2, SplineCurve3, SplineError, SplineSurface, array3};
use crate::extrusion::ExtrusionInputError;
use crate::planar_profile::PlanarProfileInputError;
use crate::topology::{
    Curve2, Curve3, Edge, EdgeKey, Face, FaceKey, FaceRole, Orientation, ParameterRange, Plane,
    Point2, Record, Shell, ShellKey, Solid, Surface, Topology, VertexKey,
};

/// Why a profile with splines was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SplineProfileError {
    /// Every refusal an ordinary profile can have.
    Profile(PlanarProfileInputError),
    /// A spline the kernel does not carry.
    Spline(SplineError),
    /// A spline that stalls — its rate vanishes, at a cusp or at an end whose
    /// first two control points coincide — or is no longer than the feature
    /// floor. A wall swept from it would have no side to face there.
    Degenerate,
    /// Two pieces the subdivision could not tell apart from touching within
    /// its limit: neither clearly clear nor clearly in contact.
    Indeterminate,
}

impl From<PlanarProfileInputError> for SplineProfileError {
    fn from(error: PlanarProfileInputError) -> Self {
        Self::Profile(error)
    }
}

fn extrusion_error(error: ExtrusionInputError) -> SplineProfileError {
    SplineProfileError::Profile(PlanarProfileInputError::Extrusion(error))
}

/// One boundary piece of a profile loop.
///
/// The spline variant is a handle and much smaller than a segment; the
/// pieces are few and copied freely while a loop is checked, so the segment
/// is kept inline rather than boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ProfilePiece {
    /// A line or circular arc, exactly as every other profile carries it.
    Segment(Segment),
    /// A B-spline walked over its whole domain, from its first control point
    /// to its last.
    Spline(SplineCurve2),
}

impl ProfilePiece {
    pub(crate) fn start(self) -> Point2 {
        match self {
            Self::Segment(segment) => segment.start(),
            Self::Spline(curve) => crate::bspline::point2(curve.first()),
        }
    }

    pub(crate) fn end(self) -> Point2 {
        match self {
            Self::Segment(segment) => segment.end(),
            Self::Spline(curve) => crate::bspline::point2(curve.last()),
        }
    }

    fn reversed(self) -> Self {
        match self {
            Self::Segment(segment) => Self::Segment(segment.reversed()),
            Self::Spline(curve) => Self::Spline(curve.reversed()),
        }
    }

    /// `½∮(x dy − y dx)` along the piece, measured from `anchor`: exact for
    /// every kind.
    fn area_contribution(self, anchor: Point2) -> f64 {
        match self {
            Self::Segment(segment) => segment.translated(anchor).signed_area_contribution(),
            Self::Spline(curve) => {
                let (start, end) = curve.domain();
                curve.contour(start, end, anchor)[0]
            }
        }
    }

    /// The piece as patches the subdivision tests start from.
    fn patches(self) -> Vec<Patch> {
        match self {
            Self::Segment(Segment::Line { start, end }) => vec![Patch::Line { start, end }],
            Self::Segment(Segment::Arc {
                center,
                start,
                end,
                radius,
                start_angle,
                sweep,
            }) => {
                // At most a quarter turn each, so a chord bounds its sagitta
                // and a box its arc.
                let pieces = ((sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize).max(1);
                let mut patches = Vec::with_capacity(pieces);
                let mut from_point = start;
                for piece in 0..pieces {
                    let from = sweep.mul_add(piece as f64 / pieces as f64, start_angle);
                    let to = if piece + 1 == pieces {
                        start_angle + sweep
                    } else {
                        sweep.mul_add((piece + 1) as f64 / pieces as f64, start_angle)
                    };
                    let to_point = if piece + 1 == pieces {
                        end
                    } else {
                        Point2::new(
                            radius.mul_add(to.cos(), center.x),
                            radius.mul_add(to.sin(), center.y),
                        )
                    };
                    patches.push(Patch::Arc {
                        center,
                        radius,
                        from,
                        to,
                        start: from_point,
                        end: to_point,
                    });
                    from_point = to_point;
                }
                patches
            }
            Self::Segment(_) => Vec::new(),
            Self::Spline(curve) => curve
                .bezier_segments()
                .1
                .into_iter()
                .map(|points| Patch::Bezier { points })
                .collect(),
        }
    }
}

/// One loop of a profile: its pieces, and its signed area, positive for a
/// loop walked counter-clockwise.
#[derive(Clone, Debug)]
pub(crate) struct SplineLoop {
    pub(crate) pieces: Vec<ProfilePiece>,
    pub(crate) signed_area: f64,
}

impl SplineLoop {
    fn reversed(&self) -> Self {
        Self {
            pieces: self
                .pieces
                .iter()
                .rev()
                .map(|piece| piece.reversed())
                .collect(),
            signed_area: -self.signed_area,
        }
    }
}

/// One region checked and ready to sweep.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedSplineRegion {
    pub(crate) frame: Frame,
    /// The outer loop, counter-clockwise, then the holes, clockwise.
    pub(crate) loops: Vec<SplineLoop>,
    pub(crate) distance: f64,
}

/// Whether any loop of the profile carries a B-spline.
pub(crate) fn profile_contains_splines(profile: &PlanarProfile2) -> bool {
    profile
        .regions
        .iter()
        .flat_map(|region| std::iter::once(&region.outer).chain(&region.holes))
        .flat_map(|profile_loop| &profile_loop.curves)
        .any(|curve| matches!(curve, PlanarCurve2::Bspline { .. }))
}

/// A sketch spline as a curve the kernel carries, or the reason it is not
/// one. Weights that are all equal describe the polynomial spline exactly —
/// they cancel from every rational expression — and are admitted as it;
/// weights that differ are a rational spline, which the kernel does not
/// carry.
pub(crate) fn spline_from_protocol(
    degree: usize,
    control_points: &[ProtocolPoint2],
    knots: &[f64],
    weights: Option<&[f64]>,
) -> Result<SplineCurve2, SplineError> {
    if let Some(weights) = weights {
        if weights.len() != control_points.len() {
            return Err(SplineError::Knots);
        }
        if weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight <= 0.0)
        {
            return Err(SplineError::NonFinite);
        }
        if weights.iter().any(|weight| *weight != weights[0]) {
            return Err(SplineError::Rational);
        }
    }
    SplineCurve2::new(
        degree,
        knots.to_vec(),
        control_points
            .iter()
            .map(|point| [point.x, point.y])
            .collect(),
    )
}

/// One loop of a profile, with its splines, as pieces: every piece checked
/// against the feature floor, every pair checked for contact, and the signed
/// area. A loop with no spline is parsed as any other profile's is.
pub(crate) fn parse_spline_loop(
    profile_loop: &PlanarLoop2,
    minimum: f64,
    agreement: f64,
) -> Result<SplineLoop, SplineProfileError> {
    if profile_loop.curves.is_empty() {
        return Err(PlanarProfileInputError::EmptyLoop.into());
    }
    if !profile_loop
        .curves
        .iter()
        .any(|curve| matches!(curve, PlanarCurve2::Bspline { .. }))
    {
        let parsed = parse_loop(profile_loop, minimum, agreement)?;
        return Ok(SplineLoop {
            pieces: parsed
                .segments
                .into_iter()
                .map(ProfilePiece::Segment)
                .collect(),
            signed_area: parsed.signed_area,
        });
    }
    let mut pieces = Vec::with_capacity(profile_loop.curves.len() + 1);
    for curve in &profile_loop.curves {
        pieces.push(match curve {
            PlanarCurve2::Bspline {
                degree,
                control_points,
                knots,
                weights,
            } => {
                let spline =
                    spline_from_protocol(*degree, control_points, knots, weights.as_deref())
                        .map_err(SplineProfileError::Spline)?;
                check_spline(spline, minimum)?;
                ProfilePiece::Spline(spline)
            }
            other => ProfilePiece::Segment(parse_curve(other, minimum, agreement)?),
        });
    }
    if (0..pieces.len())
        .any(|index| pieces[index].end() != pieces[(index + 1) % pieces.len()].start())
    {
        return Err(PlanarProfileInputError::DisconnectedLoop.into());
    }
    // A spline that closes on itself is cut in two at the middle of its
    // domain, as a whole circle is cut into two semicircles.
    if let [ProfilePiece::Spline(curve)] = pieces.as_slice() {
        let (start, end) = curve.domain();
        let (left, right) = curve
            .split(0.5 * (start + end))
            .ok_or(SplineProfileError::Degenerate)?;
        pieces = vec![ProfilePiece::Spline(left), ProfilePiece::Spline(right)];
    }
    if pieces.len() < 2 {
        return Err(PlanarProfileInputError::DisconnectedLoop.into());
    }
    check_loop_contacts(&pieces, minimum, agreement)?;
    let anchor = pieces[0].start();
    let signed_area = pieces
        .iter()
        .map(|piece| piece.area_contribution(anchor))
        .sum::<f64>();
    if !signed_area.is_finite() {
        return Err(extrusion_error(
            ExtrusionInputError::NumericallyIndeterminate,
        ));
    }
    Ok(SplineLoop {
        pieces,
        signed_area,
    })
}

/// A spline long enough to be a feature, whose rate vanishes nowhere.
fn check_spline(curve: SplineCurve2, minimum: f64) -> Result<(), SplineProfileError> {
    let (start, end) = curve.domain();
    let length = curve.length(start, end);
    if !length.is_finite() || length <= minimum {
        return Err(extrusion_error(ExtrusionInputError::FeatureTooSmall));
    }
    let mean_speed = length / (end - start);
    let least = curve.least_speed();
    if least.is_nan() || least <= 1.0e-6 * mean_speed {
        return Err(SplineProfileError::Degenerate);
    }
    Ok(())
}

/// Every pair of pieces of a loop, and every pair of Bézier segments within
/// one spline, clear of each other: neighbours may meet only at the vertex
/// they share, and pieces that are not neighbours must stay the feature floor
/// apart.
fn check_loop_contacts(
    pieces: &[ProfilePiece],
    minimum: f64,
    agreement: f64,
) -> Result<(), SplineProfileError> {
    let count = pieces.len();
    let patches = pieces
        .iter()
        .map(|piece| boxed(piece.patches()))
        .collect::<Vec<_>>();
    for (index, piece) in pieces.iter().enumerate() {
        if let ProfilePiece::Spline(_) = piece {
            let segments = &patches[index];
            for first in 0..segments.len() {
                for second in first + 1..segments.len() {
                    let shared = if second == first + 1 {
                        vec![segments[first].patch.end()]
                    } else {
                        Vec::new()
                    };
                    resolve(pair_clash(
                        &segments[first],
                        &segments[second],
                        agreement,
                        &shared,
                    ))?;
                }
            }
        }
    }
    for first in 0..count {
        for second in first + 1..count {
            let adjacent = second == first + 1 || (first == 0 && second + 1 == count);
            let mut shared = Vec::new();
            if adjacent {
                if second == first + 1 {
                    shared.push(pieces[first].end());
                }
                if first == 0 && second + 1 == count {
                    shared.push(pieces[first].start());
                }
            }
            if let (ProfilePiece::Segment(left), ProfilePiece::Segment(right)) =
                (pieces[first], pieces[second])
            {
                // Two exact segments are tested exactly, as every profile's are.
                let contact = if adjacent {
                    adjacent_has_extra_contact(left, right, &shared, agreement)
                } else {
                    segment_clearance(left, right) <= minimum
                };
                if contact {
                    return Err(extrusion_error(ExtrusionInputError::SelfIntersecting));
                }
                continue;
            }
            let threshold = if adjacent { agreement } else { minimum };
            for left in &patches[first] {
                for right in &patches[second] {
                    resolve(pair_clash(left, right, threshold, &shared))?;
                }
            }
        }
    }
    Ok(())
}

fn resolve(outcome: Clash) -> Result<(), SplineProfileError> {
    match outcome {
        Clash::Clear => Ok(()),
        Clash::Contact => Err(extrusion_error(ExtrusionInputError::SelfIntersecting)),
        Clash::Unknown => Err(SplineProfileError::Indeterminate),
    }
}

/// Two loops — of one region, or of two — clear of each other by more than
/// the feature floor.
fn loops_clear(
    first: &SplineLoop,
    second: &SplineLoop,
    minimum: f64,
) -> Result<bool, SplineProfileError> {
    let patches = |spline_loop: &SplineLoop| {
        boxed(
            spline_loop
                .pieces
                .iter()
                .flat_map(|piece| piece.patches())
                .collect(),
        )
    };
    let (lefts, rights) = (patches(first), patches(second));
    for left in &lefts {
        for right in &rights {
            match pair_clash(left, right, minimum, &[]) {
                Clash::Clear => {}
                Clash::Contact => return Ok(false),
                Clash::Unknown => return Err(SplineProfileError::Indeterminate),
            }
        }
    }
    Ok(true)
}

/// Whether `point` lies inside a loop, by the crossings of a ray with the
/// loop walked as chords within `flatness` of it. A point further than
/// `flatness` from the loop — every point this is asked about is further
/// than the feature floor, which the clearance checks have already proved —
/// is inside the chords exactly when it is inside the loop.
fn point_inside(point: Point2, profile_loop: &SplineLoop, flatness: f64) -> bool {
    let mut polygon = Vec::new();
    for piece in &profile_loop.pieces {
        for patch in piece.patches() {
            flatten(&patch, flatness, &mut polygon, 0);
        }
    }
    let mut inside = false;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        if (a.y > point.y) != (b.y > point.y) {
            let x = (b.x - a.x).mul_add((point.y - a.y) / (b.y - a.y), a.x);
            if point.x < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// The patch's start and every chord end after it, each chord within
/// `flatness` of the patch.
fn flatten(patch: &Patch, flatness: f64, polygon: &mut Vec<Point2>, depth: usize) {
    if patch.flatness() <= flatness || depth >= 32 {
        polygon.push(patch.start());
        return;
    }
    let (left, right) = patch.halves();
    flatten(&left, flatness, polygon, depth + 1);
    flatten(&right, flatness, polygon, depth + 1);
}

// ---------------------------------------------------------------------------
// Certified contact by subdivision
// ---------------------------------------------------------------------------

/// How many pair tests the question about one pair of patches may spend
/// before it gives up and refuses: far more than any two curves that are
/// either clearly apart or clearly touching need.
const CLASH_BUDGET: usize = 1 << 18;

/// A patch and the box it lies in, found once for the many pairs it is
/// asked about.
struct Boxed {
    patch: Patch,
    bounds: (Point2, Point2),
}

fn boxed(patches: Vec<Patch>) -> Vec<Boxed> {
    patches
        .into_iter()
        .map(|patch| Boxed {
            bounds: patch.bounds(),
            patch,
        })
        .collect()
}

/// [`clash`] for one pair of patches, with a budget of its own.
///
/// A loop of a thousand Bézier segments has half a million pairs of them,
/// nearly all far apart. Those whose boxes are further apart than the
/// threshold are clear at once, as the subdivision's first step would find
/// them, and cost nothing; each pair left is its own question, which the
/// number of other pairs in the loop makes no harder. Shared out of one
/// budget, the pairs of a long spline would spend it on each other's first
/// steps, and a loop that is plainly clear would be refused as undecided.
fn pair_clash(first: &Boxed, second: &Boxed, threshold: f64, shared: &[Point2]) -> Clash {
    if box_gap(first.bounds, second.bounds) > threshold {
        return Clash::Clear;
    }
    let mut budget = CLASH_BUDGET;
    clash(&first.patch, &second.patch, threshold, shared, &mut budget)
}

/// A piece of a piece, inside a box and within [`Patch::flatness`] of its
/// own chord.
#[derive(Clone, Debug)]
enum Patch {
    Line {
        start: Point2,
        end: Point2,
    },
    /// At most half a turn.
    Arc {
        center: Point2,
        radius: f64,
        from: f64,
        to: f64,
        start: Point2,
        end: Point2,
    },
    /// One Bézier segment, degree one less than its point count.
    Bezier {
        points: Vec<[f64; 2]>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Clash {
    Clear,
    Contact,
    Unknown,
}

impl Patch {
    fn start(&self) -> Point2 {
        match self {
            Self::Line { start, .. } | Self::Arc { start, .. } => *start,
            Self::Bezier { points } => Point2::new(points[0][0], points[0][1]),
        }
    }

    fn end(&self) -> Point2 {
        match self {
            Self::Line { end, .. } | Self::Arc { end, .. } => *end,
            Self::Bezier { points } => {
                let last = points[points.len() - 1];
                Point2::new(last[0], last[1])
            }
        }
    }

    /// A box the patch lies inside.
    fn bounds(&self) -> (Point2, Point2) {
        let mut low = Point2::new(f64::INFINITY, f64::INFINITY);
        let mut high = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut grow = |point: Point2| {
            low = Point2::new(low.x.min(point.x), low.y.min(point.y));
            high = Point2::new(high.x.max(point.x), high.y.max(point.y));
        };
        match self {
            Self::Line { start, end } => {
                grow(*start);
                grow(*end);
            }
            Self::Arc {
                center,
                radius,
                from,
                to,
                start,
                end,
            } => {
                grow(*start);
                grow(*end);
                // The circle's extremes along each axis where the arc
                // reaches them.
                let (low_angle, high_angle) = (from.min(*to), from.max(*to));
                let first = (low_angle / std::f64::consts::FRAC_PI_2).floor() as i64;
                let last = (high_angle / std::f64::consts::FRAC_PI_2).ceil() as i64;
                for quarter in first..=last {
                    let angle = quarter as f64 * std::f64::consts::FRAC_PI_2;
                    if angle > low_angle && angle < high_angle {
                        let (sin, cos) = match quarter.rem_euclid(4) {
                            0 => (0.0, 1.0),
                            1 => (1.0, 0.0),
                            2 => (0.0, -1.0),
                            _ => (-1.0, 0.0),
                        };
                        grow(Point2::new(
                            radius.mul_add(cos, center.x),
                            radius.mul_add(sin, center.y),
                        ));
                    }
                }
                // A little room for the rounding of the extremes.
                let slack = 4.0 * f64::EPSILON * (radius + center.x.abs() + center.y.abs());
                low = Point2::new(low.x - slack, low.y - slack);
                high = Point2::new(high.x + slack, high.y + slack);
            }
            Self::Bezier { points } => {
                for point in points {
                    grow(Point2::new(point[0], point[1]));
                }
            }
        }
        (low, high)
    }

    /// How far any point of the patch can be from its chord — and so, since
    /// the patch runs from one end of the chord to the other, how far any
    /// point of the chord can be from the patch.
    fn flatness(&self) -> f64 {
        match self {
            Self::Line { .. } => 0.0,
            Self::Arc {
                radius, from, to, ..
            } => radius * (1.0 - (0.5 * (to - from).abs()).min(std::f64::consts::PI).cos()),
            Self::Bezier { points } => {
                let (start, end) = (self.start(), self.end());
                points[1..points.len() - 1]
                    .iter()
                    .map(|point| {
                        point_segment_distance(Point2::new(point[0], point[1]), start, end)
                    })
                    .fold(0.0, f64::max)
            }
        }
    }

    /// The two halves, each keeping the end it shares with the whole to the
    /// bit.
    fn halves(&self) -> (Self, Self) {
        match self {
            Self::Line { start, end } => {
                let middle = Point2::new(0.5 * (start.x + end.x), 0.5 * (start.y + end.y));
                (
                    Self::Line {
                        start: *start,
                        end: middle,
                    },
                    Self::Line {
                        start: middle,
                        end: *end,
                    },
                )
            }
            Self::Arc {
                center,
                radius,
                from,
                to,
                start,
                end,
            } => {
                let middle_angle = 0.5 * (from + to);
                let middle = Point2::new(
                    radius.mul_add(middle_angle.cos(), center.x),
                    radius.mul_add(middle_angle.sin(), center.y),
                );
                (
                    Self::Arc {
                        center: *center,
                        radius: *radius,
                        from: *from,
                        to: middle_angle,
                        start: *start,
                        end: middle,
                    },
                    Self::Arc {
                        center: *center,
                        radius: *radius,
                        from: middle_angle,
                        to: *to,
                        start: middle,
                        end: *end,
                    },
                )
            }
            Self::Bezier { points } => {
                // de Casteljau at one half.
                let mut layer = points.clone();
                let mut left = vec![layer[0]];
                let mut right = vec![layer[layer.len() - 1]];
                while layer.len() > 1 {
                    layer = layer
                        .windows(2)
                        .map(|pair| {
                            [
                                0.5 * (pair[0][0] + pair[1][0]),
                                0.5 * (pair[0][1] + pair[1][1]),
                            ]
                        })
                        .collect();
                    left.push(layer[0]);
                    right.push(layer[layer.len() - 1]);
                }
                right.reverse();
                (
                    Self::Bezier { points: left },
                    Self::Bezier { points: right },
                )
            }
        }
    }
}

/// Whether two patches come within `threshold` of each other anywhere but
/// at a point of `shared` both end on.
///
/// Boxes further apart than the threshold are clear. Two patches flat
/// within a sixty-fourth of it are decided by their chords: each patch lies
/// within its flatness of its chord and its chord within its flatness of it,
/// so the patches' distance is the chords' give or take both flatnesses, and
/// a pair whose band falls wholly on one side of the threshold is decided.
/// Two patches that end on one shared point meet only there once their chords
/// leave it at an angle `α` with `(f₁ + f₂) ≤ threshold·sin(α/2)`: nearer the
/// point than that they are within the threshold of it, which is what
/// meeting there means. Anything else is halved and asked again, until the
/// budget runs out.
fn clash(
    first: &Patch,
    second: &Patch,
    threshold: f64,
    shared: &[Point2],
    budget: &mut usize,
) -> Clash {
    if *budget == 0 {
        return Clash::Unknown;
    }
    *budget -= 1;
    let meets = |point: &Point2| {
        (first.start() == *point || first.end() == *point)
            && (second.start() == *point || second.end() == *point)
    };
    let meeting = shared
        .iter()
        .filter(|point| meets(point))
        .collect::<Vec<_>>();
    let (first_flat, second_flat) = (first.flatness(), second.flatness());
    let fine = threshold / 64.0;
    if meeting.is_empty() {
        if box_gap(first.bounds(), second.bounds()) > threshold {
            return Clash::Clear;
        }
        if first_flat <= fine && second_flat <= fine {
            let gap = segment_distance(first.start(), first.end(), second.start(), second.end());
            if gap - first_flat - second_flat > threshold {
                return Clash::Clear;
            }
            if gap + first_flat + second_flat <= threshold {
                return Clash::Contact;
            }
        }
    } else if meeting.len() == 1 {
        let point = *meeting[0];
        let away = |patch: &Patch| {
            let other = if patch.start() == point {
                patch.end()
            } else {
                patch.start()
            };
            let (x, y) = (other.x - point.x, other.y - point.y);
            let length = x.hypot(y);
            (length > 0.0).then(|| (x / length, y / length))
        };
        if let (Some(first_away), Some(second_away)) = (away(first), away(second)) {
            let cosine = first_away
                .0
                .mul_add(second_away.0, first_away.1 * second_away.1);
            let half_sine = (0.5 * (1.0 - cosine)).max(0.0).sqrt();
            if cosine >= 1.0 - 1.0e-12 && first_flat <= fine && second_flat <= fine {
                // The two leave the point along one chord: they overlap.
                return Clash::Contact;
            }
            if first_flat + second_flat <= threshold * half_sine {
                return Clash::Clear;
            }
        }
    }
    // Halve whichever patch is further from flat, or the longer when both
    // are flat, and ask of both halves.
    let length = |patch: &Patch| {
        let (start, end) = (patch.start(), patch.end());
        (end.x - start.x).hypot(end.y - start.y)
    };
    let split_first = if (first_flat - second_flat).abs() > fine {
        first_flat > second_flat
    } else {
        length(first) >= length(second)
    };
    let (halves, other) = if split_first {
        (first.halves(), second)
    } else {
        (second.halves(), first)
    };
    let mut outcome = Clash::Clear;
    for half in [halves.0, halves.1] {
        let result = if split_first {
            clash(&half, other, threshold, shared, budget)
        } else {
            clash(other, &half, threshold, shared, budget)
        };
        match result {
            Clash::Contact => return Clash::Contact,
            Clash::Unknown => outcome = Clash::Unknown,
            Clash::Clear => {}
        }
    }
    outcome
}

fn box_gap(first: (Point2, Point2), second: (Point2, Point2)) -> f64 {
    let gap_x = (second.0.x - first.1.x)
        .max(first.0.x - second.1.x)
        .max(0.0);
    let gap_y = (second.0.y - first.1.y)
        .max(first.0.y - second.1.y)
        .max(0.0);
    gap_x.hypot(gap_y)
}

fn point_segment_distance(point: Point2, start: Point2, end: Point2) -> f64 {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length_squared = dx.mul_add(dx, dy * dy);
    let t = if length_squared > 0.0 {
        ((point.x - start.x).mul_add(dx, (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (point.x - dx.mul_add(t, start.x)).hypot(point.y - dy.mul_add(t, start.y))
}

/// The distance between two segments in the plane.
fn segment_distance(a0: Point2, a1: Point2, b0: Point2, b1: Point2) -> f64 {
    let cross = |o: Point2, p: Point2, q: Point2| {
        (p.x - o.x).mul_add(q.y - o.y, -((p.y - o.y) * (q.x - o.x)))
    };
    let (d1, d2) = (cross(a0, a1, b0), cross(a0, a1, b1));
    let (d3, d4) = (cross(b0, b1, a0), cross(b0, b1, a1));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return 0.0;
    }
    point_segment_distance(a0, b0, b1)
        .min(point_segment_distance(a1, b0, b1))
        .min(point_segment_distance(b0, a0, a1))
        .min(point_segment_distance(b1, a0, a1))
}

// ---------------------------------------------------------------------------
// The whole profile
// ---------------------------------------------------------------------------

/// A profile with splines, certified for a sweep of `distance` along its
/// frame's normal: each region's outer loop counter-clockwise and its holes
/// clockwise, inside it and clear of it and of each other, and the regions
/// clear of one another.
pub(crate) fn validate_spline_profile_extrusion(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Vec<ValidatedSplineRegion>, SplineProfileError> {
    if profile.regions.is_empty() {
        return Err(PlanarProfileInputError::EmptyProfile.into());
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return Err(PlanarProfileInputError::TooManyRegions.into());
    }
    if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
        return Err(PlanarProfileInputError::TooManyLoops.into());
    }
    if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
        return Err(PlanarProfileInputError::TooManyCurves.into());
    }
    if !frame.is_finite()
        || !distance.is_finite()
        || profile
            .regions
            .iter()
            .flat_map(|region| std::iter::once(&region.outer).chain(&region.holes))
            .flat_map(|profile_loop| &profile_loop.curves)
            .any(|curve| !curve.is_finite())
    {
        return Err(extrusion_error(ExtrusionInputError::NonFinite));
    }
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if distance <= 0.0 {
        return Err(extrusion_error(ExtrusionInputError::NonPositiveDistance));
    }
    if distance <= minimum {
        return Err(extrusion_error(ExtrusionInputError::FeatureTooSmall));
    }
    let frame = normalize_frame(frame, precision)?;
    let agreement = precision.linear_agreement;
    let mut regions = Vec::with_capacity(profile.regions.len());
    for region in &profile.regions {
        let mut outer = parse_spline_loop(&region.outer, minimum, agreement)?;
        if outer.signed_area.abs() <= minimum * minimum {
            return Err(extrusion_error(ExtrusionInputError::AreaTooSmall));
        }
        if outer.signed_area < 0.0 {
            outer = outer.reversed();
        }
        let mut loops = vec![outer];
        for hole in &region.holes {
            let mut parsed = parse_spline_loop(hole, minimum, agreement)?;
            if parsed.signed_area.abs() <= minimum * minimum {
                return Err(extrusion_error(ExtrusionInputError::AreaTooSmall));
            }
            if parsed.signed_area > 0.0 {
                parsed = parsed.reversed();
            }
            loops.push(parsed);
        }
        let flatness = minimum / 8.0;
        for (index, hole) in loops.iter().enumerate().skip(1) {
            if !point_inside(hole.pieces[0].start(), &loops[0], flatness)
                || !loops_clear(&loops[0], hole, minimum)?
            {
                return Err(PlanarProfileInputError::OverlappingRegions.into());
            }
            for other in &loops[1..index] {
                if !loops_clear(hole, other, minimum)?
                    || point_inside(hole.pieces[0].start(), other, flatness)
                    || point_inside(other.pieces[0].start(), hole, flatness)
                {
                    return Err(PlanarProfileInputError::OverlappingRegions.into());
                }
            }
        }
        let net_area = loops
            .iter()
            .map(|profile_loop| profile_loop.signed_area)
            .sum::<f64>();
        if !net_area.is_finite() || net_area <= minimum * minimum {
            return Err(extrusion_error(ExtrusionInputError::AreaTooSmall));
        }
        regions.push(ValidatedSplineRegion {
            frame,
            loops,
            distance,
        });
    }
    let flatness = minimum / 8.0;
    let in_material = |point: Point2, loops: &[SplineLoop]| {
        point_inside(point, &loops[0], flatness)
            && loops[1..]
                .iter()
                .all(|hole| !point_inside(point, hole, flatness))
    };
    for left in 0..regions.len() {
        for right in left + 1..regions.len() {
            for first in &regions[left].loops {
                for second in &regions[right].loops {
                    if !loops_clear(first, second, minimum)? {
                        return Err(PlanarProfileInputError::OverlappingRegions.into());
                    }
                }
            }
            if in_material(
                regions[left].loops[0].pieces[0].start(),
                &regions[right].loops,
            ) || in_material(
                regions[right].loops[0].pieces[0].start(),
                &regions[left].loops,
            ) {
                return Err(PlanarProfileInputError::OverlappingRegions.into());
            }
        }
    }
    // Every coordinate the sweep will write, control points included, inside
    // the configured envelope.
    let limit = precision.max_abs_coordinate;
    for region in &regions {
        for piece in region
            .loops
            .iter()
            .flat_map(|profile_loop| &profile_loop.pieces)
        {
            let points: Vec<Point2> = match piece {
                ProfilePiece::Segment(Segment::Arc { center, radius, .. }) => vec![
                    Point2::new(center.x - radius, center.y - radius),
                    Point2::new(center.x + radius, center.y + radius),
                ],
                ProfilePiece::Segment(segment) => vec![segment.start(), segment.end()],
                ProfilePiece::Spline(curve) => curve
                    .points()
                    .iter()
                    .map(|point| crate::bspline::point2(*point))
                    .collect(),
            };
            for point in points {
                for height in [0.0, distance] {
                    let world = frame.point(point, height);
                    if [world.x, world.y, world.z]
                        .iter()
                        .any(|value| !value.is_finite() || value.abs() > limit)
                    {
                        return Err(extrusion_error(ExtrusionInputError::CoordinateLimit));
                    }
                }
            }
        }
    }
    Ok(regions)
}

/// The profile swept into solids, one per region.
pub(crate) fn build_spline_extrusion(regions: &[ValidatedSplineRegion]) -> Topology {
    merge_topologies(regions.iter().map(build_spline_region).collect())
}

/// A spline piece of the profile as a space curve at `height` along the
/// frame's normal: its control points carried through the frame, which is
/// how its vertices are, so the two agree to the bit.
fn lifted(curve: SplineCurve2, frame: Frame, height: f64) -> Option<SplineCurve3> {
    curve.mapped(|point| array3(frame.point(crate::bspline::point2(point), height)))
}

fn build_spline_region(region: &ValidatedSplineRegion) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let frame = region.frame;
    let distance = region.distance;
    struct Keys {
        bottom_edges: Vec<EdgeKey>,
        top_edges: Vec<EdgeKey>,
        vertical_edges: Vec<EdgeKey>,
        /// The swept curves of each spline piece, bottom and top.
        lifts: Vec<Option<(SplineCurve3, SplineCurve3)>>,
    }
    let mut loop_keys = Vec::with_capacity(region.loops.len());
    for profile_loop in &region.loops {
        let count = profile_loop.pieces.len();
        let bottom_vertices = profile_loop
            .pieces
            .iter()
            .map(|piece| push_vertex(&mut topology, &mut next_id, frame.point(piece.start(), 0.0)))
            .collect::<Vec<VertexKey>>();
        let top_vertices = profile_loop
            .pieces
            .iter()
            .map(|piece| {
                push_vertex(
                    &mut topology,
                    &mut next_id,
                    frame.point(piece.start(), distance),
                )
            })
            .collect::<Vec<VertexKey>>();
        let lifts = profile_loop
            .pieces
            .iter()
            .map(|piece| match piece {
                ProfilePiece::Spline(curve) => Some((
                    lifted(*curve, frame, 0.0)?,
                    lifted(*curve, frame, distance)?,
                )),
                ProfilePiece::Segment(_) => None,
            })
            .collect::<Vec<_>>();
        let mut edges_at = |vertices: &[VertexKey], height: f64, top: bool| {
            profile_loop
                .pieces
                .iter()
                .enumerate()
                .map(|(index, piece)| {
                    let ends = [vertices[index], vertices[(index + 1) % count]];
                    match (piece, lifts[index]) {
                        (ProfilePiece::Spline(curve), Some((bottom, upper))) => {
                            let (start, end) = curve.domain();
                            push_edge(
                                &mut topology,
                                &mut next_id,
                                Edge {
                                    vertices: ends,
                                    curve: Curve3::Bspline {
                                        curve: if top { upper } else { bottom },
                                    },
                                    parameter_range: ParameterRange::new(start, end),
                                },
                            )
                        }
                        (ProfilePiece::Segment(segment), _) => push_boundary_edge(
                            &mut topology,
                            &mut next_id,
                            ends,
                            *segment,
                            frame,
                            height,
                        ),
                        // A spline that did not lift was refused as
                        // non-finite before it got here; the chord keeps the
                        // loop closed and the validator names the fault.
                        (ProfilePiece::Spline(_), None) => push_edge(
                            &mut topology,
                            &mut next_id,
                            Edge::line(
                                ends,
                                [piece.start(), piece.end()]
                                    .map(|point| frame.point(point, height)),
                            ),
                        ),
                    }
                })
                .collect::<Vec<_>>()
        };
        let bottom_edges = edges_at(&bottom_vertices, 0.0, false);
        let top_edges = edges_at(&top_vertices, distance, true);
        let vertical_edges = (0..count)
            .map(|index| {
                let start = topology.vertices[bottom_vertices[index].0].value.point;
                let end = topology.vertices[top_vertices[index].0].value.point;
                push_edge(
                    &mut topology,
                    &mut next_id,
                    Edge::line([bottom_vertices[index], top_vertices[index]], [start, end]),
                )
            })
            .collect::<Vec<_>>();
        loop_keys.push(Keys {
            bottom_edges,
            top_edges,
            vertical_edges,
            lifts,
        });
    }

    // The caps: the bottom faces against the sweep, in the frame's axes
    // swapped, and walks its loops backwards; the top faces along it.
    let bottom_plane = Plane::new(frame.origin, frame.v, frame.u);
    let bottom_loops = region
        .loops
        .iter()
        .zip(&loop_keys)
        .map(|(profile_loop, keys)| {
            let uses = (0..profile_loop.pieces.len())
                .rev()
                .map(|index| BoundaryUse {
                    edge: keys.bottom_edges[index],
                    orientation: Orientation::Reverse,
                    curve: piece_pcurve(profile_loop.pieces[index], true),
                })
                .collect::<Vec<_>>();
            push_loop(&mut topology, &mut next_id, uses)
        })
        .collect::<Vec<_>>();
    push_cap_face(
        &mut topology,
        &mut next_id,
        Surface::Plane(bottom_plane),
        &bottom_loops,
        FaceRole::ExtrusionBottom,
    );
    let top_loops = region
        .loops
        .iter()
        .zip(&loop_keys)
        .map(|(profile_loop, keys)| {
            let uses = profile_loop
                .pieces
                .iter()
                .enumerate()
                .map(|(index, piece)| BoundaryUse {
                    edge: keys.top_edges[index],
                    orientation: Orientation::Forward,
                    curve: piece_pcurve(*piece, false),
                })
                .collect::<Vec<_>>();
            push_loop(&mut topology, &mut next_id, uses)
        })
        .collect::<Vec<_>>();
    push_cap_face(
        &mut topology,
        &mut next_id,
        Surface::Plane(Plane::new(
            frame.origin + frame.normal * distance,
            frame.u,
            frame.v,
        )),
        &top_loops,
        FaceRole::ExtrusionTop,
    );

    let mut side_ordinal = 0_u32;
    for (profile_loop, keys) in region.loops.iter().zip(&loop_keys) {
        let count = profile_loop.pieces.len();
        for (index, piece) in profile_loop.pieces.iter().enumerate() {
            let next = (index + 1) % count;
            let edges = [
                keys.bottom_edges[index],
                keys.vertical_edges[next],
                keys.top_edges[index],
                keys.vertical_edges[index],
            ];
            let role = FaceRole::ExtrusionSide(side_ordinal);
            side_ordinal += 1;
            match (piece, keys.lifts[index]) {
                (ProfilePiece::Segment(segment), _) => push_side_face(
                    &mut topology,
                    &mut next_id,
                    (frame, distance),
                    *segment,
                    edges,
                    role,
                ),
                (ProfilePiece::Spline(curve), Some((bottom, top))) => {
                    push_spline_wall(
                        &mut topology,
                        &mut next_id,
                        *curve,
                        bottom,
                        top,
                        edges,
                        role,
                    );
                }
                (ProfilePiece::Spline(_), None) => {}
            }
        }
    }

    let shell_key = ShellKey(topology.shells.len());
    topology.shells.push(Record {
        id: allocate_id(&mut next_id),
        value: Shell {
            faces: (0..topology.faces.len()).map(FaceKey).collect(),
        },
    });
    topology.solids.push(Record {
        id: allocate_id(&mut next_id),
        value: Solid {
            outer_shell: shell_key,
            inner_shells: Vec::new(),
        },
    });
    topology
}

/// A profile piece as a curve in a cap's own coordinates: the frame's for
/// the top cap, and the frame's with its axes swapped, walked backwards, for
/// the bottom.
fn piece_pcurve(piece: ProfilePiece, bottom: bool) -> (Curve2, ParameterRange) {
    match piece {
        ProfilePiece::Segment(segment) => cap_pcurve(segment, bottom, bottom),
        ProfilePiece::Spline(curve) => {
            let (start, end) = curve.domain();
            let pcurve = if bottom {
                curve.mapped(|point| [point[1], point[0]]).unwrap_or(curve)
            } else {
                curve
            };
            (
                Curve2::Bspline { curve: pcurve },
                if bottom {
                    ParameterRange::new(end, start)
                } else {
                    ParameterRange::new(start, end)
                },
            )
        }
    }
}

/// The wall a spline sweeps: the B-spline surface of degree `p` by one whose
/// rows are the bottom and top edges, walked along the loop and up the
/// sweep, which is the direction that faces out of the material.
fn push_spline_wall(
    topology: &mut Topology,
    next_id: &mut u64,
    curve: SplineCurve2,
    bottom: SplineCurve3,
    top: SplineCurve3,
    edges: [EdgeKey; 4],
    role: FaceRole,
) {
    let Some(surface) = SplineSurface::ruled(bottom, top) else {
        return;
    };
    let (start, end) = curve.domain();
    let corners = [
        Point2::new(start, 0.0),
        Point2::new(end, 0.0),
        Point2::new(end, 1.0),
        Point2::new(start, 1.0),
    ];
    let loop_key = push_loop(
        topology,
        next_id,
        [
            (edges[0], Orientation::Forward, [corners[0], corners[1]]),
            (edges[1], Orientation::Forward, [corners[1], corners[2]]),
            (edges[2], Orientation::Reverse, [corners[2], corners[3]]),
            (edges[3], Orientation::Reverse, [corners[3], corners[0]]),
        ]
        .into_iter()
        .map(|(edge, orientation, points)| BoundaryUse {
            edge,
            orientation,
            curve: Curve2::line_segment(points),
        })
        .collect(),
    );
    topology.faces.push(Record {
        id: allocate_id(next_id),
        value: Face {
            surface: Surface::Bspline(surface),
            outer_loop: loop_key,
            inner_loops: Vec::new(),
            role,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed cubic of `count` control points spaced evenly round a circle
    /// of radius ten, its first and last the same point.
    fn ring(count: usize) -> PlanarLoop2 {
        let mut points = (0..count - 1)
            .map(|index| {
                let angle = std::f64::consts::TAU * index as f64 / (count - 1) as f64;
                ProtocolPoint2::new(10.0 * angle.cos(), 10.0 * angle.sin())
            })
            .collect::<Vec<_>>();
        points.push(points[0]);
        PlanarLoop2 {
            curves: vec![PlanarCurve2::Bspline {
                degree: 3,
                knots: crate::bspline::clamped_uniform_knots(count, 3),
                control_points: points,
                weights: None,
            }],
        }
    }

    /// Eight hundred control points make some three hundred thousand pairs
    /// of Bézier segments, all but a handful far apart. Shared out of one
    /// budget, their first steps alone spent it and the loop was refused as
    /// undecided; judged pair by pair, with the far ones passed over by
    /// their boxes, it is clear.
    #[test]
    fn a_long_closed_spline_is_judged_pair_by_pair() {
        let policy = PrecisionPolicy::default();
        let minimum = policy.modeling_resolution.max(policy.min_feature_size);
        let parsed = parse_spline_loop(&ring(800), minimum, policy.linear_agreement)
            .expect("a long ring is clear of itself");
        let area = 100.0 * std::f64::consts::PI;
        assert!(
            (parsed.signed_area - area).abs() < 1.0e-3 * area,
            "{}",
            parsed.signed_area
        );
    }
}

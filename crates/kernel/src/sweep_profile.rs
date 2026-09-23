//! Sweeps of a certified planar profile along a path (ADR 0055, S1 and S2).
//!
//! The profile is carried along the path rigidly, from where it lies at the
//! path's start. By default it turns with the path by the path's
//! rotation-minimising frame, carried from sample to sample by the
//! double-reflection method (Wang, Jüttler, Zheng and Liu, 2008); with a
//! fixed orientation it only moves. The solid is what the profile sweeps
//! through, and three routes build it:
//!
//! - A straight path is an extrusion along it: the profile lofted to its copy
//!   at the far end, with planes, cylinders and ruled walls, exact.
//! - One circular arc, followed by its rotation-minimising frame about an
//!   axis that lies in the profile's plane, is a partial revolve, exact.
//! - Anything else is skinned through copies of the profile placed along the
//!   path, by the smooth loft (ADR 0050). Its walls pass through every copy
//!   exactly and depart from the true sweep between them, so the copies are
//!   doubled until that departure — measured from the true sweep, at points
//!   between every two copies — is within the precision's approximation
//!   budget. The result states the departure it met.

use std::f64::consts::TAU;

use artificer_protocol::{
    LoftSection, PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarProfile2, Point2 as ProtocolPoint2,
    Point3 as ProtocolPoint3, PrecisionPolicy, RevolveAngle, SweepOrientation, SweepPath3,
    SweepSegment3, Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::{Frame, normalize_frame};
use crate::bspline::{SplineCurve2, SplineCurve3, SplineSurface};
use crate::loft_sections::{self, LoftSectionsError};
use crate::loft_skin;
use crate::planar_profile::PlanarProfileInputError;
use crate::revolve;
use crate::topology::{Point3, Surface, Topology, Vector3};

/// Fine steps between two consecutive profile copies, over which the frame
/// is carried and the departure from the true sweep is measured.
const FRAME_STEPS: usize = 16;

/// The most copies of the profile a skinned sweep may be built from.
const MAX_SECTIONS: usize = 257;

/// The most copies a skinned sweep starts from: half the most it may use, so
/// that refining where the skin misses its budget always has room to work.
/// A path of many spline spans asks for more; they are spread over it
/// instead, and the refinement places more where the skin needs them.
const INITIAL_SECTIONS: usize = MAX_SECTIONS.div_ceil(2);

/// How closely two path tangents must agree, in radians, for a junction to
/// be smooth rather than a corner.
const TANGENT_AGREEMENT: f64 = 1.0e-6;

/// The least angle, in radians, the path's start may make with the
/// profile's plane: about five degrees. Nearer than that, the profile is
/// swept almost along itself.
const MIN_START_ANGLE: f64 = 0.087;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SweepInputError {
    /// The profile or its frame is not certified.
    Profile(PlanarProfileInputError),
    /// The path has no segments.
    PathEmpty,
    /// A segment is not finite, has no length, or is not a sound arc or
    /// spline.
    PathInvalid { segment: usize },
    /// A segment does not begin where the one before it ends.
    PathGap { segment: usize },
    /// A segment leaves in a different direction from the one before it
    /// arrived.
    PathCorner { segment: usize },
    /// The path ends where it begins.
    PathClosed,
    /// The path starts along the profile's own plane.
    ProfileAlongPath,
    /// The profile reaches past the path's centre of curvature somewhere, so
    /// the wall would fold there.
    ProfileTooWide,
    /// The path comes back within the profile's reach of a stretch of itself
    /// it had left, so the swept profile would pass through itself there.
    SelfIntersecting,
    /// The copies along the path could not be lofted.
    Loft(LoftSectionsError),
    /// Even the most copies did not bring the skin within the budget.
    ToleranceUnmet { deviation: f64, tolerance: f64 },
}

impl From<PlanarProfileInputError> for SweepInputError {
    fn from(reason: PlanarProfileInputError) -> Self {
        Self::Profile(reason)
    }
}

/// A built sweep, the route that built it, and how far it departs from the
/// true sweep when it is not exact.
#[derive(Debug)]
pub(crate) struct Swept {
    pub(crate) topology: Topology,
    pub(crate) rung: &'static str,
    pub(crate) approximation: Option<Approximation>,
}

/// What a skinned sweep met: the worst departure measured, the budget it was
/// held to, and how many copies of the profile it took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Approximation {
    pub(crate) deviation: f64,
    pub(crate) tolerance: f64,
    pub(crate) sections: usize,
}

/// Certifies and builds a sweep.
/// What a skinned sweep is brought within.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkinBudget {
    /// The precision's approximation budget: the sweep is the body.
    Precision,
    /// The chord tolerance the faceted Boolean tier samples a B-spline of
    /// the sweep's size at: the sweep is a tool that tier will combine, and
    /// copies placed more closely than that would only be tessellated away.
    /// Worse, they would be harmful: a join packed with copies is tessellated
    /// into facets so short that those either side of it are all but
    /// coplanar, and the tier's splitting shreds them into slivers.
    FacetedTool,
}

pub(crate) fn sweep(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    path: &SweepPath3,
    orientation: SweepOrientation,
    precision: PrecisionPolicy,
    budget: SkinBudget,
) -> Result<Swept, SweepInputError> {
    let placed = normalize_frame(frame, precision)?;
    let pieces = parse_path(path, precision)?;
    let start = pieces[0].jet(0.0);
    let start_tangent = unit(start.1).ok_or(SweepInputError::PathInvalid { segment: 0 })?;
    if start_tangent.dot(placed.normal).abs() < MIN_START_ANGLE.sin() {
        return Err(SweepInputError::ProfileAlongPath);
    }
    let samples = profile_samples(profile)
        .into_iter()
        .map(|point| placed.origin + placed.u * point[0] + placed.v * point[1])
        .collect::<Vec<_>>();

    // A straight path: the profile lofted to its copy at the far end.
    if let [Piece::Line { start, end }] = pieces.as_slice() {
        let along = *end - *start;
        let sections = [
            frame,
            moved(frame, placed, |point| point + along, |vector| vector),
        ]
        .map(|frame| LoftSection {
            frame,
            profile: profile.clone(),
        });
        let loft = loft_sections::validate_loft_sections(&sections, precision)
            .map_err(SweepInputError::Loft)?;
        return Ok(Swept {
            topology: loft_sections::build_loft_sections(&loft),
            rung: "sweep/straight",
            approximation: None,
        });
    }

    // One arc turned with its own frame: a revolve about the arc's axis, when
    // that axis lies in the profile's plane.
    if orientation == SweepOrientation::RotationMinimising
        && let [
            Piece::Arc {
                center,
                normal,
                sweep,
                ..
            },
        ] = pieces.as_slice()
        && let Some(axis) = axis_in_frame(*center, *normal, placed, precision)
        && let Ok(revolved) = revolve::validate_revolve(
            frame,
            profile,
            axis,
            RevolveAngle::partial(0.0, *sweep),
            precision,
        )
    {
        return Ok(Swept {
            topology: revolve::build_revolve(&revolved),
            rung: "sweep/revolve",
            approximation: None,
        });
    }

    let tolerance = match budget {
        SkinBudget::Precision => precision
            .approximation_budget
            .max(precision.modeling_resolution),
        SkinBudget::FacetedTool => crate::faceted_spline_tolerance(
            pieces.iter().map(|piece| piece.length()).sum::<f64>(),
            precision,
        ),
    };
    skinned(
        frame,
        placed,
        profile,
        &pieces,
        orientation,
        &samples,
        precision,
        tolerance,
    )
}

/// The skinned route: copies of the profile along the path, placed more
/// closely wherever the skin is not yet within the budget.
///
/// A skin through the copies is smooth across them, so where the path's
/// curvature jumps — a line running into an arc — it cannot follow at once,
/// and its departure there shrinks only with the square of the spacing.
/// Refining just the spans that miss the budget grades the copies in towards
/// such joins instead of multiplying them everywhere.
#[allow(clippy::too_many_arguments)]
fn skinned(
    frame: PlanarFrame3,
    placed: Frame,
    profile: &PlanarProfile2,
    pieces: &[Piece],
    orientation: SweepOrientation,
    samples: &[Point3],
    precision: PrecisionPolicy,
    tolerance: f64,
) -> Result<Swept, SweepInputError> {
    let cut_profile = circles_halved(profile);
    let hull = hull_points(profile)
        .into_iter()
        .map(|point| placed.origin + placed.u * point[0] + placed.v * point[1])
        .collect::<Vec<_>>();
    let mut positions = first_positions(pieces);
    // Every join needs a copy of its own, so a path of more pieces than
    // copies allowed cannot be skinned at all. The wire format and the model
    // hold a path to fewer segments than that; this is the kernel's own
    // guard.
    if positions.len() > MAX_SECTIONS {
        return Err(SweepInputError::ToleranceUnmet {
            deviation: f64::INFINITY,
            tolerance,
        });
    }
    loop {
        let stations = stations(pieces, &positions);
        let frames = carry_frames(&stations, placed, orientation)
            .ok_or(SweepInputError::PathInvalid { segment: 0 })?;
        if orientation == SweepOrientation::RotationMinimising {
            check_width(&stations, &frames, samples, placed.normal)?;
        }
        check_clear_of_itself(&stations, &frames, &hull, placed.normal)?;
        let origin = stations[0].point;
        let sections = (0..positions.len())
            .map(|index| {
                let station = index * FRAME_STEPS;
                let turn = frames[station];
                LoftSection {
                    frame: moved(
                        frame,
                        placed,
                        |point| stations[station].point + turn.apply(point - origin),
                        |vector| turn.apply(vector),
                    ),
                    profile: cut_profile.clone(),
                }
            })
            .collect::<Vec<_>>();
        let loft = match loft_skin::validate_skinned_loft(&sections, precision) {
            Ok(loft) => loft,
            // A skin that folds between copies may be only too coarse: halve
            // every span and try again, while there is room.
            Err(LoftSectionsError::SkinFolds) if positions.len() * 2 - 1 <= MAX_SECTIONS => {
                positions = halved(&positions);
                continue;
            }
            Err(reason) => return Err(SweepInputError::Loft(reason)),
        };
        let topology = loft_skin::build_skinned_loft(&loft);
        let departures = departures(&topology, &stations, &frames, samples);
        let deviation = departures.iter().copied().fold(0.0, f64::max);
        if deviation <= tolerance {
            return Ok(Swept {
                topology,
                rung: "sweep/skinned",
                approximation: Some(Approximation {
                    deviation,
                    tolerance,
                    sections: positions.len(),
                }),
            });
        }
        // Where the path's curvature jumps — at a join between pieces — the
        // skin's departure falls only with the square of the spacing, and
        // rings away from the join, so a span touching one is cut towards
        // it, each part half the one before. Elsewhere the departure falls
        // with the fourth power, so a span is cut evenly and more sparingly.
        let last_piece = pieces.len() as f64;
        let is_join =
            |position: f64| position.fract() == 0.0 && position > 0.0 && position < last_piece;
        let mut next = Vec::with_capacity(positions.len() * 2);
        for (span, departure) in departures.iter().enumerate() {
            let (from, to) = (positions[span], positions[span + 1]);
            next.push(from);
            if *departure <= tolerance {
                continue;
            }
            let miss = departure / tolerance;
            if is_join(from) && is_join(to) {
                // Joins at both ends: halve it, and the next round grades
                // each half towards its own join.
                next.push(f64::midpoint(from, to));
            } else if is_join(from) || is_join(to) {
                let count = (miss.sqrt().log2().ceil() as usize).clamp(1, 12);
                // Halving towards the join: from the far end, cuts at
                // 1/2, 3/4, 7/8 … of the way.
                let toward_to = is_join(to) && !is_join(from);
                for part in 1..=count {
                    let fraction = 1.0 - 0.5_f64.powi(part as i32);
                    next.push(if toward_to {
                        (to - from).mul_add(fraction, from)
                    } else {
                        (from - to).mul_add(fraction, to)
                    });
                }
                if !toward_to {
                    // Cut towards `from`, the points came out descending.
                    let added = next.len() - count;
                    next[added..].reverse();
                }
            } else {
                let count = (miss.powf(0.25).ceil() as usize).clamp(2, 8);
                next.extend(
                    (1..count).map(|part| (to - from).mul_add(part as f64 / count as f64, from)),
                );
            }
        }
        if next.len() + 1 > MAX_SECTIONS {
            return Err(SweepInputError::ToleranceUnmet {
                deviation,
                tolerance,
            });
        }
        next.push(positions[positions.len() - 1]);
        positions = next;
    }
}

/// Where the first copies stand along the path: piece index plus the
/// fraction of that piece, so a copy always stands at every join.
///
/// The copies start about evenly spaced along the whole path — a smooth
/// skin through copies far apart beside copies close together overshoots
/// and folds — and at least as closely as each piece's own shape asks, but
/// never more than [`INITIAL_SECTIONS`] of them while the joins allow.
fn first_positions(pieces: &[Piece]) -> Vec<f64> {
    let spacing = pieces.iter().map(|piece| piece.length()).sum::<f64>() / 12.0;
    let least = if pieces.len() == 1 { 2 } else { 1 };
    let wanted = pieces
        .iter()
        .map(|piece| {
            let even = (piece.length() / spacing).ceil() as usize;
            piece.spans().max(even).max(least)
        })
        .collect::<Vec<_>>();
    let mut positions = Vec::new();
    for (index, spans) in spread(&wanted, INITIAL_SECTIONS - 1, least)
        .into_iter()
        .enumerate()
    {
        let first = usize::from(index > 0);
        positions.extend((first..=spans).map(|step| index as f64 + step as f64 / spans as f64));
    }
    positions
}

/// The spans each piece starts with: as many as it wants while together they
/// come to no more than `most`, and otherwise each piece's share of `most`
/// in proportion to what it wanted above `least`, never fewer than `least`.
///
/// A spline asks for two copies to each of its knot spans, and one of a
/// thousand control points would otherwise start from two thousand copies —
/// past the most a sweep may use before its first skin, each one a row of
/// every wall's net and a stretch of every check along it.
fn spread(wanted: &[usize], most: usize, least: usize) -> Vec<usize> {
    if wanted.iter().sum::<usize>() <= most {
        return wanted.to_vec();
    }
    let spare = most.saturating_sub(least * wanted.len());
    let above = wanted
        .iter()
        .map(|count| count.saturating_sub(least))
        .sum::<usize>()
        .max(1);
    wanted
        .iter()
        .map(|count| least + count.saturating_sub(least) * spare / above)
        .collect()
}

/// Every span cut in two.
fn halved(positions: &[f64]) -> Vec<f64> {
    let mut halved = Vec::with_capacity(positions.len() * 2);
    for pair in positions.windows(2) {
        halved.push(pair[0]);
        halved.push(f64::midpoint(pair[0], pair[1]));
    }
    halved.push(positions[positions.len() - 1]);
    halved
}

/// One piece of the path, parameterized over the unit interval.
#[derive(Clone, Copy, Debug)]
enum Piece {
    Line {
        start: Point3,
        end: Point3,
    },
    /// `center + radius·(cos(sweep·t)·u + sin(sweep·t)·v)`, turning
    /// right-handed about `normal`, with `sweep` positive.
    Arc {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
        sweep: f64,
        normal: Vector3,
    },
    Spline {
        curve: SplineCurve3,
        from: f64,
        to: f64,
    },
}

impl Piece {
    /// The point, first and second derivatives at `t` in `[0, 1]`.
    fn jet(self, t: f64) -> (Point3, Vector3, Vector3) {
        match self {
            Self::Line { start, end } => (
                start + (end - start) * t,
                end - start,
                Vector3::new(0.0, 0.0, 0.0),
            ),
            Self::Arc {
                center,
                u,
                v,
                radius,
                sweep,
                ..
            } => {
                let (sin, cos) = (sweep * t).sin_cos();
                (
                    center + (u * cos + v * sin) * radius,
                    (u * -sin + v * cos) * (radius * sweep),
                    (u * cos + v * sin) * (-radius * sweep * sweep),
                )
            }
            Self::Spline { curve, from, to } => {
                let span = to - from;
                let [point, first, second] = curve.derivatives(span.mul_add(t, from));
                (
                    Point3::new(point[0], point[1], point[2]),
                    Vector3::new(first[0], first[1], first[2]) * span,
                    Vector3::new(second[0], second[1], second[2]) * (span * span),
                )
            }
        }
    }

    /// The piece's length: exact for a line or an arc, the length of a fine
    /// polyline through a spline.
    fn length(self) -> f64 {
        match self {
            Self::Line { start, end } => (end - start).length(),
            Self::Arc { radius, sweep, .. } => radius * sweep,
            Self::Spline { .. } => (1..=64)
                .map(|step| {
                    let (from, to) = (
                        self.jet(f64::from(step - 1) / 64.0).0,
                        self.jet(f64::from(step) / 64.0).0,
                    );
                    (to - from).length()
                })
                .sum(),
        }
    }

    /// How many spans between profile copies the piece starts with.
    fn spans(self) -> usize {
        match self {
            Self::Line { .. } => 1,
            // One copy every eighth of a half turn.
            Self::Arc { sweep, .. } => {
                ((sweep / (std::f64::consts::PI / 8.0)).ceil() as usize).max(1)
            }
            Self::Spline { curve, from, to } => (curve.spans(from, to).len() * 2).max(4),
        }
    }
}

/// Reads the path and certifies it: every segment sound, each starting where
/// the one before ended and leaving the way it arrived, and the whole open.
fn parse_path(
    path: &SweepPath3,
    precision: PrecisionPolicy,
) -> Result<Vec<Piece>, SweepInputError> {
    if path.segments.is_empty() {
        return Err(SweepInputError::PathEmpty);
    }
    let scale = path
        .segments
        .iter()
        .flat_map(|segment| match segment {
            SweepSegment3::Line { start, end } => vec![*start, *end],
            SweepSegment3::Arc { center, start, .. } => vec![*center, *start],
            SweepSegment3::Spline { points, .. } => points.clone(),
        })
        .map(|point| point.x.abs().max(point.y.abs()).max(point.z.abs()))
        .fold(1.0_f64, f64::max);
    let meet = precision.linear_agreement.max(1.0e-12) * scale;
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let mut pieces = Vec::with_capacity(path.segments.len());
    for (index, segment) in path.segments.iter().enumerate() {
        let invalid = SweepInputError::PathInvalid { segment: index };
        let piece = match segment {
            SweepSegment3::Line { start, end } => {
                let (start, end) = (point(*start), point(*end));
                if !(start.is_finite() && end.is_finite()) || (end - start).length() <= minimum {
                    return Err(invalid);
                }
                Piece::Line { start, end }
            }
            SweepSegment3::Arc {
                center,
                start,
                normal,
                sweep,
            } => {
                let (center, start) = (point(*center), point(*start));
                let normal = vector(*normal);
                if !(center.is_finite() && start.is_finite() && sweep.is_finite()) {
                    return Err(invalid);
                }
                let mut normal = unit(normal).ok_or(invalid)?;
                let mut sweep = *sweep;
                if sweep.abs() >= TAU {
                    return Err(SweepInputError::PathClosed);
                }
                if sweep.abs() <= precision.angular_agreement_radians.max(1.0e-9) {
                    return Err(invalid);
                }
                if sweep < 0.0 {
                    normal = normal * -1.0;
                    sweep = -sweep;
                }
                let reach = start - center;
                let radius = reach.length();
                if radius <= minimum || reach.dot(normal).abs() > meet.max(radius * 1.0e-12) {
                    return Err(invalid);
                }
                let u = reach / radius;
                Piece::Arc {
                    center,
                    u,
                    v: normal.cross(u),
                    radius,
                    sweep,
                    normal,
                }
            }
            SweepSegment3::Spline {
                degree,
                knots,
                points,
            } => {
                let degree = usize::try_from(*degree).map_err(|_| invalid)?;
                if degree == 0 || points.len() <= degree {
                    return Err(invalid);
                }
                let curve = SplineCurve3::new(
                    degree,
                    knots.clone(),
                    points
                        .iter()
                        .map(|point| [point.x, point.y, point.z])
                        .collect(),
                )
                .map_err(|_| invalid)?;
                let (from, to) = curve.domain();
                if to.partial_cmp(&from) != Some(std::cmp::Ordering::Greater) {
                    return Err(invalid);
                }
                // A spline no longer than the feature floor, or one whose
                // rate vanishes somewhere — a cusp, or an end whose first
                // two control points coincide — has no tangent there for a
                // frame to follow, and is refused as the segment it is
                // rather than by whatever the frames that fail on it upset.
                let length = curve.length(from, to);
                if !length.is_finite() || length <= minimum {
                    return Err(invalid);
                }
                let least = curve.least_speed();
                if least.is_nan() || least <= 1.0e-6 * length / (to - from) {
                    return Err(invalid);
                }
                Piece::Spline { curve, from, to }
            }
        };
        if let Some(previous) = pieces.last().copied() {
            let (arrived, arriving, _) = Piece::jet(previous, 1.0);
            let (leaves, leaving, _) = piece.jet(0.0);
            if (leaves - arrived).length() > meet {
                return Err(SweepInputError::PathGap { segment: index });
            }
            let (Some(arriving), Some(leaving)) = (unit(arriving), unit(leaving)) else {
                return Err(invalid);
            };
            if arriving.cross(leaving).length() > TANGENT_AGREEMENT || arriving.dot(leaving) < 0.0 {
                return Err(SweepInputError::PathCorner { segment: index });
            }
        }
        pieces.push(piece);
    }
    let first = pieces[0].jet(0.0).0;
    let last = pieces[pieces.len() - 1].jet(1.0).0;
    if (last - first).length() <= meet {
        return Err(SweepInputError::PathClosed);
    }
    // Lines that carry straight on are one line.
    let mut merged: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        if let (Some(Piece::Line { end, .. }), Piece::Line { end: further, .. }) =
            (merged.last_mut(), piece)
        {
            *end = further;
        } else {
            merged.push(piece);
        }
    }
    Ok(merged)
}

/// A sample along the path: where it is, its unit tangent, and its first
/// and second derivatives.
#[derive(Clone, Copy, Debug)]
struct Station {
    point: Point3,
    tangent: Vector3,
    first: Vector3,
    second: Vector3,
}

/// The path sampled at `FRAME_STEPS` stations between every two copies, the
/// copies' own positions among them. A position is a piece's index plus the
/// fraction of the way along it.
fn stations(pieces: &[Piece], positions: &[f64]) -> Vec<Station> {
    let at = |position: f64| {
        let index = (position.floor() as usize).min(pieces.len() - 1);
        let (point, first, second) = pieces[index].jet((position - index as f64).clamp(0.0, 1.0));
        Station {
            point,
            tangent: unit(first).unwrap_or(Vector3::new(0.0, 0.0, 0.0)),
            first,
            second,
        }
    };
    let mut stations = Vec::with_capacity(positions.len().saturating_sub(1) * FRAME_STEPS + 1);
    stations.push(at(positions[0]));
    // Every join is a copy's position, so no span crosses one.
    for pair in positions.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        for step in 1..=FRAME_STEPS {
            let fraction = step as f64 / FRAME_STEPS as f64;
            stations.push(at(if step == FRAME_STEPS {
                to
            } else {
                (to - from).mul_add(fraction, from)
            }));
        }
    }
    stations
}

/// A rotation, as the images of the path's starting frame.
#[derive(Clone, Copy, Debug)]
struct Turn {
    start: [Vector3; 3],
    now: [Vector3; 3],
}

impl Turn {
    fn apply(self, vector: Vector3) -> Vector3 {
        self.now[0] * self.start[0].dot(vector)
            + self.now[1] * self.start[1].dot(vector)
            + self.now[2] * self.start[2].dot(vector)
    }
}

/// The frame at every station: carried by double reflection, or held as it
/// started for a fixed orientation.
fn carry_frames(
    stations: &[Station],
    placed: Frame,
    orientation: SweepOrientation,
) -> Option<Vec<Turn>> {
    let tangent = stations[0].tangent;
    // The first normal: the profile's own `u`, or `v`, square to the path.
    let normal = [placed.u, placed.v]
        .into_iter()
        .filter_map(|axis| unit(axis - tangent * axis.dot(tangent)))
        .find(|candidate| candidate.length() > 0.5)?;
    let start = [tangent, normal, tangent.cross(normal)];
    let mut turns = Vec::with_capacity(stations.len());
    let mut reference = normal;
    for (index, station) in stations.iter().enumerate() {
        if index > 0 && orientation == SweepOrientation::RotationMinimising {
            reference = reflected(stations[index - 1], reference, *station)?;
        }
        let now = match orientation {
            SweepOrientation::RotationMinimising => {
                [station.tangent, reference, station.tangent.cross(reference)]
            }
            SweepOrientation::Fixed => start,
        };
        turns.push(Turn { start, now });
    }
    Some(turns)
}

/// The double reflection: the reference vector carried from one station to
/// the next by reflecting in the plane bisecting the two points, then in the
/// plane bisecting the reflected and the new tangent.
fn reflected(from: Station, reference: Vector3, to: Station) -> Option<Vector3> {
    let chord = to.point - from.point;
    let squared = chord.dot(chord);
    let (reference, tangent) = if squared > 1.0e-24 {
        (
            reference - chord * (2.0 / squared * chord.dot(reference)),
            from.tangent - chord * (2.0 / squared * chord.dot(from.tangent)),
        )
    } else {
        (reference, from.tangent)
    };
    let turn = to.tangent - tangent;
    let squared = turn.dot(turn);
    let reference = if squared > 1.0e-24 {
        reference - turn * (2.0 / squared * turn.dot(reference))
    } else {
        reference
    };
    unit(reference - to.tangent * reference.dot(to.tangent))
}

/// Refuses a profile that reaches past the path's centre of curvature
/// anywhere: the wall would fold back through itself there.
///
/// Turned by the rotation-minimising frame, the profile turns about the
/// binormal `B` at the path's curvature `κ`, so a point of it at `y` from the
/// path moves at `T + κ·B × y`. The profile sweeps forward there while that
/// motion has a part along the profile's normal `n`, faced along the path:
/// while `κ·y·(B × n) < n·T`. Where it has none the copies stop advancing
/// and the wall folds. For a profile square to the path, `n = T` and
/// `B × T = N`, and this is the familiar `κ·(y·N) < 1`, the profile inside
/// the centre of curvature; a profile leaning on the path folds sooner on
/// the side it leans back from. The frame keeps `n·T` as it started.
fn check_width(
    stations: &[Station],
    frames: &[Turn],
    samples: &[Point3],
    normal: Vector3,
) -> Result<(), SweepInputError> {
    let origin = stations[0].point;
    let facing = if normal.dot(stations[0].tangent) < 0.0 {
        normal * -1.0
    } else {
        normal
    };
    for (station, turn) in stations.iter().zip(frames) {
        let speed = station.first.length();
        if speed <= 1.0e-12 {
            continue;
        }
        let bend = station.second - station.tangent * station.second.dot(station.tangent);
        let curvature = bend.length() / (speed * speed);
        let Some(inward) = unit(bend).filter(|_| curvature > 1.0e-12) else {
            continue;
        };
        let facing_here = turn.apply(facing);
        let ahead = facing_here.dot(station.tangent);
        let across = station.tangent.cross(inward).cross(facing_here);
        for sample in samples {
            let reach = turn.apply(*sample - origin).dot(across);
            if reach * curvature >= ahead * (1.0 - 1.0e-9) {
                return Err(SweepInputError::ProfileTooWide);
            }
        }
    }
    Ok(())
}

/// Refuses a sweep that would pass through itself: the path coming back,
/// further on, to within the profile's reach of a stretch it had left.
///
/// Every copy of the profile lies inside the ball of radius `reach` about
/// where its frame carries the centre of the profile's hull, so two copies
/// can meet only where the line those centres trace comes within `2·reach`
/// of itself. Copies near each other along it are the width check's to
/// judge; this is for copies further apart along it than half a turn about
/// the profile's reach, `π·reach`, which no path bent as gently as the width
/// check allows brings together without coming back on itself. Stretches
/// that do come that close are then judged by what they hold: the hulls of
/// the profile at either end of each, widened by half the furthest a hull
/// point moves between them, and the stretches are clear only when some
/// direction — the one between them, either profile's normal, or one square
/// to two of those — has the two sets of points wholly apart along it. A
/// path that turns back through the solid it has already swept, or comes so
/// near it that neither can be shown apart, is refused.
fn check_clear_of_itself(
    stations: &[Station],
    frames: &[Turn],
    hull: &[Point3],
    normal: Vector3,
) -> Result<(), SweepInputError> {
    let Some(first) = hull.first() else {
        return Ok(());
    };
    let (low, high) = hull.iter().fold((*first, *first), |(low, high), point| {
        (
            Point3::new(low.x.min(point.x), low.y.min(point.y), low.z.min(point.z)),
            Point3::new(
                high.x.max(point.x),
                high.y.max(point.y),
                high.z.max(point.z),
            ),
        )
    });
    let middle = low + (high - low) * 0.5;
    let reach = hull
        .iter()
        .map(|point| point.distance(middle))
        .fold(0.0, f64::max);
    if reach.is_nan() || reach <= 0.0 {
        return Ok(());
    }
    let origin = stations[0].point;
    let carried = |station: usize, point: Point3| {
        stations[station].point + frames[station].apply(point - origin)
    };
    let centres = (0..stations.len())
        .map(|station| carried(station, middle))
        .collect::<Vec<_>>();
    let mut walked = vec![0.0; centres.len()];
    for index in 1..centres.len() {
        walked[index] = walked[index - 1] + centres[index].distance(centres[index - 1]);
    }
    let (apart, near) = (std::f64::consts::PI * reach, 2.0 * reach);

    // Whether the swept stretches from station `i` to the next and from `j`
    // to the next can be shown apart.
    let hulls = |from: usize| -> Vec<Point3> {
        hull.iter()
            .flat_map(|point| [carried(from, *point), carried(from + 1, *point)])
            .collect()
    };
    let separated = |i: usize, j: usize| {
        let (first, second) = (hulls(i), hulls(j));
        let spread = |points: &[Point3]| {
            points
                .chunks(2)
                .map(|pair| pair[0].distance(pair[1]))
                .fold(0.0, f64::max)
        };
        let margin = 0.5 * (spread(&first) + spread(&second));
        let between = (centres[j] - centres[i]) + (centres[j + 1] - centres[i + 1]);
        let (facing_i, facing_j) = (frames[i].apply(normal), frames[j].apply(normal));
        [
            between,
            facing_i,
            facing_j,
            facing_i.cross(facing_j),
            between.cross(facing_i),
            between.cross(facing_j),
        ]
        .into_iter()
        .filter_map(unit)
        .any(|axis| {
            let extent = |points: &[Point3]| {
                points
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), point| {
                        let along = point.as_vector().dot(axis);
                        (low.min(along), high.max(along))
                    })
            };
            let ((first_low, first_high), (second_low, second_high)) =
                (extent(&first), extent(&second));
            second_low - first_high > margin || first_low - second_high > margin
        })
    };

    // The stretches in runs of `FRAME_STEPS`, one run to a span between
    // copies, each run boxed so that runs far apart are passed over whole.
    let segments = centres.len() - 1;
    let runs = (0..segments)
        .step_by(FRAME_STEPS)
        .map(|start| {
            let end = (start + FRAME_STEPS).min(segments);
            let (low, high) = centres[start..=end].iter().fold(
                (centres[start], centres[start]),
                |(low, high), point| {
                    (
                        Point3::new(low.x.min(point.x), low.y.min(point.y), low.z.min(point.z)),
                        Point3::new(
                            high.x.max(point.x),
                            high.y.max(point.y),
                            high.z.max(point.z),
                        ),
                    )
                },
            );
            (start, end, low, high)
        })
        .collect::<Vec<_>>();
    for (index, &(start, end, low, high)) in runs.iter().enumerate() {
        for &(other_start, other_end, other_low, other_high) in &runs[index..] {
            let gap = Vector3::new(
                (other_low.x - high.x).max(low.x - other_high.x).max(0.0),
                (other_low.y - high.y).max(low.y - other_high.y).max(0.0),
                (other_low.z - high.z).max(low.z - other_high.z).max(0.0),
            );
            if gap.length() > near || walked[other_end] - walked[start] <= apart {
                continue;
            }
            for i in start..end {
                for j in other_start.max(i + 1)..other_end {
                    if walked[j] - walked[i + 1] <= apart
                        || loft_sections::segment_distance(
                            (centres[i], centres[i + 1]),
                            (centres[j], centres[j + 1]),
                        ) > near
                    {
                        continue;
                    }
                    if !separated(i, j) {
                        return Err(SweepInputError::SelfIntersecting);
                    }
                }
            }
        }
    }
    Ok(())
}

/// How far the skin departs from the true sweep between every two copies:
/// the worst distance, over the profile's sample points, from where the true
/// sweep carries each point a quarter, a half and three quarters of the way
/// between the copies to the nearest point of the skin.
///
/// Each sample is found on the skin once, by a search; after that each
/// station starts from where the one before it landed, so the rest are a few
/// Newton steps each. A walk that ends on the edge of its wall may have
/// crossed a seam — a sample on a rung, carried a little to one side of it —
/// and is then asked of every other wall too, from the edge the seam would
/// bring it in at: without that, a sample that drifted onto the next wall
/// would be measured from the edge of the one it left, further the further it
/// went.
fn departures(
    topology: &Topology,
    stations: &[Station],
    frames: &[Turn],
    samples: &[Point3],
) -> Vec<f64> {
    let walls = topology
        .faces
        .iter()
        .filter_map(|face| match face.value.surface {
            Surface::Bspline(surface) => Some(surface),
            _ => None,
        })
        .collect::<Vec<SplineSurface>>();
    let origin = stations[0].point;
    let spans = (stations.len() - 1) / FRAME_STEPS;
    let mut worst = vec![0.0_f64; spans];
    for sample in samples {
        let mut seed: Option<(usize, crate::topology::Point2)> = None;
        for station in 0..stations.len() {
            let within = station % FRAME_STEPS;
            let measured = within != 0 && within.is_multiple_of(FRAME_STEPS / 4);
            let truth = stations[station].point + frames[station].apply(*sample - origin);
            // Every station keeps the walk going; only the quarter points
            // between copies are measured.
            let found = seed
                .and_then(|(wall, parameters)| {
                    walls[wall]
                        .invert(truth, Some(parameters))
                        .map(|parameters| (wall, parameters))
                })
                .or_else(|| {
                    walls
                        .iter()
                        .enumerate()
                        .filter_map(|(wall, surface)| {
                            surface.invert(truth, None).map(|parameters| {
                                (
                                    (surface.evaluate(parameters) - truth).length(),
                                    wall,
                                    parameters,
                                )
                            })
                        })
                        .min_by(|left, right| left.0.total_cmp(&right.0))
                        .map(|(_, wall, parameters)| (wall, parameters))
                })
                .map(|(wall, parameters)| across_seam(&walls, wall, parameters, truth));
            let distance = found.map_or(f64::INFINITY, |(wall, parameters)| {
                (walls[wall].evaluate(parameters) - truth).length()
            });
            seed = found;
            if measured {
                let span = station / FRAME_STEPS;
                worst[span] = worst[span].max(distance);
            }
        }
    }
    worst
}

/// The nearest point to `truth` on wall `wall` at `parameters`, or on
/// another wall when that point is on the wall's edge and another is nearer:
/// each other wall is walked from its opposite edge at the same height,
/// where a seam would bring the point onto it.
fn across_seam(
    walls: &[SplineSurface],
    wall: usize,
    parameters: crate::topology::Point2,
    truth: Point3,
) -> (usize, crate::topology::Point2) {
    let (u_min, u_max, _, _) = walls[wall].domain();
    if parameters.x > u_min && parameters.x < u_max {
        return (wall, parameters);
    }
    let distance = |wall: usize, parameters| (walls[wall].evaluate(parameters) - truth).length();
    let mut best = (distance(wall, parameters), wall, parameters);
    for (other, surface) in walls.iter().enumerate().filter(|(other, _)| *other != wall) {
        let (low, high, _, _) = surface.domain();
        let edge = if parameters.x <= u_min { high } else { low };
        let seed = crate::topology::Point2::new(edge, parameters.y);
        if let Some(found) = surface.invert(truth, Some(seed)) {
            let candidate = distance(other, found);
            if candidate < best.0 {
                best = (candidate, other, found);
            }
        }
    }
    (best.1, best.2)
}

/// The profile with every whole circle written as its two halves, cut where
/// the profile's own `u` axis leaves the centre and at the opposite point.
///
/// A loft through circles alone has no vertex to start them from and must
/// choose where to cut each one. A sweep need not choose: every copy is the
/// profile carried rigidly by its frame, so a cut made in the profile is
/// carried with it, and the rungs through the cuts are the paths the cut
/// points really sweep. They neither twist nor depend on the rounding of a
/// direction that a turning copy stands almost edge-on to.
fn circles_halved(profile: &PlanarProfile2) -> PlanarProfile2 {
    let mut halved = profile.clone();
    for curves in halved.regions.iter_mut().flat_map(|region| {
        std::iter::once(&mut region.outer)
            .chain(&mut region.holes)
            .map(|profile_loop| &mut profile_loop.curves)
    }) {
        *curves = curves
            .iter()
            .flat_map(|curve| match *curve {
                PlanarCurve2::Circle {
                    center,
                    radius,
                    direction,
                } => {
                    let east = ProtocolPoint2::new(center.x + radius, center.y);
                    let west = ProtocolPoint2::new(center.x - radius, center.y);
                    vec![
                        PlanarCurve2::CircularArc {
                            center,
                            start: east,
                            end: west,
                            direction,
                        },
                        PlanarCurve2::CircularArc {
                            center,
                            start: west,
                            end: east,
                            direction,
                        },
                    ]
                }
                ref other => vec![other.clone()],
            })
            .collect();
    }
    halved
}

/// Points on every curve of the profile, in its own coordinates.
fn profile_samples(profile: &PlanarProfile2) -> Vec<[f64; 2]> {
    let mut samples = Vec::new();
    for region in &profile.regions {
        for curve in std::iter::once(&region.outer)
            .chain(&region.holes)
            .flat_map(|profile_loop| &profile_loop.curves)
        {
            match curve {
                PlanarCurve2::Line { start, end } => {
                    for t in [0.0, 0.25, 0.5, 0.75] {
                        samples.push([
                            (end.x - start.x).mul_add(t, start.x),
                            (end.y - start.y).mul_add(t, start.y),
                        ]);
                    }
                }
                PlanarCurve2::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => {
                    let (radius, from, sweep) = arc_angles(*center, *start, *end, *direction);
                    for step in 0..8 {
                        let angle = sweep.mul_add(f64::from(step) / 8.0, from);
                        samples.push([
                            radius.mul_add(angle.cos(), center.x),
                            radius.mul_add(angle.sin(), center.y),
                        ]);
                    }
                }
                PlanarCurve2::Circle { center, radius, .. } => {
                    for step in 0..16 {
                        let angle = TAU * f64::from(step) / 16.0;
                        samples.push([
                            radius.mul_add(angle.cos(), center.x),
                            radius.mul_add(angle.sin(), center.y),
                        ]);
                    }
                }
                PlanarCurve2::Bspline {
                    degree,
                    control_points,
                    knots,
                    weights,
                } => {
                    let curve = weights.is_none().then(|| {
                        SplineCurve2::new(
                            *degree,
                            knots.clone(),
                            control_points
                                .iter()
                                .map(|point| [point.x, point.y])
                                .collect(),
                        )
                        .ok()
                    });
                    match curve.flatten() {
                        Some(curve) => {
                            let (from, to) = curve.domain();
                            for step in 0..12 {
                                let at = curve
                                    .evaluate((to - from).mul_add(f64::from(step) / 12.0, from));
                                samples.push(at);
                            }
                        }
                        // A rational spline lies inside its control
                        // polygon, which stands in for it.
                        None => {
                            samples.extend(control_points.iter().map(|point| [point.x, point.y]))
                        }
                    }
                }
            }
        }
    }
    samples
}

/// An arc's radius, the angle it starts at and the signed angle it turns
/// through, positive counter-clockwise; a whole turn where it ends where it
/// starts.
fn arc_angles(
    center: ProtocolPoint2,
    start: ProtocolPoint2,
    end: ProtocolPoint2,
    direction: artificer_protocol::ArcDirection,
) -> (f64, f64, f64) {
    let radius = (start.x - center.x).hypot(start.y - center.y);
    let from = (start.y - center.y).atan2(start.x - center.x);
    let to = (end.y - center.y).atan2(end.x - center.x);
    let sweep = match direction {
        artificer_protocol::ArcDirection::CounterClockwise => {
            let turn = (to - from).rem_euclid(TAU);
            if turn == 0.0 { TAU } else { turn }
        }
        artificer_protocol::ArcDirection::Clockwise => {
            let turn = (from - to).rem_euclid(TAU);
            -(if turn == 0.0 { TAU } else { turn })
        }
    };
    (radius, from, sweep)
}

/// Points whose convex hull holds every curve of the profile, in its own
/// coordinates: a line's ends, a spline's control points — a spline lies in
/// the hull of its control polygon — and for a circle or an arc the corners
/// of a polygon drawn about it, every edge touching it, in steps of at most
/// a sixteenth of a turn.
fn hull_points(profile: &PlanarProfile2) -> Vec<[f64; 2]> {
    fn about(
        points: &mut Vec<[f64; 2]>,
        center: ProtocolPoint2,
        radius: f64,
        from: f64,
        sweep: f64,
    ) {
        let steps = ((sweep.abs() / (TAU / 16.0)).ceil() as usize).max(1);
        let step = sweep / steps as f64;
        // The tangents at two neighbouring points meet at the middle angle,
        // `1/cos(step/2)` of the radius out.
        let out = radius / (0.5 * step).cos();
        for index in 0..=steps {
            let angle = step.mul_add(index as f64, from);
            points.push([
                radius.mul_add(angle.cos(), center.x),
                radius.mul_add(angle.sin(), center.y),
            ]);
            if index < steps {
                let middle = angle + 0.5 * step;
                points.push([
                    out.mul_add(middle.cos(), center.x),
                    out.mul_add(middle.sin(), center.y),
                ]);
            }
        }
    }
    let mut points = Vec::new();
    for region in &profile.regions {
        for curve in std::iter::once(&region.outer)
            .chain(&region.holes)
            .flat_map(|profile_loop| &profile_loop.curves)
        {
            match curve {
                PlanarCurve2::Line { start, end } => {
                    points.push([start.x, start.y]);
                    points.push([end.x, end.y]);
                }
                PlanarCurve2::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => {
                    let (radius, from, sweep) = arc_angles(*center, *start, *end, *direction);
                    about(&mut points, *center, radius, from, sweep);
                }
                PlanarCurve2::Circle { center, radius, .. } => {
                    about(&mut points, *center, *radius, 0.0, TAU);
                }
                PlanarCurve2::Bspline { control_points, .. } => {
                    points.extend(control_points.iter().map(|point| [point.x, point.y]));
                }
            }
        }
    }
    points
}

/// The profile frame moved by a rigid motion: `place` takes a point, `turn` a
/// direction.
fn moved(
    frame: PlanarFrame3,
    placed: Frame,
    place: impl Fn(Point3) -> Point3,
    turn: impl Fn(Vector3) -> Vector3,
) -> PlanarFrame3 {
    let origin = place(point(frame.origin));
    let (u, v) = (turn(placed.u), turn(placed.v));
    PlanarFrame3::new(
        ProtocolPoint3::new(origin.x, origin.y, origin.z),
        ProtocolVector3::new(u.x, u.y, u.z),
        ProtocolVector3::new(v.x, v.y, v.z),
    )
}

/// An arc's axis in the profile's own coordinates, when it lies in the
/// profile's plane.
fn axis_in_frame(
    center: Point3,
    normal: Vector3,
    placed: Frame,
    precision: PrecisionPolicy,
) -> Option<PlanarAxis2> {
    let offset = center - placed.origin;
    let scale = offset
        .x
        .abs()
        .max(offset.y.abs())
        .max(offset.z.abs())
        .max(1.0);
    if offset.dot(placed.normal).abs() > precision.linear_agreement.max(1.0e-12) * scale
        || normal.dot(placed.normal).abs() > 1.0e-9
    {
        return None;
    }
    let start = ProtocolPoint2::new(offset.dot(placed.u), offset.dot(placed.v));
    Some(PlanarAxis2::new(
        start,
        ProtocolPoint2::new(
            start.x + normal.dot(placed.u),
            start.y + normal.dot(placed.v),
        ),
    ))
}

const fn point(point: ProtocolPoint3) -> Point3 {
    Point3::new(point.x, point.y, point.z)
}

const fn vector(vector: ProtocolVector3) -> Vector3 {
    Vector3::new(vector.x, vector.y, vector.z)
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > 1.0e-12).then(|| vector / length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station(point: Point3, tangent: Vector3) -> Station {
        Station {
            point,
            tangent,
            first: tangent,
            second: Vector3::new(0.0, 0.0, 0.0),
        }
    }

    /// Along a helix the rotation-minimising frame turns against the Frenet
    /// frame at the helix's torsion, `τ = c / (a² + c²)` per unit length; the
    /// double reflection carries it there to within a small error.
    #[test]
    fn a_helix_frame_turns_at_its_torsion() {
        let (a, c) = (3.0_f64, 1.0_f64);
        let speed = a.hypot(c);
        let torsion = c / (a * a + c * c);
        let count = 2_000;
        let turns = 2.0 * TAU;
        let at = |t: f64| {
            station(
                Point3::new(a * t.cos(), a * t.sin(), c * t),
                Vector3::new(-a * t.sin(), a * t.cos(), c) / speed,
            )
        };
        // Frenet's normal points at the axis.
        let frenet = |t: f64| Vector3::new(-t.cos(), -t.sin(), 0.0);
        let mut reference = frenet(0.0);
        let mut previous = at(0.0);
        for step in 1..=count {
            let t = turns * f64::from(step) / f64::from(count);
            let next = at(t);
            reference = reflected(previous, reference, next).expect("carried");
            previous = next;
        }
        // Minimising rotation lags Frenet's by `τ · s`.
        let binormal = previous.tangent.cross(frenet(turns));
        let lag = torsion * speed * turns;
        let expected = frenet(turns) * lag.cos() - binormal * lag.sin();
        assert!(
            (reference - expected).length() < 1.0e-5,
            "{reference:?} against {expected:?}"
        );
    }

    fn disc(radius: f64) -> PlanarProfile2 {
        PlanarProfile2 {
            regions: vec![artificer_protocol::PlanarRegion2 {
                outer: artificer_protocol::PlanarLoop2 {
                    curves: vec![PlanarCurve2::Circle {
                        center: ProtocolPoint2::new(0.0, 0.0),
                        radius,
                        direction: artificer_protocol::ArcDirection::CounterClockwise,
                    }],
                },
                holes: vec![],
            }],
        }
    }

    /// The self-intersection check alone, for a disc of radius one swept
    /// from the origin, square to the path, by its rotation-minimising
    /// frame: the path sampled as the skinned route first samples it.
    fn clear_of_itself(segments: Vec<SweepSegment3>) -> Result<(), SweepInputError> {
        let precision = PrecisionPolicy::default();
        let pieces = parse_path(&SweepPath3 { segments }, precision)?;
        let (point, first, _) = pieces[0].jet(0.0);
        let tangent = unit(first).expect("a sound start");
        let seed = if tangent.x.abs() < 0.9 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let u = unit(seed - tangent * seed.dot(tangent)).expect("square to the path");
        let v = tangent.cross(u);
        let placed = Frame {
            origin: point,
            u,
            v,
            normal: tangent,
        };
        let positions = (0..=pieces.len() * 24)
            .map(|step| step as f64 / 24.0)
            .collect::<Vec<_>>();
        let stations = stations(&pieces, &positions);
        let frames =
            carry_frames(&stations, placed, SweepOrientation::RotationMinimising).expect("frames");
        let hull = hull_points(&disc(1.0))
            .into_iter()
            .map(|point| placed.origin + placed.u * point[0] + placed.v * point[1])
            .collect::<Vec<_>>();
        check_clear_of_itself(&stations, &frames, &hull, placed.normal)
    }

    fn line(start: [f64; 3], end: [f64; 3]) -> SweepSegment3 {
        SweepSegment3::Line {
            start: ProtocolPoint3::new(start[0], start[1], start[2]),
            end: ProtocolPoint3::new(end[0], end[1], end[2]),
        }
    }

    /// A quarter turn and a half of radius `bend` about `y`, from the top of
    /// a line up the `z` axis.
    fn hook(bend: f64) -> SweepSegment3 {
        SweepSegment3::Arc {
            center: ProtocolPoint3::new(bend, 0.0, 5.0),
            start: ProtocolPoint3::new(0.0, 0.0, 5.0),
            normal: ProtocolVector3::new(0.0, 1.0, 0.0),
            sweep: 1.5 * std::f64::consts::PI,
        }
    }

    /// A helix of `turns` about `z` as a cubic through points on it.
    fn spring(radius: f64, pitch: f64, turns: f64) -> SweepSegment3 {
        let count = (turns * 12.0).ceil() as usize + 1;
        let points = (0..count)
            .map(|index| {
                let angle = turns * TAU * index as f64 / (count - 1) as f64;
                ProtocolPoint3::new(
                    radius * angle.cos(),
                    radius * angle.sin(),
                    pitch * angle / TAU,
                )
            })
            .collect::<Vec<_>>();
        let mut knots = vec![0.0; 4];
        knots.extend((1..count - 3).map(|index| index as f64 / (count - 3) as f64));
        knots.extend([1.0; 4]);
        SweepSegment3::Spline {
            degree: 3,
            knots,
            points,
        }
    }

    /// A path that comes back through the solid it has swept is refused,
    /// and one that comes back only near it — a tight hook, a spring whose
    /// coils clear each other — is not, however tightly it bends.
    #[test]
    fn a_path_back_through_itself_is_refused_and_one_that_only_passes_near_is_not() {
        let up = || line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]);
        // Up, three quarters round a bend of 1.5 and back through the way
        // up.
        assert_eq!(
            clear_of_itself(vec![
                up(),
                hook(1.5),
                line([1.5, 0.0, 3.5], [-3.0, 0.0, 3.5]),
            ]),
            Err(SweepInputError::SelfIntersecting)
        );
        // Coils 1.5 apart, of a wire 2 across.
        assert_eq!(
            clear_of_itself(vec![spring(5.0, 1.5, 1.5)]),
            Err(SweepInputError::SelfIntersecting)
        );
        // The same hook of radius 1.3 stops short: its end is 0.3 clear of
        // the way up, and the bend never meets itself.
        assert_eq!(clear_of_itself(vec![up(), hook(1.3)]), Ok(()));
        // Coils 2.4 apart, wound on a radius only half again the wire's.
        assert_eq!(clear_of_itself(vec![spring(1.5, 2.4, 1.5)]), Ok(()));
    }

    /// The fold condition holds a profile leaning on the path to the side
    /// it leans back from: a disc of radius 2 round a bend of radius 3 is
    /// clear square to the path, and folds leaning sixty degrees back.
    #[test]
    fn a_leaning_profile_folds_sooner() {
        let segments = vec![
            line([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]),
            SweepSegment3::Arc {
                center: ProtocolPoint3::new(3.0, 0.0, 5.0),
                start: ProtocolPoint3::new(0.0, 0.0, 5.0),
                normal: ProtocolVector3::new(0.0, 1.0, 0.0),
                sweep: 0.5 * std::f64::consts::PI,
            },
        ];
        let pieces =
            parse_path(&SweepPath3 { segments }, PrecisionPolicy::default()).expect("a sound path");
        let positions = (0..=48)
            .map(|step| f64::from(step) / 24.0)
            .collect::<Vec<_>>();
        let stations = stations(&pieces, &positions);
        let width = |lean: f64| {
            let (sin, cos) = lean.sin_cos();
            // Leaning back about `y`, away from the bend's centre at `+x`.
            let u = Vector3::new(cos, 0.0, -sin);
            let v = Vector3::new(0.0, 1.0, 0.0);
            let placed = Frame {
                origin: Point3::new(0.0, 0.0, 0.0),
                u,
                v,
                normal: u.cross(v),
            };
            let frames = carry_frames(&stations, placed, SweepOrientation::RotationMinimising)
                .expect("frames");
            let samples = (0..16)
                .map(|step| {
                    let angle = TAU * f64::from(step) / 16.0;
                    placed.origin + u * (2.0 * angle.cos()) + v * (2.0 * angle.sin())
                })
                .collect::<Vec<_>>();
            check_width(&stations, &frames, &samples, placed.normal)
        };
        assert_eq!(width(0.0), Ok(()));
        assert_eq!(
            width(std::f64::consts::FRAC_PI_3),
            Err(SweepInputError::ProfileTooWide)
        );
    }

    /// Where spline spans would ask for more copies than a sweep may start
    /// from, they are spread over the path in proportion, every piece
    /// keeping its floor. A spline of four hundred control points asked for
    /// two copies to each of its 397 spans, 795 in all, past the most a
    /// sweep may use at all; it now starts from half that most.
    #[test]
    fn many_spans_are_spread_over_the_first_copies() {
        assert_eq!(spread(&[4, 8, 2], 128, 1), vec![4, 8, 2]);
        let spread_out = spread(&[794, 4, 1], 128, 1);
        assert!(spread_out.iter().sum::<usize>() <= 128, "{spread_out:?}");
        assert!(spread_out.iter().all(|spans| *spans >= 1));
        assert!(spread_out[0] > 100);
        assert_eq!(spread(&[2; 300], 128, 1), vec![1; 300]);

        // A wave of four hundred control points rising along `z`.
        let count = 400;
        let mut knots = vec![0.0; 4];
        knots.extend((1..count - 3).map(|index| f64::from(index) / f64::from(count - 3)));
        knots.extend([1.0; 4]);
        let wave = SweepSegment3::Spline {
            degree: 3,
            knots,
            points: (0..count)
                .map(|index| {
                    let t = f64::from(index) / f64::from(count - 1);
                    ProtocolPoint3::new(3.0 * (4.0 * std::f64::consts::PI * t).sin(), 0.0, 60.0 * t)
                })
                .collect(),
        };
        let pieces = parse_path(
            &SweepPath3 {
                segments: vec![wave],
            },
            PrecisionPolicy::default(),
        )
        .expect("a sound wave");
        assert_eq!(pieces[0].spans(), 2 * 397);
        let positions = first_positions(&pieces);
        assert_eq!(positions.len(), INITIAL_SECTIONS);
        assert_eq!((positions[0], positions[positions.len() - 1]), (0.0, 1.0));
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// Along a straight line the frame does not turn at all.
    #[test]
    fn a_straight_path_does_not_twist() {
        let along = Vector3::new(1.0, 2.0, 2.0) / 3.0;
        let normal = unit(Vector3::new(2.0, -1.0, 0.0)).expect("square to the line");
        let mut reference = normal;
        let mut previous = station(Point3::new(0.0, 0.0, 0.0), along);
        for step in 1..=50 {
            let next = station(Point3::new(0.0, 0.0, 0.0) + along * f64::from(step), along);
            reference = reflected(previous, reference, next).expect("carried");
            previous = next;
        }
        assert!((reference - normal).length() < 1.0e-12);
    }
}

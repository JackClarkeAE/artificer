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
pub(crate) fn sweep(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    path: &SweepPath3,
    orientation: SweepOrientation,
    precision: PrecisionPolicy,
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

    skinned(
        frame,
        placed,
        profile,
        &pieces,
        orientation,
        &samples,
        precision,
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
fn skinned(
    frame: PlanarFrame3,
    placed: Frame,
    profile: &PlanarProfile2,
    pieces: &[Piece],
    orientation: SweepOrientation,
    samples: &[Point3],
    precision: PrecisionPolicy,
) -> Result<Swept, SweepInputError> {
    let tolerance = precision
        .approximation_budget
        .max(precision.modeling_resolution);
    // Where each copy stands along the path: piece index plus the fraction
    // of that piece, so a copy always stands at every join.
    // The copies start about evenly spaced along the whole path: a smooth
    // skin through copies far apart beside copies close together overshoots
    // and folds.
    let spacing = pieces.iter().map(|piece| piece.length()).sum::<f64>() / 12.0;
    let mut positions = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let even = (piece.length() / spacing).ceil() as usize;
        let spans = piece
            .spans()
            .max(even)
            .max(if pieces.len() == 1 { 2 } else { 1 });
        let first = usize::from(index > 0);
        positions.extend((first..=spans).map(|step| index as f64 + step as f64 / spans as f64));
    }
    loop {
        let stations = stations(pieces, &positions);
        let frames = carry_frames(&stations, placed, orientation)
            .ok_or(SweepInputError::PathInvalid { segment: 0 })?;
        if orientation == SweepOrientation::RotationMinimising {
            check_width(&stations, &frames, samples)?;
        }
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
                    profile: profile.clone(),
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
fn check_width(
    stations: &[Station],
    frames: &[Turn],
    samples: &[Point3],
) -> Result<(), SweepInputError> {
    let origin = stations[0].point;
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
        for sample in samples {
            let reach = turn.apply(*sample - origin).dot(inward);
            if reach * curvature >= 1.0 - 1.0e-9 {
                return Err(SweepInputError::ProfileTooWide);
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
/// Newton steps each.
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
                });
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

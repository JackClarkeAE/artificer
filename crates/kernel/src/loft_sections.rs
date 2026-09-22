//! A loft between two planar sections (ADR 0049, stage K-A).
//!
//! Each section is one region on a plane of its own. The loops of the two
//! sections are put into correspondence segment for segment — each loop cut
//! exactly where the other has corners it lacks — and every pair of
//! segments spans one wall: a plane, a cylinder or a cone where one of those
//! is exact, and a ruled surface otherwise. The rungs between walls are
//! straight lines, and the two sections are the caps.
//!
//! Nothing here approximates. A split is at an exact angle of an arc or an
//! exact point of a line, every wall's carrier holds its own rails exactly,
//! and a loft that would pass through itself or pinch a wall to a point is
//! refused by name before anything is built.

use artificer_protocol::{LoftSection, PlanarCurve2, PrecisionPolicy};

use crate::analytic_extrusion::{
    AnalyticLoop, BoundaryUse, Frame, Segment, allocate_id, push_cap_face, push_edge, push_loop,
    push_vertex, validate_analytic_profile_extrusion,
};
use crate::planar_profile::PlanarProfileInputError;
use crate::ruled::{RailCurve, RuledRail, RuledSurface};
use crate::topology::{
    Cone, Curve2, Curve3, Cylinder, Edge, EdgeKey, Face, FaceKey, FaceRole, Orientation,
    ParameterRange, Plane, Point2, Point3, Record, Shell, ShellKey, Solid, Surface, Topology,
    Vector2, Vector3, VertexKey,
};

/// Why a loft between sections was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LoftSectionsError {
    /// Fewer than two sections.
    TooFewSections,
    /// More than two: a smooth loft through the middle sections needs a
    /// B-spline surface, which K-B brings.
    MultiSection,
    /// A section's profile failed the ordinary planar-profile checks.
    Profile(PlanarProfileInputError),
    /// A section is not exactly one region.
    RegionCount,
    /// A section carries a B-spline curve.
    SplineCurve,
    /// Both sections lie on one plane.
    Coplanar,
    /// A section reaches onto or through the other section's plane.
    CrossesPlane,
    /// The sections have different numbers of holes.
    HoleCountMismatch,
    /// Two rungs cross, or the walls between the sections do.
    RungsCross,
    /// A wall's normal vanishes somewhere: it pinches to a point or folds
    /// flat.
    WallDegenerate,
}

/// One boundary piece of a section, in model space.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Piece {
    Line {
        start: Point3,
        end: Point3,
    },
    /// `center + radius·(cos t·u + sin t·v)` from `t = from` to `t = to`.
    Arc {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
        from: f64,
        to: f64,
    },
}

impl Piece {
    fn curve(self) -> (Curve3, ParameterRange) {
        match self {
            Self::Line { start, end } => Curve3::line_segment([start, end]),
            Self::Arc {
                center,
                u,
                v,
                radius,
                from,
                to,
            } => (
                Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                },
                ParameterRange::new(from, to),
            ),
        }
    }

    fn start(self) -> Point3 {
        let (curve, range) = self.curve();
        curve.evaluate(range.start)
    }

    fn end(self) -> Point3 {
        let (curve, range) = self.curve();
        curve.evaluate(range.end)
    }

    fn length(self) -> f64 {
        match self {
            Self::Line { start, end } => start.distance(end),
            Self::Arc {
                radius, from, to, ..
            } => radius * (to - from).abs(),
        }
    }

    fn rail(self) -> RuledRail {
        match self {
            Self::Line { start, end } => RuledRail {
                curve: RailCurve::Line {
                    endpoints: [start, end],
                },
                range: ParameterRange::new(0.0, 1.0),
            },
            Self::Arc {
                center,
                u,
                v,
                radius,
                from,
                to,
            } => RuledRail {
                curve: RailCurve::Circle {
                    center,
                    u,
                    v,
                    radius,
                },
                range: ParameterRange::new(from, to),
            },
        }
    }

    /// The piece cut at rising fractions of its own parameter, which for an
    /// arc is its angle and for a line its length: exact points of the
    /// carrier either way.
    fn split(self, fractions: &[f64]) -> Vec<Self> {
        match self {
            Self::Line { start, end } => {
                let mut points = vec![start];
                points.extend(fractions.iter().map(|fraction| {
                    Point3::new(
                        (end.x - start.x).mul_add(*fraction, start.x),
                        (end.y - start.y).mul_add(*fraction, start.y),
                        (end.z - start.z).mul_add(*fraction, start.z),
                    )
                }));
                points.push(end);
                points
                    .windows(2)
                    .map(|pair| Self::Line {
                        start: pair[0],
                        end: pair[1],
                    })
                    .collect()
            }
            Self::Arc {
                center,
                u,
                v,
                radius,
                from,
                to,
            } => {
                let mut angles = vec![from];
                angles.extend(
                    fractions
                        .iter()
                        .map(|fraction| (to - from).mul_add(*fraction, from)),
                );
                angles.push(to);
                angles
                    .windows(2)
                    .map(|pair| Self::Arc {
                        center,
                        u,
                        v,
                        radius,
                        from: pair[0],
                        to: pair[1],
                    })
                    .collect()
            }
        }
    }

    fn reversed(self) -> Self {
        match self {
            Self::Line { start, end } => Self::Line {
                start: end,
                end: start,
            },
            Self::Arc {
                center,
                u,
                v,
                radius,
                from,
                to,
            } => Self::Arc {
                center,
                u,
                v,
                radius,
                from: to,
                to: from,
            },
        }
    }

    /// Points along the piece for the coarse geometric checks: its two ends,
    /// and for an arc enough between to follow it.
    fn samples(self, count: usize) -> Vec<Point3> {
        let (curve, range) = self.curve();
        let steps = match self {
            Self::Line { .. } => 1,
            Self::Arc { .. } => count.max(1),
        };
        (0..=steps)
            .map(|index| {
                curve.evaluate(
                    (range.end - range.start).mul_add(index as f64 / steps as f64, range.start),
                )
            })
            .collect()
    }
}

/// One closed boundary of a section, walked about the loft's direction:
/// counter-clockwise for the outer loop, clockwise for a hole.
#[derive(Clone, Debug)]
struct SectionLoop {
    pieces: Vec<Piece>,
    /// A whole circle, held as one piece that starts where it ends until
    /// the correspondence decides where to cut it.
    full_circle: bool,
    centroid: Point3,
}

impl SectionLoop {
    fn reversed(&self) -> Self {
        Self {
            pieces: self
                .pieces
                .iter()
                .rev()
                .map(|piece| piece.reversed())
                .collect(),
            full_circle: self.full_circle,
            centroid: self.centroid,
        }
    }
}

#[derive(Clone, Debug)]
struct ParsedSection {
    frame: Frame,
    outer: SectionLoop,
    holes: Vec<SectionLoop>,
}

/// A cap: the plane it lies on, written so its normal points out of the
/// material.
#[derive(Clone, Copy, Debug)]
struct Cap {
    plane: Plane,
}

/// The wall between two corresponding pieces.
#[derive(Clone, Copy, Debug)]
enum Wall {
    /// A plane, and the four corners in its own coordinates in loop order:
    /// bottom start, bottom end, top end, top start.
    Plane(Plane, [Point2; 4]),
    /// A cylinder or a cone about the common axis of two arcs, with the
    /// angle it sweeps and its height.
    Revolved(Surface, f64, f64),
    Ruled(RuledSurface),
}

/// Two loops put into correspondence: `bottom[i]` and `top[i]` span wall `i`.
#[derive(Clone, Debug)]
struct LoopPair {
    bottom: Vec<Piece>,
    top: Vec<Piece>,
    walls: Vec<Wall>,
}

/// A loft checked and ready to build.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedLoftSections {
    caps: [Cap; 2],
    /// The outer loops first, then each pair of matched holes.
    loops: Vec<LoopPair>,
}

pub(crate) fn validate_loft_sections(
    sections: &[LoftSection],
    precision: PrecisionPolicy,
) -> Result<ValidatedLoftSections, LoftSectionsError> {
    match sections.len() {
        0 | 1 => return Err(LoftSectionsError::TooFewSections),
        2 => {}
        _ => return Err(LoftSectionsError::MultiSection),
    }
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let mut parsed = [
        parse_section(&sections[0], precision)?,
        parse_section(&sections[1], precision)?,
    ];

    // The loft runs from section 0 toward section 1. Each section's normal
    // is turned to point along it, and a section whose own normal points
    // back has its loops reversed, so both outer loops wind the same way
    // about the loft.
    let direction = parsed[1].outer.centroid - parsed[0].outer.centroid;
    let normals = [parsed[0].frame.normal, parsed[1].frame.normal];
    let parallel = normals[0].cross(normals[1]).length() <= precision.angular_agreement_radians;
    if parallel
        && (parsed[1].frame.origin - parsed[0].frame.origin)
            .dot(normals[0])
            .abs()
            <= minimum
    {
        return Err(LoftSectionsError::Coplanar);
    }
    let mut toward = [normals[0], normals[1]];
    for (index, section) in parsed.iter_mut().enumerate() {
        if normals[index].dot(direction) < 0.0 {
            toward[index] = normals[index] * -1.0;
            section.outer = section.outer.reversed();
            section.holes = section.holes.iter().map(SectionLoop::reversed).collect();
        }
    }

    // Each section must lie strictly beyond the other's plane, on the side
    // the loft runs toward it. Parallel planes pass by being apart; planes
    // that meet pass only where the sections keep clear of the line they
    // meet in, and a loft whose sections reach across it would fold.
    let beyond = |section: &ParsedSection, plane: &ParsedSection, normal: Vector3, sign: f64| {
        std::iter::once(&section.outer)
            .chain(&section.holes)
            .flat_map(|section_loop| section_loop.pieces.iter())
            .flat_map(|piece| piece.samples(32))
            .all(|point| sign * (point - plane.frame.origin).dot(normal) > minimum)
    };
    if !beyond(&parsed[1], &parsed[0], toward[0], 1.0)
        || !beyond(&parsed[0], &parsed[1], toward[1], -1.0)
    {
        return Err(LoftSectionsError::CrossesPlane);
    }

    if parsed[0].holes.len() != parsed[1].holes.len() {
        return Err(LoftSectionsError::HoleCountMismatch);
    }
    // Holes are matched by nearest centroid, each to one.
    let mut unmatched = (0..parsed[1].holes.len()).collect::<Vec<_>>();
    let mut hole_pairs = Vec::with_capacity(parsed[0].holes.len());
    for hole in &parsed[0].holes {
        let Some((slot, _)) = unmatched
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                let left = parsed[1].holes[**left].centroid.distance(hole.centroid);
                let right = parsed[1].holes[**right].centroid.distance(hole.centroid);
                left.total_cmp(&right)
            })
        else {
            return Err(LoftSectionsError::HoleCountMismatch);
        };
        let other = unmatched.remove(slot);
        hole_pairs.push((hole.clone(), parsed[1].holes[other].clone()));
    }

    let snap = 16.0 * minimum;
    let mut loops = Vec::with_capacity(1 + hole_pairs.len());
    for (bottom, top) in
        std::iter::once((parsed[0].outer.clone(), parsed[1].outer.clone())).chain(hole_pairs)
    {
        let (bottom, top) = correspond(&bottom, &top, snap);
        let walls = bottom
            .iter()
            .zip(&top)
            .map(|(low, high)| wall(*low, *high, precision))
            .collect::<Result<Vec<_>, _>>()?;
        loops.push(LoopPair { bottom, top, walls });
    }

    rungs_clear(&loops, minimum)?;
    walls_clear(&loops, toward, minimum)?;

    let caps = [
        cap(parsed[0].frame, toward[0] * -1.0),
        cap(parsed[1].frame, toward[1]),
    ];
    Ok(ValidatedLoftSections { caps, loops })
}

/// A section's frame, loops and centroids, checked as any planar profile is.
fn parse_section(
    section: &LoftSection,
    precision: PrecisionPolicy,
) -> Result<ParsedSection, LoftSectionsError> {
    let profile = &section.profile;
    if profile
        .regions
        .iter()
        .flat_map(|region| std::iter::once(&region.outer).chain(&region.holes))
        .flat_map(|profile_loop| &profile_loop.curves)
        .any(|curve| matches!(curve, PlanarCurve2::Bspline { .. }))
    {
        return Err(LoftSectionsError::SplineCurve);
    }
    if profile.regions.len() != 1 {
        return Err(LoftSectionsError::RegionCount);
    }
    // The extrusion checks run with a height of their own smallest: they
    // certify the profile and its frame, and the height is only there to
    // be positive.
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let validated =
        validate_analytic_profile_extrusion(section.frame, profile, 2.0 * minimum, precision)
            .map_err(LoftSectionsError::Profile)?;
    let Some(region) = validated.regions.into_iter().next() else {
        return Err(LoftSectionsError::RegionCount);
    };
    let frame = region.frame;
    let source = &profile.regions[0];
    let whole_circle = |profile_loop: &artificer_protocol::PlanarLoop2| {
        matches!(
            profile_loop.curves.as_slice(),
            [PlanarCurve2::Circle { .. }]
        )
    };
    let mut loops = region
        .loops
        .iter()
        .zip(std::iter::once(&source.outer).chain(&source.holes))
        .map(|(analytic, drawn)| section_loop(frame, analytic, whole_circle(drawn)))
        .collect::<Option<Vec<_>>>()
        .ok_or(LoftSectionsError::Profile(
            PlanarProfileInputError::AnalyticCurve,
        ))?;
    let outer = loops.remove(0);
    Ok(ParsedSection {
        frame,
        outer,
        holes: loops,
    })
}

/// A validated profile loop as model-space pieces, a whole circle as one.
fn section_loop(frame: Frame, analytic: &AnalyticLoop, full_circle: bool) -> Option<SectionLoop> {
    let mut pieces = Vec::with_capacity(analytic.segments.len());
    for segment in &analytic.segments {
        pieces.push(match *segment {
            Segment::Line { start, end } => Piece::Line {
                start: frame.point(start, 0.0),
                end: frame.point(end, 0.0),
            },
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            } => Piece::Arc {
                center: frame.point(center, 0.0),
                u: frame.u,
                v: frame.v,
                radius,
                from: start_angle,
                to: start_angle + sweep,
            },
            Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => {
                return None;
            }
        });
    }
    if full_circle {
        // The validator splits a circle into two halves; the loft cuts it
        // where the other section says to, so it is held whole until then.
        let (Some(Piece::Arc { from, .. }), Some(Piece::Arc { to, .. })) =
            (pieces.first().copied(), pieces.last().copied())
        else {
            return None;
        };
        let Piece::Arc {
            center,
            u,
            v,
            radius,
            ..
        } = pieces[0]
        else {
            return None;
        };
        pieces = vec![Piece::Arc {
            center,
            u,
            v,
            radius,
            from,
            to,
        }];
    }
    let centroid = loop_centroid(frame, analytic)?;
    Some(SectionLoop {
        pieces: harmonised(pieces),
        full_circle,
        centroid,
    })
}

/// The area centroid of a profile loop, from a fine polygon of it.
fn loop_centroid(frame: Frame, analytic: &AnalyticLoop) -> Option<Point3> {
    let points = analytic
        .segments
        .iter()
        .flat_map(|segment| {
            let steps = match segment {
                Segment::Line { .. } => 1,
                _ => 64,
            };
            (0..steps).map(move |index| segment.point_at(index as f64 / steps as f64))
        })
        .collect::<Vec<_>>();
    let anchor = *points.first()?;
    let (mut area, mut x, mut y) = (0.0, 0.0, 0.0);
    for (index, start) in points.iter().enumerate() {
        let end = points[(index + 1) % points.len()];
        let (ax, ay) = (start.x - anchor.x, start.y - anchor.y);
        let (bx, by) = (end.x - anchor.x, end.y - anchor.y);
        let cross = ax.mul_add(by, -(ay * bx));
        area += cross;
        x += (ax + bx) * cross;
        y += (ay + by) * cross;
    }
    if !area.is_finite() || area == 0.0 {
        return None;
    }
    Some(frame.point(
        Point2::new(anchor.x + x / (3.0 * area), anchor.y + y / (3.0 * area)),
        0.0,
    ))
}

/// Lines take their ends from the arcs beside them, so every vertex of a
/// loop is exactly the point its carriers evaluate there.
fn harmonised(mut pieces: Vec<Piece>) -> Vec<Piece> {
    let count = pieces.len();
    if count < 2 {
        return pieces;
    }
    for index in 0..count {
        let previous = pieces[(index + count - 1) % count];
        let next = pieces[(index + 1) % count];
        if let Piece::Line { start, end } = &mut pieces[index] {
            if matches!(previous, Piece::Arc { .. }) {
                *start = previous.end();
            }
            if matches!(next, Piece::Arc { .. }) {
                *end = next.start();
            }
        }
    }
    pieces
}

/// The pieces of two loops in correspondence, one wall per pair.
///
/// A whole circle is first cut at the point nearest the other loop's first
/// vertex, and two whole circles at one direction from their centres.
/// Loops with as many pieces as each other pair in order, from the
/// cyclic offset that makes the rungs shortest in the sum of their squares.
/// Otherwise each loop is cut at the normalised arc-length positions of the
/// other's vertices — the denser loop's first vertex held fixed, and the
/// sparser loop started from whichever of its vertices makes the rungs
/// shortest — so both end up with a piece for every vertex either has.
fn correspond(bottom: &SectionLoop, top: &SectionLoop, snap: f64) -> (Vec<Piece>, Vec<Piece>) {
    let mut low = bottom.pieces.clone();
    let mut high = top.pieces.clone();
    match (bottom.full_circle, top.full_circle) {
        (true, false) => low = vec![rebased(low[0], high[0].start())],
        (false, true) => high = vec![rebased(high[0], low[0].start())],
        // Two whole circles have no vertex to be nearest to. They are cut at
        // one direction from their centres, so every rung pairs points at
        // the same angle and none of the walls twists.
        (true, true) => {
            if let (
                Piece::Arc { center, .. },
                Piece::Arc {
                    center: top_center, ..
                },
            ) = (low[0], high[0])
            {
                high = vec![rebased(high[0], top_center + (low[0].start() - center))];
            }
        }
        (false, false) => {}
    }
    let (mut low, mut high) = if low.len() == high.len() {
        let offset = (0..high.len())
            .min_by(|left, right| {
                rung_cost(&low, &rotated(&high, *left))
                    .total_cmp(&rung_cost(&low, &rotated(&high, *right)))
            })
            .unwrap_or(0);
        (low, rotated(&high, offset))
    } else {
        let low_is_dense = low.len() > high.len();
        let (dense, sparse) = if low_is_dense {
            (&low, &high)
        } else {
            (&high, &low)
        };
        let best = (0..sparse.len())
            .filter_map(|offset| cut_to_common(dense, &rotated(sparse, offset), snap))
            .min_by(|left, right| {
                rung_cost(&left.0, &left.1).total_cmp(&rung_cost(&right.0, &right.1))
            });
        match best {
            Some((dense, sparse)) if low_is_dense => (dense, sparse),
            Some((dense, sparse)) => (sparse, dense),
            // No alignment cut both loops alike; pair what there is and let
            // the wall checks refuse it.
            None => (low, high),
        }
    };
    // A wall needs two rungs of its own: a whole circle paired with a whole
    // circle is halved.
    if low.len() == 1 && high.len() == 1 {
        low = low[0].split(&[0.5]);
        high = high[0].split(&[0.5]);
    }
    (harmonised(low), harmonised(high))
}

/// A whole circle restarted at the point of it nearest `target`.
fn rebased(piece: Piece, target: Point3) -> Piece {
    let Piece::Arc {
        center,
        u,
        v,
        radius,
        from,
        to,
    } = piece
    else {
        return piece;
    };
    let offset = target - center;
    let (across, along) = (offset.dot(v), offset.dot(u));
    let angle = if across == 0.0 && along == 0.0 {
        from
    } else {
        across.atan2(along)
    };
    Piece::Arc {
        center,
        u,
        v,
        radius,
        from: angle,
        to: angle + (to - from),
    }
}

fn rotated(pieces: &[Piece], offset: usize) -> Vec<Piece> {
    let count = pieces.len();
    (0..count)
        .map(|index| pieces[(index + offset) % count])
        .collect()
}

/// The summed squared length of the rungs two equal lists of pieces imply.
fn rung_cost(bottom: &[Piece], top: &[Piece]) -> f64 {
    bottom
        .iter()
        .zip(top)
        .map(|(low, high)| {
            let rung = high.start() - low.start();
            rung.dot(rung)
        })
        .sum()
}

/// Where each vertex of a loop sits along it, as a fraction of its length.
fn positions(pieces: &[Piece]) -> (Vec<f64>, Vec<f64>) {
    let lengths = pieces
        .iter()
        .map(|piece| piece.length())
        .collect::<Vec<_>>();
    let total = lengths.iter().sum::<f64>();
    let mut starts = Vec::with_capacity(pieces.len());
    let mut walked = 0.0;
    for length in &lengths {
        starts.push(walked / total);
        walked += length;
    }
    (
        starts,
        lengths.iter().map(|length| length / total).collect(),
    )
}

/// Both loops cut at every position either has a vertex at, a position of
/// one within `snap` (in length) of a vertex of the other counting as that
/// vertex. `None` when the two do not come out with as many pieces.
fn cut_to_common(dense: &[Piece], sparse: &[Piece], snap: f64) -> Option<(Vec<Piece>, Vec<Piece>)> {
    let total = |pieces: &[Piece]| pieces.iter().map(|piece| piece.length()).sum::<f64>();
    let (dense_total, sparse_total) = (total(dense), total(sparse));
    if !(dense_total > 0.0 && sparse_total > 0.0) {
        return None;
    }
    let tolerance = snap / dense_total.min(sparse_total);
    let (dense_starts, _) = positions(dense);
    let (sparse_starts, _) = positions(sparse);
    let near = |position: f64, starts: &[f64]| {
        starts
            .iter()
            .any(|start| (start - position).abs() <= tolerance || (1.0 - position) <= tolerance)
    };
    let dense_cuts = sparse_starts[1..]
        .iter()
        .copied()
        .filter(|position| !near(*position, &dense_starts))
        .collect::<Vec<_>>();
    let sparse_cuts = dense_starts[1..]
        .iter()
        .copied()
        .filter(|position| !near(*position, &sparse_starts))
        .collect::<Vec<_>>();
    let dense = cut_at(dense, &dense_cuts);
    let sparse = cut_at(sparse, &sparse_cuts);
    (dense.len() == sparse.len()).then_some((dense, sparse))
}

/// A loop cut at rising positions along it.
fn cut_at(pieces: &[Piece], cuts: &[f64]) -> Vec<Piece> {
    let (starts, shares) = positions(pieces);
    let mut result = Vec::with_capacity(pieces.len() + cuts.len());
    let mut next = 0;
    for ((piece, start), share) in pieces.iter().zip(&starts).zip(&shares) {
        let mut fractions = Vec::new();
        while next < cuts.len() && cuts[next] < start + share {
            let fraction = (cuts[next] - start) / share;
            if fraction > 0.0 && fraction < 1.0 {
                fractions.push(fraction);
            }
            next += 1;
        }
        result.extend(piece.split(&fractions));
    }
    result
}

/// The carrier of the wall between two corresponding pieces: a plane, a
/// cylinder or a cone where one of those is exact, a ruled surface
/// otherwise, and a refusal where the wall would pinch.
fn wall(bottom: Piece, top: Piece, precision: PrecisionPolicy) -> Result<Wall, LoftSectionsError> {
    let ruled = RuledSurface {
        rails: [bottom.rail(), top.rail()],
    };
    let scale = ruled.scale().max(1.0);
    if ruled.least_normal((0.0, 1.0, 0.0, 1.0)) <= precision.linear_agreement * scale {
        return Err(LoftSectionsError::WallDegenerate);
    }
    if let Some(planar) = planar_wall(bottom, top, precision) {
        return Ok(planar);
    }
    if let Some(revolved) = revolved_wall(bottom, top, precision) {
        return Ok(revolved);
    }
    Ok(Wall::Ruled(ruled))
}

/// Two straight rails in one plane span that plane.
fn planar_wall(bottom: Piece, top: Piece, precision: PrecisionPolicy) -> Option<Wall> {
    let (Piece::Line { .. }, Piece::Line { .. }) = (bottom, top) else {
        return None;
    };
    let corners = [bottom.start(), bottom.end(), top.end(), top.start()];
    // Newell's normal walks the corners in loop order, which is the wall's
    // own orientation, so it points out of the material.
    let mut normal = Vector3::new(0.0, 0.0, 0.0);
    for (index, current) in corners.iter().enumerate() {
        let next = corners[(index + 1) % 4];
        normal = normal
            + Vector3::new(
                (current.y - next.y) * (current.z + next.z),
                (current.z - next.z) * (current.x + next.x),
                (current.x - next.x) * (current.y + next.y),
            );
    }
    let normal = normal / normal.length();
    let flatness = corners
        .iter()
        .map(|corner| (*corner - corners[0]).dot(normal).abs())
        .fold(0.0_f64, f64::max);
    if !flatness.is_finite() || flatness > 0.25 * precision.linear_agreement {
        return None;
    }
    let along = corners[1] - corners[0];
    let u = along / along.length();
    let v = normal.cross(u);
    let plane = Plane::new(corners[0], u, v);
    Some(Wall::Plane(
        plane,
        corners.map(|corner| plane.project(corner)),
    ))
}

/// Two arcs about one axis, on parallel planes, covering the same angles
/// about it, span a cylinder or a cone: the ruled surface between them
/// pairs points at equal azimuth, and that is what those carriers are.
fn revolved_wall(bottom: Piece, top: Piece, precision: PrecisionPolicy) -> Option<Wall> {
    let (
        Piece::Arc {
            center,
            u,
            v,
            radius,
            from,
            to,
        },
        Piece::Arc {
            center: top_center,
            u: top_u,
            v: top_v,
            radius: top_radius,
            from: top_from,
            to: top_to,
        },
    ) = (bottom, top)
    else {
        return None;
    };
    let angular = precision.angular_agreement_radians.max(1.0e-12);
    let rise = top_center - center;
    let height = rise.length();
    if !height.is_finite() || height <= precision.min_feature_size {
        return None;
    }
    let axis = rise / height;
    let (normal, top_normal) = (u.cross(v), top_u.cross(top_v));
    if axis.cross(normal).length() > angular || axis.cross(top_normal).length() > angular {
        return None;
    }
    let start = bottom.start() - center;
    let top_start = top.start() - top_center;
    let radial_u = start / start.length();
    if (top_start / top_start.length() - radial_u).length() > angular {
        return None;
    }
    // Both sweeps measured about the common axis.
    let sweep = (to - from) * normal.dot(axis).signum();
    let top_sweep = (top_to - top_from) * top_normal.dot(axis).signum();
    if (sweep - top_sweep).abs() > angular {
        return None;
    }
    let radial_v = axis.cross(radial_u);
    let sign = sweep.signum();
    let surface = if (radius - top_radius).abs() <= precision.linear_agreement {
        Surface::Cylinder(Cylinder {
            origin: center,
            axis,
            radial_u,
            radial_v,
            radius,
            angular_sign: sign,
        })
    } else {
        Surface::Cone(Cone {
            origin: center,
            axis,
            radial_u,
            radial_v,
            base_radius: radius,
            slope: (top_radius - radius) / height,
            angular_sign: sign,
        })
    };
    Some(Wall::Revolved(surface, sweep.abs(), height))
}

/// No two rungs may come within the feature floor of each other.
fn rungs_clear(loops: &[LoopPair], minimum: f64) -> Result<(), LoftSectionsError> {
    let rungs = loops
        .iter()
        .flat_map(|pair| {
            pair.bottom
                .iter()
                .zip(&pair.top)
                .map(|(low, high)| (low.start(), high.start()))
        })
        .collect::<Vec<_>>();
    for (index, first) in rungs.iter().enumerate() {
        for second in &rungs[index + 1..] {
            if segment_distance(*first, *second) <= minimum {
                return Err(LoftSectionsError::RungsCross);
            }
        }
    }
    Ok(())
}

/// The walls must not cross one another between the sections: at every
/// sampled height the loops they trace, seen along the loft there, are
/// simple, and each hole's lies inside the outer one's and clear of the
/// other holes'.
fn walls_clear(
    loops: &[LoopPair],
    toward: [Vector3; 2],
    minimum: f64,
) -> Result<(), LoftSectionsError> {
    const HEIGHTS: usize = 16;
    for step in 1..HEIGHTS {
        let v = step as f64 / HEIGHTS as f64;
        let normal = toward[0] * (1.0 - v) + toward[1] * v;
        let normal = normal / normal.length();
        let seed = if normal.x.abs() < 0.9 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let across = normal.cross(seed);
        let across = across / across.length();
        let up = normal.cross(across);
        let polygons = loops
            .iter()
            .map(|pair| {
                pair.bottom
                    .iter()
                    .zip(&pair.top)
                    .flat_map(|(low, high)| {
                        let surface = RuledSurface {
                            rails: [low.rail(), high.rail()],
                        };
                        let steps =
                            if matches!((low, high), (Piece::Line { .. }, Piece::Line { .. })) {
                                1
                            } else {
                                16
                            };
                        (0..steps).map(move |index| {
                            let point =
                                surface.evaluate(Point2::new(index as f64 / steps as f64, v));
                            Point2::new(point.as_vector().dot(across), point.as_vector().dot(up))
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for (index, polygon) in polygons.iter().enumerate() {
            if polygon_crosses(polygon, polygon, true, minimum) {
                return Err(LoftSectionsError::RungsCross);
            }
            if index > 0 {
                if !point_in_polygon(polygon[0], &polygons[0]) {
                    return Err(LoftSectionsError::RungsCross);
                }
                for (other_index, other) in polygons[..index].iter().enumerate() {
                    let nested = other_index > 0
                        && (point_in_polygon(polygon[0], other)
                            || point_in_polygon(other[0], polygon));
                    if nested || polygon_crosses(polygon, other, false, minimum) {
                        return Err(LoftSectionsError::RungsCross);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether two closed polygons have edges that come within `minimum` of each
/// other; for a polygon against itself, only edges that are not neighbours.
fn polygon_crosses(first: &[Point2], second: &[Point2], same: bool, minimum: f64) -> bool {
    let (n, m) = (first.len(), second.len());
    for i in 0..n {
        let a = (first[i], first[(i + 1) % n]);
        for j in 0..m {
            if same && (j <= i || j == (i + 1) % n || (j + 1) % m == i) {
                continue;
            }
            let b = (second[j], second[(j + 1) % m]);
            if segment_distance_2d(a, b) <= minimum {
                return true;
            }
        }
    }
    false
}

fn point_in_polygon(point: Point2, polygon: &[Point2]) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        if (a.y > point.y) != (b.y > point.y) {
            let x = (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x;
            if point.x < x {
                inside = !inside;
            }
        }
    }
    inside
}

fn segment_distance_2d(first: (Point2, Point2), second: (Point2, Point2)) -> f64 {
    let lift = |point: Point2| Point3::new(point.x, point.y, 0.0);
    segment_distance(
        (lift(first.0), lift(first.1)),
        (lift(second.0), lift(second.1)),
    )
}

/// The distance between two segments in space.
fn segment_distance(first: (Point3, Point3), second: (Point3, Point3)) -> f64 {
    let d1 = first.1 - first.0;
    let d2 = second.1 - second.0;
    let r = first.0 - second.0;
    let a = d1.dot(d1);
    let e = d2.dot(d2);
    let f = d2.dot(r);
    let (s, t) = if a <= f64::EPSILON && e <= f64::EPSILON {
        (0.0, 0.0)
    } else if a <= f64::EPSILON {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = d1.dot(r);
        if e <= f64::EPSILON {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = d1.dot(d2);
            let denominator = a.mul_add(e, -(b * b));
            let mut s = if denominator > 0.0 {
                (b.mul_add(f, -(c * e)) / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t = (b.mul_add(s, f)) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
            (s, t)
        }
    };
    ((first.0 + d1 * s) - (second.0 + d2 * t)).length()
}

/// A cap's plane through the section frame, facing `outward`.
fn cap(frame: Frame, outward: Vector3) -> Cap {
    let plane = if frame.normal.dot(outward) > 0.0 {
        Plane::new(frame.origin, frame.u, frame.v)
    } else {
        Plane::new(frame.origin, frame.v, frame.u)
    };
    Cap { plane }
}

/// Builds the loft: the two caps, then the walls loop by loop.
pub(crate) fn build_loft_sections(loft: &ValidatedLoftSections) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    struct Keys {
        bottom_edges: Vec<EdgeKey>,
        top_edges: Vec<EdgeKey>,
        rungs: Vec<EdgeKey>,
    }
    let mut keys = Vec::with_capacity(loft.loops.len());
    for pair in &loft.loops {
        let count = pair.bottom.len();
        let bottom_vertices = pair
            .bottom
            .iter()
            .map(|piece| push_vertex(&mut topology, &mut next_id, piece.start()))
            .collect::<Vec<_>>();
        let top_vertices = pair
            .top
            .iter()
            .map(|piece| push_vertex(&mut topology, &mut next_id, piece.start()))
            .collect::<Vec<_>>();
        let mut edges_of = |pieces: &[Piece], vertices: &[VertexKey]| {
            pieces
                .iter()
                .enumerate()
                .map(|(index, piece)| {
                    let (curve, parameter_range) = piece.curve();
                    push_edge(
                        &mut topology,
                        &mut next_id,
                        Edge {
                            vertices: [vertices[index], vertices[(index + 1) % count]],
                            curve,
                            parameter_range,
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let bottom_edges = edges_of(&pair.bottom, &bottom_vertices);
        let top_edges = edges_of(&pair.top, &top_vertices);
        let rungs = (0..count)
            .map(|index| {
                push_edge(
                    &mut topology,
                    &mut next_id,
                    Edge::line(
                        [bottom_vertices[index], top_vertices[index]],
                        [pair.bottom[index].start(), pair.top[index].start()],
                    ),
                )
            })
            .collect::<Vec<_>>();
        keys.push(Keys {
            bottom_edges,
            top_edges,
            rungs,
        });
    }

    // The caps. Each loop runs about the loft, which is the top cap's own
    // outward sense and against the bottom cap's, so the bottom walks its
    // loops backwards.
    for (cap_index, role) in [(0, FaceRole::ExtrusionBottom), (1, FaceRole::ExtrusionTop)] {
        let plane = loft.caps[cap_index].plane;
        let loops = loft
            .loops
            .iter()
            .zip(&keys)
            .map(|(pair, keys)| {
                let (pieces, edges) = if cap_index == 0 {
                    (&pair.bottom, &keys.bottom_edges)
                } else {
                    (&pair.top, &keys.top_edges)
                };
                let mut uses = pieces
                    .iter()
                    .zip(edges)
                    .map(|(piece, edge)| BoundaryUse {
                        edge: *edge,
                        orientation: Orientation::Forward,
                        curve: cap_pcurve(plane, *piece),
                    })
                    .collect::<Vec<_>>();
                if cap_index == 0 {
                    uses.reverse();
                    for boundary_use in &mut uses {
                        boundary_use.orientation = Orientation::Reverse;
                        boundary_use.curve = reversed_pcurve(boundary_use.curve);
                    }
                }
                push_loop(&mut topology, &mut next_id, uses)
            })
            .collect::<Vec<_>>();
        push_cap_face(
            &mut topology,
            &mut next_id,
            Surface::Plane(plane),
            &loops,
            role,
        );
    }

    let mut ordinal = 0_u32;
    for (pair, keys) in loft.loops.iter().zip(&keys) {
        let count = pair.bottom.len();
        for index in 0..count {
            let next = (index + 1) % count;
            let (surface, [bottom, right, top, left]) = wall_pcurves(pair.walls[index]);
            let loop_key = push_loop(
                &mut topology,
                &mut next_id,
                [
                    (keys.bottom_edges[index], Orientation::Forward, bottom),
                    (keys.rungs[next], Orientation::Forward, right),
                    (keys.top_edges[index], Orientation::Reverse, top),
                    (keys.rungs[index], Orientation::Reverse, left),
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
                id: allocate_id(&mut next_id),
                value: Face {
                    surface,
                    outer_loop: loop_key,
                    inner_loops: Vec::new(),
                    role: FaceRole::ExtrusionSide(ordinal),
                },
            });
            ordinal += 1;
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

/// A wall's carrier and the four straight pcurves of its loop: along the
/// bottom, up the far rung, back along the top, down the near rung.
fn wall_pcurves(wall: Wall) -> (Surface, [[Point2; 2]; 4]) {
    let loop_of = |corners: [Point2; 4]| {
        [
            [corners[0], corners[1]],
            [corners[1], corners[2]],
            [corners[2], corners[3]],
            [corners[3], corners[0]],
        ]
    };
    match wall {
        Wall::Plane(plane, corners) => (Surface::Plane(plane), loop_of(corners)),
        Wall::Revolved(surface, sweep, height) => (
            surface,
            loop_of([
                Point2::new(0.0, 0.0),
                Point2::new(sweep, 0.0),
                Point2::new(sweep, height),
                Point2::new(0.0, height),
            ]),
        ),
        Wall::Ruled(ruled) => (
            Surface::Ruled(ruled),
            loop_of([
                Point2::new(0.0, 0.0),
                Point2::new(1.0, 0.0),
                Point2::new(1.0, 1.0),
                Point2::new(0.0, 1.0),
            ]),
        ),
    }
}

/// A section piece as a curve in a cap's plane, walked as the edge is.
fn cap_pcurve(plane: Plane, piece: Piece) -> (Curve2, ParameterRange) {
    match piece {
        Piece::Line { start, end } => {
            Curve2::line_segment([plane.project(start), plane.project(end)])
        }
        Piece::Arc {
            center,
            u,
            v,
            radius,
            from,
            to,
        } => (
            Curve2::Circle {
                center: plane.project(center),
                u: Vector2::new(u.dot(plane.u), u.dot(plane.v)),
                v: Vector2::new(v.dot(plane.u), v.dot(plane.v)),
                radius,
            },
            ParameterRange::new(from, to),
        ),
    }
}

fn reversed_pcurve((curve, range): (Curve2, ParameterRange)) -> (Curve2, ParameterRange) {
    match curve {
        Curve2::Line { endpoints } => Curve2::line_segment([endpoints[1], endpoints[0]]),
        other => (other, range.reversed()),
    }
}

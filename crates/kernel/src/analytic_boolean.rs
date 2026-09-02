//! The general analytic Boolean: ADR 0025's imprint, classify, regularize,
//! and sew stages over whole B-rep shells, at any relative orientation.
//!
//! The reduction that makes this exact is per-face: every face's kept
//! portion is a 2D Boolean *in that face's own parameter space* between the
//! face's region and the other solid's **section** on the face's carrier.
//! Sections are assembled from the surface-intersection matrix: each face of
//! the other solid contributes its carrier-intersection curve clipped to its
//! own parameter region, and the welded pieces chain into the closed section
//! loops. Faces the other solid never touches classify wholesale by exact
//! ray casting. The kept pieces from both operands then sew into shells,
//! with cavity components attached as inner shells, and the validator
//! checks every stage's output before anything publishes.
//!
//! The domain is the published intersection matrix. Inside it, results are
//! exact — an oblique plane through a cylinder included, whose ellipse is
//! carried as an elliptical chord on the plane and a harmonic trace on the
//! cylinder; outside it — a blended
//! operand's torus meeting anything off-axis — the operation refuses before
//! any geometry is built, and tangential or coincident contact between the
//! operands fails closed at whichever stage first sees it.

use artificer_protocol::{BooleanOperation, PrecisionPolicy};

use crate::analytic_extrusion::Segment;
use crate::profile_boolean::{
    ProfileBooleanError, ProfileRegion, chain_welded_segments, chord_region_pieces,
    profile_boolean_multi, welded,
};
use crate::sew::{SewError, SewFace, ray_directions, ray_face_crossings, sew_shells};
use crate::surface_intersection::{IntersectionCurve, SurfaceIntersection, intersect};
use crate::topology::{Cylinder, Face, Plane, Point2, Point3, Surface, Topology, Vector3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnalyticBooleanError {
    /// The operand pair leaves the engine's domain: an out-of-matrix carrier
    /// pair, a tangential or coincident contact, or a face class the sewing
    /// vocabulary cannot carry.
    DomainUnsupported,
    /// The operation succeeded and produced no material.
    EmptyResult,
}

/// Runs the general analytic Boolean over two validated solids.
pub(crate) fn build_analytic_boolean(
    target: &Topology,
    tool: &Topology,
    operation: BooleanOperation,
    precision: PrecisionPolicy,
) -> Result<Topology, AnalyticBooleanError> {
    let mut pieces = Vec::new();
    collect_operand_pieces(
        target,
        tool,
        operation,
        OperandSide::Target,
        precision,
        &mut pieces,
    )?;
    collect_operand_pieces(
        tool,
        target,
        operation,
        OperandSide::Tool,
        precision,
        &mut pieces,
    )?;
    if pieces.is_empty() {
        return Err(AnalyticBooleanError::EmptyResult);
    }
    sew_shells(&pieces, precision).map_err(|error| match error {
        SewError::Inconsistent | SewError::Degenerate => AnalyticBooleanError::DomainUnsupported,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperandSide {
    Target,
    Tool,
}

/// Which 2D operation keeps an operand's boundary, and whether kept pieces
/// flip their material side — the standard directed-boundary rules lifted to
/// faces.
fn keep_rule(side: OperandSide, operation: BooleanOperation) -> (BooleanOperation, bool) {
    match (side, operation) {
        (OperandSide::Target, BooleanOperation::Union | BooleanOperation::Difference) => {
            (BooleanOperation::Difference, false)
        }
        (OperandSide::Target, BooleanOperation::Intersection)
        | (OperandSide::Tool, BooleanOperation::Intersection) => {
            (BooleanOperation::Intersection, false)
        }
        (OperandSide::Tool, BooleanOperation::Union) => (BooleanOperation::Difference, false),
        (OperandSide::Tool, BooleanOperation::Difference) => (BooleanOperation::Intersection, true),
    }
}

fn collect_operand_pieces(
    own: &Topology,
    other: &Topology,
    operation: BooleanOperation,
    side: OperandSide,
    precision: PrecisionPolicy,
    pieces: &mut Vec<SewFace>,
) -> Result<(), AnalyticBooleanError> {
    let (operation_2d, reverse) = keep_rule(side, operation);
    for face in &own.faces {
        let region = face_region(own, &face.value)?;
        let section = section_on_face(&face.value, &region, other, precision)?;
        let kept: Vec<Vec<Vec<Segment>>> = if section.is_empty() {
            // Untouched face: wholesale in-or-out of the other solid.
            let inside = face_sample_inside(own, &face.value, &region, other, precision)?;
            let keep = match operation_2d {
                BooleanOperation::Difference => !inside,
                BooleanOperation::Intersection => inside,
                BooleanOperation::Union => unreachable!("no 2D union rule exists"),
            };
            if keep {
                vec![region.to_vec()]
            } else {
                Vec::new()
            }
        } else {
            let own_region = ProfileRegion {
                outer: region[0].clone(),
                holes: region[1..].to_vec(),
            };
            match profile_boolean_multi(
                std::slice::from_ref(&own_region),
                &section,
                operation_2d,
                precision,
            ) {
                Ok(regions) => regions
                    .into_iter()
                    .map(|region| {
                        let mut loops = vec![region.outer];
                        loops.extend(region.holes);
                        loops
                    })
                    .collect(),
                Err(ProfileBooleanError::EmptyResult) => Vec::new(),
                Err(ProfileBooleanError::Unsupported) => {
                    return Err(AnalyticBooleanError::DomainUnsupported);
                }
            }
        };
        for loops in kept {
            let piece = SewFace {
                surface: face.value.surface,
                loops,
                role: face.value.role,
            };
            pieces.push(if reverse {
                mirror_sew_face(piece)?
            } else {
                piece
            });
        }
    }
    Ok(())
}

/// A face's parameter region: outer loop first, welded for exact chaining.
fn face_region(
    topology: &Topology,
    face: &Face,
) -> Result<Vec<Vec<Segment>>, AnalyticBooleanError> {
    face.loops()
        .map(|loop_key| {
            crate::analytic_extrusion::topology_loop_chords(topology, loop_key)
                .ok_or(AnalyticBooleanError::DomainUnsupported)
                .and_then(|segments| {
                    welded(&segments, PrecisionPolicy::default())
                        .map_err(|_| AnalyticBooleanError::DomainUnsupported)
                })
        })
        .collect()
}

/// The other solid's section on this face's carrier, in the face's own
/// parameter space, as zero or more closed regions.
fn section_on_face(
    face: &Face,
    own_region: &[Vec<Segment>],
    other: &Topology,
    precision: PrecisionPolicy,
) -> Result<Vec<ProfileRegion>, AnalyticBooleanError> {
    let mut pieces: Vec<Segment> = Vec::new();
    for other_face in &other.faces {
        let outcome = intersect(face.surface, other_face.value.surface, precision)
            .map_err(|_| AnalyticBooleanError::DomainUnsupported)?;
        let curves = match outcome {
            SurfaceIntersection::Empty => continue,
            // Coincident carriers are tangential contact: fail closed.
            SurfaceIntersection::Coincident => {
                return Err(AnalyticBooleanError::DomainUnsupported);
            }
            SurfaceIntersection::Curves(curves) => curves,
        };
        let other_region = face_region(other, &other_face.value)?;
        for curve in curves {
            // Clip the carrier curve to the other face's own extent, in the
            // other face's parameter space, then re-express the kept pieces
            // in this face's parameter space.
            let Some(other_chords) = curve_chords(&other_face.value.surface, curve) else {
                return Err(AnalyticBooleanError::DomainUnsupported);
            };
            for chord in other_chords {
                let clipped = chord_region_pieces(chord, &other_region, precision)
                    .map_err(|_| AnalyticBooleanError::DomainUnsupported)?;
                for piece in clipped {
                    pieces.push(
                        reparameterize(&other_face.value.surface, piece, &face.surface)
                            .ok_or(AnalyticBooleanError::DomainUnsupported)?,
                    );
                }
            }
        }
    }
    if pieces.is_empty() {
        return Ok(Vec::new());
    }
    let loops = match face.surface {
        Surface::Cylinder(cylinder) => {
            close_periodic_sections(pieces, own_region, &cylinder, other, precision)?
        }
        _ => chain_welded_segments(pieces, precision)
            .map_err(|_| AnalyticBooleanError::DomainUnsupported)?,
    };
    nest_section_loops(loops)
}

/// Closes the sections on a periodic face.
///
/// A plane through a whole cylinder leaves a trace that runs the full turn
/// of the azimuth and never meets itself in parameter space: it enters the
/// face's window at one seam and leaves at the other, one period on. Such a
/// chain is closed round the outside of the window, on whichever side the
/// other solid's material lies, so the face's 2D Boolean sees a region
/// rather than a cut line. Chains that already close are kept as they are.
fn close_periodic_sections(
    pieces: Vec<Segment>,
    region: &[Vec<Segment>],
    cylinder: &Cylinder,
    other: &Topology,
    precision: PrecisionPolicy,
) -> Result<Vec<Vec<Segment>>, AnalyticBooleanError> {
    let tau = std::f64::consts::TAU;
    let (u_min, u_max, v_min, v_max) = region
        .iter()
        .flatten()
        .flat_map(|segment| [segment.start(), segment.end()])
        .fold(
            (
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ),
            |(a, b, c, d), point| {
                (
                    a.min(point.x),
                    b.max(point.x),
                    c.min(point.y),
                    d.max(point.y),
                )
            },
        );
    if !u_min.is_finite() || !u_max.is_finite() {
        return Err(AnalyticBooleanError::DomainUnsupported);
    }
    // Every piece into the face's own angular window, by whole turns, and
    // pieces that only touch the window at a seam, or never enter it, are
    // left out: they belong to the face across the seam, or to a far cap's
    // section of the same carrier.
    let scale_hint = pieces
        .iter()
        .flat_map(|segment| [segment.start(), segment.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let margin = precision.linear_agreement.max(1.0e-12) * scale_hint * 128.0;
    let pieces: Vec<Segment> = pieces
        .into_iter()
        .map(|piece| {
            let middle = 0.5 * (piece.start().x + piece.end().x);
            let turns = ((u_min - middle) / tau).ceil();
            if turns == 0.0 {
                piece
            } else {
                piece.translated(Point2::new(-turns * tau, 0.0))
            }
        })
        .filter(|piece| {
            (1..8).any(|step| {
                let point = piece.point_at(f64::from(step) / 8.0);
                point.x > u_min + margin
                    && point.x < u_max - margin
                    && point.y >= v_min - margin
                    && point.y <= v_max + margin
            })
        })
        .collect();
    if pieces.is_empty() {
        return Ok(Vec::new());
    }
    // Weld endpoints and read off closed loops and open chains.
    let scale = pieces
        .iter()
        .flat_map(|segment| [segment.start(), segment.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let weld = precision.linear_agreement.max(1.0e-12) * scale * 32.0;
    let mut representatives: Vec<Point2> = Vec::new();
    let mut canonical = |point: Point2| -> Point2 {
        if let Some(found) = representatives
            .iter()
            .find(|candidate| (candidate.x - point.x).hypot(candidate.y - point.y) <= weld)
        {
            return *found;
        }
        representatives.push(point);
        point
    };
    let welded: Vec<Segment> = pieces
        .into_iter()
        .map(|piece| {
            let start = canonical(piece.start());
            let end = canonical(piece.end());
            piece.with_endpoints(start, end)
        })
        .collect();
    let key = |point: Point2| (point.x.to_bits(), point.y.to_bits());
    let mut touching: std::collections::BTreeMap<(u64, u64), Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, segment) in welded.iter().enumerate() {
        touching
            .entry(key(segment.start()))
            .or_default()
            .push(index);
        touching.entry(key(segment.end())).or_default().push(index);
    }
    if touching.values().any(|ends| ends.len() > 2) {
        return Err(AnalyticBooleanError::DomainUnsupported);
    }
    let mut used = vec![false; welded.len()];
    let mut loops: Vec<Vec<Segment>> = Vec::new();
    let mut open: Vec<Vec<Segment>> = Vec::new();
    // Open chains start at a degree-one end; closed ones anywhere.
    let mut order: Vec<usize> = (0..welded.len())
        .filter(|index| {
            touching[&key(welded[*index].start())].len() == 1
                || touching[&key(welded[*index].end())].len() == 1
        })
        .collect();
    order.extend(0..welded.len());
    for start in order {
        if used[start] {
            continue;
        }
        // Start at a free end, if the piece has one, and walk away from it.
        let start_free = touching[&key(welded[start].start())].len() == 1;
        let end_free = touching[&key(welded[start].end())].len() == 1;
        let mut forward = start_free || !end_free;
        let mut chain = Vec::new();
        let mut cursor = start;
        loop {
            used[cursor] = true;
            let oriented = if forward {
                welded[cursor]
            } else {
                welded[cursor].reversed()
            };
            let arrival = key(oriented.end());
            chain.push(oriented);
            let next = touching[&arrival]
                .iter()
                .copied()
                .find(|candidate| !used[*candidate]);
            let Some(next) = next else { break };
            forward = key(welded[next].start()) == arrival;
            cursor = next;
        }
        let first = chain[0].start();
        let last = chain[chain.len() - 1].end();
        if key(first) == key(last) {
            loops.push(chain);
            continue;
        }
        // Every open chain runs left to right, and must reach from one seam
        // of the window to the other: round the whole period, or across a
        // face that is one part of it.
        if first.x > last.x {
            chain = chain.iter().rev().map(|piece| piece.reversed()).collect();
        }
        let first = chain[0].start();
        let last = chain[chain.len() - 1].end();
        if first.x > u_min + margin || last.x < u_max - margin {
            return Err(AnalyticBooleanError::DomainUnsupported);
        }
        open.push(chain);
    }
    if open.is_empty() {
        return Ok(loops);
    }
    // The open chains part the window into bands. The other solid's
    // material fills every other band, starting on whichever side of the
    // lowest chain a probe says it lies; each band closes round the
    // outside of the window, past the seams, so nothing it adds lies on
    // the face's own edges.
    // Each chain reaches past both seams by a clear margin, along its own
    // carrier, so the connectors the bands add never touch the face.
    let reach = 0.05;
    let left = u_min - reach;
    let right = u_max + reach;
    let extend = |segment: Segment, to_x: f64, at_start: bool| -> Option<Segment> {
        match segment {
            Segment::Harmonic {
                mean,
                amplitude,
                phase,
                start,
                end,
            } => {
                let section = CylinderSectionHarmonic {
                    cylinder: *cylinder,
                    mean,
                    amplitude,
                    phase,
                };
                Some(if at_start {
                    section.segment(to_x, end.x)
                } else {
                    section.segment(start.x, to_x)
                })
            }
            Segment::Line { start, end } if (end.y - start.y).abs() <= weld => Some(if at_start {
                Segment::Line {
                    start: Point2::new(to_x, start.y),
                    end,
                }
            } else {
                Segment::Line {
                    start,
                    end: Point2::new(to_x, end.y),
                }
            }),
            _ => None,
        }
    };
    for chain in &mut open {
        let count = chain.len();
        if chain[0].start().x > left {
            chain[0] =
                extend(chain[0], left, true).ok_or(AnalyticBooleanError::DomainUnsupported)?;
        }
        if chain[count - 1].end().x < right {
            chain[count - 1] = extend(chain[count - 1], right, false)
                .ok_or(AnalyticBooleanError::DomainUnsupported)?;
        }
    }
    let mean_height = |chain: &[Segment]| -> f64 {
        let samples = chain
            .iter()
            .flat_map(|piece| (0..=4).map(move |step| piece.point_at(f64::from(step) / 4.0).y))
            .collect::<Vec<_>>();
        samples.iter().sum::<f64>() / samples.len() as f64
    };
    open.sort_by(|a, b| mean_height(a).total_cmp(&mean_height(b)));
    let lowest = &open[0];
    let probe = lowest[lowest.len() / 2].point_at(0.5);
    let step = (v_max - v_min).max(1.0) * 1.0e-3;
    let above_lowest = point_in_solid(
        other,
        cylinder.evaluate(Point2::new(probe.x, probe.y + step)),
    )
    .ok_or(AnalyticBooleanError::DomainUnsupported)?;
    let left = open
        .iter()
        .map(|chain| chain[0].start().x)
        .fold(f64::INFINITY, f64::min);
    let right = open
        .iter()
        .map(|chain| chain[chain.len() - 1].end().x)
        .fold(f64::NEG_INFINITY, f64::max);
    let far_below = v_min - (v_max - v_min).max(1.0);
    let far_above = v_max + (v_max - v_min).max(1.0);
    let level = |height: f64| -> Vec<Segment> {
        vec![Segment::Line {
            start: Point2::new(left, height),
            end: Point2::new(right, height),
        }]
    };
    let mut levels: Vec<Vec<Segment>> = Vec::new();
    if !above_lowest {
        levels.push(level(far_below));
    }
    levels.extend(open);
    if levels.len() % 2 == 1 {
        levels.push(level(far_above));
    }
    for pair in levels.chunks(2) {
        let lower = &pair[0];
        let upper = &pair[1];
        let mut band = lower.clone();
        let lower_end = lower[lower.len() - 1].end();
        let upper_end = upper[upper.len() - 1].end();
        band.push(Segment::Line {
            start: lower_end,
            end: upper_end,
        });
        band.extend(upper.iter().rev().map(|piece| piece.reversed()));
        let upper_start = upper[0].start();
        let lower_start = lower[0].start();
        band.push(Segment::Line {
            start: upper_start,
            end: lower_start,
        });
        loops.push(band);
    }
    Ok(loops)
}

/// Groups chained section loops into regions by even-odd depth.
fn nest_section_loops(
    loops: Vec<Vec<Segment>>,
) -> Result<Vec<ProfileRegion>, AnalyticBooleanError> {
    let area = |segments: &[Segment]| -> f64 {
        segments
            .iter()
            .map(|segment| segment.signed_area_contribution())
            .sum()
    };
    let sample = |segments: &[Segment]| segments[0].start();
    let inside = |point: Point2, segments: &[Segment]| {
        let wrapped = crate::analytic_extrusion::AnalyticLoop {
            segments: segments.to_vec(),
            signed_area: 0.0,
        };
        crate::analytic_extrusion::point_inside_loop(point, &wrapped)
    };
    let depths: Vec<usize> = (0..loops.len())
        .map(|index| {
            loops
                .iter()
                .enumerate()
                .filter(|(other, segments)| {
                    *other != index && inside(sample(&loops[index]), segments)
                })
                .count()
        })
        .collect();
    let mut regions: Vec<(usize, ProfileRegion)> = Vec::new();
    for (index, chain) in loops.iter().enumerate() {
        if depths[index].is_multiple_of(2) {
            let outer = if area(chain) > 0.0 {
                chain.clone()
            } else {
                reverse_chain(chain)
            };
            regions.push((
                index,
                ProfileRegion {
                    outer,
                    holes: Vec::new(),
                },
            ));
        }
    }
    for (index, chain) in loops.iter().enumerate() {
        if !depths[index].is_multiple_of(2) {
            let hole = if area(chain) < 0.0 {
                chain.clone()
            } else {
                reverse_chain(chain)
            };
            let parent = regions
                .iter_mut()
                .filter(|(outer_index, _)| {
                    depths[*outer_index] + 1 == depths[index]
                        && inside(sample(chain), &loops[*outer_index])
                })
                .min_by(|(left, _), (right, _)| {
                    area(&loops[*left])
                        .abs()
                        .total_cmp(&area(&loops[*right]).abs())
                });
            let Some((_, region)) = parent else {
                return Err(AnalyticBooleanError::DomainUnsupported);
            };
            region.holes.push(hole);
        }
    }
    Ok(regions.into_iter().map(|(_, region)| region).collect())
}

fn reverse_chain(segments: &[Segment]) -> Vec<Segment> {
    segments
        .iter()
        .rev()
        .map(|segment| segment.reversed())
        .collect()
}

/// An intersection curve as chords in the given surface's parameter space.
///
/// Lines map to long line chords; circles map to two semicircle arcs on a
/// plane, or to horizontal ring chords on a cylinder. `None` marks a curve
/// the surface's parameter space cannot carry with lines and arcs — a helix
/// from a skewed line, for instance — which refuses the operation.
fn curve_chords(surface: &Surface, curve: IntersectionCurve) -> Option<Vec<Segment>> {
    const SPAN: f64 = 1.0e6;
    match (surface, curve) {
        (Surface::Plane(plane), IntersectionCurve::Line { origin, direction }) => {
            let local = |point: Point3| {
                Point2::new(
                    (point - plane.origin).dot(plane.u),
                    (point - plane.origin).dot(plane.v),
                )
            };
            let start = local(origin + direction * -SPAN);
            let end = local(origin + direction * SPAN);
            Some(vec![Segment::Line { start, end }])
        }
        (
            Surface::Plane(plane),
            IntersectionCurve::Circle {
                center,
                u,
                v,
                radius,
            },
        ) => {
            // The circle lies in this plane; express it in plane coordinates
            // as two exact semicircles.
            let local_center = Point2::new(
                (center - plane.origin).dot(plane.u),
                (center - plane.origin).dot(plane.v),
            );
            let u2 = Point2::new(u.dot(plane.u), u.dot(plane.v));
            let start_angle = u2.y.atan2(u2.x);
            let orientation = if u.cross(v).dot(plane.normal) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let point_at = |angle: f64| {
                Point2::new(
                    radius.mul_add(angle.cos(), local_center.x),
                    radius.mul_add(angle.sin(), local_center.y),
                )
            };
            let half = std::f64::consts::PI * orientation;
            let a = point_at(start_angle);
            let b = point_at(start_angle + half);
            Some(vec![
                Segment::Arc {
                    center: local_center,
                    start: a,
                    end: b,
                    radius,
                    start_angle,
                    sweep: half,
                },
                Segment::Arc {
                    center: local_center,
                    start: b,
                    end: a,
                    radius,
                    start_angle: start_angle + half,
                    sweep: half,
                },
            ])
        }
        (
            Surface::Plane(plane),
            IntersectionCurve::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
                seam_angle,
                ..
            },
        ) => {
            // The ellipse lies in this plane; express it in plane
            // coordinates as two exact half-ellipses, parted at the
            // cylinder's azimuths zero and π so the cylinder's own chords
            // subdivide the same way.
            let local = |point: Point3| {
                Point2::new(
                    (point - plane.origin).dot(plane.u),
                    (point - plane.origin).dot(plane.v),
                )
            };
            let local_center = local(center);
            let u2 = Point2::new(u.dot(plane.u), u.dot(plane.v));
            let orientation = if u.cross(v).dot(plane.normal) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let half = std::f64::consts::PI * orientation;
            let carrier = |start_angle: f64| Segment::Ellipse {
                center: local_center,
                u: u2,
                major: major_radius,
                minor: minor_radius,
                start: Point2::new(0.0, 0.0),
                end: Point2::new(0.0, 0.0),
                start_angle,
                sweep: half,
            };
            let seam = orientation * seam_angle;
            let first = carrier(seam);
            let second = carrier(seam + half);
            let a = first.point_at(0.0);
            let b = first.point_at(1.0);
            Some(vec![
                first.with_endpoints(a, b),
                second.with_endpoints(b, a),
            ])
        }
        (
            Surface::Cylinder(cylinder),
            IntersectionCurve::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
                ..
            },
        ) => {
            // The plane section of this cylinder, as its harmonic trace.
            // Cover every angular branch a bounded face domain might use,
            // parted at every multiple of π, where the plane parts its
            // half-ellipses.
            let harmonic =
                cylinder_section_harmonic(cylinder, center, u, v, major_radius, minor_radius)?;
            let pi = std::f64::consts::PI;
            Some(
                [(-2.0 * pi, -pi), (-pi, 0.0), (0.0, pi), (pi, 2.0 * pi)]
                    .into_iter()
                    .map(|(from, to)| harmonic.segment(from, to))
                    .collect(),
            )
        }
        (Surface::Cylinder(cylinder), IntersectionCurve::Line { origin, direction }) => {
            // A generator: constant angle, varying height.
            let axis = cylinder.axis / cylinder.axis.length();
            if direction.cross(axis).length() > 1.0e-9 {
                return None;
            }
            let offset = origin - cylinder.origin;
            let radial = offset - axis * offset.dot(axis);
            let angle = radial
                .dot(cylinder.radial_v)
                .atan2(radial.dot(cylinder.radial_u));
            let u = cylinder.angular_sign * angle;
            let base = offset.dot(axis);
            let along = direction.dot(axis);
            Some(vec![Segment::Line {
                start: Point2::new(u, along.mul_add(-SPAN, base)),
                end: Point2::new(u, along.mul_add(SPAN, base)),
            }])
        }
        (
            Surface::Cylinder(cylinder),
            IntersectionCurve::Circle {
                center,
                u: _,
                v: _,
                radius,
            },
        ) => {
            // A ring: constant height, full angular turn. Only rings on this
            // cylinder's own carrier are expressible.
            let axis = cylinder.axis / cylinder.axis.length();
            if (radius - cylinder.radius).abs() > 1.0e-9 {
                return None;
            }
            let offset = center - cylinder.origin;
            if (offset - axis * offset.dot(axis)).length() > 1.0e-9 {
                return None;
            }
            let height = offset.dot(axis);
            let tau = std::f64::consts::TAU;
            // Cover every angular branch a bounded face domain might use.
            Some(vec![
                Segment::Line {
                    start: Point2::new(-tau, height),
                    end: Point2::new(0.0, height),
                },
                Segment::Line {
                    start: Point2::new(0.0, height),
                    end: Point2::new(tau, height),
                },
            ])
        }
        _ => None,
    }
}

/// Re-expresses a chord piece from one face's parameter space into another's
/// through world coordinates.
fn reparameterize(from: &Surface, piece: Segment, to: &Surface) -> Option<Segment> {
    let world = |point: Point2| -> Option<Point3> {
        match from {
            Surface::Plane(plane) => Some(plane.evaluate(point)),
            Surface::Cylinder(cylinder) => Some(cylinder.evaluate(point)),
            _ => None,
        }
    };
    match to {
        Surface::Plane(plane) => {
            let local = |point: Point3| {
                Point2::new(
                    (point - plane.origin).dot(plane.u),
                    (point - plane.origin).dot(plane.v),
                )
            };
            match piece {
                Segment::Line { start, end } => {
                    // A ring chord on a cylinder — constant height — is a
                    // circular arc in space, and in this plane; only a
                    // generator maps to a line.
                    if let Surface::Cylinder(cylinder) = from
                        && (start.y - end.y).abs() <= 1.0e-12
                        && (start.x - end.x).abs() > 1.0e-12
                    {
                        let axis = cylinder.axis / cylinder.axis.length();
                        let local_center = local(cylinder.origin + axis * start.y);
                        let local_start = local(cylinder.evaluate(start));
                        let orientation = cylinder
                            .radial_u
                            .cross(cylinder.radial_v)
                            .dot(plane.normal)
                            .signum();
                        return Some(Segment::Arc {
                            center: local_center,
                            start: local_start,
                            end: local(cylinder.evaluate(end)),
                            radius: cylinder.radius,
                            start_angle: (local_start.y - local_center.y)
                                .atan2(local_start.x - local_center.x),
                            sweep: orientation * cylinder.angular_sign * (end.x - start.x),
                        });
                    }
                    Some(Segment::Line {
                        start: local(world(start)?),
                        end: local(world(end)?),
                    })
                }
                Segment::Arc {
                    center,
                    start,
                    end,
                    radius,
                    sweep,
                    ..
                } => {
                    // An arc on the source face is a circular arc in space;
                    // in the destination plane it stays circular only when
                    // that plane contains it, which the intersection matrix
                    // guarantees for in-matrix pairs.
                    let local_center = local(world(center)?);
                    let local_start = local(world(start)?);
                    let start_angle =
                        (local_start.y - local_center.y).atan2(local_start.x - local_center.x);
                    Some(Segment::Arc {
                        center: local_center,
                        start: local_start,
                        end: local(world(end)?),
                        radius,
                        start_angle,
                        sweep,
                    })
                }
                Segment::Harmonic {
                    mean,
                    amplitude,
                    phase,
                    start,
                    end,
                } => {
                    // The trace on the source cylinder is an ellipse in
                    // space; it lies in this plane, where it is an
                    // elliptical chord with the same parameter.
                    let Surface::Cylinder(cylinder) = from else {
                        return None;
                    };
                    let section = CylinderSectionHarmonic {
                        cylinder: *cylinder,
                        mean,
                        amplitude,
                        phase,
                    };
                    let (center, major_axis, minor_axis, major, minor) = section.ellipse()?;
                    let local_center = local(center);
                    let u2 = Point2::new(major_axis.dot(plane.u), major_axis.dot(plane.v));
                    let orientation = if major_axis.cross(minor_axis).dot(plane.normal) >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    };
                    let angle_at = |azimuth: f64| orientation * section.angle_at(azimuth);
                    Some(Segment::Ellipse {
                        center: local_center,
                        u: u2,
                        major,
                        minor,
                        start: local(world(start)?),
                        end: local(world(end)?),
                        start_angle: angle_at(start.x),
                        sweep: angle_at(end.x) - angle_at(start.x),
                    })
                }
                Segment::Ellipse { .. } => None,
            }
        }
        Surface::Cylinder(cylinder) => {
            let axis = cylinder.axis / cylinder.axis.length();
            let local = |point: Point3| -> Point2 {
                let offset = point - cylinder.origin;
                let height = offset.dot(axis);
                let radial = offset - axis * height;
                let angle = radial
                    .dot(cylinder.radial_v)
                    .atan2(radial.dot(cylinder.radial_u));
                Point2::new(cylinder.angular_sign * angle, height)
            };
            match piece {
                // A straight piece on the source face lands on a cylinder
                // only as a generator (constant angle) or a ring chord
                // (constant height); both stay lines in parameter space.
                Segment::Line { start, end } => {
                    let a = local(world(start)?);
                    let b = local(world(end)?);
                    if (a.x - b.x).abs() <= 1.0e-9 || (a.y - b.y).abs() <= 1.0e-9 {
                        Some(Segment::Line { start: a, end: b })
                    } else {
                        None
                    }
                }
                arc @ Segment::Arc { start, end, .. } => {
                    // A circular arc lies on the cylinder only as a ring arc:
                    // constant height, linear in angle.
                    let a = local(world(start)?);
                    let b = local(world(end)?);
                    if (a.y - b.y).abs() > 1.0e-9 {
                        return None;
                    }
                    // The angular branch is the one the arc's own midpoint
                    // lies on: a half turn is ambiguous by length alone, and
                    // the two halves of a ring used to fold onto one branch.
                    let tau = std::f64::consts::TAU;
                    let nearest =
                        |value: f64, target: f64| value + ((target - value) / tau).round() * tau;
                    let middle = nearest(local(world(arc.point_at(0.5))?).x, a.x);
                    let bx = nearest(b.x, a.x + 2.0 * (middle - a.x));
                    Some(Segment::Line {
                        start: a,
                        end: Point2::new(bx, b.y),
                    })
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
                    // The elliptical chord on the source plane is this
                    // cylinder's section: its harmonic trace, walked over
                    // the same parameter span.
                    let Surface::Plane(source) = from else {
                        return None;
                    };
                    let center3 = source.evaluate(center);
                    let major_axis = source.u * u.x + source.v * u.y;
                    let minor_axis = source.u * -u.y + source.v * u.x;
                    let section = cylinder_section_harmonic(
                        cylinder, center3, major_axis, minor_axis, major, minor,
                    )?;
                    // The ellipse parameter is the azimuth less the phase
                    // (up to the parameterization sign), so the span maps
                    // linearly.
                    let sign = section.angle_sign(major_axis, minor_axis);
                    let from_azimuth = section.azimuth_at(start_angle * sign);
                    let to_azimuth = section.azimuth_at((start_angle + sweep) * sign);
                    Some(section.segment(from_azimuth, to_azimuth))
                }
                Segment::Harmonic { .. } => None,
            }
        }
        _ => None,
    }
}

/// A plane section of a cylinder as the trace it leaves in the cylinder's
/// parameter space, `v(u) = mean + amplitude·cos(u − phase)`, together with
/// the cylinder it lies on so the trace and the ellipse in space convert
/// both ways.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CylinderSectionHarmonic {
    pub(crate) cylinder: Cylinder,
    pub(crate) mean: f64,
    pub(crate) amplitude: f64,
    pub(crate) phase: f64,
}

impl CylinderSectionHarmonic {
    fn height(self, azimuth: f64) -> f64 {
        self.mean + self.amplitude * (azimuth - self.phase).cos()
    }

    /// The chord from one azimuth to another.
    fn segment(self, from: f64, to: f64) -> Segment {
        Segment::Harmonic {
            mean: self.mean,
            amplitude: self.amplitude,
            phase: self.phase,
            start: Point2::new(from, self.height(from)),
            end: Point2::new(to, self.height(to)),
        }
    }

    /// The ellipse the trace draws in space: `(center, u, v, major, minor)`
    /// with `u` up the slant and `v` round the cylinder, parameterized so
    /// that the ellipse angle is `azimuth − phase`, up to the cylinder's
    /// angular sign.
    pub(crate) fn ellipse(self) -> Option<(Point3, Vector3, Vector3, f64, f64)> {
        let cylinder = self.cylinder;
        let axis_length = cylinder.axis.length();
        if axis_length <= f64::EPSILON {
            return None;
        }
        let axis = cylinder.axis / axis_length;
        // The trace's crest sits at azimuth `phase`, i.e. at parameter
        // `phase`, whose physical angle is `angular_sign · phase`.
        let crest = cylinder.angular_sign * self.phase;
        let radial_crest = cylinder.radial_u * crest.cos() + cylinder.radial_v * crest.sin();
        let radial_quarter = cylinder.radial_u * -crest.sin() + cylinder.radial_v * crest.cos();
        let major_vector = radial_crest * cylinder.radius + axis * self.amplitude;
        let major = major_vector.length();
        if major <= f64::EPSILON {
            return None;
        }
        Some((
            cylinder.origin + axis * self.mean,
            major_vector / major,
            radial_quarter * cylinder.angular_sign,
            major,
            cylinder.radius,
        ))
    }

    /// The ellipse angle at a parameter azimuth: the physical angle past
    /// the crest, which the frame above measures with the angular sign
    /// folded into `v`.
    pub(crate) fn angle_at(self, azimuth: f64) -> f64 {
        azimuth - self.phase
    }

    /// The azimuth at an ellipse angle, the inverse of `angle_at`.
    fn azimuth_at(self, angle: f64) -> f64 {
        angle + self.phase
    }

    /// Whether a given major/minor frame for the same ellipse runs its
    /// angle the same way as `ellipse()`'s frame (`1`) or backwards (`−1`).
    fn angle_sign(self, major_axis: Vector3, minor_axis: Vector3) -> f64 {
        match self.ellipse() {
            Some((_, own_major, own_minor, _, _)) => {
                let same_major = own_major.dot(major_axis) >= 0.0;
                let same_minor = own_minor.dot(minor_axis) >= 0.0;
                if same_major == same_minor { 1.0 } else { -1.0 }
            }
            None => 1.0,
        }
    }
}

/// The harmonic trace on `cylinder` of the ellipse
/// `center + major·cos(t)·u + minor·sin(t)·v`, or `None` when the ellipse
/// is not a plane section of this cylinder: its centre off the axis, its
/// minor radius not the cylinder's, or its minor axis not around the
/// cylinder.
fn cylinder_section_harmonic(
    cylinder: &Cylinder,
    center: Point3,
    u: Vector3,
    v: Vector3,
    major_radius: f64,
    minor_radius: f64,
) -> Option<CylinderSectionHarmonic> {
    let axis_length = cylinder.axis.length();
    if axis_length <= f64::EPSILON {
        return None;
    }
    let axis = cylinder.axis / axis_length;
    let scale = cylinder.radius.max(major_radius).max(1.0);
    let tolerance = 1.0e-9 * scale;
    let offset = center - cylinder.origin;
    let mean = offset.dot(axis);
    if (offset - axis * mean).length() > tolerance
        || (minor_radius - cylinder.radius).abs() > tolerance
        || v.dot(axis).abs() > 1.0e-9
    {
        return None;
    }
    // Up the slant: the major axis's axial reach is the amplitude, and its
    // radial part points at the crest.
    let amplitude = major_radius * u.dot(axis);
    let radial = u - axis * u.dot(axis);
    let radial_length = radial.length();
    if radial_length <= 1.0e-12 {
        return None;
    }
    if (radial_length * major_radius - cylinder.radius).abs() > tolerance {
        return None;
    }
    let crest = radial
        .dot(cylinder.radial_v)
        .atan2(radial.dot(cylinder.radial_u));
    Some(CylinderSectionHarmonic {
        cylinder: *cylinder,
        mean,
        amplitude,
        phase: cylinder.angular_sign * crest,
    })
}

/// Whether a robust interior sample of the face lies inside the other solid.
fn face_sample_inside(
    _own: &Topology,
    face: &Face,
    region: &[Vec<Segment>],
    other: &Topology,
    precision: PrecisionPolicy,
) -> Result<bool, AnalyticBooleanError> {
    // A deterministic interior point: cast a horizontal chord through the
    // region at a boundary-free height and take an inside interval midpoint.
    let anchor = region[0][0].start();
    let mut interior = None;
    for jitter in [0.318_412_357, 0.239_558_53, 0.077_215_664, 0.412_339_2] {
        let bounds = region
            .iter()
            .flatten()
            .flat_map(|segment| [segment.start(), segment.end()])
            .fold(
                (
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                ),
                |acc, p| {
                    (
                        acc.0.min(p.x),
                        acc.1.max(p.x),
                        acc.2.min(p.y),
                        acc.3.max(p.y),
                    )
                },
            );
        let y = (bounds.3 - bounds.2).mul_add(jitter, anchor.y * 1.0e-12 + bounds.2);
        let chord = Segment::Line {
            start: Point2::new(bounds.0 - (bounds.1 - bounds.0) - 1.0, y),
            end: Point2::new(bounds.1 + (bounds.1 - bounds.0) + 1.0, y),
        };
        if let Ok(pieces) = chord_region_pieces(chord, region, precision)
            && let Some(piece) = pieces.first()
        {
            let mid = Point2::new(
                (piece.start().x + piece.end().x) / 2.0,
                (piece.start().y + piece.end().y) / 2.0,
            );
            interior = Some(mid);
            break;
        }
    }
    let Some(sample_2d) = interior else {
        return Err(AnalyticBooleanError::DomainUnsupported);
    };
    let sample = match face.surface {
        Surface::Plane(plane) => plane.evaluate(sample_2d),
        Surface::Cylinder(cylinder) => cylinder.evaluate(sample_2d),
        _ => return Err(AnalyticBooleanError::DomainUnsupported),
    };
    point_in_solid(other, sample).ok_or(AnalyticBooleanError::DomainUnsupported)
}

/// Exact parity ray cast against a whole topology, retrying awkward
/// directions before giving up.
fn point_in_solid(topology: &Topology, point: Point3) -> Option<bool> {
    for direction in ray_directions() {
        let mut crossings = 0_usize;
        let mut degenerate = false;
        for face in &topology.faces {
            match ray_face_crossings(topology, &face.value, point, direction) {
                Some(count) => crossings += count,
                None => {
                    degenerate = true;
                    break;
                }
            }
        }
        if !degenerate {
            return Some(crossings % 2 == 1);
        }
    }
    None
}

/// Flips a kept piece's material side: the surface mirrors (a plane swaps u
/// and v, a cylinder negates its angular sign) and every loop re-mirrors to
/// stay positively wound — reverse the surface, never the loop.
fn mirror_sew_face(piece: SewFace) -> Result<SewFace, AnalyticBooleanError> {
    let (surface, mirror): (Surface, fn(Point2) -> Point2) = match piece.surface {
        Surface::Plane(plane) => (
            Surface::Plane(Plane::new(plane.origin, plane.v, plane.u)),
            |point: Point2| Point2::new(point.y, point.x),
        ),
        Surface::Cylinder(cylinder) => (
            Surface::Cylinder(Cylinder {
                angular_sign: -cylinder.angular_sign,
                ..cylinder
            }),
            |point: Point2| Point2::new(-point.x, point.y),
        ),
        _ => return Err(AnalyticBooleanError::DomainUnsupported),
    };
    let loops = piece
        .loops
        .iter()
        .map(|segments| {
            segments
                .iter()
                .rev()
                .map(|segment| mirror_segment(*segment, mirror))
                .collect()
        })
        .collect();
    Ok(SewFace {
        surface,
        loops,
        role: piece.role,
    })
}

fn mirror_segment(segment: Segment, mirror: fn(Point2) -> Point2) -> Segment {
    match segment {
        Segment::Line { start, end } => Segment::Line {
            start: mirror(end),
            end: mirror(start),
        },
        Segment::Arc {
            center,
            start,
            end,
            radius,
            start_angle,
            sweep,
        } => {
            let new_center = mirror(center);
            let new_start = mirror(end);
            // The mirrored arc runs from the old end backwards; its start
            // angle re-derives from the mirrored geometry and the sweep
            // keeps its magnitude with the mirrored plane's handedness.
            let new_start_angle = (new_start.y - new_center.y).atan2(new_start.x - new_center.x);
            let _ = start_angle;
            let _ = start;
            Segment::Arc {
                center: new_center,
                start: new_start,
                end: mirror(segment.start()),
                radius,
                start_angle: new_start_angle,
                sweep,
            }
        }
        Segment::Ellipse {
            center,
            u,
            major,
            minor,
            start,
            end,
            start_angle,
            sweep,
        } => {
            // A reflection sends the frame's left quarter turn to a right
            // one, so the mirrored ellipse in standard form runs its
            // parameter backwards: the walk from the old end to the old
            // start covers `−(start + sweep)` onward by `sweep`.
            let mirrored_u = mirror(u);
            let origin = mirror(Point2::new(0.0, 0.0));
            Segment::Ellipse {
                center: mirror(center),
                u: Point2::new(mirrored_u.x - origin.x, mirrored_u.y - origin.y),
                major,
                minor,
                start: mirror(end),
                end: mirror(start),
                start_angle: -(start_angle + sweep),
                sweep,
            }
        }
        Segment::Harmonic {
            mean,
            amplitude,
            phase,
            start,
            end,
        } => {
            // Only a cylinder carries a harmonic, and its mirror negates the
            // azimuth: `cos(−u − φ) = cos(u + φ)`.
            Segment::Harmonic {
                mean,
                amplitude,
                phase: -phase,
                start: mirror(end),
                end: mirror(start),
            }
        }
    }
}

/// Guard: the engine only carries planes and cylinders today.
pub(crate) fn operands_in_engine_vocabulary(target: &Topology, tool: &Topology) -> bool {
    target
        .faces
        .iter()
        .chain(&tool.faces)
        .all(|face| matches!(face.value.surface, Surface::Plane(_) | Surface::Cylinder(_)))
}

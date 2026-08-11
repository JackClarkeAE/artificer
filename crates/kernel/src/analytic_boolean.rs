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
//! exact; outside it — an oblique plane through a cylinder, a blended
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
use crate::topology::{Cylinder, Face, Plane, Point2, Point3, Surface, Topology};

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
        let section = section_on_face(&face.value, other, precision)?;
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
            crate::analytic_extrusion::topology_loop_segments(topology, loop_key)
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
    let loops = chain_welded_segments(pieces, precision)
        .map_err(|_| AnalyticBooleanError::DomainUnsupported)?;
    nest_section_loops(loops)
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
        .map(|segment| match *segment {
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
        })
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
                Segment::Line { start, end } => Some(Segment::Line {
                    start: local(world(start)?),
                    end: local(world(end)?),
                }),
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
                Segment::Arc { start, end, .. } => {
                    // A circular arc lies on the cylinder only as a ring arc:
                    // constant height, linear in angle.
                    let a = local(world(start)?);
                    let b = local(world(end)?);
                    if (a.y - b.y).abs() > 1.0e-9 {
                        return None;
                    }
                    // Choose the angular branch that keeps the chord short.
                    let tau = std::f64::consts::TAU;
                    let mut bx = b.x;
                    while bx - a.x > std::f64::consts::PI {
                        bx -= tau;
                    }
                    while a.x - bx > std::f64::consts::PI {
                        bx += tau;
                    }
                    Some(Segment::Line {
                        start: a,
                        end: Point2::new(bx, b.y),
                    })
                }
            }
        }
        _ => None,
    }
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

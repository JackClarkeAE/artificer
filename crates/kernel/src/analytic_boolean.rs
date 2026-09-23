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
use crate::cylinder_trace::{CylinderTrace, cylinder_local, same_carrier};
use crate::profile_boolean::{
    ProfileBooleanError, ProfileRegion, chain_welded_segments, chord_region_pieces,
    profile_boolean_multi, split_at_mutual_crossings, weld_aligned, welded,
};
use crate::sew::{SewError, SewFace, ray_directions, ray_face_crossings, sew_shells};
use crate::surface_intersection::{IntersectionCurve, SurfaceIntersection, intersect};
use crate::topology::{Cylinder, Face, Plane, Point2, Point3, Surface, Topology, Vector3};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AnalyticBooleanError {
    /// The operand pair leaves the engine's domain: a tangential or
    /// coincident contact it cannot classify, or a face class the sewing
    /// vocabulary cannot carry.
    DomainUnsupported,
    /// Two faces that could meet lie on carriers whose intersection is
    /// outside the curve vocabulary — two bores of unequal radius crossing,
    /// say. The pair is named so the refusal can be; boxed, because a
    /// surface is large and every other variant is a word.
    CarrierPair(Box<[Surface; 2]>),
    /// The operation succeeded and produced no material.
    EmptyResult,
    /// A section that includes the curve two cylinders share (ADR 0047) did
    /// not close on a cylinder face: one of its curves ends inside the
    /// window, or an edge generator is crossed an odd number of times. Both
    /// mean a face of the other solid did not report the piece that
    /// continues a curve, and the closure refuses rather than guess it.
    TraceUnclosed,
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
        let overlays = coincident_overlays(&face.value, &region, other, precision)?;
        let own_region = ProfileRegion {
            outer: region[0].clone(),
            holes: region[1..].to_vec(),
        };

        // Where the other solid has a face on this same carrier, the two
        // overlap in area, and no sample can say which side of a skin the
        // skin itself is on. That overlap is answered by the operand table
        // (below); what is classified in the ordinary way is the rest of the
        // face, with the overlaps taken out of it first.
        let mut rest = vec![own_region.clone()];
        for overlay in &overlays {
            let mut remaining = Vec::new();
            for piece in &rest {
                match profile_boolean_multi(
                    std::slice::from_ref(piece),
                    std::slice::from_ref(&overlay.region),
                    BooleanOperation::Difference,
                    precision,
                ) {
                    Ok(regions) => remaining.extend(regions),
                    Err(ProfileBooleanError::EmptyResult) => {}
                    Err(ProfileBooleanError::Unsupported) => {
                        return Err(AnalyticBooleanError::DomainUnsupported);
                    }
                }
            }
            rest = remaining;
        }

        let mut kept: Vec<Vec<Vec<Segment>>> = Vec::new();
        for piece in rest {
            let piece_loops = {
                let mut loops = vec![piece.outer.clone()];
                loops.extend(piece.holes.iter().cloned());
                loops
            };
            if section.is_empty() {
                // Untouched face: wholesale in-or-out of the other solid.
                let inside = face_sample_inside(own, &face.value, &piece_loops, other, precision)?;
                let keep = match operation_2d {
                    BooleanOperation::Difference => !inside,
                    BooleanOperation::Intersection => inside,
                    BooleanOperation::Union => unreachable!("no 2D union rule exists"),
                };
                if keep {
                    kept.push(piece_loops);
                }
            } else {
                match profile_boolean_multi(
                    std::slice::from_ref(&piece),
                    &section,
                    operation_2d,
                    precision,
                ) {
                    Ok(regions) => kept.extend(regions.into_iter().map(|region| {
                        let mut loops = vec![region.outer];
                        loops.extend(region.holes);
                        loops
                    })),
                    Err(ProfileBooleanError::EmptyResult) => {}
                    Err(ProfileBooleanError::Unsupported) => {
                        return Err(AnalyticBooleanError::DomainUnsupported);
                    }
                }
            }
        }

        // The overlaps themselves. Two faces on one carrier are the same
        // skin twice, so the result carries it once or not at all, and the
        // first operand is the one that carries it: the second never does.
        // Which of "once" and "not at all" is the standard directed rule —
        // a difference keeps the skin where the two materials lie on
        // opposite sides of it, a union or an intersection where they lie
        // on the same side.
        if side == OperandSide::Target {
            for overlay in &overlays {
                let keep = match operation {
                    BooleanOperation::Difference => !overlay.same_side,
                    BooleanOperation::Union | BooleanOperation::Intersection => overlay.same_side,
                };
                if !keep {
                    continue;
                }
                match profile_boolean_multi(
                    std::slice::from_ref(&own_region),
                    std::slice::from_ref(&overlay.region),
                    BooleanOperation::Intersection,
                    precision,
                ) {
                    Ok(regions) => kept.extend(regions.into_iter().map(|region| {
                        let mut loops = vec![region.outer];
                        loops.extend(region.holes);
                        loops
                    })),
                    Err(ProfileBooleanError::EmptyResult) => {}
                    Err(ProfileBooleanError::Unsupported) => {
                        return Err(AnalyticBooleanError::DomainUnsupported);
                    }
                }
            }
        }
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

/// Section pieces with any curve that arrives twice reduced to one.
///
/// Sameness is the whole curve, not its ends. Two crossing bores meet in two
/// branches that share both endpoints and are not the same curve at all, so
/// matching on endpoints alone would quietly weld them into one and publish a
/// solid that is wrong rather than refused. Interior samples are what tell
/// them apart; the weld distance is the one the chaining itself uses, two
/// faces reporting one curve by different routes agreeing to the last few bits
/// rather than to all of them.
fn without_repeated_pieces(pieces: Vec<Segment>, precision: PrecisionPolicy) -> Vec<Segment> {
    let scale = pieces
        .iter()
        .flat_map(|piece| [piece.start(), piece.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let weld = precision.linear_agreement * scale * 32.0;
    let near = |left: Point2, right: Point2| (left.x - right.x).hypot(left.y - right.y) <= weld;
    let same = |held: &Segment, piece: &Segment| {
        let ends_match = (near(held.start(), piece.start()) && near(held.end(), piece.end()))
            || (near(held.start(), piece.end()) && near(held.end(), piece.start()));
        if !ends_match {
            return false;
        }
        // Along the curve, both ways round, since one copy may run the other
        // way: two branches between the same ends part company in between.
        [0.25_f64, 0.5, 0.75].into_iter().all(|fraction| {
            near(held.point_at(fraction), piece.point_at(fraction))
                || near(held.point_at(fraction), piece.point_at(1.0 - fraction))
        })
    };
    let mut kept: Vec<Segment> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        if !kept.iter().any(|held| same(held, &piece)) {
            kept.push(piece);
        }
    }
    kept
}

/// A box in model space that a face cannot leave.
///
/// The intersection matrix answers for *carriers*, which are unbounded, and
/// refuses a pair it cannot trace — two bores of unequal radius crossing,
/// say — whether or not the two bounded faces ever come near each other. A
/// boss on one end of a block is nowhere near the bore through the other
/// end, and a refusal about their carriers is not a refusal about the
/// Boolean. The extent is a superset of the face, so a pair it separates is
/// a pair the faces separate: a plane face's parameter box mapped to the
/// plane, a cylinder face's whole drum over its height range. Faces on a
/// carrier the engine does not carry have no extent, and gate nothing.
#[derive(Clone, Copy, Debug)]
struct FaceExtent {
    min: Point3,
    max: Point3,
}

fn face_extent(face: &Face, region: &[Vec<Segment>]) -> Option<FaceExtent> {
    let mut low = Point2::new(f64::INFINITY, f64::INFINITY);
    let mut high = Point2::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut include = |point: Point2| {
        low = Point2::new(low.x.min(point.x), low.y.min(point.y));
        high = Point2::new(high.x.max(point.x), high.y.max(point.y));
    };
    for segment in region.iter().flatten() {
        include(segment.start());
        include(segment.end());
        match *segment {
            Segment::Line { .. } => {}
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            } => {
                // The arc bulges past its chord wherever it passes a
                // cardinal direction; those are the only interior extremes.
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
            // A trace has no closed-form extreme, and the extent only has to
            // contain the piece, so it is sampled: a box a little large still
            // separates the faces it is asked about.
            Segment::Trace { .. } => {
                for step in 0..=32 {
                    include(segment.point_at(f64::from(step) / 32.0));
                }
            }
        }
    }
    if !(low.x.is_finite() && low.y.is_finite() && high.x.is_finite() && high.y.is_finite()) {
        return None;
    }
    let mut min = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut max = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut grow = |point: Point3| {
        min = Point3::new(min.x.min(point.x), min.y.min(point.y), min.z.min(point.z));
        max = Point3::new(max.x.max(point.x), max.y.max(point.y), max.z.max(point.z));
    };
    match face.surface {
        Surface::Plane(plane) => {
            for corner in [
                Point2::new(low.x, low.y),
                Point2::new(high.x, low.y),
                Point2::new(low.x, high.y),
                Point2::new(high.x, high.y),
            ] {
                grow(plane.evaluate(corner));
            }
        }
        Surface::Cylinder(cylinder) => {
            // The whole drum between the lowest and highest height the face
            // reaches: along each model axis the circle reaches
            // `radius·√(uᵢ² + vᵢ²)` either side of the axis line.
            let radius = cylinder.radius.abs();
            let reach = Vector3::new(
                radius * cylinder.radial_u.x.hypot(cylinder.radial_v.x),
                radius * cylinder.radial_u.y.hypot(cylinder.radial_v.y),
                radius * cylinder.radial_u.z.hypot(cylinder.radial_v.z),
            );
            for height in [low.y, high.y] {
                let on_axis = cylinder.origin + cylinder.axis * height;
                grow(Point3::new(
                    on_axis.x - reach.x,
                    on_axis.y - reach.y,
                    on_axis.z - reach.z,
                ));
                grow(Point3::new(
                    on_axis.x + reach.x,
                    on_axis.y + reach.y,
                    on_axis.z + reach.z,
                ));
            }
        }
        Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) | Surface::Ruled(_) => {
            return None;
        }
    }
    Some(FaceExtent { min, max })
}

/// Whether two faces can be told apart by their extents alone, so that a
/// carrier pair the intersection matrix refuses is one the Boolean never
/// needs. Unknown extents keep the refusal.
fn faces_apart(
    own: Option<FaceExtent>,
    other: &Topology,
    other_face: &Face,
    precision: PrecisionPolicy,
) -> bool {
    let (Some(own), Ok(region)) = (own, face_region(other, other_face)) else {
        return false;
    };
    let Some(other) = face_extent(other_face, &region) else {
        return false;
    };
    let scale = [own.min, own.max, other.min, other.max]
        .iter()
        .map(|point| point.x.abs().max(point.y.abs()).max(point.z.abs()))
        .fold(1.0_f64, f64::max);
    let margin = precision.linear_agreement.max(1.0e-12) * scale * 32.0;
    own.max.x + margin < other.min.x
        || other.max.x + margin < own.min.x
        || own.max.y + margin < other.min.y
        || other.max.y + margin < own.min.y
        || own.max.z + margin < other.min.z
        || other.max.z + margin < own.min.z
}

/// One face of the other solid lying on this face's own carrier, as a region
/// in this face's parameter space, and whether the two materials lie on the
/// same side of that carrier.
struct CoincidentOverlay {
    region: ProfileRegion,
    same_side: bool,
}

/// Every face of the other solid that lies on this face's carrier.
///
/// Two boxes meeting on a whole face, a boss whose bore wall continues the
/// hole it surrounds, a counterbore widening a hole: in each the two solids
/// share a piece of skin, and the shared piece is neither inside the other
/// solid nor outside it. It is carried through in this face's own parameter
/// space, oriented by whether the two faces look the same way — which is
/// what the operand table needs to keep it once or drop it.
fn coincident_overlays(
    face: &Face,
    own_region: &[Vec<Segment>],
    other: &Topology,
    precision: PrecisionPolicy,
) -> Result<Vec<CoincidentOverlay>, AnalyticBooleanError> {
    let own_extent = face_extent(face, own_region);
    let mut overlays = Vec::new();
    for other_face in &other.faces {
        if faces_apart(own_extent, other, &other_face.value, precision) {
            continue;
        }
        let outcome =
            intersect(face.surface, other_face.value.surface, precision).map_err(|_| {
                AnalyticBooleanError::CarrierPair(Box::new([
                    face.surface,
                    other_face.value.surface,
                ]))
            })?;
        if !matches!(outcome, SurfaceIntersection::Coincident) {
            continue;
        }
        let window = azimuth_window(own_region);
        let loops = face_region(other, &other_face.value)?
            .into_iter()
            .map(|segments| {
                reparameterize_loop(&other_face.value.surface, &segments, &face.surface, window)
                    .ok_or(AnalyticBooleanError::DomainUnsupported)
                    .and_then(|segments| {
                        welded(&segments, precision)
                            .map_err(|_| AnalyticBooleanError::DomainUnsupported)
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some((outer, holes)) = loops.split_first() else {
            continue;
        };
        // Both faces looked at from one point of the shared carrier: the
        // materials lie on the same side exactly when the outward normals
        // agree.
        let probe = match other_face.value.surface {
            Surface::Plane(plane) => plane.evaluate(outer[0].start()),
            Surface::Cylinder(cylinder) => cylinder.evaluate(outer[0].start()),
            _ => return Err(AnalyticBooleanError::DomainUnsupported),
        };
        let (Some(own_normal), Some(other_normal)) = (
            face.surface.outward_normal_at(probe),
            other_face.value.surface.outward_normal_at(probe),
        ) else {
            return Err(AnalyticBooleanError::DomainUnsupported);
        };
        overlays.push(CoincidentOverlay {
            region: ProfileRegion {
                outer: outer.clone(),
                holes: holes.to_vec(),
            },
            same_side: own_normal.dot(other_normal) > 0.0,
        });
    }
    Ok(overlays)
}

/// The other solid's section on this face's carrier, in the face's own
/// parameter space, as zero or more closed regions.
fn section_on_face(
    face: &Face,
    own_region: &[Vec<Segment>],
    other: &Topology,
    precision: PrecisionPolicy,
) -> Result<Vec<ProfileRegion>, AnalyticBooleanError> {
    let own_extent = face_extent(face, own_region);
    let mut pieces: Vec<Segment> = Vec::new();
    for other_face in &other.faces {
        // The section is closed by pieces from every face the carrier
        // crosses, near this face or not, so a pair the matrix answers is
        // always taken. Only a pair it refuses is asked whether the two
        // faces could meet at all.
        let outcome = match intersect(face.surface, other_face.value.surface, precision) {
            Ok(outcome) => outcome,
            Err(_) if faces_apart(own_extent, other, &other_face.value, precision) => continue,
            Err(_) => {
                return Err(AnalyticBooleanError::CarrierPair(Box::new([
                    face.surface,
                    other_face.value.surface,
                ])));
            }
        };
        let curves = match outcome {
            SurfaceIntersection::Empty => continue,
            // A face on this very carrier contributes no crossing curve: it
            // overlaps this face in area, and `coincident_overlays` answers
            // for that overlap by the operand table rather than by a sample
            // that would land on the other solid's own skin.
            SurfaceIntersection::Coincident => continue,
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
    // Where the other solid touches this carrier tangentially, two of its
    // faces answer with the same curve: the band that grazes the plane gives
    // the generator they share, and the flank springing from that same
    // tangency gives its own edge, which is the very same line. The outline
    // runs along it once, so the second copy is dropped rather than left to
    // make the chain ambiguous — a vertex with four ends where a loop needs
    // two.
    let pieces = without_repeated_pieces(pieces, precision);
    match face.surface {
        Surface::Cylinder(_) => close_periodic_sections(pieces, own_region, precision),
        _ => nest_section_loops(
            chain_welded_segments(pieces, precision)
                .map_err(|_| AnalyticBooleanError::DomainUnsupported)?,
        ),
    }
}

/// A piece walked one way: halfedge `2·index` is piece `index` forward and
/// `2·index + 1` the same piece reversed.
fn halfedge_segment(welded: &[Segment], halfedge: usize) -> Segment {
    let piece = welded[halfedge / 2];
    if halfedge.is_multiple_of(2) {
        piece
    } else {
        piece.reversed()
    }
}

/// The face-boundary walk over welded pieces: every halfedge in exactly one
/// cycle, each cycle keeping the cell it bounds on its left.
///
/// The walk is over *directed* halfedges with a fixed successor, not over
/// pieces with a "take whichever is still unused" continuation. That
/// distinction is the whole of this function.
///
/// Where a seam crosses itself — the pinch between the two lobes of a
/// Steinmetz seam is the case that brought this about — several branches leave
/// one point, and a walk that continues into whichever branch happens to be
/// unused has a successor that depends on where it has already been. The same
/// four arcs then trace differently according to the order they arrived in:
/// measured on the fixture in this module's tests, some orders give two lobes
/// wound the same way and others give them wound oppositely, which is exactly
/// the disagreement that leaves two faces traversing a shared edge the same
/// way round.
///
/// With a fixed successor the cycles are a property of the arrangement. At the
/// far end of a halfedge, take the outgoing branch immediately clockwise from
/// the way back: that is the standard face-boundary walk, it keeps the
/// material the chain bounds on one side, and every halfedge lies in exactly
/// one cycle however the pieces were listed.
fn halfedge_cycles(welded: &[Segment]) -> Vec<Vec<usize>> {
    let count = welded.len();
    let oriented = |halfedge: usize| halfedge_segment(welded, halfedge);
    let twin = |halfedge: usize| halfedge ^ 1;
    let key = |point: Point2| (point.x.to_bits(), point.y.to_bits());
    // The direction a halfedge sets off in from its own origin.
    let leaving = |halfedge: usize| -> Option<Point2> {
        let segment = oriented(halfedge);
        let (from, to) = (segment.start(), segment.point_at(0.05));
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let length = dx.hypot(dy);
        (length > 0.0).then(|| Point2::new(dx / length, dy / length))
    };
    let mut outgoing: std::collections::BTreeMap<(u64, u64), Vec<usize>> =
        std::collections::BTreeMap::new();
    for halfedge in 0..count * 2 {
        outgoing
            .entry(key(oriented(halfedge).start()))
            .or_default()
            .push(halfedge);
    }
    // The next halfedge round the face this one bounds: the branch immediately
    // clockwise from the way back. With one branch that is the way back
    // itself, so a loose end turns the walk around, which is what a face
    // boundary does at a dangling edge.
    let successor = |halfedge: usize| -> Option<usize> {
        let back = twin(halfedge);
        let branches = outgoing.get(&key(oriented(back).start()))?;
        let reference = leaving(back)?;
        branches.iter().copied().min_by(|left, right| {
            let turn = |branch: &usize| {
                if *branch == back {
                    // The way back is the last resort, not the first
                    // choice: a full turn rather than none.
                    return std::f64::consts::TAU;
                }
                let Some(out) = leaving(*branch) else {
                    return f64::INFINITY;
                };
                let angle = (reference.x * out.y - reference.y * out.x)
                    .atan2(reference.x * out.x + reference.y * out.y);
                if angle >= -1.0e-12 {
                    std::f64::consts::TAU - angle
                } else {
                    -angle
                }
            };
            turn(left).total_cmp(&turn(right))
        })
    };

    let mut visited = vec![false; count * 2];
    let mut cycles: Vec<Vec<usize>> = Vec::new();
    for start in 0..count * 2 {
        if visited[start] {
            continue;
        }
        let mut cycle = Vec::new();
        let mut cursor = start;
        for _ in 0..count * 2 {
            visited[cursor] = true;
            cycle.push(cursor);
            let Some(next) = successor(cursor) else { break };
            if next == start {
                break;
            }
            if visited[next] {
                // A successor already spoken for means the relation is not the
                // permutation it should be; stop rather than loop.
                break;
            }
            cursor = next;
        }
        cycles.push(cycle);
    }

    cycles
}

/// The azimuths a section piece on a cylinder spans, lowest first.
///
/// Every piece a cylinder's section is made of runs one way in the azimuth —
/// a ring chord, a harmonic, a trace between its landmarks — or not at all,
/// as a generator does, so its two ends bound it.
fn abscissa_span(piece: Segment) -> Option<(f64, f64)> {
    match piece {
        Segment::Line { start, end }
        | Segment::Harmonic { start, end, .. }
        | Segment::Trace { start, end, .. } => Some((start.x.min(end.x), start.x.max(end.x))),
        Segment::Arc { .. } | Segment::Ellipse { .. } => None,
    }
}

/// A section piece cut to the part whose azimuth lies in `[low, high]`, with
/// every cut end landing on the bound itself. `None` when nothing of the
/// piece is strictly inside.
fn clip_to_azimuths(piece: Segment, low: f64, high: f64) -> Option<Segment> {
    let (first, last) = abscissa_span(piece)?;
    if (last - first).abs() <= f64::EPSILON * first.abs().max(1.0) {
        // A generator: in the band or not, whole.
        return (first > low && first < high).then_some(piece);
    }
    if last <= low || first >= high {
        return None;
    }
    let carry = |piece: Segment, abscissa: f64, at_start: bool| -> Option<Segment> {
        match piece {
            Segment::Line { start, end } => {
                let from = if at_start { end } else { start };
                let to = if at_start { start } else { end };
                let along = (abscissa - from.x) / (to.x - from.x);
                let landed = Point2::new(abscissa, (to.y - from.y).mul_add(along, from.y));
                Some(if at_start {
                    Segment::Line { start: landed, end }
                } else {
                    Segment::Line { start, end: landed }
                })
            }
            Segment::Harmonic {
                mean,
                amplitude,
                phase,
                start,
                end,
            } => {
                let landed =
                    Point2::new(abscissa, amplitude.mul_add((abscissa - phase).cos(), mean));
                Some(Segment::Harmonic {
                    mean,
                    amplitude,
                    phase,
                    start: if at_start { landed } else { start },
                    end: if at_start { end } else { landed },
                })
            }
            trace @ Segment::Trace { .. } => trace.trace_to_abscissa(abscissa, at_start),
            Segment::Arc { .. } | Segment::Ellipse { .. } => None,
        }
    };
    let mut piece = piece;
    for at_start in [true, false] {
        let end = if at_start { piece.start() } else { piece.end() };
        if end.x < low {
            piece = carry(piece, low, at_start)?;
        } else if end.x > high {
            piece = carry(piece, high, at_start)?;
        }
    }
    Some(piece)
}

/// Closes the other solid's section on a periodic face into regions.
///
/// A section is the part of this face's carrier that lies inside the other
/// solid, and its boundary is every curve the other solid's faces cut the
/// carrier in. On a cylinder those curves live on a surface that wraps round,
/// and the face is one window of it: a plane's trace crosses the window from
/// seam to seam, a bore of the same size crosses it in a lens, and a narrower
/// bore that reaches a seam without passing it takes a bite out of the edge
/// and leaves by the seam it came in by. Asking which of those shapes a
/// section is, and closing each its own way, is the approach that kept
/// needing another case; this does not ask.
///
/// The curves are lifted onto the unrolled carrier and cut to a window a
/// little wider than the face's own, so the face lies strictly inside it and
/// nothing added below ever touches the face's edges. Inside that window the
/// section is bounded by the curves and by stretches of the window's two edge
/// generators — and which stretches is not a question about shapes but a
/// count. A generator is a line on the carrier. Far enough along it, it is
/// outside the other solid, which is bounded; every curve it crosses takes it
/// in or out. So along each edge generator the crossings, taken in order,
/// pair off: first and second bound a stretch inside, third and fourth the
/// next. Those stretches close every chain that the window cut open, and the
/// section comes back as closed loops for [`nest_section_loops`] — whatever
/// mixture of bands, bites, lenses and islands it happens to be.
///
/// A curve that meets an edge generator and turns back, or two curves that
/// cross on it, put an even number of ends at one point: the side does not
/// change there, so those ends pair with each other rather than with the
/// stretch, and a stretch that runs past such a point is cut at it.
fn close_periodic_sections(
    pieces: Vec<Segment>,
    region: &[Vec<Segment>],
    precision: PrecisionPolicy,
) -> Result<Vec<ProfileRegion>, AnalyticBooleanError> {
    let tau = std::f64::consts::TAU;
    let unclosed = |pieces: &[Segment]| {
        if pieces
            .iter()
            .any(|piece| matches!(piece, Segment::Trace { .. }))
        {
            AnalyticBooleanError::TraceUnclosed
        } else {
            AnalyticBooleanError::DomainUnsupported
        }
    };
    let Some((u_min, u_max)) = azimuth_window(region) else {
        return Err(AnalyticBooleanError::DomainUnsupported);
    };
    let reach = 0.05;
    let (low, high) = (u_min - reach, u_max + reach);

    // Each curve once: the matrix offers a piece on every turn a window
    // might use, and pieces handed over from another face arrive on
    // whichever turn that face's arctangent gave. Brought to one turn, the
    // copies are the same piece, and a copy left in would double a curve.
    let scale = pieces
        .iter()
        .flat_map(|segment| [segment.start(), segment.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let weld = precision.linear_agreement.max(1.0e-12) * scale * 32.0;
    let mut once: Vec<Segment> = Vec::new();
    for piece in pieces {
        let (first, _) = abscissa_span(piece).ok_or(AnalyticBooleanError::DomainUnsupported)?;
        let turns = (first / tau).floor();
        let piece = if turns == 0.0 {
            piece
        } else {
            piece.translated(Point2::new(turns * tau, 0.0))
        };
        once.push(piece);
    }
    let once = without_repeated_pieces(once, precision);

    // Then every turn of each that reaches into the window, cut to it.
    let mut lifted: Vec<Segment> = Vec::new();
    for piece in once {
        let (first, last) = abscissa_span(piece).ok_or(AnalyticBooleanError::DomainUnsupported)?;
        let lowest = ((low - last) / tau).floor() as i64;
        let highest = ((high - first) / tau).ceil() as i64;
        for turns in lowest..=highest {
            let shifted = if turns == 0 {
                piece
            } else {
                piece.translated(Point2::new(-(turns as f64) * tau, 0.0))
            };
            if let Some(kept) = clip_to_azimuths(shifted, low, high)
                && kept.length() > weld
            {
                lifted.push(kept);
            }
        }
    }
    if lifted.is_empty() {
        return Ok(Vec::new());
    }

    // Weld ends that meet, then cut the curves wherever they cross or touch
    // away from an edge of the other solid — the pinch of a Steinmetz seam
    // is two curves crossing where the cylinders are tangent — so the pieces
    // meet only at their ends and the walk below sees cells, not one loop
    // wound through a crossing.
    let welded = weld_aligned(lifted, weld);
    // Curves that overlap along a stretch, rather than crossing, are a
    // contact this closure does not classify.
    let crossed = split_at_mutual_crossings(&welded, precision)
        .map_err(|_| AnalyticBooleanError::DomainUnsupported)?;
    let crossed = weld_aligned(crossed, weld);
    // A generator the carrier is only tangent along bounds nothing. On the
    // face's own edge that is a smooth edge of the result — a fillet's
    // cutter touches the body's faces exactly along its two seams — and the
    // line is dropped. Strictly inside the face it is two solids touching
    // along a line, which publishes a seam of no width; tangential contact
    // fails closed, as it does everywhere else in this engine.
    let margin = precision.linear_agreement.max(1.0e-12) * scale * 128.0;
    let mut welded: Vec<Segment> = Vec::with_capacity(crossed.len());
    for piece in &crossed {
        if generator_bounds(*piece, &crossed) {
            welded.push(*piece);
        } else if piece.start().x > u_min + margin && piece.start().x < u_max - margin {
            return Err(AnalyticBooleanError::DomainUnsupported);
        }
    }

    // The stretches of each edge generator that lie inside the other solid.
    let mut closures: Vec<Segment> = Vec::new();
    for bound in [low, high] {
        let mut ends: Vec<(Point2, usize)> = Vec::new();
        for piece in &welded {
            for point in [piece.start(), piece.end()] {
                if (point.x - bound).abs() > weld {
                    continue;
                }
                match ends.iter_mut().find(|(held, _)| *held == point) {
                    Some((_, count)) => *count += 1,
                    None => ends.push((point, 1)),
                }
            }
        }
        let mut crossings: Vec<Point2> = ends
            .iter()
            .filter(|(_, count)| count % 2 == 1)
            .map(|(point, _)| *point)
            .collect();
        let touches: Vec<Point2> = ends
            .iter()
            .filter(|(_, count)| count % 2 == 0)
            .map(|(point, _)| *point)
            .collect();
        crossings.sort_by(|left, right| left.y.total_cmp(&right.y));
        if !crossings.len().is_multiple_of(2) {
            return Err(unclosed(&welded));
        }
        for pair in crossings.chunks(2) {
            let mut stops = vec![pair[0]];
            stops.extend(
                touches
                    .iter()
                    .filter(|touch| touch.y > pair[0].y && touch.y < pair[1].y),
            );
            stops.push(pair[1]);
            stops.sort_by(|left, right| left.y.total_cmp(&right.y));
            for stretch in stops.windows(2) {
                closures.push(Segment::Line {
                    start: stretch[0],
                    end: stretch[1],
                });
            }
        }
    }
    welded.extend(closures);

    // Every cycle of the arrangement bounds the cell on its left, and each
    // cell is wholly inside the other solid or wholly outside it, since
    // every curve is a boundary of that solid's section. The cells inside
    // are the section: their outer cycles wind positive, and a cycle round
    // a hole in one winds negative with the material still on its left.
    // Cycles whose left is outside — the boundary of the window's outside,
    // and of every void a curve encloses — are dropped whatever their
    // winding. Asking the cell rather than the winding is what lets two
    // lobes pinched at a point, or a lens cut out of a band, come back as
    // what they are.
    let mut outers: Vec<Vec<Segment>> = Vec::new();
    let mut holes: Vec<Vec<Segment>> = Vec::new();
    for cycle in halfedge_cycles(&welded) {
        let length = cycle.len();
        if (0..length).any(|position| cycle[position] == cycle[(position + 1) % length] ^ 1) {
            // A walk that turns back ran out along a dangling curve: some
            // face of the other solid did not report the piece that
            // continues it.
            return Err(unclosed(&welded));
        }
        let cycle: Vec<Segment> = cycle
            .into_iter()
            .map(|halfedge| halfedge_segment(&welded, halfedge))
            .collect();
        if !material_on_left(&cycle, &welded) {
            continue;
        }
        if loop_area(&cycle) > 0.0 {
            outers.push(cycle);
        } else {
            holes.push(cycle);
        }
    }
    let mut regions: Vec<ProfileRegion> = outers
        .into_iter()
        .map(|outer| ProfileRegion {
            outer,
            holes: Vec::new(),
        })
        .collect();
    for hole in holes {
        // A hole's own pieces are its alone — no two cycles share one — so
        // the middle of one of them sits strictly inside the cell it holes.
        let sample = longest_piece(&hole).point_at(0.5);
        let owner = regions
            .iter_mut()
            .filter(|region| {
                crate::analytic_extrusion::point_inside_loop(
                    sample,
                    &crate::analytic_extrusion::AnalyticLoop {
                        segments: region.outer.clone(),
                        signed_area: 0.0,
                    },
                )
            })
            .min_by(|left, right| loop_area(&left.outer).total_cmp(&loop_area(&right.outer)));
        let Some(owner) = owner else {
            return Err(unclosed(&welded));
        };
        owner.holes.push(hole);
    }
    Ok(regions)
}

/// Whether a generator in the section actually bounds it.
///
/// A plane tangent to the carrier touches it along a generator, and the
/// matrix reports that line; but the carrier does not cross the plane there,
/// so the other solid is on the same side of it to the left and to the right,
/// and the line bounds nothing. A fillet's own cutter meets the body's faces
/// exactly so, along both of its seams. Kept, such a line splits one cell of
/// the section into two that share it, which the face's 2D Boolean then has
/// to reconcile along the face's own edge. So a generator is asked what every
/// piece of a section must be: a place where the count changes. Only a
/// generator can fail the question — a curve that is not one is crossed by
/// the count itself — and only a tangency makes it fail.
fn generator_bounds(piece: Segment, arrangement: &[Segment]) -> bool {
    let Segment::Line { start, end } = piece else {
        return true;
    };
    if (start.x - end.x).abs() > 1.0e-12 * start.x.abs().max(1.0) {
        return true;
    }
    let middle = piece.point_at(0.5);
    let reach = 1.0e-7 * (1.0 + middle.x.abs() + middle.y.abs());
    let parity = |azimuth: f64| {
        arrangement
            .iter()
            .filter(|candidate| {
                ordinate_at(**candidate, azimuth).is_some_and(|height| height < middle.y)
            })
            .count()
            % 2
    };
    parity(middle.x - reach) != parity(middle.x + reach)
}

/// A loop's signed area in its face's parameter space.
fn loop_area(segments: &[Segment]) -> f64 {
    segments
        .iter()
        .map(|segment| segment.signed_area_contribution())
        .sum()
}

/// The longest piece of a loop: its middle is as far from the loop's
/// vertices as the loop allows.
fn longest_piece(segments: &[Segment]) -> Segment {
    segments
        .iter()
        .copied()
        .max_by(|left, right| left.length().total_cmp(&right.length()))
        .unwrap_or(segments[0])
}

/// The height of a section piece at an azimuth it spans, for the count
/// below. Each piece is taken half-open, so a vertex two pieces share is
/// counted once; a generator spans no azimuth at all.
fn ordinate_at(piece: Segment, azimuth: f64) -> Option<f64> {
    let (start, end) = (piece.start(), piece.end());
    let (low, high) = (start.x.min(end.x), start.x.max(end.x));
    if !(low <= azimuth && azimuth < high) {
        return None;
    }
    match piece {
        Segment::Line { start, end } => {
            Some((end.y - start.y).mul_add((azimuth - start.x) / (end.x - start.x), start.y))
        }
        Segment::Harmonic {
            mean,
            amplitude,
            phase,
            ..
        } => Some(amplitude.mul_add((azimuth - phase).cos(), mean)),
        Segment::Trace {
            host,
            other,
            branch,
            shift,
            ..
        } => Some(
            CylinderTrace {
                host,
                other,
                branch,
            }
            .height_clamped(azimuth - shift.x)
                + shift.y,
        ),
        Segment::Arc { .. } | Segment::Ellipse { .. } => None,
    }
}

/// Whether the other solid's material lies on the left of a traced cycle.
///
/// The count is the closure's own: walk down the generator through a point
/// just left of the cycle, and every curve crossed below takes the walk in
/// or out of the other solid, which it starts outside of. The point is
/// taken off the middle of the cycle's longest piece, a hair to its left —
/// far from every vertex, and nearer that piece than anything else.
fn material_on_left(cycle: &[Segment], arrangement: &[Segment]) -> bool {
    let piece = longest_piece(cycle);
    let middle = piece.point_at(0.5);
    let (ahead, behind) = (piece.point_at(0.5 + 1.0e-3), piece.point_at(0.5 - 1.0e-3));
    let (dx, dy) = (ahead.x - behind.x, ahead.y - behind.y);
    let length = dx.hypot(dy);
    if length <= 0.0 {
        return false;
    }
    let reach = 1.0e-7 * (1.0 + middle.x.abs() + middle.y.abs());
    let probe = Point2::new(
        (-dy / length).mul_add(reach, middle.x),
        (dx / length).mul_add(reach, middle.y),
    );
    arrangement
        .iter()
        .filter(|candidate| {
            ordinate_at(**candidate, probe.x).is_some_and(|height| height < probe.y)
        })
        .count()
        % 2
        == 1
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
    // A point of the loop that no other loop passes through. A vertex will
    // not do: loops of a section meet at points — the pinch of a Steinmetz
    // seam, a curve crossing the window's edge — and a vertex there is on
    // both, where containment is a coin toss. Loops never share a stretch,
    // so the middle of one of a loop's own pieces is its alone.
    let sample = |segments: &[Segment]| {
        segments
            .iter()
            .max_by(|left, right| left.length().total_cmp(&right.length()))
            .map_or_else(|| segments[0].start(), |piece| piece.point_at(0.5))
    };
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
            // On every turn a bounded face window can reach, as a ring or a
            // harmonic is: the arctangent hands back the principal azimuth,
            // and a face whose window is the other half turn would otherwise
            // never see the generator that runs down the middle of it.
            let tau = std::f64::consts::TAU;
            Some(
                [-1.0, 0.0, 1.0]
                    .into_iter()
                    .map(|turns: f64| {
                        let at = turns.mul_add(tau, u);
                        Segment::Line {
                            start: Point2::new(at, along.mul_add(-SPAN, base)),
                            end: Point2::new(at, along.mul_add(SPAN, base)),
                        }
                    })
                    .collect(),
            )
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
        (Surface::Cylinder(cylinder), IntersectionCurve::Trace(trace)) => {
            // Read over this face's own azimuth, whichever of the pair holds
            // the edge's parameter: every 2D stage reads a trace as a graph
            // over its own face (ADR 0047). The curve is cut at its
            // landmarks — both cylinders' branch points — and nowhere else,
            // so the face across the curve cuts it at the same points.
            let other = if same_carrier(*cylinder, trace.other) {
                trace.host
            } else if same_carrier(*cylinder, trace.host) {
                trace.other
            } else {
                return None;
            };
            let own = CylinderTrace {
                host: *cylinder,
                other,
                branch: 1.0,
            };
            let tau = std::f64::consts::TAU;
            let mut pieces = Vec::new();
            for arc in own.arcs()? {
                // The matrix names the curve one root at a time, over the
                // pair's canonical reading; each arc lies on one of those
                // roots, since the canonical host's branch points are among
                // the landmarks, and is offered once, with its own root.
                let reading = CylinderTrace {
                    branch: arc.branch,
                    ..own
                };
                let middle = reading.point_clamped(0.5 * (arc.from + arc.to));
                let canonical = cylinder_local(trace.host, middle);
                if trace.branch_at(canonical) != Some(trace.branch) {
                    continue;
                }
                // A face's window may sit on any whole turn, so each arc is
                // offered on every turn a bounded window can reach, exactly
                // as a ring chord is.
                for turns in [-2.0, -1.0, 0.0, 1.0] {
                    let shift = Point2::new(turns * tau, 0.0);
                    let place = |point: Point2| Point2::new(point.x + shift.x, point.y);
                    pieces.push(Segment::Trace {
                        host: *cylinder,
                        other,
                        branch: arc.branch,
                        shift,
                        from: arc.from,
                        to: arc.to,
                        start: place(arc.start),
                        end: place(arc.end),
                    });
                }
            }
            (!pieces.is_empty()).then_some(pieces)
        }
        _ => None,
    }
}

/// The azimuth span a region occupies, for placing another loop on the same
/// branch of a periodic face.
fn azimuth_window(region: &[Vec<Segment>]) -> Option<(f64, f64)> {
    let (low, high) = region
        .iter()
        .flatten()
        .flat_map(|segment| [segment.start().x, segment.end().x])
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), x| {
            (low.min(x), high.max(x))
        });
    (low.is_finite() && high.is_finite()).then_some((low, high))
}

/// A whole loop re-expressed on another face.
///
/// Segment by segment, the mapping lands each end on whichever branch the
/// arctangent returns, and a loop that crosses the seam comes back torn:
/// one piece ending at `π`, the next starting at `−π`. The loop is
/// continuous, so each piece is carried by whole turns onto the branch the
/// previous piece ended on, and the finished loop is brought by whole turns
/// onto the window the receiving face's own region uses — the branch on
/// which the two regions can be compared at all.
fn reparameterize_loop(
    from: &Surface,
    segments: &[Segment],
    to: &Surface,
    window: Option<(f64, f64)>,
) -> Option<Vec<Segment>> {
    let tau = std::f64::consts::TAU;
    let periodic = matches!(to, Surface::Cylinder(_));
    let mut mapped: Vec<Segment> = Vec::with_capacity(segments.len());
    for segment in segments {
        let mut piece = reparameterize(from, *segment, to)?;
        if periodic && let Some(previous) = mapped.last() {
            let turns = ((previous.end().x - piece.start().x) / tau).round();
            if turns != 0.0 {
                piece = piece.translated(Point2::new(-turns * tau, 0.0));
            }
        }
        mapped.push(piece);
    }
    if periodic
        && let Some((low, high)) = window
        && let Some((own_low, own_high)) = azimuth_window(std::slice::from_ref(&mapped))
    {
        let turns = (((low + high) - (own_low + own_high)) / (2.0 * tau)).round();
        if turns != 0.0 {
            for piece in &mut mapped {
                *piece = piece.translated(Point2::new(-turns * tau, 0.0));
            }
        }
    }
    Some(mapped)
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
                // Two cylinders meet in a plane curve only where they meet
                // in a circle or an ellipse, which the matrix names as such;
                // a trace piece that reached here would not be planar.
                Segment::Trace { .. } => None,
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
                line @ Segment::Line { start, end } => {
                    let a = local(world(start)?);
                    let b = local(world(end)?);
                    let tau = std::f64::consts::TAU;
                    let nearest =
                        |value: f64, target: f64| value + ((target - value) / tau).round() * tau;
                    // A generator's two ends are one azimuth, which the
                    // arctangent may hand back as `π` for one end and `−π`
                    // for the other when the generator lies on the seam;
                    // the azimuth is the same and the first end's branch is
                    // kept.
                    let bx = nearest(b.x, a.x);
                    if (a.x - bx).abs() <= 1.0e-9 {
                        Some(Segment::Line {
                            start: a,
                            end: Point2::new(a.x, b.y),
                        })
                    } else if (a.y - b.y).abs() <= 1.0e-9 {
                        // A ring chord: the branch is the one its midpoint
                        // lies on, as for an arc below.
                        let middle = nearest(local(world(line.point_at(0.5))?).x, a.x);
                        let bx = nearest(b.x, a.x + 2.0 * (middle - a.x));
                        Some(Segment::Line {
                            start: a,
                            end: Point2::new(bx, b.y),
                        })
                    } else {
                        None
                    }
                }
                // The piece already carries both cylinders, so handing it to
                // another face is a change of which azimuth it is read over,
                // not a change of curve: the receiving face's own, whether
                // that is the other cylinder or this one in another frame.
                // Its ends are carried exactly, so a branch point's double
                // root stays the end both branches share.
                Segment::Trace {
                    host,
                    other,
                    branch,
                    shift,
                    from,
                    to,
                    start,
                    end,
                } => {
                    let trace = CylinderTrace {
                        host,
                        other,
                        branch,
                    };
                    let unshifted =
                        |point: Point2| Point2::new(point.x - shift.x, point.y - shift.y);
                    let ends = [
                        host.evaluate(unshifted(start)),
                        host.evaluate(unshifted(end)),
                    ];
                    let (read, arc) = trace.read_on(*cylinder, from, to, ends)?;
                    Some(Segment::Trace {
                        host: *cylinder,
                        other: read.other,
                        branch: read.branch,
                        shift: Point2::new(0.0, 0.0),
                        from: arc.from,
                        to: arc.to,
                        start: arc.start,
                        end: arc.end,
                    })
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
                harmonic @ Segment::Harmonic {
                    mean,
                    amplitude,
                    phase,
                    start,
                    end,
                } => {
                    // The seam two crossing cylinders share (ADR 0026, K1
                    // stage 3). It is one ellipse in space traced on two
                    // walls, so it is rebuilt from the source trace and asked
                    // for its trace here; only the parameterization changes.
                    //
                    // This is the first pair where both carriers are
                    // cylinders. Every earlier in-matrix pair put a plane on
                    // one side, which is why the arm above exists and this one
                    // did not.
                    let Surface::Cylinder(source_cylinder) = from else {
                        return None;
                    };
                    let source = CylinderSectionHarmonic {
                        cylinder: *source_cylinder,
                        mean,
                        amplitude,
                        phase,
                    };
                    let (center, major_axis, minor_axis, major, minor) = source.ellipse()?;
                    let section = cylinder_section_harmonic(
                        cylinder, center, major_axis, minor_axis, major, minor,
                    )?;
                    // The azimuths come from the endpoints themselves, and the
                    // midpoint says which way round the branch runs: a pair of
                    // endpoints alone cannot tell a span from its complement,
                    // and on a seam that wraps the wall the difference is the
                    // whole face.
                    let first = local(world(start)?);
                    let last = local(world(end)?);
                    let middle = local(world(harmonic.point_at(0.5))?);
                    let tau = std::f64::consts::TAU;
                    let nearest =
                        |value: f64, target: f64| value + ((target - value) / tau).round() * tau;
                    let through = nearest(middle.x, first.x);
                    let to = nearest(last.x, first.x + 2.0 * (through - first.x));
                    Some(section.segment(first.x, to))
                }
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
pub(crate) fn point_in_solid(topology: &Topology, point: Point3) -> Option<bool> {
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
        // Mirroring the azimuth is exactly reversing this face's angular
        // sense: `radial(−x)` is what `radial(x)` becomes when the sign
        // flips, so the graph mirrors in parameter space with the same root
        // and no reflection of anything in space. Only the face's own
        // cylinder changes frame; the other is the face across the curve,
        // and keeps the frame that face walks it in.
        Segment::Trace {
            host,
            other,
            branch,
            shift,
            from,
            to,
            start,
            end,
        } => Segment::Trace {
            host: Cylinder {
                angular_sign: -host.angular_sign,
                ..host
            },
            other,
            branch,
            shift: Point2::new(-shift.x, shift.y),
            from: -to,
            to: -from,
            start: mirror(end),
            end: mirror(start),
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The Steinmetz seam as it reaches a bore wall, reduced to the smallest
    /// fixture that carries the defect: two harmonics `v = ±8·cos u` over
    /// `u ∈ [−π/2, 3π/2]`, each split at `u = π/2`.
    ///
    /// Four arcs, with endpoint degrees 2, 4 and 2 — the pattern ADR 0025
    /// records from the real body. The two lobes enclose 32 each, so the
    /// traversal has exactly one right answer: two loops, of area ±32.
    fn four_harmonic_arcs() -> Vec<Segment> {
        let pi = std::f64::consts::PI;
        // Both curves cross zero at every one of −π/2, π/2 and 3π/2, so the
        // endpoints are written as exact zeros. Evaluating `8·cos` there
        // instead gives values a few times 1e-16 apart that differ between the
        // two curves, and the pinch they are supposed to share stops being one
        // point. The pipeline welds before it traverses; the fixture has to
        // arrive welded for the same reason.
        let arc = |phase: f64, from: f64, to: f64| Segment::Harmonic {
            mean: 0.0,
            amplitude: 8.0,
            phase,
            start: Point2::new(from, 0.0),
            end: Point2::new(to, 0.0),
        };
        vec![
            arc(0.0, -pi / 2.0, pi / 2.0),
            arc(0.0, pi / 2.0, 3.0 * pi / 2.0),
            arc(pi, -pi / 2.0, pi / 2.0),
            arc(pi, pi / 2.0, 3.0 * pi / 2.0),
        ]
    }

    /// The area a chain encloses in the face's own parameter space, sampled
    /// along each arc so a harmonic's bow counts rather than only its chord.
    fn signed_area(chain: &[Segment]) -> f64 {
        let mut points: Vec<Point2> = Vec::new();
        for segment in chain {
            for step in 0..32 {
                points.push(segment.point_at(f64::from(step) / 32.0));
            }
        }
        let count = points.len();
        (0..count)
            .map(|index| {
                let (a, b) = (points[index], points[(index + 1) % count]);
                a.x.mul_add(b.y, -(b.x * a.y))
            })
            .sum::<f64>()
            * 0.5
    }

    /// What a chain *is*, written so two chains describing the same loop from
    /// different starting pieces come out the same.
    ///
    /// Each piece is rendered by where it begins, where it ends and where its
    /// middle bows to, so an arc is told apart from its chord and from the
    /// other arc on the same two endpoints. A closed chain is then rotated to
    /// its least rendering, which is the cyclic-loop-start half of the property
    /// under test: a loop has no first edge, so the traversal must not depend
    /// on which one it happened to walk first.
    fn canonical(chain: &[Segment]) -> String {
        let place = |point: Point2| format!("{:.6},{:.6}", point.x, point.y);
        let mut pieces: Vec<String> = chain
            .iter()
            .map(|segment| {
                format!(
                    "{}>{}~{}",
                    place(segment.start()),
                    place(segment.end()),
                    place(segment.point_at(0.5))
                )
            })
            .collect();
        let closes = chain
            .first()
            .zip(chain.last())
            .is_some_and(|(first, last)| {
                let (from, to) = (first.start(), last.end());
                (from.x - to.x).hypot(from.y - to.y) < 1.0e-9
            });
        if closes
            && let Some(least) = (0..pieces.len()).min_by_key(|start| {
                let mut rotated = pieces.clone();
                rotated.rotate_left(*start);
                rotated.join("|")
            })
        {
            pieces.rotate_left(least);
        }
        pieces.join("|")
    }

    /// Every cycle the face-boundary walk traces, as pieces.
    fn cycles(pieces: &[Segment]) -> Vec<Vec<Segment>> {
        halfedge_cycles(pieces)
            .into_iter()
            .map(|cycle| {
                cycle
                    .into_iter()
                    .map(|halfedge| halfedge_segment(pieces, halfedge))
                    .collect()
            })
            .collect()
    }

    /// The cycles that wind positive: the outer boundary of every bounded
    /// cell of the arrangement.
    fn rings(pieces: &[Segment]) -> Vec<Vec<Segment>> {
        cycles(pieces)
            .into_iter()
            .filter(|cycle| signed_area(cycle) > 0.0)
            .collect()
    }

    /// What a presentation traces, as one comparable string: how many cycles,
    /// what each encloses, and what each *is*.
    fn fingerprint(pieces: &[Segment]) -> String {
        let chains = cycles(pieces);
        let mut described: Vec<String> = chains
            .iter()
            .map(|chain| {
                format!(
                    "[{}] {}",
                    (signed_area(chain) * 1_000.0).round() as i64,
                    canonical(chain)
                )
            })
            .collect();
        described.sort();
        format!(
            "{} chain(s):\n    {}",
            chains.len(),
            described.join("\n    ")
        )
    }

    /// Asserts every presentation traces the same thing, and hands back what
    /// they agree on so the caller can go on to check it is the right thing.
    fn agreed(presentations: &[Vec<Segment>]) -> String {
        let expected = fingerprint(&presentations[0]);
        let mut wrong: Vec<String> = Vec::new();
        for (index, pieces) in presentations.iter().enumerate().skip(1) {
            let found = fingerprint(pieces);
            if found != expected && wrong.len() < 4 {
                wrong.push(format!("presentation {index}: {found}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "the same pieces trace differently depending on the order they \
             arrive in.\n  presentation 0: {expected}\n  {}",
            wrong.join("\n  ")
        );
        expected
    }

    /// Deterministic shufflings of a piece list, each with its own subset
    /// handed in reversed, for fixtures too large to enumerate exhaustively.
    fn some_presentations(pieces: &[Segment], how_many: usize) -> Vec<Vec<Segment>> {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        (0..how_many)
            .map(|_| {
                let mut order: Vec<usize> = (0..pieces.len()).collect();
                for slot in (1..order.len()).rev() {
                    order.swap(slot, (next() % (slot as u64 + 1)) as usize);
                }
                order
                    .into_iter()
                    .map(|index| {
                        if next() % 2 == 0 {
                            pieces[index]
                        } else {
                            pieces[index].reversed()
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Every ordering of the same four arcs, with every subset of them handed
    /// in reversed: 24 × 16 ways of describing one curve.
    fn every_presentation(arcs: &[Segment]) -> Vec<Vec<Segment>> {
        let mut orders = vec![vec![0, 1, 2, 3]];
        for _ in 0..3 {
            let mut grown = Vec::new();
            for order in &orders {
                for rotation in 0..4 {
                    let mut next = order.clone();
                    next.rotate_left(rotation);
                    if !grown.contains(&next) {
                        grown.push(next);
                    }
                }
            }
            orders = grown;
        }
        // Rotations alone are four of the twenty-four; add the swaps that
        // reach the rest.
        let mut permutations: Vec<Vec<usize>> = Vec::new();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let order = vec![a, b, c, d];
                        let mut seen = order.clone();
                        seen.sort_unstable();
                        seen.dedup();
                        if seen.len() == 4 {
                            permutations.push(order);
                        }
                    }
                }
            }
        }
        let mut presentations = Vec::new();
        for order in permutations {
            for mask in 0..16_u32 {
                presentations.push(
                    order
                        .iter()
                        .enumerate()
                        .map(|(slot, index)| {
                            let arc = arcs[*index];
                            if mask >> slot & 1 == 1 {
                                arc.reversed()
                            } else {
                                arc
                            }
                        })
                        .collect(),
                );
            }
        }
        presentations
    }

    /// The traversal must describe the curve, not the order the curve arrived
    /// in. Two lobes meeting at a pinch are two loops of 32 however the arcs
    /// are listed — and a walk that continues into whichever branch is still
    /// unused can instead return one pinched loop, or one whose lobes cancel
    /// to nothing.
    ///
    /// Two things are asserted, and they are not the same thing. That every
    /// presentation agrees is the property the fix is for; that what they agree
    /// on is two lobes of 32 wound the same way is what says the agreement is
    /// on the right answer rather than on a consistent wrong one.
    #[test]
    fn a_pinched_section_traces_the_same_two_lobes_however_its_arcs_arrive() {
        let arcs = four_harmonic_arcs();
        let presentations = every_presentation(&arcs);
        assert_eq!(presentations.len(), 384);
        // The fingerprint carries the chains themselves, each up to where its
        // walk started, and not only their areas: that is what makes this a
        // test of the arrangement rather than of two numbers that could agree
        // by coincidence.
        agreed(&presentations);

        // And the answer they agree on is the one the geometry has. Each lobe
        // is `∫ 8·cos u du` over half a period, which is 32; sampling it as a
        // polygon undercuts that by a few hundredths.
        let chains = rings(&presentations[0]);
        assert_eq!(chains.len(), 2, "a pinch is two lobes, not one loop");
        for chain in &chains {
            let area = signed_area(chain);
            assert!(
                (area - 32.0).abs() < 0.1,
                "a lobe of this fixture encloses 32, wound positive; this one \
                 encloses {area:.3}"
            );
        }
    }

    /// A curve that stops inside the window is a section some face of the
    /// other solid did not finish. The closure refuses it rather than guess
    /// which way round it was meant to go: the walk goes out along it and
    /// comes back, and a walk that turns back is not a boundary.
    #[test]
    fn a_section_that_stops_inside_the_window_is_refused() {
        let pi = std::f64::consts::PI;
        let corner = |from: (f64, f64), to: (f64, f64)| Segment::Line {
            start: Point2::new(from.0, from.1),
            end: Point2::new(to.0, to.1),
        };
        let face = vec![vec![
            corner((0.0, 0.0), (pi, 0.0)),
            corner((pi, 0.0), (pi, 10.0)),
            corner((pi, 10.0), (0.0, 10.0)),
            corner((0.0, 10.0), (0.0, 0.0)),
        ]];
        let dangling = vec![
            corner((0.5, 2.0), (1.5, 3.0)),
            corner((1.5, 3.0), (2.5, 2.0)),
        ];
        assert_eq!(
            close_periodic_sections(dangling, &face, PrecisionPolicy::default()).err(),
            Some(AnalyticBooleanError::DomainUnsupported)
        );
    }

    /// Two sections that never meet are two loops. The walk finds four cycles —
    /// each loop's inside and each loop's outside — and only the insides are
    /// regions. Keeping the single most negative of them, as a first attempt
    /// did, leaves one loop still doubled by its own outside.
    #[test]
    fn two_loops_that_never_meet_come_back_as_two_loops() {
        let corner = |from: (f64, f64), to: (f64, f64)| Segment::Line {
            start: Point2::new(from.0, from.1),
            end: Point2::new(to.0, to.1),
        };
        let square = |x: f64| {
            vec![
                corner((x, 0.0), (x + 1.0, 0.0)),
                corner((x + 1.0, 0.0), (x + 1.0, 1.0)),
                corner((x + 1.0, 1.0), (x, 1.0)),
                corner((x, 1.0), (x, 0.0)),
            ]
        };
        let mut pieces = square(0.0);
        pieces.extend(square(4.0));
        let presentations = some_presentations(&pieces, 200);
        agreed(&presentations);

        let chains = rings(&presentations[0]);
        assert_eq!(chains.len(), 2, "two squares are two loops");
        for chain in &chains {
            let area = signed_area(chain);
            assert!(
                (area - 1.0).abs() < 1.0e-9,
                "a unit square encloses 1, wound positive; this one encloses \
                 {area}"
            );
        }
    }

    /// The two traces a Steinmetz seam leaves on a bore wall are reflections of
    /// one another about the middle of the window, so they have the same
    /// average height — to the last bit, not merely close. When the section was
    /// closed by stacking its chains into bands, that average decided the
    /// order, the order came from the order the traces arrived in, and the
    /// bands spanned the lens between them instead of avoiding it: the section
    /// claimed the other solid's material was exactly where its void is.
    ///
    /// The closure now counts crossings along the window's edge generators
    /// instead, and nothing in that depends on arrival order. The fixture
    /// stays because it is the one that once fooled the closure: the lens has
    /// to come back as material however the traces arrive.
    #[test]
    fn two_traces_that_average_alike_still_close_on_the_lens_between_them() {
        // `v = 980 ± 8·cos(u − 3π/2)` over the window `u ∈ [π, 2π]`, each split
        // at its apex and reaching a half period past either seam, which is how
        // the bore wall's own section arrives: one whole period, in two pieces.
        let pi = std::f64::consts::PI;
        let (u_min, u_max) = (pi, 2.0 * pi);
        let apex = 1.5 * pi;
        let trace = |amplitude: f64| -> Vec<Segment> {
            let at = |u: f64| Point2::new(u, 8.0f64.mul_add(amplitude * (u - apex).cos(), 980.0));
            [(u_min - pi / 2.0, apex), (apex, u_max + pi / 2.0)]
                .into_iter()
                .map(|(from, to)| Segment::Harmonic {
                    mean: 980.0,
                    amplitude: 8.0 * amplitude,
                    phase: apex,
                    start: at(from),
                    end: at(to),
                })
                .collect()
        };
        let (upper, lower) = (trace(1.0), trace(-1.0));

        // The average cannot tell them apart. This is the premise, so it is
        // asserted rather than described: if it ever stopped being true the
        // test below would pass for a reason that has nothing to do with the
        // closure.
        let mean = |chain: &[Segment]| -> f64 {
            let heights: Vec<f64> = chain
                .iter()
                .flat_map(|piece| (0..=4).map(move |step| piece.point_at(f64::from(step) / 4.0).y))
                .collect();
            heights.iter().sum::<f64>() / heights.len() as f64
        };
        assert_eq!(
            mean(&upper).to_bits(),
            mean(&lower).to_bits(),
            "the fixture's two traces must average alike for this test to mean anything"
        );

        let corner = |u: f64, v: f64| Point2::new(u, v);
        let face = vec![vec![
            Segment::Line {
                start: corner(u_min, 900.0),
                end: corner(u_max, 900.0),
            },
            Segment::Line {
                start: corner(u_max, 900.0),
                end: corner(u_max, 1_060.0),
            },
            Segment::Line {
                start: corner(u_max, 1_060.0),
                end: corner(u_min, 1_060.0),
            },
            Segment::Line {
                start: corner(u_min, 1_060.0),
                end: corner(u_min, 900.0),
            },
        ]];
        for arrival in [
            [upper.clone(), lower.clone()].concat(),
            [lower.clone(), upper.clone()].concat(),
        ] {
            let regions = close_periodic_sections(arrival, &face, PrecisionPolicy::default())
                .expect("the Steinmetz section closes");
            let loops: Vec<Vec<Segment>> = regions
                .into_iter()
                .flat_map(|region| std::iter::once(region.outer).chain(region.holes))
                .collect();
            let wrapped = crate::profile_boolean::wrap_loops(&loops);
            for (u, v) in [(apex, 980.0), (1.2 * pi, 980.0), (1.8 * pi, 980.0)] {
                assert!(
                    crate::profile_boolean::point_in_loops(Point2::new(u, v), &wrapped),
                    "the lens is the other bore's material, at ({u}, {v})"
                );
            }
            for (u, v) in [(apex, 989.0), (apex, 971.0), (1.05 * pi, 985.0)] {
                assert!(
                    !crate::profile_boolean::point_in_loops(Point2::new(u, v), &wrapped),
                    "outside the lens is not, at ({u}, {v})"
                );
            }
        }
    }

    /// The traversal as it was before directed halfedges: a walk that marks
    /// the undirected piece used and continues into whichever branch is still
    /// unused. Kept in the tests only, to show this fixture has teeth.
    fn previous_traversal(welded: &[Segment]) -> Vec<Vec<Segment>> {
        let key = |point: Point2| (point.x.to_bits(), point.y.to_bits());
        let mut touching: std::collections::BTreeMap<(u64, u64), Vec<(usize, bool)>> =
            std::collections::BTreeMap::new();
        for (index, segment) in welded.iter().enumerate() {
            touching
                .entry(key(segment.start()))
                .or_default()
                .push((index, true));
            touching
                .entry(key(segment.end()))
                .or_default()
                .push((index, false));
        }
        let departure = |index: usize, at_start: bool| -> Option<Point2> {
            let piece = welded[index];
            let (from, to) = if at_start {
                (piece.start(), piece.point_at(0.05))
            } else {
                (piece.end(), piece.point_at(0.95))
            };
            let (dx, dy) = (to.x - from.x, to.y - from.y);
            let length = dx.hypot(dy);
            (length > 0.0).then(|| Point2::new(dx / length, dy / length))
        };
        let mut used = vec![false; welded.len()];
        let mut chains: Vec<Vec<Segment>> = Vec::new();
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
            let start_free = touching[&key(welded[start].start())].len() == 1;
            let end_free = touching[&key(welded[start].end())].len() == 1;
            let mut forward = start_free || !end_free;
            let mut chain = Vec::new();
            let mut cursor = start;
            loop {
                used[cursor] = true;
                let piece = if forward {
                    welded[cursor]
                } else {
                    welded[cursor].reversed()
                };
                let arrival = key(piece.end());
                let arriving = departure(cursor, !forward);
                chain.push(piece);
                let next = touching[&arrival]
                    .iter()
                    .copied()
                    .filter(|(candidate, _)| !used[*candidate])
                    .min_by(|left, right| {
                        let turn = |end: &(usize, bool)| {
                            let (Some(back), Some(out)) = (arriving, departure(end.0, end.1))
                            else {
                                return f64::INFINITY;
                            };
                            let angle = (back.x * out.y - back.y * out.x)
                                .atan2(back.x * out.x + back.y * out.y);
                            if angle <= 1.0e-12 {
                                angle + std::f64::consts::TAU
                            } else {
                                angle
                            }
                        };
                        turn(left).total_cmp(&turn(right))
                    });
                let Some((next, at_start)) = next else { break };
                forward = at_start;
                cursor = next;
            }
            chains.push(chain);
        }
        chains
    }

    /// The fixture has teeth: the walk this replaced does not describe the
    /// same curve when the same arcs arrive in a different order.
    #[test]
    fn the_previous_traversal_did_depend_on_the_order_its_arcs_arrived_in() {
        let arcs = four_harmonic_arcs();
        let presentations = every_presentation(&arcs);
        let fingerprint = |pieces: &Vec<Segment>| -> String {
            let chains = previous_traversal(pieces);
            let mut areas: Vec<i64> = chains
                .iter()
                .map(|chain| (signed_area(chain) * 1_000.0).round() as i64)
                .collect();
            areas.sort_unstable();
            format!("{} chain(s) enclosing {areas:?}", chains.len())
        };
        let first = fingerprint(&presentations[0]);
        assert!(
            presentations
                .iter()
                .any(|pieces| fingerprint(pieces) != first),
            "this fixture no longer catches the fault it was written for"
        );
    }
}

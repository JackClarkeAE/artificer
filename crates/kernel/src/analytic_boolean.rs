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
    ProfileBooleanError, ProfileRegion, chain_welded_segments, chord_region_pieces, point_in_loops,
    profile_boolean_multi, welded, wrap_loops,
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
    /// The faces meet along the curve two cylinders share (ADR 0047), and
    /// the section it leaves on one of them is a shape this closure does not
    /// assemble yet: a chain that enters and leaves the face by the same
    /// edge, or one that ends inside the window. The curve is exact and
    /// carried; what is missing is the step that turns it into a boundary.
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
    let named = |error: AnalyticBooleanError| match error {
        AnalyticBooleanError::DomainUnsupported
            if operands_share_a_trace(target, tool, precision) =>
        {
            AnalyticBooleanError::TraceUnclosed
        }
        other => other,
    };
    collect_operand_pieces(
        target,
        tool,
        operation,
        OperandSide::Target,
        precision,
        &mut pieces,
    )
    .map_err(named)?;
    collect_operand_pieces(
        tool,
        target,
        operation,
        OperandSide::Tool,
        precision,
        &mut pieces,
    )
    .map_err(named)?;
    if pieces.is_empty() {
        return Err(AnalyticBooleanError::EmptyResult);
    }
    sew_shells(&pieces, precision).map_err(|error| {
        named(match error {
            SewError::Inconsistent | SewError::Degenerate => {
                AnalyticBooleanError::DomainUnsupported
            }
        })
    })
}

/// Whether any face of one solid meets a face of the other along the curve
/// two cylinders share.
///
/// A failure anywhere in the engine is reported against that curve when the
/// operands have one, because it is the part of this pair the engine has
/// only just learned to carry, and saying "tangential or coincident" of a
/// quartic sends the reader looking for the wrong thing.
fn operands_share_a_trace(target: &Topology, tool: &Topology, precision: PrecisionPolicy) -> bool {
    target.faces.iter().any(|face| {
        tool.faces.iter().any(|other| {
            matches!(
                intersect(face.value.surface, other.value.surface, precision),
                Ok(SurfaceIntersection::Curves(ref curves))
                    if curves
                        .iter()
                        .any(|curve| matches!(curve, IntersectionCurve::Trace(_)))
            )
        })
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
        Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => return None,
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
/// Walks welded section pieces into oriented chains, each either closed or
/// running from one loose end to another.
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
fn trace_section_chains(welded: &[Segment]) -> Vec<Vec<Segment>> {
    // A halfedge is a piece walked one way: `2·index` forward, `+1` reversed.
    let count = welded.len();
    let oriented = |halfedge: usize| -> Segment {
        let piece = welded[halfedge / 2];
        if halfedge.is_multiple_of(2) {
            piece
        } else {
            piece.reversed()
        }
    };
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

    // A cycle that walks a halfedge and then its twin is going out along a
    // dangling run and coming back: that is an open chain, and the turn-backs
    // are where to cut it. A cycle with no turn-back is closed.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut rings: Vec<Vec<usize>> = Vec::new();
    for cycle in cycles {
        let length = cycle.len();
        let turns_back = |position: usize| cycle[position] == twin(cycle[(position + 1) % length]);
        let Some(cut) = (0..length).find(|position| turns_back(*position)) else {
            rings.push(cycle);
            continue;
        };
        // Start just after a turn-back, so the runs between turn-backs are
        // whole. A cycle whose walk happened to begin in the middle of a
        // dangling run would otherwise hand back that run's two halves as
        // though they were separate chains.
        let mut ordered = cycle;
        ordered.rotate_left(cut + 1);
        let mut run: Vec<usize> = Vec::new();
        for position in 0..length {
            run.push(ordered[position]);
            if ordered[position] == twin(ordered[(position + 1) % length]) {
                runs.push(std::mem::take(&mut run));
            }
        }
        if !run.is_empty() {
            runs.push(run);
        }
    }

    let mut chains: Vec<Vec<Segment>> = Vec::new();
    // A face boundary walks a dangling run once in each direction, because
    // there is no other face on the far side of it to walk it back. The run is
    // one chain, not two, so the return journey is dropped. Which of the pair
    // survives is then settled by the geometry rather than by which was walked
    // first: an open chain has no material side to hold it one way round, and
    // left to right is the way the periodic window is stitched.
    let mut kept: Vec<Vec<usize>> = Vec::new();
    for run in runs {
        let back: Vec<usize> = run.iter().rev().map(|halfedge| twin(*halfedge)).collect();
        if !kept.contains(&back) {
            kept.push(run);
        }
    }
    for run in kept {
        let walked: Vec<Segment> = run.into_iter().map(&oriented).collect();
        let (from, to) = (walked[0].start(), walked[walked.len() - 1].end());
        let forwards = from
            .x
            .total_cmp(&to.x)
            .then(from.y.total_cmp(&to.y))
            .is_le();
        chains.push(if forwards {
            walked
        } else {
            walked.iter().rev().map(|piece| piece.reversed()).collect()
        });
    }
    // Every closed cycle bounds a cell, with the cell on its left. The cells
    // that hold material wind positive; the arrangement's complement, and the
    // inside of every ring, wind negative. Keeping the positive ones keeps each
    // bounding loop exactly once — whether it stands alone, is one of several
    // disjoint loops, or is a ring, whose inner loop arrives positive as the
    // boundary of the cell it encloses. Nesting is then read off by
    // containment, which is `nest_section_loops`, not by winding.
    for ring in rings {
        let walked: Vec<Segment> = ring.into_iter().map(&oriented).collect();
        if chain_signed_area(&walked) > 0.0 {
            chains.push(walked);
        }
    }
    chains
}

/// The area a chain encloses in the face's own parameter space, sampled along
/// each arc so a harmonic's bow counts rather than only its chord.
fn chain_signed_area(chain: &[Segment]) -> f64 {
    let mut points: Vec<Point2> = Vec::new();
    for segment in chain {
        for step in 0..16 {
            points.push(segment.point_at(f64::from(step) / 16.0));
        }
    }
    let count = points.len();
    if count < 3 {
        return 0.0;
    }
    (0..count)
        .map(|index| {
            let (a, b) = (points[index], points[(index + 1) % count]);
            a.x.mul_add(b.y, -(b.x * a.y))
        })
        .sum::<f64>()
        * 0.5
}

/// Orders the open chains across a periodic window from low to high, by where
/// they run rather than by their average height.
///
/// The bands that close the window are cut between consecutive chains, so this
/// order decides which chain each band reaches from and to, and therefore which
/// side of each chain the section claims as the other solid's material.
///
/// Averaging a chain's height cannot do it. Two chains that are reflections of
/// one another about the same level average to the same number, and the two
/// traces of a Steinmetz seam on a bore wall are exactly that pair:
/// `v = 980 ± 8·cos u`, both averaging 980 to the last bit. The order then came
/// from the order the chains happened to arrive in, and the bands came out
/// spanning the lens between the traces instead of avoiding it — so the section
/// said the other solid's material was where its void is, and the face's 2D
/// Boolean was handed a hole where it should have been handed a region.
///
/// Comparing heights at sampled azimuths is a real order wherever the chains do
/// not cross inside the window, which is the case they have to be stackable in
/// anyway: two chains that cross part the window into more bands than there are
/// gaps between them. The Steinmetz pair crosses exactly on the seams, where
/// the window ends.
fn stack_open_chains(open: &mut [Vec<Segment>], u_min: f64, u_max: f64) {
    // A chain's height at one azimuth, by bisecting the piece that spans it.
    // Every chain reaches across the whole window, so every chain has a height
    // at every azimuth inside it.
    let height_at = |chain: &[Segment], u: f64| -> f64 {
        let piece = chain
            .iter()
            .find(|piece| piece.start().x <= u && u <= piece.end().x)
            .copied()
            .unwrap_or(chain[chain.len() / 2]);
        let (mut low, mut high) = (0.0_f64, 1.0_f64);
        for _ in 0..48 {
            let middle = 0.5 * (low + high);
            if piece.point_at(middle).x < u {
                low = middle;
            } else {
                high = middle;
            }
        }
        piece.point_at(0.5 * (low + high)).y
    };
    let profile = |chain: &[Segment]| -> Vec<f64> {
        (1..8)
            .map(|step| height_at(chain, (u_max - u_min).mul_add(f64::from(step) / 8.0, u_min)))
            .collect()
    };
    open.sort_by(|left, right| {
        profile(left)
            .into_iter()
            .zip(profile(right))
            .map(|(left, right)| left.total_cmp(&right))
            .find(|order| order.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

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
    // A trace can run across this face's seam without ever leaving the face
    // it was trimmed against, so a piece may straddle the window's edge. The
    // seam is a boundary of this face, and a piece that crosses it is two
    // pieces: one here, one on the face across it. A plane section never
    // needs this — its trace runs the whole period — so only a trace is cut.
    let mut pieces = pieces;
    for seam in [u_min, u_max] {
        let mut split = Vec::with_capacity(pieces.len());
        for piece in pieces {
            let (low, high) = (
                piece.start().x.min(piece.end().x),
                piece.start().x.max(piece.end().x),
            );
            if !matches!(piece, Segment::Trace { .. })
                || seam <= low + 1.0e-9
                || seam >= high - 1.0e-9
            {
                split.push(piece);
                continue;
            }
            match (
                piece.trace_to_abscissa(seam, false),
                piece.trace_to_abscissa(seam, true),
            ) {
                (Some(before), Some(after)) => {
                    split.push(before);
                    split.push(after);
                }
                _ => split.push(piece),
            }
        }
        pieces = split;
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
    let mut loops: Vec<Vec<Segment>> = Vec::new();
    let mut open: Vec<Vec<Segment>> = Vec::new();
    for mut chain in trace_section_chains(&welded) {
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
        // A chain that leaves one seam and returns to it takes a bite out of
        // the face's edge rather than crossing the face. A plane section
        // never does this — its trace runs the whole period — but the curve
        // of two cylinders does whenever the narrower one reaches the seam
        // without passing it. The bite closes along the seam itself, which
        // is a boundary the face already has.
        let same_seam =
            |seam: f64| (first.x - seam).abs() <= margin && (last.x - seam).abs() <= margin;
        if same_seam(u_min) || same_seam(u_max) {
            chain.push(Segment::Line {
                start: last,
                end: first,
            });
            loops.push(chain);
            continue;
        }
        if first.x > u_min + margin || last.x < u_max - margin {
            return Err(
                if chain.iter().any(|p| matches!(p, Segment::Trace { .. })) {
                    AnalyticBooleanError::TraceUnclosed
                } else {
                    AnalyticBooleanError::DomainUnsupported
                },
            );
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
            trace @ Segment::Trace { .. } => trace.trace_to_abscissa(to_x, at_start),
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
    stack_open_chains(&mut open, u_min, u_max);
    let lowest = &open[0];
    let probe = lowest[lowest.len() / 2].point_at(0.5);
    let step = (v_max - v_min).max(1.0) * 1.0e-3;
    let probe_point = cylinder.evaluate(Point2::new(probe.x, probe.y + step));
    // A section is a closure: where the other solid has a face on this very
    // carrier, the probe lies on that skin, and a ray's parity there is a
    // coin toss. The skin counts as covered, and only a probe off every
    // coincident face is asked of the solid's interior.
    let above_lowest =
        match on_coincident_face(&Surface::Cylinder(*cylinder), other, probe_point, precision) {
            Some(covered) => covered,
            None => {
                point_in_solid(other, probe_point).ok_or(AnalyticBooleanError::DomainUnsupported)?
            }
        };
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
        (Surface::Cylinder(cylinder), IntersectionCurve::Trace(trace)) => {
            // The curve exists only between its branch points, where the
            // discriminant is positive, so the turn is cut there first and
            // each surviving span becomes one piece. A face's own window may
            // sit on any whole turn, so each piece is offered on the three
            // branches a bounded window can reach, exactly as a ring chord is.
            let on_other = same_cylinder(cylinder, &trace.other);
            if !on_other && !same_cylinder(cylinder, &trace.host) {
                return None;
            }
            let tau = std::f64::consts::TAU;
            let interior = trace.branch_points(-tau, tau);
            let mut cuts = vec![-tau];
            cuts.extend(interior.iter().copied());
            cuts.push(tau);
            let mut pieces = Vec::new();
            for (index, window) in cuts.windows(2).enumerate() {
                let (from, to) = (window[0], window[1]);
                let (from_is_branch, to_is_branch) = (index > 0, index + 2 < cuts.len());
                if to - from <= 1.0e-9 {
                    continue;
                }
                let inside = trace.height_at(0.5 * (from + to)).is_some();
                if !inside {
                    continue;
                }
                for turns in [-1.0, 0.0, 1.0] {
                    let shift = Point2::new(turns * tau, 0.0);
                    let piece = Segment::Trace {
                        host: trace.host,
                        other: trace.other,
                        branch: trace.branch,
                        on_other,
                        shift,
                        from,
                        to,
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(0.0, 0.0),
                    };
                    let start = trace.endpoint(from, from_is_branch, on_other);
                    let end = trace.endpoint(to, to_is_branch, on_other);
                    let place = |point: Point2| Point2::new(point.x + shift.x, point.y + shift.y);
                    pieces.push(piece.with_endpoints(place(start), place(end)));
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
                // the other face is a change of which one it is read in, not
                // a change of curve. The parameter stays the host's azimuth
                // either way, which is what keeps the two faces' uses of the
                // edge welded.
                Segment::Trace {
                    host,
                    other,
                    branch,
                    from,
                    to,
                    ..
                } => {
                    let on_other = same_cylinder(cylinder, &other);
                    if !on_other && !same_cylinder(cylinder, &host) {
                        return None;
                    }
                    let carried = Segment::Trace {
                        host,
                        other,
                        branch,
                        on_other,
                        shift: Point2::new(0.0, 0.0),
                        from,
                        to,
                        start: Point2::new(0.0, 0.0),
                        end: Point2::new(0.0, 0.0),
                    };
                    Some(carried.with_endpoints(carried.point_at(0.0), carried.point_at(1.0)))
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
/// A point's parameters on a surface, for a point that lies on it.
fn surface_local(surface: &Surface, point: Point3) -> Option<Point2> {
    match surface {
        Surface::Plane(plane) => Some(Point2::new(
            (point - plane.origin).dot(plane.u),
            (point - plane.origin).dot(plane.v),
        )),
        Surface::Cylinder(cylinder) => {
            let axis = cylinder.axis / cylinder.axis.length();
            let offset = point - cylinder.origin;
            let height = offset.dot(axis);
            let radial = offset - axis * height;
            let angle = radial
                .dot(cylinder.radial_v)
                .atan2(radial.dot(cylinder.radial_u));
            Some(Point2::new(cylinder.angular_sign * angle, height))
        }
        _ => None,
    }
}

/// Whether a point of `carrier` lies on a face of `other` that shares that
/// carrier: `Some(true)` on such a face, `None` otherwise — off every such
/// face the point may still be inside the solid, which is the interior's
/// question and not the skin's.
fn on_coincident_face(
    carrier: &Surface,
    other: &Topology,
    point: Point3,
    precision: PrecisionPolicy,
) -> Option<bool> {
    let tau = std::f64::consts::TAU;
    for other_face in &other.faces {
        if !matches!(
            intersect(*carrier, other_face.value.surface, precision),
            Ok(SurfaceIntersection::Coincident)
        ) {
            continue;
        }
        let Ok(region) = face_region(other, &other_face.value) else {
            continue;
        };
        let Some(mut local) = surface_local(&other_face.value.surface, point) else {
            continue;
        };
        // On a periodic face the azimuth is asked on the face's own branch.
        if let (Surface::Cylinder(_), Some((low, high))) =
            (other_face.value.surface, azimuth_window(&region))
        {
            let turns = (((low + high) / 2.0 - local.x) / tau).round();
            local = Point2::new(local.x + turns * tau, local.y);
        }
        if point_in_loops(local, &wrap_loops(&region)) {
            return Some(true);
        }
    }
    None
}

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

/// Whether two cylinder records describe the same carrier, to the agreement
/// the rest of the engine uses rather than to the last bit.
fn same_cylinder(left: &Cylinder, right: &Cylinder) -> bool {
    let scale = left.radius.abs().max(right.radius.abs()).max(1.0);
    let tolerance = scale * 1.0e-9;
    let (Some(left_axis), Some(right_axis)) = (
        (left.axis.length() > f64::EPSILON).then(|| left.axis / left.axis.length()),
        (right.axis.length() > f64::EPSILON).then(|| right.axis / right.axis.length()),
    ) else {
        return false;
    };
    (left.radius - right.radius).abs() <= tolerance
        && left_axis.cross(right_axis).length() <= 1.0e-9
        && (right.origin - left.origin).cross(left_axis).length() <= tolerance
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
        // Mirroring the azimuth is exactly reversing the carriers' angular
        // sense: `radial(−x)` is what `radial(x)` becomes when the sign
        // flips, so the curve mirrors in parameter space with no reflection
        // of the carriers themselves.
        Segment::Trace {
            host,
            other,
            branch,
            on_other,
            shift,
            from,
            to,
            start,
            end,
        } => {
            let flip = |mut cylinder: crate::topology::Cylinder| {
                cylinder.angular_sign = -cylinder.angular_sign;
                cylinder
            };
            Segment::Trace {
                host: flip(host),
                other: flip(other),
                branch,
                on_other,
                shift: Point2::new(-shift.x, shift.y),
                from: -to,
                to: -from,
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

    /// What a presentation traces, as one comparable string: how many chains,
    /// what each encloses, and what each *is*.
    fn fingerprint(pieces: &[Segment]) -> String {
        let chains = trace_section_chains(pieces);
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
        let chains = trace_section_chains(&presentations[0]);
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

    /// A section that does not close is the ordinary case on a bore wall: the
    /// trace runs across the parameter window and out the other side, and the
    /// periodic stitching downstream closes it round the seams.
    ///
    /// A face boundary walks such a run once each way, because there is no
    /// second face on the far side of it to walk it back. Both journeys are the
    /// same chain, and handing back both doubles every open section — which
    /// `nest_section_loops` reads as a loop containing itself, so every depth
    /// comes out one too high and outer loops are taken for holes.
    #[test]
    fn an_open_run_is_one_chain_and_not_its_return_journey_as_well() {
        let corner = |from: (f64, f64), to: (f64, f64)| Segment::Line {
            start: Point2::new(from.0, from.1),
            end: Point2::new(to.0, to.1),
        };
        let path = vec![
            corner((0.0, 0.0), (1.0, 1.0)),
            corner((1.0, 1.0), (2.0, 0.0)),
            corner((2.0, 0.0), (3.0, 1.5)),
        ];
        let presentations = some_presentations(&path, 200);
        agreed(&presentations);

        let chains = trace_section_chains(&presentations[0]);
        assert_eq!(chains.len(), 1, "one run out and back is one chain");
        assert_eq!(chains[0].len(), 3, "and it is the whole run");
        assert!(
            chains[0][0].start().x < chains[0][2].end().x,
            "an open chain is handed back running left to right"
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

        let chains = trace_section_chains(&presentations[0]);
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
    /// average height — to the last bit, not merely close. Stacking them by
    /// that average is therefore not stacking them at all: the order comes from
    /// the order they arrived in, and it decides which chain each band is cut
    /// between. The bands then span the lens between the traces instead of
    /// avoiding it, and the section claims the other solid's material is
    /// exactly where its void is.
    #[test]
    fn two_traces_that_average_alike_are_still_stacked_by_where_they_run() {
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
        // fix.
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

        // However they arrive, the lower trace is the lower one.
        for arrival in [
            vec![upper.clone(), lower.clone()],
            vec![lower.clone(), upper.clone()],
        ] {
            let mut stacked = arrival;
            stack_open_chains(&mut stacked, u_min, u_max);
            let apex_of = |chain: &[Segment]| chain[0].end().y;
            assert!(
                apex_of(&stacked[0]) < apex_of(&stacked[1]),
                "stacked low to high, the first chain's apex ({}) must sit below \
                 the second's ({})",
                apex_of(&stacked[0]),
                apex_of(&stacked[1])
            );
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

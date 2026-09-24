//! Sheet constructions (ADR 0056, S1): a surface extrusion, a surface
//! revolve and a planar patch.
//!
//! Each is the solid builder it mirrors with the caps left off. A surface
//! extrusion sweeps an open or closed chain of lines and arcs along the
//! frame's normal into planes and cylinders, exactly as
//! [`crate::analytic_extrusion::build_analytic_extrusion`] sweeps a loop; a
//! surface revolve turns a chain about an axis into the bands
//! [`crate::section_revolve`] builds for a revolve, without the wedge faces
//! that close a partial turn; a planar patch is the cap face a profile
//! would have had. Nothing here is a new carrier: every face is one the
//! solid builders already write, and the validator holds it to the same
//! standard, less the closure a sheet does not have.

use std::f64::consts::TAU;

use artificer_protocol::{
    ExecuteRequest, KernelError, KernelErrorCode, MAX_PLANAR_PROFILE_CURVES, PlanarAxis2,
    PlanarCurve2, PlanarFrame3, PlanarProfile2, PrecisionPolicy, RevolveAngle, SnapshotId,
};

use crate::analytic_extrusion::{
    BoundaryUse, Frame, Segment, ValidatedAnalyticRegionExtrusion, adjacent_has_extra_contact,
    allocate_id, angle_on_arc, cap_pcurve, merge_topologies, normalize_frame, parse_curve,
    push_boundary_edge, push_cap_face, push_edge, push_loop, push_side_face, push_vertex,
    segment_clearance, validate_analytic_profile_extrusion,
};
use crate::planar_profile::PlanarProfileInputError;
use crate::section_revolve::{RzSection, build_turned_sheet};
use crate::sheet::{self, SheetResult};
use crate::topology::{
    Edge, FaceKey, FaceRole, Orientation, Plane, Point2, Record, Shell, Surface, Topology,
};
use crate::{CancellationToken, ExecutionOutcome, Snapshot, planar_profile_input_error};

/// An open or closed chain of exact segments, joined end to end.
#[derive(Clone, Debug)]
pub(crate) struct Chain {
    pub(crate) segments: Vec<Segment>,
    /// Whether the last segment ends where the first begins.
    pub(crate) closed: bool,
}

/// Why a chain was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChainError {
    Empty,
    TooLong,
    Disconnected,
    SelfIntersects,
    Spline,
    /// A piece the profile parser refused: too small, not finite, a whole
    /// circle among other curves.
    Curve(PlanarProfileInputError),
}

impl ChainError {
    fn refuse(self, snapshot: SnapshotId, what: &str) -> KernelError {
        let (code, name, message) = match self {
            Self::Empty => (
                KernelErrorCode::InvalidInput,
                "SURFACE_CHAIN_EMPTY",
                format!("{what} needs at least one line or arc to sweep"),
            ),
            Self::TooLong => (
                KernelErrorCode::ResourceLimitExceeded,
                "SURFACE_CHAIN_TOO_LONG",
                format!("{what} takes at most {MAX_PLANAR_PROFILE_CURVES} pieces in one chain"),
            ),
            Self::Disconnected => (
                KernelErrorCode::InvalidInput,
                "SURFACE_CHAIN_DISCONNECTED",
                format!(
                    "{what} sweeps one chain: every piece must start exactly where the piece \
                     before it ends"
                ),
            ),
            Self::SelfIntersects => (
                KernelErrorCode::InvalidInput,
                "SURFACE_CHAIN_SELF_INTERSECTS",
                format!("{what}'s chain crosses or touches itself, so the sheet would too"),
            ),
            Self::Spline => (
                KernelErrorCode::Unsupported,
                "SURFACE_CHAIN_SPLINE_UNSUPPORTED",
                format!(
                    "{what} sweeps lines and arcs; a spline in the chain would sweep a B-spline \
                     sheet, which this release does not build"
                ),
            ),
            Self::Curve(reason) => return planar_profile_input_error(snapshot, reason),
        };
        sheet::refuse(snapshot, code, name, message)
    }
}

/// Reads a chain of lines and arcs, or one whole circle, as exact segments
/// joined end to end, and checks that it does not touch itself anywhere but
/// at the joins.
pub(crate) fn parse_chain(
    curves: &[PlanarCurve2],
    precision: PrecisionPolicy,
) -> Result<Chain, ChainError> {
    if curves.is_empty() {
        return Err(ChainError::Empty);
    }
    if curves.len() > MAX_PLANAR_PROFILE_CURVES {
        return Err(ChainError::TooLong);
    }
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let agreement = precision.linear_agreement;
    if curves
        .iter()
        .any(|curve| matches!(curve, PlanarCurve2::Circle { .. }))
    {
        // A whole circle is a chain of its own: two semicircles, so the
        // sheet it sweeps has two seams like every other closed carrier.
        let [PlanarCurve2::Circle {
            center,
            radius,
            direction,
        }] = curves
        else {
            return Err(ChainError::Disconnected);
        };
        if *radius <= minimum {
            return Err(ChainError::Curve(PlanarProfileInputError::Extrusion(
                crate::extrusion::ExtrusionInputError::FeatureTooSmall,
            )));
        }
        let center = Point2::new(center.x, center.y);
        let sign = match direction {
            artificer_protocol::ArcDirection::CounterClockwise => 1.0,
            artificer_protocol::ArcDirection::Clockwise => -1.0,
        };
        let positive = Point2::new(center.x + radius, center.y);
        let negative = Point2::new(center.x - radius, center.y);
        return Ok(Chain {
            segments: vec![
                Segment::Arc {
                    center,
                    start: positive,
                    end: negative,
                    radius: *radius,
                    start_angle: 0.0,
                    sweep: sign * std::f64::consts::PI,
                },
                Segment::Arc {
                    center,
                    start: negative,
                    end: positive,
                    radius: *radius,
                    start_angle: sign * std::f64::consts::PI,
                    sweep: sign * std::f64::consts::PI,
                },
            ],
            closed: true,
        });
    }
    let mut segments = Vec::with_capacity(curves.len());
    for curve in curves {
        segments.push(parse_curve(curve, minimum, agreement).map_err(|reason| match reason {
            PlanarProfileInputError::SplineCurve => ChainError::Spline,
            other => ChainError::Curve(other),
        })?);
    }
    for pair in segments.windows(2) {
        if pair[0].end() != pair[1].start() {
            return Err(ChainError::Disconnected);
        }
    }
    let count = segments.len();
    let closed = count >= 2 && segments[count - 1].end() == segments[0].start();
    for first in 0..count {
        for second in first + 1..count {
            let consecutive = second == first + 1;
            let wraps = closed && first == 0 && second + 1 == count;
            let invalid_contact = if consecutive || wraps {
                let mut allowed = vec![if consecutive {
                    segments[first].end()
                } else {
                    segments[first].start()
                }];
                if closed && count == 2 {
                    allowed.push(segments[first].start());
                }
                adjacent_has_extra_contact(segments[first], segments[second], &allowed, agreement)
            } else {
                segment_clearance(segments[first], segments[second]) <= minimum
            };
            if invalid_contact {
                return Err(ChainError::SelfIntersects);
            }
        }
    }
    if segments
        .iter()
        .any(|segment| !segment.length().is_finite())
    {
        return Err(ChainError::Curve(PlanarProfileInputError::Extrusion(
            crate::extrusion::ExtrusionInputError::NumericallyIndeterminate,
        )));
    }
    Ok(Chain { segments, closed })
}

/// The walls a chain sweeps along the frame's normal: one plane per line
/// and one cylinder per arc, sharing their generators, in one open shell.
pub(crate) fn build_surface_extrusion(frame: Frame, chain: &Chain, distance: f64) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let count = chain.segments.len();
    let vertex_count = if chain.closed { count } else { count + 1 };
    let station = |index: usize| -> Point2 {
        if index < count {
            chain.segments[index].start()
        } else {
            chain.segments[count - 1].end()
        }
    };
    let bottom: Vec<_> = (0..vertex_count)
        .map(|index| push_vertex(&mut topology, &mut next_id, frame.point(station(index), 0.0)))
        .collect();
    let top: Vec<_> = (0..vertex_count)
        .map(|index| {
            push_vertex(
                &mut topology,
                &mut next_id,
                frame.point(station(index), distance),
            )
        })
        .collect();
    let next = |index: usize| {
        if chain.closed {
            (index + 1) % count
        } else {
            index + 1
        }
    };
    let bottom_edges: Vec<_> = chain
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            push_boundary_edge(
                &mut topology,
                &mut next_id,
                [bottom[index], bottom[next(index)]],
                *segment,
                frame,
                0.0,
            )
        })
        .collect();
    let top_edges: Vec<_> = chain
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            push_boundary_edge(
                &mut topology,
                &mut next_id,
                [top[index], top[next(index)]],
                *segment,
                frame,
                distance,
            )
        })
        .collect();
    let vertical: Vec<_> = (0..vertex_count)
        .map(|index| {
            let start = topology.vertices[bottom[index].0].value.point;
            let end = topology.vertices[top[index].0].value.point;
            push_edge(
                &mut topology,
                &mut next_id,
                Edge::line([bottom[index], top[index]], [start, end]),
            )
        })
        .collect();
    for (index, segment) in chain.segments.iter().enumerate() {
        push_side_face(
            &mut topology,
            &mut next_id,
            (frame, distance),
            *segment,
            [
                bottom_edges[index],
                vertical[next(index)],
                top_edges[index],
                vertical[index],
            ],
            FaceRole::ExtrusionSide(index as u32),
        );
    }
    push_shell(&mut topology, &mut next_id);
    topology
}

/// One planar face per region: the cap a profile extrusion would have had
/// at the frame, facing the way the frame does.
pub(crate) fn build_planar_patch(regions: &[ValidatedAnalyticRegionExtrusion]) -> Topology {
    merge_topologies(regions.iter().map(build_patch_region).collect())
}

fn build_patch_region(region: &ValidatedAnalyticRegionExtrusion) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let mut loops = Vec::with_capacity(region.loops.len());
    for profile_loop in &region.loops {
        let count = profile_loop.segments.len();
        let vertices: Vec<_> = profile_loop
            .segments
            .iter()
            .map(|segment| {
                push_vertex(
                    &mut topology,
                    &mut next_id,
                    region.frame.point(segment.start(), 0.0),
                )
            })
            .collect();
        let edges: Vec<_> = profile_loop
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                push_boundary_edge(
                    &mut topology,
                    &mut next_id,
                    [vertices[index], vertices[(index + 1) % count]],
                    *segment,
                    region.frame,
                    0.0,
                )
            })
            .collect();
        let uses = profile_loop
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| BoundaryUse {
                edge: edges[index],
                orientation: Orientation::Forward,
                curve: cap_pcurve(*segment, false, false),
            })
            .collect();
        loops.push(push_loop(&mut topology, &mut next_id, uses));
    }
    push_cap_face(
        &mut topology,
        &mut next_id,
        Surface::Plane(Plane::new(
            region.frame.origin,
            region.frame.u,
            region.frame.v,
        )),
        &loops,
        FaceRole::ExtrusionTop,
    );
    push_shell(&mut topology, &mut next_id);
    topology
}

fn push_shell(topology: &mut Topology, next_id: &mut u64) {
    topology.shells.push(Record {
        id: allocate_id(next_id),
        value: Shell {
            faces: (0..topology.faces.len()).map(FaceKey).collect(),
        },
    });
}

// ---------------------------------------------------------------------------
// Surface revolve
// ---------------------------------------------------------------------------

/// Why a chain could not be turned about an axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RevolveChainError {
    DegenerateAxis,
    CrossesAxis,
    PinchedOnAxis,
    SegmentOnAxis,
    ArcCentreAcrossAxis,
    AngleInvalid,
    CoordinateLimit,
}

impl RevolveChainError {
    fn refuse(self, snapshot: SnapshotId) -> KernelError {
        let (name, message) = match self {
            Self::DegenerateAxis => (
                "SURFACE_REVOLVE_AXIS_DEGENERATE",
                "The revolve axis's two points coincide, so there is no axis.",
            ),
            Self::CrossesAxis => (
                "SURFACE_REVOLVE_CROSSES_AXIS",
                "The chain reaches both sides of the axis; turned, the sheet would pass through itself.",
            ),
            Self::PinchedOnAxis => (
                "SURFACE_REVOLVE_PINCHED_ON_AXIS",
                "The chain touches the axis between its ends, so the turned sheet would be pinched to a point there; only the ends of the chain may lie on the axis.",
            ),
            Self::SegmentOnAxis => (
                "SURFACE_REVOLVE_SEGMENT_ON_AXIS",
                "A piece of the chain lies along the axis and sweeps nothing.",
            ),
            Self::ArcCentreAcrossAxis => (
                "SURFACE_REVOLVE_ARC_CENTRE_ACROSS_AXIS",
                "An arc is centred across the axis, so it would turn into the inner lemon of a spindle torus, which is not a carrier of this kernel.",
            ),
            Self::AngleInvalid => (
                "SURFACE_REVOLVE_ANGLE_INVALID",
                "A partial turn sweeps strictly between nothing and a full turn, from a finite start within a turn, and both the sweep and the gap it leaves must clear the minimum feature size.",
            ),
            Self::CoordinateLimit => (
                "SURFACE_REVOLVE_COORDINATE_LIMIT",
                "The turned sheet would reach past the precision policy's coordinate limit.",
            ),
        };
        sheet::refuse(snapshot, KernelErrorCode::InvalidInput, name, message)
    }
}

/// Rewrites a chain in the frame as an `(r, z)` section about the axis,
/// with the sweep it turns through, by the rules a revolved profile obeys.
fn section_from_chain(
    frame: Frame,
    chain: &Chain,
    axis: PlanarAxis2,
    angle: RevolveAngle,
    precision: PrecisionPolicy,
) -> Result<(RzSection, f64), RevolveChainError> {
    let turn = match angle {
        RevolveAngle::FullTurn => None,
        RevolveAngle::Partial { start, sweep } => {
            if !start.is_finite() || !sweep.is_finite() || start.abs() > TAU {
                return Err(RevolveChainError::AngleInvalid);
            }
            if sweep <= 0.0 || sweep >= TAU {
                return Err(RevolveChainError::AngleInvalid);
            }
            Some((start, sweep))
        }
    };
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let origin = Point2::new(axis.start.x, axis.start.y);
    let span = Point2::new(axis.end.x - axis.start.x, axis.end.y - axis.start.y);
    let length = span.x.hypot(span.y);
    if !axis.is_finite() || !length.is_finite() || length <= minimum {
        return Err(RevolveChainError::DegenerateAxis);
    }
    let mut along = Point2::new(span.x / length, span.y / length);
    let mut radial = Point2::new(along.y, -along.x);
    let distance = |left: Point2, right: Point2| (left.x - right.x).hypot(left.y - right.y);
    let reach = chain
        .segments
        .iter()
        .map(|segment| {
            let ends = distance(segment.start(), origin).max(distance(segment.end(), origin));
            match *segment {
                Segment::Arc { center, radius, .. } => ends.max(distance(center, origin) + radius),
                _ => ends,
            }
        })
        .fold(0.0_f64, f64::max);
    let axis_place = frame.point(origin, 0.0);
    if [axis_place.x, axis_place.y, axis_place.z]
        .into_iter()
        .any(|value| !value.is_finite() || value.abs() + reach > precision.max_abs_coordinate)
    {
        return Err(RevolveChainError::CoordinateLimit);
    }
    let radius_of = |point: Point2, radial: Point2| {
        (point.x - origin.x).mul_add(radial.x, (point.y - origin.y) * radial.y)
    };
    let extent = chain
        .segments
        .iter()
        .flat_map(|segment| [segment.start(), segment.end()])
        .fold(1.0_f64, |extent, point| {
            extent.max(point.x.abs().max(point.y.abs()))
        });
    let on_axis = precision.linear_agreement.max(1.0e-12) * extent;
    let side = |radial: Point2| {
        let toward = radial.y.atan2(radial.x);
        chain
            .segments
            .iter()
            .flat_map(|segment| {
                let bulges = match *segment {
                    Segment::Arc {
                        center,
                        radius,
                        start_angle,
                        sweep,
                        ..
                    } => [toward, toward + std::f64::consts::PI].map(|angle| {
                        angle_on_arc(angle, start_angle, sweep, 0.0).then(|| {
                            Point2::new(
                                radius.mul_add(angle.cos(), center.x),
                                radius.mul_add(angle.sin(), center.y),
                            )
                        })
                    }),
                    _ => [None, None],
                };
                [Some(segment.start()), Some(segment.end())]
                    .into_iter()
                    .chain(bulges)
                    .flatten()
            })
            .map(|point| radius_of(point, radial))
            .fold((false, false), |(negative, positive), radius| {
                (negative || radius < -on_axis, positive || radius > on_axis)
            })
    };
    let reversed_axis = match side(radial) {
        (true, true) => return Err(RevolveChainError::CrossesAxis),
        (true, false) => {
            along = Point2::new(-along.x, -along.y);
            radial = Point2::new(along.y, -along.x);
            true
        }
        _ => false,
    };
    let to_section = |point: Point2| {
        let radius = radius_of(point, radial);
        Point2::new(
            if radius <= on_axis { 0.0 } else { radius },
            (point.x - origin.x).mul_add(along.x, (point.y - origin.y) * along.y),
        )
    };
    let centre_to_section = |point: Point2| {
        let radius = radius_of(point, radial);
        Point2::new(
            if radius.abs() <= on_axis { 0.0 } else { radius },
            (point.x - origin.x).mul_add(along.x, (point.y - origin.y) * along.y),
        )
    };
    let phase = radial.y.atan2(radial.x);
    let mut section = Vec::with_capacity(chain.segments.len());
    for segment in &chain.segments {
        let start = to_section(segment.start());
        let end = to_section(segment.end());
        section.push(match *segment {
            Segment::Line { .. } => {
                if start.x <= 0.0 && end.x <= 0.0 {
                    return Err(RevolveChainError::SegmentOnAxis);
                }
                Segment::Line { start, end }
            }
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            } => {
                let center = centre_to_section(center);
                if center.x < 0.0 {
                    return Err(RevolveChainError::ArcCentreAcrossAxis);
                }
                Segment::Arc {
                    center,
                    start,
                    end,
                    radius,
                    start_angle: start_angle - phase,
                    sweep,
                }
            }
            Segment::Ellipse { .. } | Segment::Harmonic { .. } | Segment::Trace { .. } => {
                unreachable!("a chain carries lines and arcs only")
            }
        });
    }
    // The axis may be touched only at the chain's ends: anywhere else the
    // turned sheet is pinched to a point, and no shell is.
    let corner_on_axis = section.windows(2).any(|pair| pair[0].end().x <= 0.0)
        || (chain.closed && section[0].start().x <= 0.0);
    let bulge_on_axis = section.iter().any(|segment| match *segment {
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            ..
        } => {
            let toward = start_angle + (std::f64::consts::PI - start_angle).rem_euclid(TAU);
            let progress = if sweep > 0.0 {
                (toward - start_angle) / sweep
            } else {
                (start_angle - toward).rem_euclid(TAU) / -sweep
            };
            progress > 0.0 && progress < 1.0 && center.x - radius <= on_axis
        }
        _ => false,
    });
    if corner_on_axis || bulge_on_axis {
        return Err(RevolveChainError::PinchedOnAxis);
    }
    let (begin, sweep) = match turn {
        None => (0.0, TAU),
        Some((start, sweep)) => {
            let outermost = section
                .iter()
                .map(|segment| {
                    let ends = segment.start().x.max(segment.end().x);
                    match *segment {
                        Segment::Arc { center, radius, .. } => ends.max(center.x + radius),
                        _ => ends,
                    }
                })
                .fold(0.0_f64, f64::max);
            if sweep * outermost < minimum || (TAU - sweep) * outermost < minimum {
                return Err(RevolveChainError::AngleInvalid);
            }
            if reversed_axis {
                (-(start + sweep), sweep)
            } else {
                (start, sweep)
            }
        }
    };
    let center = frame.point(origin, 0.0);
    let axis_direction = frame.u * along.x + frame.v * along.y;
    let profile_radial = frame.u * radial.x + frame.v * radial.y;
    let profile_tangent = axis_direction.cross(profile_radial);
    let (radial_u, radial_v) = if begin == 0.0 {
        (profile_radial, profile_tangent)
    } else {
        let radial_u = profile_radial * begin.cos() + profile_tangent * begin.sin();
        (radial_u, axis_direction.cross(radial_u))
    };
    let roles = (0..section.len())
        .map(|index| FaceRole::ExtrusionSide(index as u32))
        .collect();
    Ok((
        RzSection::from_parts(
            center,
            axis_direction,
            radial_u,
            radial_v,
            section,
            roles,
            chain.closed,
        ),
        sweep,
    ))
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

fn require_empty(input: &Snapshot) -> Result<(), KernelError> {
    crate::validate_extrusion_source(input)
}

fn frame_of(
    snapshot: SnapshotId,
    frame: PlanarFrame3,
    precision: PrecisionPolicy,
) -> Result<Frame, KernelError> {
    if !frame.is_finite() {
        return Err(planar_profile_input_error(
            snapshot,
            PlanarProfileInputError::Extrusion(crate::extrusion::ExtrusionInputError::NonFinite),
        ));
    }
    normalize_frame(frame, precision).map_err(|reason| planar_profile_input_error(snapshot, reason))
}

/// `KernelCommand::SurfaceExtrude`.
pub(crate) fn execute_surface_extrude(
    input: &Snapshot,
    request: &ExecuteRequest,
    cancellation: &CancellationToken,
    frame: PlanarFrame3,
    chain: &[PlanarCurve2],
    distance: f64,
) -> Result<ExecutionOutcome, KernelError> {
    require_empty(input)?;
    let precision = request.precision;
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if !distance.is_finite() || distance.abs() <= minimum {
        return Err(sheet::refuse(
            input.id,
            KernelErrorCode::InvalidInput,
            "SURFACE_DISTANCE_INVALID",
            "A surface extrusion's distance must be a finite length beyond the minimum feature size, in either direction.",
        ));
    }
    let frame = frame_of(input.id, frame, precision)?;
    let chain = parse_chain(chain, precision)
        .map_err(|reason| reason.refuse(input.id, "A surface extrusion"))?;
    let limit = precision.max_abs_coordinate;
    let out_of_range = chain.segments.iter().any(|segment| {
        [segment.start(), segment.end()]
            .into_iter()
            .flat_map(|point| {
                let low = frame.point(point, 0.0);
                let high = frame.point(point, distance);
                [low.x, low.y, low.z, high.x, high.y, high.z]
            })
            .any(|value| !value.is_finite() || value.abs() > limit)
    });
    if out_of_range {
        return Err(planar_profile_input_error(
            input.id,
            PlanarProfileInputError::Extrusion(
                crate::extrusion::ExtrusionInputError::CoordinateLimit,
            ),
        ));
    }
    // A negative distance sweeps the other way: the same walls, built up
    // from the far frame. Every wall's normal is the chain's tangent
    // crossed with the frame's normal wherever the wall starts, so the
    // sheet faces the chain's right-hand side either way.
    let topology = if distance > 0.0 {
        build_surface_extrusion(frame, &chain, distance)
    } else {
        let far = Frame {
            origin: frame.origin + frame.normal * distance,
            u: frame.u,
            v: frame.v,
            normal: frame.normal,
        };
        build_surface_extrusion(far, &chain, -distance)
    };
    sheet::commit(
        input,
        request,
        cancellation,
        SheetResult {
            topology,
            rung: sheet::SURFACE_EXTRUDE_RUNG,
            warnings: Vec::new(),
        },
    )
}

/// `KernelCommand::PlanarPatch`.
pub(crate) fn execute_planar_patch(
    input: &Snapshot,
    request: &ExecuteRequest,
    cancellation: &CancellationToken,
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
) -> Result<ExecutionOutcome, KernelError> {
    require_empty(input)?;
    // A patch has no depth; the profile is certified as the cap of a unit
    // extrusion, which checks everything a face needs and nothing more.
    let regions = validate_analytic_profile_extrusion(frame, profile, 1.0, request.precision)
        .map_err(|reason| planar_profile_input_error(input.id, reason))?;
    let topology = build_planar_patch(&regions.regions);
    sheet::commit(
        input,
        request,
        cancellation,
        SheetResult {
            topology,
            rung: sheet::PLANAR_PATCH_RUNG,
            warnings: Vec::new(),
        },
    )
}

/// `KernelCommand::SurfaceRevolve`.
pub(crate) fn execute_surface_revolve(
    input: &Snapshot,
    request: &ExecuteRequest,
    cancellation: &CancellationToken,
    frame: PlanarFrame3,
    chain: &[PlanarCurve2],
    axis: PlanarAxis2,
    angle: RevolveAngle,
) -> Result<ExecutionOutcome, KernelError> {
    require_empty(input)?;
    let precision = request.precision;
    let frame = frame_of(input.id, frame, precision)?;
    let chain =
        parse_chain(chain, precision).map_err(|reason| reason.refuse(input.id, "A surface revolve"))?;
    let (section, sweep) = section_from_chain(frame, &chain, axis, angle, precision)
        .map_err(|reason| reason.refuse(input.id))?;
    let topology = build_turned_sheet(&section, sweep);
    sheet::commit(
        input,
        request,
        cancellation,
        SheetResult {
            topology,
            rung: sheet::SURFACE_REVOLVE_RUNG,
            warnings: Vec::new(),
        },
    )
}

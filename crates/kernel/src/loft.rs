//! Loft between a planar profile and its offset section: the first rung of
//! the loft ladder, and the exact form of a drafted extrusion.
//!
//! The top section is the profile's mitred offset, so every straight edge and
//! its offset are parallel and sweep one plane, and every arc and its
//! concentric offset sweep one cone. Both are in the analytic vocabulary, so
//! the result is certified like any extrusion rather than approximated.
//!
//! The domain is exactly the profiles whose offset keeps that structure. At a
//! sharp corner between two straight edges the walls meet in a straight mitre
//! line. At a sharp corner involving an arc the offset arc's endpoint slides
//! along the arc, the wall's side edge is no longer a cone generator, and the
//! two walls would meet in a conic — outside the vocabulary, so it is refused
//! by name. Tangent junctions, including the two halves of a full circle, are
//! always fine.

use artificer_protocol::{PlanarFrame3, PlanarProfile2, PrecisionPolicy};

use crate::analytic_extrusion::{
    AnalyticLoop, BoundaryUse, Frame, Segment, allocate_id, cap_pcurve, merge_topologies,
    push_boundary_edge, push_cap_face, push_edge, push_loop, push_vertex, reversed_loop,
    validate_analytic_profile_extrusion,
};
use crate::loop_offset::{LoopOffsetError, ReflexPolicy, SpineVertexKind, mitred_offset};
use crate::planar_profile::PlanarProfileInputError;
use crate::topology::{
    Cone, Curve2, Edge, EdgeKey, Face, FaceKey, FaceRole, Orientation, Plane, Point2, Record,
    Shell, ShellKey, Solid, Surface, Topology, VertexKey,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LoftInputError {
    Profile(PlanarProfileInputError),
    OffsetNonFinite,
    /// The offset section collapsed, crossed itself, or could not be formed.
    OffsetInfeasible(LoopOffsetError),
    /// A sharp corner involves an arc, so the walls would meet in a conic.
    CornerNotTangent,
    /// The offset section leaves the certified coordinate range.
    CoordinateLimit,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedOffsetLoft {
    pub(crate) regions: Vec<OffsetLoftRegion>,
}

#[derive(Clone, Debug)]
pub(crate) struct OffsetLoftRegion {
    pub(crate) frame: Frame,
    pub(crate) distance: f64,
    pub(crate) loops: Vec<LoftLoop>,
}

/// One boundary loop and its offset, segment for segment: `top[i]` is the
/// offset of `base.segments[i]` and keeps its carrier kind.
#[derive(Clone, Debug)]
pub(crate) struct LoftLoop {
    pub(crate) base: AnalyticLoop,
    pub(crate) top: Vec<Segment>,
}

pub(crate) fn validate_offset_loft(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    distance: f64,
    offset: f64,
    precision: PrecisionPolicy,
) -> Result<ValidatedOffsetLoft, LoftInputError> {
    if !offset.is_finite() {
        return Err(LoftInputError::OffsetNonFinite);
    }
    let extrusion = validate_analytic_profile_extrusion(frame, profile, distance, precision)
        .map_err(LoftInputError::Profile)?;
    let coordinate_limit = precision.max_abs_coordinate;
    let mut regions = Vec::with_capacity(extrusion.regions.len());
    for region in extrusion.regions {
        let mut loops = Vec::with_capacity(region.loops.len());
        for (index, base) in region.loops.into_iter().enumerate() {
            // The offset routine walks counter-clockwise loops. The outer
            // boundary already does; a hole runs clockwise, so it is walked
            // reversed and its offset reversed back. Growing the section by
            // `offset` moves the outer boundary outward and every hole
            // boundary inward by the same amount.
            let is_outer = index == 0;
            let source = if is_outer {
                base.segments.clone()
            } else {
                reversed_loop(base.clone()).segments
            };
            let inward = if is_outer { -offset } else { offset };
            let spine = mitred_offset(&source, inward, ReflexPolicy::MitreLines, precision)
                .map_err(|error| match error {
                    LoopOffsetError::ReflexSharpCorner => LoftInputError::CornerNotTangent,
                    other => LoftInputError::OffsetInfeasible(other),
                })?;
            let count = source.len();
            for (vertex, kind) in spine.vertices.iter().enumerate() {
                if matches!(kind, SpineVertexKind::Tangent) {
                    continue;
                }
                let previous = (vertex + count - 1) % count;
                let straight = |segment: Segment| matches!(segment, Segment::Line { .. });
                if !(straight(source[previous]) && straight(source[vertex])) {
                    return Err(LoftInputError::CornerNotTangent);
                }
            }
            // Keep each arc's own angular range for its offset: the offset is
            // concentric and, at tangent junctions, spans exactly the same
            // angles, so the cone's rings share one parameterization.
            let top_ccw = spine
                .segments
                .iter()
                .zip(&source)
                .map(
                    |(offset_segment, base_segment)| match (offset_segment, base_segment) {
                        (
                            Segment::Arc {
                                center,
                                start,
                                end,
                                radius,
                                ..
                            },
                            Segment::Arc {
                                start_angle, sweep, ..
                            },
                        ) => Segment::Arc {
                            center: *center,
                            start: *start,
                            end: *end,
                            radius: *radius,
                            start_angle: *start_angle,
                            sweep: *sweep,
                        },
                        _ => *offset_segment,
                    },
                )
                .collect::<Vec<_>>();
            let top = if is_outer {
                top_ccw
            } else {
                reversed_loop(AnalyticLoop {
                    segments: top_ccw,
                    signed_area: 0.0,
                })
                .segments
            };
            for segment in &top {
                let start = region.frame.point(segment.start(), distance);
                let end = region.frame.point(segment.end(), distance);
                if [start.x, start.y, start.z, end.x, end.y, end.z]
                    .into_iter()
                    .any(|value| !value.is_finite() || value.abs() > coordinate_limit)
                {
                    return Err(LoftInputError::CoordinateLimit);
                }
            }
            loops.push(LoftLoop { base, top });
        }
        regions.push(OffsetLoftRegion {
            frame: region.frame,
            distance: region.distance,
            loops,
        });
    }
    Ok(ValidatedOffsetLoft { regions })
}

pub(crate) fn build_offset_loft(loft: &ValidatedOffsetLoft) -> Topology {
    merge_topologies(loft.regions.iter().map(build_region).collect())
}

struct LoopKeys {
    bottom_vertices: Vec<VertexKey>,
    top_vertices: Vec<VertexKey>,
    bottom_edges: Vec<EdgeKey>,
    top_edges: Vec<EdgeKey>,
    slant_edges: Vec<EdgeKey>,
}

fn build_region(region: &OffsetLoftRegion) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let frame = region.frame;
    let distance = region.distance;
    let mut loop_keys = Vec::with_capacity(region.loops.len());

    for loft_loop in &region.loops {
        let base = &loft_loop.base.segments;
        let top = &loft_loop.top;
        let count = base.len();
        let mut keys = LoopKeys {
            bottom_vertices: Vec::with_capacity(count),
            top_vertices: Vec::with_capacity(count),
            bottom_edges: Vec::with_capacity(count),
            top_edges: Vec::with_capacity(count),
            slant_edges: Vec::with_capacity(count),
        };
        for segment in base {
            keys.bottom_vertices.push(push_vertex(
                &mut topology,
                &mut next_id,
                frame.point(segment.start(), 0.0),
            ));
        }
        for segment in top {
            keys.top_vertices.push(push_vertex(
                &mut topology,
                &mut next_id,
                frame.point(segment.start(), distance),
            ));
        }
        for (index, segment) in base.iter().copied().enumerate() {
            let next = (index + 1) % count;
            keys.bottom_edges.push(push_boundary_edge(
                &mut topology,
                &mut next_id,
                [keys.bottom_vertices[index], keys.bottom_vertices[next]],
                segment,
                frame,
                0.0,
            ));
        }
        for (index, segment) in top.iter().copied().enumerate() {
            let next = (index + 1) % count;
            keys.top_edges.push(push_boundary_edge(
                &mut topology,
                &mut next_id,
                [keys.top_vertices[index], keys.top_vertices[next]],
                segment,
                frame,
                distance,
            ));
        }
        for index in 0..count {
            let start = topology.vertices[keys.bottom_vertices[index].0].value.point;
            let end = topology.vertices[keys.top_vertices[index].0].value.point;
            keys.slant_edges.push(push_edge(
                &mut topology,
                &mut next_id,
                Edge::line(
                    [keys.bottom_vertices[index], keys.top_vertices[index]],
                    [start, end],
                ),
            ));
        }
        loop_keys.push(keys);
    }

    let bottom_loops = region
        .loops
        .iter()
        .zip(&loop_keys)
        .map(|(loft_loop, keys)| {
            let uses = (0..loft_loop.base.segments.len())
                .rev()
                .map(|index| BoundaryUse {
                    edge: keys.bottom_edges[index],
                    orientation: Orientation::Reverse,
                    curve: cap_pcurve(loft_loop.base.segments[index], true, true),
                })
                .collect::<Vec<_>>();
            push_loop(&mut topology, &mut next_id, uses)
        })
        .collect::<Vec<_>>();
    push_cap_face(
        &mut topology,
        &mut next_id,
        Surface::Plane(Plane::new(frame.origin, frame.v, frame.u)),
        &bottom_loops,
        FaceRole::ExtrusionBottom,
    );

    let top_loops = region
        .loops
        .iter()
        .zip(&loop_keys)
        .map(|(loft_loop, keys)| {
            let uses = loft_loop
                .top
                .iter()
                .copied()
                .enumerate()
                .map(|(index, segment)| BoundaryUse {
                    edge: keys.top_edges[index],
                    orientation: Orientation::Forward,
                    curve: cap_pcurve(segment, false, false),
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
    for (loft_loop, keys) in region.loops.iter().zip(&loop_keys) {
        let count = loft_loop.base.segments.len();
        for index in 0..count {
            let next = (index + 1) % count;
            push_slant_face(
                &mut topology,
                &mut next_id,
                frame,
                distance,
                loft_loop.base.segments[index],
                loft_loop.top[index],
                [
                    keys.bottom_edges[index],
                    keys.slant_edges[next],
                    keys.top_edges[index],
                    keys.slant_edges[index],
                ],
                FaceRole::ExtrusionSide(side_ordinal),
            );
            side_ordinal += 1;
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

/// The wall between a base segment and its offset.
///
/// A straight edge and its parallel offset span a plane whose `u` runs along
/// the base edge and whose `v` climbs the slant, so the wall is a trapezoid
/// in its own metric coordinates. An arc and its concentric offset lie on one
/// cone about the frame normal, parameterized like the cylinder a straight
/// extrusion would have made, with the ring radius growing by `slope` per
/// unit of height.
#[allow(clippy::too_many_arguments)]
fn push_slant_face(
    topology: &mut Topology,
    next_id: &mut u64,
    frame: Frame,
    distance: f64,
    base: Segment,
    top: Segment,
    edges: [EdgeKey; 4],
    role: FaceRole,
) {
    let (surface, bottom, right, top_curve, left) = match (base, top) {
        (
            Segment::Line { start, end },
            Segment::Line {
                start: top_start,
                end: top_end,
            },
        ) => {
            let length = (end.x - start.x).hypot(end.y - start.y);
            let tangent =
                frame.u * ((end.x - start.x) / length) + frame.v * ((end.y - start.y) / length);
            let base_start = frame.point(start, 0.0);
            let rise = frame.point(top_start, distance) - base_start;
            let along_start = rise.dot(tangent);
            let climb = rise - tangent * along_start;
            let slant = climb.length();
            let along_end = (frame.point(top_end, distance) - base_start).dot(tangent);
            (
                Surface::Plane(Plane::new(base_start, tangent, climb / slant)),
                Curve2::line_segment([Point2::new(0.0, 0.0), Point2::new(length, 0.0)]),
                Curve2::line_segment([Point2::new(length, 0.0), Point2::new(along_end, slant)]),
                Curve2::line_segment([
                    Point2::new(along_end, slant),
                    Point2::new(along_start, slant),
                ]),
                Curve2::line_segment([Point2::new(along_start, slant), Point2::new(0.0, 0.0)]),
            )
        }
        (
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            },
            Segment::Arc {
                radius: top_radius, ..
            },
        ) => {
            let sign = sweep.signum();
            let start = sign * start_angle;
            let end = sign * (start_angle + sweep);
            (
                Surface::Cone(Cone {
                    origin: frame.center(center, 0.0),
                    axis: frame.normal,
                    radial_u: frame.u,
                    radial_v: frame.v,
                    base_radius: radius,
                    slope: (top_radius - radius) / distance,
                    angular_sign: sign,
                }),
                Curve2::line_segment([Point2::new(start, 0.0), Point2::new(end, 0.0)]),
                Curve2::line_segment([Point2::new(end, 0.0), Point2::new(end, distance)]),
                Curve2::line_segment([Point2::new(end, distance), Point2::new(start, distance)]),
                Curve2::line_segment([Point2::new(start, distance), Point2::new(start, 0.0)]),
            )
        }
        _ => unreachable!("an offset keeps every segment's carrier kind"),
    };
    let loop_key = push_loop(
        topology,
        next_id,
        [bottom, right, top_curve, left]
            .into_iter()
            .zip([
                (edges[0], Orientation::Forward),
                (edges[1], Orientation::Forward),
                (edges[2], Orientation::Reverse),
                (edges[3], Orientation::Reverse),
            ])
            .map(|(curve, (edge, orientation))| BoundaryUse {
                edge,
                orientation,
                curve,
            })
            .collect(),
    );
    topology.faces.push(Record {
        id: allocate_id(next_id),
        value: Face {
            surface,
            outer_loop: loop_key,
            inner_loops: Vec::new(),
            role,
        },
    });
}

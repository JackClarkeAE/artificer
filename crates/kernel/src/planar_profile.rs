//! Exact orchestration for planar profiles made exclusively from line uses.
//!
//! The ordinary polygon and selected-face feature constructors remain the
//! single-loop compatibility surface. This module certifies the richer wire
//! payload, applies profile holes without display faceting, and combines
//! disjoint standalone regions as independent solids in one snapshot.

use artificer_geometry::{Orientation2, Point2, orient2d};
use artificer_protocol::{
    EntityKind, EntityRef, FaceExtrusionOperation, MAX_PLANAR_PROFILE_CURVES,
    MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS, PlanarCurve2, PlanarFrame3,
    PlanarProfile2, Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy,
    SnapshotId, Vector3 as ProtocolVector3,
};

use crate::extrusion::{ExtrusionInputError, build_extrusion, validate_extrusion_input};
use crate::face_feature::{
    FaceFeatureArguments, FaceFeatureInputError, build_face_feature, validate_face_feature_input,
};
use crate::topology::{
    CoedgeKey, EdgeKey, EntityId, FaceKey, FaceRole, LoopKey, ShellKey, Topology, VertexKey,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanarProfileInputError {
    EmptyProfile,
    TooManyRegions,
    TooManyLoops,
    TooManyCurves,
    EmptyLoop,
    DisconnectedLoop,
    AnalyticCurve,
    OverlappingRegions,
    HoledFrameUnsupported,
    Extrusion(ExtrusionInputError),
    FaceFeature(FaceFeatureInputError),
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedLinearProfileExtrusion {
    pub(crate) topology: Topology,
}

#[derive(Clone, Debug, PartialEq)]
struct LinearRegion {
    outer: Vec<ProtocolPoint2>,
    holes: Vec<Vec<ProtocolPoint2>>,
}

pub(crate) fn profile_contains_analytic_curves(profile: &PlanarProfile2) -> bool {
    profile.regions.iter().any(|region| {
        std::iter::once(&region.outer)
            .chain(&region.holes)
            .flat_map(|profile_loop| &profile_loop.curves)
            .any(|curve| !matches!(curve, PlanarCurve2::Line { .. }))
    })
}

pub(crate) fn validate_linear_profile_extrusion(
    snapshot: SnapshotId,
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<ValidatedLinearProfileExtrusion, PlanarProfileInputError> {
    let regions = extract_linear_regions(profile)?;
    if linear_regions_overlap(&regions) {
        return Err(PlanarProfileInputError::OverlappingRegions);
    }
    let mut topologies = Vec::with_capacity(regions.len());
    for region in regions {
        let extrusion = validate_extrusion_input(frame, &region.outer, distance, precision)
            .map_err(PlanarProfileInputError::Extrusion)?;
        let mut topology = build_extrusion(&extrusion);

        for hole in &region.holes {
            let top = topology
                .faces
                .iter()
                .find(|face| face.value.role == FaceRole::ExtrusionTop)
                .ok_or(PlanarProfileInputError::HoledFrameUnsupported)?;
            let target_face = EntityRef {
                snapshot,
                entity: artificer_protocol::EntityId(top.id.get()),
                kind: EntityKind::Face,
            };
            let top_plane = top
                .value
                .surface
                .as_plane()
                .ok_or(PlanarProfileInputError::HoledFrameUnsupported)?;
            let top_frame = PlanarFrame3 {
                origin: ProtocolPoint3::new(
                    top_plane.origin.x,
                    top_plane.origin.y,
                    top_plane.origin.z,
                ),
                u: ProtocolVector3::new(top_plane.u.x, top_plane.u.y, top_plane.u.z),
                v: ProtocolVector3::new(top_plane.v.x, top_plane.v.y, top_plane.v.z),
            };
            let through_distance = distance
                + 2.0
                    * precision
                        .min_feature_size
                        .max(precision.modeling_resolution);
            if !through_distance.is_finite() {
                return Err(PlanarProfileInputError::HoledFrameUnsupported);
            }
            let feature = validate_face_feature_input(FaceFeatureArguments {
                snapshot,
                topology: &topology,
                target_face,
                frame: top_frame,
                vertices: hole,
                distance: through_distance,
                operation: FaceExtrusionOperation::Cut,
                precision,
            })
            .map_err(|error| match error {
                FaceFeatureInputError::TargetNotPlanar
                | FaceFeatureInputError::TargetNotAlignedToFrame
                | FaceFeatureInputError::FrameNotOrthonormal
                | FaceFeatureInputError::FrameOffTargetPlane => {
                    PlanarProfileInputError::HoledFrameUnsupported
                }
                other => PlanarProfileInputError::FaceFeature(other),
            })?;
            topology = build_face_feature(&feature);
        }
        topologies.push(topology);
    }

    Ok(ValidatedLinearProfileExtrusion {
        topology: merge_topologies(topologies),
    })
}

fn extract_linear_regions(
    profile: &PlanarProfile2,
) -> Result<Vec<LinearRegion>, PlanarProfileInputError> {
    if profile.regions.is_empty() {
        return Err(PlanarProfileInputError::EmptyProfile);
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return Err(PlanarProfileInputError::TooManyRegions);
    }
    if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
        return Err(PlanarProfileInputError::TooManyLoops);
    }
    if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
        return Err(PlanarProfileInputError::TooManyCurves);
    }
    let mut regions = profile
        .regions
        .iter()
        .map(|region| {
            Ok(LinearRegion {
                outer: linear_loop_vertices(&region.outer.curves)?,
                holes: region
                    .holes
                    .iter()
                    .map(|profile_loop| linear_loop_vertices(&profile_loop.curves))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect::<Result<Vec<_>, PlanarProfileInputError>>()?;
    for region in &mut regions {
        canonicalize_loop(&mut region.outer);
        for hole in &mut region.holes {
            canonicalize_loop(hole);
        }
        region
            .holes
            .sort_by(|left, right| compare_loops(left, right));
    }
    regions.sort_by(|left, right| {
        compare_loops(&left.outer, &right.outer).then_with(|| {
            left.holes
                .iter()
                .zip(&right.holes)
                .find_map(|(left, right)| {
                    let ordering = compare_loops(left, right);
                    ordering.ne(&std::cmp::Ordering::Equal).then_some(ordering)
                })
                .unwrap_or_else(|| left.holes.len().cmp(&right.holes.len()))
        })
    });
    Ok(regions)
}

fn canonicalize_loop(vertices: &mut [ProtocolPoint2]) {
    if vertices.is_empty() {
        return;
    }
    let twice_area = (0..vertices.len()).fold(0.0, |area, index| {
        let current = vertices[index];
        let next = vertices[(index + 1) % vertices.len()];
        area + current.x * next.y - current.y * next.x
    });
    if twice_area.is_finite() && twice_area < 0.0 {
        vertices.reverse();
    }
    if let Some((start, _)) = vertices
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| left.total_cmp(right))
    {
        vertices.rotate_left(start);
    }
}

fn compare_loops(left: &[ProtocolPoint2], right: &[ProtocolPoint2]) -> std::cmp::Ordering {
    left.iter()
        .zip(right)
        .find_map(|(left, right)| {
            let ordering = left.total_cmp(right);
            ordering.ne(&std::cmp::Ordering::Equal).then_some(ordering)
        })
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn linear_loop_vertices(
    curves: &[PlanarCurve2],
) -> Result<Vec<ProtocolPoint2>, PlanarProfileInputError> {
    if curves.is_empty() {
        return Err(PlanarProfileInputError::EmptyLoop);
    }
    let lines = curves
        .iter()
        .map(|curve| match *curve {
            PlanarCurve2::Line { start, end } => Ok((start, end)),
            PlanarCurve2::CircularArc { .. } | PlanarCurve2::Circle { .. } => {
                Err(PlanarProfileInputError::AnalyticCurve)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if (0..lines.len()).any(|index| lines[index].1 != lines[(index + 1) % lines.len()].0) {
        return Err(PlanarProfileInputError::DisconnectedLoop);
    }
    Ok(lines.into_iter().map(|(start, _)| start).collect())
}

fn linear_regions_overlap(regions: &[LinearRegion]) -> bool {
    for left in 0..regions.len() {
        for right in left + 1..regions.len() {
            let left_boundaries = std::iter::once(regions[left].outer.as_slice())
                .chain(regions[left].holes.iter().map(Vec::as_slice));
            let right_boundaries = std::iter::once(regions[right].outer.as_slice())
                .chain(regions[right].holes.iter().map(Vec::as_slice))
                .collect::<Vec<_>>();
            if left_boundaries.clone().any(|first| {
                right_boundaries
                    .iter()
                    .any(|second| loops_intersect(first, second))
            }) {
                return true;
            }
            if point_in_material(regions[left].outer[0], &regions[right])
                || point_in_material(regions[right].outer[0], &regions[left])
            {
                return true;
            }
        }
    }
    false
}

fn loops_intersect(first: &[ProtocolPoint2], second: &[ProtocolPoint2]) -> bool {
    (0..first.len()).any(|first_index| {
        (0..second.len()).any(|second_index| {
            segments_intersect(
                first[first_index],
                first[(first_index + 1) % first.len()],
                second[second_index],
                second[(second_index + 1) % second.len()],
            )
        })
    })
}

fn segments_intersect(
    first_start: ProtocolPoint2,
    first_end: ProtocolPoint2,
    second_start: ProtocolPoint2,
    second_end: ProtocolPoint2,
) -> bool {
    if first_start.x.min(first_end.x) > second_start.x.max(second_end.x)
        || second_start.x.min(second_end.x) > first_start.x.max(first_end.x)
        || first_start.y.min(first_end.y) > second_start.y.max(second_end.y)
        || second_start.y.min(second_end.y) > first_start.y.max(first_end.y)
    {
        return false;
    }
    let convert = |point: ProtocolPoint2| Point2::new(point.x, point.y);
    let orientations = [
        orient2d(
            convert(first_start),
            convert(first_end),
            convert(second_start),
        ),
        orient2d(
            convert(first_start),
            convert(first_end),
            convert(second_end),
        ),
        orient2d(
            convert(second_start),
            convert(second_end),
            convert(first_start),
        ),
        orient2d(
            convert(second_start),
            convert(second_end),
            convert(first_end),
        ),
    ];
    if orientations.contains(&Orientation2::Indeterminate) {
        return true;
    }
    let proper_crossing = matches!(
        (orientations[0], orientations[1]),
        (Orientation2::Clockwise, Orientation2::CounterClockwise)
            | (Orientation2::CounterClockwise, Orientation2::Clockwise)
    ) && matches!(
        (orientations[2], orientations[3]),
        (Orientation2::Clockwise, Orientation2::CounterClockwise)
            | (Orientation2::CounterClockwise, Orientation2::Clockwise)
    );
    proper_crossing
        || (orientations[0] == Orientation2::Collinear
            && point_in_segment_bounds(second_start, first_start, first_end))
        || (orientations[1] == Orientation2::Collinear
            && point_in_segment_bounds(second_end, first_start, first_end))
        || (orientations[2] == Orientation2::Collinear
            && point_in_segment_bounds(first_start, second_start, second_end))
        || (orientations[3] == Orientation2::Collinear
            && point_in_segment_bounds(first_end, second_start, second_end))
}

fn point_in_segment_bounds(
    point: ProtocolPoint2,
    start: ProtocolPoint2,
    end: ProtocolPoint2,
) -> bool {
    point.x >= start.x.min(end.x)
        && point.x <= start.x.max(end.x)
        && point.y >= start.y.min(end.y)
        && point.y <= start.y.max(end.y)
}

fn point_in_material(point: ProtocolPoint2, region: &LinearRegion) -> bool {
    point_in_polygon(point, &region.outer)
        && !region
            .holes
            .iter()
            .any(|hole| point_in_polygon(point, hole))
}

fn point_in_polygon(point: ProtocolPoint2, polygon: &[ProtocolPoint2]) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let first = polygon[index];
        let second = polygon[(index + 1) % polygon.len()];
        let crosses = (first.y > point.y) != (second.y > point.y)
            && point.x
                < (second.x - first.x) * (point.y - first.y) / (second.y - first.y) + first.x;
        if crosses {
            inside = !inside;
        }
    }
    inside
}

fn merge_topologies(topologies: Vec<Topology>) -> Topology {
    let mut merged = Topology::default();
    let mut next_id = 1_u64;
    for mut topology in topologies {
        let vertex_offset = merged.vertices.len();
        let edge_offset = merged.edges.len();
        let coedge_offset = merged.coedges.len();
        let loop_offset = merged.loops.len();
        let face_offset = merged.faces.len();
        let shell_offset = merged.shells.len();

        for record in &mut topology.vertices {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
        }
        for record in &mut topology.edges {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
            record.value.vertices = record
                .value
                .vertices
                .map(|key| VertexKey(key.0 + vertex_offset));
        }
        for record in &mut topology.coedges {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
            record.value.edge = EdgeKey(record.value.edge.0 + edge_offset);
        }
        for record in &mut topology.loops {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
            for key in &mut record.value.coedges {
                *key = CoedgeKey(key.0 + coedge_offset);
            }
        }
        for record in &mut topology.faces {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
            record.value.outer_loop = LoopKey(record.value.outer_loop.0 + loop_offset);
            for key in &mut record.value.inner_loops {
                *key = LoopKey(key.0 + loop_offset);
            }
        }
        for record in &mut topology.shells {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
            for key in &mut record.value.faces {
                *key = FaceKey(key.0 + face_offset);
            }
        }
        for record in &mut topology.solids {
            record.id = EntityId::from_raw(next_id);
            next_id += 1;
            record.value.outer_shell = ShellKey(record.value.outer_shell.0 + shell_offset);
            for inner in &mut record.value.inner_shells {
                *inner = ShellKey(inner.0 + shell_offset);
            }
        }

        merged.vertices.extend(topology.vertices);
        merged.edges.extend(topology.edges);
        merged.coedges.extend(topology.coedges);
        merged.loops.extend(topology.loops);
        merged.faces.extend(topology.faces);
        merged.shells.extend(topology.shells);
        merged.solids.extend(topology.solids);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use artificer_protocol::{PlanarLoop2, PlanarRegion2};

    fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> LinearRegion {
        LinearRegion {
            outer: vec![
                ProtocolPoint2::new(min_x, min_y),
                ProtocolPoint2::new(max_x, min_y),
                ProtocolPoint2::new(max_x, max_y),
                ProtocolPoint2::new(min_x, max_y),
            ],
            holes: Vec::new(),
        }
    }

    #[test]
    fn collinear_but_separated_region_edges_do_not_intersect() {
        assert!(!linear_regions_overlap(&[
            rectangle(0.0, 0.0, 1.0, 1.0),
            rectangle(3.0, 0.0, 4.0, 1.0),
        ]));
    }

    #[test]
    fn region_boundaries_that_t_touch_are_rejected() {
        assert!(linear_regions_overlap(&[
            rectangle(0.0, 0.0, 2.0, 2.0),
            rectangle(2.0, 0.5, 3.0, 1.5),
        ]));
    }

    #[test]
    fn typed_profiles_enforce_region_loop_and_curve_limits_before_construction() {
        let triangle = || {
            PlanarLoop2::from_polygon(&[
                ProtocolPoint2::new(0.0, 0.0),
                ProtocolPoint2::new(1.0, 0.0),
                ProtocolPoint2::new(0.0, 1.0),
            ])
        };
        let region = || PlanarRegion2 {
            outer: triangle(),
            holes: Vec::new(),
        };
        assert_eq!(
            extract_linear_regions(&PlanarProfile2 {
                regions: (0..=MAX_PLANAR_PROFILE_REGIONS).map(|_| region()).collect(),
            }),
            Err(PlanarProfileInputError::TooManyRegions)
        );
        assert_eq!(
            extract_linear_regions(&PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: triangle(),
                    holes: (0..MAX_PLANAR_PROFILE_LOOPS).map(|_| triangle()).collect(),
                }],
            }),
            Err(PlanarProfileInputError::TooManyLoops)
        );
        assert_eq!(
            extract_linear_regions(&PlanarProfile2 {
                regions: vec![PlanarRegion2 {
                    outer: PlanarLoop2 {
                        curves: (0..=MAX_PLANAR_PROFILE_CURVES)
                            .map(|index| PlanarCurve2::Line {
                                start: ProtocolPoint2::new(index as f64, 0.0),
                                end: ProtocolPoint2::new(index as f64 + 1.0, 0.0),
                            })
                            .collect(),
                    },
                    holes: Vec::new(),
                }],
            }),
            Err(PlanarProfileInputError::TooManyCurves)
        );
    }
}

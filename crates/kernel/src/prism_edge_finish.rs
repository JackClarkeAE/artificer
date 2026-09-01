//! Exact fillets and chamfers on the vertical edges of any line/arc prism.
//!
//! This generalizes the cuboid path: a vertical edge of an extruded profile is
//! a profile *vertex*, so finishing it is a planar corner blend followed by an
//! exact re-extrusion. The profile is recovered from the committed top cap
//! rather than from provenance, so the path keeps working after replay and
//! after an earlier finish.

use artificer_protocol::{
    ArcDirection, EdgeFinishKind, EntityKind, EntityRef, PlanarCurve2, PlanarFrame3, PlanarLoop2,
    PlanarProfile2, PlanarRegion2, Point2 as ProtocolPoint2, PrecisionPolicy, SnapshotId,
    Vector3 as ProtocolVector3,
};

use crate::analytic_extrusion::{
    AnalyticLoop, Frame, Segment, build_analytic_extrusion, topology_loop_segments,
    validate_analytic_profile_extrusion,
};
use crate::corner_blend::{
    CornerBlend, CornerBlendError, corner_blend, retarget_end, retarget_start, segment_length,
};
use crate::topology::{Curve3, FaceRole, Point2, Point3, Surface, Topology, Vector3};

const MAX_TARGETS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrismEdgeFinishError {
    TargetInvalid,
    DomainUnsupported,
    DistanceInvalid,
    ConstructionFailed,
}

/// A prism recovered from committed topology: the planar frame of its bottom
/// cap, the extrusion height, and the profile loops in profile orientation.
pub(crate) struct PrismProfile {
    frame: Frame,
    height: f64,
    outer: Vec<Segment>,
    holes: Vec<Vec<Segment>>,
}

impl PrismProfile {
    pub(crate) const fn frame(&self) -> Frame {
        self.frame
    }

    pub(crate) const fn height(&self) -> f64 {
        self.height
    }

    /// Every loop in profile order: the outer boundary, then each hole.
    pub(crate) fn loops(&self) -> impl Iterator<Item = &[Segment]> {
        std::iter::once(self.outer.as_slice()).chain(self.holes.iter().map(Vec::as_slice))
    }

    /// The same solid seen from the other end: the frame sits on the far cap
    /// with its `v` axis and normal reversed, and every loop is traversed
    /// backwards with `y` negated. Both reversals are needed to keep the frame
    /// right-handed and the loops counter-clockwise, so a builder written for
    /// the top rim produces the bottom rim unchanged.
    pub(crate) fn mirrored(&self) -> Self {
        Self {
            frame: Frame {
                origin: self.frame.origin + self.frame.normal * self.height,
                u: self.frame.u,
                v: self.frame.v * -1.0,
                normal: self.frame.normal * -1.0,
            },
            height: self.height,
            outer: mirror_loop(&self.outer),
            holes: self.holes.iter().map(|hole| mirror_loop(hole)).collect(),
        }
    }
}

/// Reverses a loop and reflects it in the `x` axis.
fn mirror_loop(source: &[Segment]) -> Vec<Segment> {
    let reflect = |point: Point2| Point2::new(point.x, -point.y);
    source
        .iter()
        .rev()
        .map(|segment| match *segment {
            Segment::Line { start, end } => Segment::Line {
                start: reflect(end),
                end: reflect(start),
            },
            Segment::Arc {
                center,
                start,
                end,
                radius,
                start_angle,
                sweep,
            } => Segment::Arc {
                center: reflect(center),
                start: reflect(end),
                end: reflect(start),
                radius,
                // Reflection negates every azimuth and reversal negates the
                // sweep again, so the arc keeps its convexity.
                start_angle: -(start_angle + sweep),
                sweep,
            },
        })
        .collect()
}

/// Builds an exact vertical-edge finish, or reports why the request leaves the
/// domain so the caller can try the next path.
pub(crate) fn build_prism_edge_finishes(
    snapshot: SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, PrismEdgeFinishError> {
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err(PrismEdgeFinishError::TargetInvalid);
    }
    if targets
        .iter()
        .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
    {
        return Err(PrismEdgeFinishError::TargetInvalid);
    }
    let mut identifiers = targets
        .iter()
        .map(|target| target.entity.0)
        .collect::<Vec<_>>();
    identifiers.sort_unstable();
    identifiers.dedup();
    if identifiers.len() != targets.len() {
        return Err(PrismEdgeFinishError::TargetInvalid);
    }
    if !distance.is_finite() || distance < precision.min_feature_size {
        return Err(PrismEdgeFinishError::DistanceInvalid);
    }

    let prism = extract_prism(topology, precision)?;
    let mut selections = Vec::with_capacity(targets.len());
    for target in targets {
        selections.push(resolve_vertex(topology, &prism, *target, precision)?);
    }
    selections.sort_unstable();
    selections.dedup();

    let blended = blend_loops(&prism, &selections, kind, distance, precision)?;
    let profile = protocol_profile(&blended)?;
    let frame = PlanarFrame3::new(
        protocol_point(prism.frame.origin),
        protocol_vector(prism.frame.u),
        protocol_vector(prism.frame.v),
    );
    let validated = validate_analytic_profile_extrusion(frame, &profile, prism.height, precision)
        .map_err(|_| PrismEdgeFinishError::ConstructionFailed)?;
    Ok(build_analytic_extrusion(&validated))
}

/// Which loop a selected vertex belongs to, and its index within that loop.
/// `loop_index` 0 is the outer loop; later indices are holes.
type VertexSelection = (usize, usize);

/// Accepts one solid whose caps are planar and anti-parallel and whose walls
/// are all planes or cylinders generated along the cap normal.
pub(crate) fn extract_prism(
    topology: &Topology,
    precision: PrecisionPolicy,
) -> Result<PrismProfile, PrismEdgeFinishError> {
    if topology.solids.len() != 1 {
        return Err(PrismEdgeFinishError::DomainUnsupported);
    }
    // A primitive cuboid names its caps by world axis rather than by
    // extrusion role; it is the same prism, so its `+Z` face is the top cap.
    let top_index = single_face_with_role(topology, FaceRole::ExtrusionTop)
        .or_else(|_| single_face_with_role(topology, FaceRole::PositiveZ))?;
    let bottom_index = single_face_with_role(topology, FaceRole::ExtrusionBottom)
        .or_else(|_| single_face_with_role(topology, FaceRole::NegativeZ))?;
    let top = &topology.faces[top_index].value;
    let bottom = &topology.faces[bottom_index].value;
    let top_plane = top
        .surface
        .as_plane()
        .ok_or(PrismEdgeFinishError::DomainUnsupported)?;
    let bottom_plane = bottom
        .surface
        .as_plane()
        .ok_or(PrismEdgeFinishError::DomainUnsupported)?;

    let normal = top_plane.normal;
    let agreement = frame_agreement(topology);
    if (bottom_plane.normal + normal).length() > agreement {
        return Err(PrismEdgeFinishError::DomainUnsupported);
    }
    // Every wall must be generated along the cap normal, so the body really is
    // a prism and its vertical edges really are profile vertices.
    for (index, face) in topology.faces.iter().enumerate() {
        if index == top_index || index == bottom_index {
            continue;
        }
        let parallel = match face.value.surface {
            Surface::Plane(plane) => plane.normal.dot(normal).abs() <= agreement,
            Surface::Cylinder(cylinder) => cylinder.axis.cross(normal).length() <= agreement,
            Surface::Torus(_) | Surface::Cone(_) | Surface::Sphere(_) => false,
        };
        if !parallel {
            return Err(PrismEdgeFinishError::DomainUnsupported);
        }
    }

    let height = (top_plane.origin - bottom_plane.origin).dot(normal);
    if !height.is_finite() || height <= precision.min_feature_size {
        return Err(PrismEdgeFinishError::DomainUnsupported);
    }

    // The top cap stores its boundary in profile orientation, so its pcurves
    // are the profile directly. Referring it to the bottom plane gives the
    // frame the rebuild extrudes from.
    let outer = topology_loop_segments(topology, top.outer_loop)
        .ok_or(PrismEdgeFinishError::DomainUnsupported)?;
    let holes = top
        .inner_loops
        .iter()
        .map(|loop_key| topology_loop_segments(topology, *loop_key))
        .collect::<Option<Vec<_>>>()
        .ok_or(PrismEdgeFinishError::DomainUnsupported)?;
    let frame = Frame {
        origin: top_plane.origin + normal * -height,
        u: top_plane.u,
        v: top_plane.v,
        normal,
    };
    Ok(PrismProfile {
        frame,
        height,
        outer,
        holes,
    })
}

fn single_face_with_role(
    topology: &Topology,
    role: FaceRole,
) -> Result<usize, PrismEdgeFinishError> {
    let mut found = None;
    for (index, face) in topology.faces.iter().enumerate() {
        if face.value.role == role {
            if found.is_some() {
                return Err(PrismEdgeFinishError::DomainUnsupported);
            }
            found = Some(index);
        }
    }
    found.ok_or(PrismEdgeFinishError::DomainUnsupported)
}

fn frame_agreement(topology: &Topology) -> f64 {
    let scale = topology
        .vertices
        .iter()
        .map(|vertex| {
            vertex
                .value
                .point
                .x
                .abs()
                .max(vertex.value.point.y.abs())
                .max(vertex.value.point.z.abs())
        })
        .fold(1.0_f64, f64::max);
    1.0e-9 * scale
}

/// Maps one target edge to the profile vertex it generates.
fn resolve_vertex(
    topology: &Topology,
    prism: &PrismProfile,
    target: EntityRef,
    precision: PrecisionPolicy,
) -> Result<VertexSelection, PrismEdgeFinishError> {
    let edge = topology
        .edges
        .iter()
        .find(|edge| edge.id.get() == target.entity.0)
        .ok_or(PrismEdgeFinishError::TargetInvalid)?;
    let Curve3::Line { endpoints } = edge.value.curve else {
        // A circular edge is a rim, not a generator; another path owns it.
        return Err(PrismEdgeFinishError::DomainUnsupported);
    };
    let agreement = frame_agreement(topology);
    let heights = endpoints.map(|point| (point - prism.frame.origin).dot(prism.frame.normal));
    let low = heights[0].min(heights[1]);
    let high = heights[0].max(heights[1]);
    // Only a full-height generator identifies a profile vertex; a partial
    // vertical edge belongs to a face feature.
    if low.abs() > agreement || (high - prism.height).abs() > agreement {
        return Err(PrismEdgeFinishError::DomainUnsupported);
    }
    let base = if heights[0] <= heights[1] {
        endpoints[0]
    } else {
        endpoints[1]
    };
    let relative = base - prism.frame.origin;
    let planar = Point2::new(relative.dot(prism.frame.u), relative.dot(prism.frame.v));

    let mut located = None;
    for (loop_index, segments) in std::iter::once(&prism.outer)
        .chain(prism.holes.iter())
        .enumerate()
    {
        for (vertex_index, segment) in segments.iter().enumerate() {
            if planar_points_agree(segment.start(), planar, precision) {
                if located.is_some() {
                    return Err(PrismEdgeFinishError::DomainUnsupported);
                }
                located = Some((loop_index, vertex_index));
            }
        }
    }
    located.ok_or(PrismEdgeFinishError::DomainUnsupported)
}

fn planar_points_agree(first: Point2, second: Point2, precision: PrecisionPolicy) -> bool {
    let scale = 1.0
        + first
            .x
            .abs()
            .max(first.y.abs())
            .max(second.x.abs())
            .max(second.y.abs());
    (first.x - second.x).hypot(first.y - second.y)
        <= precision.linear_agreement.max(1.0e-12) * scale
}

/// Rewrites every selected corner, leaving all other segments untouched.
fn blend_loops(
    prism: &PrismProfile,
    selections: &[VertexSelection],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Vec<Vec<Segment>>, PrismEdgeFinishError> {
    let source_loops = std::iter::once(prism.outer.clone())
        .chain(prism.holes.iter().cloned())
        .collect::<Vec<_>>();
    // The material probe consults the untouched profile, so a blend's branch
    // is decided against the body as committed rather than as partly rewritten.
    let analytic = source_loops
        .iter()
        .map(|segments| AnalyticLoop {
            signed_area: loop_signed_area(segments),
            segments: segments.clone(),
        })
        .collect::<Vec<_>>();
    let probe = |point: Point2| crate::analytic_extrusion::point_in_material(point, &analytic);

    let mut blended = source_loops.clone();
    for (loop_index, segments) in source_loops.iter().enumerate() {
        let count = segments.len();
        let mut selected_here = selections
            .iter()
            .filter(|(selected_loop, _)| *selected_loop == loop_index)
            .map(|(_, vertex)| *vertex)
            .collect::<Vec<_>>();
        if selected_here.is_empty() {
            continue;
        }
        selected_here.sort_unstable();
        if count < 3 {
            return Err(PrismEdgeFinishError::DomainUnsupported);
        }

        // Resolve every corner against the untouched loop first, so two
        // finishes on one segment cannot see each other's trim.
        let mut blends = Vec::with_capacity(selected_here.len());
        for vertex in &selected_here {
            let incoming = segments[(*vertex + count - 1) % count];
            let outgoing = segments[*vertex];
            let blend = corner_blend(incoming, outgoing, kind, distance, &probe, precision)
                .map_err(map_corner_error)?;
            // A prism profile is rebuilt segment by segment, so a neighbour
            // consumed to nothing has no representation here. Revolved
            // sections can drop such a piece; this builder cannot.
            if blend.consumed.any() {
                return Err(PrismEdgeFinishError::DistanceInvalid);
            }
            blends.push((*vertex, blend));
        }
        // Two blends sharing a segment must both fit inside it.
        for (first_index, (first_vertex, first_blend)) in blends.iter().enumerate() {
            for (second_vertex, second_blend) in blends.iter().skip(first_index + 1) {
                let shared = shared_segment(*first_vertex, *second_vertex, count);
                if let Some(shared) = shared {
                    let original = segment_length(segments[shared]);
                    let consumed = consumed_length(segments[shared], *first_vertex, first_blend)
                        + consumed_length(segments[shared], *second_vertex, second_blend);
                    if consumed > original - precision.min_feature_size {
                        return Err(PrismEdgeFinishError::DistanceInvalid);
                    }
                }
            }
        }

        blended[loop_index] = rebuild_loop(segments, &blends)?;
    }
    Ok(blended)
}

/// The segment index shared by two corners, if they are adjacent.
fn shared_segment(first_vertex: usize, second_vertex: usize, count: usize) -> Option<usize> {
    if (first_vertex + 1) % count == second_vertex {
        Some(first_vertex)
    } else if (second_vertex + 1) % count == first_vertex {
        Some(second_vertex)
    } else {
        None
    }
}

/// How much of `segment` a blend at `vertex` consumes.
fn consumed_length(segment: Segment, vertex: usize, blend: &CornerBlend) -> f64 {
    // A corner blend trims the end of its incoming neighbour and the start of
    // its outgoing one; whichever role this segment plays, the consumed length
    // is the difference from the original.
    let original = segment_length(segment);
    let incoming_left = segment_length(blend.trimmed_incoming);
    let outgoing_left = segment_length(blend.trimmed_outgoing);
    let _ = vertex;
    (original - incoming_left)
        .abs()
        .min((original - outgoing_left).abs())
}

/// Applies every resolved blend to one loop in order.
///
/// Trims accumulate per endpoint rather than per segment: when two blended
/// corners share a segment, that segment is shortened at both ends, so the
/// later blend cannot discard the earlier one's trim.
fn rebuild_loop(
    segments: &[Segment],
    blends: &[(usize, CornerBlend)],
) -> Result<Vec<Segment>, PrismEdgeFinishError> {
    let count = segments.len();
    let mut new_start = vec![None; count];
    let mut new_end = vec![None; count];
    let mut connectors: Vec<(usize, Segment)> = Vec::with_capacity(blends.len());
    for (vertex, blend) in blends {
        let incoming_index = (*vertex + count - 1) % count;
        new_end[incoming_index] = Some(blend.trimmed_incoming.end());
        new_start[*vertex] = Some(blend.trimmed_outgoing.start());
        connectors.push((*vertex, blend.connector));
    }
    let mut rebuilt = Vec::with_capacity(count + connectors.len());
    for (index, segment) in segments.iter().enumerate() {
        let mut current = *segment;
        if let Some(start) = new_start[index] {
            current = retarget_start(current, start).map_err(map_corner_error)?;
        }
        if let Some(end) = new_end[index] {
            current = retarget_end(current, end).map_err(map_corner_error)?;
        }
        rebuilt.push(current);
    }
    // Insert connectors before their outgoing segment, walking from the back
    // so earlier indices stay valid.
    connectors.sort_by_key(|(vertex, _)| std::cmp::Reverse(*vertex));
    for (vertex, connector) in connectors {
        rebuilt.insert(vertex, connector);
    }
    Ok(rebuilt)
}

fn loop_signed_area(segments: &[Segment]) -> f64 {
    let Some(anchor) = segments.first().map(|segment| segment.start()) else {
        return 0.0;
    };
    segments
        .iter()
        .map(|segment| segment.translated(anchor).signed_area_contribution())
        .sum()
}

const fn map_corner_error(error: CornerBlendError) -> PrismEdgeFinishError {
    match error {
        CornerBlendError::NoCorner => PrismEdgeFinishError::DomainUnsupported,
        CornerBlendError::TrimTooLarge => PrismEdgeFinishError::DistanceInvalid,
        CornerBlendError::NoSolution | CornerBlendError::Ambiguous => {
            PrismEdgeFinishError::ConstructionFailed
        }
    }
}

fn protocol_profile(loops: &[Vec<Segment>]) -> Result<PlanarProfile2, PrismEdgeFinishError> {
    let mut iterator = loops.iter();
    let outer = iterator
        .next()
        .ok_or(PrismEdgeFinishError::ConstructionFailed)?;
    Ok(PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: protocol_loop(outer),
            holes: iterator.map(|hole| protocol_loop(hole)).collect(),
        }],
    })
}

fn protocol_loop(segments: &[Segment]) -> PlanarLoop2 {
    // The exact rebuild requires bit-identical shared endpoints. Recovering a
    // profile from committed pcurves can differ by an ulp between a segment's
    // end and its successor's start, so every curve is emitted against one
    // canonical vertex per junction.
    let vertices = segments
        .iter()
        .map(|segment| segment.start())
        .collect::<Vec<_>>();
    let count = vertices.len();
    PlanarLoop2 {
        curves: segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                let from = vertices[index];
                let to = vertices[(index + 1) % count];
                match *segment {
                    Segment::Line { .. } => PlanarCurve2::Line {
                        start: ProtocolPoint2::new(from.x, from.y),
                        end: ProtocolPoint2::new(to.x, to.y),
                    },
                    Segment::Arc { center, sweep, .. } => PlanarCurve2::CircularArc {
                        center: ProtocolPoint2::new(center.x, center.y),
                        start: ProtocolPoint2::new(from.x, from.y),
                        end: ProtocolPoint2::new(to.x, to.y),
                        direction: if sweep >= 0.0 {
                            ArcDirection::CounterClockwise
                        } else {
                            ArcDirection::Clockwise
                        },
                    },
                }
            })
            .collect(),
    }
}

fn protocol_point(point: Point3) -> artificer_protocol::Point3 {
    artificer_protocol::Point3::new(point.x, point.y, point.z)
}

fn protocol_vector(vector: Vector3) -> ProtocolVector3 {
    ProtocolVector3::new(vector.x, vector.y, vector.z)
}

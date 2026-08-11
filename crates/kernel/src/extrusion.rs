//! Deterministic construction of a watertight prism from a certified simple
//! linear planar polygon.

use artificer_geometry::{
    InvalidProfile, Point2, ProfileClassification, ProfileWinding, classify_profile,
};
use artificer_protocol::{
    MAX_EXTRUSION_PROFILE_VERTICES, PlanarFrame3 as ProtocolPlanarFrame3, Point2 as ProtocolPoint2,
    PrecisionPolicy,
};

use crate::topology::{
    Coedge, CoedgeKey, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop, LoopKey,
    Orientation, Plane, Point3, Record, Shell, ShellKey, Solid, Surface, Topology, Vector3, Vertex,
    VertexKey,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExtrusionInputError {
    NonFinite,
    TooFewVertices,
    TooManyVertices,
    RepeatedVertex,
    SelfIntersecting,
    NumericallyIndeterminate,
    DegenerateFrame,
    NonPositiveDistance,
    FeatureTooSmall,
    AreaTooSmall,
    CoordinateLimit,
    PrecisionUnrepresentable,
}

#[derive(Clone, Copy, Debug)]
struct OrthonormalFrame {
    origin: Point3,
    u: Vector3,
    v: Vector3,
    normal: Vector3,
}

impl OrthonormalFrame {
    fn bottom_plane(self) -> Plane {
        Plane::new(self.origin, self.u, self.v)
    }

    fn top_plane(self, distance: f64) -> Plane {
        Plane::new(self.origin + self.normal * distance, self.u, self.v)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedExtrusion {
    frame: OrthonormalFrame,
    vertices: Vec<Point2>,
    distance: f64,
    bottom_points: Vec<Point3>,
    top_points: Vec<Point3>,
    sides: Vec<SideGeometry>,
}

#[derive(Clone, Copy, Debug)]
struct SideGeometry {
    plane: Plane,
    edge_length: f64,
}

impl ValidatedExtrusion {
    pub(crate) fn vertex_count(&self) -> usize {
        self.vertices.len()
    }
}

pub(crate) fn validate_extrusion_input(
    frame: ProtocolPlanarFrame3,
    vertices: &[ProtocolPoint2],
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<ValidatedExtrusion, ExtrusionInputError> {
    if !frame.is_finite()
        || !distance.is_finite()
        || vertices.iter().any(|point| !point.is_finite())
    {
        return Err(ExtrusionInputError::NonFinite);
    }
    if vertices.len() < 3 {
        return Err(ExtrusionInputError::TooFewVertices);
    }
    if vertices.len() > MAX_EXTRUSION_PROFILE_VERTICES {
        return Err(ExtrusionInputError::TooManyVertices);
    }
    if distance <= 0.0 {
        return Err(ExtrusionInputError::NonPositiveDistance);
    }

    let coordinate_limit = precision.max_abs_coordinate;
    if [frame.origin.x, frame.origin.y, frame.origin.z, distance]
        .into_iter()
        .chain(vertices.iter().flat_map(|point| [point.x, point.y]))
        .any(|value| value.abs() > coordinate_limit)
    {
        return Err(ExtrusionInputError::CoordinateLimit);
    }

    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    if distance <= minimum {
        return Err(ExtrusionInputError::FeatureTooSmall);
    }

    let mut polygon = vertices
        .iter()
        .map(|point| Point2::new(point.x, point.y))
        .collect::<Vec<_>>();
    for index in 0..polygon.len() {
        if polygon[index + 1..].contains(&polygon[index]) {
            return Err(ExtrusionInputError::RepeatedVertex);
        }
        let next = polygon[(index + 1) % polygon.len()];
        let edge_length = (next.x - polygon[index].x).hypot(next.y - polygon[index].y);
        if !edge_length.is_finite() {
            return Err(ExtrusionInputError::NumericallyIndeterminate);
        }
        if edge_length <= minimum {
            return Err(ExtrusionInputError::FeatureTooSmall);
        }
    }

    let mut closed = polygon.clone();
    closed.push(polygon[0]);
    let winding = match classify_profile(&closed) {
        ProfileClassification::Closed { winding } => winding,
        ProfileClassification::SelfIntersecting => {
            return Err(ExtrusionInputError::SelfIntersecting);
        }
        ProfileClassification::Invalid(InvalidProfile::RepeatedVertex) => {
            return Err(ExtrusionInputError::RepeatedVertex);
        }
        ProfileClassification::Invalid(InvalidProfile::TooFewVertices) => {
            return Err(ExtrusionInputError::TooFewVertices);
        }
        ProfileClassification::Invalid(InvalidProfile::NonFiniteCoordinate) => {
            return Err(ExtrusionInputError::NonFinite);
        }
        ProfileClassification::Indeterminate => {
            return Err(ExtrusionInputError::NumericallyIndeterminate);
        }
        ProfileClassification::Open => {
            return Err(ExtrusionInputError::NumericallyIndeterminate);
        }
    };

    // Edge lengths and total area alone do not reject long, arbitrarily thin
    // convex profiles. Certify every vertex against every nonincident edge so
    // the profile has no local separation at or below the feature-size floor.
    validate_vertex_edge_separation(&polygon, minimum)?;

    // Normalize winding first, then choose a canonical cyclic start so reverse
    // and shifted declarations publish identical authoritative topology.
    if winding == ProfileWinding::Clockwise {
        polygon[1..].reverse();
    }
    let canonical_start = polygon
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            left.x
                .total_cmp(&right.x)
                .then_with(|| left.y.total_cmp(&right.y))
        })
        .map(|(index, _)| index)
        .expect("three vertices guarantee a canonical start");
    polygon.rotate_left(canonical_start);

    let anchor = polygon[0];
    let mut twice_area = 0.0;
    for index in 1..polygon.len() - 1 {
        let first = polygon[index];
        let second = polygon[index + 1];
        let determinant = (first.x - anchor.x) * (second.y - anchor.y)
            - (first.y - anchor.y) * (second.x - anchor.x);
        if !determinant.is_finite() {
            return Err(ExtrusionInputError::NumericallyIndeterminate);
        }
        twice_area += determinant;
        if !twice_area.is_finite() {
            return Err(ExtrusionInputError::NumericallyIndeterminate);
        }
    }
    if twice_area * 0.5 <= minimum * minimum {
        return Err(ExtrusionInputError::AreaTooSmall);
    }

    let origin = Point3::new(frame.origin.x, frame.origin.y, frame.origin.z);
    let raw_u = Vector3::new(frame.u.x, frame.u.y, frame.u.z);
    let raw_v = Vector3::new(frame.v.x, frame.v.y, frame.v.z);
    let Some(u) = robust_unit(raw_u) else {
        return Err(ExtrusionInputError::DegenerateFrame);
    };
    let Some(raw_v) = robust_unit(raw_v) else {
        return Err(ExtrusionInputError::DegenerateFrame);
    };
    let cross = u.cross(raw_v);
    let cross_length = cross.length();
    let angular_floor = precision
        .angular_agreement_radians
        .clamp(64.0 * f64::EPSILON, 1.0);
    if !cross_length.is_finite() || cross_length <= angular_floor {
        return Err(ExtrusionInputError::DegenerateFrame);
    }
    let Some(normal) = robust_unit(cross) else {
        return Err(ExtrusionInputError::DegenerateFrame);
    };
    let Some(v) = robust_unit(normal.cross(u)) else {
        return Err(ExtrusionInputError::DegenerateFrame);
    };
    let frame = OrthonormalFrame {
        origin,
        u,
        v,
        normal,
    };

    let bottom_plane = frame.bottom_plane();
    let top_plane = frame.top_plane(distance);
    let bottom_points = polygon
        .iter()
        .map(|point| bottom_plane.evaluate(*point))
        .collect::<Vec<_>>();
    let top_points = polygon
        .iter()
        .map(|point| top_plane.evaluate(*point))
        .collect::<Vec<_>>();
    let mut sides = Vec::with_capacity(polygon.len());
    for index in 0..polygon.len() {
        let next = (index + 1) % polygon.len();
        let dx = polygon[next].x - polygon[index].x;
        let dy = polygon[next].y - polygon[index].y;
        let edge_length = dx.hypot(dy);
        let local_direction = frame.u * (dx / edge_length) + frame.v * (dy / edge_length);
        let Some(side_u) = robust_unit(local_direction) else {
            return Err(ExtrusionInputError::NumericallyIndeterminate);
        };
        sides.push(SideGeometry {
            plane: Plane::new(bottom_points[index], side_u, frame.normal),
            edge_length,
        });
    }

    let derived_world_coordinates = bottom_points
        .iter()
        .chain(&top_points)
        .flat_map(|point| [point.x, point.y, point.z])
        .chain(
            [bottom_plane.origin, top_plane.origin]
                .into_iter()
                .chain(sides.iter().map(|side| side.plane.origin))
                .flat_map(|point| [point.x, point.y, point.z]),
        );
    let derived_parameter_coordinates = sides.iter().flat_map(|side| [side.edge_length, distance]);
    if derived_world_coordinates
        .chain(derived_parameter_coordinates)
        .any(|value| !value.is_finite() || value.abs() > coordinate_limit)
    {
        return Err(ExtrusionInputError::CoordinateLimit);
    }

    validate_representability(
        frame,
        &polygon,
        distance,
        &bottom_points,
        &top_points,
        &sides,
        precision.linear_agreement,
    )?;

    Ok(ValidatedExtrusion {
        frame,
        vertices: polygon,
        distance,
        bottom_points,
        top_points,
        sides,
    })
}

fn validate_representability(
    frame: OrthonormalFrame,
    polygon: &[Point2],
    distance: f64,
    bottom_points: &[Point3],
    top_points: &[Point3],
    sides: &[SideGeometry],
    linear_agreement: f64,
) -> Result<(), ExtrusionInputError> {
    let bottom_face = Plane::new(frame.origin, frame.v, frame.u);
    let top_face = frame.top_plane(distance);
    let mut worst_error = plane_frame_error(bottom_face).max(plane_frame_error(top_face));

    for index in 0..polygon.len() {
        let next = (index + 1) % polygon.len();
        let expected_edge_length = sides[index].edge_length;
        worst_error = worst_error
            .max((bottom_points[index].distance(bottom_points[next]) - expected_edge_length).abs())
            .max((top_points[index].distance(top_points[next]) - expected_edge_length).abs())
            .max((bottom_points[index].distance(top_points[index]) - distance).abs())
            .max(plane_frame_error(sides[index].plane));

        let bottom_parameters = [
            Point2::new(polygon[next].y, polygon[next].x),
            Point2::new(polygon[index].y, polygon[index].x),
        ];
        let top_parameters = [polygon[index], polygon[next]];
        let side_parameters = [
            Point2::new(0.0, 0.0),
            Point2::new(expected_edge_length, 0.0),
            Point2::new(expected_edge_length, distance),
            Point2::new(0.0, distance),
        ];
        let side_targets = [
            bottom_points[index],
            bottom_points[next],
            top_points[next],
            top_points[index],
        ];

        for (parameters, target) in bottom_parameters
            .into_iter()
            .zip([bottom_points[next], bottom_points[index]])
        {
            worst_error = worst_error.max(bottom_face.evaluate(parameters).distance(target));
        }
        for (parameters, target) in top_parameters
            .into_iter()
            .zip([top_points[index], top_points[next]])
        {
            worst_error = worst_error.max(top_face.evaluate(parameters).distance(target));
        }
        for (parameters, target) in side_parameters.into_iter().zip(side_targets) {
            worst_error = worst_error.max(sides[index].plane.evaluate(parameters).distance(target));
        }
    }

    // Adjacent edges alone do not certify the represented cap shape: a rounded
    // placement can perturb angles while leaving side lengths nearly intact.
    // Pairwise chords cover the complete finite polygon geometry.
    for left in 0..polygon.len() {
        for right in left + 1..polygon.len() {
            let expected =
                (polygon[right].x - polygon[left].x).hypot(polygon[right].y - polygon[left].y);
            worst_error = worst_error
                .max((bottom_points[left].distance(bottom_points[right]) - expected).abs())
                .max((top_points[left].distance(top_points[right]) - expected).abs());
        }
    }

    if !worst_error.is_finite() || worst_error > linear_agreement {
        return Err(ExtrusionInputError::PrecisionUnrepresentable);
    }
    Ok(())
}

fn plane_frame_error(plane: Plane) -> f64 {
    (plane.u.length() - 1.0)
        .abs()
        .max((plane.v.length() - 1.0).abs())
        .max((plane.normal.length() - 1.0).abs())
        .max(plane.u.dot(plane.v).abs())
}

fn validate_vertex_edge_separation(
    polygon: &[Point2],
    minimum: f64,
) -> Result<(), ExtrusionInputError> {
    for (vertex_index, point) in polygon.iter().copied().enumerate() {
        for edge_start in 0..polygon.len() {
            let edge_end = (edge_start + 1) % polygon.len();
            if vertex_index == edge_start || vertex_index == edge_end {
                continue;
            }

            let Some((distance, arithmetic_scale)) =
                point_segment_distance(point, polygon[edge_start], polygon[edge_end])
            else {
                return Err(ExtrusionInputError::NumericallyIndeterminate);
            };
            // Keep this certification conservative near the policy boundary:
            // accept only when the computed separation clears both the floor
            // and a small bound for the floating-point arithmetic above it.
            let roundoff_guard = 64.0 * f64::EPSILON * arithmetic_scale;
            if !roundoff_guard.is_finite()
                || distance <= minimum
                || distance - minimum <= roundoff_guard
            {
                return Err(ExtrusionInputError::FeatureTooSmall);
            }
        }
    }
    Ok(())
}

fn point_segment_distance(point: Point2, start: Point2, end: Point2) -> Option<(f64, f64)> {
    let edge_x = end.x - start.x;
    let edge_y = end.y - start.y;
    let edge_length = edge_x.hypot(edge_y);
    let offset_x = point.x - start.x;
    let offset_y = point.y - start.y;
    let offset_length = offset_x.hypot(offset_y);
    if !edge_length.is_finite()
        || edge_length == 0.0
        || !offset_length.is_finite()
        || !offset_x.is_finite()
        || !offset_y.is_finite()
    {
        return None;
    }

    let unit_x = edge_x / edge_length;
    let unit_y = edge_y / edge_length;
    let projection = offset_x.mul_add(unit_x, offset_y * unit_y);
    if !projection.is_finite() {
        return None;
    }
    let projection = projection.clamp(0.0, edge_length);
    let residual_x = offset_x - unit_x * projection;
    let residual_y = offset_y - unit_y * projection;
    let distance = residual_x.hypot(residual_y);
    let arithmetic_scale = edge_length.max(offset_length);
    if !distance.is_finite() || !arithmetic_scale.is_finite() {
        return None;
    }
    Some((distance, arithmetic_scale))
}

fn robust_unit(vector: Vector3) -> Option<Vector3> {
    let maximum = [vector.x.abs(), vector.y.abs(), vector.z.abs()]
        .into_iter()
        .fold(0.0_f64, f64::max);
    if !maximum.is_finite() || maximum == 0.0 {
        return None;
    }
    let scaled = vector / maximum;
    let length = scaled.length();
    if !length.is_finite() || length == 0.0 {
        return None;
    }
    Some(scaled / length)
}

pub(crate) fn build_extrusion(extrusion: &ValidatedExtrusion) -> Topology {
    let count = extrusion.vertices.len();
    let top_plane = extrusion.frame.top_plane(extrusion.distance);
    let bottom_points = &extrusion.bottom_points;
    let top_points = &extrusion.top_points;

    let mut next_entity_id = 1_u64;
    let mut vertices = Vec::with_capacity(count * 2);
    for point in bottom_points.iter().chain(top_points.iter()) {
        vertices.push(Record {
            id: allocate_id(&mut next_entity_id),
            value: Vertex { point: *point },
        });
    }

    let mut edges = Vec::with_capacity(count * 3);
    for index in 0..count {
        let next = (index + 1) % count;
        edges.push(edge_record(
            &mut next_entity_id,
            [VertexKey(index), VertexKey(next)],
            [bottom_points[index], bottom_points[next]],
        ));
    }
    for index in 0..count {
        let next = (index + 1) % count;
        edges.push(edge_record(
            &mut next_entity_id,
            [VertexKey(count + index), VertexKey(count + next)],
            [top_points[index], top_points[next]],
        ));
    }
    for index in 0..count {
        edges.push(edge_record(
            &mut next_entity_id,
            [VertexKey(index), VertexKey(count + index)],
            [bottom_points[index], top_points[index]],
        ));
    }

    let mut coedges = Vec::with_capacity(count * 6);
    let mut loops = Vec::with_capacity(count + 2);
    let mut faces = Vec::with_capacity(count + 2);

    let bottom_uses = (0..count)
        .rev()
        .map(|index| {
            let next = (index + 1) % count;
            FaceUse {
                edge: EdgeKey(index),
                orientation: Orientation::Reverse,
                pcurve_endpoints: [
                    Point2::new(extrusion.vertices[next].y, extrusion.vertices[next].x),
                    Point2::new(extrusion.vertices[index].y, extrusion.vertices[index].x),
                ],
            }
        })
        .collect();
    append_face(
        &mut coedges,
        &mut loops,
        &mut faces,
        &mut next_entity_id,
        FaceRole::ExtrusionBottom,
        Plane::new(extrusion.frame.origin, extrusion.frame.v, extrusion.frame.u),
        bottom_uses,
    );

    let top_uses = (0..count)
        .map(|index| {
            let next = (index + 1) % count;
            FaceUse {
                edge: EdgeKey(count + index),
                orientation: Orientation::Forward,
                pcurve_endpoints: [extrusion.vertices[index], extrusion.vertices[next]],
            }
        })
        .collect();
    append_face(
        &mut coedges,
        &mut loops,
        &mut faces,
        &mut next_entity_id,
        FaceRole::ExtrusionTop,
        top_plane,
        top_uses,
    );

    for index in 0..count {
        let next = (index + 1) % count;
        let side = extrusion.sides[index];
        let uses = vec![
            FaceUse {
                edge: EdgeKey(index),
                orientation: Orientation::Forward,
                pcurve_endpoints: [Point2::new(0.0, 0.0), Point2::new(side.edge_length, 0.0)],
            },
            FaceUse {
                edge: EdgeKey(2 * count + next),
                orientation: Orientation::Forward,
                pcurve_endpoints: [
                    Point2::new(side.edge_length, 0.0),
                    Point2::new(side.edge_length, extrusion.distance),
                ],
            },
            FaceUse {
                edge: EdgeKey(count + index),
                orientation: Orientation::Reverse,
                pcurve_endpoints: [
                    Point2::new(side.edge_length, extrusion.distance),
                    Point2::new(0.0, extrusion.distance),
                ],
            },
            FaceUse {
                edge: EdgeKey(2 * count + index),
                orientation: Orientation::Reverse,
                pcurve_endpoints: [Point2::new(0.0, extrusion.distance), Point2::new(0.0, 0.0)],
            },
        ];
        append_face(
            &mut coedges,
            &mut loops,
            &mut faces,
            &mut next_entity_id,
            FaceRole::ExtrusionSide(index as u32),
            side.plane,
            uses,
        );
    }

    let shells = vec![Record {
        id: allocate_id(&mut next_entity_id),
        value: Shell {
            faces: (0..faces.len()).map(FaceKey).collect(),
        },
    }];
    let solids = vec![Record {
        id: allocate_id(&mut next_entity_id),
        value: Solid {
            outer_shell: ShellKey(0),
            inner_shells: Vec::new(),
        },
    }];

    Topology {
        vertices,
        edges,
        coedges,
        loops,
        faces,
        shells,
        solids,
    }
}

#[derive(Clone, Copy, Debug)]
struct FaceUse {
    edge: EdgeKey,
    orientation: Orientation,
    pcurve_endpoints: [Point2; 2],
}

fn allocate_id(next: &mut u64) -> EntityId {
    let id = EntityId::from_raw(*next);
    *next += 1;
    id
}

fn edge_record(
    next_id: &mut u64,
    vertices: [VertexKey; 2],
    curve_endpoints: [Point3; 2],
) -> Record<Edge> {
    Record {
        id: allocate_id(next_id),
        value: Edge::line(vertices, curve_endpoints),
    }
}

#[allow(clippy::too_many_arguments)]
fn append_face(
    coedges: &mut Vec<Record<Coedge>>,
    loops: &mut Vec<Record<Loop>>,
    faces: &mut Vec<Record<Face>>,
    next_id: &mut u64,
    role: FaceRole,
    plane: Plane,
    uses: Vec<FaceUse>,
) {
    let mut loop_coedges = Vec::with_capacity(uses.len());
    for face_use in uses {
        let coedge_key = CoedgeKey(coedges.len());
        coedges.push(Record {
            id: allocate_id(next_id),
            value: Coedge::line(
                face_use.edge,
                face_use.orientation,
                face_use.pcurve_endpoints,
            ),
        });
        loop_coedges.push(coedge_key);
    }

    let loop_key = LoopKey(loops.len());
    loops.push(Record {
        id: allocate_id(next_id),
        value: Loop {
            coedges: loop_coedges,
        },
    });
    faces.push(Record {
        id: allocate_id(next_id),
        value: Face {
            surface: Surface::Plane(plane),
            outer_loop: loop_key,
            inner_loops: Vec::new(),
            role,
        },
    });
}

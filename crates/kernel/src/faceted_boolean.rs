//! Regularized native fallback for a cut whose sweep crosses earlier feature
//! topology.
//!
//! The local analytic feature writers deliberately stop before they would
//! have to split an existing side surface.  This module owns that next rung of
//! capability: it evaluates the committed boundary and the new prismatic tool
//! through a deterministic BSP Boolean, welds the regularized result, and
//! publishes a closed planar B-rep. Uncrossed analytic operations stay on the
//! exact plane/cylinder writers. A snapshot that requires this fallback is
//! rebuilt as planar facets according to the request's approximation budget;
//! its earlier immutable snapshots remain analytic and available to history.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use artificer_protocol::{EdgeFinishKind, EntityRef, PlanarProfile2, PrecisionPolicy};

use crate::DebugScene;
use crate::analytic_extrusion::{
    AnalyticLoop, Frame, Segment, validate_analytic_profile_extrusion,
};
use crate::face_feature::FaceFeatureInputError;
use crate::planar_profile::PlanarProfileInputError;
use crate::topology::{
    Coedge, CoedgeKey, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop, LoopKey,
    Orientation, Plane, Point2, Point3, Record, Shell, ShellKey, Solid, Surface, Topology, Vector3,
    Vertex, VertexKey,
};

#[derive(Clone, Debug)]
struct Polygon {
    vertices: Vec<Point3>,
    plane: SplitPlane,
    role: FaceRole,
}

impl Polygon {
    fn new(vertices: Vec<Point3>, role: FaceRole, epsilon: f64) -> Option<Self> {
        let plane = SplitPlane::from_points(&vertices, epsilon)?;
        Some(Self {
            vertices,
            plane,
            role,
        })
    }

    fn new_narrow(vertices: Vec<Point3>, role: FaceRole, epsilon: f64) -> Option<Self> {
        // Cutter panels can be narrow at a shallow dihedral. The cross
        // product inspected by `from_points` has squared-length units, so use
        // the squared modeling tolerance for these already size-certified
        // inputs. Ordinary BSP fragments retain the conservative threshold
        // in `new`, preventing numerical slivers from being published.
        let plane = SplitPlane::from_points(&vertices, epsilon * epsilon)?;
        Some(Self {
            vertices,
            plane,
            role,
        })
    }

    fn invert(&mut self) {
        self.vertices.reverse();
        self.plane.flip();
    }
}

#[derive(Clone, Copy, Debug)]
struct SplitPlane {
    normal: Vector3,
    offset: f64,
}

impl SplitPlane {
    fn from_points(points: &[Point3], epsilon: f64) -> Option<Self> {
        let origin = *points.first()?;
        for first in 1..points.len().saturating_sub(1) {
            for second in first + 1..points.len() {
                let normal = (points[first] - origin).cross(points[second] - origin);
                let length = normal.length();
                if length > epsilon {
                    let normal = normal / length;
                    return Some(Self {
                        normal,
                        offset: normal.dot(origin.as_vector()),
                    });
                }
            }
        }
        None
    }

    fn flip(&mut self) {
        self.normal = self.normal * -1.0;
        self.offset = -self.offset;
    }

    fn split_polygon(
        self,
        polygon: &Polygon,
        epsilon: f64,
        coplanar_front: &mut Vec<Polygon>,
        coplanar_back: &mut Vec<Polygon>,
        front: &mut Vec<Polygon>,
        back: &mut Vec<Polygon>,
    ) {
        const COPLANAR: u8 = 0;
        const FRONT: u8 = 1;
        const BACK: u8 = 2;
        const SPANNING: u8 = FRONT | BACK;

        let mut polygon_type = COPLANAR;
        let mut types = Vec::with_capacity(polygon.vertices.len());
        for vertex in &polygon.vertices {
            let distance = self.normal.dot(vertex.as_vector()) - self.offset;
            let kind = if distance < -epsilon {
                BACK
            } else if distance > epsilon {
                FRONT
            } else {
                COPLANAR
            };
            polygon_type |= kind;
            types.push(kind);
        }

        match polygon_type {
            COPLANAR => {
                if self.normal.dot(polygon.plane.normal) >= 0.0 {
                    coplanar_front.push(polygon.clone());
                } else {
                    coplanar_back.push(polygon.clone());
                }
            }
            FRONT => front.push(polygon.clone()),
            BACK => back.push(polygon.clone()),
            SPANNING => {
                let mut front_vertices = Vec::new();
                let mut back_vertices = Vec::new();
                for index in 0..polygon.vertices.len() {
                    let next = (index + 1) % polygon.vertices.len();
                    let kind = types[index];
                    let next_kind = types[next];
                    let vertex = polygon.vertices[index];
                    let next_vertex = polygon.vertices[next];
                    if kind != BACK {
                        front_vertices.push(vertex);
                    }
                    if kind != FRONT {
                        back_vertices.push(vertex);
                    }
                    if (kind | next_kind) == SPANNING {
                        let direction = next_vertex - vertex;
                        let denominator = self.normal.dot(direction);
                        if denominator.abs() <= epsilon {
                            continue;
                        }
                        let parameter =
                            (self.offset - self.normal.dot(vertex.as_vector())) / denominator;
                        let intersection = vertex + direction * parameter.clamp(0.0, 1.0);
                        front_vertices.push(intersection);
                        back_vertices.push(intersection);
                    }
                }
                if let Some(polygon) = Polygon::new(front_vertices, polygon.role, epsilon) {
                    front.push(polygon);
                }
                if let Some(polygon) = Polygon::new(back_vertices, polygon.role, epsilon) {
                    back.push(polygon);
                }
            }
            _ => unreachable!("polygon classification uses two bits"),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct BspNode {
    plane: Option<SplitPlane>,
    front: Option<Box<Self>>,
    back: Option<Box<Self>>,
    polygons: Vec<Polygon>,
    epsilon: f64,
}

impl BspNode {
    fn from_polygons(polygons: Vec<Polygon>, epsilon: f64) -> Self {
        let mut node = Self {
            epsilon,
            ..Self::default()
        };
        node.build(polygons);
        node
    }

    fn invert(&mut self) {
        for polygon in &mut self.polygons {
            polygon.invert();
        }
        if let Some(plane) = &mut self.plane {
            plane.flip();
        }
        if let Some(front) = &mut self.front {
            front.invert();
        }
        if let Some(back) = &mut self.back {
            back.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    fn clip_polygons(&self, polygons: Vec<Polygon>) -> Vec<Polygon> {
        let Some(plane) = self.plane else {
            return polygons;
        };
        let mut front = Vec::new();
        let mut back = Vec::new();
        for polygon in polygons {
            let mut coplanar_front = Vec::new();
            let mut coplanar_back = Vec::new();
            plane.split_polygon(
                &polygon,
                self.epsilon,
                &mut coplanar_front,
                &mut coplanar_back,
                &mut front,
                &mut back,
            );
            front.extend(coplanar_front);
            back.extend(coplanar_back);
        }
        if let Some(node) = &self.front {
            front = node.clip_polygons(front);
        }
        if let Some(node) = &self.back {
            back = node.clip_polygons(back);
        } else {
            back.clear();
        }
        front.extend(back);
        front
    }

    fn clip_to(&mut self, other: &Self) {
        self.polygons = other.clip_polygons(std::mem::take(&mut self.polygons));
        if let Some(front) = &mut self.front {
            front.clip_to(other);
        }
        if let Some(back) = &mut self.back {
            back.clip_to(other);
        }
    }

    fn all_polygons(&self) -> Vec<Polygon> {
        let mut polygons = self.polygons.clone();
        if let Some(front) = &self.front {
            polygons.extend(front.all_polygons());
        }
        if let Some(back) = &self.back {
            polygons.extend(back.all_polygons());
        }
        polygons
    }

    fn build(&mut self, polygons: Vec<Polygon>) {
        if polygons.is_empty() {
            return;
        }
        let plane = *self.plane.get_or_insert(polygons[0].plane);
        let mut front = Vec::new();
        let mut back = Vec::new();
        for polygon in polygons {
            let mut coplanar_front = Vec::new();
            let mut coplanar_back = Vec::new();
            plane.split_polygon(
                &polygon,
                self.epsilon,
                &mut coplanar_front,
                &mut coplanar_back,
                &mut front,
                &mut back,
            );
            self.polygons.extend(coplanar_front);
            self.polygons.extend(coplanar_back);
        }
        if !front.is_empty() {
            self.front
                .get_or_insert_with(|| {
                    Box::new(Self {
                        epsilon: self.epsilon,
                        ..Self::default()
                    })
                })
                .build(front);
        }
        if !back.is_empty() {
            self.back
                .get_or_insert_with(|| {
                    Box::new(Self {
                        epsilon: self.epsilon,
                        ..Self::default()
                    })
                })
                .build(back);
        }
    }
}

fn subtract(mut left: BspNode, mut right: BspNode) -> BspNode {
    let epsilon = left.epsilon.max(right.epsilon);
    left.invert();
    left.clip_to(&right);
    right.clip_to(&left);
    right.invert();
    right.clip_to(&left);
    right.invert();
    left.build(right.all_polygons());
    left.invert();
    BspNode::from_polygons(left.all_polygons(), epsilon)
}

fn union(mut left: BspNode, mut right: BspNode) -> BspNode {
    let epsilon = left.epsilon.max(right.epsilon);
    left.clip_to(&right);
    right.clip_to(&left);
    right.invert();
    right.clip_to(&left);
    right.invert();
    left.build(right.all_polygons());
    BspNode::from_polygons(left.all_polygons(), epsilon)
}

/// Regularized multi-axis/successor edge finish.
///
/// The analytic edge-finisher owns complete parallel edges of a six-plane
/// prism. Once selected edge neighbourhoods interact, a vertex blend is an
/// N-sided setback surface rather than another independent cylinder. This
/// fallback evaluates the complete committed boundary and the union of all
/// requested removal sweeps through the same deterministic BSP tier used by
/// crossing face cuts. Chamfers remain planar-exact; fillet arcs are bounded
/// by the request's explicit approximation budget. No display tessellation is
/// ever published without passing the ordinary closed-solid validator.
pub(crate) fn finish_edges(
    source_topology: Option<&Topology>,
    scene: &DebugScene,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Option<Topology> {
    if targets.is_empty()
        || targets.len() > 64
        || !distance.is_finite()
        || distance < precision.min_feature_size
    {
        return None;
    }
    let mut unique = targets.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != targets.len()
        || targets.iter().any(|target| {
            target.snapshot != scene.snapshot || target.kind != artificer_protocol::EntityKind::Edge
        })
    {
        return None;
    }

    let epsilon = precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(1.0e-8)
        * 16.0;
    let source_polygons = source_topology
        .and_then(|topology| planar_topology_polygons(topology, epsilon))
        .unwrap_or_else(|| {
            scene
                .triangles
                .iter()
                .filter_map(|triangle| {
                    Polygon::new(
                        triangle.vertices.map(internal_point).to_vec(),
                        triangle.role,
                        epsilon,
                    )
                })
                .collect::<Vec<_>>()
        });
    if source_polygons.is_empty() {
        return None;
    }

    // Resolve the selected removal sweeps as one material volume before the
    // body is cut. This gives connected chamfer/fillet sets one shared corner
    // boundary instead of repeatedly splitting an already-split successor.
    let mut cutters = None::<BspNode>;
    let cutter_precision = if kind == EdgeFinishKind::Fillet
        && source_topology.is_some_and(|topology| topology.faces.len() > 6)
    {
        // Successor corner blends intersect an already faceted boundary. A
        // dense cutter amplifies coincident BSP split paths without adding a
        // meaningful visible improvement. Twelve arc panels retain a smooth
        // bounded transition while keeping those intersections regularizable;
        // first-generation fillets continue to use the document-wide cap.
        PrecisionPolicy {
            max_subdivisions: precision.max_subdivisions.min(12),
            ..precision
        }
    } else {
        precision
    };
    // Where the selected edges meet each other, the sweeps must overlap so the
    // union has material to build the mitre from. Everywhere else that overlap
    // buys nothing.
    let shared_endpoints = shared_selection_endpoints(scene, targets, epsilon);
    for target in targets {
        for polygons in edge_finish_cutters(
            scene,
            targets,
            *target,
            kind,
            distance,
            cutter_precision,
            epsilon,
            &shared_endpoints,
        )? {
            let cutter = BspNode::from_polygons(polygons, epsilon);
            cutters = Some(match cutters {
                None => cutter,
                Some(current) => union(current, cutter),
            });
        }
    }
    let result = subtract(BspNode::from_polygons(source_polygons, epsilon), cutters?);
    let conformed = conform_polygon_edges(result.all_polygons(), epsilon);
    let publication_epsilon = epsilon;
    topology_from_polygons_with_heal_limit(
        conformed,
        publication_epsilon,
        Some(distance.mul_add(6.0, publication_epsilon * 32.0)),
    )
}

fn planar_topology_polygons(topology: &Topology, epsilon: f64) -> Option<Vec<Polygon>> {
    let mut polygons = Vec::with_capacity(topology.faces.len());
    for face in &topology.faces {
        if !matches!(face.value.surface, Surface::Plane(_)) || !face.value.inner_loops.is_empty() {
            return None;
        }
        let outer_loop = &topology.loops[face.value.outer_loop.0].value;
        let mut vertices = Vec::with_capacity(outer_loop.coedges.len());
        for coedge_key in &outer_loop.coedges {
            let coedge = topology.coedges[coedge_key.0].value;
            let edge = topology.edges[coedge.edge.0].value;
            let vertex_key = match coedge.orientation {
                Orientation::Forward => edge.vertices[0],
                Orientation::Reverse => edge.vertices[1],
            };
            vertices.push(topology.vertices[vertex_key.0].value.point);
        }
        polygons.push(Polygon::new(vertices, face.value.role, epsilon)?);
    }
    Some(polygons)
}

/// The points where two or more of the selected segments meet each other.
fn shared_selection_endpoints(
    scene: &DebugScene,
    targets: &[EntityRef],
    epsilon: f64,
) -> Vec<Point3> {
    let endpoints = targets
        .iter()
        .flat_map(|target| {
            scene
                .edges
                .iter()
                .filter(move |edge| edge.source_edge == *target && !edge.is_smooth)
                .flat_map(|edge| edge.endpoints.map(internal_point))
        })
        .collect::<Vec<_>>();
    endpoints
        .iter()
        .enumerate()
        .filter(|(index, point)| {
            endpoints.iter().enumerate().any(|(other, candidate)| {
                other != *index && candidate.distance(**point) <= epsilon * 32.0
            })
        })
        .map(|(_, point)| *point)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn edge_finish_cutters(
    scene: &DebugScene,
    targets: &[EntityRef],
    target: EntityRef,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
    epsilon: f64,
    shared_endpoints: &[Point3],
) -> Option<Vec<Vec<Polygon>>> {
    let segments = scene
        .edges
        .iter()
        .filter(|edge| edge.source_edge == target && !edge.is_smooth)
        .collect::<Vec<_>>();
    if segments.is_empty() || segments.len() > 256 {
        return None;
    }
    segments
        .into_iter()
        .map(|edge| {
            edge_finish_segment_cutter(
                scene,
                targets,
                edge,
                kind,
                distance,
                precision,
                epsilon,
                shared_endpoints,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn edge_finish_segment_cutter(
    scene: &DebugScene,
    targets: &[EntityRef],
    edge: &crate::DebugEdge,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
    epsilon: f64,
    shared_endpoints: &[Point3],
) -> Option<Vec<Polygon>> {
    let [edge_start, edge_end] = edge.endpoints.map(internal_point);
    let edge_vector = edge_end - edge_start;
    let edge_length = edge_vector.length();
    if !edge_length.is_finite() || edge_length <= precision.min_feature_size {
        return None;
    }
    let axis = edge_vector / edge_length;
    let [mut u, mut v] = edge_inward_directions(scene, edge_start, edge_end, axis, epsilon)?;
    let corner_dot = u.dot(v).clamp(-1.0, 1.0);
    if 1.0 - corner_dot.abs() <= precision.angular_agreement_radians.max(1.0e-8) {
        return None;
    }
    if u.cross(v).dot(axis) < 0.0 {
        std::mem::swap(&mut u, &mut v);
    }
    let available_u = scene
        .vertices
        .iter()
        .map(|vertex| (internal_point(vertex.point) - edge_start).dot(u))
        .fold(0.0_f64, f64::max);
    let available_v = scene
        .vertices
        .iter()
        .map(|vertex| (internal_point(vertex.point) - edge_start).dot(v))
        .fold(0.0_f64, f64::max);
    let required_setback = if kind == EdgeFinishKind::Fillet {
        let half_angle_ratio = ((1.0 + corner_dot) / (1.0 - corner_dot).max(1.0e-12)).sqrt();
        distance * half_angle_ratio
    } else {
        distance
    };
    if required_setback
        >= available_u.min(available_v) - precision.min_feature_size.max(epsilon * 2.0)
    {
        return None;
    }

    let local = edge_finish_profile(kind, distance, precision, u, v)?;
    let extension = required_setback + epsilon * 8.0;
    let start_origin = edge_start + axis * -extension;
    let sweep = axis * (edge_length + extension * 2.0);
    let mut start = local
        .iter()
        .map(|point| start_origin + u * point.x + v * point.y)
        .collect::<Vec<_>>();
    let mut end = start
        .iter()
        .copied()
        .map(|point| point + sweep)
        .collect::<Vec<_>>();
    let carry_onto = |points: &mut Vec<Point3>, endpoint: Point3, normal: Vector3| {
        let denominator = axis.dot(normal);
        if denominator.abs() <= 1.0e-12 {
            return;
        }
        for point in points.iter_mut() {
            *point = *point + axis * ((endpoint - *point).dot(normal) / denominator);
        }
    };
    if let Some(miter) = meeting_edge_miter_plane(
        scene,
        targets,
        edge.endpoints,
        edge_start,
        axis,
        u,
        shared_endpoints,
        epsilon,
    ) {
        carry_onto(&mut start, edge_start, miter);
    } else if let Some(normal) =
        terminating_wall(scene, edge_start, axis * -1.0, shared_endpoints, epsilon)
    {
        carry_onto(&mut start, edge_start, normal);
    }
    if let Some(miter) = meeting_edge_miter_plane(
        scene,
        targets,
        edge.endpoints,
        edge_end,
        axis,
        u,
        shared_endpoints,
        epsilon,
    ) {
        carry_onto(&mut end, edge_end, miter);
    } else if let Some(normal) = terminating_wall(scene, edge_end, axis, shared_endpoints, epsilon)
    {
        carry_onto(&mut end, edge_end, normal);
    }
    let mut cap_triangles = ear_clip(&local);
    if cap_triangles.len() != local.len().saturating_sub(2) {
        cap_triangles = (1..local.len().saturating_sub(1))
            .map(|index| [0, index, index + 1])
            .collect();
    }
    let mut polygons = Vec::with_capacity(cap_triangles.len() * 2 + local.len());
    for [first, second, third] in cap_triangles {
        polygons.push(Polygon::new_narrow(
            vec![start[first], start[third], start[second]],
            FaceRole::FeatureEnd,
            epsilon,
        )?);
        polygons.push(Polygon::new_narrow(
            vec![end[first], end[second], end[third]],
            FaceRole::FeatureEnd,
            epsilon,
        )?);
    }
    for index in 0..start.len() {
        let next = (index + 1) % start.len();
        polygons.push(Polygon::new_narrow(
            vec![start[index], start[next], end[next], end[index]],
            FaceRole::FeatureSide(index as u32),
            epsilon,
        )?);
    }
    Some(polygons)
}

fn edge_finish_profile(
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
    u: Vector3,
    v: Vector3,
) -> Option<Vec<Point2>> {
    if kind == EdgeFinishKind::Chamfer {
        return Some(vec![
            Point2::new(0.0, 0.0),
            Point2::new(distance, 0.0),
            Point2::new(0.0, distance),
        ]);
    }
    let dot = u.dot(v).clamp(-1.0, 1.0);
    let denominator = 1.0 - dot * dot;
    let bisector_denominator = 1.0 + dot;
    if denominator <= precision.angular_agreement_radians.max(1.0e-10)
        || bisector_denominator <= precision.angular_agreement_radians.max(1.0e-10)
        || 1.0 - dot <= precision.angular_agreement_radians.max(1.0e-10)
    {
        return None;
    }
    let radius = distance;
    let setback = radius * ((1.0 + dot) / (1.0 - dot)).sqrt();
    let center_coeff = radius / denominator.sqrt();
    let center = (u + v) * center_coeff;
    let start = u * setback;
    let end = v * setback;
    let start_radius = start - center;
    let end_radius = end - center;
    let computed_radius = start_radius.length();
    if !computed_radius.is_finite() || computed_radius <= precision.min_feature_size {
        return None;
    }
    let first = start_radius / computed_radius;
    let last = end_radius / computed_radius;
    let sweep = (-dot).clamp(-1.0, 1.0).acos();
    let tangent = last + first * dot;
    let tangent_length = tangent.length();
    if !sweep.is_finite()
        || sweep <= precision.angular_agreement_radians
        || tangent_length <= 1.0e-12
    {
        return None;
    }
    let second = tangent / tangent_length;
    let tolerance = precision
        .approximation_budget
        .max(precision.modeling_resolution)
        .min(radius * 0.5);
    let maximum_angle = (2.0 * (1.0 - tolerance / radius).clamp(-1.0, 1.0).acos()).max(0.04);
    let maximum_subdivisions = precision.max_subdivisions.clamp(4, 96) as usize;
    let min_chord_budget = precision.linear_agreement.max(1.0e-8) * 128.0;
    let max_from_chord = (radius * sweep / min_chord_budget).max(2.0);
    let subdivisions = (sweep / maximum_angle)
        .ceil()
        .clamp(2.0, (maximum_subdivisions as f64).min(max_from_chord))
        as usize;
    let mut profile = Vec::with_capacity(subdivisions + 2);
    profile.push(Point2::new(0.0, 0.0));
    for step in 0..=subdivisions {
        let angle = sweep * step as f64 / subdivisions as f64;
        let point = center + first * (radius * angle.cos()) + second * (radius * angle.sin());
        let point_u = point.dot(u);
        let point_v = point.dot(v);
        profile.push(Point2::new(
            (point_u - dot * point_v) / denominator,
            (point_v - dot * point_u) / denominator,
        ));
    }
    Some(profile)
}

#[allow(clippy::too_many_arguments)]
fn meeting_edge_miter_plane(
    scene: &DebugScene,
    targets: &[EntityRef],
    current_endpoints: [artificer_protocol::Point3; 2],
    endpoint: Point3,
    axis: Vector3,
    u: Vector3,
    shared_endpoints: &[Point3],
    epsilon: f64,
) -> Option<Vector3> {
    if !shared_endpoints
        .iter()
        .any(|point| point.distance(endpoint) <= epsilon * 32.0)
    {
        return None;
    }
    let mut other_target = None;
    for other in &scene.edges {
        if !targets.contains(&other.source_edge)
            || other.is_smooth
            || other.endpoints == current_endpoints
            || other.endpoints == [current_endpoints[1], current_endpoints[0]]
        {
            continue;
        }
        let [other_start, other_end] = other.endpoints.map(internal_point);
        if other_start.distance(endpoint) <= epsilon * 32.0
            || other_end.distance(endpoint) <= epsilon * 32.0
        {
            if other_target.is_some() && other_target != Some(other.source_edge) {
                // 3 or more selected edges meet: let them union naturally
                return None;
            }
            other_target = Some(other.source_edge);
        }
    }
    other_target?;
    for other in &scene.edges {
        if Some(other.source_edge) != other_target
            || other.is_smooth
            || other.endpoints == current_endpoints
            || other.endpoints == [current_endpoints[1], current_endpoints[0]]
        {
            continue;
        }
        let [other_start, other_end] = other.endpoints.map(internal_point);
        let other_vec = other_end - other_start;
        let other_len = other_vec.length();
        if other_len <= epsilon {
            continue;
        }
        let other_axis = other_vec / other_len;
        let other_dir = if other_start.distance(endpoint) <= epsilon * 32.0 {
            Some(other_axis)
        } else if other_end.distance(endpoint) <= epsilon * 32.0 {
            Some(other_axis * -1.0)
        } else {
            None
        };
        if let Some(departure) = other_dir {
            let outward =
                if endpoint.distance(internal_point(current_endpoints[0])) <= epsilon * 32.0 {
                    axis * -1.0
                } else {
                    axis
                };
            let alignment = outward.dot(departure);
            if alignment >= 1.0 - 1.0e-5 {
                return None;
            }
            let corner_normal = axis.cross(u);
            let turn_cross = outward.cross(departure);
            if turn_cross.dot(corner_normal) < -1.0e-5 {
                // Hole / concave pocket rim corner: sweeps naturally overlap and merge
                return None;
            }
            let miter = outward + departure;
            let len = miter.length();
            if len > 1.0e-6 {
                return Some(miter / len);
            }
        }
    }
    None
}

/// The wall that stops a finish sweep leaving `endpoint` along `outward`, if
/// one does.
///
/// Every face meeting at the endpoint carries the boundary's own outward
/// normal, so a face the departure heads *behind* is a face the departure
/// would have to cut through. The two faces the finish eats into both contain
/// the edge, so their normals are square to the axis and never answer here;
/// what answers is the third face that terminates the edge — a block's end
/// face, whose normal runs with the departure and lets it leave, or a pocket's
/// perpendicular wall, which does not. An endpoint another selected edge also
/// reaches is exempt: the overlap there is the material their mitre is built
/// from.
fn terminating_wall(
    scene: &DebugScene,
    endpoint: Point3,
    outward: Vector3,
    shared_endpoints: &[Point3],
    epsilon: f64,
) -> Option<Vector3> {
    if shared_endpoints
        .iter()
        .any(|point| point.distance(endpoint) <= epsilon * 32.0)
    {
        return None;
    }
    let mut stopper = None::<(f64, Vector3)>;
    for triangle in &scene.triangles {
        for (vertex, normal) in triangle.vertices.iter().zip(&triangle.normals) {
            if internal_point(*vertex).distance(endpoint) > epsilon * 32.0 {
                continue;
            }
            let normal = Vector3::new(normal.x, normal.y, normal.z);
            let length = normal.length();
            if length <= epsilon {
                continue;
            }
            let normal = normal / length;
            let alignment = normal.dot(outward);
            if alignment >= -0.15 {
                continue;
            }
            if stopper.is_none_or(|(nearest, _)| alignment < nearest) {
                stopper = Some((alignment, normal));
            }
        }
    }
    stopper.map(|(_, normal)| normal)
}

fn edge_inward_directions(
    scene: &DebugScene,
    start: Point3,
    end: Point3,
    axis: Vector3,
    epsilon: f64,
) -> Option<[Vector3; 2]> {
    let contains =
        |candidate: Point3, expected: Point3| candidate.distance(expected) <= epsilon * 32.0;
    let midpoint = start + (end - start) * 0.5;
    let mut directions = Vec::<Vector3>::new();
    for triangle in &scene.triangles {
        let points = triangle.vertices.map(internal_point);
        if !points.iter().any(|point| contains(*point, start))
            || !points.iter().any(|point| contains(*point, end))
        {
            continue;
        }
        let centroid = Point3::new(
            (points[0].x + points[1].x + points[2].x) / 3.0,
            (points[0].y + points[1].y + points[2].y) / 3.0,
            (points[0].z + points[1].z + points[2].z) / 3.0,
        );
        let relative = centroid - midpoint;
        let tangent = relative - axis * relative.dot(axis);
        let length = tangent.length();
        if length <= epsilon {
            continue;
        }
        let direction = tangent / length;
        if directions
            .iter()
            .all(|existing| existing.dot(direction).abs() < 1.0 - 1.0e-5)
        {
            directions.push(direction);
        }
    }
    if directions.len() < 2 {
        for triangle in &scene.triangles {
            let points = triangle.vertices.map(internal_point);
            if !points
                .iter()
                .any(|point| contains(*point, start) || contains(*point, end))
            {
                continue;
            }
            let centroid = Point3::new(
                (points[0].x + points[1].x + points[2].x) / 3.0,
                (points[0].y + points[1].y + points[2].y) / 3.0,
                (points[0].z + points[1].z + points[2].z) / 3.0,
            );
            let relative = centroid - midpoint;
            let tangent = relative - axis * relative.dot(axis);
            let length = tangent.length();
            if length <= epsilon {
                continue;
            }
            let direction = tangent / length;
            if directions
                .iter()
                .all(|existing| existing.dot(direction).abs() < 1.0 - 1.0e-5)
            {
                directions.push(direction);
                if directions.len() == 2 {
                    break;
                }
            }
        }
    }
    if directions.len() == 2 {
        return Some([directions[0], directions[1]]);
    }

    // Triangle tessellation can split the source boundary differently on the
    // two incident faces. Axis-aligned bounds are a deterministic fallback for
    // the current cuboid/linear-feature family.
    let points = scene
        .vertices
        .iter()
        .map(|vertex| internal_point(vertex.point))
        .collect::<Vec<_>>();
    let minimum = Point3::new(
        points
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min),
        points
            .iter()
            .map(|point| point.y)
            .fold(f64::INFINITY, f64::min),
        points
            .iter()
            .map(|point| point.z)
            .fold(f64::INFINITY, f64::min),
    );
    let maximum = Point3::new(
        points
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max),
        points
            .iter()
            .map(|point| point.y)
            .fold(f64::NEG_INFINITY, f64::max),
        points
            .iter()
            .map(|point| point.z)
            .fold(f64::NEG_INFINITY, f64::max),
    );
    let components = [axis.x.abs(), axis.y.abs(), axis.z.abs()];
    let edge_axis = components
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))?
        .0;
    if components
        .iter()
        .enumerate()
        .any(|(index, component)| index != edge_axis && *component > 1.0e-6)
    {
        return None;
    }
    let coordinates = [start.x, start.y, start.z];
    let minima = [minimum.x, minimum.y, minimum.z];
    let maxima = [maximum.x, maximum.y, maximum.z];
    let mut fallback = Vec::new();
    for coordinate_axis in 0..3 {
        if coordinate_axis == edge_axis {
            continue;
        }
        let sign = if (coordinates[coordinate_axis] - minima[coordinate_axis]).abs()
            <= epsilon * 32.0
        {
            1.0
        } else if (coordinates[coordinate_axis] - maxima[coordinate_axis]).abs() <= epsilon * 32.0 {
            -1.0
        } else {
            return None;
        };
        fallback.push(match coordinate_axis {
            0 => Vector3::new(sign, 0.0, 0.0),
            1 => Vector3::new(0.0, sign, 0.0),
            _ => Vector3::new(0.0, 0.0, sign),
        });
    }
    (fallback.len() == 2).then(|| [fallback[0], fallback[1]])
}

pub(crate) fn mirror_scene(
    scene: &DebugScene,
    plane_origin: Point3,
    plane_normal: Vector3,
    precision: PrecisionPolicy,
) -> Option<Topology> {
    let epsilon = precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(1.0e-8)
        * 16.0;
    let normal = plane_normal / plane_normal.length();
    let polygons = scene
        .triangles
        .iter()
        .filter_map(|triangle| {
            let mut vertices = triangle
                .vertices
                .map(internal_point)
                .map(|point| point + normal * (-2.0 * (point - plane_origin).dot(normal)))
                .to_vec();
            vertices.reverse();
            Polygon::new(vertices, triangle.role, epsilon)
        })
        .collect();
    topology_from_polygons(polygons, epsilon)
}

pub(crate) fn linear_pattern_scene(
    scene: &DebugScene,
    direction: Vector3,
    spacing: f64,
    count: u16,
    precision: PrecisionPolicy,
) -> Option<Topology> {
    let epsilon = precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(1.0e-8)
        * 16.0;
    let direction = direction / direction.length();
    let mut polygons = Vec::new();
    for instance in 0..count {
        let offset = direction * (spacing * f64::from(instance));
        polygons.extend(scene.triangles.iter().filter_map(|triangle| {
            Polygon::new(
                triangle
                    .vertices
                    .map(internal_point)
                    .map(|point| point + offset)
                    .to_vec(),
                triangle.role,
                epsilon,
            )
        }));
    }
    topology_from_polygons(polygons, epsilon)
}

/// Subtracts a crossing prismatic profile and returns a fresh immutable body
/// boundary. The caller still runs the ordinary solid validator before commit.
pub(crate) fn subtract_crossing_profile(
    scene: &DebugScene,
    frame: artificer_protocol::PlanarFrame3,
    profile: &PlanarProfile2,
    direction: Vector3,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, PlanarProfileInputError> {
    // Iterated plane splitting can reach the same intersection through a
    // different arithmetic path on neighbouring polygons.  Keep the BSP
    // classifier above model resolution while remaining two orders of
    // magnitude below the default display approximation.
    let epsilon = precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(1.0e-8)
        * 16.0;
    let source_polygons = scene
        .triangles
        .iter()
        .filter_map(|triangle| {
            Polygon::new(
                triangle.vertices.map(internal_point).to_vec(),
                triangle.role,
                epsilon,
            )
        })
        .collect::<Vec<_>>();
    if source_polygons.is_empty() {
        return Err(face_error(FaceFeatureInputError::SourceUnsupported));
    }
    let first_side_role = scene
        .triangles
        .iter()
        .filter_map(|triangle| match triangle.role {
            FaceRole::FeatureSide(role) if role < u32::MAX - 1 => Some(role),
            _ => None,
        })
        .max()
        .map_or(0, |role| role.saturating_add(1));
    let validated = validate_analytic_profile_extrusion(frame, profile, distance, precision)?;
    let cutter = cutter_from_profile(
        &validated.regions,
        direction,
        distance,
        precision,
        epsilon,
        first_side_role,
    )?;
    let result = subtract(BspNode::from_polygons(source_polygons, epsilon), cutter);
    // Crossed analytic approximations can leave a minute non-planar boundary
    // cycle where independently split cylinder panels converge.  Heal only
    // approximation-scale residue; a body-scale opening remains invalid and
    // is rejected by the ordinary solid validator.
    let maximum_healed_cycle_span = precision
        .approximation_budget
        .max(precision.modeling_resolution)
        .max(precision.min_feature_size)
        * 512.0;
    topology_from_polygons_with_heal_limit(
        result.all_polygons(),
        epsilon,
        Some(maximum_healed_cycle_span),
    )
    .ok_or_else(|| face_error(FaceFeatureInputError::SweepCollision))
}

fn cutter_from_profile(
    regions: &[crate::analytic_extrusion::ValidatedAnalyticRegionExtrusion],
    direction: Vector3,
    distance: f64,
    precision: PrecisionPolicy,
    epsilon: f64,
    mut next_side_role: u32,
) -> Result<BspNode, PlanarProfileInputError> {
    let mut result = None::<BspNode>;
    for region in regions {
        let outer = prism_from_loop(
            &region.loops[0],
            region.frame,
            direction,
            distance,
            precision,
            epsilon,
            next_side_role,
        )?;
        next_side_role = next_side_role
            .saturating_add(u32::try_from(region.loops[0].segments.len()).unwrap_or(u32::MAX));
        let mut material = BspNode::from_polygons(outer, epsilon);
        for profile_hole in &region.loops[1..] {
            let hole = prism_from_loop(
                profile_hole,
                region.frame,
                direction,
                distance,
                precision,
                epsilon,
                next_side_role,
            )?;
            next_side_role = next_side_role
                .saturating_add(u32::try_from(profile_hole.segments.len()).unwrap_or(u32::MAX));
            material = subtract(material, BspNode::from_polygons(hole, epsilon));
        }
        result = Some(match result {
            None => material,
            Some(current) => union(current, material),
        });
    }
    result.ok_or(PlanarProfileInputError::EmptyProfile)
}

fn prism_from_loop(
    profile_loop: &AnalyticLoop,
    frame: Frame,
    direction: Vector3,
    distance: f64,
    precision: PrecisionPolicy,
    epsilon: f64,
    side_role_base: u32,
) -> Result<Vec<Polygon>, PlanarProfileInputError> {
    let sampled = sampled_loop(profile_loop, precision);
    if sampled.len() < 3 {
        return Err(face_error(FaceFeatureInputError::TooFewVertices));
    }
    let local = sampled
        .iter()
        .map(|sample| sample.point)
        .collect::<Vec<_>>();
    let start = sampled
        .iter()
        .map(|sample| frame.point(sample.point, 0.0))
        .collect::<Vec<_>>();
    let end = start
        .iter()
        .copied()
        .map(|point| point + direction * distance)
        .collect::<Vec<_>>();
    let mut polygons = Vec::new();
    for triangle in ear_clip(&local) {
        polygons.push(
            Polygon::new(
                triangle.map(|index| start[index]).to_vec(),
                FaceRole::FeatureEnd,
                epsilon,
            )
            .ok_or_else(|| face_error(FaceFeatureInputError::NumericallyUnrepresentable))?,
        );
        polygons.push(
            Polygon::new(
                [end[triangle[2]], end[triangle[1]], end[triangle[0]]].to_vec(),
                FaceRole::FeatureEnd,
                epsilon,
            )
            .ok_or_else(|| face_error(FaceFeatureInputError::NumericallyUnrepresentable))?,
        );
    }
    for index in 0..start.len() {
        let next = (index + 1) % start.len();
        polygons.push(
            Polygon::new(
                vec![start[index], end[index], end[next], start[next]],
                // Every sampled panel produced by one analytic sketch curve
                // remains one logical side surface. A circle therefore owns
                // one cylindrical carrier instead of presenting 32 unrelated
                // faces, while adjacent line/arc entities still retain the
                // real edge between their distinct curve owners.
                FaceRole::FeatureSide(side_role_base.saturating_add(sampled[index].source_curve)),
                epsilon,
            )
            .ok_or_else(|| face_error(FaceFeatureInputError::NumericallyUnrepresentable))?,
        );
    }
    Ok(polygons)
}

#[derive(Clone, Copy)]
struct SampledLoopPoint {
    point: Point2,
    source_curve: u32,
}

fn sampled_loop(profile_loop: &AnalyticLoop, precision: PrecisionPolicy) -> Vec<SampledLoopPoint> {
    let mut points = Vec::new();
    let mut source_curve = 0_u32;
    let mut previous = None::<Segment>;
    for segment in &profile_loop.segments {
        if previous.is_some_and(|previous| !previous.shares_side_carrier(*segment)) {
            source_curve = source_curve.saturating_add(1);
        }
        match *segment {
            Segment::Line { start, .. } => points.push(SampledLoopPoint {
                point: start,
                source_curve,
            }),
            Segment::Arc {
                center,
                radius,
                start_angle,
                sweep,
                ..
            } => {
                let tolerance = precision
                    .approximation_budget
                    .max(precision.modeling_resolution)
                    .min(radius * 0.5);
                let maximum_angle =
                    (2.0 * (1.0 - tolerance / radius).clamp(-1.0, 1.0).acos()).max(0.04);
                // Crossing-profile Booleans currently publish a faceted
                // successor because this deliberately small native kernel
                // slice does not yet own a general analytic surface/surface
                // intersection.  Sixteen panels made that implementation
                // detail visible in silhouettes and faceted interchange.
                // Keep subdivision adaptive to the chord-error budget, but
                // require a bounded 64-panel carrier (5.625 degrees for a
                // full circle). The hard cap is
                // important: BSP intersection cost grows much faster than the
                // input panel count when several voids cross.
                let full_turn_subdivisions =
                    (1_usize << precision.max_subdivisions.min(6)).clamp(32, 64);
                let maximum_subdivisions = ((full_turn_subdivisions as f64 * sweep.abs()
                    / std::f64::consts::TAU)
                    .ceil() as usize)
                    .clamp(1, full_turn_subdivisions);
                let minimum_angle_density = sweep.abs() / (std::f64::consts::TAU / 64.0);
                let subdivisions = (sweep.abs() / maximum_angle)
                    .ceil()
                    .max(minimum_angle_density.ceil())
                    .clamp(1.0, maximum_subdivisions as f64)
                    as usize;
                for index in 0..subdivisions {
                    let angle = sweep.mul_add(index as f64 / subdivisions as f64, start_angle);
                    points.push(SampledLoopPoint {
                        point: Point2::new(
                            radius.mul_add(angle.cos(), center.x),
                            radius.mul_add(angle.sin(), center.y),
                        ),
                        source_curve,
                    });
                }
            }
            Segment::Ellipse { .. } | Segment::Harmonic { .. } => {
                for step in 0..16 {
                    points.push(SampledLoopPoint {
                        point: segment.point_at(f64::from(step) / 16.0),
                        source_curve,
                    });
                }
            }
        }
        previous = Some(*segment);
    }
    points
}

fn ear_clip(points: &[Point2]) -> Vec<[usize; 3]> {
    let mut remaining = (0..points.len()).collect::<Vec<_>>();
    let mut triangles = Vec::new();
    while remaining.len() > 3 {
        let mut best_ear = None::<usize>;
        let mut best_area = 0.0_f64;
        for current in 0..remaining.len() {
            let previous = (current + remaining.len() - 1) % remaining.len();
            let next = (current + 1) % remaining.len();
            let triangle = [remaining[previous], remaining[current], remaining[next]];
            let area = signed_area(
                points[triangle[0]],
                points[triangle[1]],
                points[triangle[2]],
            );
            if area <= 1.0e-12 {
                continue;
            }
            if remaining.iter().copied().any(|candidate| {
                !triangle.contains(&candidate)
                    && point_in_triangle(points[candidate], triangle.map(|index| points[index]))
            }) {
                continue;
            }
            if area > best_area {
                best_area = area;
                best_ear = Some(current);
            }
        }
        let Some(index) = best_ear.or_else(|| {
            for current in 0..remaining.len() {
                let previous = (current + remaining.len() - 1) % remaining.len();
                let next = (current + 1) % remaining.len();
                let triangle = [remaining[previous], remaining[current], remaining[next]];
                if signed_area(
                    points[triangle[0]],
                    points[triangle[1]],
                    points[triangle[2]],
                ) > 0.0
                {
                    return Some(current);
                }
            }
            None
        }) else {
            return Vec::new();
        };
        let previous = (index + remaining.len() - 1) % remaining.len();
        let next = (index + 1) % remaining.len();
        triangles.push([remaining[previous], remaining[index], remaining[next]]);
        remaining.remove(index);
    }
    if remaining.len() == 3 {
        triangles.push([remaining[0], remaining[1], remaining[2]]);
    }
    triangles
}

fn signed_area(a: Point2, b: Point2, c: Point2) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn point_in_triangle(point: Point2, triangle: [Point2; 3]) -> bool {
    let signs = [
        signed_area(triangle[0], triangle[1], point),
        signed_area(triangle[1], triangle[2], point),
        signed_area(triangle[2], triangle[0], point),
    ];
    signs.iter().all(|value| *value >= 0.0) || signs.iter().all(|value| *value <= 0.0)
}

fn topology_from_polygons(polygons: Vec<Polygon>, epsilon: f64) -> Option<Topology> {
    topology_from_polygons_with_heal_limit(polygons, epsilon, None)
}

fn split_non_planar_polygons(polygons: Vec<Polygon>, epsilon: f64) -> Vec<Polygon> {
    let mut result = Vec::with_capacity(polygons.len());
    for polygon in polygons {
        if polygon.vertices.len() <= 3 {
            result.push(polygon);
            continue;
        }
        let Some(plane) = SplitPlane::from_points(&polygon.vertices, epsilon * epsilon) else {
            result.push(polygon);
            continue;
        };
        let p0 = polygon.vertices[0];
        let Some(u) = polygon.vertices.iter().skip(1).find_map(|p| {
            let d = *p - p0;
            (d.length() > epsilon).then(|| d / d.length())
        }) else {
            result.push(polygon);
            continue;
        };
        let v = plane.normal.cross(u);
        let planar_frame = Plane::new(p0, u, v);
        let planar_error = polygon
            .vertices
            .iter()
            .map(|p| ((*p - p0).dot(plane.normal)).abs())
            .fold(0.0_f64, f64::max);
        if planar_error > (epsilon * 1.0e-4).max(1.0e-12) {
            let projected = polygon
                .vertices
                .iter()
                .map(|p| planar_frame.project(*p))
                .collect::<Vec<_>>();
            let triangles = ear_clip(&projected);
            if triangles.len() == polygon.vertices.len().saturating_sub(2) {
                for tri in triangles {
                    if let Some(fragment) = Polygon::new(
                        tri.map(|i| polygon.vertices[i]).to_vec(),
                        polygon.role,
                        epsilon,
                    ) {
                        result.push(fragment);
                    }
                }
                continue;
            }
        }
        result.push(polygon);
    }
    result
}

fn topology_from_polygons_with_heal_limit(
    polygons: Vec<Polygon>,
    epsilon: f64,
    maximum_healed_cycle_span: Option<f64>,
) -> Option<Topology> {
    let polygons = split_non_planar_polygons(polygons, epsilon);
    let polygons = conform_polygon_edges(polygons, epsilon);
    // The BSP classifies a vertex as "on" a split plane within `epsilon`
    // without moving it there, so after several splits two spellings of one
    // corner can sit tens of `epsilon` apart where a cutter panel grazes an
    // existing edge. Welding at a coarser distance collapses those into one
    // vertex, and the sliver between them into nothing, instead of
    // publishing a face a few microns wide that overlaps its neighbours.
    // Welding first, then conforming again at the weld scale, means a
    // T-junction whose stem vertex had a second spelling is still split.
    // The distance scales with the body: the BSP's errors do, being the
    // conditioning of intersections a body's own size, and an absolute
    // weld that is right for a hundred-millimetre block would visibly bend
    // the panels of a part a few millimetres across.
    let extent = polygon_extent(&polygons);
    let weld = (extent * 1.0e-5).max(epsilon);
    let polygons = weld_polygon_vertices(polygons, weld);
    let polygons = conform_polygon_edges(polygons, weld / 8.0);
    let mut pending = VecDeque::from(polygons);
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let mut vertex_map = BTreeMap::<[i64; 3], VertexKey>::new();
    let mut edge_map = BTreeMap::<[usize; 2], EdgeKey>::new();
    let mut shell_faces = Vec::new();

    while let Some(polygon) = pending.pop_front() {
        if polygon.vertices.len() < 3 {
            continue;
        }
        let mut points = polygon.vertices;
        points.dedup_by(|left, right| left.distance(*right) <= weld);
        if points.len() >= 2 && points[0].distance(*points.last().unwrap()) <= weld {
            points.pop();
        }
        if points.len() < 3 {
            continue;
        }
        let mut vertex_keys = points
            .iter()
            .copied()
            .map(|point| {
                let key = welded_key(
                    |key| {
                        vertex_map
                            .get(key)
                            .map(|key| topology.vertices[key.0].value.point)
                    },
                    point,
                    weld,
                );
                *vertex_map.entry(key).or_insert_with(|| {
                    let vertex_key = VertexKey(topology.vertices.len());
                    topology.vertices.push(Record {
                        id: allocate_id(&mut next_id),
                        value: Vertex { point },
                    });
                    vertex_key
                })
            })
            .collect::<Vec<_>>();
        // Welding may map two neighbouring vertices of a sliver onto one key;
        // collapse those before the loop is inspected, so a fragment that is
        // still a polygon on its welded points is not thrown away.
        vertex_keys.dedup();
        while vertex_keys.len() > 1 && vertex_keys.first() == vertex_keys.last() {
            vertex_keys.pop();
        }
        if vertex_keys.len() < 3 {
            continue;
        }
        // A key that repeats non-consecutively is a pinch: the fragment
        // touches itself at one welded vertex. Split it there into two simple
        // loops and assemble each on its own; dropping the fragment would
        // leave a hole in the shell that no later stage can close.
        if let Some((first, second)) = repeated_key_pair(&vertex_keys) {
            let welded = vertex_keys
                .iter()
                .map(|key| topology.vertices[key.0].value.point)
                .collect::<Vec<_>>();
            let inner = welded[first..second].to_vec();
            let outer = welded[second..]
                .iter()
                .chain(welded[..first].iter())
                .copied()
                .collect::<Vec<_>>();
            for part in [inner, outer] {
                if part.len() >= 3
                    && let Some(fragment) = Polygon::new_narrow(part, polygon.role, epsilon)
                {
                    pending.push_back(fragment);
                }
            }
            continue;
        }
        // Welding chooses one canonical model point for every quantized
        // vertex. Build the face plane and its pcurves from those same points,
        // not from polygon-local pre-weld coordinates; otherwise two BSP
        // paths that differ below epsilon can publish a coedge which misses
        // its authoritative vertex by several modeling resolutions.
        points = vertex_keys
            .iter()
            .map(|key| topology.vertices[key.0].value.point)
            .collect();
        // The face normal comes from the whole loop, never from one vertex
        // triple: a sliver's first non-degenerate triple can be a few microns
        // wide and point anywhere.
        let newell = newell_normal(&points);
        let newell_length = newell.length();
        if !newell_length.is_finite() || newell_length <= epsilon * epsilon {
            continue;
        }
        let normal = newell / newell_length;
        let Some(u) = points
            .iter()
            .copied()
            .skip(1)
            .map(|point| point - points[0])
            .map(|direction| direction - normal * direction.dot(normal))
            .find(|direction| direction.length() > epsilon)
            .map(|direction| direction / direction.length())
        else {
            continue;
        };
        let v = normal.cross(u);
        let plane = Plane::new(points[0], u, v);
        let mut projected = points
            .iter()
            .map(|point| plane.project(*point))
            .collect::<Vec<_>>();
        // Quantized welding can move a many-sided BSP fragment by less than
        // the modeling tolerance while still making it microscopically
        // non-planar at the much stricter B-rep agreement tolerance. Split
        // only that fragment into planar triangles which reuse the same
        // welded vertices, and assemble those instead. Presentation later
        // marks their shared coplanar edges smooth, so this creates no
        // selectable/display seam.
        let planar_error = points
            .iter()
            .map(|point| ((*point - points[0]).dot(normal)).abs())
            .fold(0.0_f64, f64::max);
        if points.len() > 3 && planar_error > (epsilon * 1.0e-2).max(1.0e-7) {
            let triangles = ear_clip(&projected);
            let degenerate = triangles.iter().any(|triangle| {
                signed_area(
                    projected[triangle[0]],
                    projected[triangle[1]],
                    projected[triangle[2]],
                )
                .abs()
                    <= epsilon * epsilon
            });
            if triangles.len() == points.len().saturating_sub(2) && !degenerate {
                for triangle in triangles {
                    if let Some(fragment) = Polygon::new_narrow(
                        triangle.map(|index| points[index]).to_vec(),
                        polygon.role,
                        epsilon,
                    ) {
                        pending.push_back(fragment);
                    }
                }
            } else {
                // Fan from the centroid instead of from a corner: a corner
                // fan produces a zero-area triangle wherever three
                // consecutive vertices are collinear, which is exactly what
                // a conformed T-junction looks like, and dropping that
                // triangle would leave its neighbours' edges single-use.
                let centre = polygon_centroid(&points);
                for index in 0..points.len() {
                    let next = (index + 1) % points.len();
                    if let Some(fragment) = Polygon::new_narrow(
                        vec![points[index], points[next], centre],
                        polygon.role,
                        epsilon,
                    ) {
                        pending.push_back(fragment);
                    }
                }
            }
            continue;
        }
        let mut twice_area = projected
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let next = projected[(index + 1) % projected.len()];
                point.x * next.y - next.x * point.y
            })
            .sum::<f64>();
        if !twice_area.is_finite() || twice_area.abs() <= epsilon * epsilon {
            continue;
        }
        if twice_area < 0.0 {
            points.reverse();
            vertex_keys.reverse();
            projected.reverse();
            twice_area = -twice_area;
        }
        let mut coedges = Vec::new();
        for index in 0..points.len() {
            let next = (index + 1) % points.len();
            let start = vertex_keys[index];
            let end = vertex_keys[next];
            let ordered = if start.0 < end.0 {
                [start.0, end.0]
            } else {
                [end.0, start.0]
            };
            let edge_key = *edge_map.entry(ordered).or_insert_with(|| {
                let edge_key = EdgeKey(topology.edges.len());
                let vertices = [VertexKey(ordered[0]), VertexKey(ordered[1])];
                let endpoints = vertices.map(|key| topology.vertices[key.0].value.point);
                topology.edges.push(Record {
                    id: allocate_id(&mut next_id),
                    value: Edge::line(vertices, endpoints),
                });
                edge_key
            });
            let orientation = if topology.edges[edge_key.0].value.vertices == [start, end] {
                Orientation::Forward
            } else {
                Orientation::Reverse
            };
            let coedge_key = CoedgeKey(topology.coedges.len());
            topology.coedges.push(Record {
                id: allocate_id(&mut next_id),
                value: Coedge::line(edge_key, orientation, [projected[index], projected[next]]),
            });
            coedges.push(coedge_key);
        }
        let loop_key = LoopKey(topology.loops.len());
        topology.loops.push(Record {
            id: allocate_id(&mut next_id),
            value: Loop { coedges },
        });
        let face_key = FaceKey(topology.faces.len());
        topology.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: Face {
                surface: Surface::Plane(plane),
                outer_loop: loop_key,
                inner_loops: Vec::new(),
                role: polygon.role,
            },
        });
        shell_faces.push(face_key);
    }
    heal_planar_boundary_cycles(
        &mut topology,
        &mut next_id,
        &mut shell_faces,
        epsilon,
        maximum_healed_cycle_span,
    );
    if shell_faces.is_empty() {
        return None;
    }
    // A regularized difference can split one body into several disconnected
    // closed components. Preserve those as independent shells/solids instead
    // of publishing a disconnected shell that only looks like one body.
    let mut edge_faces = BTreeMap::<usize, Vec<FaceKey>>::new();
    for face_key in &shell_faces {
        for loop_key in topology.faces[face_key.0].value.loops() {
            for coedge in &topology.loops[loop_key.0].value.coedges {
                edge_faces
                    .entry(topology.coedges[coedge.0].value.edge.0)
                    .or_default()
                    .push(*face_key);
            }
        }
    }
    let mut adjacency = BTreeMap::<FaceKey, Vec<FaceKey>>::new();
    for faces in edge_faces.values() {
        for first in faces {
            for second in faces {
                if first != second {
                    adjacency.entry(*first).or_default().push(*second);
                }
            }
        }
    }
    let mut remaining = shell_faces
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    while let Some(seed) = remaining.pop_first() {
        let mut component = vec![seed];
        let mut cursor = 0;
        while cursor < component.len() {
            for neighbour in adjacency.get(&component[cursor]).into_iter().flatten() {
                if remaining.remove(neighbour) {
                    component.push(*neighbour);
                }
            }
            cursor += 1;
        }
        component.sort_by_key(|face| face.0);
        let shell_key = ShellKey(topology.shells.len());
        topology.shells.push(Record {
            id: allocate_id(&mut next_id),
            value: Shell { faces: component },
        });
        topology.solids.push(Record {
            id: allocate_id(&mut next_id),
            value: Solid {
                outer_shell: shell_key,
                inner_shells: Vec::new(),
            },
        });
    }
    Some(topology)
}

/// Closes bounded boundary cycles left where several BSP split paths converge
/// at one regularized transition. Every accepted cycle must be consistently
/// oriented by the already-present neighbouring faces. Planar cycles remain
/// planar caps; non-planar edge-finish transitions are triangulated from the
/// same certified boundary and approximation budget. Anything branched or
/// open remains rejected by the ordinary closed-solid validator.
fn heal_planar_boundary_cycles(
    topology: &mut Topology,
    next_id: &mut u64,
    shell_faces: &mut Vec<FaceKey>,
    epsilon: f64,
    maximum_cycle_span: Option<f64>,
) {
    let mut uses = vec![Vec::<CoedgeKey>::new(); topology.edges.len()];
    for (index, coedge) in topology.coedges.iter().enumerate() {
        uses[coedge.value.edge.0].push(CoedgeKey(index));
    }
    let mut boundary = Vec::<(usize, usize, EdgeKey, Orientation)>::new();
    for (edge_index, edge_uses) in uses.iter().enumerate() {
        if edge_uses.len() != 1 {
            continue;
        }
        let coedge = topology.coedges[edge_uses[0].0].value;
        let edge = topology.edges[edge_index].value;
        let missing_orientation = coedge.orientation.reversed();
        let [start, end] = match missing_orientation {
            Orientation::Forward => edge.vertices,
            Orientation::Reverse => [edge.vertices[1], edge.vertices[0]],
        };
        boundary.push((start.0, end.0, EdgeKey(edge_index), missing_orientation));
    }
    let mut unused = (0..boundary.len()).collect::<BTreeSet<_>>();
    while let Some(seed) = unused.first().copied() {
        let mut path = vec![seed];
        let mut visited_vertices = BTreeSet::from([boundary[seed].0, boundary[seed].1]);
        let directed = find_boundary_cycle(
            &boundary,
            &unused,
            boundary[seed].1,
            boundary[seed].0,
            &mut path,
            &mut visited_vertices,
        );
        let fallback = (!directed)
            .then(|| find_undirected_boundary_cycle(&boundary, &unused, seed))
            .flatten();
        if !directed && fallback.is_none() {
            break;
        }
        if let Some((fallback_path, _)) = &fallback {
            path.clone_from(fallback_path);
        }
        for edge in &path {
            unused.remove(edge);
        }
        let cycle = fallback.map_or_else(
            || path.into_iter().map(|index| boundary[index]).collect(),
            |(_, cycle)| cycle,
        );
        // Each record's start vertex, which the fan closure pairs with the
        // record; and the vertices in walking order, which the planar closure
        // triangulates. The two differ on an undirected fallback cycle, whose
        // records are listed along the walk but keep their own required
        // direction.
        let record_points = cycle
            .iter()
            .map(|(vertex, _, _, _)| topology.vertices[*vertex].value.point)
            .collect::<Vec<_>>();
        let Some(walk) = cycle_walk(&cycle) else {
            continue;
        };
        let points = walk
            .iter()
            .map(|vertex| topology.vertices[*vertex].value.point)
            .collect::<Vec<_>>();
        let cycle_span = boundary_cycle_span(&points);
        let Some(mut split_plane) = SplitPlane::from_points(&points, epsilon * epsilon) else {
            continue;
        };
        // The missing face must use every boundary edge in the record's
        // direction. Twice the vector area of those directed edges is the
        // outward normal that satisfies them all, whatever order they are
        // listed in; the split plane's sign is whatever its first vertex
        // triple happened to give.
        let required_normal = cycle
            .iter()
            .fold(Vector3::default(), |sum, (start, end, _, _)| {
                let start = topology.vertices[*start].value.point.as_vector();
                let end = topology.vertices[*end].value.point.as_vector();
                sum + start.cross(end)
            });
        if required_normal.dot(split_plane.normal) < 0.0 {
            split_plane.flip();
        }
        let first_direction = points[1] - points[0];
        if first_direction.length() <= epsilon {
            continue;
        }
        let u = first_direction / first_direction.length();
        let v = split_plane.normal.cross(u);
        let plane = Plane::new(points[0], u, v);
        let projected = points
            .iter()
            .map(|point| plane.project(*point))
            .collect::<Vec<_>>();
        let triangles = ear_clip(&projected);
        if triangles.len() != points.len().saturating_sub(2) {
            if maximum_cycle_span.is_some_and(|maximum| cycle_span <= maximum)
                && append_non_planar_boundary_fan(
                    topology,
                    next_id,
                    shell_faces,
                    &cycle,
                    &record_points,
                    epsilon,
                )
            {
                continue;
            }
            // Only the ordinary planar closure may span a large Boolean
            // boundary. A failed/non-planar cycle is healed solely inside the
            // caller's approximation-scale limit.
            continue;
        }
        let mut cycle_edges = BTreeMap::<[usize; 2], EdgeKey>::new();
        for (start, end, edge, _) in &cycle {
            cycle_edges.insert(
                if start < end {
                    [*start, *end]
                } else {
                    [*end, *start]
                },
                *edge,
            );
        }
        for mut triangle in triangles {
            let mut model_triangle = triangle.map(|index| points[index]);
            let triangle_u = model_triangle[1] - model_triangle[0];
            let triangle_cross = triangle_u.cross(model_triangle[2] - model_triangle[0]);
            let triangle_plane =
                if triangle_u.length() > epsilon && triangle_cross.length() > epsilon * epsilon {
                    let u = triangle_u / triangle_u.length();
                    let mut normal = triangle_cross / triangle_cross.length();
                    if normal.dot(split_plane.normal) < 0.0 {
                        normal = normal * -1.0;
                    }
                    Plane::new(model_triangle[0], u, normal.cross(u))
                } else {
                    plane
                };
            let p0 = triangle_plane.project(model_triangle[0]);
            let p1 = triangle_plane.project(model_triangle[1]);
            let p2 = triangle_plane.project(model_triangle[2]);
            let twice_area = (p1.x - p0.x) * (p2.y - p0.y) - (p1.y - p0.y) * (p2.x - p0.x);
            if twice_area < 0.0 {
                triangle.swap(1, 2);
                model_triangle.swap(1, 2);
            }
            let mut coedges = Vec::with_capacity(3);
            for side in 0..3 {
                let start = walk[triangle[side]];
                let end = walk[triangle[(side + 1) % 3]];
                let ordered = if start < end {
                    [start, end]
                } else {
                    [end, start]
                };
                let edge = *cycle_edges.entry(ordered).or_insert_with(|| {
                    let key = EdgeKey(topology.edges.len());
                    let vertices = [VertexKey(ordered[0]), VertexKey(ordered[1])];
                    let endpoints = vertices.map(|key| topology.vertices[key.0].value.point);
                    topology.edges.push(Record {
                        id: allocate_id(next_id),
                        value: Edge::line(vertices, endpoints),
                    });
                    key
                });
                let orientation = if topology.edges[edge.0].value.vertices
                    == [VertexKey(start), VertexKey(end)]
                {
                    Orientation::Forward
                } else {
                    Orientation::Reverse
                };
                let coedge_key = CoedgeKey(topology.coedges.len());
                topology.coedges.push(Record {
                    id: allocate_id(next_id),
                    value: Coedge::line(
                        edge,
                        orientation,
                        [
                            triangle_plane.project(model_triangle[side]),
                            triangle_plane.project(model_triangle[(side + 1) % 3]),
                        ],
                    ),
                });
                coedges.push(coedge_key);
            }
            let loop_key = LoopKey(topology.loops.len());
            topology.loops.push(Record {
                id: allocate_id(next_id),
                value: Loop { coedges },
            });
            let face_key = FaceKey(topology.faces.len());
            topology.faces.push(Record {
                id: allocate_id(next_id),
                value: Face {
                    surface: Surface::Plane(triangle_plane),
                    outer_loop: loop_key,
                    inner_loops: Vec::new(),
                    role: FaceRole::FeatureEnd,
                },
            });
            shell_faces.push(face_key);
        }
    }
}

/// The area-weighted normal of a closed loop (Newell's method): twice the
/// loop's vector area, oriented by its winding. Unlike a vertex triple it is
/// stable on slivers and on loops with reflex corners.
fn newell_normal(points: &[Point3]) -> Vector3 {
    let mut normal = Vector3::default();
    for (index, start) in points.iter().enumerate() {
        let end = points[(index + 1) % points.len()];
        normal = normal
            + Vector3::new(
                (start.y - end.y) * (start.z + end.z),
                (start.z - end.z) * (start.x + end.x),
                (start.x - end.x) * (start.y + end.y),
            );
    }
    normal
}

fn polygon_centroid(points: &[Point3]) -> Point3 {
    let inverse_count = 1.0 / points.len().max(1) as f64;
    let sum = points
        .iter()
        .fold(Vector3::default(), |sum, point| sum + point.as_vector())
        * inverse_count;
    Point3::new(sum.x, sum.y, sum.z)
}

/// The first pair of positions whose welded keys coincide without being
/// consecutive, which is where a fragment pinches against itself.
fn repeated_key_pair(keys: &[VertexKey]) -> Option<(usize, usize)> {
    let count = keys.len();
    for first in 0..count {
        for second in first + 2..count {
            if keys[first] == keys[second] && !(first == 0 && second == count - 1) {
                return Some((first, second));
            }
        }
    }
    None
}

fn boundary_cycle_span(points: &[Point3]) -> f64 {
    let Some(first) = points.first().copied() else {
        return 0.0;
    };
    let (minimum, maximum) =
        points
            .iter()
            .copied()
            .skip(1)
            .fold((first, first), |(minimum, maximum), point| {
                (
                    Point3::new(
                        minimum.x.min(point.x),
                        minimum.y.min(point.y),
                        minimum.z.min(point.z),
                    ),
                    Point3::new(
                        maximum.x.max(point.x),
                        maximum.y.max(point.y),
                        maximum.z.max(point.z),
                    ),
                )
            });
    minimum.distance(maximum)
}

/// Closes a small, ordered 3D boundary cycle with triangles sharing one
/// interior vertex. Unlike a planar ear clip this remains well-defined where
/// several curved Boolean panels converge on slightly different planes.
/// Callers must impose a strict span limit before invoking this routine.
fn append_non_planar_boundary_fan(
    topology: &mut Topology,
    next_id: &mut u64,
    shell_faces: &mut Vec<FaceKey>,
    cycle: &[BoundaryRecord],
    points: &[Point3],
    epsilon: f64,
) -> bool {
    if cycle.len() < 3 || cycle.len() != points.len() {
        return false;
    }
    let inverse_count = 1.0 / points.len() as f64;
    let center_vector = points
        .iter()
        .fold(Vector3::default(), |sum, point| sum + point.as_vector())
        * inverse_count;
    let center = Point3::new(center_vector.x, center_vector.y, center_vector.z);
    if points.iter().enumerate().any(|(index, start)| {
        let end = points[(index + 1) % points.len()];
        (*start - center).cross(end - center).length() <= epsilon * epsilon
    }) {
        return false;
    }

    let center_key = VertexKey(topology.vertices.len());
    topology.vertices.push(Record {
        id: allocate_id(next_id),
        value: Vertex { point: center },
    });
    let mut radial_edges = BTreeMap::<[usize; 2], EdgeKey>::new();
    for (index, &(start_index, end_index, boundary_edge, boundary_orientation)) in
        cycle.iter().enumerate()
    {
        let vertices = [VertexKey(start_index), VertexKey(end_index), center_key];
        let model_points = [points[index], points[(index + 1) % points.len()], center];
        let first_direction = model_points[1] - model_points[0];
        let cross = first_direction.cross(model_points[2] - model_points[0]);
        let first_direction = first_direction / first_direction.length();
        let normal = cross / cross.length();
        let plane = Plane::new(
            model_points[0],
            first_direction,
            normal.cross(first_direction),
        );
        let mut coedges = Vec::with_capacity(3);
        for side in 0..3 {
            let start = vertices[side];
            let end = vertices[(side + 1) % 3];
            let (edge, orientation) = if side == 0 {
                (boundary_edge, boundary_orientation)
            } else {
                let ordered = if start.0 < end.0 {
                    [start.0, end.0]
                } else {
                    [end.0, start.0]
                };
                let edge = *radial_edges.entry(ordered).or_insert_with(|| {
                    let key = EdgeKey(topology.edges.len());
                    let edge_vertices = [VertexKey(ordered[0]), VertexKey(ordered[1])];
                    let endpoints = edge_vertices.map(|key| topology.vertices[key.0].value.point);
                    topology.edges.push(Record {
                        id: allocate_id(next_id),
                        value: Edge::line(edge_vertices, endpoints),
                    });
                    key
                });
                let orientation = if topology.edges[edge.0].value.vertices == [start, end] {
                    Orientation::Forward
                } else {
                    Orientation::Reverse
                };
                (edge, orientation)
            };
            let coedge_key = CoedgeKey(topology.coedges.len());
            topology.coedges.push(Record {
                id: allocate_id(next_id),
                value: Coedge::line(
                    edge,
                    orientation,
                    [
                        plane.project(model_points[side]),
                        plane.project(model_points[(side + 1) % 3]),
                    ],
                ),
            });
            coedges.push(coedge_key);
        }
        let loop_key = LoopKey(topology.loops.len());
        topology.loops.push(Record {
            id: allocate_id(next_id),
            value: Loop { coedges },
        });
        let face_key = FaceKey(topology.faces.len());
        topology.faces.push(Record {
            id: allocate_id(next_id),
            value: Face {
                surface: Surface::Plane(plane),
                outer_loop: loop_key,
                inner_loops: Vec::new(),
                role: FaceRole::FeatureSide(u32::MAX),
            },
        });
        shell_faces.push(face_key);
    }
    true
}

type BoundaryRecord = (usize, usize, EdgeKey, Orientation);
type BoundaryCycle = (Vec<usize>, Vec<BoundaryRecord>);

/// The vertices of a cycle in walking order, one per record, starting at
/// the vertex the first record shares with the last. `None` when consecutive
/// records do not share a vertex, which is not a cycle.
fn cycle_walk(cycle: &[BoundaryRecord]) -> Option<Vec<usize>> {
    let count = cycle.len();
    if count < 3 {
        return None;
    }
    let (first_start, first_end, _, _) = cycle[0];
    let (last_start, last_end, _, _) = cycle[count - 1];
    let mut current = if first_start == last_start || first_start == last_end {
        first_start
    } else {
        first_end
    };
    let mut walk = Vec::with_capacity(count);
    for (start, end, _, _) in cycle {
        walk.push(current);
        current = if current == *start {
            *end
        } else if current == *end {
            *start
        } else {
            return None;
        };
    }
    (current == walk[0]).then_some(walk)
}

fn find_undirected_boundary_cycle(
    boundary: &[BoundaryRecord],
    unused: &BTreeSet<usize>,
    seed: usize,
) -> Option<BoundaryCycle> {
    let first = boundary[seed];
    let mut path = vec![seed];
    let mut cycle = vec![first];
    let mut visited = BTreeSet::from([first.0, first.1]);
    if extend_undirected_boundary_cycle(
        boundary,
        unused,
        first.1,
        first.0,
        &mut path,
        &mut cycle,
        &mut visited,
    ) {
        Some((path, cycle))
    } else {
        None
    }
}

fn extend_undirected_boundary_cycle(
    boundary: &[(usize, usize, EdgeKey, Orientation)],
    unused: &BTreeSet<usize>,
    cursor: usize,
    goal: usize,
    path: &mut Vec<usize>,
    cycle: &mut Vec<(usize, usize, EdgeKey, Orientation)>,
    visited: &mut BTreeSet<usize>,
) -> bool {
    if cursor == goal {
        return path.len() >= 3;
    }
    for candidate in unused.iter().copied() {
        if path.contains(&candidate) {
            continue;
        }
        let (start, end, edge, orientation) = boundary[candidate];
        let oriented = if start == cursor {
            (start, end, edge, orientation)
        } else if end == cursor {
            (end, start, edge, orientation.reversed())
        } else {
            continue;
        };
        if oriented.1 != goal && !visited.insert(oriented.1) {
            continue;
        }
        path.push(candidate);
        cycle.push(oriented);
        if extend_undirected_boundary_cycle(
            boundary, unused, oriented.1, goal, path, cycle, visited,
        ) {
            return true;
        }
        cycle.pop();
        path.pop();
        if oriented.1 != goal {
            visited.remove(&oriented.1);
        }
    }
    false
}

fn find_boundary_cycle(
    boundary: &[(usize, usize, EdgeKey, Orientation)],
    unused: &BTreeSet<usize>,
    cursor: usize,
    goal: usize,
    path: &mut Vec<usize>,
    visited_vertices: &mut BTreeSet<usize>,
) -> bool {
    if cursor == goal {
        return path.len() >= 3;
    }
    for candidate in unused.iter().copied() {
        let (start, end, _, _) = boundary[candidate];
        if start != cursor || path.contains(&candidate) {
            continue;
        }
        if end != goal && !visited_vertices.insert(end) {
            continue;
        }
        path.push(candidate);
        if find_boundary_cycle(boundary, unused, end, goal, path, visited_vertices) {
            return true;
        }
        path.pop();
        if end != goal {
            visited_vertices.remove(&end);
        }
    }
    false
}

/// BSP splitting is polygon-local: one face can acquire a vertex in the
/// middle of an edge while its neighbour retains the unsplit edge.  A B-rep
/// cannot publish that T-junction. Insert every collinear result vertex into
/// every containing polygon edge before topology is assembled, so both faces
/// reference identical edge segments.
fn conform_polygon_edges(mut polygons: Vec<Polygon>, epsilon: f64) -> Vec<Polygon> {
    let mut unique = BTreeMap::<[i64; 3], Point3>::new();
    for point in polygons.iter().flat_map(|polygon| &polygon.vertices) {
        let key = welded_key(|key| unique.get(key).copied(), *point, epsilon);
        unique.entry(key).or_insert(*point);
    }
    let candidates = unique.into_values().collect::<Vec<_>>();
    for polygon in &mut polygons {
        let original = std::mem::take(&mut polygon.vertices);
        let mut conformed = Vec::new();
        for index in 0..original.len() {
            let start = original[index];
            let end = original[(index + 1) % original.len()];
            let direction = end - start;
            let denominator = direction.dot(direction);
            let mut points = vec![(0.0_f64, start)];
            if denominator > epsilon * epsilon {
                for candidate in &candidates {
                    let parameter = (*candidate - start).dot(direction) / denominator;
                    if parameter <= 1.0e-9 || parameter >= 1.0 - 1.0e-9 {
                        continue;
                    }
                    let closest = start + direction * parameter;
                    if closest.distance(*candidate) <= epsilon * 8.0 {
                        points.push((parameter, *candidate));
                    }
                }
            }
            points.sort_by(|left, right| left.0.total_cmp(&right.0));
            points.dedup_by(|left, right| {
                quantized_key(left.1, epsilon) == quantized_key(right.1, epsilon)
            });
            conformed.extend(points.into_iter().map(|(_, point)| point));
        }
        polygon.vertices = conformed;
    }
    polygons
}

/// The diagonal of the bounding box of every polygon vertex.
fn polygon_extent(polygons: &[Polygon]) -> f64 {
    let mut minimum = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut maximum = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for point in polygons.iter().flat_map(|polygon| &polygon.vertices) {
        minimum = Point3::new(
            minimum.x.min(point.x),
            minimum.y.min(point.y),
            minimum.z.min(point.z),
        );
        maximum = Point3::new(
            maximum.x.max(point.x),
            maximum.y.max(point.y),
            maximum.z.max(point.z),
        );
    }
    let extent = minimum.distance(maximum);
    if extent.is_finite() { extent } else { 0.0 }
}

/// Replaces every polygon vertex by the canonical spelling of its welded
/// bucket, so every later stage sees one point per corner. Polygons that
/// collapse below three distinct vertices are dropped.
fn weld_polygon_vertices(polygons: Vec<Polygon>, weld: f64) -> Vec<Polygon> {
    let mut canonical = BTreeMap::<[i64; 3], Point3>::new();
    let mut welded = Vec::with_capacity(polygons.len());
    for mut polygon in polygons {
        for vertex in &mut polygon.vertices {
            let key = welded_key(|key| canonical.get(key).copied(), *vertex, weld);
            *vertex = *canonical.entry(key).or_insert(*vertex);
        }
        polygon
            .vertices
            .dedup_by(|left, right| left.distance(*right) <= weld);
        while polygon.vertices.len() > 1
            && polygon.vertices[0].distance(*polygon.vertices.last().unwrap()) <= weld
        {
            polygon.vertices.pop();
        }
        if polygon.vertices.len() >= 3 {
            welded.push(polygon);
        }
    }
    welded
}

/// The bucket a point should weld into, given the buckets already occupied.
///
/// Rounding each coordinate onto an `epsilon` grid decides "these are the same
/// point" with a hard boundary, and the boundary does not care how close the
/// two points are. Two evaluations of one intersection which differ in the
/// last bit share a bucket almost everywhere, and fall either side of one
/// exactly when the coordinate lands on a half-bucket — which the offsets a
/// finish sweep is built from, being whole multiples of `epsilon`, arrange
/// rather often. The two spellings then publish two vertices a nanometre
/// apart, and every face meeting there is torn into two single-use edges: an
/// open shell the validator rejects, from a body that was closed.
///
/// So read the grid as a hint rather than as the answer. A point within
/// `epsilon` of another is at most one bucket away on each axis, so look at
/// those neighbours and weld to the nearest representative genuinely inside
/// the tolerance. A point with no such neighbour keeps its own bucket, so this
/// only ever repairs a straddle; it never merges two points the grid had
/// already told apart by more than the tolerance.
fn welded_key(
    occupant: impl Fn(&[i64; 3]) -> Option<Point3>,
    point: Point3,
    epsilon: f64,
) -> [i64; 3] {
    let key = quantized_key(point, epsilon);
    if occupant(&key).is_some() {
        return key;
    }
    let mut nearest = None::<([i64; 3], f64)>;
    for x in -1..=1_i64 {
        for y in -1..=1_i64 {
            for z in -1..=1_i64 {
                if [x, y, z] == [0, 0, 0] {
                    continue;
                }
                let neighbour = [key[0] + x, key[1] + y, key[2] + z];
                let Some(occupied) = occupant(&neighbour) else {
                    continue;
                };
                let separation = occupied.distance(point);
                if separation <= epsilon && nearest.is_none_or(|(_, closest)| separation < closest)
                {
                    nearest = Some((neighbour, separation));
                }
            }
        }
    }
    nearest.map_or(key, |(neighbour, _)| neighbour)
}

fn quantized_key(point: Point3, epsilon: f64) -> [i64; 3] {
    [point.x, point.y, point.z].map(|coordinate| {
        let scaled = (coordinate / epsilon).round();
        scaled.clamp(i64::MIN as f64, i64::MAX as f64) as i64
    })
}

const fn allocate_id(next_id: &mut u64) -> EntityId {
    let id = EntityId::from_raw(*next_id);
    *next_id += 1;
    id
}

const fn internal_point(point: artificer_protocol::Point3) -> Point3 {
    Point3::new(point.x, point.y, point.z)
}

const fn face_error(error: FaceFeatureInputError) -> PlanarProfileInputError {
    PlanarProfileInputError::FaceFeature(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_circle_boolean_carrier_has_sixty_four_panels_and_one_logical_surface() {
        let center = Point2::new(0.0, 0.0);
        let loop_ = AnalyticLoop {
            segments: vec![
                Segment::Arc {
                    center,
                    start: Point2::new(1.0, 0.0),
                    end: Point2::new(-1.0, 0.0),
                    radius: 1.0,
                    start_angle: 0.0,
                    sweep: std::f64::consts::PI,
                },
                Segment::Arc {
                    center,
                    start: Point2::new(-1.0, 0.0),
                    end: Point2::new(1.0, 0.0),
                    radius: 1.0,
                    start_angle: std::f64::consts::PI,
                    sweep: std::f64::consts::PI,
                },
            ],
            signed_area: std::f64::consts::PI,
        };
        let sampled = sampled_loop(&loop_, PrecisionPolicy::default());
        assert_eq!(sampled.len(), 64);
        assert!(sampled.iter().all(|sample| sample.source_curve == 0));
    }
}

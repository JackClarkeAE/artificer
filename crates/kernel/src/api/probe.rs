//! Non-mutating probes: questions a verification-driven caller asks of a
//! session without changing it.
//!
//! A probe reads the session's committed snapshots and answers with a
//! number, the tier of that number, and the method behind it. Exact probes
//! integrate the analytic B-rep; approximate ones read the display facets
//! and say so. Nothing here commits, journals, or moves the current
//! snapshot: the session's digest is the same after any probe as before.

use artificer_protocol::{
    Aabb3, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, EntityKind, EntityRef,
    Point3, RequestId, Tier, Vector3,
};
use serde::{Deserialize, Serialize};

use crate::api::debug::{ApiError, ApiErrorCode};
use crate::api::interference::{FacetIndex, Placement, clearance};
use crate::api::query::MeasureTarget;
use crate::api::selectors::{EntitySelector, point_triangle_distance_sq, resolve_selector};
use crate::api::session::Session;
use crate::{CancellationToken, DebugScene, NativeKernel, Snapshot};

/// A question about the model. Every step-scoped probe takes an optional
/// `step` label and reads the body that step left behind; without one it
/// reads the current body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "probe", rename_all = "snake_case")]
pub enum ProbeRequest {
    /// Exact volume of a body.
    Volume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
    /// Exact surface area of a body.
    SurfaceArea {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
    /// Exact area of one face of the current body.
    Area { face: EntitySelector },
    /// Exact length of one edge of the current body.
    Length { edge: EntitySelector },
    /// Minimum distance between two entities or points of the current
    /// body. Exact between points, planar faces, and straight edges; read
    /// off display facets otherwise.
    Distance {
        from: MeasureTarget,
        to: MeasureTarget,
    },
    /// Volume common to the bodies two steps left behind, by a committed
    /// intersection Boolean that the session does not keep.
    IntersectionVolume { a: String, b: String },
    /// Whether a point lies inside a body: 1 inside, 0 outside.
    Contains {
        point: Point3,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
    /// The closest approach of the bodies two steps left behind, with
    /// where it is and whether they are apart, touching, or inside one
    /// another. No Boolean runs, so this answers for bodies the Boolean
    /// engine would refuse.
    Clearance { a: String, b: String },
    /// The thinnest wall of a body: the shortest inward ray from any facet
    /// centre to the far side. Read off display facets.
    MinWall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
}

/// A probe's answer: the number, its unit, how sure it is, and how it was
/// found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    /// The probe kind, as requested.
    pub probe: String,
    pub value: f64,
    /// `mm`, `mm^2`, `mm^3`, or `boolean`.
    pub unit: String,
    /// Exact when the value integrates the analytic body; approximate when
    /// it reads facets, or the body itself is faceted.
    pub tier: Tier,
    /// How the value was found, for the record.
    pub method: String,
    /// Where the value is, or why it is what it is, in words.
    pub detail: String,
}

/// Answers one probe against the session without changing it.
pub fn probe(session: &Session, request: &ProbeRequest) -> Result<ProbeResult, ApiError> {
    match request {
        ProbeRequest::Volume { step } => {
            let (snapshot, tier) = body(session, step.as_deref())?;
            Ok(ProbeResult {
                probe: "volume".to_owned(),
                value: snapshot.measures().volume,
                unit: "mm^3".to_owned(),
                tier,
                method: "exact shell integral of the committed B-rep".to_owned(),
                detail: tier_detail(tier),
            })
        }
        ProbeRequest::SurfaceArea { step } => {
            let (snapshot, tier) = body(session, step.as_deref())?;
            Ok(ProbeResult {
                probe: "surface_area".to_owned(),
                value: snapshot.measures().surface_area,
                unit: "mm^2".to_owned(),
                tier,
                method: "exact face integrals of the committed B-rep".to_owned(),
                detail: tier_detail(tier),
            })
        }
        ProbeRequest::Area { face } => {
            let entity = resolve(session, face)?;
            if entity.kind != EntityKind::Face {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    "The area probe needs a face selector",
                ));
            }
            let description =
                NativeKernel::describe_face(&session.snapshot, entity).map_err(ApiError::from)?;
            Ok(ProbeResult {
                probe: "area".to_owned(),
                value: description.area,
                unit: "mm^2".to_owned(),
                tier: session.tier(),
                method: format!(
                    "exact integral over the {} carrier",
                    description.geometry.surface_kind()
                ),
                detail: description.summary,
            })
        }
        ProbeRequest::Length { edge } => {
            let entity = resolve(session, edge)?;
            if entity.kind != EntityKind::Edge {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    "The length probe needs an edge selector",
                ));
            }
            let description =
                NativeKernel::describe_edge(&session.snapshot, entity).map_err(ApiError::from)?;
            Ok(ProbeResult {
                probe: "length".to_owned(),
                value: description.length,
                unit: "mm".to_owned(),
                tier: session.tier(),
                method: format!(
                    "exact arc length of the {}",
                    description.geometry.curve_kind()
                ),
                detail: description.summary,
            })
        }
        ProbeRequest::Distance { from, to } => distance(session, from, to),
        ProbeRequest::IntersectionVolume { a, b } => intersection_volume(session, a, b),
        ProbeRequest::Clearance { a, b } => clearance_between(session, a, b),
        ProbeRequest::Contains { point, step } => {
            let (snapshot, tier) = body(session, step.as_deref())?;
            let scene = NativeKernel::debug_scene(snapshot);
            let inside = contains(&scene, *point);
            let exact = NativeKernel::is_polyhedral(snapshot);
            Ok(ProbeResult {
                probe: "contains".to_owned(),
                value: if inside { 1.0 } else { 0.0 },
                unit: "boolean".to_owned(),
                tier: if exact { tier } else { Tier::Approximate },
                method: if exact {
                    "ray parity against the body's planar faces".to_owned()
                } else {
                    "ray parity against display facets".to_owned()
                },
                detail: format!(
                    "({}, {}, {}) is {} the body",
                    point.x,
                    point.y,
                    point.z,
                    if inside { "inside" } else { "outside" }
                ),
            })
        }
        ProbeRequest::MinWall { step } => {
            let (snapshot, _) = body(session, step.as_deref())?;
            let scene = NativeKernel::debug_scene(snapshot);
            let (value, at) = min_wall(&scene).ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::KernelError,
                    "The body has no facets to measure a wall through",
                )
            })?;
            Ok(ProbeResult {
                probe: "min_wall".to_owned(),
                value,
                unit: "mm".to_owned(),
                tier: Tier::Approximate,
                method: "shortest inward ray from a facet centre to the far side".to_owned(),
                detail: format!("thinnest at ({:.3}, {:.3}, {:.3})", at.x, at.y, at.z),
            })
        }
    }
}

/// The closest approach of two committed bodies.
fn clearance_between(session: &Session, a: &str, b: &str) -> Result<ProbeResult, ApiError> {
    let (first, _) = body(session, Some(a))?;
    let (second, _) = body(session, Some(b))?;
    let precision = first.precision_policy().unwrap_or_default();
    let left = FacetIndex::build(first, Placement::IDENTITY);
    let right = FacetIndex::build(second, Placement::IDENTITY);
    let report = clearance(&left, &right, precision);
    Ok(ProbeResult {
        probe: "clearance".to_owned(),
        value: report.distance,
        unit: "mm".to_owned(),
        tier: report.tier,
        method: if report.bound > 0.0 {
            format!(
                "closest approach of display facets, within {} mm of the true surfaces",
                report.bound
            )
        } else {
            "closest approach of exact planar geometry".to_owned()
        },
        detail: format!(
            "\"{a}\" and \"{b}\" are {}; closest at ({:.4}, {:.4}, {:.4}) and ({:.4}, {:.4}, {:.4})",
            report.state.as_str(),
            report.witness_a.x,
            report.witness_a.y,
            report.witness_a.z,
            report.witness_b.x,
            report.witness_b.y,
            report.witness_b.z
        ),
    })
}

fn tier_detail(tier: Tier) -> String {
    match tier {
        Tier::Exact => "every step of this body was exact".to_owned(),
        Tier::Approximate => {
            "a step of this body fell to the faceted tier; the integral is exact over its facets"
                .to_owned()
        }
    }
}

/// The body a step left behind, or the current one, with its tier.
fn body<'a>(session: &'a Session, step: Option<&str>) -> Result<(&'a Snapshot, Tier), ApiError> {
    let Some(label) = step else {
        return Ok((&session.snapshot, session.tier()));
    };
    let id = session.step_snapshots.get(label).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::SelectorNotFound,
            format!("Step \"{label}\" is not in the session"),
        )
    })?;
    let snapshot = session.snapshot_cache.get(id).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::SessionError,
            format!("The snapshot of step \"{label}\" is no longer cached"),
        )
    })?;
    Ok((snapshot, session.tier_through(label)))
}

fn resolve(session: &Session, selector: &EntitySelector) -> Result<EntityRef, ApiError> {
    resolve_selector(
        selector,
        &session.snapshot,
        &session.step_order,
        &session.step_reports,
    )
}

fn intersection_volume(session: &Session, a: &str, b: &str) -> Result<ProbeResult, ApiError> {
    let (first, first_tier) = body(session, Some(a))?;
    let (second, second_tier) = body(session, Some(b))?;
    let tier = first_tier.combine(second_tier);
    if first.id() == second.id() {
        // One body overlaps itself entirely; the engine would refuse the
        // coincident contact, and rightly, so answer without it.
        return Ok(ProbeResult {
            probe: "intersection_volume".to_owned(),
            value: first.measures().volume,
            unit: "mm^3".to_owned(),
            tier,
            method: "the two steps left the same body".to_owned(),
            detail: format!("\"{a}\" and \"{b}\" are one snapshot"),
        });
    }
    let disjoint = match (first.measures().bounds, second.measures().bounds) {
        (Some(x), Some(y)) => !boxes_overlap(x, y),
        _ => true,
    };
    if disjoint {
        return Ok(ProbeResult {
            probe: "intersection_volume".to_owned(),
            value: 0.0,
            unit: "mm^3".to_owned(),
            tier,
            method: "bounding boxes do not overlap".to_owned(),
            detail: format!("\"{a}\" and \"{b}\" cannot share material"),
        });
    }
    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(format!("probe::intersection::{a}::{b}")),
        expected_target_snapshot: first.id(),
        expected_tool_snapshot: second.id(),
        precision: session.precision,
        operation: BooleanOperation::Intersection,
    };
    match NativeKernel::execute_boolean(first, second, &request, &CancellationToken::default()) {
        Ok(outcome) => {
            let rung = outcome.report.rung.clone().unwrap_or_default();
            Ok(ProbeResult {
                probe: "intersection_volume".to_owned(),
                value: outcome.snapshot.measures().volume,
                unit: "mm^3".to_owned(),
                tier: tier.combine(outcome.report.tier()),
                method: format!("committed intersection Boolean ({rung}), not kept"),
                detail: format!(
                    "the overlap of \"{a}\" and \"{b}\" is a solid with {}",
                    outcome.snapshot.counts()
                ),
            })
        }
        Err(error)
            if error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code.as_str() == "BOOLEAN_EMPTY_OR_UNRESOLVED_RESULT"
            }) =>
        {
            Ok(ProbeResult {
                probe: "intersection_volume".to_owned(),
                value: 0.0,
                unit: "mm^3".to_owned(),
                tier,
                method: "intersection Boolean found no regularized overlap".to_owned(),
                detail: format!("\"{a}\" and \"{b}\" share no solid material"),
            })
        }
        Err(error) => Err(ApiError::from(error).with_suggestion(
            "The Boolean engine cannot carry this pair of bodies; the report's diagnostics name the surface pair or contact that stopped it",
        )),
    }
}

fn boxes_overlap(a: Aabb3, b: Aabb3) -> bool {
    a.min.x <= b.max.x
        && b.min.x <= a.max.x
        && a.min.y <= b.max.y
        && b.min.y <= a.max.y
        && a.min.z <= b.max.z
        && b.min.z <= a.max.z
}

// ---------------------------------------------------------------------------
// Distance
// ---------------------------------------------------------------------------

/// A piece of a measurement target.
#[derive(Clone, Copy)]
enum Primitive {
    Point(Point3),
    Segment([Point3; 2]),
    Triangle([Point3; 3]),
}

/// What a target is made of, and whether that is its exact geometry.
struct Shape {
    primitives: Vec<Primitive>,
    exact: bool,
    description: String,
}

fn shape(session: &Session, target: &MeasureTarget, scene: &DebugScene) -> Result<Shape, ApiError> {
    match target {
        MeasureTarget::Point(point) => Ok(Shape {
            primitives: vec![Primitive::Point(*point)],
            exact: true,
            description: format!("({}, {}, {})", point.x, point.y, point.z),
        }),
        MeasureTarget::Entity(selector) => {
            let entity = resolve(session, selector)?;
            let snapshot = &session.snapshot;
            match entity.kind {
                EntityKind::Vertex => {
                    let vertex = scene
                        .vertices
                        .iter()
                        .find(|vertex| vertex.source_vertex == entity)
                        .ok_or_else(|| {
                            ApiError::new(
                                ApiErrorCode::SelectorNotFound,
                                "Vertex position unavailable",
                            )
                        })?;
                    Ok(Shape {
                        primitives: vec![Primitive::Point(vertex.point)],
                        exact: true,
                        description: format!("vertex {}", entity.entity),
                    })
                }
                EntityKind::Edge => {
                    let description =
                        NativeKernel::describe_edge(snapshot, entity).map_err(ApiError::from)?;
                    let primitives = scene
                        .edges
                        .iter()
                        .filter(|edge| edge.source_edge == entity)
                        .map(|edge| Primitive::Segment(edge.endpoints))
                        .collect::<Vec<_>>();
                    if primitives.is_empty() {
                        return Err(ApiError::new(
                            ApiErrorCode::SelectorNotFound,
                            "Edge geometry unavailable",
                        ));
                    }
                    Ok(Shape {
                        primitives,
                        exact: description.geometry.curve_kind() == "line",
                        description: description.summary,
                    })
                }
                EntityKind::Face => {
                    let description =
                        NativeKernel::describe_face(snapshot, entity).map_err(ApiError::from)?;
                    let primitives = scene
                        .triangles
                        .iter()
                        .filter(|triangle| triangle.source_face == entity)
                        .map(|triangle| Primitive::Triangle(triangle.vertices))
                        .collect::<Vec<_>>();
                    if primitives.is_empty() {
                        return Err(ApiError::new(
                            ApiErrorCode::SelectorNotFound,
                            "Face geometry unavailable",
                        ));
                    }
                    // A planar face with curved edges is triangulated to
                    // chords, so only a straight-edged plane is exact.
                    let exact = description.geometry.surface_kind() == "plane"
                        && scene
                            .edges
                            .iter()
                            .filter(|edge| edge.incident_faces.contains(&Some(entity)))
                            .all(|edge| {
                                NativeKernel::describe_edge(snapshot, edge.source_edge)
                                    .is_ok_and(|edge| edge.geometry.curve_kind() == "line")
                            });
                    Ok(Shape {
                        primitives,
                        exact,
                        description: description.summary,
                    })
                }
                other => Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    format!("Cannot measure a distance to an entity of kind {other:?}"),
                )),
            }
        }
    }
}

fn distance(
    session: &Session,
    from: &MeasureTarget,
    to: &MeasureTarget,
) -> Result<ProbeResult, ApiError> {
    let scene = NativeKernel::debug_scene(&session.snapshot);
    let first = shape(session, from, &scene)?;
    let second = shape(session, to, &scene)?;
    let mut best = f64::INFINITY;
    for a in &first.primitives {
        for b in &second.primitives {
            best = best.min(primitive_distance(*a, *b));
            if best == 0.0 {
                break;
            }
        }
    }
    let exact = first.exact && second.exact;
    Ok(ProbeResult {
        probe: "distance".to_owned(),
        value: best,
        unit: "mm".to_owned(),
        tier: if exact {
            session.tier()
        } else {
            Tier::Approximate
        },
        method: if exact {
            "closest points of exact planar geometry".to_owned()
        } else {
            "closest points of display facets".to_owned()
        },
        detail: format!("from {} to {}", first.description, second.description),
    })
}

fn primitive_distance(a: Primitive, b: Primitive) -> f64 {
    match (a, b) {
        (Primitive::Point(p), Primitive::Point(q)) => dist(p, q),
        (Primitive::Point(p), Primitive::Segment(s))
        | (Primitive::Segment(s), Primitive::Point(p)) => point_segment_distance(p, s),
        (Primitive::Point(p), Primitive::Triangle(t))
        | (Primitive::Triangle(t), Primitive::Point(p)) => point_triangle_distance_sq(p, &t).sqrt(),
        (Primitive::Segment(s), Primitive::Segment(u)) => segment_segment_distance(s, u),
        (Primitive::Segment(s), Primitive::Triangle(t))
        | (Primitive::Triangle(t), Primitive::Segment(s)) => segment_triangle_distance(s, &t),
        (Primitive::Triangle(t), Primitive::Triangle(u)) => {
            let mut best = f64::INFINITY;
            for edge in triangle_edges(&t) {
                best = best.min(segment_triangle_distance(edge, &u));
            }
            for edge in triangle_edges(&u) {
                best = best.min(segment_triangle_distance(edge, &t));
            }
            best
        }
    }
}

fn triangle_edges(t: &[Point3; 3]) -> [[Point3; 2]; 3] {
    [[t[0], t[1]], [t[1], t[2]], [t[2], t[0]]]
}

fn segment_triangle_distance(s: [Point3; 2], t: &[Point3; 3]) -> f64 {
    if segment_hits_triangle(s, t) {
        return 0.0;
    }
    let mut best = point_triangle_distance_sq(s[0], t)
        .sqrt()
        .min(point_triangle_distance_sq(s[1], t).sqrt());
    for edge in triangle_edges(t) {
        best = best.min(segment_segment_distance(s, edge));
    }
    best
}

/// Whether a segment crosses a triangle's interior (Möller–Trumbore, with
/// the segment's own extent as the parameter range).
fn segment_hits_triangle(s: [Point3; 2], t: &[Point3; 3]) -> bool {
    let direction = sub(s[1], s[0]);
    ray_triangle(s[0], direction, t).is_some_and(|hit| (0.0..=1.0).contains(&hit))
}

/// The ray parameter at which `origin + direction·t` crosses the triangle,
/// if it does (front or back), for `t ≥ 0`.
fn ray_triangle(origin: Point3, direction: Vector3, t: &[Point3; 3]) -> Option<f64> {
    const EPSILON: f64 = 1.0e-12;
    let edge1 = sub(t[1], t[0]);
    let edge2 = sub(t[2], t[0]);
    let h = cross(direction, edge2);
    let a = dot(edge1, h);
    if a.abs() < EPSILON * (length(edge1) * length(edge2) * length(direction)).max(EPSILON) {
        return None;
    }
    let f = 1.0 / a;
    let s = sub(origin, t[0]);
    let u = f * dot(s, h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, edge1);
    let v = f * dot(direction, q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let parameter = f * dot(edge2, q);
    (parameter >= 0.0).then_some(parameter)
}

fn point_segment_distance(p: Point3, s: [Point3; 2]) -> f64 {
    let d = sub(s[1], s[0]);
    let dd = dot(d, d);
    let t = if dd > 0.0 {
        (dot(sub(p, s[0]), d) / dd).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(p, add(s[0], scale(d, t)))
}

/// Closest approach of two segments (Ericson, Real-Time Collision
/// Detection, 5.1.9).
fn segment_segment_distance(a: [Point3; 2], b: [Point3; 2]) -> f64 {
    const EPSILON: f64 = 1.0e-18;
    let d1 = sub(a[1], a[0]);
    let d2 = sub(b[1], b[0]);
    let r = sub(a[0], b[0]);
    let la = dot(d1, d1);
    let lb = dot(d2, d2);
    let f = dot(d2, r);
    let (s, t);
    if la <= EPSILON && lb <= EPSILON {
        return dist(a[0], b[0]);
    }
    if la <= EPSILON {
        s = 0.0;
        t = (f / lb).clamp(0.0, 1.0);
    } else {
        let c = dot(d1, r);
        if lb <= EPSILON {
            t = 0.0;
            s = (-c / la).clamp(0.0, 1.0);
        } else {
            let bb = dot(d1, d2);
            let denominator = la * lb - bb * bb;
            let mut s_value = if denominator != 0.0 {
                ((bb * f - c * lb) / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t_value = (bb * s_value + f) / lb;
            if t_value < 0.0 {
                t_value = 0.0;
                s_value = (-c / la).clamp(0.0, 1.0);
            } else if t_value > 1.0 {
                t_value = 1.0;
                s_value = ((bb - c) / la).clamp(0.0, 1.0);
            }
            s = s_value;
            t = t_value;
        }
    }
    dist(add(a[0], scale(d1, s)), add(b[0], scale(d2, t)))
}

// ---------------------------------------------------------------------------
// Containment and walls
// ---------------------------------------------------------------------------

/// Whether `point` is inside the closed facet shell, by the parity of a
/// ray's crossings. The ray leans off every axis so it does not run along
/// a facet edge of an axis-aligned body.
fn contains(scene: &DebugScene, point: Point3) -> bool {
    let direction = Vector3::new(0.507_3, 0.331_9, 0.795_4);
    let crossings = scene
        .triangles
        .iter()
        .filter(|triangle| {
            ray_triangle(point, direction, &triangle.vertices).is_some_and(|t| t > 0.0)
        })
        .count();
    crossings % 2 == 1
}

/// The shortest inward ray from any facet centre to the far side of the
/// body, and where it starts.
fn min_wall(scene: &DebugScene) -> Option<(f64, Point3)> {
    // A body of many thousands of facets is sampled at every `stride`-th
    // origin so the quadratic search stays within a second.
    const MAX_ORIGINS: usize = 4000;
    let triangles = &scene.triangles;
    if triangles.is_empty() {
        return None;
    }
    let stride = triangles.len().div_ceil(MAX_ORIGINS).max(1);
    let mut best: Option<(f64, Point3)> = None;
    for (index, origin_triangle) in triangles.iter().enumerate().step_by(stride) {
        let [a, b, c] = origin_triangle.vertices;
        let centre = Point3::new(
            (a.x + b.x + c.x) / 3.0,
            (a.y + b.y + c.y) / 3.0,
            (a.z + b.z + c.z) / 3.0,
        );
        let [n0, n1, n2] = origin_triangle.normals;
        let outward = Vector3::new(
            (n0.x + n1.x + n2.x) / 3.0,
            (n0.y + n1.y + n2.y) / 3.0,
            (n0.z + n1.z + n2.z) / 3.0,
        );
        let outward_length = length(outward);
        if outward_length <= f64::EPSILON {
            continue;
        }
        let inward = scale(outward, -1.0 / outward_length);
        let mut nearest = f64::INFINITY;
        for (other_index, triangle) in triangles.iter().enumerate() {
            if other_index == index {
                continue;
            }
            // The far side faces the ray: its outward normal points along
            // the inward direction.
            let [m0, m1, m2] = triangle.normals;
            let facing = Vector3::new(m0.x + m1.x + m2.x, m0.y + m1.y + m2.y, m0.z + m1.z + m2.z);
            if dot(facing, inward) <= 0.0 {
                continue;
            }
            if let Some(t) = ray_triangle(centre, inward, &triangle.vertices)
                && t > 1.0e-9
                && t < nearest
            {
                nearest = t;
            }
        }
        if nearest.is_finite() && best.is_none_or(|(value, _)| nearest < value) {
            best = Some((nearest, centre));
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Vector arithmetic on protocol points
// ---------------------------------------------------------------------------

fn sub(a: Point3, b: Point3) -> Vector3 {
    Vector3::new(a.x - b.x, a.y - b.y, a.z - b.z)
}

fn add(a: Point3, v: Vector3) -> Point3 {
    Point3::new(a.x + v.x, a.y + v.y, a.z + v.z)
}

fn scale(v: Vector3, s: f64) -> Vector3 {
    Vector3::new(v.x * s, v.y * s, v.z * s)
}

fn dot(a: Vector3, b: Vector3) -> f64 {
    a.x * b.x + a.y * b.y + a.z * b.z
}

fn cross(a: Vector3, b: Vector3) -> Vector3 {
    Vector3::new(
        a.y * b.z - a.z * b.y,
        a.z * b.x - a.x * b.z,
        a.x * b.y - a.y * b.x,
    )
}

fn length(v: Vector3) -> f64 {
    dot(v, v).sqrt()
}

fn dist(a: Point3, b: Point3) -> f64 {
    length(sub(a, b))
}

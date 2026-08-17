//! Minimal STEP (ISO 10303-21) importer for analytic B-rep solids.
//!
//! Reads the subset a machined part actually uses — planes, cylinders,
//! cones, bounded by lines and circles — and tessellates it into a
//! `TriangleMesh` for the simulator and the pipeline. Anything outside
//! that subset (B-splines, toruses, offset surfaces) is refused by
//! name rather than approximated: a fixture that silently lies about
//! its own geometry poisons every measurement built on it.
//!
//! Two design rules keep the mesh watertight:
//! - **Every edge is discretized once**, cached by entity id, and both
//!   faces that share it reuse the same points, so shared boundaries
//!   weld exactly.
//! - **Loops are normalized by geometry, not by flag**: outer bounds
//!   are wound counter-clockwise and holes clockwise by signed area,
//!   whatever the file's orientation flags say, and the face's
//!   `same_sense` flag then decides which way the triangles face.

use crate::mesh::TriangleMesh;
use artificer_geometry::{Point3, Vector3};

#[derive(Debug)]
pub enum StepError {
    Syntax(String),
    Unsupported(String),
    Topology(String),
    EmptyResult,
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::Syntax(what) => write!(f, "STEP syntax: {what}"),
            StepError::Unsupported(what) => write!(f, "STEP unsupported: {what}"),
            StepError::Topology(what) => write!(f, "STEP topology: {what}"),
            StepError::EmptyResult => write!(f, "STEP file produced no triangles"),
        }
    }
}

impl std::error::Error for StepError {}

/// One parsed argument of an entity.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Ref(u64),
    Num(f64),
    Str(String),
    Enum(String),
    List(Vec<Value>),
    Null,
}

impl Value {
    fn as_ref(&self) -> Option<u64> {
        match self {
            Value::Ref(id) => Some(*id),
            _ => None,
        }
    }

    fn as_num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    fn is_true(&self) -> bool {
        matches!(self, Value::Enum(name) if name == "T")
    }
}

struct Entity {
    kind: String,
    args: Vec<Value>,
}

/// The DATA section as an id-keyed entity graph.
struct Graph {
    entities: std::collections::HashMap<u64, Entity>,
    /// Multiplies every coordinate into millimetres.
    scale: f64,
}

impl Graph {
    fn get(&self, id: u64, kind: &str) -> Result<&Entity, StepError> {
        let entity = self
            .entities
            .get(&id)
            .ok_or_else(|| StepError::Topology(format!("#{id} is referenced but absent")))?;
        if entity.kind != kind {
            return Err(StepError::Topology(format!(
                "#{id} is {} where {kind} was expected",
                entity.kind
            )));
        }
        Ok(entity)
    }

    fn point(&self, id: u64) -> Result<Point3, StepError> {
        let entity = self.get(id, "CARTESIAN_POINT")?;
        let coordinates = entity.args.get(1).and_then(Value::as_list).ok_or_else(|| {
            StepError::Syntax(format!("CARTESIAN_POINT #{id} has no coordinate list"))
        })?;
        let mut xyz = [0.0; 3];
        for (slot, value) in coordinates.iter().take(3).enumerate() {
            xyz[slot] = value.as_num().unwrap_or(0.0) * self.scale;
        }
        Ok(Point3::new(xyz[0], xyz[1], xyz[2]))
    }

    fn direction(&self, id: u64) -> Result<Vector3, StepError> {
        let entity = self.get(id, "DIRECTION")?;
        let ratios = entity
            .args
            .get(1)
            .and_then(Value::as_list)
            .ok_or_else(|| StepError::Syntax(format!("DIRECTION #{id} has no ratio list")))?;
        let mut xyz = [0.0; 3];
        for (slot, value) in ratios.iter().take(3).enumerate() {
            xyz[slot] = value.as_num().unwrap_or(0.0);
        }
        let v = Vector3::new(xyz[0], xyz[1], xyz[2]);
        let length = v.length();
        Ok(if length > 1e-12 {
            v / length
        } else {
            Vector3::new(0.0, 0.0, 1.0)
        })
    }

    /// AXIS2_PLACEMENT_3D -> right-handed frame (origin, x, y, z).
    fn frame(&self, id: u64) -> Result<(Point3, Vector3, Vector3, Vector3), StepError> {
        let entity = self.get(id, "AXIS2_PLACEMENT_3D")?;
        let origin = self.point(
            entity.args[1]
                .as_ref()
                .ok_or_else(|| StepError::Syntax(format!("placement #{id} lacks a location")))?,
        )?;
        let z = match entity.args.get(2).and_then(Value::as_ref) {
            Some(axis) => self.direction(axis)?,
            None => Vector3::new(0.0, 0.0, 1.0),
        };
        let hint = match entity.args.get(3).and_then(Value::as_ref) {
            Some(reference) => self.direction(reference)?,
            None => {
                if z.x.abs() < 0.9 {
                    Vector3::new(1.0, 0.0, 0.0)
                } else {
                    Vector3::new(0.0, 1.0, 0.0)
                }
            }
        };
        let x = {
            let projected = hint - z * hint.dot(z);
            let length = projected.length();
            if length > 1e-9 {
                projected / length
            } else if z.x.abs() < 0.9 {
                let p = Vector3::new(1.0, 0.0, 0.0) - z * z.x;
                p / p.length()
            } else {
                let p = Vector3::new(0.0, 1.0, 0.0) - z * z.y;
                p / p.length()
            }
        };
        let y = z.cross(x);
        Ok((origin, x, y, z))
    }
}

/// Parses the tokens inside one entity's argument parentheses.
fn parse_arguments(text: &str) -> Result<Vec<Value>, StepError> {
    fn parse_list(chars: &[u8], mut at: usize) -> Result<(Vec<Value>, usize), StepError> {
        let mut values = Vec::new();
        loop {
            while at < chars.len() && (chars[at] as char).is_whitespace() {
                at += 1;
            }
            if at >= chars.len() {
                return Err(StepError::Syntax("unterminated argument list".into()));
            }
            match chars[at] as char {
                ')' => return Ok((values, at + 1)),
                ',' => {
                    at += 1;
                }
                '(' => {
                    let (inner, next) = parse_list(chars, at + 1)?;
                    values.push(Value::List(inner));
                    at = next;
                }
                '\'' => {
                    let mut end = at + 1;
                    let mut text = String::new();
                    loop {
                        if end >= chars.len() {
                            return Err(StepError::Syntax("unterminated string".into()));
                        }
                        if chars[end] as char == '\'' {
                            // Doubled quote is an escaped quote.
                            if end + 1 < chars.len() && chars[end + 1] as char == '\'' {
                                text.push('\'');
                                end += 2;
                                continue;
                            }
                            break;
                        }
                        text.push(chars[end] as char);
                        end += 1;
                    }
                    values.push(Value::Str(text));
                    at = end + 1;
                }
                '#' => {
                    let mut end = at + 1;
                    while end < chars.len() && (chars[end] as char).is_ascii_digit() {
                        end += 1;
                    }
                    let id: u64 = std::str::from_utf8(&chars[at + 1..end])
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| StepError::Syntax("bad entity reference".into()))?;
                    values.push(Value::Ref(id));
                    at = end;
                }
                '.' => {
                    let mut end = at + 1;
                    while end < chars.len() && chars[end] as char != '.' {
                        end += 1;
                    }
                    values.push(Value::Enum(
                        std::str::from_utf8(&chars[at + 1..end])
                            .unwrap_or("")
                            .to_owned(),
                    ));
                    at = end + 1;
                }
                '$' | '*' => {
                    values.push(Value::Null);
                    at += 1;
                }
                _ => {
                    let mut end = at;
                    while end < chars.len() && !matches!(chars[end] as char, ',' | ')' | '(') {
                        end += 1;
                    }
                    let token = std::str::from_utf8(&chars[at..end])
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    if token.is_empty() {
                        return Err(StepError::Syntax("empty token".into()));
                    }
                    match token.parse::<f64>() {
                        Ok(number) => values.push(Value::Num(number)),
                        Err(_) => values.push(Value::Str(token)),
                    }
                    at = end;
                }
            }
        }
    }
    let chars = text.as_bytes();
    let (values, _) = parse_list(chars, 0)?;
    Ok(values)
}

fn parse_graph(bytes: &[u8]) -> Result<Graph, StepError> {
    let text = String::from_utf8_lossy(bytes);
    // Strip /* */ comments, then isolate the DATA section.
    let mut clean = String::with_capacity(text.len());
    let mut rest = text.as_ref();
    while let Some(open) = rest.find("/*") {
        clean.push_str(&rest[..open]);
        match rest[open..].find("*/") {
            Some(close) => rest = &rest[open + close + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    clean.push_str(rest);
    let data_start = clean
        .find("DATA;")
        .ok_or_else(|| StepError::Syntax("no DATA section".into()))?;
    let data = &clean[data_start + 5..];
    let data = &data[..data.find("ENDSEC").unwrap_or(data.len())];
    // Units: AP214 writes length units as a complex instance; presence
    // of the MILLI prefix is the practical test. A bare metre file
    // scales by a thousand.
    let scale = if clean.contains("SI_UNIT(.MILLI.,.METRE.)")
        || clean.contains("SI_UNIT( .MILLI., .METRE. )")
    {
        1.0
    } else if clean.contains("SI_UNIT($,.METRE.)") {
        1000.0
    } else {
        1.0
    };
    let mut entities = std::collections::HashMap::new();
    for statement in data.split(';') {
        let statement = statement.trim();
        if statement.is_empty() || !statement.starts_with('#') {
            continue;
        }
        let Some(equals) = statement.find('=') else {
            continue;
        };
        let Ok(id) = statement[1..equals].trim().parse::<u64>() else {
            continue;
        };
        let body = statement[equals + 1..].trim();
        // Complex instances open with a bare parenthesis; nothing this
        // importer needs lives in one.
        let Some(open) = body.find('(') else { continue };
        let kind = body[..open].trim().to_owned();
        if kind.is_empty() {
            continue;
        }
        let args = parse_arguments(&body[open + 1..])?;
        entities.insert(id, Entity { kind, args });
    }
    Ok(Graph { entities, scale })
}

/// A discretized edge: the polyline from the edge's start vertex to its
/// end vertex, in the curve's own direction.
struct EdgePoints {
    points: Vec<Point3>,
}

fn discretize_edge(graph: &Graph, edge_id: u64, chord: f64) -> Result<EdgePoints, StepError> {
    let edge = graph.get(edge_id, "EDGE_CURVE")?;
    let start_vertex = edge.args[1]
        .as_ref()
        .ok_or_else(|| StepError::Topology("edge without start vertex".into()))?;
    let end_vertex = edge.args[2]
        .as_ref()
        .ok_or_else(|| StepError::Topology("edge without end vertex".into()))?;
    let curve_id = edge.args[3]
        .as_ref()
        .ok_or_else(|| StepError::Topology("edge without curve".into()))?;
    let same_sense = edge.args[4].is_true();
    let start = graph.point(
        graph.get(start_vertex, "VERTEX_POINT")?.args[1]
            .as_ref()
            .ok_or_else(|| StepError::Topology("vertex without point".into()))?,
    )?;
    let end = graph.point(
        graph.get(end_vertex, "VERTEX_POINT")?.args[1]
            .as_ref()
            .ok_or_else(|| StepError::Topology("vertex without point".into()))?,
    )?;
    let curve = graph
        .entities
        .get(&curve_id)
        .ok_or_else(|| StepError::Topology(format!("curve #{curve_id} absent")))?;
    let mut points = match curve.kind.as_str() {
        "LINE" => vec![start, end],
        "CIRCLE" => {
            let placement = curve.args[1]
                .as_ref()
                .ok_or_else(|| StepError::Syntax("circle without placement".into()))?;
            let radius = curve.args[2]
                .as_num()
                .ok_or_else(|| StepError::Syntax("circle without radius".into()))?
                * graph.scale;
            let (center, x, y, _z) = graph.frame(placement)?;
            let angle_of = |p: Point3| -> f64 {
                let arm = p - center;
                arm.dot(y).atan2(arm.dot(x))
            };
            let theta_start = angle_of(start);
            let full_circle = start_vertex == end_vertex || (end - start).length() < 1e-9;
            let mut theta_end = if full_circle {
                theta_start + std::f64::consts::TAU
            } else {
                angle_of(end)
            };
            if same_sense {
                while theta_end <= theta_start + 1e-12 {
                    theta_end += std::f64::consts::TAU;
                }
            } else {
                while theta_end >= theta_start - 1e-12 {
                    theta_end -= std::f64::consts::TAU;
                }
            }
            let span = (theta_end - theta_start).abs();
            // Chord-height subdivision, never fewer than 8 arcs on a
            // full turn so tiny features stay round.
            let step = 2.0 * (1.0 - (chord / radius).min(0.5)).acos().max(1e-3);
            let count = ((span / step).ceil() as usize)
                .max((8.0 * span / std::f64::consts::TAU).ceil() as usize)
                .max(1);
            let mut sampled = Vec::with_capacity(count + 1);
            for index in 0..=count {
                let theta = theta_start + (theta_end - theta_start) * index as f64 / count as f64;
                sampled.push(Point3::new(
                    center.x + radius * (x.x * theta.cos() + y.x * theta.sin()),
                    center.y + radius * (x.y * theta.cos() + y.y * theta.sin()),
                    center.z + radius * (x.z * theta.cos() + y.z * theta.sin()),
                ));
            }
            // The topology's vertices are the truth the loop welds on.
            if let Some(first) = sampled.first_mut() {
                *first = start;
            }
            if let Some(last) = sampled.last_mut() {
                *last = if full_circle { start } else { end };
            }
            sampled
        }
        other => {
            return Err(StepError::Unsupported(format!(
                "curve type {other} (edge #{edge_id})"
            )));
        }
    };
    if !same_sense && curve.kind == "LINE" {
        points.reverse();
    }
    Ok(EdgePoints { points })
}

/// Walks one loop into a closed 3D polyline using cached edges.
fn loop_polyline(
    graph: &Graph,
    loop_id: u64,
    cache: &mut std::collections::HashMap<u64, EdgePoints>,
    chord: f64,
) -> Result<Vec<Point3>, StepError> {
    let edge_loop = graph.get(loop_id, "EDGE_LOOP")?;
    let oriented = edge_loop.args[1]
        .as_list()
        .ok_or_else(|| StepError::Topology("edge loop without edge list".into()))?;
    let mut polyline: Vec<Point3> = Vec::new();
    for value in oriented {
        let oriented_id = value
            .as_ref()
            .ok_or_else(|| StepError::Topology("edge loop holds a non-reference".into()))?;
        let oriented_edge = graph.get(oriented_id, "ORIENTED_EDGE")?;
        let edge_id = oriented_edge.args[3]
            .as_ref()
            .ok_or_else(|| StepError::Topology("oriented edge without edge".into()))?;
        let forwards = oriented_edge.args[4].is_true();
        if !cache.contains_key(&edge_id) {
            let discretized = discretize_edge(graph, edge_id, chord)?;
            cache.insert(edge_id, discretized);
        }
        let points = &cache[&edge_id].points;
        let walk: Vec<Point3> = if forwards {
            points.clone()
        } else {
            points.iter().rev().copied().collect()
        };
        for point in walk {
            if polyline
                .last()
                .is_none_or(|last| (*last - point).length() > 1e-9)
            {
                polyline.push(point);
            }
        }
    }
    // Closed: drop a duplicated closing point.
    if polyline.len() > 1 && (polyline[0] - *polyline.last().expect("non-empty")).length() < 1e-9 {
        polyline.pop();
    }
    if polyline.len() < 3 {
        return Err(StepError::Topology(format!(
            "loop #{loop_id} degenerates to {} point(s)",
            polyline.len()
        )));
    }
    Ok(polyline)
}

/// Ear clipping over a polygon whose holes have been bridged in.
///
/// Returns index triples into the vertex list. Quadratic and content
/// with it: a machined face's boundary is hundreds of points, not
/// millions.
fn ear_clip(polygon: &[(f64, f64)]) -> Vec<[usize; 3]> {
    let n = polygon.len();
    if n < 3 {
        return Vec::new();
    }
    let mut remaining: Vec<usize> = (0..n).collect();
    let mut triangles = Vec::with_capacity(n.saturating_sub(2));
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| -> f64 {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let coincident = |p: (f64, f64), q: (f64, f64)| -> bool {
        (p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) < 1e-18
    };
    let mut stall = 0usize;
    while remaining.len() > 3 && stall < remaining.len() {
        let m = remaining.len();
        let mut clipped = false;
        for slot in 0..m {
            let (i0, i1, i2) = (
                remaining[(slot + m - 1) % m],
                remaining[slot],
                remaining[(slot + 1) % m],
            );
            let (a, b, c) = (polygon[i0], polygon[i1], polygon[i2]);
            if cross(a, b, c) <= 1e-12 {
                continue;
            }
            // No remaining vertex may sit strictly inside the candidate
            // ear. Bridged polygons repeat vertices, so a point that
            // merely coincides with a corner or touches an edge does
            // not block — only genuine interior material does.
            let mut blocked = false;
            for &other in &remaining {
                if other == i0 || other == i1 || other == i2 {
                    continue;
                }
                let p = polygon[other];
                if coincident(p, a) || coincident(p, b) || coincident(p, c) {
                    continue;
                }
                let inside =
                    cross(a, b, p) > 1e-12 && cross(b, c, p) > 1e-12 && cross(c, a, p) > 1e-12;
                if inside {
                    blocked = true;
                    break;
                }
            }
            if blocked {
                continue;
            }
            triangles.push([i0, i1, i2]);
            remaining.remove(slot);
            clipped = true;
            break;
        }
        if clipped {
            stall = 0;
        } else {
            // Numerical corner: rotate the start and retry; the stall
            // counter stops a genuinely degenerate polygon looping.
            remaining.rotate_left(1);
            stall += 1;
        }
    }
    if remaining.len() == 3 {
        triangles.push([remaining[0], remaining[1], remaining[2]]);
    }
    triangles
}

fn signed_area(polygon: &[(f64, f64)]) -> f64 {
    let mut doubled = 0.0;
    for index in 0..polygon.len() {
        let (x0, y0) = polygon[index];
        let (x1, y1) = polygon[(index + 1) % polygon.len()];
        doubled += x0 * y1 - x1 * y0;
    }
    doubled / 2.0
}

/// Bridges holes into the outer ring so one ear clip covers the face.
///
/// Standard construction: take each hole's rightmost vertex, pick the
/// outer-ring vertex it can see most cheaply, and stitch the hole in
/// through a doubled bridge edge.
fn bridge_holes(
    outer: Vec<(f64, f64, usize)>,
    holes: Vec<Vec<(f64, f64, usize)>>,
) -> Vec<(f64, f64, usize)> {
    let mut merged = outer;
    let mut pending = holes;
    // Rightmost holes first: the classic ordering that keeps bridges
    // from crossing each other.
    pending.sort_by(|a, b| {
        let ax = a.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
        let bx = b.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
        bx.total_cmp(&ax)
    });
    for hole in pending {
        // The hole's rightmost vertex.
        let mouth = (0..hole.len())
            .max_by(|&a, &b| hole[a].0.total_cmp(&hole[b].0))
            .unwrap_or(0);
        let m = (hole[mouth].0, hole[mouth].1);
        // Cheapest visible vertex on the merged ring: nearest by
        // distance whose connecting segment crosses no merged edge.
        let mut best: Option<(f64, usize)> = None;
        'candidate: for (slot, candidate) in merged.iter().enumerate() {
            let c = (candidate.0, candidate.1);
            let cost = (c.0 - m.0).powi(2) + (c.1 - m.1).powi(2);
            if best.is_some_and(|(known, _)| cost >= known) {
                continue;
            }
            for index in 0..merged.len() {
                let a = merged[index];
                let b = merged[(index + 1) % merged.len()];
                if index == slot || (index + 1) % merged.len() == slot {
                    continue;
                }
                if segments_cross(m, c, (a.0, a.1), (b.0, b.1)) {
                    continue 'candidate;
                }
            }
            best = Some((cost, slot));
        }
        let Some((_, anchor)) = best else {
            // No visible vertex — leave the hole out rather than emit
            // crossing geometry.
            continue;
        };
        // Stitch: ...anchor, hole[mouth..], hole[..=mouth], anchor...
        let mut stitched: Vec<(f64, f64, usize)> =
            Vec::with_capacity(merged.len() + hole.len() + 2);
        stitched.extend_from_slice(&merged[..=anchor]);
        stitched.extend(hole[mouth..].iter().copied());
        stitched.extend(hole[..=mouth].iter().copied());
        stitched.push(merged[anchor]);
        stitched.extend_from_slice(&merged[anchor + 1..]);
        merged = stitched;
    }
    merged
}

fn segments_cross(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> bool {
    let orient = |o: (f64, f64), p: (f64, f64), q: (f64, f64)| -> f64 {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    };
    let (o1, o2) = (orient(a, b, c), orient(a, b, d));
    let (o3, o4) = (orient(c, d, a), orient(c, d, b));
    (o1 * o2 < -1e-18) && (o3 * o4 < -1e-18)
}

/// Zips two closed rings on a revolved surface into a triangle strip.
///
/// Both rings are walked by unwrapped angle about the axis; whichever
/// ring's next breakpoint comes first advances, so the strip honours
/// every sample either ring brought.
fn zip_rings(
    ring_a: &[Point3],
    ring_b: &[Point3],
    origin: Point3,
    x: Vector3,
    y: Vector3,
) -> Vec<[Point3; 3]> {
    let unwrap = |ring: &[Point3]| -> Vec<(f64, Point3)> {
        let mut previous = 0.0f64;
        let mut out = Vec::with_capacity(ring.len() + 1);
        for (index, &point) in ring.iter().enumerate() {
            let arm = point - origin;
            let mut theta = arm.dot(y).atan2(arm.dot(x));
            if index > 0 {
                while theta < previous - std::f64::consts::PI {
                    theta += std::f64::consts::TAU;
                }
                while theta > previous + std::f64::consts::PI {
                    theta -= std::f64::consts::TAU;
                }
            }
            previous = theta;
            out.push((theta, point));
        }
        // Close the ring one turn on from its start.
        if let Some(&(first_theta, first_point)) = out.first() {
            let direction = if previous >= first_theta { 1.0 } else { -1.0 };
            out.push((first_theta + direction * std::f64::consts::TAU, first_point));
        }
        out
    };
    let mut a = unwrap(ring_a);
    let mut b = unwrap(ring_b);
    // Walk both in increasing angle from a common origin.
    for ring in [&mut a, &mut b] {
        if ring.last().is_some_and(|last| last.0 < ring[0].0) {
            ring.reverse();
            for entry in ring.iter_mut() {
                entry.0 = -entry.0;
            }
        }
        let base = ring[0].0;
        for entry in ring.iter_mut() {
            entry.0 -= base;
        }
    }
    let mut triangles = Vec::new();
    let (mut ia, mut ib) = (0usize, 0usize);
    while ia + 1 < a.len() || ib + 1 < b.len() {
        let next_a = a.get(ia + 1).map(|entry| entry.0);
        let next_b = b.get(ib + 1).map(|entry| entry.0);
        let advance_a = match (next_a, next_b) {
            (Some(ta), Some(tb)) => ta <= tb,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if advance_a {
            triangles.push([a[ia].1, a[ia + 1].1, b[ib].1]);
            ia += 1;
        } else {
            triangles.push([b[ib + 1].1, b[ib].1, a[ia].1]);
            ib += 1;
        }
    }
    triangles.retain(|[p, q, r]| (*q - *p).cross(*r - *p).length() > 1e-12);
    triangles
}

/// Zips two open rails, both ascending in parameter, into a strip.
///
/// Whichever rail's next breakpoint comes first advances; when one
/// runs out the rest of the other fans to its end. Only original rail
/// points are emitted — interpolating new boundary points would break
/// the weld with neighbouring faces that share the cached edges.
fn zip_rails(a: &[(f64, Point3)], b: &[(f64, Point3)]) -> Vec<[Point3; 3]> {
    let mut triangles = Vec::new();
    if a.is_empty() || b.is_empty() {
        return triangles;
    }
    let (mut ia, mut ib) = (0usize, 0usize);
    while ia + 1 < a.len() || ib + 1 < b.len() {
        let next_a = a.get(ia + 1).map(|entry| entry.0);
        let next_b = b.get(ib + 1).map(|entry| entry.0);
        let advance_a = match (next_a, next_b) {
            (Some(ta), Some(tb)) => ta <= tb,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if advance_a {
            triangles.push([a[ia].1, a[ia + 1].1, b[ib].1]);
            ia += 1;
        } else {
            triangles.push([b[ib + 1].1, b[ib].1, a[ia].1]);
            ib += 1;
        }
    }
    triangles.retain(|[p, q, r]| (*q - *p).cross(*r - *p).length() > 1e-12);
    triangles
}

/// Drops constant-angle runs from a rail's ends.
///
/// A seam line lies at one angle, and the loop walk deposits it inside
/// a rail as a zero-sweep step the length of the part — zipped, that
/// step fans into knife slivers against the whole opposite rail. The
/// rails must be the monotone arcs alone; trimmed this way, each seam
/// comes back as the strip's exact end rung between the two rails'
/// first points.
fn trim_rail_seams(rail: &mut Vec<(f64, Point3)>) {
    const FLAT: f64 = 1e-7;
    while rail.len() >= 2 && (rail[1].0 - rail[0].0).abs() < FLAT {
        rail.remove(0);
    }
    while rail.len() >= 2 && (rail[rail.len() - 1].0 - rail[rail.len() - 2].0).abs() < FLAT {
        rail.pop();
    }
}

/// Whether a ring closes a whole turn about the axis.
fn full_turn(ring: &[Point3], origin: Point3, x: Vector3, y: Vector3) -> bool {
    let mut previous = 0.0f64;
    let mut total = 0.0f64;
    for (index, &point) in ring.iter().enumerate() {
        let arm = point - origin;
        let theta = arm.dot(y).atan2(arm.dot(x));
        if index > 0 {
            let mut delta = theta - previous;
            while delta > std::f64::consts::PI {
                delta -= std::f64::consts::TAU;
            }
            while delta < -std::f64::consts::PI {
                delta += std::f64::consts::TAU;
            }
            total += delta;
        }
        previous = theta;
    }
    total.abs() > 0.9 * std::f64::consts::TAU
}

/// Winds a revolved face's triangles to its material side: radially out
/// for `same_sense`, in against it.
fn orient_revolved(
    strip: Vec<[Point3; 3]>,
    origin: Point3,
    z: Vector3,
    same_sense: bool,
) -> Vec<[Point3; 3]> {
    strip
        .into_iter()
        .filter_map(|[pa, pb, pc]| {
            let centroid = Point3::new(
                (pa.x + pb.x + pc.x) / 3.0,
                (pa.y + pb.y + pc.y) / 3.0,
                (pa.z + pb.z + pc.z) / 3.0,
            );
            let arm = centroid - origin;
            let radial = arm - z * arm.dot(z);
            if radial.length() < 1e-12 {
                return None;
            }
            let wanted = if same_sense { radial } else { radial * -1.0 };
            let emitted = (pb - pa).cross(pc - pa);
            if emitted.length() < 1e-15 {
                return None;
            }
            Some(if emitted.dot(wanted) >= 0.0 {
                [pa, pb, pc]
            } else {
                [pa, pc, pb]
            })
        })
        .collect()
}

/// Splits triangles until their midpoints sit on the surface to within
/// twice the chord tolerance, sharing split midpoints through a cache
/// so neighbouring triangles stay sewn.
///
/// Boundary edges arrive already at chord tolerance (arcs were
/// discretized to it; seam lines lie on the surface exactly), so they
/// never trip the test and the face's rim is left untouched — which is
/// what keeps the joint with the neighbouring face exact.
fn refine_onto_surface(
    strip: Vec<[Point3; 3]>,
    project: &impl Fn(Point3) -> Point3,
    chord: f64,
) -> Vec<[Point3; 3]> {
    let key = |p: Point3| -> (i64, i64, i64) {
        (
            (p.x * 1e7).round() as i64,
            (p.y * 1e7).round() as i64,
            (p.z * 1e7).round() as i64,
        )
    };
    let mut midpoints: std::collections::HashMap<((i64, i64, i64), (i64, i64, i64)), Point3> =
        std::collections::HashMap::new();
    let mut split_of = |a: Point3, b: Point3| -> Option<Point3> {
        let middle = Point3::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0);
        let landed = project(middle);
        if (landed - middle).length() <= 2.0 * chord {
            return None;
        }
        let (ka, kb) = (key(a), key(b));
        let slot = if ka <= kb { (ka, kb) } else { (kb, ka) };
        Some(*midpoints.entry(slot).or_insert(landed))
    };
    let mut queue: Vec<([Point3; 3], u8)> =
        strip.into_iter().map(|triangle| (triangle, 0u8)).collect();
    let mut out = Vec::new();
    while let Some(([a, b, c], depth)) = queue.pop() {
        if depth >= 10 {
            out.push([a, b, c]);
            continue;
        }
        let (mab, mbc, mca) = (split_of(a, b), split_of(b, c), split_of(c, a));
        match (mab, mbc, mca) {
            (None, None, None) => out.push([a, b, c]),
            (Some(m), None, None) => {
                queue.push(([a, m, c], depth + 1));
                queue.push(([m, b, c], depth + 1));
            }
            (None, Some(m), None) => {
                queue.push(([b, m, a], depth + 1));
                queue.push(([m, c, a], depth + 1));
            }
            (None, None, Some(m)) => {
                queue.push(([c, m, b], depth + 1));
                queue.push(([m, a, b], depth + 1));
            }
            (Some(p), Some(q), None) => {
                queue.push(([p, b, q], depth + 1));
                queue.push(([a, p, q], depth + 1));
                queue.push(([a, q, c], depth + 1));
            }
            (None, Some(p), Some(q)) => {
                queue.push(([p, c, q], depth + 1));
                queue.push(([b, p, q], depth + 1));
                queue.push(([b, q, a], depth + 1));
            }
            (Some(p), None, Some(q)) => {
                queue.push(([a, p, q], depth + 1));
                queue.push(([p, b, c], depth + 1));
                queue.push(([q, p, c], depth + 1));
            }
            (Some(p), Some(q), Some(r)) => {
                queue.push(([a, p, r], depth + 1));
                queue.push(([p, b, q], depth + 1));
                queue.push(([r, q, c], depth + 1));
                queue.push(([p, q, r], depth + 1));
            }
        }
    }
    out
}

/// Reads a STEP file into a triangle mesh at the given chord tolerance
/// (mm). Returns the mesh and the notes of what was read.
pub fn read_step(bytes: &[u8], chord: f64) -> Result<(TriangleMesh, Vec<String>), StepError> {
    let graph = parse_graph(bytes)?;
    let mut notes = Vec::new();
    // Faces reach the mesh through every ADVANCED_FACE in every closed
    // shell; the solid/shell layers carry no geometry of their own.
    let mut face_ids: Vec<u64> = graph
        .entities
        .iter()
        .filter(|(_, entity)| entity.kind == "ADVANCED_FACE")
        .map(|(&id, _)| id)
        .collect();
    face_ids.sort_unstable();
    if face_ids.is_empty() {
        return Err(StepError::Topology("no ADVANCED_FACE entities".into()));
    }
    let mut cache: std::collections::HashMap<u64, EdgePoints> = std::collections::HashMap::new();
    let mut soup: Vec<[Point3; 3]> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for &face_id in &face_ids {
        let face = graph.get(face_id, "ADVANCED_FACE")?;
        let bounds = face.args[1]
            .as_list()
            .ok_or_else(|| StepError::Topology("face without bounds".into()))?;
        let surface_id = face.args[2]
            .as_ref()
            .ok_or_else(|| StepError::Topology("face without surface".into()))?;
        let same_sense = face.args[3].is_true();
        let surface = graph
            .entities
            .get(&surface_id)
            .ok_or_else(|| StepError::Topology(format!("surface #{surface_id} absent")))?;
        // Bound loops, outer first.
        let mut loops: Vec<(bool, Vec<Point3>)> = Vec::new();
        for bound_value in bounds {
            let bound_id = bound_value
                .as_ref()
                .ok_or_else(|| StepError::Topology("bound list holds non-reference".into()))?;
            let bound = graph
                .entities
                .get(&bound_id)
                .ok_or_else(|| StepError::Topology(format!("bound #{bound_id} absent")))?;
            let is_outer = bound.kind == "FACE_OUTER_BOUND";
            let loop_id = bound.args[1]
                .as_ref()
                .ok_or_else(|| StepError::Topology("bound without loop".into()))?;
            let polyline = loop_polyline(&graph, loop_id, &mut cache, chord)?;
            loops.push((is_outer, polyline));
        }
        loops.sort_by_key(|(is_outer, _)| std::cmp::Reverse(*is_outer));
        let face_triangles: Vec<[Point3; 3]> = match surface.kind.as_str() {
            "PLANE" => {
                let placement = surface.args[1]
                    .as_ref()
                    .ok_or_else(|| StepError::Syntax("plane without placement".into()))?;
                let (origin, x, y, z) = graph.frame(placement)?;
                let to_uv = |p: Point3| -> (f64, f64) {
                    let arm = p - origin;
                    (arm.dot(x), arm.dot(y))
                };
                // Flatten with source indices so triangles evaluate back
                // through the exact 3D boundary points.
                let mut vertices: Vec<Point3> = Vec::new();
                let mut outer: Vec<(f64, f64, usize)> = Vec::new();
                let mut holes: Vec<Vec<(f64, f64, usize)>> = Vec::new();
                for (index, (is_outer, polyline)) in loops.iter().enumerate() {
                    let mut ring: Vec<(f64, f64, usize)> = polyline
                        .iter()
                        .map(|&p| {
                            let (u, v) = to_uv(p);
                            vertices.push(p);
                            (u, v, vertices.len() - 1)
                        })
                        .collect();
                    let area =
                        signed_area(&ring.iter().map(|&(u, v, _)| (u, v)).collect::<Vec<_>>());
                    let outer_ring = *is_outer || (index == 0 && outer.is_empty());
                    // Outer counter-clockwise, holes clockwise.
                    if (outer_ring && area < 0.0) || (!outer_ring && area > 0.0) {
                        ring.reverse();
                    }
                    if outer_ring && outer.is_empty() {
                        outer = ring;
                    } else {
                        holes.push(ring);
                    }
                }
                let merged = bridge_holes(outer, holes);
                let polygon: Vec<(f64, f64)> = merged.iter().map(|&(u, v, _)| (u, v)).collect();
                let mut triangles = Vec::new();
                for [a, b, c] in ear_clip(&polygon) {
                    let (pa, pb, pc) = (
                        vertices[merged[a].2],
                        vertices[merged[b].2],
                        vertices[merged[c].2],
                    );
                    // Wind with the face normal.
                    let emitted = (pb - pa).cross(pc - pa);
                    let wanted = if same_sense { z } else { z * -1.0 };
                    if emitted.dot(wanted) >= 0.0 {
                        triangles.push([pa, pb, pc]);
                    } else {
                        triangles.push([pa, pc, pb]);
                    }
                }
                triangles
            }
            "CYLINDRICAL_SURFACE" | "CONICAL_SURFACE" => {
                let placement = surface.args[1]
                    .as_ref()
                    .ok_or_else(|| StepError::Syntax("surface without placement".into()))?;
                let (origin, x, y, z) = graph.frame(placement)?;
                let radius = surface.args[2]
                    .as_num()
                    .ok_or_else(|| StepError::Syntax("revolved surface without radius".into()))?
                    * graph.scale;
                // A cone's radius grows along +z by the tangent of its
                // half angle; a cylinder's does not.
                let slope = if surface.kind == "CONICAL_SURFACE" {
                    surface.args[3]
                        .as_num()
                        .ok_or_else(|| StepError::Syntax("cone without a half angle".into()))?
                        .tan()
                } else {
                    0.0
                };
                // Exact projection onto the surface: split any triangle
                // whose midpoint stands off it, so interior spans land
                // on the metal while boundary points stay exactly the
                // loop's own (they are already at chord tolerance, so
                // they never split and faces stay crack-free).
                let project = |p: Point3| -> Point3 {
                    let arm = p - origin;
                    let axial = arm.dot(z);
                    let radial = arm - z * axial;
                    let length = radial.length();
                    let target = (radius + slope * axial).abs().max(1e-9);
                    if length < 1e-12 {
                        return p;
                    }
                    let unit = radial / length;
                    Point3::new(
                        origin.x + z.x * axial + unit.x * target,
                        origin.y + z.y * axial + unit.y * target,
                        origin.z + z.z * axial + unit.z * target,
                    )
                };
                if loops.len() == 2 && loops.iter().all(|(_, ring)| full_turn(ring, origin, x, y)) {
                    // A full revolved band between two rings: zip them.
                    let strip = zip_rings(&loops[0].1, &loops[1].1, origin, x, y);
                    orient_revolved(strip, origin, z, same_sense)
                } else if loops.len() == 1 {
                    // The exporter split the surface at its seam: one
                    // loop that walks up one rim, down the seam, back
                    // along the other rim, and up again. In unwrapped
                    // angle that is two monotone rails joined by short
                    // seams, and zipping the rails is robust where a
                    // general ear clip degenerates (a 300:1 strip whose
                    // rails are collinear points has no mid-rail ears
                    // and fans from its corners, cutting the arc).
                    let polyline = &loops[0].1;
                    let mut unwrapped: Vec<(f64, Point3)> = Vec::with_capacity(polyline.len());
                    let mut previous = 0.0f64;
                    for (order, &p) in polyline.iter().enumerate() {
                        let arm = p - origin;
                        let mut theta = arm.dot(y).atan2(arm.dot(x));
                        if order > 0 {
                            while theta < previous - std::f64::consts::PI {
                                theta += std::f64::consts::TAU;
                            }
                            while theta > previous + std::f64::consts::PI {
                                theta -= std::f64::consts::TAU;
                            }
                        }
                        previous = theta;
                        unwrapped.push((theta, p));
                    }
                    // Start the cycle at the angular minimum; the walk
                    // to the maximum is one rail, the return the other.
                    let low = (0..unwrapped.len())
                        .min_by(|&i, &j| unwrapped[i].0.total_cmp(&unwrapped[j].0))
                        .unwrap_or(0);
                    unwrapped.rotate_left(low);
                    let base = unwrapped[0].0;
                    for entry in unwrapped.iter_mut() {
                        entry.0 -= base;
                    }
                    let high = (0..unwrapped.len())
                        .max_by(|&i, &j| unwrapped[i].0.total_cmp(&unwrapped[j].0))
                        .unwrap_or(0);
                    let mut rail_up: Vec<(f64, Point3)> = unwrapped[..=high].to_vec();
                    let mut rail_back: Vec<(f64, Point3)> = unwrapped[high..].to_vec();
                    rail_back.push(unwrapped[0]);
                    rail_back.reverse();
                    trim_rail_seams(&mut rail_up);
                    trim_rail_seams(&mut rail_back);
                    let strip = zip_rails(&rail_up, &rail_back);
                    let refined = refine_onto_surface(strip, &project, chord);
                    orient_revolved(refined, origin, z, same_sense)
                } else {
                    // Several bounds without two full rings: the
                    // general polygon path, holes and all.
                    let r_reference = {
                        let sum: f64 = loops
                            .iter()
                            .flat_map(|(_, ring)| ring.iter())
                            .map(|&p| {
                                let arm = p - origin;
                                (arm - z * arm.dot(z)).length()
                            })
                            .sum();
                        let count = loops.iter().map(|(_, r)| r.len()).sum::<usize>();
                        (sum / count.max(1) as f64).max(1e-6)
                    };
                    let mut vertices: Vec<Point3> = Vec::new();
                    let mut outer: Vec<(f64, f64, usize)> = Vec::new();
                    let mut holes: Vec<Vec<(f64, f64, usize)>> = Vec::new();
                    for (index, (is_outer, polyline)) in loops.iter().enumerate() {
                        let mut previous = 0.0f64;
                        let mut ring: Vec<(f64, f64, usize)> = polyline
                            .iter()
                            .enumerate()
                            .map(|(order, &p)| {
                                let arm = p - origin;
                                let mut theta = arm.dot(y).atan2(arm.dot(x));
                                if order > 0 {
                                    while theta < previous - std::f64::consts::PI {
                                        theta += std::f64::consts::TAU;
                                    }
                                    while theta > previous + std::f64::consts::PI {
                                        theta -= std::f64::consts::TAU;
                                    }
                                }
                                previous = theta;
                                vertices.push(p);
                                (theta * r_reference, arm.dot(z), vertices.len() - 1)
                            })
                            .collect();
                        let area =
                            signed_area(&ring.iter().map(|&(u, v, _)| (u, v)).collect::<Vec<_>>());
                        let outer_ring = *is_outer || (index == 0 && outer.is_empty());
                        if (outer_ring && area < 0.0) || (!outer_ring && area > 0.0) {
                            ring.reverse();
                        }
                        if outer_ring && outer.is_empty() {
                            outer = ring;
                        } else {
                            holes.push(ring);
                        }
                    }
                    let merged = bridge_holes(outer, holes);
                    let polygon: Vec<(f64, f64)> = merged.iter().map(|&(u, v, _)| (u, v)).collect();
                    let mut strip: Vec<[Point3; 3]> = Vec::new();
                    for [a, b, c] in ear_clip(&polygon) {
                        strip.push([
                            vertices[merged[a].2],
                            vertices[merged[b].2],
                            vertices[merged[c].2],
                        ]);
                    }
                    let refined = refine_onto_surface(strip, &project, chord);
                    orient_revolved(refined, origin, z, same_sense)
                }
            }
            other => {
                refused.push(format!("face #{face_id}: surface type {other}"));
                continue;
            }
        };
        soup.extend(face_triangles);
    }
    if !refused.is_empty() {
        notes.push(format!(
            "{} face(s) refused: {}",
            refused.len(),
            refused.join("; ")
        ));
    }
    let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-4).ok_or(StepError::EmptyResult)?;
    notes.push(format!(
        "STEP: {} face(s) -> {} triangles at {chord} mm chord",
        face_ids.len(),
        mesh.triangles().len()
    ));
    Ok((mesh, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_parse_nested_lists_strings_and_enums() {
        let values = parse_arguments("'a''b',#12,(1.5,-2.,#3),.T.,$,4)").expect("parse");
        assert_eq!(values[0], Value::Str("a'b".into()));
        assert_eq!(values[1], Value::Ref(12));
        assert_eq!(
            values[2],
            Value::List(vec![Value::Num(1.5), Value::Num(-2.0), Value::Ref(3)])
        );
        assert_eq!(values[3], Value::Enum("T".into()));
        assert_eq!(values[4], Value::Null);
        assert_eq!(values[5], Value::Num(4.0));
    }

    #[test]
    fn ear_clip_covers_a_square_annulus() {
        // Outer 10x10 square CCW, inner 4x4 square hole CW, bridged.
        let outer: Vec<(f64, f64, usize)> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]
            .iter()
            .enumerate()
            .map(|(i, &(u, v))| (u, v, i))
            .collect();
        let hole: Vec<(f64, f64, usize)> = [(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0)]
            .iter()
            .enumerate()
            .map(|(i, &(u, v))| (u, v, 4 + i))
            .collect();
        let merged = bridge_holes(outer, vec![hole]);
        let polygon: Vec<(f64, f64)> = merged.iter().map(|&(u, v, _)| (u, v)).collect();
        let triangles = ear_clip(&polygon);
        let area: f64 = triangles
            .iter()
            .map(|&[a, b, c]| {
                let (pa, pb, pc) = (polygon[a], polygon[b], polygon[c]);
                ((pb.0 - pa.0) * (pc.1 - pa.1) - (pc.0 - pa.0) * (pb.1 - pa.1)).abs() / 2.0
            })
            .sum();
        assert!(
            (area - 84.0).abs() < 1e-6,
            "annulus area 100-16=84, clipped {area}"
        );
    }

    #[test]
    fn rings_zip_into_a_closed_band() {
        // Two unit-spaced octagon rings about +Z.
        let ring = |z: f64, r: f64| -> Vec<Point3> {
            (0..8)
                .map(|k| {
                    let theta = std::f64::consts::TAU * k as f64 / 8.0;
                    Point3::new(r * theta.cos(), r * theta.sin(), z)
                })
                .collect()
        };
        let triangles = zip_rings(
            &ring(0.0, 5.0),
            &ring(1.0, 5.0),
            Point3::default(),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        );
        // A closed band over 8+8 breakpoints: 16 triangles.
        assert_eq!(triangles.len(), 16);
        let area: f64 = triangles
            .iter()
            .map(|[a, b, c]| (*b - *a).cross(*c - *a).length() / 2.0)
            .sum();
        // Octagon band lateral area: perimeter 8*2R*sin(pi/8) x height.
        let expected = 8.0 * 2.0 * 5.0 * (std::f64::consts::PI / 8.0).sin();
        assert!(
            (area - expected).abs() < 0.05 * expected,
            "band area {area} vs {expected}"
        );
    }

    /// The wheel-spacer STEP the importer was scoped against, when it
    /// is present on this machine: closed analytic solid in, watertight
    /// mesh out.
    #[test]
    fn reads_the_wheel_spacer_when_present() {
        let path = concat!(
            "/private/tmp/claude-501/-Users-jackclarke-Desktop-OpenCad/",
            "b99cff9e-fdbe-4667-b526-3892e78fc0c8/scratchpad/espacador/",
            "Espaçador de rodas/Espaçador de rodas.step"
        );
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let (mesh, notes) = read_step(&bytes, 0.03).expect("import");
        assert!(mesh.triangles().len() > 1000, "{notes:?}");
        let hygiene = crate::hygiene::inspect(&mesh);
        assert_eq!(hygiene.boundary_edges, 0, "watertight: {notes:?}");
    }
}

//! The reference mesh: what an imported part opens as when a face could
//! not be read into the kernel's vocabulary, or a shell does not close.
//!
//! Every face is triangulated the way the scan add-on's reader does it:
//! each edge is discretised once and shared by the faces either side, each
//! face's loops are laid out in its surface's parameters (a plane's own,
//! a revolved carrier's unwrapped azimuth and height, a B-spline's by
//! inversion), holes are bridged into the outer ring, the ring is
//! ear-clipped, and the triangles are refined onto the surface until their
//! midpoints sit on it within the chord. A face whose surface could not be
//! read is capped by its boundary in the best-fitting plane, and a face
//! whose curve could not be read is bounded by the chord between its
//! vertices. The mesh is then a faceted B-rep, labelled approximate.

use std::collections::HashMap;

use crate::step_import::conform::{EdgeRead, Importer};
use crate::topology::{Curve3, FaceRole, ParameterRange, Point2, Point3, Surface, Vector3};

/// One polygon of the reference mesh: its corners, wound about the
/// outward normal, and the STEP face it came from.
pub(crate) type MeshPolygon = (Vec<Point3>, FaceRole);

/// Triangulates every face at `chord` tolerance.
pub(crate) fn build(importer: &Importer<'_>, chord: f64) -> Vec<MeshPolygon> {
    let mut cache: HashMap<u64, Vec<Point3>> = HashMap::new();
    let mut polygons = Vec::new();
    for face in &importer.faces {
        let mut loops: Vec<Vec<Point3>> = Vec::new();
        for loop_read in &face.loops {
            if loop_read.uses.is_empty() {
                continue;
            }
            let mut polyline: Vec<Point3> = Vec::new();
            for edge_use in &loop_read.uses {
                let points = cache
                    .entry(edge_use.edge)
                    .or_insert_with(|| edge_polyline(importer, edge_use.edge, chord))
                    .clone();
                let walk: Box<dyn Iterator<Item = Point3>> = if edge_use.forward {
                    Box::new(points.into_iter())
                } else {
                    Box::new(points.into_iter().rev())
                };
                for point in walk {
                    if polyline
                        .last()
                        .is_none_or(|last| last.distance(point) > 1.0e-9)
                    {
                        polyline.push(point);
                    }
                }
            }
            if polyline.len() > 1 && polyline[0].distance(polyline[polyline.len() - 1]) <= 1.0e-9 {
                polyline.pop();
            }
            if polyline.len() >= 3 {
                loops.push(polyline);
            }
        }
        if loops.is_empty() {
            continue;
        }
        let role = FaceRole::FeatureSide(face.ordinal);
        let triangles = match &face.surface {
            Ok(surface) => triangulate_on_surface(*surface, &loops, chord),
            Err(_) => triangulate_planar_cap(&loops),
        };
        polygons.extend(
            triangles
                .into_iter()
                .map(|triangle| (triangle.to_vec(), role)),
        );
    }
    polygons
}

/// The polyline of one STEP edge from its start vertex to its end vertex.
fn edge_polyline(importer: &Importer<'_>, edge_id: u64, chord: f64) -> Vec<Point3> {
    match importer.edges.get(&edge_id) {
        Some(Ok(edge)) => curve_polyline(edge, chord),
        _ => {
            // The curve could not be read: the chord between the vertices.
            let reader = importer.reader;
            let Ok(entity) = reader.entity(edge_id) else {
                return Vec::new();
            };
            let Some(instance) = entity.instance("EDGE_CURVE") else {
                return Vec::new();
            };
            let ends: Option<Vec<Point3>> = [instance.arg(1), instance.arg(2)]
                .into_iter()
                .map(|value| value.as_ref().and_then(|id| reader.vertex_point(id).ok()))
                .collect();
            ends.unwrap_or_default()
        }
    }
}

fn curve_polyline(edge: &EdgeRead, chord: f64) -> Vec<Point3> {
    let range = edge.range;
    if edge.pole {
        return vec![edge.curve.evaluate(range.start)];
    }
    let parameters: Vec<f64> = match edge.curve {
        Curve3::Line { .. } => vec![range.start, range.end],
        Curve3::Circle { radius, .. } => arc_parameters(range, radius, chord),
        Curve3::Ellipse { major_radius, .. } => arc_parameters(range, major_radius, chord),
        Curve3::Bspline { curve } => curve.samples(range.start, range.end, chord, 256),
        Curve3::Trace { .. } => vec![range.start, range.end],
    };
    parameters
        .into_iter()
        .map(|t| edge.curve.evaluate(t))
        .collect()
}

/// Parameters along an arc so its chords stay within `chord` of it, and
/// never fewer than eight per turn.
fn arc_parameters(range: ParameterRange, radius: f64, chord: f64) -> Vec<f64> {
    let sweep = (range.end - range.start).abs();
    let step = if radius > 0.0 {
        2.0 * (1.0 - (chord / radius).min(0.5)).acos().max(1.0e-3)
    } else {
        std::f64::consts::FRAC_PI_4
    };
    let count = ((sweep / step).ceil() as usize)
        .max((8.0 * sweep / std::f64::consts::TAU).ceil() as usize)
        .max(1);
    (0..=count)
        .map(|index| range.start + (range.end - range.start) * index as f64 / count as f64)
        .collect()
}

/// A point's parameters on a surface, continuous with `previous` across
/// the periodic branch.
fn parameters(surface: Surface, point: Point3, previous: Option<Point2>) -> Point2 {
    let unwrap = |value: f64, reference: Option<f64>| match reference {
        Some(reference) => {
            value + std::f64::consts::TAU * ((reference - value) / std::f64::consts::TAU).round()
        }
        None => value,
    };
    match surface {
        Surface::Plane(plane) => plane.project(point),
        Surface::Cylinder(cylinder) => {
            let relative = point - cylinder.origin;
            let theta = relative
                .dot(cylinder.radial_v)
                .atan2(relative.dot(cylinder.radial_u));
            let u = cylinder.angular_sign * theta;
            Point2::new(
                unwrap(u, previous.map(|p| p.x)),
                relative.dot(cylinder.axis),
            )
        }
        Surface::Cone(cone) => {
            let relative = point - cone.origin;
            let radial = relative - cone.axis * relative.dot(cone.axis);
            let theta = if radial.length() > 1.0e-9 {
                radial.dot(cone.radial_v).atan2(radial.dot(cone.radial_u))
            } else {
                previous.map_or(0.0, |p| cone.angular_sign * p.x)
            };
            let u = cone.angular_sign * theta;
            Point2::new(unwrap(u, previous.map(|p| p.x)), relative.dot(cone.axis))
        }
        Surface::Sphere(sphere) => {
            let relative = point - sphere.origin;
            let radial = relative - sphere.axis * relative.dot(sphere.axis);
            let theta = if radial.length() > 1.0e-9 * sphere.radius.max(1.0) {
                radial
                    .dot(sphere.radial_v)
                    .atan2(radial.dot(sphere.radial_u))
            } else {
                previous.map_or(0.0, |p| sphere.angular_sign * p.x)
            };
            let u = sphere.angular_sign * theta;
            let v = (relative.dot(sphere.axis) / sphere.radius)
                .clamp(-1.0, 1.0)
                .asin();
            Point2::new(unwrap(u, previous.map(|p| p.x)), v)
        }
        Surface::Torus(torus) => {
            let relative = point - torus.origin;
            let radial = relative - torus.axis * relative.dot(torus.axis);
            let theta = radial.dot(torus.radial_v).atan2(radial.dot(torus.radial_u));
            let u = torus.angular_sign * theta;
            let outward = if radial.length() > 1.0e-9 {
                radial / radial.length()
            } else {
                torus.radial_u
            };
            let from_ring = relative - outward * torus.major_radius;
            let v = from_ring.dot(torus.axis).atan2(from_ring.dot(outward));
            Point2::new(
                unwrap(u, previous.map(|p| p.x)),
                unwrap(v, previous.map(|p| p.y)),
            )
        }
        Surface::Bspline(spline) => spline
            .invert(point, previous)
            .or_else(|| spline.invert(point, None))
            .unwrap_or(previous.unwrap_or(Point2::new(0.0, 0.0))),
        Surface::Ruled(ruled) => ruled
            .invert(point, previous)
            .unwrap_or(Point2::new(0.0, 0.0)),
    }
}

/// How much one unit of `u` and of `v` measures in millimetres, so the
/// triangulation sees roughly metric cells: a revolved carrier's azimuth
/// is scaled by its largest radius over the face's height.
fn scales(surface: Surface, v_extent: (f64, f64)) -> (f64, f64) {
    match surface {
        Surface::Cylinder(cylinder) => (cylinder.radius, 1.0),
        Surface::Cone(cone) => {
            let radius = [v_extent.0, v_extent.1]
                .into_iter()
                .map(|v| (cone.base_radius + cone.slope * v).abs())
                .fold(0.0_f64, f64::max);
            (radius.max(1.0e-3), 1.0)
        }
        Surface::Sphere(sphere) => (sphere.radius, sphere.radius),
        Surface::Torus(torus) => (torus.major_radius + torus.minor_radius, torus.minor_radius),
        _ => (1.0, 1.0),
    }
}

/// The angle an arc of `radius` may span before its chord stands more than
/// `chord` off it.
fn angular_step(radius: f64, chord: f64) -> f64 {
    if radius > 0.0 {
        (2.0 * (1.0 - (chord / radius).min(0.5)).acos()).clamp(1.0e-2, std::f64::consts::FRAC_PI_2)
    } else {
        std::f64::consts::FRAC_PI_4
    }
}

/// The size of the cells a face is cut into along each parameter, in the
/// scaled parameters, or `None` where the surface is straight along it. A
/// triangle within one cell has edges no longer than the cell, so its
/// chord stays within the tolerance without any refinement.
fn cell_sizes(
    surface: Surface,
    chord: f64,
    scale: (f64, f64),
    extent: (f64, f64),
) -> (Option<f64>, Option<f64>) {
    let sizes = match surface {
        Surface::Plane(_) => (None, None),
        Surface::Cylinder(_) | Surface::Cone(_) => {
            (Some(scale.0 * angular_step(scale.0, chord)), None)
        }
        Surface::Sphere(sphere) => {
            let cell = sphere.radius * angular_step(sphere.radius, chord);
            (Some(cell), Some(cell))
        }
        Surface::Torus(torus) => (
            Some(scale.0 * angular_step(scale.0, chord)),
            Some(torus.minor_radius * angular_step(torus.minor_radius, chord)),
        ),
        Surface::Bspline(_) | Surface::Ruled(_) => (Some(extent.0 / 12.0), Some(extent.1 / 12.0)),
    };
    // Never more cells than the mesh budget could hold anyway.
    let cap = |size: Option<f64>, extent: f64| {
        size.map(|size| size.max(extent / 200.0))
            .filter(|size| *size > 0.0)
    };
    (cap(sizes.0, extent.0), cap(sizes.1, extent.1))
}

/// The grid lines strictly inside `[low, high]` at spacing `size`.
fn grid_lines(low: f64, high: f64, size: Option<f64>) -> Vec<f64> {
    let Some(size) = size else {
        return Vec::new();
    };
    let count = ((high - low) / size).ceil().max(1.0);
    let step = (high - low) / count;
    (1..count as usize)
        .map(|index| low + step * index as f64)
        .collect()
}

/// A corner of a face's triangulation: where it sits in the scaled
/// parameters, and in space.
#[derive(Clone, Copy, Debug)]
struct Corner {
    x: f64,
    y: f64,
    point: Point3,
}

fn lerp(a: Point3, b: Point3, t: f64) -> Point3 {
    Point3::new(
        a.x + (b.x - a.x) * t,
        a.y + (b.y - a.y) * t,
        a.z + (b.z - a.z) * t,
    )
}

/// Splits every edge of the polygon where it crosses a grid line. Each
/// crossing is computed from the edge's own ends, taken in a fixed order,
/// so the cells either side of the line find the very same corner; its
/// point in space lies on the chord between the ends, which is what the
/// face next door draws along that edge.
fn subdivide_at_lines(polygon: &[Corner], u_lines: &[f64], v_lines: &[f64]) -> Vec<Corner> {
    let mut out = Vec::with_capacity(polygon.len() * 2);
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        out.push(a);
        let forward = (a.x, a.y) <= (b.x, b.y);
        let (lo, hi) = if forward { (a, b) } else { (b, a) };
        let mut splits: Vec<(f64, Corner)> = Vec::new();
        for &u in u_lines {
            if (lo.x < u && u < hi.x) || (hi.x < u && u < lo.x) {
                let t = (u - lo.x) / (hi.x - lo.x);
                let corner = Corner {
                    x: u,
                    y: lo.y + (hi.y - lo.y) * t,
                    point: lerp(lo.point, hi.point, t),
                };
                splits.push((if forward { t } else { 1.0 - t }, corner));
            }
        }
        for &v in v_lines {
            if (lo.y < v && v < hi.y) || (hi.y < v && v < lo.y) {
                let t = (v - lo.y) / (hi.y - lo.y);
                let corner = Corner {
                    x: lo.x + (hi.x - lo.x) * t,
                    y: v,
                    point: lerp(lo.point, hi.point, t),
                };
                splits.push((if forward { t } else { 1.0 - t }, corner));
            }
        }
        splits.sort_by(|p, q| p.0.total_cmp(&q.0));
        out.extend(splits.into_iter().map(|(_, corner)| corner));
    }
    out
}

/// Sutherland–Hodgman against one half-plane of the grid. Every crossing
/// of an edge with a grid line already is a corner, so the only points
/// this makes are where a run along one grid line meets another: a grid
/// corner, evaluated on the surface.
fn clip_half_plane(
    polygon: &[Corner],
    along_u: bool,
    line: f64,
    keep_greater: bool,
    grid_point: &impl Fn(f64, f64) -> Option<Point3>,
) -> Vec<Corner> {
    let coordinate = |corner: &Corner| if along_u { corner.x } else { corner.y };
    let inside = |corner: &Corner| {
        if keep_greater {
            coordinate(corner) >= line
        } else {
            coordinate(corner) <= line
        }
    };
    let crossing = |s: Corner, e: Corner| -> Corner {
        let t = (line - coordinate(&s)) / (coordinate(&e) - coordinate(&s));
        if t <= 0.0 {
            return s;
        }
        if t >= 1.0 {
            return e;
        }
        let (x, y) = if along_u {
            (line, s.y + (e.y - s.y) * t)
        } else {
            (s.x + (e.x - s.x) * t, line)
        };
        let point = grid_point(x, y).unwrap_or_else(|| lerp(s.point, e.point, t));
        Corner { x, y, point }
    };
    let mut out = Vec::with_capacity(polygon.len() + 4);
    for index in 0..polygon.len() {
        let s = polygon[index];
        let e = polygon[(index + 1) % polygon.len()];
        match (inside(&s), inside(&e)) {
            (true, true) => out.push(e),
            (true, false) => out.push(crossing(s, e)),
            (false, true) => {
                out.push(crossing(s, e));
                out.push(e);
            }
            (false, false) => {}
        }
    }
    out
}

/// Drops corners that repeat their neighbour.
fn dedupe(mut piece: Vec<Corner>) -> Vec<Corner> {
    piece.dedup_by(|b, a| (a.x - b.x).abs() <= 1.0e-12 && (a.y - b.y).abs() <= 1.0e-12);
    while piece.len() > 1 {
        let (first, last) = (piece[0], piece[piece.len() - 1]);
        if (first.x - last.x).abs() <= 1.0e-12 && (first.y - last.y).abs() <= 1.0e-12 {
            piece.pop();
        } else {
            break;
        }
    }
    piece
}

/// Triangulates a face on its surface: the loops are laid out in the
/// surface's parameters, holes are bridged into the outer ring, the ring
/// is cut into cells no wider than the chord allows, and each cell's piece
/// is ear-clipped. Nothing is moved afterwards, so no two corners end up
/// closer than the boundary's own spacing.
fn triangulate_on_surface(surface: Surface, loops: &[Vec<Point3>], chord: f64) -> Vec<[Point3; 3]> {
    let mut raw: Vec<Vec<(Point2, Point3)>> = Vec::with_capacity(loops.len());
    for polyline in loops {
        let mut previous = None;
        raw.push(
            polyline
                .iter()
                .map(|&point| {
                    let uv = parameters(surface, point, previous);
                    previous = Some(uv);
                    (uv, point)
                })
                .collect(),
        );
    }
    let v_extent = raw.iter().flatten().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(low, high), (uv, _)| (low.min(uv.y), high.max(uv.y)),
    );
    let (su, sv) = scales(surface, v_extent);
    let mut vertices: Vec<Point3> = Vec::new();
    let rings: Vec<Vec<(f64, f64, usize)>> = raw
        .iter()
        .map(|ring| {
            ring.iter()
                .map(|&(uv, point)| {
                    vertices.push(point);
                    (uv.x * su, uv.y * sv, vertices.len() - 1)
                })
                .collect()
        })
        .collect();
    let merged = merge_rings(rings);
    if merged.len() < 3 {
        return Vec::new();
    }
    let polygon: Vec<Corner> = merged
        .iter()
        .map(|&(x, y, index)| Corner {
            x,
            y,
            point: vertices[index],
        })
        .collect();
    let (low, high) = polygon.iter().fold(
        (
            (f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY),
        ),
        |(low, high), corner| {
            (
                (low.0.min(corner.x), low.1.min(corner.y)),
                (high.0.max(corner.x), high.1.max(corner.y)),
            )
        },
    );
    let extent = (high.0 - low.0, high.1 - low.1);
    let (cell_u, cell_v) = cell_sizes(surface, chord, (su, sv), extent);
    let u_lines = grid_lines(low.0, high.0, cell_u);
    let v_lines = grid_lines(low.1, high.1, cell_v);
    let polygon = subdivide_at_lines(&polygon, &u_lines, &v_lines);
    let grid_point = |x: f64, y: f64| -> Option<Point3> {
        (u_lines.contains(&x) && v_lines.contains(&y))
            .then(|| surface.evaluate(Point2::new(x / su, y / sv)))
    };
    let mut triangles = Vec::new();
    for strip in 0..=u_lines.len() {
        let mut piece = polygon.clone();
        if strip > 0 {
            piece = clip_half_plane(&piece, true, u_lines[strip - 1], true, &grid_point);
        }
        if strip < u_lines.len() {
            piece = clip_half_plane(&piece, true, u_lines[strip], false, &grid_point);
        }
        if piece.len() < 3 {
            continue;
        }
        for cell in 0..=v_lines.len() {
            let mut cell_piece = piece.clone();
            if cell > 0 {
                cell_piece =
                    clip_half_plane(&cell_piece, false, v_lines[cell - 1], true, &grid_point);
            }
            if cell < v_lines.len() {
                cell_piece = clip_half_plane(&cell_piece, false, v_lines[cell], false, &grid_point);
            }
            let cell_piece = dedupe(cell_piece);
            if cell_piece.len() < 3 {
                continue;
            }
            let flat: Vec<(f64, f64)> = cell_piece
                .iter()
                .map(|corner| (corner.x, corner.y))
                .collect();
            for [a, b, c] in ear_clip(&flat) {
                let (a, b, c) = (cell_piece[a], cell_piece[b], cell_piece[c]);
                let centroid = Point2::new(
                    (a.x + b.x + c.x) / (3.0 * su),
                    (a.y + b.y + c.y) / (3.0 * sv),
                );
                let Some(wanted) = surface.outward_normal_at(surface.evaluate(centroid)) else {
                    continue;
                };
                let emitted = (b.point - a.point).cross(c.point - a.point);
                if emitted.length() <= 1.0e-18 {
                    continue;
                }
                triangles.push(if emitted.dot(wanted) >= 0.0 {
                    [a.point, b.point, c.point]
                } else {
                    [a.point, c.point, b.point]
                });
            }
        }
    }
    triangles
}

/// A face whose surface could not be read: its loops in their own best
/// plane, wound as the loop runs (counter-clockwise about the face normal).
fn triangulate_planar_cap(loops: &[Vec<Point3>]) -> Vec<[Point3; 3]> {
    let outer = &loops[0];
    let normal = newell(outer);
    let Some(normal) = (normal.length() > 1.0e-18).then(|| normal / normal.length()) else {
        return Vec::new();
    };
    let seed = if normal.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    let u = seed - normal * seed.dot(normal);
    let u = u / u.length();
    let v = normal.cross(u);
    let origin = outer[0];
    let mut vertices: Vec<Point3> = Vec::new();
    let rings: Vec<Vec<(f64, f64, usize)>> = loops
        .iter()
        .map(|polyline| {
            polyline
                .iter()
                .map(|&point| {
                    let relative = point - origin;
                    vertices.push(point);
                    (relative.dot(u), relative.dot(v), vertices.len() - 1)
                })
                .collect()
        })
        .collect();
    clip_rings(rings)
        .into_iter()
        .filter_map(|[a, b, c]| {
            let (pa, pb, pc) = (vertices[a], vertices[b], vertices[c]);
            let emitted = (pb - pa).cross(pc - pa);
            if emitted.length() <= 1.0e-18 {
                return None;
            }
            Some(if emitted.dot(normal) >= 0.0 {
                [pa, pb, pc]
            } else {
                [pa, pc, pb]
            })
        })
        .collect()
}

fn newell(points: &[Point3]) -> Vector3 {
    let mut normal = Vector3::new(0.0, 0.0, 0.0);
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        normal = normal
            + Vector3::new(
                (a.y - b.y) * (a.z + b.z),
                (a.z - b.z) * (a.x + b.x),
                (a.x - b.x) * (a.y + b.y),
            );
    }
    normal
}

/// Winds the outer ring counter-clockwise and the holes clockwise, and
/// bridges the holes into the outer ring: one weakly simple polygon.
fn merge_rings(mut rings: Vec<Vec<(f64, f64, usize)>>) -> Vec<(f64, f64, usize)> {
    if rings.is_empty() {
        return Vec::new();
    }
    // The outer ring is the one of largest area, whatever the file said.
    let areas: Vec<f64> = rings
        .iter()
        .map(|ring| signed_area(&ring.iter().map(|&(u, v, _)| (u, v)).collect::<Vec<_>>()))
        .collect();
    let outer_index = areas
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map_or(0, |(index, _)| index);
    rings.swap(0, outer_index);
    let mut outer = rings.remove(0);
    if signed_area(&outer.iter().map(|&(u, v, _)| (u, v)).collect::<Vec<_>>()) < 0.0 {
        outer.reverse();
    }
    let holes: Vec<Vec<(f64, f64, usize)>> = rings
        .into_iter()
        .map(|mut ring| {
            if signed_area(&ring.iter().map(|&(u, v, _)| (u, v)).collect::<Vec<_>>()) > 0.0 {
                ring.reverse();
            }
            ring
        })
        .collect();
    bridge_holes(outer, holes)
}

/// Bridges the rings into one polygon and ear-clips it to index triples.
fn clip_rings(rings: Vec<Vec<(f64, f64, usize)>>) -> Vec<[usize; 3]> {
    let merged = merge_rings(rings);
    let polygon: Vec<(f64, f64)> = merged.iter().map(|&(u, v, _)| (u, v)).collect();
    ear_clip(&polygon)
        .into_iter()
        .map(|[a, b, c]| [merged[a].2, merged[b].2, merged[c].2])
        .collect()
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

/// Ear clipping over a polygon whose holes have been bridged in: the
/// largest ear whose closed triangle holds no other corner is clipped
/// each round, so a run of collinear corners along one side is never
/// left behind as a zero-area remainder, and every boundary corner ends
/// up on a triangle edge that the face next door also draws.
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
        (p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) < 1.0e-18
    };
    while remaining.len() > 3 {
        let m = remaining.len();
        let mut best: Option<(f64, usize)> = None;
        let mut convex: Option<usize> = None;
        for slot in 0..m {
            let (i0, i1, i2) = (
                remaining[(slot + m - 1) % m],
                remaining[slot],
                remaining[(slot + 1) % m],
            );
            let (a, b, c) = (polygon[i0], polygon[i1], polygon[i2]);
            let area = cross(a, b, c);
            if area <= 1.0e-12 {
                continue;
            }
            convex.get_or_insert(slot);
            if best.is_some_and(|(known, _)| area <= known) {
                continue;
            }
            let blocked = remaining.iter().any(|&other| {
                if other == i0 || other == i1 || other == i2 {
                    return false;
                }
                let p = polygon[other];
                if coincident(p, a) || coincident(p, b) || coincident(p, c) {
                    return false;
                }
                cross(a, b, p) >= -1.0e-12
                    && cross(b, c, p) >= -1.0e-12
                    && cross(c, a, p) >= -1.0e-12
            });
            if !blocked {
                best = Some((area, slot));
            }
        }
        // A polygon with no clean ear is not simple; any convex corner
        // keeps the clip moving rather than leaving the face unmeshed.
        let Some(slot) = best.map(|(_, slot)| slot).or(convex) else {
            break;
        };
        triangles.push([
            remaining[(slot + m - 1) % m],
            remaining[slot],
            remaining[(slot + 1) % m],
        ]);
        remaining.remove(slot);
    }
    if remaining.len() == 3 {
        triangles.push([remaining[0], remaining[1], remaining[2]]);
    }
    triangles
}

/// Bridges holes into the outer ring so one ear clip covers the face:
/// each hole, rightmost first, is joined from its rightmost corner to the
/// nearest corner of the ring so far that the bridge reaches without
/// crossing any ring, its own or a hole still to come.
fn bridge_holes(
    outer: Vec<(f64, f64, usize)>,
    holes: Vec<Vec<(f64, f64, usize)>>,
) -> Vec<(f64, f64, usize)> {
    let mut merged = outer;
    let mut pending = holes;
    pending.sort_by(|a, b| {
        let ax = a.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
        let bx = b.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
        bx.total_cmp(&ax)
    });
    while !pending.is_empty() {
        let hole = pending.remove(0);
        let mouth = (0..hole.len())
            .max_by(|&a, &b| hole[a].0.total_cmp(&hole[b].0))
            .unwrap_or(0);
        let m = (hole[mouth].0, hole[mouth].1);
        let crosses =
            |ring: &[(f64, f64, usize)], c: (f64, f64), incident: Option<usize>| -> bool {
                (0..ring.len()).any(|index| {
                    let next = (index + 1) % ring.len();
                    if incident.is_some_and(|skip| index == skip || next == skip) {
                        return false;
                    }
                    segments_cross(
                        m,
                        c,
                        (ring[index].0, ring[index].1),
                        (ring[next].0, ring[next].1),
                    )
                })
            };
        let mut best: Option<(f64, usize)> = None;
        let mut nearest: Option<(f64, usize)> = None;
        for (slot, candidate) in merged.iter().enumerate() {
            let c = (candidate.0, candidate.1);
            let cost = (c.0 - m.0).powi(2) + (c.1 - m.1).powi(2);
            if nearest.is_none_or(|(known, _)| cost < known) {
                nearest = Some((cost, slot));
            }
            if best.is_some_and(|(known, _)| cost >= known) {
                continue;
            }
            if crosses(&merged, c, Some(slot))
                || crosses(&hole, c, Some(mouth))
                || pending.iter().any(|other| crosses(other, c, None))
            {
                continue;
            }
            best = Some((cost, slot));
        }
        // A hole no bridge reaches cleanly is still a hole: the nearest
        // corner keeps it out of the face rather than filling it in.
        let Some((_, anchor)) = best.or(nearest) else {
            continue;
        };
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
    (o1 * o2 < -1.0e-18) && (o3 * o4 < -1.0e-18)
}

//! Display tessellation of a face on a surface of revolution whose region in
//! parameter space is more than a rectangle of rings and meridians: a face
//! with a hole in it, or one bounded by a numerically traced curve (ADR 0056
//! Track B).
//!
//! The face's loops are walked into polygons in the surface's `(u, v)` plane
//! at the display budget's density, triangulated there the way a planar face
//! with holes is, refined by bisecting any chord longer than one display
//! step in either parameter, and carried onto the surface. The rectangle
//! every tessellator drew before keeps its grid: this route answers only
//! where the grid would draw material the face does not have. Display only;
//! nothing here reaches a measure or a snapshot.

use artificer_protocol::PrecisionPolicy;

use super::{ChordBudget, TessellationFallback, arc_subdivisions, triangulate_face_boundaries};
use crate::revolved::Revolved;
use crate::topology::{Curve2, Face, Plane, Point2, Point3, Surface, Topology, Vector3};

/// The most triangles the refinement emits for one face.
const MOST_TRIANGLES: usize = 250_000;

/// The face's display triangles, or `None` where its region is the
/// rectangle of its parameter box and the grid tessellators' own drawing
/// stands: no inner loop, every pcurve a line along a ring or a meridian (a
/// cylinder also keeps its harmonic strips), and the outer polygon filling
/// its box.
pub(crate) fn tessellate(
    topology: &Topology,
    face: &Face,
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> Option<Vec<[Point3; 3]>> {
    let revolved = Revolved::of(face.surface)?;
    let cylinder = matches!(face.surface, Surface::Cylinder(_));
    let extent = parameter_extent(topology, face)?;
    let (step_u, step_v) = steps(revolved, extent, budget, precision);
    let mut polygons = Vec::with_capacity(1 + face.inner_loops.len());
    let mut beyond_rectangle = !face.inner_loops.is_empty();
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        let mut polygon: Vec<Point2> = Vec::new();
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            let [start, end] = coedge.pcurve_endpoints();
            match coedge.pcurve {
                Curve2::Line { .. } => {
                    let scale = start.x.abs().max(start.y.abs()).max(1.0);
                    if !cylinder
                        && (start.x - end.x).abs() > 1.0e-12 * scale
                        && (start.y - end.y).abs() > 1.0e-12 * scale
                    {
                        beyond_rectangle = true;
                    }
                    push_distinct(&mut polygon, start);
                }
                Curve2::Harmonic { .. } if cylinder => {
                    for point in
                        curve_samples(coedge.pcurve, coedge.parameter_range, step_u, step_v)
                    {
                        push_distinct(&mut polygon, point);
                    }
                }
                Curve2::Bspline { .. } => {
                    beyond_rectangle = true;
                    for point in
                        curve_samples(coedge.pcurve, coedge.parameter_range, step_u, step_v)
                    {
                        push_distinct(&mut polygon, point);
                    }
                }
                _ => {
                    beyond_rectangle |= !cylinder;
                    for point in
                        curve_samples(coedge.pcurve, coedge.parameter_range, step_u, step_v)
                    {
                        push_distinct(&mut polygon, point);
                    }
                }
            }
        }
        if polygon.len() >= 2 && same_point(polygon[0], polygon[polygon.len() - 1]) {
            polygon.pop();
        }
        polygons.push(polygon);
    }
    let outer_area = signed_area(&polygons[0]);
    if !cylinder && !beyond_rectangle {
        let (u_min, u_max, v_min, v_max) = extent;
        let box_area = (u_max - u_min) * (v_max - v_min);
        if outer_area.abs() >= 0.999 * box_area {
            return None;
        }
    } else if cylinder && !beyond_rectangle {
        return None;
    }
    if polygons[0].len() < 3 || outer_area == 0.0 {
        return None;
    }
    // The triangulator walks a counter-clockwise outer boundary and
    // clockwise holes; a face whose loops wind the other way in parameter
    // space is turned over, holes with it.
    if outer_area < 0.0 {
        for polygon in &mut polygons {
            polygon.reverse();
        }
    }
    // Triangulated in display steps, so the clipper's tolerances and the
    // refinement below see both parameters alike.
    let scaled = polygons
        .iter()
        .map(|polygon| {
            polygon
                .iter()
                .map(|point| Point3::new(point.x / step_u, point.y / step_v, 0.0))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let sheet = Plane::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(1.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
    );
    let coarse = triangulate_face_boundaries(&scaled, sheet, TessellationFallback::Display);
    let refined = refine(
        coarse
            .into_iter()
            .map(|triangle| triangle.map(|point| Point2::new(point.x, point.y)))
            .collect(),
    );
    Some(
        refined
            .into_iter()
            .map(|triangle| {
                triangle.map(|point| {
                    face.surface
                        .evaluate(Point2::new(point.x * step_u, point.y * step_v))
                })
            })
            .collect(),
    )
}

/// The parameter box of every loop of the face.
fn parameter_extent(topology: &Topology, face: &Face) -> Option<(f64, f64, f64, f64)> {
    let mut u = (f64::INFINITY, f64::NEG_INFINITY);
    let mut v = (f64::INFINITY, f64::NEG_INFINITY);
    for loop_key in face.loops() {
        let loop_record = topology.loop_record(loop_key)?;
        for coedge_key in &loop_record.value.coedges {
            let coedge = topology.coedge(*coedge_key)?.value;
            let points = match coedge.pcurve {
                Curve2::Line { .. } => coedge.pcurve_endpoints().to_vec(),
                _ => (0..=32)
                    .map(|step| {
                        let range = coedge.parameter_range;
                        coedge.pcurve.evaluate(
                            (range.end - range.start).mul_add(f64::from(step) / 32.0, range.start),
                        )
                    })
                    .collect(),
            };
            for point in points {
                u = (u.0.min(point.x), u.1.max(point.x));
                v = (v.0.min(point.y), v.1.max(point.y));
            }
        }
    }
    (u.0 < u.1
        && v.0 < v.1
        && u.0.is_finite()
        && u.1.is_finite()
        && v.0.is_finite()
        && v.1.is_finite())
    .then_some((u.0, u.1, v.0, v.1))
}

/// One display step in each parameter: the azimuth cut as the grid
/// tessellators cut it at the largest ring, the other parameter by the arc
/// it turns through where it is an angle and to match the azimuth step's
/// length where it is a length.
fn steps(
    revolved: Revolved,
    (u_min, u_max, v_min, v_max): (f64, f64, f64, f64),
    budget: ChordBudget,
    precision: PrecisionPolicy,
) -> (f64, f64) {
    let largest_ring = match revolved {
        Revolved::Cylinder(cylinder) => cylinder.radius.abs(),
        Revolved::Cone(cone) => cone
            .ring_radius(v_min)
            .abs()
            .max(cone.ring_radius(v_max).abs()),
        Revolved::Sphere(sphere) => sphere.radius.abs(),
        Revolved::Torus(torus) => torus.major_radius.abs() + torus.minor_radius.abs(),
    };
    let azimuthal = arc_subdivisions(largest_ring, u_max - u_min, budget, precision).max(1);
    let step_u = (u_max - u_min) / azimuthal as f64;
    let meridional = match revolved {
        Revolved::Cylinder(_) | Revolved::Cone(_) => {
            let along = step_u * largest_ring.max(precision.min_feature_size.max(1.0e-9));
            ((v_max - v_min) / along).ceil().clamp(1.0, 4096.0) as usize
        }
        Revolved::Sphere(sphere) => {
            arc_subdivisions(sphere.radius, v_max - v_min, budget, precision).max(1)
        }
        Revolved::Torus(torus) => {
            arc_subdivisions(torus.minor_radius, v_max - v_min, budget, precision).max(1)
        }
    };
    (step_u, (v_max - v_min) / meridional as f64)
}

/// A curved pcurve walked from its start to just before its end, in steps
/// short enough that no chord spans more than one display step in either
/// parameter.
fn curve_samples(
    pcurve: Curve2,
    range: crate::topology::ParameterRange,
    step_u: f64,
    step_v: f64,
) -> Vec<Point2> {
    const PROBE: usize = 32;
    let at =
        |fraction: f64| pcurve.evaluate((range.end - range.start).mul_add(fraction, range.start));
    let mut length = 0.0;
    let mut previous = at(0.0);
    for step in 1..=PROBE {
        let current = at(step as f64 / PROBE as f64);
        length +=
            ((current.x - previous.x) / step_u).abs() + ((current.y - previous.y) / step_v).abs();
        previous = current;
    }
    let count = (length.ceil() as usize).clamp(2, 4096);
    (0..count)
        .map(|step| at(step as f64 / count as f64))
        .collect()
}

fn push_distinct(polygon: &mut Vec<Point2>, point: Point2) {
    if polygon.last().is_none_or(|last| !same_point(*last, point)) {
        polygon.push(point);
    }
}

fn same_point(first: Point2, second: Point2) -> bool {
    let scale = first.x.abs().max(first.y.abs()).max(1.0);
    (first.x - second.x).abs() <= 1.0e-12 * scale && (first.y - second.y).abs() <= 1.0e-12 * scale
}

fn signed_area(polygon: &[Point2]) -> f64 {
    let mut twice = 0.0;
    for (index, point) in polygon.iter().enumerate() {
        let next = polygon[(index + 1) % polygon.len()];
        twice += point.x * next.y - next.x * point.y;
    }
    0.5 * twice
}

/// Bisects every triangle across its longest side until no side is longer
/// than one display step, keeping each triangle's winding.
fn refine(coarse: Vec<[Point2; 3]>) -> Vec<[Point2; 3]> {
    let mut pending = coarse;
    let mut done = Vec::new();
    while let Some(triangle) = pending.pop() {
        let sides = [
            (triangle[1].x - triangle[0].x).hypot(triangle[1].y - triangle[0].y),
            (triangle[2].x - triangle[1].x).hypot(triangle[2].y - triangle[1].y),
            (triangle[0].x - triangle[2].x).hypot(triangle[0].y - triangle[2].y),
        ];
        let (longest, length) =
            sides
                .iter()
                .enumerate()
                .fold((0, 0.0_f64), |best, (index, side)| {
                    if *side > best.1 { (index, *side) } else { best }
                });
        if length <= 1.0 || done.len() + pending.len() >= MOST_TRIANGLES {
            done.push(triangle);
            continue;
        }
        let (a, b, c) = (
            triangle[longest],
            triangle[(longest + 1) % 3],
            triangle[(longest + 2) % 3],
        );
        let middle = Point2::new(0.5 * (a.x + b.x), 0.5 * (a.y + b.y));
        pending.push([a, middle, c]);
        pending.push([middle, b, c]);
    }
    done
}

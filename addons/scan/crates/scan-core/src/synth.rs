//! Synthetic scan geometry for tests and demos: analytic patches sampled the
//! way a structured-light scanner would see them, so the fitting and
//! segmentation stages can be validated against exact ground truth.

use artificer_geometry::{Point3, Vector3};

use crate::mesh::TriangleMesh;
use crate::transform::{normalize, orthonormal_basis};

pub fn plane_patch_soup(
    origin: Point3,
    u: Vector3,
    v: Vector3,
    width: f64,
    height: f64,
    nu: usize,
    nv: usize,
) -> Vec<[Point3; 3]> {
    let du = u * (width / nu as f64);
    let dv = v * (height / nv as f64);
    let mut soup = Vec::with_capacity(nu * nv * 2);
    for i in 0..nu {
        for j in 0..nv {
            let a = origin + du * i as f64 + dv * j as f64;
            let b = a + du;
            let c = b + dv;
            let d = a + dv;
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    soup
}

/// Open cylindrical shell: axis +Z, base at z = 0, outward winding.
pub fn open_cylinder_soup(
    radius: f64,
    height: f64,
    segments: usize,
    rings: usize,
) -> Vec<[Point3; 3]> {
    let mut soup = Vec::with_capacity(segments * rings * 2);
    let point = |s: usize, r: usize| {
        let angle = std::f64::consts::TAU * s as f64 / segments as f64;
        Point3::new(
            radius * angle.cos(),
            radius * angle.sin(),
            height * r as f64 / rings as f64,
        )
    };
    for s in 0..segments {
        for r in 0..rings {
            let a = point(s, r);
            let b = point(s + 1, r);
            let c = point(s + 1, r + 1);
            let d = point(s, r + 1);
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    soup
}

pub fn open_cylinder(radius: f64, height: f64, segments: usize, rings: usize) -> TriangleMesh {
    TriangleMesh::from_triangle_soup(&open_cylinder_soup(radius, height, segments, rings), 1e-9)
        .expect("cylinder soup is valid")
}

/// Triangle fan disk in the plane through `center` with the given normal.
pub fn disk_soup(
    center: Point3,
    normal: Vector3,
    radius: f64,
    segments: usize,
) -> Vec<[Point3; 3]> {
    let unit = normalize(normal).expect("disk normal must be nonzero");
    let (e1, e2) = orthonormal_basis(unit);
    let rim = |s: usize| {
        let angle = std::f64::consts::TAU * s as f64 / segments as f64;
        center + e1 * (radius * angle.cos()) + e2 * (radius * angle.sin())
    };
    (0..segments)
        .map(|s| [center, rim(s), rim(s + 1)])
        .collect()
}

/// Axis-aligned box with each face subdivided so segmentation sees
/// scanner-like face counts. Winding is outward on every face.
pub fn box_soup(min: Point3, size: Vector3, subdivisions: usize) -> Vec<[Point3; 3]> {
    let n = subdivisions.max(1);
    let mut soup = Vec::new();
    let x = Vector3::new(size.x, 0.0, 0.0);
    let y = Vector3::new(0.0, size.y, 0.0);
    let z = Vector3::new(0.0, 0.0, size.z);
    let max = min + size;
    // (origin, u edge, v edge) per face, chosen so u x v points outward.
    let faces = [
        (min, y, x),                              // bottom (-Z)
        (Point3::new(min.x, min.y, max.z), x, y), // top (+Z)
        (min, x, z),                              // front (-Y)
        (Point3::new(min.x, max.y, min.z), z, x), // back (+Y)
        (min, z, y),                              // left (-X)
        (Point3::new(max.x, min.y, min.z), y, z), // right (+X)
    ];
    for (origin, u, v) in faces {
        let un = normalize(u).expect("box edge");
        let vn = normalize(v).expect("box edge");
        soup.extend(plane_patch_soup(
            origin,
            un,
            vn,
            u.length(),
            v.length(),
            n,
            n,
        ));
    }
    soup
}

/// The acceptance part: a rectangular plate with a cylindrical boss, the
/// canonical scan-to-CAD smoke test (planes + cylinder + cap).
pub fn plate_with_boss() -> TriangleMesh {
    let mut soup = box_soup(
        Point3::new(-40.0, -30.0, 0.0),
        Vector3::new(80.0, 60.0, 10.0),
        6,
    );
    let boss: Vec<[Point3; 3]> = open_cylinder_soup(12.0, 20.0, 96, 10)
        .into_iter()
        .map(|t| t.map(|p| Point3::new(p.x, p.y, p.z + 10.0)))
        .collect();
    soup.extend(boss);
    soup.extend(disk_soup(
        Point3::new(0.0, 0.0, 30.0),
        Vector3::new(0.0, 0.0, 1.0),
        12.0,
        96,
    ));
    TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("plate soup is valid")
}

/// Partial cylindrical shell covering `arc_start..arc_end` radians:
/// axis +Z, base at z = 0, outward winding.
pub fn cylinder_arc_soup(
    radius: f64,
    height: f64,
    arc_start: f64,
    arc_end: f64,
    segments: usize,
    rings: usize,
) -> Vec<[Point3; 3]> {
    let mut soup = Vec::with_capacity(segments * rings * 2);
    let point = |s: usize, r: usize| {
        let angle = arc_start + (arc_end - arc_start) * s as f64 / segments as f64;
        Point3::new(
            radius * angle.cos(),
            radius * angle.sin(),
            height * r as f64 / rings as f64,
        )
    };
    for s in 0..segments {
        for r in 0..rings {
            let a = point(s, r);
            let b = point(s + 1, r);
            let c = point(s + 1, r + 1);
            let d = point(s, r + 1);
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    soup
}

/// Fillet ring: the arc `t0..t1` of a circle with `minor` radius centred
/// at `(major, z_center)` in profile space, revolved fully about +Z.
/// Profile angle 0 points away from the axis, `PI/2` points up.
pub fn revolved_blend_soup(
    major: f64,
    minor: f64,
    z_center: f64,
    t0: f64,
    t1: f64,
    revolve_segments: usize,
    profile_steps: usize,
) -> Vec<[Point3; 3]> {
    let mut soup = Vec::with_capacity(revolve_segments * profile_steps * 2);
    let point = |s: usize, p: usize| {
        let angle = std::f64::consts::TAU * s as f64 / revolve_segments as f64;
        let t = t0 + (t1 - t0) * p as f64 / profile_steps as f64;
        let radial = major + minor * t.cos();
        Point3::new(
            radial * angle.cos(),
            radial * angle.sin(),
            z_center + minor * t.sin(),
        )
    };
    for s in 0..revolve_segments {
        for p in 0..profile_steps {
            let a = point(s, p);
            let b = point(s + 1, p);
            let c = point(s + 1, p + 1);
            let d = point(s, p + 1);
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    soup
}

/// Point/normal samples over a full sphere (poles excluded).
pub fn sphere_patch_samples(
    center: Point3,
    radius: f64,
    slices: usize,
    stacks: usize,
) -> (Vec<Point3>, Vec<(Vector3, f64)>) {
    let mut points = Vec::new();
    let mut normals = Vec::new();
    for i in 1..stacks {
        let phi = std::f64::consts::PI * i as f64 / stacks as f64;
        for j in 0..slices {
            let theta = std::f64::consts::TAU * j as f64 / slices as f64;
            let normal = Vector3::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos());
            points.push(center + normal * radius);
            normals.push((normal, 1.0));
        }
    }
    (points, normals)
}

/// Point/normal samples over a partial cylindrical shell.
pub fn cylinder_patch_samples(
    axis_point: Point3,
    axis: Vector3,
    radius: f64,
    height: f64,
    arc: f64,
    arc_steps: usize,
    height_steps: usize,
) -> (Vec<Point3>, Vec<(Vector3, f64)>) {
    let unit = normalize(axis).expect("cylinder axis must be nonzero");
    let (e1, e2) = orthonormal_basis(unit);
    let mut points = Vec::new();
    let mut normals = Vec::new();
    for i in 0..=arc_steps {
        let angle = -arc / 2.0 + arc * i as f64 / arc_steps as f64;
        let radial = e1 * angle.cos() + e2 * angle.sin();
        for j in 0..=height_steps {
            let h = height * j as f64 / height_steps as f64;
            points.push(axis_point + radial * radius + unit * h);
            normals.push((radial, 1.0));
        }
    }
    (points, normals)
}

/// Point/normal samples over a cone frustum between heights `h0` and `h1`
/// measured from the apex along the axis (which points into the material).
pub fn cone_patch_samples(
    apex: Point3,
    axis: Vector3,
    half_angle: f64,
    h0: f64,
    h1: f64,
    arc_steps: usize,
    height_steps: usize,
) -> (Vec<Point3>, Vec<(Vector3, f64)>) {
    let unit = normalize(axis).expect("cone axis must be nonzero");
    let (e1, e2) = orthonormal_basis(unit);
    let (sin_a, cos_a) = half_angle.sin_cos();
    let mut points = Vec::new();
    let mut normals = Vec::new();
    for i in 0..arc_steps {
        let angle = std::f64::consts::TAU * i as f64 / arc_steps as f64;
        let radial = e1 * angle.cos() + e2 * angle.sin();
        for j in 0..=height_steps {
            let h = h0 + (h1 - h0) * j as f64 / height_steps as f64;
            points.push(apex + unit * h + radial * (h * half_angle.tan()));
            normals.push((radial * cos_a - unit * sin_a, 1.0));
        }
    }
    (points, normals)
}

/// Half extents (mm) of the freeform block's footprint in x and y.
pub const FREEFORM_HALF: (f64, f64) = (45.0, 35.0);

/// Height of the freeform block's top at `(x, y)`: an off-centre bump
/// blended into a saddle, with a skewed ripple across both.
///
/// Nothing in the analytic vocabulary describes it to a scanner's
/// tolerance — the saddle defeats spheres, the bump defeats cylinders
/// and cones, and the ripple defeats a torus — and that is the point:
/// it is ground truth for the path that fits what the vocabulary cannot.
pub fn freeform_top_height(x: f64, y: f64) -> f64 {
    let bump = 5.0 * (-((x - 12.0).powi(2) + (y + 6.0).powi(2)) / (2.0 * 13.0 * 13.0)).exp();
    let saddle = 3.0 * ((x / FREEFORM_HALF.0).powi(2) - (y / FREEFORM_HALF.1).powi(2));
    let ripple = 0.8 * (0.09 * x + 0.05 * y).sin();
    14.0 + bump + saddle + ripple
}

/// The gradient `(dz/dx, dz/dy)` of [`freeform_top_height`].
pub fn freeform_top_gradient(x: f64, y: f64) -> (f64, f64) {
    let s2 = 2.0 * 13.0 * 13.0;
    let bump = 5.0 * (-((x - 12.0).powi(2) + (y + 6.0).powi(2)) / s2).exp();
    let phase = (0.09 * x + 0.05 * y).cos();
    (
        bump * (-2.0 * (x - 12.0) / s2)
            + 6.0 * x / (FREEFORM_HALF.0 * FREEFORM_HALF.0)
            + 0.8 * 0.09 * phase,
        bump * (-2.0 * (y + 6.0) / s2) - 6.0 * y / (FREEFORM_HALF.1 * FREEFORM_HALF.1)
            + 0.8 * 0.05 * phase,
    )
}

/// A closed block whose top is [`freeform_top_height`]: flat bottom at
/// z = 0, four planar walls, and the freeform top, tessellated at 1 mm
/// so the facets sit within a few microns of the true surface. Outward
/// winding throughout, welded so the simulator sees one closed skin.
///
/// The walls and floor give the datum stage its planes; the top is the
/// region no analytic surface can claim.
pub fn freeform_block() -> TriangleMesh {
    let (hx, hy) = FREEFORM_HALF;
    let (nx, ny) = ((2.0 * hx) as usize, (2.0 * hy) as usize);
    let x_at = |i: usize| -hx + 2.0 * hx * i as f64 / nx as f64;
    let y_at = |j: usize| -hy + 2.0 * hy * j as f64 / ny as f64;
    let top = |i: usize, j: usize| {
        let (x, y) = (x_at(i), y_at(j));
        Point3::new(x, y, freeform_top_height(x, y))
    };
    let floor = |i: usize, j: usize| Point3::new(x_at(i), y_at(j), 0.0);
    let mut soup = Vec::with_capacity(4 * nx * ny + 64 * (nx + ny));
    for i in 0..nx {
        for j in 0..ny {
            let (a, b, c, d) = (top(i, j), top(i + 1, j), top(i + 1, j + 1), top(i, j + 1));
            soup.push([a, b, c]);
            soup.push([a, c, d]);
            let (a, b, c, d) = (
                floor(i, j),
                floor(i + 1, j),
                floor(i + 1, j + 1),
                floor(i, j + 1),
            );
            soup.push([a, c, b]);
            soup.push([a, d, c]);
        }
    }
    // The rim, walked anticlockwise from above so each wall's outward
    // normal is the travel direction crossed with up.
    let mut rim: Vec<(usize, usize)> = Vec::new();
    rim.extend((0..nx).map(|i| (i, 0)));
    rim.extend((0..ny).map(|j| (nx, j)));
    rim.extend((0..nx).map(|i| (nx - i, ny)));
    rim.extend((0..ny).map(|j| (0, ny - j)));
    const WALL_STEPS: usize = 7;
    for index in 0..rim.len() {
        let (i0, j0) = rim[index];
        let (i1, j1) = rim[(index + 1) % rim.len()];
        let (p0, p1) = (top(i0, j0), top(i1, j1));
        // Fractions of the local height, so every wall row is planar and
        // the top row (t = 1 exactly) lands on the top's own rim vertices.
        let at = |p: Point3, t: f64| Point3::new(p.x, p.y, t * p.z);
        for k in 0..WALL_STEPS {
            let (t0, t1) = (
                k as f64 / WALL_STEPS as f64,
                (k + 1) as f64 / WALL_STEPS as f64,
            );
            let (a, b, c, d) = (at(p0, t0), at(p1, t0), at(p1, t1), at(p0, t1));
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("freeform block soup is valid")
}

/// Ground truth for a synthetic part: the signed distance from a point
/// to the part's true freeform surface, where the point lies over that
/// surface's interior (clear of the scanner-rounded rim) and near it.
/// `None` elsewhere — a truth that answers everywhere would score the
/// walls against the top.
pub type GroundTruth = fn(Point3) -> Option<f64>;

fn freeform_block_truth(point: Point3) -> Option<f64> {
    /// Keep this far inside the rim (mm): the scanner's spot rounds
    /// the crease, and a rounded crease is not the top.
    const MARGIN: f64 = 2.0;
    let (hx, hy) = FREEFORM_HALF;
    if point.x.abs() > hx - MARGIN || point.y.abs() > hy - MARGIN {
        return None;
    }
    let (gx, gy) = freeform_top_gradient(point.x, point.y);
    let vertical = point.z - freeform_top_height(point.x, point.y);
    // Normal distance to first order: the vertical offset times the
    // cosine of the surface's tilt.
    (vertical.abs() < 2.0).then(|| vertical / (1.0 + gx * gx + gy * gy).sqrt())
}

/// A synthetic part by name, for fixtures that are a command line
/// rather than a file: `plate-with-boss`, `freeform-block`.
pub fn named_part(name: &str) -> Option<TriangleMesh> {
    match name {
        "plate-with-boss" => Some(plate_with_boss()),
        "freeform-block" => Some(freeform_block()),
        _ => None,
    }
}

/// The ground truth a named synthetic part carries, if any.
pub fn ground_truth(name: &str) -> Option<GroundTruth> {
    match name {
        "freeform-block" => Some(freeform_block_truth),
        _ => None,
    }
}

/// Revolves an open profile polyline `(radial distance, z)` fully about
/// +Z, outward winding.
pub fn revolved_profile_soup(profile: &[(f64, f64)], segments: usize) -> Vec<[Point3; 3]> {
    let mut soup = Vec::new();
    let point = |s: usize, p: (f64, f64)| {
        let angle = std::f64::consts::TAU * s as f64 / segments as f64;
        Point3::new(p.0 * angle.cos(), p.0 * angle.sin(), p.1)
    };
    for pair in profile.windows(2) {
        for s in 0..segments {
            let a = point(s, pair[0]);
            let b = point(s + 1, pair[0]);
            let c = point(s + 1, pair[1]);
            let d = point(s, pair[1]);
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    soup
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_freeform_block_is_one_closed_consistently_wound_skin() {
        let mesh = freeform_block();
        let health = crate::hygiene::inspect(&mesh);
        assert!(health.is_clean(), "{}", health.describe());
        assert_eq!(health.boundary_edges, 0);
        // Outward winding: the enclosed volume comes out positive.
        let volume: f64 = (0..mesh.triangles().len())
            .map(|face| {
                let [a, b, c] = mesh.triangle_points(face).map(|p| p - Point3::default());
                a.dot(b.cross(c)) / 6.0
            })
            .sum();
        assert!(volume > 0.0, "volume {volume}");
    }

    #[test]
    fn the_truth_answers_over_the_top_and_nowhere_else() {
        let truth = ground_truth("freeform-block").expect("the block carries its truth");
        let on = Point3::new(3.0, -4.0, freeform_top_height(3.0, -4.0));
        assert!(truth(on).expect("over the top").abs() < 1e-12);
        let (gx, gy) = freeform_top_gradient(3.0, -4.0);
        let tilt = (1.0 + gx * gx + gy * gy).sqrt();
        let above = Point3::new(on.x, on.y, on.z + 0.1);
        assert!((truth(above).expect("near the top") - 0.1 / tilt).abs() < 1e-12);
        // The floor and the scanner-rounded rim are not the top.
        assert!(truth(Point3::new(3.0, -4.0, 0.0)).is_none());
        assert!(truth(Point3::new(44.5, 0.0, freeform_top_height(44.5, 0.0))).is_none());
        // The gradient is the height's own.
        let h = 1e-6;
        let numeric = (
            (freeform_top_height(3.0 + h, -4.0) - freeform_top_height(3.0 - h, -4.0)) / (2.0 * h),
            (freeform_top_height(3.0, -4.0 + h) - freeform_top_height(3.0, -4.0 - h)) / (2.0 * h),
        );
        assert!((numeric.0 - gx).abs() < 1e-6 && (numeric.1 - gy).abs() < 1e-6);
    }
}

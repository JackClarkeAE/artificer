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
pub fn disk_soup(center: Point3, normal: Vector3, radius: f64, segments: usize) -> Vec<[Point3; 3]> {
    let unit = normalize(normal).expect("disk normal must be nonzero");
    let (e1, e2) = orthonormal_basis(unit);
    let rim = |s: usize| {
        let angle = std::f64::consts::TAU * s as f64 / segments as f64;
        center + e1 * (radius * angle.cos()) + e2 * (radius * angle.sin())
    };
    (0..segments).map(|s| [center, rim(s), rim(s + 1)]).collect()
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
        (min, y, x),                      // bottom (-Z)
        (Point3::new(min.x, min.y, max.z), x, y), // top (+Z)
        (min, x, z),                      // front (-Y)
        (Point3::new(min.x, max.y, min.z), z, x), // back (+Y)
        (min, z, y),                      // left (-X)
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
            let normal = Vector3::new(
                phi.sin() * theta.cos(),
                phi.sin() * theta.sin(),
                phi.cos(),
            );
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

/// Revolves an open profile polyline `(radial distance, z)` fully about
/// +Z, outward winding.
pub fn revolved_profile_soup(
    profile: &[(f64, f64)],
    segments: usize,
) -> Vec<[Point3; 3]> {
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

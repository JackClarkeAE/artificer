//! Analytic primitive fitting: algebraic initial estimates refined by
//! damped least squares, the same estimate-then-polish structure the
//! commercial reverse-engineering kernels use.
//!
//! Every fit reports signed-distance statistics against the input samples so
//! callers can do tolerance-based model selection and deviation reporting.

use artificer_geometry::{Point3, Vector3};

use crate::numeric::{refine_least_squares, solve_linear, sym_eigen_3x3};
use crate::transform::{normalize, orthonormal_basis};

#[derive(Clone, Copy, Debug)]
pub struct DeviationStats {
    pub rms: f64,
    pub max_abs: f64,
}

pub(crate) fn stats(residuals: impl Iterator<Item = f64>) -> DeviationStats {
    let mut squared = 0.0;
    let mut max_abs = 0.0f64;
    let mut count = 0usize;
    for r in residuals {
        squared += r * r;
        max_abs = max_abs.max(r.abs());
        count += 1;
    }
    DeviationStats {
        rms: (squared / count.max(1) as f64).sqrt(),
        max_abs,
    }
}

fn centroid(points: &[Point3]) -> Point3 {
    let mut sum = Vector3::default();
    for p in points {
        sum = sum + (*p - Point3::default());
    }
    Point3::default() + sum / points.len().max(1) as f64
}

#[derive(Clone, Copy, Debug)]
pub struct PlaneFit {
    pub origin: Point3,
    pub normal: Vector3,
    pub deviation: DeviationStats,
}

impl PlaneFit {
    pub fn signed_distance(&self, point: Point3) -> f64 {
        (point - self.origin).dot(self.normal)
    }
}

/// Total least squares plane through the centroid; the normal is the
/// smallest-variance principal direction, oriented along `orientation_hint`.
pub fn fit_plane(points: &[Point3], orientation_hint: Option<Vector3>) -> Option<PlaneFit> {
    if points.len() < 3 {
        return None;
    }
    let center = centroid(points);
    let mut covariance = [[0.0; 3]; 3];
    for p in points {
        let d = *p - center;
        let v = [d.x, d.y, d.z];
        for i in 0..3 {
            for j in 0..3 {
                covariance[i][j] += v[i] * v[j];
            }
        }
    }
    let (_, vectors) = sym_eigen_3x3(covariance);
    let mut normal = Vector3::new(vectors[0][0], vectors[0][1], vectors[0][2]);
    normal = normalize(normal)?;
    if let Some(hint) = orientation_hint
        && normal.dot(hint) < 0.0
    {
        normal = normal * -1.0;
    }
    let fit = PlaneFit {
        origin: center,
        normal,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    let deviation = stats(points.iter().map(|p| fit.signed_distance(*p)));
    Some(PlaneFit { deviation, ..fit })
}

#[derive(Clone, Copy, Debug)]
pub struct SphereFit {
    pub center: Point3,
    pub radius: f64,
    pub deviation: DeviationStats,
}

impl SphereFit {
    pub fn signed_distance(&self, point: Point3) -> f64 {
        (point - self.center).length() - self.radius
    }
}

pub fn fit_sphere(points: &[Point3]) -> Option<SphereFit> {
    if points.len() < 4 {
        return None;
    }
    // Kasa algebraic fit: 2 c . p + k = |p|^2 in least squares.
    let mut a = vec![vec![0.0; 4]; 4];
    let mut b = vec![0.0; 4];
    for p in points {
        let row = [2.0 * p.x, 2.0 * p.y, 2.0 * p.z, 1.0];
        let rhs = p.x * p.x + p.y * p.y + p.z * p.z;
        for i in 0..4 {
            b[i] += row[i] * rhs;
            for j in 0..4 {
                a[i][j] += row[i] * row[j];
            }
        }
    }
    let solution = solve_linear(a, b)?;
    let center = Point3::new(solution[0], solution[1], solution[2]);
    let radius_squared =
        solution[3] + center.x * center.x + center.y * center.y + center.z * center.z;
    if !(radius_squared.is_finite() && radius_squared > 0.0) {
        return None;
    }
    let initial = vec![center.x, center.y, center.z, radius_squared.sqrt()];
    let refined = refine_least_squares(
        initial,
        |p| {
            let c = Point3::new(p[0], p[1], p[2]);
            points.iter().map(|q| (*q - c).length() - p[3]).collect()
        },
        20,
    );
    let center = Point3::new(refined[0], refined[1], refined[2]);
    let radius = refined[3];
    if !(radius.is_finite() && radius > 0.0) {
        return None;
    }
    let fit = SphereFit {
        center,
        radius,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    let deviation = stats(points.iter().map(|p| fit.signed_distance(*p)));
    Some(SphereFit { deviation, ..fit })
}

#[derive(Clone, Copy, Debug)]
pub struct CylinderFit {
    pub axis_point: Point3,
    /// Unit axis direction; the sign is arbitrary.
    pub axis: Vector3,
    pub radius: f64,
    pub deviation: DeviationStats,
}

impl CylinderFit {
    pub fn signed_distance(&self, point: Point3) -> f64 {
        (point - self.axis_point).cross(self.axis).length() - self.radius
    }
}

/// The axis of a cylindrical patch is the direction in which its surface
/// normals do not vary: the smallest eigenvector of the centred normal
/// covariance. Weights are triangle areas so tessellation density does not
/// bias the estimate.
fn normal_covariance_axis(normals: &[(Vector3, f64)]) -> Option<Vector3> {
    let total: f64 = normals.iter().map(|(_, w)| w).sum();
    if total.is_nan() || total <= 0.0 {
        return None;
    }
    let mut mean = Vector3::default();
    for (n, w) in normals {
        mean = mean + *n * *w;
    }
    mean = mean / total;
    let mut covariance = [[0.0; 3]; 3];
    for (n, w) in normals {
        let d = *n - mean;
        let v = [d.x, d.y, d.z];
        for i in 0..3 {
            for j in 0..3 {
                covariance[i][j] += w * v[i] * v[j];
            }
        }
    }
    let (_, vectors) = sym_eigen_3x3(covariance);
    normalize(Vector3::new(vectors[0][0], vectors[0][1], vectors[0][2]))
}

/// Kasa circle fit in 2D: returns (center_x, center_y, radius).
pub(crate) fn fit_circle_2d(samples: &[(f64, f64)]) -> Option<(f64, f64, f64)> {
    if samples.len() < 3 {
        return None;
    }
    let mut a = vec![vec![0.0; 3]; 3];
    let mut b = vec![0.0; 3];
    for (x, y) in samples {
        let row = [2.0 * x, 2.0 * y, 1.0];
        let rhs = x * x + y * y;
        for i in 0..3 {
            b[i] += row[i] * rhs;
            for j in 0..3 {
                a[i][j] += row[i] * row[j];
            }
        }
    }
    let solution = solve_linear(a, b)?;
    let radius_squared = solution[2] + solution[0] * solution[0] + solution[1] * solution[1];
    (radius_squared.is_finite() && radius_squared > 0.0)
        .then(|| (solution[0], solution[1], radius_squared.sqrt()))
}

pub fn fit_cylinder(points: &[Point3], normals: &[(Vector3, f64)]) -> Option<CylinderFit> {
    if points.len() < 6 {
        return None;
    }
    let axis = normal_covariance_axis(normals)?;
    let center = centroid(points);
    let (e1, e2) = orthonormal_basis(axis);
    let projected: Vec<(f64, f64)> = points
        .iter()
        .map(|p| {
            let d = *p - center;
            (d.dot(e1), d.dot(e2))
        })
        .collect();
    let (cx, cy, radius) = fit_circle_2d(&projected)?;
    // Parameters: axis-point offset in (e1, e2), axis tilt in (e1, e2), radius.
    let initial = vec![cx, cy, 0.0, 0.0, radius];
    let refined = refine_least_squares(
        initial,
        |p| {
            let Some(axis_dir) = normalize(axis + e1 * p[2] + e2 * p[3]) else {
                return points.iter().map(|_| f64::MAX).collect();
            };
            let axis_point = center + e1 * p[0] + e2 * p[1];
            points
                .iter()
                .map(|q| (*q - axis_point).cross(axis_dir).length() - p[4])
                .collect()
        },
        25,
    );
    let axis_dir = normalize(axis + e1 * refined[2] + e2 * refined[3])?;
    let axis_point = center + e1 * refined[0] + e2 * refined[1];
    let radius = refined[4];
    if !(radius.is_finite() && radius > 0.0) {
        return None;
    }
    let fit = CylinderFit {
        axis_point,
        axis: axis_dir,
        radius,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    let deviation = stats(points.iter().map(|p| fit.signed_distance(*p)));
    Some(CylinderFit { deviation, ..fit })
}

/// Exact least-squares cylinder with a prescribed axis direction: the
/// remaining unknowns (axis station and radius) reduce to a 2D circle fit
/// in the plane orthogonal to the axis. Used to re-fit patches once a
/// datum axis is established — small noisy patches get a stable radius
/// instead of a wobbling free axis.
pub fn fit_cylinder_with_axis(points: &[Point3], axis: Vector3) -> Option<CylinderFit> {
    let axis = normalize(axis)?;
    if points.len() < 3 {
        return None;
    }
    let center = centroid(points);
    let (e1, e2) = orthonormal_basis(axis);
    let projected: Vec<(f64, f64)> = points
        .iter()
        .map(|p| {
            let d = *p - center;
            (d.dot(e1), d.dot(e2))
        })
        .collect();
    let (cx, cy, radius) = fit_circle_2d(&projected)?;
    if !(radius.is_finite() && radius > 0.0) {
        return None;
    }
    let fit = CylinderFit {
        axis_point: center + e1 * cx + e2 * cy,
        axis,
        radius,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    let deviation = stats(points.iter().map(|p| fit.signed_distance(*p)));
    Some(CylinderFit { deviation, ..fit })
}

/// A torus patch produced by revolving a circular blend arc about an axis:
/// the shape of a fillet ring on a turned or revolved part.
#[derive(Clone, Copy, Debug)]
pub struct RevolvedBlendFit {
    /// Point on the revolve axis at the blend circle's height.
    pub axis_point: Point3,
    /// Unit revolve axis.
    pub axis: Vector3,
    /// Distance from the axis to the blend arc's centre.
    pub major_radius: f64,
    /// The blend (fillet) radius.
    pub minor_radius: f64,
    pub deviation: DeviationStats,
}

impl RevolvedBlendFit {
    pub fn signed_distance(&self, point: Point3) -> f64 {
        let v = point - self.axis_point;
        let h = v.dot(self.axis);
        let radial = (v - self.axis * h).length();
        (radial - self.major_radius).hypot(h) - self.minor_radius
    }
}

/// Fits a revolved blend to a patch given the revolve axis: points map to
/// profile space `(radial distance, height)` where a fillet ring becomes a
/// plain circular arc.
pub fn fit_revolved_blend(
    points: &[Point3],
    axis_point: Point3,
    axis: Vector3,
) -> Option<RevolvedBlendFit> {
    let axis = normalize(axis)?;
    if points.len() < 6 {
        return None;
    }
    let profile: Vec<(f64, f64)> = points
        .iter()
        .map(|p| {
            let v = *p - axis_point;
            let h = v.dot(axis);
            ((v - axis * h).length(), h)
        })
        .collect();
    let (major, height, minor) = fit_circle_2d(&profile)?;
    if !(minor.is_finite() && minor > 0.0 && major.is_finite() && major > minor) {
        return None;
    }
    let fit = RevolvedBlendFit {
        axis_point: axis_point + axis * height,
        axis,
        major_radius: major,
        minor_radius: minor,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    let deviation = stats(
        profile
            .iter()
            .map(|(radial, h)| (radial - major).hypot(h - height) - minor),
    );
    Some(RevolvedBlendFit { deviation, ..fit })
}

/// An n-fold circular pattern feature: one master surface, sampled as a
/// height-field in folded profile space, repeated about the datum axis.
/// The deviation statistics measure how tightly all instances collapse
/// onto the master — the tooth-to-tooth error of a gear.
#[derive(Clone, Copy, Debug)]
pub struct PatternFit {
    pub axis_point: Point3,
    pub axis: Vector3,
    pub count: usize,
    pub z_range: (f64, f64),
    pub radius_range: (f64, f64),
    /// Fold residual across all instances against the master surface.
    pub deviation: DeviationStats,
    /// The worst single instance's RMS fold residual.
    pub worst_instance_rms: f64,
    /// Helical drift of the pattern about the axis (radians of azimuth per
    /// millimetre of height); zero for a straight pattern.
    pub helix_rate: f64,
}

/// Transition geometry along the shared edge of two recognized features:
/// the physical round or chamfer band that connects them. Not an analytic
/// surface — its identity is the feature pair it connects.
#[derive(Clone, Copy, Debug)]
pub struct EdgeRoundFit {
    /// Mean total gap the band spans between its two support surfaces (mm).
    pub span: f64,
    /// Fold of the band's distances to its supports.
    pub deviation: DeviationStats,
}

#[derive(Clone, Copy, Debug)]
pub struct ConeFit {
    pub apex: Point3,
    /// Unit axis pointing from the apex into the material of the cone.
    pub axis: Vector3,
    pub half_angle: f64,
    pub deviation: DeviationStats,
}

impl ConeFit {
    pub fn signed_distance(&self, point: Point3) -> f64 {
        let v = point - self.apex;
        let h = v.dot(self.axis);
        let radial = (v - self.axis * h).length();
        radial * self.half_angle.cos() - h * self.half_angle.sin()
    }
}

/// Fits a cone to paired samples of `(surface point, unit normal, weight)`.
/// The pairing matters: the apex estimate intersects the tangent planes,
/// which requires each normal to belong to its point.
pub fn fit_cone(samples: &[(Point3, Vector3, f64)]) -> Option<ConeFit> {
    if samples.len() < 6 {
        return None;
    }
    let points: Vec<Point3> = samples.iter().map(|(p, _, _)| *p).collect();
    let normals: Vec<(Vector3, f64)> = samples.iter().map(|(_, n, w)| (*n, *w)).collect();
    let points = &points[..];
    let mut axis = normal_covariance_axis(&normals)?;
    let total: f64 = normals.iter().map(|(_, w)| w).sum();
    let mut sine = normals.iter().map(|(n, w)| n.dot(axis) * w).sum::<f64>() / total;
    if sine < 0.0 {
        axis = axis * -1.0;
        sine = -sine;
    }
    let half_angle = sine.clamp(0.0, 1.0).asin();
    if !(0.01..=1.55).contains(&half_angle) {
        return None;
    }
    // Every tangent plane of a cone passes through the apex:
    // n . (apex - p) = 0 in least squares over the samples.
    let mut a = vec![vec![0.0; 3]; 3];
    let mut b = vec![0.0; 3];
    for ((n, w), p) in normals.iter().zip(points) {
        let row = [n.x, n.y, n.z];
        let rhs = n.dot(*p - Point3::default());
        for i in 0..3 {
            b[i] += w * row[i] * rhs;
            for j in 0..3 {
                a[i][j] += w * row[i] * row[j];
            }
        }
    }
    let apex_solution = solve_linear(a, b)?;
    let apex = Point3::new(apex_solution[0], apex_solution[1], apex_solution[2]);
    // One centroid for all three uses below. Recomputing it inside the map
    // made the scale check quadratic, which at the 20 000-sample fit budget
    // is four hundred million point operations to produce one scalar.
    let center = centroid(points);
    let scale = points
        .iter()
        .map(|p| (*p - center).length())
        .fold(0.0f64, f64::max)
        .max(1.0);
    if (apex - center).length() > 200.0 * scale {
        return None;
    }
    // Point the axis from the apex toward the sampled material.
    if (center - apex).dot(axis) < 0.0 {
        axis = axis * -1.0;
    }
    let (e1, e2) = orthonormal_basis(axis);
    let initial = vec![apex.x, apex.y, apex.z, 0.0, 0.0, half_angle];
    let refined = refine_least_squares(
        initial,
        |p| {
            let Some(axis_dir) = normalize(axis + e1 * p[3] + e2 * p[4]) else {
                return points.iter().map(|_| f64::MAX).collect();
            };
            let apex = Point3::new(p[0], p[1], p[2]);
            let (sin_a, cos_a) = p[5].sin_cos();
            points
                .iter()
                .map(|q| {
                    let v = *q - apex;
                    let h = v.dot(axis_dir);
                    (v - axis_dir * h).length() * cos_a - h * sin_a
                })
                .collect()
        },
        25,
    );
    let axis_dir = normalize(axis + e1 * refined[3] + e2 * refined[4])?;
    let apex = Point3::new(refined[0], refined[1], refined[2]);
    let half_angle = refined[5];
    if !(0.008..=1.56).contains(&half_angle) {
        return None;
    }
    let fit = ConeFit {
        apex,
        axis: axis_dir,
        half_angle,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    let deviation = stats(points.iter().map(|p| fit.signed_distance(*p)));
    Some(ConeFit { deviation, ..fit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    #[test]
    fn plane_fit_recovers_a_tilted_plane() {
        let normal = normalize(Vector3::new(1.0, 2.0, 4.0)).unwrap();
        let origin = Point3::new(3.0, -1.0, 2.0);
        let (e1, e2) = orthonormal_basis(normal);
        let mut points = Vec::new();
        for i in 0..20 {
            for j in 0..20 {
                let noise = (((i * 7 + j * 13) % 11) as f64 - 5.0) * 1e-4;
                points
                    .push(origin + e1 * (i as f64 * 0.5) + e2 * (j as f64 * 0.5) + normal * noise);
            }
        }
        let fit = fit_plane(&points, Some(normal)).unwrap();
        assert!(fit.normal.dot(normal) > 1.0 - 1e-6);
        assert!(fit.deviation.rms < 1e-3);
    }

    #[test]
    fn sphere_fit_recovers_center_and_radius() {
        let (points, _) = synth::sphere_patch_samples(Point3::new(4.0, 5.0, -2.0), 9.0, 24, 12);
        let fit = fit_sphere(&points).unwrap();
        assert!((fit.radius - 9.0).abs() < 1e-6);
        assert!((fit.center - Point3::new(4.0, 5.0, -2.0)).length() < 1e-6);
    }

    #[test]
    fn cylinder_fit_recovers_axis_and_radius_on_a_partial_patch() {
        let axis = normalize(Vector3::new(0.2, 0.1, 1.0)).unwrap();
        let (points, normals) = synth::cylinder_patch_samples(
            Point3::new(1.0, 2.0, 3.0),
            axis,
            6.5,
            30.0,
            // A 140-degree partial patch, as a scan of a boss would produce.
            2.4,
            40,
            20,
        );
        let fit = fit_cylinder(&points, &normals).unwrap();
        assert!((fit.radius - 6.5).abs() < 1e-6, "radius {}", fit.radius);
        assert!(fit.axis.dot(axis).abs() > 1.0 - 1e-8);
        assert!(fit.deviation.rms < 1e-9);
    }

    #[test]
    fn cone_fit_recovers_apex_and_angle() {
        let axis = Vector3::new(0.0, 0.0, 1.0);
        let apex = Point3::new(2.0, -1.0, 5.0);
        let half_angle = 0.4f64;
        let (points, normals) =
            synth::cone_patch_samples(apex, axis, half_angle, 4.0, 25.0, 48, 16);
        let samples: Vec<_> = points
            .iter()
            .zip(&normals)
            .map(|(p, (n, w))| (*p, *n, *w))
            .collect();
        let fit = fit_cone(&samples).unwrap();
        assert!((fit.half_angle - half_angle).abs() < 1e-6);
        assert!((fit.apex - apex).length() < 1e-4);
        assert!(fit.axis.dot(axis) > 1.0 - 1e-8);
    }
}

/// Fits a torus to a patch without being told its axis.
///
/// [`fit_revolved_blend`] already fits a torus, but only when the
/// revolve axis is known — it is how a fillet ring is recovered on a
/// turned part. A moulded surface has no such axis to hand, and the
/// primitive is needed for a different reason: a cast panel crowned
/// unequally along its two principal directions is not a sphere, and
/// the sphere the vocabulary currently offers is stretched over it and
/// carries the mismatch as systematic residual. Over a 100 mm panel
/// crowned at 2000 mm one way and 6000 mm the other, that residual is
/// three times the measurement noise: the fit passes tolerance while
/// describing a shape the part does not have. A torus has two
/// independent principal radii and describes it exactly.
///
/// The axis is found rather than assumed. Over a shallow patch the
/// surface is its own second-order expansion, so the frame comes from a
/// plane fit, a quadric is fitted in that frame by linear least squares
/// — stable, unlike a direct seven-parameter torus solve — and its
/// Hessian's eigenvalues are the principal curvatures. At a torus's
/// outer equator the tube curvature 1/r runs along the axis and the
/// parallel curvature 1/(R+r) runs around it, so the sharper principal
/// direction *is* the axis and the two radii fall out by inversion.
///
/// Returns `None` for saddles (the two curvatures disagree in sign),
/// for a patch too near-spherical to have a meaningful major radius,
/// and for one too near-cylindrical — each of those has a simpler
/// primitive that fits it better, and offering a degenerate torus
/// instead would only take work away from a model that deserves it.
pub fn fit_torus(points: &[Point3]) -> Option<RevolvedBlendFit> {
    // A/B switch for a primitive with no ground truth to score against.
    // It lives here rather than at any one call site because the torus
    // is reachable from two of them, and a switch that silences only
    // one produces a comparison that looks clean and is not.
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DISABLED.get_or_init(|| std::env::var_os("ARTIFICER_NO_TORUS").is_some()) {
        return None;
    }
    /// Below this the major radius is noise and the patch is a sphere.
    const MIN_MAJOR: f64 = 1e-3;
    /// Above this the patch is a cylinder wearing a torus costume.
    const MAX_MAJOR: f64 = 1.0e6;
    if points.len() < 10 {
        return None;
    }
    let plane = fit_plane(points, None)?;
    let normal = plane.normal;
    // Any pair spanning the plane will do; the quadric is diagonalized
    // afterwards, so the frame's rotation about the normal is arbitrary.
    let hint = if normal.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    let u = {
        let raw = hint - normal * hint.dot(normal);
        let length = raw.length();
        if length < 1e-9 {
            return None;
        }
        raw / length
    };
    let v = normal.cross(u);
    let centroid = plane.origin;
    // z = a x^2 + b xy + c y^2 + d x + e y + f, in the plane's frame.
    let mut matrix = vec![vec![0.0; 6]; 6];
    let mut rhs = vec![0.0; 6];
    for point in points {
        let offset = Vector3::new(
            point.x - centroid.x,
            point.y - centroid.y,
            point.z - centroid.z,
        );
        let (x, y, z) = (offset.dot(u), offset.dot(v), offset.dot(normal));
        let row = [x * x, x * y, y * y, x, y, 1.0];
        for i in 0..6 {
            rhs[i] += row[i] * z;
            for j in 0..6 {
                matrix[i][j] += row[i] * row[j];
            }
        }
    }
    let quadric = solve_linear(matrix, rhs)?;
    let (a, b, c) = (quadric[0], quadric[1], quadric[2]);
    if !(a.is_finite() && b.is_finite() && c.is_finite()) {
        return None;
    }
    // Principal curvatures: eigenvalues of [[2a, b], [b, 2c]].
    let (h11, h12, h22) = (2.0 * a, b, 2.0 * c);
    let mean = 0.5 * (h11 + h22);
    let spread = (0.25 * (h11 - h22) * (h11 - h22) + h12 * h12).max(0.0).sqrt();
    let (kappa_1, kappa_2) = (mean + spread, mean - spread);
    // A saddle is not a torus patch at its outer equator, and the
    // vocabulary should say so rather than fit something plausible.
    if kappa_1 * kappa_2 <= 0.0 {
        return None;
    }
    let (sharp, gentle) = if kappa_1.abs() >= kappa_2.abs() {
        (kappa_1, kappa_2)
    } else {
        (kappa_2, kappa_1)
    };
    let minor_radius = 1.0 / sharp.abs();
    let major_radius = 1.0 / gentle.abs() - minor_radius;
    if !(major_radius.is_finite() && (MIN_MAJOR..=MAX_MAJOR).contains(&major_radius)) {
        return None;
    }
    // The axis runs along the sharper principal direction: at the outer
    // equator the tube's own curvature is the one measured along it.
    let axis_angle = if h12.abs() < 1e-15 && (h11 - h22).abs() < 1e-15 {
        0.0
    } else {
        0.5 * (2.0 * h12).atan2(h11 - h22)
    };
    let (first, second) = (
        u * axis_angle.cos() + v * axis_angle.sin(),
        u * -axis_angle.sin() + v * axis_angle.cos(),
    );
    let axis = if (kappa_1 - sharp).abs() < 1e-12 { first } else { second };
    // The centre of curvature sits on whichever side the patch bends
    // toward, and the axis passes through it.
    let side = if sharp > 0.0 { 1.0 } else { -1.0 };
    let axis_point = centroid + normal * (side * (major_radius + minor_radius));
    let seed = RevolvedBlendFit {
        axis_point,
        axis,
        major_radius,
        minor_radius,
        deviation: DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        },
    };
    // The quadric is only the leading term, so finish on the true torus
    // distance the same way every other fit here does.
    let refined = refine_least_squares(
        vec![
            axis_point.x,
            axis_point.y,
            axis_point.z,
            axis.x,
            axis.y,
            axis.z,
            major_radius,
            minor_radius,
        ],
        |p| {
            let Some(direction) = normalize(Vector3::new(p[3], p[4], p[5])) else {
                return points.iter().map(|_| 0.0).collect();
            };
            let candidate = RevolvedBlendFit {
                axis_point: Point3::new(p[0], p[1], p[2]),
                axis: direction,
                major_radius: p[6],
                minor_radius: p[7],
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            };
            points.iter().map(|q| candidate.signed_distance(*q)).collect()
        },
        30,
    );
    let fit = match normalize(Vector3::new(refined[3], refined[4], refined[5])) {
        Some(direction)
            if refined[6].is_finite()
                && refined[7].is_finite()
                && refined[7] > 0.0
                && (MIN_MAJOR..=MAX_MAJOR).contains(&refined[6]) =>
        {
            RevolvedBlendFit {
                axis_point: Point3::new(refined[0], refined[1], refined[2]),
                axis: direction,
                major_radius: refined[6],
                minor_radius: refined[7],
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            }
        }
        // Refinement wandered somewhere unusable; the seed still
        // describes the patch, so report that rather than nothing.
        _ => seed,
    };
    let deviation = stats(points.iter().map(|p| fit.signed_distance(*p)));
    // The curvature has to be measurably there. A seven-parameter
    // surface will fit almost any small noisy patch, and an absolute
    // radius ceiling is no defence: on a prismatic part this produced
    // tori across flat and cylindrical faces and cost twelve points of
    // invented surface — geometry emitted where the scan has none. It
    // is the same failure the revolved-band extractor already names,
    // where a shallow taper fits an absurd sphere whenever the
    // vocabulary offers one.
    //
    // So gate on evidence rather than on magnitude. Across a patch of
    // this extent, the gentler principal radius bows the surface away
    // from flat by roughly `(extent/2)^2 / 2R`. Unless that sagitta
    // stands clear of the fit's own residual, nothing here distinguishes
    // the curve from noise, and the honest answer is that this patch is
    // not evidence of a torus. The same test set the flat-versus-crowned
    // crossover at the noise floor, which is where it belongs.
    if !torus_is_evidenced(&fit, points) {
        return None;
    }
    Some(RevolvedBlendFit { deviation, ..fit })
}

/// Whether a patch actually evidences the torus fitted to it.
///
/// Across a patch of this extent the gentler principal radius bows the
/// surface away from flat by roughly `(extent/2)^2 / 2R`. Unless that
/// sagitta stands clear of the residual, nothing distinguishes the curve
/// from noise and the patch is not evidence of a torus.
///
/// This is deliberately separate from the fit so it can be asked again
/// later. A surface is fitted to one set of faces and then grows: merge,
/// absorption and consolidation all change what a feature owns, and a
/// torus that was evidenced by the patch it was born on may own
/// something quite different by the end. Testing only at birth lets an
/// unevidenced torus survive to the output wearing a freshly measured
/// deviation that says nothing about whether it should exist.
pub(crate) fn torus_is_evidenced(fit: &RevolvedBlendFit, points: &[Point3]) -> bool {
    /// How far the bow must stand clear of the residual to count as
    /// curvature rather than as noise.
    const EVIDENCE: f64 = 3.0;
    if points.len() < 10 {
        return false;
    }
    let count = points.len() as f64;
    let centroid = Point3::new(
        points.iter().map(|p| p.x).sum::<f64>() / count,
        points.iter().map(|p| p.y).sum::<f64>() / count,
        points.iter().map(|p| p.z).sum::<f64>() / count,
    );
    // Each radius has to be evidenced by the direction it actually
    // curves in, which one isotropic extent cannot express. A long thin
    // band bends along its length and barely wraps the tube at all: a
    // 69 mm^2 sliver 42 mm long survived an isotropic test claiming a
    // 2548 mm sweep about a 6.6 mm tube, because its length alone
    // carried the measure while the tube radius rested on a width of
    // about a millimetre and a half. The tube's curvature runs along the
    // axis and the sweep's runs around it, so measure the patch in both
    // and ask each radius to earn its own.
    let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut across = 0.0f64;
    for point in points {
        let v = Vector3::new(
            point.x - centroid.x,
            point.y - centroid.y,
            point.z - centroid.z,
        );
        let along = v.dot(fit.axis);
        low = low.min(along);
        high = high.max(along);
        across = across.max((v - fit.axis * along).length());
    }
    let meridian = high - low;
    let parallel = 2.0 * across;
    let rms = stats(points.iter().map(|p| fit.signed_distance(*p))).rms;
    let bar = EVIDENCE * rms;
    let tube = (meridian * meridian) / (8.0 * fit.minor_radius.max(1e-9));
    let sweep = (parallel * parallel) / (8.0 * (fit.major_radius + fit.minor_radius).max(1e-9));
    tube >= bar && sweep >= bar
}

#[cfg(test)]
mod torus_tests {
    use super::*;

    /// A patch of a real torus about +Z, sampled near its outer equator.
    fn torus_patch(major: f64, minor: f64, span: f64, steps: usize) -> Vec<Point3> {
        let mut points = Vec::new();
        for i in 0..=steps {
            for j in 0..=steps {
                let theta = -span + 2.0 * span * i as f64 / steps as f64;
                let phi = -span + 2.0 * span * j as f64 / steps as f64;
                let ring = major + minor * phi.cos();
                points.push(Point3::new(
                    ring * theta.cos(),
                    ring * theta.sin(),
                    minor * phi.sin(),
                ));
            }
        }
        points
    }

    #[test]
    fn a_torus_patch_gives_back_its_own_radii_without_being_told_the_axis() {
        let points = torus_patch(50.0, 10.0, 0.45, 24);
        let fit = fit_torus(&points).expect("a torus patch fits a torus");
        assert!(
            (fit.major_radius - 50.0).abs() < 0.5,
            "major {} should be 50",
            fit.major_radius
        );
        assert!(
            (fit.minor_radius - 10.0).abs() < 0.2,
            "minor {} should be 10",
            fit.minor_radius
        );
        // The axis was never supplied: it has to fall out of the patch.
        assert!(
            fit.axis.z.abs() > 0.99,
            "axis {:?} should be +Z",
            (fit.axis.x, fit.axis.y, fit.axis.z)
        );
        assert!(fit.deviation.rms < 1e-3, "rms {}", fit.deviation.rms);
    }

    #[test]
    fn an_unequally_crowned_panel_fits_to_its_two_principal_radii() {
        // The moulded case: a 100 mm panel crowned 2000 mm one way and
        // 6000 mm the other. A sphere stretched over this carries the
        // mismatch as systematic residual; a torus should not.
        let (rx, ry, half) = (2000.0f64, 6000.0f64, 50.0);
        let mut points = Vec::new();
        for i in 0..=60 {
            for j in 0..=60 {
                let x = -half + 2.0 * half * i as f64 / 60.0;
                let y = -half + 2.0 * half * j as f64 / 60.0;
                points.push(Point3::new(x, y, -(x * x) / (2.0 * rx) - (y * y) / (2.0 * ry)));
            }
        }
        let sphere = fit_sphere(&points).expect("a sphere always fits something");
        let torus = fit_torus(&points).expect("an unequal crown is a torus patch");
        // Principal radii, recovered: the tube is the sharper one.
        assert!(
            (torus.minor_radius - rx).abs() < 0.05 * rx,
            "minor {} should be near {rx}",
            torus.minor_radius
        );
        assert!(
            (torus.major_radius + torus.minor_radius - ry).abs() < 0.05 * ry,
            "major+minor {} should be near {ry}",
            torus.major_radius + torus.minor_radius
        );
        // And it describes the panel far better than the sphere does.
        assert!(
            torus.deviation.rms < 0.2 * sphere.deviation.rms,
            "torus rms {} against sphere rms {}",
            torus.deviation.rms,
            sphere.deviation.rms
        );
    }

    #[test]
    fn a_torus_that_outgrows_its_evidence_stops_being_one() {
        // Born on a patch that genuinely curves: a real torus patch.
        let born = torus_patch(50.0, 10.0, 0.45, 24);
        let fit = fit_torus(&born).expect("a torus patch fits a torus");
        assert!(
            torus_is_evidenced(&fit, &born),
            "the patch it was fitted to must evidence it"
        );
        // Then it grows. Merge, absorption and consolidation all change
        // what a feature owns, and here it ends up holding a flat sheet
        // sitting where the tube's outer equator was. The stored fit is
        // untouched and would still report its old radii; only asking
        // the question again catches it.
        let mut grown = Vec::new();
        for i in 0..=30 {
            for j in 0..=30 {
                grown.push(Point3::new(
                    60.0 + 0.02 * i as f64,
                    -3.0 + 0.2 * j as f64,
                    0.0,
                ));
            }
        }
        assert!(
            !torus_is_evidenced(&fit, &grown),
            "a flat sheet is not evidence of a torus"
        );
        // And the demotion actually happens on the surface itself.
        let mut surface = crate::segment::SurfaceClass::Torus(fit);
        surface.recompute_deviation(&grown);
        assert!(
            matches!(surface, crate::segment::SurfaceClass::Freeform),
            "an unevidenced torus demotes rather than surviving to output"
        );
    }

    #[test]
    fn a_saddle_is_refused_rather_than_fitted_plausibly() {
        let mut points = Vec::new();
        for i in 0..=40 {
            for j in 0..=40 {
                let x = -50.0 + 100.0 * i as f64 / 40.0;
                let y = -50.0 + 100.0 * j as f64 / 40.0;
                // Opposite signs: no outer-equator torus patch has this.
                points.push(Point3::new(x, y, x * x / 4000.0 - y * y / 4000.0));
            }
        }
        assert!(fit_torus(&points).is_none(), "a saddle is not a torus patch");
    }
}

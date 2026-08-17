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

fn stats(residuals: impl Iterator<Item = f64>) -> DeviationStats {
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
    let scale = points
        .iter()
        .map(|p| (*p - centroid(points)).length())
        .fold(0.0f64, f64::max)
        .max(1.0);
    if (apex - centroid(points)).length() > 200.0 * scale {
        return None;
    }
    // Point the axis from the apex toward the sampled material.
    if (centroid(points) - apex).dot(axis) < 0.0 {
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

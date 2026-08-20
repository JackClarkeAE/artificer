//! Scan alignment: PCA pre-alignment plus trimmed point-to-plane ICP.
//!
//! This is the "best fit" alignment of metrology suites: correspondences are
//! trimmed against a robust median scale each iteration so partial overlap
//! and scanner outliers do not drag the fit, and the point-to-plane error
//! metric lets flat regions slide into place instead of stalling the way
//! point-to-point ICP does.

use artificer_geometry::{Point3, Vector3};

use crate::mesh::TriangleMesh;
use crate::numeric::{solve_linear, sym_eigen_3x3};
use crate::spatial::KdTree3;
use crate::transform::{RigidTransform, normalize};

#[derive(Clone, Copy, Debug)]
pub struct IcpParams {
    pub max_iterations: usize,
    /// Upper bound on source samples per iteration; vertices are strided.
    pub sample_budget: usize,
    /// Stop when an iteration moves the sampled points less than this
    /// fraction of the target's own extent.
    ///
    /// A length would only mean the same thing at one model scale: 1e-5 mm
    /// is a reasonable floor on a 100 mm part and far too coarse on a scan
    /// whose units are metres. Expressing it as a ratio makes convergence
    /// independent of what the file measures in.
    pub convergence_ratio: f64,
    /// Keep this fraction of correspondences, best residual first.
    ///
    /// This is the trim in "trimmed ICP", and it is what makes partial
    /// overlap work: the correspondences for source geometry the target
    /// simply does not contain are discarded by rank rather than by
    /// magnitude, so they cannot drag the fit however large they are.
    pub keep_fraction: f64,
    /// Reject correspondences farther than this multiple of the median, on
    /// top of the trim above.
    pub rejection_scale: f64,
    /// Run PCA axis pre-alignment before iterating.
    pub prealign: bool,
}

impl Default for IcpParams {
    fn default() -> Self {
        Self {
            max_iterations: 60,
            sample_budget: 4000,
            // 1e-7 of a 100 mm part is the 1e-5 mm this used to be.
            convergence_ratio: 1e-7,
            keep_fraction: 0.8,
            rejection_scale: 3.0,
            prealign: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IcpResult {
    /// Maps source coordinates into target coordinates.
    pub transform: RigidTransform,
    /// Root-mean-square point-to-plane distance over accepted matches.
    pub rms: f64,
    pub iterations: usize,
    /// Fraction of sampled correspondences accepted in the final iteration.
    pub inlier_fraction: f64,
}

fn centroid(points: &[Point3]) -> Point3 {
    let mut sum = Vector3::default();
    for p in points {
        sum = sum + (*p - Point3::default());
    }
    Point3::default() + sum / points.len().max(1) as f64
}

fn covariance(points: &[Point3], center: Point3) -> [[f64; 3]; 3] {
    let mut m = [[0.0; 3]; 3];
    for p in points {
        let d = *p - center;
        let v = [d.x, d.y, d.z];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += v[i] * v[j];
            }
        }
    }
    m
}

fn subsample(points: &[Point3], budget: usize) -> Vec<Point3> {
    let stride = points.len().div_ceil(budget.max(1)).max(1);
    points.iter().step_by(stride).copied().collect()
}

/// Aligns principal axes of the source cloud onto the target cloud, testing
/// the four proper-rotation sign choices and keeping whichever leaves the
/// smallest nearest-neighbour error.
fn pca_prealign(source: &[Point3], target: &[Point3], tree: &KdTree3) -> RigidTransform {
    let source_center = centroid(source);
    let target_center = centroid(target);
    let (_, source_axes) = sym_eigen_3x3(covariance(source, source_center));
    let (_, target_axes) = sym_eigen_3x3(covariance(target, target_center));
    let axis = |rows: [[f64; 3]; 3], i: usize| Vector3::new(rows[i][0], rows[i][1], rows[i][2]);
    let mut best: Option<(f64, RigidTransform)> = None;
    for signs in [[1.0, 1.0], [1.0, -1.0], [-1.0, 1.0], [-1.0, -1.0]] {
        let s0 = axis(source_axes, 0) * signs[0];
        let s1 = axis(source_axes, 1) * signs[1];
        let s2 = s0.cross(s1);
        let t0 = axis(target_axes, 0);
        let t1 = axis(target_axes, 1);
        let t2 = t0.cross(t1);
        // rotation = [t0 t1 t2] * [s0 s1 s2]^T maps source axes onto target axes.
        let t_axes = [t0.to_array(), t1.to_array(), t2.to_array()];
        let s_axes = [s0.to_array(), s1.to_array(), s2.to_array()];
        let mut rotation = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    rotation[i][j] += t_axes[k][i] * s_axes[k][j];
                }
            }
        }
        let rotate = RigidTransform {
            rotation,
            translation: Vector3::default(),
        };
        let shifted = target_center - rotate.apply_point(source_center);
        let candidate = rotate.then(&RigidTransform::from_translation(shifted));
        let probe = subsample(source, 400);
        let mut error = 0.0;
        for p in &probe {
            if let Some((_, d2)) = tree.nearest(candidate.apply_point(*p)) {
                error += d2;
            }
        }
        if best.is_none_or(|(best_error, _)| error < best_error) {
            best = Some((error, candidate));
        }
    }
    best.map(|(_, t)| t).unwrap_or(RigidTransform::IDENTITY)
}

trait ToArray {
    fn to_array(self) -> [f64; 3];
}

impl ToArray for Vector3 {
    fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
}

/// Best-fit aligns `source` onto `target`.
pub fn best_fit_align(
    source: &TriangleMesh,
    target: &TriangleMesh,
    params: IcpParams,
) -> Option<IcpResult> {
    let target_points = target.positions().to_vec();
    let target_normals = target.vertex_normals();
    let tree = KdTree3::build(target_points.clone());
    if tree.is_empty() || source.positions().is_empty() {
        return None;
    }
    let samples = subsample(source.positions(), params.sample_budget);
    // Resolve the convergence ratio against the target's extent once, so
    // every iteration below compares a length to a length at this model's
    // own scale.
    let extent = target.bounds_diagonal();
    let convergence = if extent.is_finite() && extent > 0.0 {
        params.convergence_ratio * extent
    } else {
        params.convergence_ratio
    };
    let mut transform = if params.prealign {
        pca_prealign(source.positions(), &target_points, &tree)
    } else {
        RigidTransform::from_translation(centroid(&target_points) - centroid(source.positions()))
    };
    let mut rms = f64::INFINITY;
    let mut inlier_fraction = 0.0;
    let mut iterations = 0;
    for iteration in 0..params.max_iterations {
        iterations = iteration + 1;
        // Gather correspondences under the current transform.
        let mut matches: Vec<(Point3, Point3, Vector3, f64)> = Vec::with_capacity(samples.len());
        let mut distances: Vec<f64> = Vec::with_capacity(samples.len());
        for p in &samples {
            let moved = transform.apply_point(*p);
            let Some((index, d2)) = tree.nearest(moved) else {
                continue;
            };
            let normal = target_normals[index as usize];
            if normal.length() < 0.5 {
                continue;
            }
            let q = target_points[index as usize];
            matches.push((moved, q, normal, d2.sqrt()));
            distances.push(d2.sqrt());
        }
        if matches.len() < 6 {
            return None;
        }
        distances.sort_by(f64::total_cmp);
        let median = distances[distances.len() / 2];
        // A trimmed fit has to actually trim. A multiple of the median does
        // not: where the overlap is partial, the correspondences that ought
        // to be excluded are themselves what inflates the median, the band
        // widens to swallow them, and the run reports a 100% inlier
        // fraction — the one number a caller would read to notice the
        // alignment never took. Taking the best `keep_fraction` by rank
        // bounds the accepted set no matter how the residuals are
        // distributed; the median band then still rejects outliers inside
        // that set when the fit is already good.
        let keep = ((distances.len() as f64 * params.keep_fraction).round() as usize)
            .clamp(6, distances.len());
        let trimmed = distances[keep - 1];
        let cutoff = trimmed
            .min(params.rejection_scale * median)
            .max(convergence * 10.0);
        // Point-to-plane linearization: rows [p x n; n], residual n . (p - q).
        let mut a = vec![vec![0.0; 6]; 6];
        let mut b = vec![0.0; 6];
        let mut accepted = 0usize;
        let mut squared = 0.0;
        for (moved, q, normal, distance) in &matches {
            if *distance > cutoff {
                continue;
            }
            accepted += 1;
            let p = *moved - Point3::default();
            let c = p.cross(*normal);
            let row = [c.x, c.y, c.z, normal.x, normal.y, normal.z];
            let residual = normal.dot(*moved - *q);
            squared += residual * residual;
            for i in 0..6 {
                b[i] -= row[i] * residual;
                for j in 0..6 {
                    a[i][j] += row[i] * row[j];
                }
            }
        }
        if accepted < 6 {
            return None;
        }
        rms = (squared / accepted as f64).sqrt();
        inlier_fraction = accepted as f64 / matches.len() as f64;
        // Tikhonov regularization: a scan that does not constrain every
        // degree of freedom (all-parallel normals, a single plane) gets a
        // zero step in the unconstrained directions instead of a divergent
        // solve.
        let trace: f64 = (0..6).map(|i| a[i][i]).sum();
        let ridge = 1e-9 * (trace / 6.0).max(1e-12);
        for (i, row) in a.iter_mut().enumerate() {
            row[i] += ridge;
        }
        let Some(delta) = solve_linear(a, b) else {
            break;
        };
        let omega = Vector3::new(delta[0], delta[1], delta[2]);
        let translation = Vector3::new(delta[3], delta[4], delta[5]);
        let step = RigidTransform::from_axis_angle(omega, omega.length())
            .unwrap_or(RigidTransform::IDENTITY)
            .then(&RigidTransform::from_translation(translation));
        transform = transform.then(&step).renormalized();
        if omega.length() + translation.length() < convergence {
            break;
        }
    }
    Some(IcpResult {
        transform,
        rms,
        iterations,
        inlier_fraction,
    })
}

/// Datum (3-2-1) alignment: primary plane normal becomes +Z, the secondary
/// direction becomes +X, and the origin datum maps to the world origin.
pub fn datum_alignment(
    origin: Point3,
    primary_normal: Vector3,
    secondary_direction: Vector3,
) -> Option<RigidTransform> {
    let _ = normalize(primary_normal)?;
    RigidTransform::to_frame(origin, secondary_direction, primary_normal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    #[test]
    fn icp_recovers_a_known_pose() {
        let pose = RigidTransform::from_axis_angle(Vector3::new(0.3, 1.0, 0.2), 0.35)
            .unwrap()
            .then(&RigidTransform::from_translation(Vector3::new(
                7.0, -4.0, 2.5,
            )));
        // A plate breaks the cylinder's rotational symmetry and a top cap
        // supplies axial normals, so the pose is fully constrained.
        let mut soup = synth::open_cylinder_soup(12.0, 40.0, 96, 24);
        soup.extend(synth::plane_patch_soup(
            Point3::new(12.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
            18.0,
            30.0,
            12,
            20,
        ));
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 40.0),
            Vector3::new(0.0, 0.0, 1.0),
            12.0,
            96,
        ));
        let target = TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let source = target.transformed(&pose.inverse());
        let result = best_fit_align(&source, &target, IcpParams::default()).unwrap();
        assert!(result.rms < 1e-3, "rms {} too high", result.rms);
        // The recovered transform must reproduce the pose on sample points.
        for p in source.positions().iter().step_by(97) {
            let expected = pose.apply_point(*p);
            let actual = result.transform.apply_point(*p);
            assert!((expected - actual).length() < 5e-3);
        }
    }

    #[test]
    fn a_partial_overlap_reports_an_inlier_fraction_that_means_something() {
        // The source carries a wing the target does not have at all. Those
        // correspondences can never be satisfied, so a genuine trim has to
        // throw them out and say it did. A cutoff taken as a multiple of the
        // median instead widens to include them — they are what raises the
        // median — and the run comes back claiming every correspondence was
        // an inlier while the fit has been dragged off by the wing.
        let mut soup = synth::open_cylinder_soup(12.0, 40.0, 96, 24);
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 40.0),
            Vector3::new(0.0, 0.0, 1.0),
            12.0,
            96,
        ));
        let target = TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();

        // The wing is deliberately the majority of the source. That is what
        // makes this the failing case: with the unmatched points in the
        // minority any sane band excludes them, but once they dominate they
        // *are* the median, and a cutoff defined as a multiple of the median
        // stretches to cover them.
        let mut with_wing = soup.clone();
        with_wing.extend(synth::plane_patch_soup(
            Point3::new(400.0, 0.0, 20.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
            120.0,
            120.0,
            80,
            80,
        ));
        let source = TriangleMesh::from_triangle_soup(&with_wing, 1e-9).unwrap();

        let result = best_fit_align(&source, &target, IcpParams::default())
            .expect("a partial overlap still aligns");
        assert!(
            result.inlier_fraction < 0.95,
            "most of the source has no counterpart in the target, so the \
             inlier fraction must not read as a complete match: {}",
            result.inlier_fraction
        );
    }

    #[test]
    fn convergence_is_a_ratio_so_it_survives_a_change_of_units() {
        // The same part expressed in different units must take the same
        // path. With an absolute millimetre threshold the metre-scale copy
        // stops a hundred times too early.
        let build = |scale: f64| {
            let mut soup = synth::open_cylinder_soup(12.0 * scale, 40.0 * scale, 96, 24);
            soup.extend(synth::disk_soup(
                Point3::new(0.0, 0.0, 40.0 * scale),
                Vector3::new(0.0, 0.0, 1.0),
                12.0 * scale,
                96,
            ));
            soup.extend(synth::plane_patch_soup(
                Point3::new(12.0 * scale, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
                18.0 * scale,
                30.0 * scale,
                12,
                20,
            ));
            TriangleMesh::from_triangle_soup(&soup, 1e-12).unwrap()
        };
        let pose = |scale: f64| {
            RigidTransform::from_axis_angle(Vector3::new(0.3, 1.0, 0.2), 0.35)
                .unwrap()
                .then(&RigidTransform::from_translation(Vector3::new(
                    7.0 * scale,
                    -4.0 * scale,
                    2.5 * scale,
                )))
        };

        for scale in [1.0, 0.001] {
            let target = build(scale);
            let source = target.transformed(&pose(scale).inverse());
            let result =
                best_fit_align(&source, &target, IcpParams::default()).expect("both scales align");
            // The residual has to be small *relative to the part*, which is
            // the only comparison that means anything across units.
            let relative = result.rms / (40.0 * scale);
            assert!(
                relative < 1e-4,
                "scale {scale}: relative rms {relative} (rms {}) — convergence \
                 did not follow the model's units",
                result.rms
            );
        }
    }

    #[test]
    fn datum_alignment_levels_a_tilted_plane() {
        let normal = Vector3::new(0.1, 0.2, 0.97);
        let t = datum_alignment(
            Point3::new(3.0, 1.0, 2.0),
            normal,
            Vector3::new(1.0, 0.0, 0.0),
        )
        .unwrap();
        let mapped = t.apply_vector(normal);
        assert!(mapped.x.abs() < 1e-12 && mapped.y.abs() < 1e-12 && mapped.z > 0.0);
    }
}

//! Which motion generates a surface.
//!
//! The pipeline's revolved path asks one question — "is this a surface of
//! revolution about the datum axis?" — and answers yes or no. That is a
//! special case of a better question. A surface swept by a one-parameter
//! rigid motion is a *kinematic surface*, and revolution, extrusion and
//! helical sweep are the same object with different motion parameters.
//! Asking which motion generates a patch returns the classification and
//! its parameters together, from one eigenproblem.
//!
//! The construction is Pottmann and Randrup's. A rigid velocity field is
//! `v(x) = c̄ + c × x`, and the pair `(c, c̄)` names the motion:
//!
//! - `c = 0` — pure translation, sliding along `c̄`: an extrusion
//! - `c · c̄ = 0` — pure rotation about an axis read off `(c, c̄)`
//! - `c · c̄ ≠ 0` — a helical motion of pitch `c·c̄ / c²`
//!
//! A surface normal, taken as a line, is a *path normal* of the motion
//! exactly when `c · n̄ + c̄ · n = 0`, where `(n, n̄ = p × n)` are the
//! line's Plücker coordinates. So fitting the motion means fitting a
//! linear line complex to the estimated normals — a quadratic form in six
//! unknowns, which reduces to a symmetric 3×3 eigenproblem.
//!
//! Draft comes free. For a translational sweep the normals lie on a great
//! circle of the Gauss sphere when the walls are parallel to the sweep,
//! and on a small circle offset by `sin δ` when they lean by a draft
//! angle `δ`. The mean of `n · direction` is that offset; the scatter
//! about it is the fit's quality. Nothing else in the pipeline can
//! measure draft, and on cast parts almost every wall carries some.

use artificer_geometry::{Point3, Vector3};

/// A one-parameter rigid motion that sweeps a surface.
#[derive(Clone, Copy, Debug)]
pub enum Motion {
    /// Swept by sliding along `direction`: a general extrusion. `draft`
    /// is the angle by which the walls lean out of that direction, zero
    /// for a prismatic wall.
    Translation { direction: Vector3, draft: f64 },
    /// Swept by turning about the axis through `point` along `axis`.
    Rotation { point: Point3, axis: Vector3 },
    /// Turning and sliding together, `pitch` millimetres per radian.
    Helical {
        point: Point3,
        axis: Vector3,
        pitch: f64,
    },
}

impl Motion {
    pub fn describe(&self) -> String {
        match self {
            Motion::Translation { direction, draft } => format!(
                "extrusion along ({:+.3} {:+.3} {:+.3}), draft {:.2} deg",
                direction.x,
                direction.y,
                direction.z,
                draft.to_degrees()
            ),
            Motion::Rotation { axis, .. } => format!(
                "revolution about ({:+.3} {:+.3} {:+.3})",
                axis.x, axis.y, axis.z
            ),
            Motion::Helical { axis, pitch, .. } => format!(
                "helical about ({:+.3} {:+.3} {:+.3}), pitch {:.3} mm/rad",
                axis.x, axis.y, axis.z, pitch
            ),
        }
    }
}

pub struct MotionFit {
    pub motion: Motion,
    /// Deviation from the motion's path-normal condition, in millimetres:
    /// how far the surface is from being swept by it.
    pub residual: f64,
}

/// Cyclic Jacobi eigen-decomposition of a symmetric 3×3 matrix. Returns
/// eigenvalues ascending with their eigenvectors.
#[allow(clippy::needless_range_loop)]
fn eigen_symmetric(matrix: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut a = matrix;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..64 {
        let (mut p, mut q, mut worst) = (0usize, 1usize, 0.0);
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            if a[i][j].abs() > worst {
                worst = a[i][j].abs();
                p = i;
                q = j;
            }
        }
        if worst < 1e-14 {
            break;
        }
        let theta = 0.5 * (a[q][q] - a[p][p]) / a[p][q];
        let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
        let c = 1.0 / (t * t + 1.0).sqrt();
        let s = t * c;
        for k in 0..3 {
            let (akp, akq) = (a[k][p], a[k][q]);
            a[k][p] = c * akp - s * akq;
            a[k][q] = s * akp + c * akq;
        }
        for k in 0..3 {
            let (apk, aqk) = (a[p][k], a[q][k]);
            a[p][k] = c * apk - s * aqk;
            a[q][k] = s * apk + c * aqk;
        }
        for k in 0..3 {
            let (vkp, vkq) = (v[k][p], v[k][q]);
            v[k][p] = c * vkp - s * vkq;
            v[k][q] = s * vkp + c * vkq;
        }
    }
    let mut order = [0usize, 1, 2];
    order.sort_by(|&i, &j| a[i][i].total_cmp(&a[j][j]));
    let values = [
        a[order[0]][order[0]],
        a[order[1]][order[1]],
        a[order[2]][order[2]],
    ];
    let vectors = [
        [v[0][order[0]], v[1][order[0]], v[2][order[0]]],
        [v[0][order[1]], v[1][order[1]], v[2][order[1]]],
        [v[0][order[2]], v[1][order[2]], v[2][order[2]]],
    ];
    (values, vectors)
}

/// Inverse of a symmetric 3×3, ridged so a rank-deficient normal
/// distribution (every normal alike, as on a plane) stays solvable.
fn inverse_ridged(m: [[f64; 3]; 3], ridge: f64) -> Option<[[f64; 3]; 3]> {
    let a = [
        [m[0][0] + ridge, m[0][1], m[0][2]],
        [m[1][0], m[1][1] + ridge, m[1][2]],
        [m[2][0], m[2][1], m[2][2] + ridge],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inv,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inv,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inv,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inv,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inv,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inv,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inv,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inv,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inv,
        ],
    ])
}

fn apply(m: &[[f64; 3]; 3], v: Vector3) -> Vector3 {
    Vector3::new(
        m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
        m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
        m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
    )
}

/// Fits the motion that best sweeps the sampled surface.
///
/// Samples are `(point, unit normal, weight)`; weight is normally the
/// face area, so large well-conditioned faces dominate and scanner noise
/// on small ones averages out.
#[allow(clippy::needless_range_loop)]
/// The translation reading alone: direction, draft, and residual.
///
/// `fit_motion` answers "what motion sweeps this best", and on a
/// spot-smoothed box the rotation branch can score a hair lower than
/// the translation that actually made the part — the general question
/// then refuses a perfectly good extrusion on branch identity. When
/// the *hypothesis* is already a translation, this asks exactly that
/// question and lets the residual cap do the judging.
pub fn fit_translation(samples: &[(Point3, Vector3, f64)]) -> Option<(Vector3, f64, f64)> {
    if samples.len() < 12 {
        return None;
    }
    let mut weight_sum = 0.0;
    let mut centroid = Vector3::new(0.0, 0.0, 0.0);
    for &(point, _, weight) in samples {
        centroid = centroid + Vector3::new(point.x, point.y, point.z) * weight;
        weight_sum += weight;
    }
    if weight_sum <= 0.0 {
        return None;
    }
    centroid = centroid / weight_sum;
    let origin = Point3::new(centroid.x, centroid.y, centroid.z);
    let mut c = [[0.0f64; 3]; 3];
    let mut radius_sum = 0.0;
    for &(point, normal, weight) in samples {
        radius_sum += weight * (point - origin).length();
        let nn = [normal.x, normal.y, normal.z];
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] += weight * nn[i] * nn[j];
            }
        }
    }
    let mean_radius = (radius_sum / weight_sum).max(1e-6);
    let (values, vectors) = eigen_symmetric(c);
    // The degeneracy guard is unchanged: one plane is swept by any
    // in-plane translation and deserves no confident answer.
    if (values[1] - values[0]) / weight_sum <= 0.05 {
        return None;
    }
    let direction = Vector3::new(vectors[0][0], vectors[0][1], vectors[0][2]);
    let offset_over = |kept: &dyn Fn(f64) -> bool| -> (f64, f64, f64) {
        let (mut offset, mut weight_kept) = (0.0, 0.0);
        for &(_, normal, weight) in samples {
            let along = normal.dot(direction);
            if kept(along) {
                offset += weight * along;
                weight_kept += weight;
            }
        }
        if weight_kept <= 0.0 {
            return (0.0, f64::INFINITY, 0.0);
        }
        offset /= weight_kept;
        let mut scatter = 0.0;
        for &(_, normal, weight) in samples {
            let along = normal.dot(direction);
            if kept(along) {
                let deviation = along - offset;
                scatter += weight * deviation * deviation;
            }
        }
        (offset, (scatter / weight_kept).sqrt(), weight_kept)
    };
    let (offset, _, _) = offset_over(&|_| true);
    // A wall's claimed faces drag their own rounded borders along, and
    // a border normal leans as far as forty-five degrees — judged
    // untrimmed, four clean walls read as a five-millimetre residual.
    // Judge the core and require it to be most of the material: a
    // genuinely scattered sheet stays broad after any trim and still
    // refuses honestly.
    let mut deviations: Vec<f64> = samples
        .iter()
        .map(|&(_, normal, _)| (normal.dot(direction) - offset).abs())
        .collect();
    deviations.sort_by(f64::total_cmp);
    let median = deviations[deviations.len() / 2];
    let cut = (3.5 * median).max(0.05);
    let (offset, scatter, weight_kept) = offset_over(&|along: f64| (along - offset).abs() <= cut);
    if weight_kept < 0.6 * weight_sum {
        return None;
    }
    let residual = scatter * mean_radius;
    let draft = offset.clamp(-1.0, 1.0).asin().abs();
    Some((direction, draft, residual))
}

pub fn fit_motion(samples: &[(Point3, Vector3, f64)]) -> Option<MotionFit> {
    if samples.len() < 12 {
        return None;
    }
    let mut weight_sum = 0.0;
    // A = sum w n̄ n̄ᵀ, B = sum w n̄ nᵀ, C = sum w n nᵀ.
    let mut a = [[0.0f64; 3]; 3];
    let mut b = [[0.0f64; 3]; 3];
    let mut c = [[0.0f64; 3]; 3];
    let mut centroid = Vector3::new(0.0, 0.0, 0.0);
    for &(point, _, weight) in samples {
        centroid = centroid + Vector3::new(point.x, point.y, point.z) * weight;
        weight_sum += weight;
    }
    if weight_sum <= 0.0 {
        return None;
    }
    centroid = centroid / weight_sum;
    let origin = Point3::new(centroid.x, centroid.y, centroid.z);
    // Moments are taken about the sample centroid: referring them to a
    // far-away world origin swamps the fit with a huge common offset.
    let mut radius_sum = 0.0;
    for &(point, normal, weight) in samples {
        let offset = point - origin;
        let moment = offset.cross(normal);
        radius_sum += weight * offset.length();
        let (n, m) = (normal, moment);
        let nn = [n.x, n.y, n.z];
        let mm = [m.x, m.y, m.z];
        for i in 0..3 {
            for j in 0..3 {
                a[i][j] += weight * mm[i] * mm[j];
                b[i][j] += weight * mm[i] * nn[j];
                c[i][j] += weight * nn[i] * nn[j];
            }
        }
    }
    let mean_radius = (radius_sum / weight_sum).max(1e-6);

    // Translation: c = 0, so the condition collapses to c̄ · n = 0 and
    // the direction is the least-explained axis of the normal cloud.
    let (values, vectors) = eigen_symmetric(c);
    let direction = Vector3::new(vectors[0][0], vectors[0][1], vectors[0][2]);
    // A translation is only determined when the normals span two
    // dimensions. On a single plane they span one, every direction lying
    // in the plane sweeps it, and the eigenvector returned is whichever
    // way the arithmetic happened to fall — a confident-looking answer to
    // a question with no unique answer. Two near-equal smallest
    // eigenvalues are exactly that case.
    let translation_determined = (values[1] - values[0]) / weight_sum > 0.05;
    let mut offset = 0.0;
    for &(_, normal, weight) in samples {
        offset += weight * normal.dot(direction);
    }
    offset /= weight_sum;
    // The mean of n·direction is the sine of the draft angle; the scatter
    // about it is the fit's quality. Splitting them matters — a drafted
    // wall is a good extrusion with a non-zero mean, not a bad one.
    let mut scatter = 0.0;
    for &(_, normal, weight) in samples {
        let deviation = normal.dot(direction) - offset;
        scatter += weight * deviation * deviation;
    }
    let translation_residual = if translation_determined {
        (scatter / weight_sum).sqrt() * mean_radius
    } else {
        f64::INFINITY
    };

    // Rotation or helical: minimise over c̄ in closed form, leaving a
    // symmetric 3×3 in c alone (the Schur complement).
    let ridge = 1e-9 * weight_sum;
    let rotational = inverse_ridged(c, ridge).map(|c_inv| {
        let mut schur = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                let mut sum = 0.0;
                for k in 0..3 {
                    for l in 0..3 {
                        sum += b[i][k] * c_inv[k][l] * b[j][l];
                    }
                }
                schur[i][j] = a[i][j] - sum;
            }
        }
        let (values, vectors) = eigen_symmetric(schur);
        let axis = Vector3::new(vectors[0][0], vectors[0][1], vectors[0][2]);
        let mut rhs = Vector3::new(0.0, 0.0, 0.0);
        for i in 0..3 {
            let column = Vector3::new(b[0][i], b[1][i], b[2][i]);
            let value = column.dot(axis);
            match i {
                0 => rhs.x = value,
                1 => rhs.y = value,
                _ => rhs.z = value,
            }
        }
        let moment = apply(&c_inv, rhs) * -1.0;
        let residual = (values[0].max(0.0) / weight_sum).sqrt();
        (axis, moment, residual)
    });

    // Whichever explains the surface more tightly wins. Both residuals
    // are millimetres by construction, so the comparison is direct.
    let translation = Motion::Translation {
        direction,
        draft: offset.clamp(-1.0, 1.0).asin().abs(),
    };
    match rotational {
        Some((axis, moment, residual)) if residual < translation_residual => {
            let pitch = axis.dot(moment);
            let line_moment = moment - axis * pitch;
            let closest = axis.cross(line_moment);
            let point = Point3::new(
                origin.x + closest.x,
                origin.y + closest.y,
                origin.z + closest.z,
            );
            // A pitch far below the sampled scale is a rotation; the
            // helical reading only means something once a turn advances
            // the surface appreciably.
            let motion = if pitch.abs() < 0.02 * mean_radius {
                Motion::Rotation { point, axis }
            } else {
                Motion::Helical { point, axis, pitch }
            };
            Some(MotionFit { motion, residual })
        }
        // Neither reading stands up: a lone plane is swept by too many
        // motions to name one, and saying so is the honest answer.
        _ if !translation_determined => None,
        _ => Some(MotionFit {
            motion: translation,
            residual: translation_residual,
        }),
    }
}

/// Classifies every substantial feature by the motion that sweeps it and
/// groups the results, so a part answers "what shapes is this made of?"
/// rather than only "which of these is a revolution about my datum?".
///
/// Shared directions are the interesting output: a cast housing's arms,
/// bolt bosses and rib webs are extrusions along a handful of common
/// directions, and reading those out — with the draft on each — is the
/// information a prismatic rebuild would need.
pub fn survey(
    mesh: &crate::mesh::TriangleMesh,
    features: &[crate::report::FeatureRecord],
    alignment: &crate::datum::DatumAlignment,
    min_area: f64,
) -> Vec<String> {
    /// Directions within this angle are the same direction.
    const CLUSTER_DEG: f64 = 6.0;
    /// Below this a wall is prismatic rather than drafted.
    const DRAFT_FLOOR_DEG: f64 = 0.3;
    let mut translations: Vec<(Vector3, f64, f64)> = Vec::new();
    let mut rotations: Vec<(Vector3, f64)> = Vec::new();
    let mut helical = 0usize;
    for feature in features {
        if feature.area < min_area
            || matches!(
                feature.surface,
                crate::segment::SurfaceClass::Freeform | crate::segment::SurfaceClass::Pattern(_)
            )
        {
            continue;
        }
        let stride = feature.faces.len().div_ceil(3000).max(1);
        let samples: Vec<(Point3, Vector3, f64)> = feature
            .faces
            .iter()
            .step_by(stride)
            .filter_map(|&face| {
                let normal = mesh.face_normal(face as usize)?;
                Some((
                    alignment
                        .transform
                        .apply_point(mesh.face_centroid(face as usize)),
                    alignment.transform.apply_vector(normal),
                    mesh.face_area(face as usize),
                ))
            })
            .collect();
        let Some(fit) = fit_motion(&samples) else {
            continue;
        };
        match fit.motion {
            Motion::Translation { direction, draft } => {
                translations.push((direction, feature.area, draft))
            }
            Motion::Rotation { axis, .. } => rotations.push((axis, feature.area)),
            Motion::Helical { .. } => helical += 1,
        }
    }
    let cluster = |items: &[(Vector3, f64, f64)]| -> Vec<(Vector3, f64, usize, f64)> {
        let limit = CLUSTER_DEG.to_radians().cos();
        let mut groups: Vec<(Vector3, f64, usize, f64)> = Vec::new();
        for &(direction, area, draft) in items {
            match groups
                .iter_mut()
                .find(|(seed, ..)| seed.dot(direction).abs() >= limit)
            {
                Some(group) => {
                    group.1 += area;
                    group.2 += 1;
                    group.3 += area * draft;
                }
                None => groups.push((direction, area, 1, area * draft)),
            }
        }
        groups.sort_by(|a, b| b.1.total_cmp(&a.1));
        groups
    };
    let mut lines = Vec::new();
    for (direction, area, count, draft_moment) in cluster(&translations).into_iter().take(6) {
        let draft = (draft_moment / area.max(1e-9)).to_degrees();
        let drafted = if draft >= DRAFT_FLOOR_DEG {
            format!(", mean draft {draft:.2} deg")
        } else {
            ", prismatic".to_owned()
        };
        lines.push(format!(
            "  extrusion along ({:+.3} {:+.3} {:+.3}): {count} feature(s), {area:.0} mm^2{drafted}",
            direction.x, direction.y, direction.z
        ));
    }
    let rotation_items: Vec<(Vector3, f64, f64)> =
        rotations.iter().map(|&(a, w)| (a, w, 0.0)).collect();
    for (axis, area, count, _) in cluster(&rotation_items).into_iter().take(4) {
        lines.push(format!(
            "  revolution about ({:+.3} {:+.3} {:+.3}): {count} feature(s), {area:.0} mm^2",
            axis.x, axis.y, axis.z
        ));
    }
    if helical > 0 {
        lines.push(format!("  helical: {helical} feature(s)"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples a surface as (point, unit normal, weight).
    fn sample<F: Fn(f64, f64) -> (Point3, Vector3)>(f: F, n: usize) -> Vec<(Point3, Vector3, f64)> {
        let mut out = Vec::new();
        for i in 0..n {
            for j in 0..n {
                let (u, v) = (i as f64 / (n - 1) as f64, j as f64 / (n - 1) as f64);
                let (p, normal) = f(u, v);
                out.push((p, normal / normal.length(), 1.0));
            }
        }
        out
    }

    #[test]
    fn recognizes_a_prismatic_extrusion() {
        // A square tube swept along +Z: four walls, no draft.
        let mut samples = Vec::new();
        for wall in 0..4 {
            let angle = wall as f64 * std::f64::consts::FRAC_PI_2;
            let (nx, ny) = (angle.cos(), angle.sin());
            samples.extend(sample(
                |u, v| {
                    let along = (u - 0.5) * 20.0;
                    let p = Point3::new(
                        nx * 10.0 - ny * along,
                        ny * 10.0 + nx * along,
                        (v - 0.5) * 40.0,
                    );
                    (p, Vector3::new(nx, ny, 0.0))
                },
                12,
            ));
        }
        let fit = fit_motion(&samples).expect("fit");
        match fit.motion {
            Motion::Translation { direction, draft } => {
                assert!(direction.z.abs() > 0.999, "direction {direction:?}");
                assert!(draft.to_degrees() < 0.5, "draft {}", draft.to_degrees());
            }
            other => panic!("expected translation, got {}", other.describe()),
        }
        assert!(fit.residual < 0.05, "residual {}", fit.residual);
    }

    #[test]
    fn measures_draft_on_a_tapered_boss() {
        // A cone frustum is a drafted round boss: 7 degrees of draft.
        let draft_true: f64 = 7.0_f64.to_radians();
        let samples = sample(
            |u, v| {
                let theta = u * std::f64::consts::TAU;
                let z = v * 30.0;
                let radius = 20.0 + z * draft_true.tan();
                let p = Point3::new(radius * theta.cos(), radius * theta.sin(), z);
                // Outward normal leans by the draft angle.
                let n = Vector3::new(
                    theta.cos() * draft_true.cos(),
                    theta.sin() * draft_true.cos(),
                    -draft_true.sin(),
                );
                (p, n)
            },
            26,
        );
        let fit = fit_motion(&samples).expect("fit");
        match fit.motion {
            Motion::Translation { direction, draft } => {
                assert!(direction.z.abs() > 0.99, "direction {direction:?}");
                assert!(
                    (draft.to_degrees() - 7.0).abs() < 0.5,
                    "draft {} deg",
                    draft.to_degrees()
                );
            }
            // A frustum is genuinely both; a rotation reading is correct
            // too, provided the axis is right.
            Motion::Rotation { axis, .. } => assert!(axis.z.abs() > 0.99),
            other => panic!("unexpected {}", other.describe()),
        }
    }

    #[test]
    fn recognizes_a_revolution() {
        // A torus band: only a rotation sweeps it.
        let samples = sample(
            |u, v| {
                let (theta, phi) = (u * std::f64::consts::TAU, v * std::f64::consts::TAU);
                let (major, minor) = (30.0, 6.0);
                let radius = major + minor * phi.cos();
                let p = Point3::new(
                    radius * theta.cos(),
                    radius * theta.sin(),
                    minor * phi.sin(),
                );
                let n = Vector3::new(phi.cos() * theta.cos(), phi.cos() * theta.sin(), phi.sin());
                (p, n)
            },
            30,
        );
        let fit = fit_motion(&samples).expect("fit");
        match fit.motion {
            Motion::Rotation { axis, point } => {
                assert!(axis.z.abs() > 0.99, "axis {axis:?}");
                assert!(point.x.hypot(point.y) < 0.5, "axis offset {point:?}");
            }
            other => panic!("expected rotation, got {}", other.describe()),
        }
        assert!(fit.residual < 0.2, "residual {}", fit.residual);
    }
}

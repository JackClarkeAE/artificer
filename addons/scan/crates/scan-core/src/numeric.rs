//! Small dense linear algebra used by registration and primitive fitting.

/// Eigendecomposition of a symmetric 3x3 matrix via cyclic Jacobi rotations.
///
/// Returns eigenvalues in ascending order; `vectors[i]` is the unit
/// eigenvector paired with `values[i]`.
pub fn sym_eigen_3x3(matrix: [[f64; 3]; 3]) -> ([f64; 3], [[f64; 3]; 3]) {
    let mut a = matrix;
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..64 {
        let off = a[0][1] * a[0][1] + a[0][2] * a[0][2] + a[1][2] * a[1][2];
        let diag = a[0][0] * a[0][0] + a[1][1] * a[1][1] + a[2][2] * a[2][2];
        if off <= f64::EPSILON * f64::EPSILON * (diag + f64::MIN_POSITIVE) {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            let apq = a[p][q];
            if apq.abs() < 1e-300 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for row in &mut a {
                let akp = row[p];
                let akq = row[q];
                row[p] = c * akp - s * akq;
                row[q] = s * akp + c * akq;
            }
            let (head, tail) = a.split_at_mut(q);
            for (apk, aqk) in head[p].iter_mut().zip(tail[0].iter_mut()) {
                let old_p = *apk;
                let old_q = *aqk;
                *apk = c * old_p - s * old_q;
                *aqk = s * old_p + c * old_q;
            }
            for row in &mut v {
                let vkp = row[p];
                let vkq = row[q];
                row[p] = c * vkp - s * vkq;
                row[q] = s * vkp + c * vkq;
            }
        }
    }
    let mut order = [0usize, 1, 2];
    order.sort_by(|&i, &j| a[i][i].total_cmp(&a[j][j]));
    let values = [
        a[order[0]][order[0]],
        a[order[1]][order[1]],
        a[order[2]][order[2]],
    ];
    let mut vectors = [[0.0; 3]; 3];
    for (row, &i) in order.iter().enumerate() {
        vectors[row] = [v[0][i], v[1][i], v[2][i]];
    }
    (values, vectors)
}

/// Solves `a * x = b` by Gaussian elimination with partial pivoting.
pub fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    if a.len() != n || a.iter().any(|row| row.len() != n) {
        return None;
    }
    for col in 0..n {
        let pivot_row = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot_row][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, pivot_row);
        b.swap(col, pivot_row);
        let b_col = b[col];
        let (pivot_part, rest) = a.split_at_mut(col + 1);
        let pivot = &pivot_part[col];
        for (row, b_row) in rest.iter_mut().zip(b[col + 1..].iter_mut()) {
            let factor = row[col] / pivot[col];
            for (target, pivot_value) in row[col..].iter_mut().zip(&pivot[col..]) {
                *target -= factor * pivot_value;
            }
            *b_row -= factor * b_col;
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let mut sum = b[row];
        for col in row + 1..n {
            sum -= a[row][col] * x[col];
        }
        x[row] = sum / a[row][row];
        if !x[row].is_finite() {
            return None;
        }
    }
    Some(x)
}

/// Levenberg-Marquardt refinement with a forward-difference Jacobian.
///
/// Returns the parameter vector with the smallest observed residual
/// sum-of-squares, so a diverging step can never make the result worse
/// than the initial guess.
pub fn refine_least_squares(
    initial: Vec<f64>,
    residuals: impl Fn(&[f64]) -> Vec<f64>,
    iterations: usize,
) -> Vec<f64> {
    let mut params = initial;
    let mut current = residuals(&params);
    let mut current_ss: f64 = current.iter().map(|r| r * r).sum();
    let mut lambda = 1e-6;
    let n = params.len();
    for _ in 0..iterations {
        let m = current.len();
        if m == 0 {
            break;
        }
        let mut jacobian = vec![vec![0.0; n]; m];
        for j in 0..n {
            let step = 1e-7 * params[j].abs().max(1e-2);
            let mut bumped = params.clone();
            bumped[j] += step;
            let shifted = residuals(&bumped);
            if shifted.len() != m {
                return params;
            }
            for i in 0..m {
                jacobian[i][j] = (shifted[i] - current[i]) / step;
            }
        }
        let mut jtj = vec![vec![0.0; n]; n];
        let mut jtr = vec![0.0; n];
        for i in 0..m {
            for a in 0..n {
                jtr[a] += jacobian[i][a] * current[i];
                for b in 0..n {
                    jtj[a][b] += jacobian[i][a] * jacobian[i][b];
                }
            }
        }
        let mut improved = false;
        for _attempt in 0..8 {
            let mut damped = jtj.clone();
            for (d, row) in damped.iter_mut().enumerate() {
                row[d] += lambda * (jtj[d][d].abs() + 1e-12);
            }
            let rhs: Vec<f64> = jtr.iter().map(|v| -v).collect();
            let Some(delta) = solve_linear(damped, rhs) else {
                lambda *= 10.0;
                continue;
            };
            let trial: Vec<f64> = params
                .iter()
                .zip(&delta)
                .map(|(p, d)| p + d)
                .collect();
            let trial_res = residuals(&trial);
            let trial_ss: f64 = trial_res.iter().map(|r| r * r).sum();
            if trial_ss.is_finite() && trial_ss < current_ss {
                params = trial;
                current = trial_res;
                current_ss = trial_ss;
                lambda = (lambda * 0.3).max(1e-12);
                improved = true;
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eigen_recovers_known_spectrum() {
        // diag(1, 2, 3) conjugated by a rotation keeps its spectrum.
        let (s, c) = (0.6f64, 0.8f64);
        let r = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
        let d = [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]];
        let mut rd = [[0.0; 3]; 3];
        let mut m = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    rd[i][j] += r[i][k] * d[k][j];
                }
            }
        }
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    m[i][j] += rd[i][k] * r[j][k];
                }
            }
        }
        let (values, vectors) = sym_eigen_3x3(m);
        assert!((values[0] - 1.0).abs() < 1e-12);
        assert!((values[1] - 2.0).abs() < 1e-12);
        assert!((values[2] - 3.0).abs() < 1e-12);
        for vector in vectors {
            let len: f64 = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
            assert!((len - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn solves_small_system() {
        let a = vec![vec![2.0, 1.0], vec![1.0, 3.0]];
        let x = solve_linear(a, vec![5.0, 10.0]).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-12);
        assert!((x[1] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn refinement_reaches_quadratic_minimum() {
        let refined = refine_least_squares(
            vec![10.0, -4.0],
            |p| vec![p[0] - 3.0, 2.0 * (p[1] - 1.0)],
            25,
        );
        assert!((refined[0] - 3.0).abs() < 1e-6);
        assert!((refined[1] - 1.0).abs() < 1e-6);
    }
}

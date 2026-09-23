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
            let trial: Vec<f64> = params.iter().zip(&delta).map(|(p, d)| p + d).collect();
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

/// A symmetric positive-definite matrix held as its lower band, solved
/// by Cholesky factorization inside that band.
///
/// Least-squares systems over local bases — a B-spline's control net
/// is the case here — couple each unknown only to its near neighbours,
/// so every non-zero sits within a fixed distance of the diagonal and
/// the factor never fills in outside it. That makes the solve
/// `O(n b^2)` rather than the `O(n^3)` of [`solve_linear`], which is
/// the difference between milliseconds and seconds at a thousand
/// unknowns.
#[derive(Clone, Debug)]
pub struct BandedSpd {
    n: usize,
    band: usize,
    /// Row `i` holds columns `i - band ..= i`, left to right.
    data: Vec<f64>,
}

impl BandedSpd {
    pub fn new(n: usize, band: usize) -> Self {
        Self {
            n,
            band,
            data: vec![0.0; n * (band + 1)],
        }
    }

    pub fn size(&self) -> usize {
        self.n
    }

    fn slot(&self, row: usize, column: usize) -> usize {
        row * (self.band + 1) + (column + self.band - row)
    }

    /// Adds `value` at `(i, j)` and, by symmetry, `(j, i)`. Entries
    /// outside the band are a caller error and are ignored in release.
    pub fn add(&mut self, i: usize, j: usize, value: f64) {
        let (row, column) = if i >= j { (i, j) } else { (j, i) };
        debug_assert!(
            row - column <= self.band,
            "({i}, {j}) lies outside the band"
        );
        if row - column <= self.band {
            let slot = self.slot(row, column);
            self.data[slot] += value;
        }
    }

    pub fn get(&self, i: usize, j: usize) -> f64 {
        let (row, column) = if i >= j { (i, j) } else { (j, i) };
        if row - column > self.band {
            return 0.0;
        }
        self.data[self.slot(row, column)]
    }

    /// The largest diagonal entry, for scaling a ridge.
    pub fn max_diagonal(&self) -> f64 {
        (0..self.n).map(|i| self.get(i, i)).fold(0.0, f64::max)
    }

    /// Solves `A x = b` for three right-hand sides at once (the three
    /// coordinates of a control net share one matrix). `None` when the
    /// matrix is not numerically positive definite.
    pub fn solve3(mut self, rhs: &[[f64; 3]]) -> Option<Vec<[f64; 3]>> {
        let (n, band) = (self.n, self.band);
        if rhs.len() != n {
            return None;
        }
        let scale = self.max_diagonal();
        if !(scale.is_finite() && scale > 0.0) {
            return None;
        }
        // In-place Cholesky, lower factor within the band.
        for j in 0..n {
            let start = j.saturating_sub(band);
            let mut pivot = self.data[self.slot(j, j)];
            for k in start..j {
                let value = self.data[self.slot(j, k)];
                pivot -= value * value;
            }
            if pivot.is_nan() || pivot <= 1e-14 * scale {
                return None;
            }
            let pivot = pivot.sqrt();
            let diagonal = self.slot(j, j);
            self.data[diagonal] = pivot;
            for i in j + 1..(j + band + 1).min(n) {
                let mut sum = self.data[self.slot(i, j)];
                for k in i.saturating_sub(band)..j {
                    sum -= self.data[self.slot(i, k)] * self.data[self.slot(j, k)];
                }
                let slot = self.slot(i, j);
                self.data[slot] = sum / pivot;
            }
        }
        // Forward then back substitution.
        let mut x: Vec<[f64; 3]> = rhs.to_vec();
        for i in 0..n {
            for k in i.saturating_sub(band)..i {
                let factor = self.data[self.slot(i, k)];
                let known = x[k];
                for (value, from) in x[i].iter_mut().zip(known) {
                    *value -= factor * from;
                }
            }
            let pivot = self.data[self.slot(i, i)];
            for value in &mut x[i] {
                *value /= pivot;
            }
        }
        for i in (0..n).rev() {
            for k in i + 1..(i + band + 1).min(n) {
                let factor = self.data[self.slot(k, i)];
                let known = x[k];
                for (value, from) in x[i].iter_mut().zip(known) {
                    *value -= factor * from;
                }
            }
            let pivot = self.data[self.slot(i, i)];
            for value in &mut x[i] {
                *value /= pivot;
            }
        }
        x.iter().flatten().all(|v| v.is_finite()).then_some(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banded_cholesky_matches_the_dense_solve() {
        // A pentadiagonal SPD matrix: 4 on the diagonal, -1 at distance
        // one, 0.25 at distance two.
        let n = 9;
        let mut banded = BandedSpd::new(n, 2);
        let mut dense = vec![vec![0.0; n]; n];
        for i in 0..n {
            banded.add(i, i, 4.0);
            dense[i][i] = 4.0;
            if i + 1 < n {
                banded.add(i + 1, i, -1.0);
                dense[i + 1][i] = -1.0;
                dense[i][i + 1] = -1.0;
            }
            if i + 2 < n {
                banded.add(i, i + 2, 0.25);
                dense[i + 2][i] = 0.25;
                dense[i][i + 2] = 0.25;
            }
        }
        let rhs: Vec<[f64; 3]> = (0..n).map(|i| [i as f64, 1.0, (i as f64).sin()]).collect();
        let x = banded.solve3(&rhs).expect("positive definite");
        for column in 0..3 {
            let b: Vec<f64> = rhs.iter().map(|r| r[column]).collect();
            let expected = solve_linear(dense.clone(), b).expect("dense");
            for i in 0..n {
                assert!((x[i][column] - expected[i]).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn a_banded_matrix_that_is_not_positive_definite_is_refused() {
        let mut banded = BandedSpd::new(3, 1);
        banded.add(0, 0, 1.0);
        banded.add(1, 1, 1.0);
        banded.add(1, 0, 2.0);
        banded.add(2, 2, 1.0);
        assert!(banded.solve3(&[[1.0; 3]; 3]).is_none());
    }

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

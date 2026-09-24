//! The one linear solver: preconditioned conjugate gradients, matrix-free.
//!
//! Nothing is assembled. The operator is asked for `K x` and answers it
//! element by element, so the memory is the fields and the element table
//! rather than a sparse matrix, and the same loop serves elasticity, heat,
//! and every iteration of an optimisation. The preconditioner is Jacobi —
//! the assembled diagonal — which is cheap, deterministic, and enough for a
//! grid of well-shaped cubes.
//!
//! The loop is single-threaded and walks its elements in one order, so the
//! same input gives bit-identical output every run. It checks its
//! cancellation token every iteration and reports its residual, so a
//! caller can show a bar and stop it.

use artificer_kernel::CancellationToken;

/// A linear operator the solver can apply without seeing its matrix.
pub trait Operator {
    /// How many degrees of freedom the operator acts on.
    fn dofs(&self) -> usize;
    /// `y = K x`, over every degree of freedom.
    fn apply(&self, x: &[f64], y: &mut [f64]);
    /// The diagonal of `K`, one entry per degree of freedom, for the
    /// preconditioner.
    fn diagonal(&self) -> &[f64];
}

/// Where a solve has got to, for a progress bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    /// What the solver is doing, in a word or two.
    pub phase: &'static str,
    pub done: usize,
    pub total: usize,
    /// The relative residual `‖r‖ / ‖f‖` at this point, or `NaN` before
    /// the first iteration.
    pub residual: f64,
}

/// How a solve ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolveOutcome {
    pub iterations: usize,
    /// The relative residual `‖r‖ / ‖f‖` the solver stopped at.
    pub residual: f64,
    pub converged: bool,
    pub cancelled: bool,
}

/// Solves `K x = rhs` by Jacobi-preconditioned conjugate gradients on the
/// free degrees of freedom, leaving the fixed ones at whatever `x` holds.
///
/// `rhs` is read at the free degrees of freedom only, so a caller applying
/// prescribed values folds them into the right-hand side first. The solve
/// stops when the relative residual falls under `tolerance`, when it has
/// run `max_iterations`, or when it is cancelled; the outcome says which.
#[allow(clippy::too_many_arguments)]
pub fn conjugate_gradients(
    operator: &dyn Operator,
    rhs: &[f64],
    free: &[bool],
    x: &mut [f64],
    tolerance: f64,
    max_iterations: usize,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
) -> SolveOutcome {
    let n = operator.dofs();
    debug_assert_eq!(rhs.len(), n);
    debug_assert_eq!(free.len(), n);
    debug_assert_eq!(x.len(), n);
    let diagonal = operator.diagonal();
    let inverse_diagonal = diagonal
        .iter()
        .zip(free)
        .map(|(value, free)| {
            if *free && value.is_finite() && *value > 0.0 {
                1.0 / value
            } else {
                0.0
            }
        })
        .collect::<Vec<_>>();

    let mut q = vec![0.0; n];
    // What drives the system is the right-hand side less what the
    // prescribed values push on the free dofs: a bar held at two
    // temperatures has no source term at all, and is driven entirely by
    // its ends. The residual is measured against that, not against `rhs`.
    let prescribed = x
        .iter()
        .zip(free)
        .map(|(value, free)| if *free { 0.0 } else { *value })
        .collect::<Vec<_>>();
    operator.apply(&prescribed, &mut q);
    let effective = rhs
        .iter()
        .zip(&q)
        .zip(free)
        .map(|((rhs, kx), free)| if *free { rhs - kx } else { 0.0 })
        .collect::<Vec<_>>();
    let rhs_norm = norm(&effective, free);
    // r = rhs − K x on the free dofs, from wherever `x` starts.
    operator.apply(x, &mut q);
    let mut r = rhs
        .iter()
        .zip(&q)
        .zip(free)
        .map(|((rhs, kx), free)| if *free { rhs - kx } else { 0.0 })
        .collect::<Vec<_>>();
    if rhs_norm == 0.0 {
        progress(Progress {
            phase: "solved",
            done: 0,
            total: max_iterations,
            residual: 0.0,
        });
        return SolveOutcome {
            iterations: 0,
            residual: 0.0,
            converged: true,
            cancelled: false,
        };
    }
    let mut z = precondition(&r, &inverse_diagonal);
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut residual = norm(&r, free) / rhs_norm;
    if residual <= tolerance {
        return SolveOutcome {
            iterations: 0,
            residual,
            converged: true,
            cancelled: false,
        };
    }

    let mut iterations = 0;
    let mut converged = false;
    let mut cancelled = false;
    while iterations < max_iterations {
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
        operator.apply(&p, &mut q);
        for (value, free) in q.iter_mut().zip(free) {
            if !*free {
                *value = 0.0;
            }
        }
        let pq = dot(&p, &q);
        if !(pq.is_finite() && pq > 0.0) {
            // A direction with no energy: the operator is singular on the
            // free dofs, which a rigid-body mode is. Stop rather than divide.
            break;
        }
        let alpha = rz / pq;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * q[i];
        }
        iterations += 1;
        residual = norm(&r, free) / rhs_norm;
        if iterations % 20 == 0 {
            progress(Progress {
                phase: "solving",
                done: iterations,
                total: max_iterations,
                residual,
            });
        }
        if residual <= tolerance {
            converged = true;
            break;
        }
        z = precondition(&r, &inverse_diagonal);
        let rz_next = dot(&r, &z);
        let beta = rz_next / rz;
        rz = rz_next;
        for i in 0..n {
            p[i] = beta.mul_add(p[i], z[i]);
        }
    }
    progress(Progress {
        phase: if converged { "solved" } else { "stopped" },
        done: iterations,
        total: max_iterations,
        residual,
    });
    SolveOutcome {
        iterations,
        residual,
        converged,
        cancelled,
    }
}

fn precondition(r: &[f64], inverse_diagonal: &[f64]) -> Vec<f64> {
    r.iter()
        .zip(inverse_diagonal)
        .map(|(r, inverse)| r * inverse)
        .collect()
}

/// The dot product, accumulated in one fixed order.
#[must_use]
pub fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).fold(0.0, |sum, (a, b)| a.mul_add(*b, sum))
}

fn norm(values: &[f64], free: &[bool]) -> f64 {
    values
        .iter()
        .zip(free)
        .filter(|(_, free)| **free)
        .fold(0.0, |sum, (value, _)| value.mul_add(*value, sum))
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small dense symmetric positive-definite matrix, applied directly.
    struct Dense {
        matrix: Vec<Vec<f64>>,
        diagonal: Vec<f64>,
    }

    impl Operator for Dense {
        fn dofs(&self) -> usize {
            self.matrix.len()
        }

        fn apply(&self, x: &[f64], y: &mut [f64]) {
            for (row, out) in self.matrix.iter().zip(y.iter_mut()) {
                *out = dot(row, x);
            }
        }

        fn diagonal(&self) -> &[f64] {
            &self.diagonal
        }
    }

    #[test]
    fn solves_a_small_system_and_leaves_fixed_dofs_alone() {
        let matrix = vec![
            vec![4.0, 1.0, 0.0, 0.0],
            vec![1.0, 5.0, 1.0, 0.0],
            vec![0.0, 1.0, 6.0, 1.0],
            vec![0.0, 0.0, 1.0, 7.0],
        ];
        let diagonal = (0..4).map(|i| matrix[i][i]).collect();
        let operator = Dense { matrix, diagonal };
        let rhs = vec![1.0, 2.0, 3.0, 4.0];
        let free = vec![true, true, true, false];
        let mut x = vec![0.0, 0.0, 0.0, 0.5];
        let outcome = conjugate_gradients(
            &operator,
            &rhs,
            &free,
            &mut x,
            1.0e-12,
            100,
            &CancellationToken::default(),
            &mut |_| {},
        );
        assert!(outcome.converged, "{outcome:?}");
        assert_eq!(x[3], 0.5, "a fixed dof keeps its prescribed value");
        // Check the free rows of K x = rhs, with x[3] prescribed.
        let mut kx = vec![0.0; 4];
        operator.apply(&x, &mut kx);
        for i in 0..3 {
            assert!((kx[i] - rhs[i]).abs() < 1.0e-9, "row {i}: {}", kx[i]);
        }
    }

    #[test]
    fn a_cancelled_solve_says_so() {
        let operator = Dense {
            matrix: vec![vec![2.0, 1.0], vec![1.0, 2.0]],
            diagonal: vec![2.0, 2.0],
        };
        let token = CancellationToken::default();
        token.cancel();
        let mut x = vec![0.0; 2];
        let outcome = conjugate_gradients(
            &operator,
            &[1.0, 1.0],
            &[true, true],
            &mut x,
            1.0e-12,
            100,
            &token,
            &mut |_| {},
        );
        assert!(outcome.cancelled);
        assert!(!outcome.converged);
        assert_eq!(outcome.iterations, 0);
    }
}

//! The one element every study is built from: the eight-node trilinear
//! hexahedron, one per solid voxel.
//!
//! The stiffness is the standard formulation of the topology-optimisation
//! codes — `top88` (Andreassen, Clausen, Schevenels, Lazarov and Sigmund,
//! "Efficient topology optimization in MATLAB using 88 lines of code",
//! Struct Multidisc Optim 43, 2011) in two dimensions and its
//! three-dimensional sibling `top3d` (Liu and Tovar, "An efficient 3D
//! topology optimization code written in Matlab", Struct Multidisc Optim
//! 50, 2014): a unit cube with full 2 × 2 × 2 Gauss integration of
//! `Bᵀ D B`, isotropic linear elasticity, computed once for a unit modulus
//! and scaled per element. Those codes tabulate the result by hand; this
//! one integrates it, which is the same matrix without the transcription.
//!
//! Because the integrand scales as `1/h²` and the volume as `h³`, the
//! matrix for a cell of side `h` is `h` times the unit one, so one table
//! serves every resolution.
//!
//! ## Node order
//!
//! Counter-clockwise round the bottom face, then the same round the top:
//! `(0,0,0) (1,0,0) (1,1,0) (0,1,0) (0,0,1) (1,0,1) (1,1,1) (0,1,1)`, as
//! offsets from the cell's minimum corner. Every table in this module is in
//! that order.

/// The corner each element node sits at, as a step from the cell's minimum
/// corner along x, y and z.
pub const NODE_OFFSETS: [[usize; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 1, 0],
    [0, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [1, 1, 1],
    [0, 1, 1],
];

/// The four nodes on one side of the element, for each of the six sides in
/// the order `-x +x -y +y -z +z`.
pub const SIDE_NODES: [[usize; 4]; 6] = [
    [0, 3, 4, 7],
    [1, 2, 5, 6],
    [0, 1, 4, 5],
    [2, 3, 6, 7],
    [0, 1, 2, 3],
    [4, 5, 6, 7],
];

/// The outward step of each side, in the order of [`SIDE_NODES`].
pub const SIDE_STEPS: [[i8; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
];

/// The side whose outward step this is, if it is one.
#[must_use]
pub fn side_of_step(step: [i8; 3]) -> Option<usize> {
    SIDE_STEPS.iter().position(|held| *held == step)
}

/// The 24 × 24 stiffness of a unit cube of unit modulus, for one Poisson
/// ratio. Multiply by `E · h` for a cell of side `h`.
pub type Stiffness = [[f64; 24]; 24];

/// The 8 × 8 conductivity of a unit cube of unit conductivity. Multiply by
/// `k · h` for a cell of side `h`.
pub type Conductivity = [[f64; 8]; 8];

const GAUSS: [f64; 2] = [-0.577_350_269_189_625_8, 0.577_350_269_189_625_8];

/// The natural coordinate of each node: `-1` or `+1` per axis.
fn natural(node: usize) -> [f64; 3] {
    NODE_OFFSETS[node].map(|offset| 2.0 * offset as f64 - 1.0)
}

/// The derivatives of the eight shape functions with respect to x, y and z
/// at one natural point, for a unit cube (so `dN/dx = 2 dN/dξ`).
fn shape_gradients(point: [f64; 3]) -> [[f64; 3]; 8] {
    let mut gradients = [[0.0; 3]; 8];
    for (node, gradient) in gradients.iter_mut().enumerate() {
        let [xi, eta, zeta] = natural(node);
        let [x, y, z] = point;
        *gradient = [
            2.0 * 0.125 * xi * (1.0 + eta * y) * (1.0 + zeta * z),
            2.0 * 0.125 * eta * (1.0 + xi * x) * (1.0 + zeta * z),
            2.0 * 0.125 * zeta * (1.0 + xi * x) * (1.0 + eta * y),
        ];
    }
    gradients
}

/// The isotropic elasticity matrix for a unit modulus, over the strain
/// ordering `εx εy εz γxy γyz γxz`.
#[must_use]
pub fn elasticity(poisson_ratio: f64) -> [[f64; 6]; 6] {
    let nu = poisson_ratio;
    let lambda = nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = 1.0 / (2.0 * (1.0 + nu));
    let mut d = [[0.0; 6]; 6];
    for row in 0..3 {
        for column in 0..3 {
            d[row][column] = if row == column {
                lambda + 2.0 * mu
            } else {
                lambda
            };
        }
    }
    for shear in 3..6 {
        d[shear][shear] = mu;
    }
    d
}

/// The strain-displacement matrix at one natural point of a unit cube.
fn strain_displacement(point: [f64; 3]) -> [[f64; 24]; 6] {
    let gradients = shape_gradients(point);
    let mut b = [[0.0; 24]; 6];
    for (node, [dx, dy, dz]) in gradients.into_iter().enumerate() {
        let column = 3 * node;
        b[0][column] = dx;
        b[1][column + 1] = dy;
        b[2][column + 2] = dz;
        b[3][column] = dy;
        b[3][column + 1] = dx;
        b[4][column + 1] = dz;
        b[4][column + 2] = dy;
        b[5][column] = dz;
        b[5][column + 2] = dx;
    }
    b
}

/// The unit-cube, unit-modulus stiffness for one Poisson ratio.
#[must_use]
pub fn hex_stiffness(poisson_ratio: f64) -> Stiffness {
    let d = elasticity(poisson_ratio);
    let mut ke = [[0.0; 24]; 24];
    // The Jacobian of a unit cube in natural coordinates is 1/2 per axis.
    let volume_weight = 0.125;
    for &x in &GAUSS {
        for &y in &GAUSS {
            for &z in &GAUSS {
                let b = strain_displacement([x, y, z]);
                // D B, then Bᵀ (D B), accumulated.
                let mut db = [[0.0; 24]; 6];
                for row in 0..6 {
                    for column in 0..24 {
                        let mut sum = 0.0;
                        for inner in 0..6 {
                            sum += d[row][inner] * b[inner][column];
                        }
                        db[row][column] = sum;
                    }
                }
                for row in 0..24 {
                    for column in 0..24 {
                        let mut sum = 0.0;
                        for inner in 0..6 {
                            sum += b[inner][row] * db[inner][column];
                        }
                        ke[row][column] += sum * volume_weight;
                    }
                }
            }
        }
    }
    // Symmetrise bit-exactly: the products above are symmetric in exact
    // arithmetic and the solver's convergence theory wants them so in
    // floating point too.
    for row in 0..24 {
        for column in row + 1..24 {
            let average = f64::midpoint(ke[row][column], ke[column][row]);
            ke[row][column] = average;
            ke[column][row] = average;
        }
    }
    ke
}

/// The unit-cube, unit-conductivity conduction matrix.
#[must_use]
pub fn hex_conductivity() -> Conductivity {
    let mut kc = [[0.0; 8]; 8];
    let volume_weight = 0.125;
    for &x in &GAUSS {
        for &y in &GAUSS {
            for &z in &GAUSS {
                let gradients = shape_gradients([x, y, z]);
                for row in 0..8 {
                    for column in 0..8 {
                        let dot = gradients[row][0] * gradients[column][0]
                            + gradients[row][1] * gradients[column][1]
                            + gradients[row][2] * gradients[column][2];
                        kc[row][column] += dot * volume_weight;
                    }
                }
            }
        }
    }
    kc
}

/// The six strain components at the centre of an element of side `h`, from
/// its 24 nodal displacements.
#[must_use]
pub fn centre_strain(displacements: &[f64; 24], h: f64) -> [f64; 6] {
    let b = strain_displacement([0.0, 0.0, 0.0]);
    let mut strain = [0.0; 6];
    for (row, component) in strain.iter_mut().enumerate() {
        let mut sum = 0.0;
        for column in 0..24 {
            sum += b[row][column] * displacements[column];
        }
        // The unit-cube gradients are `2 dN/dξ`; a cell of side `h` has
        // `(2/h) dN/dξ`.
        *component = sum / h;
    }
    strain
}

/// The stress a strain produces in a material of the given modulus and
/// Poisson ratio.
#[must_use]
pub fn stress(strain: [f64; 6], youngs_modulus: f64, poisson_ratio: f64) -> [f64; 6] {
    let d = elasticity(poisson_ratio);
    let mut stress = [0.0; 6];
    for (row, component) in stress.iter_mut().enumerate() {
        let mut sum = 0.0;
        for column in 0..6 {
            sum += d[row][column] * strain[column];
        }
        *component = sum * youngs_modulus;
    }
    stress
}

/// The von Mises equivalent of a stress tensor `σx σy σz τxy τyz τxz`.
#[must_use]
pub fn von_mises(stress: [f64; 6]) -> f64 {
    let [sx, sy, sz, txy, tyz, txz] = stress;
    let normal = (sx - sy).powi(2) + (sy - sz).powi(2) + (sz - sx).powi(2);
    let shear = txy.powi(2) + tyz.powi(2) + txz.powi(2);
    (0.5 * normal + 3.0 * shear).max(0.0).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rigid_translation() -> [f64; 24] {
        let mut u = [0.0; 24];
        for node in 0..8 {
            u[3 * node] = 0.3;
            u[3 * node + 1] = -0.2;
            u[3 * node + 2] = 0.7;
        }
        u
    }

    #[test]
    fn stiffness_is_symmetric_and_annihilates_rigid_translation() {
        let ke = hex_stiffness(0.3);
        for row in 0..24 {
            for column in 0..24 {
                assert_eq!(ke[row][column], ke[column][row]);
            }
        }
        let u = rigid_translation();
        for row in 0..24 {
            let force: f64 = (0..24).map(|column| ke[row][column] * u[column]).sum();
            assert!(force.abs() < 1.0e-12, "row {row} carries {force}");
        }
    }

    /// Uniform extension in x must produce the uniaxial-strain stress state
    /// `σx = (λ + 2μ) ε`, `σy = σz = λ ε`.
    #[test]
    fn uniform_extension_recovers_the_constitutive_law() {
        let nu = 0.3;
        let e = 200_000.0;
        let strain_x = 1.0e-3;
        let mut u = [0.0; 24];
        for node in 0..8 {
            u[3 * node] = NODE_OFFSETS[node][0] as f64 * strain_x * 2.0;
        }
        let strain = centre_strain(&u, 2.0);
        assert!((strain[0] - strain_x).abs() < 1.0e-15);
        assert!(strain[1..].iter().all(|value| value.abs() < 1.0e-15));
        let sigma = stress(strain, e, nu);
        let lambda = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
        let mu = e / (2.0 * (1.0 + nu));
        assert!((sigma[0] - (lambda + 2.0 * mu) * strain_x).abs() < 1.0e-9);
        assert!((sigma[1] - lambda * strain_x).abs() < 1.0e-9);
        assert!((sigma[2] - lambda * strain_x).abs() < 1.0e-9);
        // And the element's own stiffness agrees with the analytic energy
        // `½ V ε D ε` for this state.
        let ke = hex_stiffness(nu);
        let mut energy = 0.0;
        for row in 0..24 {
            for column in 0..24 {
                energy += u[row] * ke[row][column] * u[column];
            }
        }
        energy *= 0.5 * e * 2.0;
        let expected = 0.5 * 8.0 * (lambda + 2.0 * mu) * strain_x * strain_x;
        assert!((energy - expected).abs() < 1.0e-9, "{energy} vs {expected}");
    }

    #[test]
    fn conductivity_rows_sum_to_zero_and_a_linear_field_conducts_uniformly() {
        let kc = hex_conductivity();
        for row in 0..8 {
            let sum: f64 = kc[row].iter().sum();
            assert!(sum.abs() < 1.0e-12);
            for column in 0..8 {
                assert!((kc[row][column] - kc[column][row]).abs() < 1.0e-15);
            }
        }
        // A unit gradient along x: the flux through the -x nodes is the
        // negative of that through the +x nodes, and their sum is the cube's
        // conductance, 1.
        let temperature: [f64; 8] = std::array::from_fn(|node| NODE_OFFSETS[node][0] as f64);
        let flux: Vec<f64> = (0..8)
            .map(|row| {
                (0..8)
                    .map(|column| kc[row][column] * temperature[column])
                    .sum()
            })
            .collect();
        let into_hot: f64 = SIDE_NODES[1].iter().map(|node| flux[*node]).sum();
        assert!((into_hot - 1.0).abs() < 1.0e-12, "{into_hot}");
    }

    #[test]
    fn von_mises_of_pure_shear_and_uniaxial_tension() {
        assert!((von_mises([100.0, 0.0, 0.0, 0.0, 0.0, 0.0]) - 100.0).abs() < 1.0e-12);
        assert!(
            (von_mises([0.0, 0.0, 0.0, 10.0, 0.0, 0.0]) - 10.0 * 3.0_f64.sqrt()).abs() < 1.0e-12
        );
        assert_eq!(von_mises([50.0, 50.0, 50.0, 0.0, 0.0, 0.0]), 0.0);
    }
}

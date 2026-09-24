//! Steady-state heat on the same grid and the same solver: one degree of
//! freedom per node, temperatures held on some faces, the rest of the
//! surface losing heat to the air.
//!
//! The conduction matrix is the hexahedron's `∫ ∇Nᵀ k ∇N dV`, integrated
//! the way the stiffness is. Convection is lumped: every exposed side of a
//! cell that is not held at a temperature gives each of its four corners a
//! quarter of `h A` on the diagonal and `h A T∞` on the right-hand side,
//! which is the usual first approximation and is labelled as one.

use artificer_kernel::CancellationToken;
use artificer_protocol::{EntityRef, Tier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::element::{Conductivity, hex_conductivity};
use crate::material::Material;
use crate::mesh::VoxelMesh;
use crate::solver::{Operator, Progress, SolveOutcome, conjugate_gradients};
use crate::structural::default_iterations;

/// Heat lost from every exposed side that is not held at a temperature.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Convection {
    /// The film coefficient, in watts per square metre-kelvin: still air
    /// is about 10, a fan about 50.
    pub coefficient_w_m2k: f64,
    /// The air temperature, in degrees Celsius.
    pub ambient_c: f64,
}

/// A steady-state thermal study by face.
#[derive(Clone, Debug, PartialEq)]
pub struct ThermalStudy {
    pub material: Material,
    /// Faces held at temperatures, in degrees Celsius.
    pub held: Vec<(EntityRef, f64)>,
    pub convection: Option<Convection>,
    pub tolerance: f64,
    pub max_iterations: Option<usize>,
}

impl ThermalStudy {
    #[must_use]
    pub fn new(material: Material) -> Self {
        Self {
            material,
            held: Vec::new(),
            convection: None,
            tolerance: 1.0e-8,
            max_iterations: None,
        }
    }
}

/// Why a thermal study could not be run.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum ThermalError {
    #[error("the voxel grid is empty at this resolution; nothing to solve on")]
    EmptyGrid,
    #[error(
        "no face is held at a temperature and nothing loses heat: every temperature is possible"
    )]
    NoTemperatures,
    #[error("the held face {face} has no voxels on it at this resolution")]
    FaceOnNoCells { face: EntityRef },
}

/// What a thermal study found.
#[derive(Clone, Debug, PartialEq)]
pub struct ThermalResult {
    /// Every node's temperature, in degrees Celsius.
    pub temperature: Vec<f64>,
    /// Every element's temperature: the mean of its corners.
    pub element_temperature: Vec<f64>,
    pub min: f64,
    pub max: f64,
    pub voxels: usize,
    pub nodes: usize,
    pub cell: f64,
    pub held_nodes: usize,
    pub convecting_sides: usize,
    pub solve: SolveOutcome,
    pub tier: Tier,
}

/// Conduction through the mesh with lumped convection on its skin.
struct ConductionOperator<'a> {
    mesh: &'a VoxelMesh,
    kc: Conductivity,
    /// `k · h` for every element, in watts per kelvin.
    scale: f64,
    /// Lumped convection conductance per node, in watts per kelvin.
    film: Vec<f64>,
    diagonal: Vec<f64>,
}

impl Operator for ConductionOperator<'_> {
    fn dofs(&self) -> usize {
        self.mesh.node_count()
    }

    fn apply(&self, x: &[f64], y: &mut [f64]) {
        y.fill(0.0);
        for nodes in self.mesh.elements() {
            let local: [f64; 8] = std::array::from_fn(|slot| x[nodes[slot] as usize]);
            for (slot, node) in nodes.iter().enumerate() {
                let mut sum = 0.0;
                for column in 0..8 {
                    sum += self.kc[slot][column] * local[column];
                }
                y[*node as usize] += self.scale * sum;
            }
        }
        for (node, film) in self.film.iter().enumerate() {
            if *film != 0.0 {
                y[node] += film * x[node];
            }
        }
    }

    fn diagonal(&self) -> &[f64] {
        &self.diagonal
    }
}

/// Runs a thermal study on a mesh.
pub fn solve_steady_state(
    mesh: &VoxelMesh,
    study: &ThermalStudy,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
) -> Result<ThermalResult, ThermalError> {
    if mesh.element_count() == 0 {
        return Err(ThermalError::EmptyGrid);
    }
    if study.held.is_empty() && study.convection.is_none() {
        return Err(ThermalError::NoTemperatures);
    }
    let nodes = mesh.node_count();
    let mut free = vec![true; nodes];
    let mut temperature = vec![0.0; nodes];
    let mut held_sides = std::collections::BTreeSet::new();
    for (face, celsius) in &study.held {
        let sides = mesh.sides_on_face(*face);
        if sides.is_empty() {
            return Err(ThermalError::FaceOnNoCells { face: *face });
        }
        for (element, side) in sides {
            held_sides.insert((element, side));
            for node in mesh.side_nodes(element, side) {
                free[node as usize] = false;
                temperature[node as usize] = *celsius;
            }
        }
    }
    if free.iter().all(|free| *free) && study.convection.is_none() {
        return Err(ThermalError::NoTemperatures);
    }

    let kc = hex_conductivity();
    let scale = study.material.conductivity_w_mmk() * mesh.grid().cell();
    let mut film = vec![0.0; nodes];
    let mut rhs = vec![0.0; nodes];
    let mut convecting_sides = 0;
    if let Some(convection) = study.convection {
        // W/(m²K) over a side of h² mm².
        let conductance = convection.coefficient_w_m2k * 1.0e-6 * mesh.side_area();
        for element in 0..mesh.element_count() {
            for side in mesh.exposed_sides(element) {
                if held_sides.contains(&(element, side)) {
                    continue;
                }
                convecting_sides += 1;
                for node in mesh.side_nodes(element, side) {
                    film[node as usize] += conductance / 4.0;
                    rhs[node as usize] += conductance / 4.0 * convection.ambient_c;
                }
            }
        }
    }
    let mut diagonal = film.clone();
    for nodes in mesh.elements() {
        for (slot, node) in nodes.iter().enumerate() {
            diagonal[*node as usize] += scale * kc[slot][slot];
        }
    }
    let operator = ConductionOperator {
        mesh,
        kc,
        scale,
        film,
        diagonal,
    };
    let max_iterations = study
        .max_iterations
        .unwrap_or_else(|| default_iterations(nodes));
    let solve = conjugate_gradients(
        &operator,
        &rhs,
        &free,
        &mut temperature,
        study.tolerance,
        max_iterations,
        cancellation,
        progress,
    );
    let element_temperature = mesh
        .elements()
        .iter()
        .map(|nodes| {
            nodes
                .iter()
                .map(|node| temperature[*node as usize])
                .sum::<f64>()
                / 8.0
        })
        .collect::<Vec<_>>();
    let min = temperature.iter().copied().fold(f64::INFINITY, f64::min);
    let max = temperature
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    Ok(ThermalResult {
        temperature,
        element_temperature,
        min,
        max,
        voxels: mesh.element_count(),
        nodes,
        cell: mesh.grid().cell(),
        held_nodes: free.iter().filter(|free| !**free).count(),
        convecting_sides,
        solve,
        tier: Tier::Approximate,
    })
}

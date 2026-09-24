//! Topology optimisation on the voxel grid, experimental: SIMP with a
//! density filter and optimality-criteria updates, straight from `top88`
//! (Andreassen et al. 2011) with the three-dimensional element of `top3d`.
//!
//! Each element has a density in `0..=1`; its stiffness is
//! `E_min + ρᵖ (E₀ − E_min)`, so intermediate densities are penalised into
//! being either material or void. Every iteration solves the structural
//! study at the current densities, takes the compliance and its derivative
//! per element, filters both through a cone of radius `r_min` so no
//! feature thinner than the filter can form, and moves every density by
//! the optimality-criteria rule under a bisection that keeps the volume at
//! its target. The picture a caller draws is the filtered density.
//!
//! It is experimental because the solver is the study's own — a few
//! hundred conjugate-gradient iterations per step, warm-started — and the
//! result is a field of densities, not a body: nothing here is exported as
//! geometry.

use artificer_kernel::CancellationToken;
use artificer_protocol::Tier;
use thiserror::Error;

use crate::material::Material;
use crate::mesh::VoxelMesh;
use crate::solver::Progress;
use crate::structural::{Conditions, ElasticOperator, conjugate_gradients_on};

/// An optimisation as the caller sets it up.
#[derive(Clone, Debug, PartialEq)]
pub struct TopologyStudy {
    pub material: Material,
    /// The supports and loads, on the mesh's own degrees of freedom.
    pub conditions: Conditions,
    /// The fraction of the design domain to keep, in `0..1`.
    pub volume_fraction: f64,
    /// The SIMP penalty; 3 is the classic.
    pub penalty: f64,
    /// The filter radius, in cells; 1.5 is the classic.
    pub filter_radius: f64,
    /// The most iterations to take.
    pub max_iterations: usize,
    /// Stop once no density moves by more than this in one iteration.
    pub change_tolerance: f64,
    /// The move limit of the optimality-criteria update.
    pub move_limit: f64,
    /// The relative residual each iteration's solve stops at.
    pub solve_tolerance: f64,
}

impl TopologyStudy {
    /// The classic parameters over a set of conditions.
    #[must_use]
    pub fn new(material: Material, conditions: Conditions, volume_fraction: f64) -> Self {
        Self {
            material,
            conditions,
            volume_fraction,
            penalty: 3.0,
            filter_radius: 1.5,
            max_iterations: 40,
            change_tolerance: 0.01,
            move_limit: 0.2,
            solve_tolerance: 1.0e-5,
        }
    }
}

/// Why an optimisation could not be run.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum TopologyError {
    #[error("the voxel grid is empty at this resolution; nothing to optimise")]
    EmptyGrid,
    #[error("no fixed faces: the part would move as a rigid body under any load")]
    NoSupports,
    #[error("no load: nothing pushes on the part, so nothing decides where material matters")]
    NoLoad,
    #[error("the volume fraction must be between 0 and 1, not {0}")]
    VolumeFraction(f64),
}

/// Where an optimisation has got to, reported once an iteration.
#[derive(Clone, Copy, Debug)]
pub struct TopologyProgress<'a> {
    pub iteration: usize,
    pub max_iterations: usize,
    pub compliance: f64,
    /// The largest change of any density in this iteration.
    pub change: f64,
    /// The filtered density of every element, in element order.
    pub densities: &'a [f64],
    /// The last solve's iteration count and residual.
    pub solve_iterations: usize,
    pub solve_residual: f64,
}

/// What an optimisation ended with.
#[derive(Clone, Debug, PartialEq)]
pub struct TopologyResult {
    /// The filtered density of every element, in element order.
    pub densities: Vec<f64>,
    /// The compliance at every iteration, first to last.
    pub compliance_history: Vec<f64>,
    pub iterations: usize,
    /// The mean density the result actually has.
    pub volume_fraction: f64,
    pub voxels: usize,
    pub converged: bool,
    pub cancelled: bool,
    pub tier: Tier,
}

impl TopologyResult {
    /// Whether an element is material at a threshold.
    #[must_use]
    pub fn is_material(&self, element: usize, threshold: f64) -> bool {
        self.densities
            .get(element)
            .is_some_and(|density| *density >= threshold)
    }
}

/// The filter: for each element, the elements within the radius and their
/// cone weights, and the weight sum.
struct DensityFilter {
    neighbours: Vec<Vec<(u32, f64)>>,
    sums: Vec<f64>,
}

impl DensityFilter {
    fn new(mesh: &VoxelMesh, radius: f64) -> Self {
        let reach = radius.ceil() as isize;
        let count = mesh.element_count();
        let mut neighbours = Vec::with_capacity(count);
        let mut sums = Vec::with_capacity(count);
        for element in 0..count {
            let cell = mesh.cell_of_element(element);
            let mut within = Vec::new();
            let mut sum = 0.0;
            for dz in -reach..=reach {
                for dy in -reach..=reach {
                    for dx in -reach..=reach {
                        let distance = ((dx * dx + dy * dy + dz * dz) as f64).sqrt();
                        let weight = radius - distance;
                        if weight <= 0.0 {
                            continue;
                        }
                        let candidate = [
                            cell[0] as isize + dx,
                            cell[1] as isize + dy,
                            cell[2] as isize + dz,
                        ];
                        if candidate.iter().any(|value| *value < 0) {
                            continue;
                        }
                        let Some(other) =
                            mesh.element_in_cell(candidate.map(|value| value as usize))
                        else {
                            continue;
                        };
                        within.push((other as u32, weight));
                        sum += weight;
                    }
                }
            }
            neighbours.push(within);
            sums.push(sum);
        }
        Self { neighbours, sums }
    }

    /// `(H x) / Hs`: the filtered field.
    fn apply(&self, values: &[f64]) -> Vec<f64> {
        self.neighbours
            .iter()
            .zip(&self.sums)
            .map(|(within, sum)| {
                within
                    .iter()
                    .map(|(other, weight)| weight * values[*other as usize])
                    .sum::<f64>()
                    / sum
            })
            .collect()
    }

    /// `H (values / Hs)`: the chain rule of the filter applied to a
    /// sensitivity.
    fn back(&self, values: &[f64]) -> Vec<f64> {
        let scaled = values
            .iter()
            .zip(&self.sums)
            .map(|(value, sum)| value / sum)
            .collect::<Vec<_>>();
        self.neighbours
            .iter()
            .map(|within| {
                within
                    .iter()
                    .map(|(other, weight)| weight * scaled[*other as usize])
                    .sum::<f64>()
            })
            .collect()
    }
}

/// Runs the optimisation, reporting the density field once an iteration.
pub fn optimise_topology(
    mesh: &VoxelMesh,
    study: &TopologyStudy,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(TopologyProgress<'_>),
) -> Result<TopologyResult, TopologyError> {
    let count = mesh.element_count();
    if count == 0 {
        return Err(TopologyError::EmptyGrid);
    }
    if study.conditions.free.iter().all(|free| *free) {
        return Err(TopologyError::NoSupports);
    }
    if study.conditions.force.iter().all(|force| *force == 0.0) {
        return Err(TopologyError::NoLoad);
    }
    if !(study.volume_fraction > 0.0 && study.volume_fraction < 1.0) {
        return Err(TopologyError::VolumeFraction(study.volume_fraction));
    }
    let filter = DensityFilter::new(mesh, study.filter_radius.max(1.0));
    let penalty = study.penalty;
    let e_min = 1.0e-9;
    let mut x = vec![study.volume_fraction; count];
    let mut physical = filter.apply(&x);
    let mut displacements = vec![0.0; 3 * mesh.node_count()];
    let mut history = Vec::new();
    let mut converged = false;
    let mut cancelled = false;
    let mut iterations = 0;
    let dofs = 3 * mesh.node_count();
    let max_solve = crate::structural::default_iterations(dofs);

    while iterations < study.max_iterations {
        if cancellation.is_cancelled() {
            cancelled = true;
            break;
        }
        // The stiffness at these densities, and the displacements under
        // it, warm-started from the last iteration's.
        let relative = physical
            .iter()
            .map(|density| e_min + density.powf(penalty) * (1.0 - e_min))
            .collect::<Vec<_>>();
        let operator = ElasticOperator::new(mesh, &study.material, Some(&relative));
        let solve = conjugate_gradients_on(
            &operator,
            &study.conditions,
            &mut displacements,
            study.solve_tolerance,
            max_solve,
            cancellation,
            &mut |_: Progress| {},
        );
        if solve.cancelled {
            cancelled = true;
            break;
        }
        // Compliance and its sensitivity to each element's density.
        let mut compliance = 0.0;
        let mut sensitivity = Vec::with_capacity(count);
        let base = study.material.youngs_modulus_mpa * mesh.grid().cell();
        for element in 0..count {
            let energy = base * operator.element_energy(element, &displacements);
            compliance += relative[element] * energy;
            sensitivity
                .push(-penalty * physical[element].powf(penalty - 1.0) * (1.0 - e_min) * energy);
        }
        history.push(compliance);
        let filtered_sensitivity = filter.back(&sensitivity);
        let filtered_volume = filter.back(&vec![1.0; count]);

        // Optimality criteria under a bisection on the volume multiplier.
        let mut low = 0.0;
        let mut high = 1.0e9;
        let mut next = x.clone();
        let mut next_physical = physical.clone();
        while (high - low) / (high + low) > 1.0e-3 {
            let middle = f64::midpoint(low, high);
            for element in 0..count {
                let ratio = (-filtered_sensitivity[element] / filtered_volume[element] / middle)
                    .max(0.0)
                    .sqrt();
                let candidate = x[element] * ratio;
                next[element] = candidate
                    .min(x[element] + study.move_limit)
                    .max(x[element] - study.move_limit)
                    .clamp(0.0, 1.0);
            }
            next_physical = filter.apply(&next);
            let mean = next_physical.iter().sum::<f64>() / count as f64;
            if mean > study.volume_fraction {
                low = middle;
            } else {
                high = middle;
            }
        }
        let change = x
            .iter()
            .zip(&next)
            .map(|(before, after)| (after - before).abs())
            .fold(0.0, f64::max);
        x = next;
        physical = next_physical;
        iterations += 1;
        progress(TopologyProgress {
            iteration: iterations,
            max_iterations: study.max_iterations,
            compliance,
            change,
            densities: &physical,
            solve_iterations: solve.iterations,
            solve_residual: solve.residual,
        });
        if change < study.change_tolerance {
            converged = true;
            break;
        }
    }
    let volume_fraction = physical.iter().sum::<f64>() / count as f64;
    Ok(TopologyResult {
        densities: physical,
        compliance_history: history,
        iterations,
        volume_fraction,
        voxels: count,
        converged,
        cancelled,
        tier: Tier::Approximate,
    })
}

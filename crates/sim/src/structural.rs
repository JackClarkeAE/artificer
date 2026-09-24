//! The static structural check: a voxelised part, held on some faces and
//! loaded on others, and where it goes and how hard it is worked.
//!
//! ## What is approximate, and how
//!
//! - The part is its voxel grid: a staircase a cell deep at every surface
//!   that is not on a grid plane, and no feature thinner than a cell.
//! - The element is the trilinear hexahedron, which is too stiff in
//!   bending; a coarse grid deflects less than the part, and the answer
//!   rises towards the true one as the grid is refined.
//! - Stress is evaluated per element at its centre, half a cell in from
//!   any surface, so the surface value of a steep gradient is under-read;
//!   the node stresses extrapolate to the corners and read closer.
//!
//! None of that is hidden: every result carries its voxel count, the
//! solver's residual, and [`Tier::Approximate`].

use std::collections::BTreeMap;

use artificer_kernel::CancellationToken;
use artificer_protocol::{EntityRef, Tier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::element::{
    NODE_OFFSETS, SIDE_STEPS, Stiffness, centre_strain, hex_stiffness, strain_at, stress, von_mises,
};
use crate::material::Material;
use crate::mesh::VoxelMesh;
use crate::solver::{Operator, Progress, SolveOutcome, conjugate_gradients};

/// How a face is held.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Support {
    /// Every node on the face is held in all three directions.
    Fixed { face: EntityRef },
    /// Every node on the face is held along one axis only, free to slide
    /// in the other two: a roller, or a plane of symmetry.
    Roller { face: EntityRef, axis: usize },
}

impl Support {
    #[must_use]
    pub const fn face(self) -> EntityRef {
        match self {
            Self::Fixed { face } | Self::Roller { face, .. } => face,
        }
    }
}

/// What pushes on the part.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Load {
    /// A total force in newtons, spread evenly over the face.
    Force { face: EntityRef, newtons: [f64; 3] },
    /// A pressure in megapascals pushing into the part everywhere on the
    /// face.
    Pressure { face: EntityRef, megapascals: f64 },
    /// The part's own weight along a unit direction.
    Gravity { direction: [f64; 3] },
}

impl Load {
    #[must_use]
    pub const fn face(self) -> Option<EntityRef> {
        match self {
            Self::Force { face, .. } | Self::Pressure { face, .. } => Some(face),
            Self::Gravity { .. } => None,
        }
    }
}

/// A study as the user sets it up: a material, supports and loads by face.
#[derive(Clone, Debug, PartialEq)]
pub struct StructuralStudy {
    pub material: Material,
    pub supports: Vec<Support>,
    pub loads: Vec<Load>,
    /// The relative residual the solver stops at.
    pub tolerance: f64,
    /// The most iterations the solver may take; `None` for a bound from
    /// the size of the system.
    pub max_iterations: Option<usize>,
}

impl StructuralStudy {
    #[must_use]
    pub fn new(material: Material) -> Self {
        Self {
            material,
            supports: Vec::new(),
            loads: Vec::new(),
            tolerance: 1.0e-6,
            max_iterations: None,
        }
    }
}

/// Why a study could not be run.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum StructuralError {
    #[error("the voxel grid is empty at this resolution; nothing to solve on")]
    EmptyGrid,
    #[error("no fixed faces: the part would move as a rigid body under any load")]
    NoSupports,
    #[error("the support face {face} has no voxels on it at this resolution")]
    SupportOnNoCells { face: EntityRef },
    #[error("the loaded face {face} has no voxels on it at this resolution")]
    LoadOnNoCells { face: EntityRef },
    #[error("no load: nothing pushes on the part")]
    NoLoad,
    #[error("gravity needs a direction with a length")]
    GravityWithoutDirection,
}

/// A study's boundary conditions on the mesh's own degrees of freedom.
#[derive(Clone, Debug, PartialEq)]
pub struct Conditions {
    /// One flag per degree of freedom: `true` where the solver may move it.
    pub free: Vec<bool>,
    /// The nodal force at every degree of freedom, in newtons.
    pub force: Vec<f64>,
    /// The sum of every force that was applied, including the share that
    /// lands on held nodes and goes straight into the supports.
    pub applied: [f64; 3],
}

impl Conditions {
    /// Every degree of freedom free and unloaded.
    #[must_use]
    pub fn unconstrained(mesh: &VoxelMesh) -> Self {
        let dofs = 3 * mesh.node_count();
        Self {
            free: vec![true; dofs],
            force: vec![0.0; dofs],
            applied: [0.0; 3],
        }
    }

    /// Holds one node along the given axes.
    pub fn hold(&mut self, node: u32, axes: [bool; 3]) {
        for (axis, held) in axes.into_iter().enumerate() {
            if held {
                self.free[3 * node as usize + axis] = false;
            }
        }
    }

    /// Adds a force at one node.
    pub fn push(&mut self, node: u32, newtons: [f64; 3]) {
        for (axis, component) in newtons.into_iter().enumerate() {
            self.force[3 * node as usize + axis] += component;
            self.applied[axis] += component;
        }
    }

    /// The conditions a face-level study describes, on this mesh.
    pub fn from_study(mesh: &VoxelMesh, study: &StructuralStudy) -> Result<Self, StructuralError> {
        if mesh.element_count() == 0 {
            return Err(StructuralError::EmptyGrid);
        }
        if study.supports.is_empty() {
            return Err(StructuralError::NoSupports);
        }
        if study.loads.is_empty() {
            return Err(StructuralError::NoLoad);
        }
        let mut conditions = Self::unconstrained(mesh);
        for support in &study.supports {
            let nodes = mesh.nodes_on_face(support.face());
            if nodes.is_empty() {
                return Err(StructuralError::SupportOnNoCells {
                    face: support.face(),
                });
            }
            let axes = match support {
                Support::Fixed { .. } => [true; 3],
                Support::Roller { axis, .. } => {
                    let mut axes = [false; 3];
                    axes[*axis % 3] = true;
                    axes
                }
            };
            for node in nodes {
                conditions.hold(node, axes);
            }
        }
        let area = mesh.side_area();
        for load in &study.loads {
            match *load {
                Load::Force { face, newtons } => {
                    let sides = mesh.sides_on_face(face);
                    if sides.is_empty() {
                        return Err(StructuralError::LoadOnNoCells { face });
                    }
                    // Spread evenly over the face's sides, and each side's
                    // share over its four corners: the consistent load of a
                    // uniform traction.
                    let per_node = newtons.map(|component| component / (4.0 * sides.len() as f64));
                    for (element, side) in sides {
                        for node in mesh.side_nodes(element, side) {
                            conditions.push(node, per_node);
                        }
                    }
                }
                Load::Pressure { face, megapascals } => {
                    let sides = mesh.sides_on_face(face);
                    if sides.is_empty() {
                        return Err(StructuralError::LoadOnNoCells { face });
                    }
                    for (element, side) in sides {
                        // Into the part: against the side's outward step.
                        let step = SIDE_STEPS[side];
                        let per_node =
                            step.map(|component| -f64::from(component) * megapascals * area / 4.0);
                        for node in mesh.side_nodes(element, side) {
                            conditions.push(node, per_node);
                        }
                    }
                }
                Load::Gravity { direction } => {
                    let length = direction[0].hypot(direction[1]).hypot(direction[2]);
                    if !length.is_finite() || length <= 0.0 {
                        return Err(StructuralError::GravityWithoutDirection);
                    }
                    let weight = study.material.weight_per_mm3_n() * mesh.cell_volume();
                    let per_node = direction.map(|component| component / length * weight / 8.0);
                    for element in 0..mesh.element_count() {
                        for node in mesh.nodes_of(element) {
                            conditions.push(node, per_node);
                        }
                    }
                }
            }
        }
        // A force on a held dof does no work and would only distort the
        // residual, so it is dropped here.
        for (force, free) in conditions.force.iter_mut().zip(&conditions.free) {
            if !*free {
                *force = 0.0;
            }
        }
        Ok(conditions)
    }

    /// How many nodes are held along at least one axis.
    #[must_use]
    pub fn held_nodes(&self) -> usize {
        self.free
            .chunks(3)
            .filter(|axes| axes.iter().any(|free| !*free))
            .count()
    }

    /// How many nodes carry a force.
    #[must_use]
    pub fn loaded_nodes(&self) -> usize {
        self.force
            .chunks(3)
            .filter(|components| components.iter().any(|value| *value != 0.0))
            .count()
    }

    /// The sum of every force applied, as the study set it.
    #[must_use]
    pub const fn total_force(&self) -> [f64; 3] {
        self.applied
    }
}

/// The stiffness of the whole mesh, applied element by element.
pub struct ElasticOperator<'a> {
    mesh: &'a VoxelMesh,
    ke: Stiffness,
    /// `E · h` for each element, in newtons per millimetre.
    scale: Vec<f64>,
    diagonal: Vec<f64>,
}

impl<'a> ElasticOperator<'a> {
    /// The operator for a uniform material, optionally with a relative
    /// stiffness per element (a topology optimisation's densities).
    #[must_use]
    pub fn new(mesh: &'a VoxelMesh, material: &Material, relative: Option<&[f64]>) -> Self {
        let ke = hex_stiffness(material.poisson_ratio);
        let base = material.youngs_modulus_mpa * mesh.grid().cell();
        let scale = (0..mesh.element_count())
            .map(|element| base * relative.map_or(1.0, |relative| relative[element]))
            .collect::<Vec<_>>();
        let mut diagonal = vec![0.0; 3 * mesh.node_count()];
        for (element, nodes) in mesh.elements().iter().enumerate() {
            for (slot, node) in nodes.iter().enumerate() {
                for axis in 0..3 {
                    let local = 3 * slot + axis;
                    diagonal[3 * *node as usize + axis] += scale[element] * ke[local][local];
                }
            }
        }
        Self {
            mesh,
            ke,
            scale,
            diagonal,
        }
    }

    /// The element's 24 displacements gathered from a global vector.
    fn gather(nodes: &[u32; 8], x: &[f64]) -> [f64; 24] {
        let mut local = [0.0; 24];
        for (slot, node) in nodes.iter().enumerate() {
            let base = 3 * *node as usize;
            local[3 * slot] = x[base];
            local[3 * slot + 1] = x[base + 1];
            local[3 * slot + 2] = x[base + 2];
        }
        local
    }

    /// `uᵀ K_e u` for one element with unit scale: the strain energy the
    /// optimiser's sensitivities need.
    #[must_use]
    pub fn element_energy(&self, element: usize, x: &[f64]) -> f64 {
        let local = Self::gather(&self.mesh.elements()[element], x);
        let mut energy = 0.0;
        for row in 0..24 {
            let mut sum = 0.0;
            for column in 0..24 {
                sum += self.ke[row][column] * local[column];
            }
            energy += local[row] * sum;
        }
        energy
    }
}

impl Operator for ElasticOperator<'_> {
    fn dofs(&self) -> usize {
        3 * self.mesh.node_count()
    }

    fn apply(&self, x: &[f64], y: &mut [f64]) {
        y.fill(0.0);
        for (element, nodes) in self.mesh.elements().iter().enumerate() {
            let scale = self.scale[element];
            if scale == 0.0 {
                continue;
            }
            let local = Self::gather(nodes, x);
            for (slot, node) in nodes.iter().enumerate() {
                let base = 3 * *node as usize;
                for axis in 0..3 {
                    let row = &self.ke[3 * slot + axis];
                    let mut sum = 0.0;
                    for column in 0..24 {
                        sum += row[column] * local[column];
                    }
                    y[base + axis] += scale * sum;
                }
            }
        }
    }

    fn diagonal(&self) -> &[f64] {
        &self.diagonal
    }
}

/// The displacement field a set of conditions produces, and how the solve
/// went.
pub struct Displacements {
    /// Three components per node, in millimetres.
    pub values: Vec<f64>,
    pub outcome: SolveOutcome,
}

/// Solves for the displacements under given conditions, with an optional
/// relative stiffness per element.
#[allow(clippy::too_many_arguments)]
pub fn solve_displacements(
    mesh: &VoxelMesh,
    material: &Material,
    relative: Option<&[f64]>,
    conditions: &Conditions,
    tolerance: f64,
    max_iterations: Option<usize>,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
) -> Displacements {
    let operator = ElasticOperator::new(mesh, material, relative);
    let mut values = vec![0.0; operator.dofs()];
    let max_iterations = max_iterations.unwrap_or_else(|| default_iterations(operator.dofs()));
    let outcome = conjugate_gradients(
        &operator,
        &conditions.force,
        &conditions.free,
        &mut values,
        tolerance,
        max_iterations,
        cancellation,
        progress,
    );
    Displacements { values, outcome }
}

/// A bound on iterations that grows with the system: conjugate gradients
/// on a well-conditioned grid converges in far fewer, and a slender beam
/// in more, so this is a stop rather than an expectation.
#[must_use]
pub fn default_iterations(dofs: usize) -> usize {
    (20 * (dofs as f64).sqrt() as usize).clamp(500, 60_000)
}

/// What a static study found.
#[derive(Clone, Debug, PartialEq)]
pub struct StructuralResult {
    /// Every node's displacement, in millimetres.
    pub displacement: Vec<[f64; 3]>,
    /// Von Mises stress at every element's centre, in megapascals.
    pub von_mises: Vec<f64>,
    /// Von Mises stress at every node: the element stresses extrapolated
    /// to that corner and averaged over the elements that share it. Reads
    /// closer to a surface than the element centres do, and is what a
    /// picture is painted from.
    pub node_von_mises: Vec<f64>,
    pub max_von_mises: f64,
    pub max_von_mises_element: usize,
    /// The largest node stress, which is the picture's peak.
    pub peak_node_von_mises: f64,
    pub peak_node: usize,
    pub max_deflection: f64,
    pub max_deflection_node: usize,
    /// Yield strength over the largest element stress; infinite for an
    /// unstressed part.
    pub safety_factor: f64,
    /// `fᵀ u`: the work the loads do, which is what an optimiser minimises.
    pub compliance: f64,
    pub voxels: usize,
    pub nodes: usize,
    pub cell: f64,
    pub held_nodes: usize,
    pub loaded_nodes: usize,
    pub total_force: [f64; 3],
    pub solve: SolveOutcome,
    /// Always approximate: see the module documentation.
    pub tier: Tier,
}

/// Runs a face-level study on a mesh.
pub fn solve_static(
    mesh: &VoxelMesh,
    study: &StructuralStudy,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
) -> Result<StructuralResult, StructuralError> {
    let conditions = Conditions::from_study(mesh, study)?;
    Ok(solve_conditions(
        mesh,
        &study.material,
        &conditions,
        study.tolerance,
        study.max_iterations,
        cancellation,
        progress,
    ))
}

/// Runs a study whose conditions are already on the mesh's own degrees of
/// freedom, which is how a test or an optimiser sets one up.
pub fn solve_conditions(
    mesh: &VoxelMesh,
    material: &Material,
    conditions: &Conditions,
    tolerance: f64,
    max_iterations: Option<usize>,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
) -> StructuralResult {
    let displacements = solve_displacements(
        mesh,
        material,
        None,
        conditions,
        tolerance,
        max_iterations,
        cancellation,
        progress,
    );
    progress(Progress {
        phase: "stresses",
        done: displacements.outcome.iterations,
        total: displacements.outcome.iterations.max(1),
        residual: displacements.outcome.residual,
    });
    let u = &displacements.values;
    let h = mesh.grid().cell();
    let nodes = mesh.node_count();
    let displacement = (0..nodes)
        .map(|node| [u[3 * node], u[3 * node + 1], u[3 * node + 2]])
        .collect::<Vec<_>>();

    let mut von_mises_per_element = Vec::with_capacity(mesh.element_count());
    let mut node_sum = vec![0.0; nodes];
    let mut node_share = vec![0_u32; nodes];
    for element_nodes in mesh.elements() {
        let local = ElasticOperator::gather(element_nodes, u);
        let strain = centre_strain(&local, h);
        let sigma = stress(strain, material.youngs_modulus_mpa, material.poisson_ratio);
        von_mises_per_element.push(von_mises(sigma));
        for (slot, node) in element_nodes.iter().enumerate() {
            let corner = NODE_OFFSETS[slot].map(|offset| 2.0 * offset as f64 - 1.0);
            let corner_strain = strain_at(&local, h, corner);
            let corner_stress = stress(
                corner_strain,
                material.youngs_modulus_mpa,
                material.poisson_ratio,
            );
            node_sum[*node as usize] += von_mises(corner_stress);
            node_share[*node as usize] += 1;
        }
    }
    let node_von_mises = node_sum
        .iter()
        .zip(&node_share)
        .map(|(sum, share)| {
            if *share == 0 {
                0.0
            } else {
                sum / f64::from(*share)
            }
        })
        .collect::<Vec<_>>();

    let (max_von_mises_element, max_von_mises) = argmax(&von_mises_per_element);
    let (peak_node, peak_node_von_mises) = argmax(&node_von_mises);
    let deflections = displacement
        .iter()
        .map(|[x, y, z]| x.hypot(*y).hypot(*z))
        .collect::<Vec<_>>();
    let (max_deflection_node, max_deflection) = argmax(&deflections);
    let compliance = crate::solver::dot(&conditions.force, u);
    let safety_factor = if max_von_mises > 0.0 {
        material.yield_strength_mpa / max_von_mises
    } else {
        f64::INFINITY
    };
    StructuralResult {
        displacement,
        von_mises: von_mises_per_element,
        node_von_mises,
        max_von_mises,
        max_von_mises_element,
        peak_node_von_mises,
        peak_node,
        max_deflection,
        max_deflection_node,
        safety_factor,
        compliance,
        voxels: mesh.element_count(),
        nodes,
        cell: h,
        held_nodes: conditions.held_nodes(),
        loaded_nodes: conditions.loaded_nodes(),
        total_force: conditions.total_force(),
        solve: displacements.outcome,
        tier: Tier::Approximate,
    }
}

fn argmax(values: &[f64]) -> (usize, f64) {
    let mut best = (0, 0.0);
    for (index, value) in values.iter().enumerate() {
        if *value > best.1 {
            best = (index, *value);
        }
    }
    best
}

/// The faces a study names, for a caller that wants to check them against
/// a grid before solving.
#[must_use]
pub fn named_faces(study: &StructuralStudy) -> BTreeMap<EntityRef, &'static str> {
    let mut faces = BTreeMap::new();
    for support in &study.supports {
        faces.insert(support.face(), "support");
    }
    for load in &study.loads {
        if let Some(face) = load.face() {
            faces.entry(face).or_insert("load");
        }
    }
    faces
}

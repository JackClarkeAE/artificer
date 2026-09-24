//! Simulation over a voxelised body (ADR 0058): a static structural check,
//! steady-state heat, and topology optimisation, all on one grid and one
//! solver; and the clearance of a mechanism over its motion.
//!
//! This crate takes a [`Snapshot`](artificer_kernel::Snapshot) through the
//! kernel's [`voxelise`](artificer_kernel::NativeKernel::voxelise), boundary
//! conditions named by face, and a material, and returns fields: a
//! displacement per node, a stress per element, a temperature per node, a
//! density per element. It never draws.
//!
//! ## Honesty
//!
//! Everything here rests on a uniform grid of cubic cells that is inside or
//! outside the part by its cell centres, and on trilinear hexahedral
//! elements that are stiffer than the material they stand for, more so the
//! coarser the grid. Every result therefore carries
//! [`Tier::Approximate`](artificer_protocol::Tier::Approximate), the count
//! of cells it was computed on, and the residual the solver stopped at, and
//! the way to trust a number is to rerun at a finer grid and watch it
//! settle. Nothing in this crate publishes an exact figure.
//!
//! ## Determinism
//!
//! The solver is single-threaded and walks its elements in one fixed
//! order, so a study run twice on the same input produces bit-identical
//! fields. That is what lets a result be hashed, compared, and trusted to
//! be the same result on another machine.

// Matrix arithmetic is written as the textbook writes it, with row and
// column indices; an iterator over a row of a stiffness matrix is not
// clearer than `ke[row][column]`.
#![allow(clippy::needless_range_loop)]

pub mod element;
pub mod material;
pub mod mesh;
pub mod solver;

pub use artificer_kernel::{CancellationToken, SurfaceCell, VoxelGrid};
pub use material::{MATERIALS, Material, material_by_key};
pub use mesh::VoxelMesh;
pub use solver::{Progress, SolveOutcome};

/// How finely a body is voxelised: how many cells span its longest side.
///
/// Three named steps rather than a free number, so "rerun finer" has a
/// meaning and the convergence hint has something to compare against.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    Coarse,
    Medium,
    Fine,
}

impl Resolution {
    pub const ALL: [Self; 3] = [Self::Coarse, Self::Medium, Self::Fine];

    /// Cells along the longest side of the bounding box.
    #[must_use]
    pub const fn cells_along_longest_side(self) -> usize {
        match self {
            Self::Coarse => 24,
            Self::Medium => 40,
            Self::Fine => 64,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Coarse => "Coarse",
            Self::Medium => "Medium",
            Self::Fine => "Fine",
        }
    }

    /// The next step up, or `None` at the finest.
    #[must_use]
    pub const fn finer(self) -> Option<Self> {
        match self {
            Self::Coarse => Some(Self::Medium),
            Self::Medium => Some(Self::Fine),
            Self::Fine => None,
        }
    }

    /// The cell size that puts this many cells along the longest side of a
    /// box, or `None` for a box with no finite extent.
    #[must_use]
    pub fn cell_for(self, bounds: artificer_protocol::Aabb3) -> Option<f64> {
        let longest = [
            bounds.max.x - bounds.min.x,
            bounds.max.y - bounds.min.y,
            bounds.max.z - bounds.min.z,
        ]
        .into_iter()
        .fold(0.0_f64, f64::max);
        if !longest.is_finite() || longest <= 0.0 {
            return None;
        }
        Some(longest / self.cells_along_longest_side() as f64)
    }
}

# ADR 0058: A simulation tab — motion, voxel structural analysis, and experiments

Status: implemented (M1–M4) in `crates/sim`, `crates/kernel/src/sim_queries.rs`
and `apps/workbench/src/simulation.rs`, with these deviations. Resolutions
are 24, 40 and 64 cells along the longest side rather than 30³ to 100³
over the box: the solver is single-threaded by design and a million-voxel
solve is minutes, not seconds. A fixed face holds the nodes on the exposed
sides of its surface voxels, not every node of those voxels, so a clamp
does not shorten the part by a cell. Stress is per element at its centre,
and the picture is painted from node stresses extrapolated to the corners,
which read nearer the surface. The motion timeline measures every frame,
including those past the first collision, and marks them, where the
kernel's sweep stops; it does not stop. Topology optimisation carries the
structural study's supports and loads rather than picking its own, and
draws the filtered densities as a thresholded voxel skin. The parity
pixel snapshots of stress and deformation are not recorded: the headless
tests check what is drawn by counting facets and displacements instead.
Date: 2026-09-24
Extends: [0028](0028-workbench-command-registry-and-contextual-properties.md), the assembly and kinematics
work (`crates/model/src/kinematics.rs`, `api/analysis.rs`,
`api/interference.rs`), and [0056](0056-the-general-geometry-programme.md).

A Simulation tab with three things a user can act on: play a mechanism's
motion with interference over a timeline (the kinematics exist; they get a
timeline), a static structural check whose numbers converge to the textbook
answer, and, behind an "experimental" badge, thermal and topology
optimisation on the same solver. Everything is visual on the part itself.

## 1. What exists

- Joints, a pose solver, kinematic sweeps with interference and clearance
  measurement (`Kinematics::solve`, `interference_study`, the workbench's
  motion and interference cards with a clearance heat map that colours
  faces by a scalar).
- `point_in_solid` (kernel, crate-private) for voxelising an exact solid;
  exact mass properties; per-face tessellation with exact normals.

Nothing computes stress, deflection or temperature.

## 2. Decision

### 2.1 Shape

- A new crate **`crates/sim`** depending on `artificer-kernel` and
  `artificer-protocol`. It takes a `Snapshot`, boundary conditions and a
  material, and returns fields: per-node displacement, per-element stress,
  per-node temperature, per-element density. It never draws.
- The kernel exposes `NativeKernel::point_in_solid` (shared with ADR 0057)
  and `NativeKernel::voxelise(&Snapshot, cell) -> VoxelGrid` (a bit grid
  plus the surface cells' face ids, so results map back to faces for
  colouring).
- The workbench gains `RibbonTab::Simulation`, a `simulation.rs` module,
  and reuses the clearance heat map's colour mapping for stress and
  temperature and the tessellation's vertex displacement for deformation.

### 2.2 M1 — Motion with a timeline

The existing joint drivers and interference sweep, presented as a study:
a timeline scrubber with play/pause/speed, the driven joint's value over
time, interference flagged red in the viewport at the frame it occurs,
minimum clearance plotted against time. No new solver; the work is the
card, the scrubber and the plot.

### 2.3 M2 — Static structural analysis on voxels

- **Mesh:** the part voxelised on a uniform grid (resolution chosen by the
  user from coarse/medium/fine, 30³ to 100³ over the bounding box), 8-node
  hexahedral elements, one element per solid voxel.
- **Element:** the standard trilinear hexahedron with the compact
  stiffness formulation used in the top88 topology-optimisation code
  (Andreassen et al.), written in-house; isotropic linear elasticity.
- **Boundary conditions by picking faces:** fixed faces (all nodes of
  surface voxels on that face), a force on a face (distributed over its
  nodes) or a pressure, gravity as a body load. Materials from a table:
  aluminium 6061, mild steel, stainless, brass, ABS, PLA, each with E, ν,
  density and yield.
- **Solve:** preconditioned conjugate gradient (Jacobi), matrix-free
  (element-by-element products), single-threaded and deterministic; a
  progress bar and cancellation through the compute pool.
- **Results:** von Mises per element mapped onto the part's faces through
  the surface-cell face ids and drawn with the heat map; a deformation
  slider that displaces the tessellation vertices by the interpolated
  displacement (exaggerated 1× to 100×); max stress, max deflection, and a
  safety factor against yield in the card; a convergence hint that reruns
  at the next resolution and reports the change.
- **Honesty:** the card states the discretisation (voxel count), that a
  coarse voxel mesh is stiffer than the part, and the change between
  resolutions. Results are labelled `Tier::Approximate` in the report.

Gates: a cantilever (10 × 10 × 100 mm, 100 N at the tip) against
Euler–Bernoulli within the discretisation error the test states, and
converging monotonically across three resolutions; a plate with a central
hole under tension showing a stress concentration factor near 3 at the
hole; the solver's symmetry (mirror the load, mirror the field); a
rigid-body-mode check (no fixed faces refuses by name).

### 2.4 M3 — Steady-state thermal

The same voxel grid and solver with one degree of freedom per node:
temperatures on faces, convection to ambient on the rest, conductivity
from the material table; temperature heat map. Gate: a bar with two ends
at different temperatures is linear along its length.

### 2.5 M4 — Topology optimisation (experimental)

SIMP on the voxel grid with the M2 solver: a target volume fraction, a
density filter, optimality-criteria updates, the density field drawn as
a threshold surface that melts away over iterations. Labelled
experimental; results are not exported as geometry in this slice. Gate:
the MBB beam reproduces the well-known layout at the classic parameters.

### 2.6 Not in this slice

Modal or dynamic analysis, nonlinear materials, contact, tetrahedral
meshing, exporting an optimised shape back to a body.

## 3. Gates for the slice

M1's timeline drives an existing assembly fixture headlessly; M2's
cantilever and plate tests; the tab renders stress and deformation in the
parity snapshots; every solve is cancellable and reports its voxel count
and residual.

## 4. Consequences

`crates/sim` is scriptable later (`simulate_static(...)`). The kernel gains
two public queries. Nothing changes on disk unless a study is confirmed
into the document as data.

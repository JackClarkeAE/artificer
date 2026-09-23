# ADR 0055: Revolve and sweep are features

Status: proposed. This is a survey of what exists and a phased plan for
making revolve a history feature and adding a sweep. Nothing in it is built
yet.

- Date: 2026-09-23
- Decision owners: Artificer project
- Extends: [0023](0023-carrier-unified-rims-and-exact-rim-blends.md),
  [0026](0026-second-expansion-programme.md) (F3, F4),
  [0041](0041-tools-and-selections-meet-in-either-order.md),
  [0049](0049-ruled-and-spline-surfaces-enter-the-vocabulary.md),
  [0050](0050-b-spline-curves-and-surfaces-as-carriers.md),
  [0051](0051-a-loft-is-a-feature.md)

## Context: what exists today

### Revolve

The kernel already revolves a profile exactly, but only as a full turn that
makes a new body.

- **Command.** `KernelCommand::RevolvePlanarProfile { frame, profile, axis,
  angle }`.
  - `RevolveAngle` has only `FullTurn`.
  - There is no operation field, and the dispatch requires an empty input
    snapshot. The result is always a new body.
- **Validation** (`crates/kernel/src/revolve.rs`). It refuses:
  - a profile with holes, or more than one region;
  - material on both sides of the axis;
  - a straight line meeting the axis obliquely, which would make a cone apex;
  - a section that is not one contiguous chain.
- **Building.** The section builder (`section_revolve.rs`) produces planes,
  cylinders, cones, tori and spheres.
  - Each curved face is split into two half-faces, with seams at azimuth 0
    and π.
  - A point on the axis becomes a degenerate pole edge.
- **Downstream support.** Blends restack on revolved bodies, shell handles
  them, they export to STEP, and scripts reach them through
  `revolve(...)`. The scripting path also refuses add, cut and any angle
  other than 360°.

The workbench's Revolve button is a preset, not a feature.

- **What it takes.** It takes the live canvas's single construction line
  (`centreline_axis`) and profile, and bakes a
  `BaseBody + ReplayAction::Kernel(RevolvePlanarProfile)`. With no sketch it
  falls back to a hard-coded tube.
- **What it lacks.** It has no preview, no angle, and no add or cut. The
  sketch is not an input, so the body does not follow sketch edits. There
  is no editor, and the feature cannot be suppressed.
- **Records.** ADR 0026 F3 already said the preset should die in favour of
  "exactly the Extrude pattern". ADR 0041's table still lists Revolve with
  no selection.

### Sweep

There is no geometric sweep. The only thing called a sweep in the kernel
API is motion-interference checking (`api/sweep.rs`).

What can be reused:

- **Profiles.** Region compilation (`compile_sketch_regions`), and placing a
  second sketch on its own plane, the way lofts do.
- **Splines.** Non-rational B-spline curves with first and second
  derivatives. B-spline surfaces with `from_rows` and `skinned`, and a cubic
  approximation of an arc (`arc_curve`).
- **Booleans.** `tool_boolean`: prism reduction, then the analytic engine,
  then a labelled faceted tier.

What is missing:

- a 3D path type (the protocol carries only planar curves);
- any way to carry a frame along a path: nothing computes Frenet or
  rotation-minimising frames;
- a way to read an ordered open chain of curves out of a sketch;
- any entity reference in a model recipe beyond region signatures;
- a viewport pick for a sketch curve.

### What both depend on

- **Adding and cutting.** Only `tool_boolean` has a faceted tier, and lofts
  already add and cut through it. `execute_boolean`, which history Boolean
  features use, refuses anything outside planes and cylinders.

  So a revolve with a cone, torus or sphere face, and any sweep, must add or
  cut inside its own command through `tool_boolean`, as the loft does. An
  exact result for coaxial revolves needs ADR 0026 F4 (`revolved_boolean`,
  a Boolean in the (r, z) section), which is not built.
- **Referring to sketch entities.** A region signature is already made of
  `SketchEntityId`s (`FragmentKey::source_entity`). A recipe that names an
  axis line or a path by entity id is therefore no new kind of reference.

## Decision (proposed)

### Revolve becomes a feature, the way the loft did

- **Recipe.** A new `FeatureKind::Revolve` with
  `ReplayAction::SketchRevolve(SketchRevolve)`:

  ```
  SketchRevolve {
      version,
      sketch: SketchId,
      regions: Vec<RegionSignature>,
      axis: RevolveAxis,
      extent: RevolveExtent,
      operation: BodyOperation,        // New | Add | Cut, as LoftOperation
      angle_expression: Option<ParameterExpression>,   // phase R3
  }
  ```

- **Axis.** `RevolveAxis` is one of:
  - `SketchLine { entity: SketchEntityId }`, a line in the same sketch
    (normally a centreline);
  - `SketchAxis { axis: U | V }`, the sketch's own horizontal or vertical
    axis through its origin.

  Both are resolved late from the sketch's current authoring, so the
  revolve follows sketch edits and linked variables (ADR 0054) exactly as
  its regions do.
- **Extent.** `RevolveExtent` is `FullTurn`, or, from phase R3, `Angle
  { radians }` with an optional symmetric flag.
- **Replay** compiles the regions and the axis into the sketch plane's
  frame, as `SketchRegionExtrusion::resolve_in_frame` does, and emits
  `Kernel(RevolvePlanarProfile { …, operation })`.
- **Inputs.** The sketch is an input. An add or a cut names its body as an
  input, which becomes its branch.
- **Files.** The document goes to version 10 (`SKETCH_REVOLVE_DOCUMENT_VERSION`).
- **Old documents keep what they have.** A document that holds the preset's
  baked `BaseBody` revolve still replays it as it is; it is not migrated.

### Sweep is a new feature on the same pattern

- **Recipe.** A new `FeatureKind::Sweep` with
  `ReplayAction::SketchSweep(SketchSweep)`:

  ```
  SketchSweep {
      version,
      profile: SketchLoftSection,      // a sketch and its regions
      path: SweepPath { sketch: SketchId, entities: Vec<SketchEntityId> },
      orientation: SweepOrientation,   // RotationMinimising | Fixed
      operation: BodyOperation,
  }
  ```

- **Path.** The path is an ordered, open, tangent-continuous chain of
  curves in a sketch on another plane. The sketch crate gains
  `SketchDefinition::ordered_chain(entities)`, which returns the chain's
  curves in order or refuses a gap, a branch or a corner. Replay places the
  chain in 3D with the path sketch's current frame, so the sweep follows the
  path sketch, its plane and its links.
- **Protocol.** A new `KernelCommand::SweepPlanarProfile { frame, profile,
  path: SweepPath3, orientation, operation }`. `SweepPath3` is a list of 3D
  segments: line, circular arc, B-spline.
- **Kernel** (`crates/kernel/src/sweep_profile.rs`):
  - **Frames.** A rotation-minimising frame is carried along the path by
    the double-reflection method. `Fixed` keeps the profile's orientation,
    which is right for planar paths.
  - **Walls.** Each profile curve is placed at N frames along each path
    segment. N is chosen from curvature until a stated chord tolerance is
    met. The placed curves are skinned into a B-spline wall with
    `SplineSurface::skinned`. The caps are the planar profile at the two
    ends.
  - **Exact cases skip the approximation.** A straight path is an
    extrusion. A circular arc whose plane contains the profile's axis is a
    partial revolve (after R3).
  - **Refusals, each named:**
    - a path that is not G1;
    - a profile wider than the path's smallest radius of curvature, which
      would fold the wall locally;
    - a wall that crosses itself anywhere else (checked by sampling);
    - a closed path (deferred);
    - a profile plane nearly parallel to the path's start tangent.
  - **Labelling.** A non-exact sweep is labelled
    `SWEEP_APPROXIMATION_TOLERANCE` with the tolerance it met. Adding and
    cutting go through `tool_boolean`, with `SWEEP_BOOLEAN` labels.

### Workbench

Both tools mirror `apps/workbench/src/loft.rs`: a staged recipe, a
synchronous preview drawn under a preview key, a card with New/Add/Cut, a
confirm that appends a feature, a timeline Edit entry that reopens the card,
and an ADR 0041 invocation spec each.

- **Revolve picks.** The first pick is the profile region. The axis
  defaults to the sketch's single centreline if it has one; otherwise the
  card offers the sketch's U and V axes.
- **Picking a curve.** The viewport gains a sketch-curve pick
  (`selected_sketch_curve`). This lets the user pick an axis line or path
  segments directly.
- **Sweep picks.** The first pick is the profile region. Then the user picks
  one segment of the path, and the whole tangent-continuous chain through
  it is taken.
- **Editing.** Each feature gains an editor through
  `feature_has_an_editor`.

## Plan

Each phase ships on its own, with tests and an ADR status update.

### R1: Revolve is a feature (full turn, new body)

- **Model:** the `SketchRevolve` recipe, validation, replay, the new
  `FeatureKind` and `ReplayAction` arms, and document version 10.
- **Workbench:** the staged tool, preview, card, confirm, edit, and
  invocation spec. The preset and the hard-coded tube are removed.
- **Tests:**
  - model round-trip and version refusal;
  - a sketch edit followed by rebuild changes the body;
  - a linked sketch dimension drives the revolve;
  - a workbench UI flow: pick, preview, confirm, edit, undo;
  - saved-part placement of a revolved part.

### R2: Revolve adds and cuts

- **Kernel:** `RevolvePlanarProfile` gains `operation`, defaulting to New
  when read from an older file. Add and cut run through `tool_boolean` with
  `REVOLVE_BOOLEAN`.
- **Tests:**
  - exact answers for planar and cylindrical results through the prism and
    analytic rungs;
  - labelled faceted answers for tori and cones.

### R3: Partial revolves and an angle that can follow a variable

- **Kernel:** `RevolveAngle::Partial { radians }`, with two planar wedge
  faces. Seams stay at 0 and π, and a sweep of π or less has one face per
  carrier. `extract_rz_section` and shell learn the wedge, or refuse it by
  name.
- **Workbench:** the angle field accepts an expression, stored as
  `angle_expression` (ADR 0052's pattern).
- **Tests:**
  - volumes by Pappus at 90°, 180° and 270°;
  - validator edge-use counts across the π seam;
  - STEP export;
  - an angle that follows a variable.

### R4: Profiles with holes and several regions (optional)

The preferred route is ADR 0026 F4, `revolved_boolean`: a Boolean of the
(r, z) sections of coaxial solids, then revolved. It is exact, and it also
gives exact coaxial add and cut.

### S1: The path

- **Sketch:** `ordered_chain`.
- **Protocol:** `SweepPath3`.
- **Kernel:** rotation-minimising frames, with unit tests: a helix's frame
  error is bounded, and a straight path does not twist.

### S2: The sweep solid

- **Kernel:** `SweepPlanarProfile` for a new body. Exact routes for a
  straight path and for a circular path (after R3); B-spline walls with a
  stated tolerance otherwise.
- **Tests:**
  - volume against Pappus–Guldin for planar paths;
  - the validator passes;
  - STEP export;
  - each refusal is named.

### S3: Sweep is a feature

- **Model:** the `SketchSweep` recipe and document version (bundled into
  version 10 if it lands before a release).
- **Workbench:** the curve pick, the sweep tool, preview, edit, and add or
  cut through `tool_boolean`.

### Order and size

R1 and R2 are the smallest steps with the most value: they reuse the kernel
as it is. R3 is a contained kernel change. S1 to S3 are the largest body of
new geometry. R4/F4 is independent, and can run in parallel with the sweep.

## Risks

- **Booleans.** Anything with a cone, torus, sphere or spline wall is
  answered by the faceted tier. Those answers are labelled, but they are
  not exact until F4, or further analytic engines, land.
- **Seams.** A partial revolve breaks the two-half-faces-per-carrier
  assumption that full-turn edge use relies on. R3 has to state the seam
  rule and test it at exactly π.
- **Face roles.** Every revolve face, caps included, is `ExtrusionSide(i)`.
  Code keyed on `ExtrusionTop` or `ExtrusionBottom` (for example
  `extract_prism`) does not recognise revolve caps. R1 should give the caps
  roles of their own, or state why not.
- **Axis tolerance.** Being "on the axis" is judged at `linear_agreement ×
  extent`. The axis pick and sketch snapping must make touching exact, or
  revolves are refused as crossing the axis.
- **Sweep exactness.** Non-rational B-splines cannot hold a circle, so
  swept circular walls are approximations. The tolerance must be stated and
  certified.
- **The document version.** Library catalog entries and saved parts embed
  `CURRENT_DOCUMENT_VERSION` and must equal it. Moving to version 10 moves
  the built-in part's revision to 1.10.0, as version 9 moved it to 1.9.0.

## Open questions for the owner

1. **Axes.** Is the sketch's own U and V enough as axes besides a drawn
   centreline? The other options are document origin axes, and datum axes
   as a new feature.
2. **Sweep exactness.** Is an approximate B-spline sweep with a stated
   tolerance acceptable for v1? The alternative is to start with exact
   special cases only: straight and circular paths.
3. **Orientation.** Is rotation-minimising a good default, with Fixed as
   the only alternative, and guide rails and twist deferred?
4. **Old revolves.** Should the preset's baked revolves in existing
   documents stay as they are, or be offered a one-time "convert to feature"
   when the sketch is still present?

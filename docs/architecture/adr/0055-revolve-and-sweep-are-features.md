# ADR 0055: Revolve and sweep are features

Status: accepted. Phases R1, R2, R3 and S1 to S3 are implemented, with
construction axes. Revolve is a history feature that turns a sketch profile
about a centreline, the sketch's own axes, an origin axis or a construction
axis, a full turn or through an angle that can follow a variable. Sweep is a
history feature that carries a sketch profile along a path drawn in another
sketch, exactly where the path is straight or one arc, and to a stated
tolerance otherwise. Both make a new body or add to or cut from the active
one. R4 remains optional.

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

## Decision

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
      operation: SolidOperation,       // New | Add | Cut
      angle_expression: Option<ParameterExpression>,   // phase R3
  }
  ```

- **Axis.** `RevolveAxis` is one of:
  - `SketchLine { entity: SketchEntityId }`, a line in the same sketch
    (normally a centreline);
  - `SketchAxis { axis: U | V }`, the sketch's own horizontal or vertical
    axis through its origin;
  - `OriginAxis { axis: X | Y | Z }`, one of the document's origin axes,
    where it lies in the sketch's plane;
  - a construction axis feature, later.

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
      operation: SolidOperation,
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

## The owner's decisions

1. **Axes: every source.** A drawn line, the sketch's own U and V axes and
   the document's origin axes are all offered now. A construction axis — a
   datum axis feature, as construction planes are (ADR 0048) — comes next,
   as another `RevolveAxis` variant.
2. **Sweeps may be approximate, to a stated tolerance.** A swept wall that
   no exact surface carries is a B-spline surface fitted to a chord
   tolerance the kernel states and certifies, and the result says so.
3. **Orientation: rotation-minimising by default**, with Fixed as the only
   alternative; twist and guide rails are deferred.
4. **No migration.** No saved document holds the preset's revolves, so the
   preset and its fixed tube were removed outright.

## As built (R1 and R2)

- **Protocol.** `LoftOperation` became `SolidOperation` (the loft keeps its
  name as an alias), and `KernelCommand::RevolvePlanarProfile` gained an
  `operation` that reads as New from a command written before it.
- **Kernel.** An add or a cut revolve runs through `tool_boolean` with the
  `REVOLVE_BOOLEAN` labels: exact through the prism or analytic rung when
  the revolve's faces are planes and coaxial cylinders, and the labelled
  faceted tier (`revolve/faceted`, `REVOLVE_FACETED_APPROXIMATION`)
  otherwise. Scripts' `revolve(operation: …)` now adds and cuts too.
- **Model.** `crates/model/src/revolve.rs` holds `SketchRevolve`,
  `RevolveAxis` (`SketchLine`, `SketchAxis`, `OriginAxis`), `RevolveExtent`
  (full turn for now) and the axis resolution. A revolve is a
  `FeatureKind::Revolve` feature whose sketch is an input and whose add or
  cut names its body. The native document is at version 10.
- **Workbench** (`apps/workbench/src/revolve.rs`):
  - The Revolve button finishes a sketch still being drawn, takes its
    picked regions or its only region, and starts on its first centreline.
  - The card lists every axis, each origin axis that does not lie in the
    sketch's plane disabled with the reason, and New/Add/Cut.
  - The preview is the solid the kernel builds, drawn in place of the body
    an add or cut changes.
  - The Revolve chip's menu reopens the editor, and confirming rewrites the
    revolve in place.
  - A sketch dimension or variable the sketch follows (ADR 0054) reshapes
    the revolve on rebuild.

## As built (R3)

- **Protocol.** `RevolveAngle::Partial { start, sweep }`: the solid between
  azimuths `start` and `start + sweep`, measured right-handed about the
  axis as given, with zero at the profile's own half-plane. `sweep` lies
  strictly between nothing and a full turn. `RevolveAngle` is no longer
  `Eq`.
- **The seam rule, as built.** The plan kept seams at 0 and π, with one
  face per carrier for a sweep of π or less. That breaks the pole: a
  sphere band reaching the axis closes through one degenerate edge, which
  must be used twice, in opposite senses, for edge use to stay exact. So
  every carrier is split halfway round the turn instead, at `sweep / 2`.
  Each face then spans at most half a turn, a pole edge is always shared
  by two faces, and a full turn is simply the case whose split falls at π
  (ADR 0016's seams, digest for digest). A partial turn adds:
  - a vertex at the end station of every ring, and generators there;
  - planar caps as sectors, and annular caps as annular sectors, bounded
    by straight generators at both ends;
  - an axis edge from the section's last point to its first, when the
    section closes through the axis;
  - two planar wedge faces, the section at azimuth zero and its turned
    copy at the end, with roles `ExtrusionBottom` and `ExtrusionTop`, as
    a loft's caps have.
- **A concave round revolved inside out, and still would have.** A
  profile arc that runs clockwise (a concave round) flipped its torus or
  sphere's axis and negated its angle. The two flips cancel, so the face
  pointed into the material, and any profile with a concave round failed
  edge-use orientation. Such an arc is now built as a descending line is:
  the section's own axis, `angular_sign = -1`, rings taken bottom to top.
  The validator's frame rule for tori and spheres required
  `radial_u × radial_v = axis · angular_sign`, which only ever allows the
  outward-facing surface — the rule ADR 0023 lifted from cones for the
  same reason. It now asks only for an orthonormal frame, and the frame's
  handedness times the angular sign decides which way the surface faces;
  either handedness is accepted, so the left-handed frames tori already
  carry keep their meaning. STEP export, tessellation and
  the analytic Boolean already read orientation through that product.
- **Refusals.** `REVOLVE_ANGLE_INVALID`: a sweep outside the open range, a
  start that is not finite or is beyond a turn, or a sweep or remaining gap
  narrower than the minimum feature at the profile's outermost radius.
  A section point within agreement of the axis is now put on it exactly.
- **Downstream.** `extract_rz_section` refuses a wedge face, so a partial
  revolve takes no rim blend or section shell; it falls to the other
  finish and shell routes, or to their refusals. It does combine: a quarter
  tube cuts a block exactly through `revolve/boolean-prism`. The rung for a
  new partial body is `revolve/partial-turn`. Scripts'
  `revolve(angle: …)` takes any angle within a turn either way.
- **Model.** `RevolveExtent::Angle { radians, direction }`, where
  `direction` is `Forward`, `Reversed` or `Symmetric`, lowered to the
  kernel's start and sweep. `SketchRevolve::angle_expression` follows
  document variables as ADR 0052's distance does; the feature declares the
  variables, replay evaluates them, and an expression that comes to a
  whole turn makes a full turn. Both stay in document version 10.
- **Workbench.** The card gains an Extent row (Full turn or Angle), an
  angle field that takes degrees or an expression over variables and says
  what it follows, and One way, Other way or Symmetric. Reopening a
  revolve restores all three, the link included.
- **Tests.**
  - Pappus volumes at a quarter, a half and three quarters of a turn, and
    either side of a half. They cover a tube, a solid cylinder, a sphere, a
    cone frustum, a torus and a concave notch.
  - The wedge area, and the direction for a start, a reversed axis and a
    profile across the axis.
  - Each refusal, STEP export for each carrier, and the exact partial cut.
  - The model's angle link, and the card's and the real widgets' angle
    typed over a variable.

## As built (construction axes)

The owner's first decision asked for every axis source, a construction axis
among them. It is a feature in the history, on the pattern of the
construction plane (ADR 0048).

- **Model** (`crates/model/src/datum_axis.rs`).
  - `DatumAxisRecipe` records its base, a flip, the line it last resolved
    to (a cache, as a plane's frame is), and whether it is shown.
  - `DatumAxisBase` is one of:
    - an origin axis;
    - a straight edge (`Edge`);
    - a curved face (`Face`), whose cylinder, cone, torus or sphere axis it
      takes;
    - two planes (`Planes`), each an origin plane, a flat face or a
      construction plane;
    - a line that names nothing (`Fixed`).
  - `DatumAxisResolver` answers what an edge, a face or a plane is at the
    moment of replay.
  - The feature is `FeatureKind::DatumAxis`, with
    `ReplayAction::DatumAxis`, and runs nothing.
  - It names its body, and any construction plane it reads, as inputs.
  - It stays in document version 10 (`DATUM_AXIS_DOCUMENT_VERSION`).
- **Revolve.** `RevolveAxis::DatumAxis { axis }` turns right-handed about
  the axis the way it runs.
  - The axis must lie in the sketch's plane (`line_in_frame`, which origin
    axes now use too).
  - The axis is an input of the revolve, so moving it rebuilds the revolve,
    and it cannot be deleted from under it.
  - `resolve_with_datums` reads the axis where the rebuild has just placed
    it. `resolve_sketch_regions_with_datums` carries both planes and axes.
- **Kernel.** `NativeKernel::face_axis` returns a curved face's axis. The
  axis is centred on, and drawn over, the stretch of it the face covers.
- **Workbench** (`apps/workbench/src/construction_axis.rs`).
  - The Axis button in the Create group stages an axis from what is
    picked: a straight edge, a curved face, two flat faces, or a flat face
    with a construction plane.
  - The CONSTRUCTION AXIS card flips it. Confirming commits "Axis N" with a
    chip of its own. The chip's menu edits the axis or deletes it, and the
    delete is refused while something is built on it.
  - A rebuild places each axis again against the bodies as they now stand.
    If its base no longer resolves, the axis holds its place and says so
    (" · held"), as a plane does.
  - Axes are drawn as dashed lines, with the end they run towards marked.
  - The revolve card lists every construction axis by name. An axis
    standing out of the sketch's plane is listed but disabled, with the
    reason.
- **Not yet.** An axis cannot be picked in the viewport, is not listed in
  the Browser, and has no scripting form. The revolve card is where it is
  chosen.
- **Tests.**
  - Model: each base, flip, the refusals, and a round trip through JSON.
  - Model: a revolve about an axis follows where a rebuild placed it, and
    the axis cannot be deleted under it.
  - Kernel: `face_axis` on a revolved tube.
  - Workbench:
    - an axis along the block's edge that a revolve turns about exactly;
    - an axis where two faces meet, flipped in its editor and deleted;
    - a flat face alone is refused.

## As built (S1 to S3)

- **Sketch.** `SketchDefinition::ordered_chain(entities)` puts a path's
  curves end to end, from the free end of the curve named first, and
  refuses by name:
  - a gap;
  - a branch;
  - a corner;
  - a closed chain;
  - a circle or an unclamped spline.

  `tangent_chain_through(entity)` finds the smooth chain through one curve.
- **Protocol.** `SweepPath3` is a list of `SweepSegment3`: a line, a
  circular arc (centre, start, normal, sweep), or a clamped B-spline.
  `KernelCommand::SweepPlanarProfile { frame, profile, path, orientation,
  operation }`: the orientation reads as rotation-minimising and the
  operation as New from a command written without them. A path has at most
  256 segments and a spline at most 1024 points.
- **Kernel** (`crates/kernel/src/sweep_profile.rs`). The profile is carried
  rigidly from where it lies. Each copy is the profile moved by the motion
  that takes the path's frame at its start to its frame further along, so
  the profile need not sit on the path.
  - **Frames.** Rotation-minimising frames are carried by double
    reflection, 16 steps between two copies of the profile. `Fixed` only
    moves the profile. Collinear lines in a row are merged into one.
  - **Exact routes.**
    - A straight path is the two-section loft to the profile's copy at the
      far end (`sweep/straight`).
    - One arc, carried by its rotation-minimising frame about an axis in the
      profile's plane, is a partial revolve (`sweep/revolve`).
  - **The skinned route** (`sweep/skinned`) lofts smoothly through copies of
    the profile (ADR 0050). It starts with copies about a twelfth of the
    path apart.
    - **Measuring.** Between every two copies, the skin's departure from the
      true sweep is measured at the quarter points. Each profile sample is
      inverted onto the skin, seeded from the previous station, so the
      measure stays cheap.
    - **Refining.** A span that misses the budget is split: into `⁴√miss`
      parts in the middle of a piece, and by repeated halving toward a join
      between pieces. A join is where curvature jumps, so the error there
      shrinks only as h².
    - **Folds and limits.** If the skin folds, every span is halved and the
      loft tried again. At most 257 copies are used; if even that many do
      not meet the budget, `SWEEP_TOLERANCE_UNMET` gives the departure it
      reached.
    - **Tolerance.** The budget is the precision's `approximation_budget`
      (10⁻⁵ by default), held at least to its modelling resolution. The
      result carries `SWEEP_APPROXIMATION_TOLERANCE`, measuring the worst
      departure found against that budget. A disc round a 90° bend of three
      times its radius takes three rounds and 55 copies.
  - **Refusals**, each a named code:
    - `SWEEP_PATH_EMPTY`;
    - `SWEEP_PATH_INVALID`, for a segment that is not sound;
    - `SWEEP_PATH_GAP`;
    - `SWEEP_PATH_CORNER`, for tangents that disagree by more than 10⁻⁶
      radians;
    - `SWEEP_PATH_CLOSED`;
    - `SWEEP_PROFILE_ALONG_PATH`, for a start within about five degrees of
      the profile's plane;
    - `SWEEP_PROFILE_TOO_WIDE`, for a profile that reaches past the path's
      centre of curvature.

    The plan's sampled check for a wall crossing itself elsewhere was not
    built. A path that comes back through its own solid is not refused.
  - **Add and cut** go through `tool_boolean` with the `SWEEP_BOOLEAN`
    labels.
    - A straight sweep cuts exactly through `sweep/boolean-prism`.
    - A skinned sweep's B-spline walls fall to the faceted tier. That tier
      cannot yet close a skinned pipe through a block, so such a cut is
      refused with `SWEEP_FACETED_UNRESOLVED` rather than answered wrongly.
- **Model** (`crates/model/src/sweep.rs`). `SketchSweep` and `SweepPath`
  are as planned, plus a `reversed` flag that runs the path from its far
  end (omitted from the file when false).
  - The feature is `FeatureKind::Sweep` with `ReplayAction::SketchSweep`.
  - It names both sketches, and for an add or cut its body, as inputs.
  - Replay places the path with its sketch's current frame, a construction
    plane's included, so the sweep follows the path sketch.
  - It stays in document version 10 (`SKETCH_SWEEP_DOCUMENT_VERSION`).
- **Workbench** (`apps/workbench/src/sweep.rs`).
  - The Sweep button, in the Solid group, needs two finished sketches, one
    of which may be still being drawn. It takes the active sketch's picked
    or only region as the profile. Clicking a region in the model view picks
    it; Shift adds or removes one.
  - **The path is chosen on the card, not picked in the viewport**, which
    departs from the plan. The card lists every open, smooth chain in the
    other finished sketches ("Sketch 2 path 1 · 3 curves") and starts on the
    first. Reverse path runs it from its far end.
  - Orientation is Follow path (rotation-minimising) or Keep orientation
    (fixed). The operation is New, Add or Cut. The card says whether the
    preview is exact or skinned, and how close a skinned one came.
  - Confirming commits "Sweep N" with a chip of its own and hides both
    sketches. The chip's menu reopens the editor, and confirming rewrites
    the sweep in place.
  - The Browser no longer greys out the other origin planes once a sketch
    is finished. That rule dated from when a document held one sketch, and
    it stopped a profile and its path going on two origin planes.
- **Not yet.** A sweep has no scripting form. There is no twist, no guide
  rail and no closed path.
- **Tests.**
  - Sketch: `ordered_chain` and `tangent_chain_through`, each refusal
    included.
  - Kernel frames: a helix's frame turns at its torsion, and a straight
    path does not twist.
  - Kernel solids:
    - a leaning straight sweep is an exact prism;
    - an arc about an axis in the profile is the revolve;
    - a bend is skinned within its stated budget, by Pappus, and exports to
      STEP;
    - a fixed sweep keeps its disc level, by Cavalieri;
    - each refusal is named;
    - a straight sweep cuts a block exactly.
  - Model: the recipe's refusals, and a reversed curve placed running the
    other way.
  - Workbench:
    - an oblique cylinder, exact, with its chip, editor and spent sketches;
    - an arc sweep that is exact until the path is reversed, which is then
      refused by name;
    - an edit that reverses the path, then a save and replay;
    - through the real widgets, a path on XZ and a disc on XY swept,
      reopened from the chip, reversed and held level.

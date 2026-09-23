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
  `REVOLVE_PROFILE_PINCHED_ON_AXIS`: the profile meets the axis at a single
  point — a corner, a place the chain closes, or an arc's tangency —
  rather than along an edge on it, so the solid would be pinched to a
  point. `REVOLVE_ARC_CENTRE_ACROSS_AXIS`: an arc centred on the far side of
  the axis would turn into the inner lemon of a spindle torus, a carrier
  the kernel does not certify. The profile is checked as an extrusion's
  is (holes nested and apart, regions disjoint, coordinates within the
  limit), an arc's bulge counts when deciding which side of the axis the
  profile lies, and a loop may start anywhere along its run on the axis.
- **Downstream.** As first landed, `extract_rz_section` refused a wedge
  face, so a partial revolve took no rim blend or section shell. Both now
  work; see "Finishing a partial revolve" below. It combines: a quarter
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
    - A skinned sweep's B-spline walls fall to the faceted tier
      (`sweep/faceted`, `SWEEP_FACETED_APPROXIMATION`). See "Skinned
      sweeps on the faceted tier" below for what that took.
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
    - a straight sweep cuts a block exactly;
    - a disc held level along a leaning arc cuts through a block, by
      Cavalieri;
    - a bent pipe cuts up through a block, round a bend inside it and out
      of its side, taking away its bore times the centreline inside the
      block, by Pappus.
  - Model: the recipe's refusals, and a reversed curve placed running the
    other way.
  - Workbench:
    - an oblique cylinder, exact, with its chip, editor and spent sketches;
    - an arc sweep that is exact until the path is reversed, which is then
      refused by name;
    - an edit that reverses the path, then a save and replay;
    - through the real widgets, a path on XZ and a disc on XY swept,
      reopened from the chip, reversed and held level.

## Skinned sweeps on the faceted tier

At first a skinned sweep could not be added or cut: the faceted tier
refused it. Two things were wrong, one at each end of the tier.

- **The tool was too dense to combine.** A B-spline face was sampled at
  the kernel's budget of 10⁻⁵ mm, with a cap only on each knot span. A
  skin has a span between every two copies of the profile, so a pipe's
  wall ran to almost half a million facets. Two changes fix this:
  - `ChordBudget::FacetedOperand` is used wherever the faceted tier
    tessellates an operand: `tool_boolean` and the two face-feature
    crossings.
    - Arcs and ruled walls are sampled as before, sixteen chords to a
      curved face.
    - B-spline faces and edges are sampled at the display chord
      tolerance, a few thousandths of a millimetre. That is what the tier
      already leaves its arcs at.
  - A sweep that will be added or cut is skinned to that same tolerance
    (`SkinBudget::FacetedTool`). Copies placed closer would only be
    tessellated away. A new body is still skinned to the kernel's budget.
- **The tier could not keep a smooth wall closed.** It judged a point to
  lie on a plane within the modelling resolution. At that scale, the
  facets of a wall that runs on tangentially, as a sweep does past every
  join of its path, read as lying on their neighbours' planes, and the
  splits that followed shredded the wall into slivers. Six changes to
  `faceted_boolean.rs` fix this:
  - `combine_bodies` judges coplanarity at the linear agreement. Both
    operands are the kernel's own tessellations, whose shared corners
    agree to rounding.
  - Only the body near the tool goes through the Boolean.
    - The body is split at a box around the tool, grown by a margin.
    - The part inside is closed with the box's faces, which carry a
      marker role and are dropped from the answer.
    - The rest is carried over untouched. Otherwise every facet plane of
      the tool splits the body's faces clear across, out to corners the
      tool never reaches.
  - The Boolean's polygons are taken as they come. They are no longer
    built into a tree again, which split every one by its neighbours'
    planes.
  - A face is flat only if its corners lie within a nanometre of its
    plane, the tolerance the validator holds every planar face to. Before,
    the rebuild accepted corners up to 1.6×10⁻⁷ mm off the plane, which
    the validator then refused. This applies in three places:
    - the split of non-planar polygons;
    - the merge of coplanar panels, which now refuses a union that is
      not flat;
    - the acceptance of a face after welding.
  - Two outlines that are one another turned over enclose nothing, and
    both are dropped before faces are made.
  - Healing handles two cases that had no answer:
    - A gap with no width is a T-junction. The face that runs straight
      past the vertex is rerouted through it, and the topology is
      compacted.
    - A point on a straight run of a gap's boundary is left out of the
      triangulation, then put back on the side it lies on.
  - The last three hold only for `combine_bodies`, whose rebuild is
    `Rebuild::Strict`. The edge finishes and the face-feature crossing cut
    keep `Rebuild::Classic`, the rules their results were certified under.
    Under the strict rules, the faceted fallback would round the second
    edge of an already rounded corner approximately, where ADR 0044 means
    it to be refused and offered as a join.

**What it covers.** Each case below was cut through a block by a bent
pipe (up, a quarter bend, along), with the block at seven placements:
- the whole bend, the pipe crossing both faces square;
- the original block through the bend;
- the bend alone;
- the far line alone;
- the straight part alone;
- an arbitrary offset block.

Six of the seven close and validate.

**What it does not.** The seventh fails: a slab whose face runs
lengthwise along the side of the pipe, parallel to whole rows of its
facets. It still leaves slivers about the weld distance wide. Such a cut
is refused with `SWEEP_FACETED_UNRESOLVED`, never answered wrongly.

## Completing the revolve

After R1 to R3, five things about the revolve were still missing. Each is
recorded here as it lands.

### Pointed cones

A slanted line that reaches the axis used to be refused
(`REVOLVE_OBLIQUE_AXIS_CONTACT`). The refusal's note said an apex is "a
singular point rather than a pole". The builder in fact closes it exactly
as it closes a sphere, through a pole, so the refusal and its code are
gone.
- **Where a pole is used.** A vertex on the axis is a pole wherever a
  curved band meets it: an arc, or now any line that is not radial. A
  radial line still closes a planar cap.
- **The carrier.** A cone is anchored at the end of the band that has a
  radius. A band that rises from its apex has its origin at the top, and
  its parameter runs from minus its height up to zero. This keeps the
  validator's rule that a cone's base radius is positive.
- **The validator.** A cone's slant-generator rule no longer applies to a
  degenerate pole edge; the pole-closure rule, which already knew cones,
  certifies it.
- **Tests.** Each is checked against a closed form, by Pappus or πr²h/3:
  - a cone on its base, one on its point, and a double cone;
  - a full turn and a quarter turn of each, exported to STEP as
    `CONICAL_SURFACE`;
  - a cone point cut into a block;
  - a fillet on a cone's base rim, exact to 10⁻⁹ by Pappus on the kite and
    sector it removes;
  - in the workbench, a triangle against its centreline.

### Holes and several regions (R4)

A profile with a hole, or with more than one region, used to be refused
(`REVOLVE_SINGLE_REGION_ONLY`). The plan's preferred route was the coaxial
Boolean of ADR 0026 F4. It was not needed: the section builder sweeps each
loop, and a region is built whole.
- **Validation.** `validate_revolve` takes every loop of every region.
  - A region's outer loop runs anticlockwise and each hole clockwise, so
    material lies on the left of every chain.
  - All the loops together must keep to one side of the axis.
  - A hole must stay clear of the axis all the way round, or it is refused
    as `REVOLVE_HOLE_ON_AXIS`. The code replaces the old one.
- **Building** (`build_turned_region`). The builder faces every band from
  the chain's direction alone, so a clockwise hole comes out facing into
  the hole with no case of its own.
  - **A full turn.** Each hole is a cavity: a closed shell of its own,
    held as an inner shell of the solid.
  - **A partial turn.** Each hole is a channel. Its outline is a hole in
    both wedge faces, which is already the right way round, since the
    chain runs clockwise.
- **Several regions.** Each region is built on its own and the topologies
  are merged, one solid each. A single region is built exactly as before,
  entity for entity.
- **Adding and cutting.** These follow the ladder as before. Two rings of
  planes and coaxial cylinders cut a block exactly.
- **Tests.** Volumes are checked by Pappus:
  - a tube with a round cavity, and a cylinder with a square one, a full
    turn and a quarter and three quarters of one;
  - STEP export without approximation;
  - two regions, a full turn and a half, as two solids;
  - two rings cut into a block exactly;
  - in the workbench, a circle drawn inside the section.

### Finishing a partial revolve

A revolve through less than a full turn used to take no rim fillet or
chamfer, and no shell: `extract_rz_section` refused its wedge faces.
- **Reading the section back.** A plane that holds the axis is a wedge
  face. The extractor now skips it and counts it.
  - A full turn has no wedge faces; a partial one has exactly two.
  - Each curved carrier's two halves must span the same azimuth, and
    that span is the sweep.
  - Azimuth zero is read from the wedge faces themselves, not from a
    carrier's frame. The turn begins at the face whose material lies
    ahead of it about the axis.
  - A partial turn's cap is a sector: its one loop has arcs and two
    straight sides. `cap_radii` reads it as a disc or a washer.
- **Rim blends.** The blended section is rebuilt through the same span,
  and its wedge faces take the blended outline. A fillet or chamfer on the
  rim of a quarter or a three-quarter shaft is exact. Each is checked by
  Pappus against its share of the full turn, and its bounds against the
  unblended body's.
- **Shell.** The core is the section offset one wall inward, as for a full
  turn, and it is turned a full turn. It then loses a prism standing along
  the axis. The prism is drawn square to the axis, from `radial_u` towards
  `radial_v`, and runs one wall past the core at each end.
  - **Up to half a turn**, the core keeps the wedge between two lines, one
    wall in from each closed wedge face. The prism is the rest of a disc
    one wall beyond the core. At exactly half a turn both faces lie in one
    plane, and each side keeps its own line.
  - **Beyond half a turn**, the empty wedge is the convex part. The prism
    holds every point within one wall of it: the empty wedge, a band one
    wall wide along each face, and the disc one wall about the axis where
    the bands meet. The void's wall bends round the axis, exactly.
  - **Open wedge faces.** An open wedge face moves its line one wall
    outward, so the core runs past the face and the difference opens it.
    This works up to half a turn, which gives a cutaway. Beyond half a
    turn, near the axis, the empty wedge is narrower than the wall the
    other face keeps. There is nowhere for the core to run, so an open
    wedge face there is `SHELL_OPEN_FACES_UNSUPPORTED`. A cap and a wedge
    face together are the revolved reading's to take, even on a body the
    prism reading would own for its caps alone.
  - **Exactness.** The prism's walls are planes parallel to the axis and
    cylinders about it. They meet planes and coaxial cylinders in lines
    and circles, so a stepped hub's shell is exact. A cone meets them in a
    hyperbola, so a partial cone's closed shell takes the faceted tier
    with its label (`shell/faceted`, `SHELL_FACETED_APPROXIMATION`). Its
    open shell is refused (`SHELL_OPEN_REVOLVE_UNSUPPORTED`), since a
    faceted core cannot be taken away exactly. The disc's rim is drawn as
    two arcs, so neither passes half a turn, which the analytic engine
    will not classify.
- **Tests.** Volumes are checked against closed forms for the area a
  wedge keeps of each disc of the core:
  - a quarter and a three-quarter stepped hub, closed;
  - a quarter hub opened at its top;
  - a half hub opened at its top and both wedge faces;
  - a quarter cylinder opened at its top and one wedge face;
  - a quarter cone, closed, faceted and within 1% of the integral;
  - an open wedge face beyond half a turn, refused.

### Exact add and cut for turned shapes (ADR 0026 F4)

Adding or cutting a revolve with a cone, torus or sphere face used to take
the faceted tier every time; only planes and cylinders about the axis came
back exact. Two bodies of revolution about one axis now combine in their
shared section, exactly, as ADR 0026 F4 planned.
- **The route** (`coaxial_boolean`). Turning a half-section is a bijection
  onto the body, and it commutes with union, intersection and difference.
  - Both bodies are read back with `extract_rz_section`.
  - Their sections are combined by the exact line/arc engine
    (`profile_boolean_multi`); the axis is an ordinary boundary run there.
  - The result is turned again through `validate_revolve` and the section
    builder, so every carrier comes back exact: plane, cylinder, cone,
    sphere and torus.
- **The domain.** Both bodies must be single solids the extractor reads.
  Their axes must agree in direction and in position, within the linear
  agreement. Both must be full turns, or both the same partial turn from
  the same azimuth. Anything else is not this route's, and the caller's
  ladder carries on.
- **Where it runs.** In `tool_boolean` after the prism rung and before the
  analytic engine, for revolves, sweeps, lofts and spline face features
  alike (`revolve/boolean-coaxial` and its siblings). In `execute_boolean`
  in the same place (`boolean/coaxial`). The open shell of a solid of
  revolution runs through `execute_boolean`, so a cone's open shell is now
  exact too.
- **What the extractor learned.**
  - A body of spheres alone, a ball, takes its frame from a sphere.
  - A section may be a single arc from pole to pole.
  - Pieces of one arc pair by their midpoint as well as their ends, so the
    two halves of a circle are no longer taken for one piece.
- **Arcs of one circle are merged** before the Boolean. A torus is built
  in an inner and an outer half, split where its section crosses the
  major radius. A round groove centred on a shaft's surface would meet the
  shaft there at a vertex, which the plane engine refuses; merged, the
  split is gone.
- **Limits.** Two bodies about different axes still meet in curves beyond
  the analytic engine's lines and circles, so a cone, torus or sphere off
  the body's axis takes the faceted tier with its label. A section contact
  that is tangent, or that meets at a vertex, is refused by the plane
  engine and falls through in the same way.
- **Tests.** Volumes are checked against closed forms:
  - a V-groove cut into a shaft, by Pappus on the triangle left inside;
  - a ball joined to a shaft end;
  - a chamfer turned on cylinder stock;
  - a bore through a tapered post;
  - a round groove, a torus, centred on the shaft's surface;
  - a quarter groove in a quarter shaft;
  - a script `difference` of two coaxial bodies;
  - the open shell of a tapered post;
  - a cone about another axis, which is not taken for coaxial.

### Picking the axis, and construction axes in the Browser and scripts

The axis used to be chosen only from the card's buttons. Construction axes
had a timeline chip but no Browser row, and scripts could name an axis only
by numbers.
- **Picking in the view.** The card's "Pick in view" arms a pick, and the
  next click in the model view names the axis.
  - The viewport offers straight lines to be picked (`PickableLine`): a
    sketch line, by its sketch and entity, or a construction axis, by its
    feature. An overlay carries them only while the shell asks, so a
    region pick is never taken for a line. The line under the pointer is
    lit, and a click within 8 px of it is reported as `selected_line`.
  - A line of the revolve's own sketch is taken as `SketchLine`, whether
    or not it is a centreline; one that is not is listed on the card as
    "Picked sketch line". A construction axis is taken as `DatumAxis`.
  - A straight model edge becomes a construction axis along it
    (`DatumAxisBase::Edge`), appended before the revolve, which turns about
    it. It follows the edge when the body changes, and it is refused when
    it does not lie in the sketch's plane. It goes with the revolve if the
    revolve is abandoned, or if the revolve turns about something else.
  - The history only appends, so a revolve being edited cannot make an
    axis that would come after it. The pick says to make the axis first,
    then pick it.
- **The Browser.** Construction axes have an "Axes" section after the
  planes, a row each with a visibility toggle. The row's menu offers what a
  plane row's does: select, edit, rename, hide or show, and delete.
  Visibility is kept in the recipe (`set_datum_axis_visible`) and changes
  how the axis is drawn, not where it is.
- **Scripts.** `axis(...)` names an axis for `revolve(axis: ...)`:
  - `axis(from: "Z")` and `axis(origin:, direction:)` are lines in space;
  - `axis(along: edge)`, `axis(through: face)` and
    `axis(between: [a, b])` are placed by the body and resolved when the
    revolve runs (`AxisPlacement`);
  - every form takes `flip: true`.
  A placed axis is journalled on the revolve (`axis_placement`), omitted
  when absent so older journals read unchanged, and decompiles to the
  `axis(...)` that made it.
- **Tests.**
  - The viewport picks the nearer offered line and nothing far from both.
  - In the workbench, an edge's axis goes with an abandoned revolve and
    stays with a confirmed one; a sketch side turns the profile about
    itself; an edited revolve refuses an edge and takes a construction
    axis.
  - An axis row's menu, and hide and show kept in the document.
  - Scripts: a world axis and a line in space match the numeric form digest
    for digest; an edge, two faces and a curved face each place the axis;
    `flip` mirrors a partial turn; a placed axis round-trips through the
    journal and the decompiler; the refusals.

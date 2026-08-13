# 0026 — The second expansion programme: presentation, mainstream features, throughput, and the kernel frontier

Status: accepted — execution plan for the next several working passes. Phase 1
has landed: P1 (smooth shading and the four-term rig), P2 (MSAA, exact
silhouettes, outline stroke weights), F1 stage 1 (the RELATIONS family, the
eight kinds staged as atomic transactions through the confirmation gate),
F3 (`RevolvePlanarProfile` with sphere bands, annulus faces, reversed bands,
the `extract_rz_section` sphere arm, and sketch-region-plus-centreline
staging), V1 (criterion benches, committed baselines, `perf_span!`), V6 (CI
enumerates its own suites, nightly benches, macOS and Linux release
artifacts), and K5 (orphan deleted, ADR index and statuses normalised). Of
F10's seven borrowings, zoom-to-selection ships; double-click tangent chains
and Enter-repeat are held back because the bindings they want are already
taken by the explicit face pick and the confirmation gate, and reassigning
those is a UX decision rather than plumbing. Also outstanding from Phase 1:
relation glyphs (they belong with P5, which Phase 3 schedules) and a live
worker-computed revolve preview.

This ADR is a survey-grounded programme, in the manner of the blend
frontier plan that produced `corner_blend`, `prism_edge_finish`,
`loop_offset`, `rim_loop_blend`, and `section_revolve`. Every claim about
the current state below was verified against the tree at the time of
writing; file and line references are anchors, not decoration. Four
tracks: **P** (presentation), **F** (features borrowed from mainstream
CAD), **V** (velocity: performance and delivery), **K** (kernel
frontier). Milestones are ordered within a track by dependency, and a
suggested cross-track execution order closes the document.

---

## 1. Where the product actually stands

Strengths worth protecting:

- A five-rung exact edge-finish ladder and a two-engine Boolean ladder
  (`prism_boolean` → `analytic_boolean`), every rung certified-or-refused.
- Exact screen-space hidden-line rendering with interval splitting
  (`crates/viewport/src/lib.rs`, `visible_edge_intervals_indexed`) — a
  presentation feature most hobby CAD never attempts.
- A real parametric history with digest-verified replay, suppression,
  rollback, and transactional rebuild.
- A disciplined refusal culture: `DomainUnsupported` consistently means
  "not this strategy", never "wrong answer".

Load-bearing gaps the surveys established:

| Fact | Where | Consequence |
|---|---|---|
| The viewport is a CPU painter's-algorithm renderer: flat per-triangle shading, one hard-coded light `[-0.35, 0.82, 0.45]`, no z-buffer, no MSAA request, orthographic only | `crates/viewport/src/lib.rs` (`camera_triangle_shade`), `apps/workbench/src/main.rs` (`NativeOptions::default()`) | Curved faces band visibly; depth sorting is O(n log n) per frame and wrong for interpenetrating geometry; the ceiling on assembly size is low |
| A persisted 8-kind geometric constraint solver exists and is reachable from nowhere | `crates/sketch/src/constraints.rs` (Fixed, Coincident, Horizontal, Vertical, Distance, Parallel, Perpendicular, EqualLength; bounded 192-iteration projection solver) | The single highest-leverage feature in the codebase is dark |
| Mirror and linear pattern require all-planar bodies and run through the faceted BSP path | `faceted_boolean::mirror_scene`, `linear_pattern_scene`; `MIRROR_DOMAIN_UNSUPPORTED` | Two headline commands are approximate in an exact kernel and refuse any blended body |
| Revolve is `MakeRevolvedAnnulus` (axis-aligned rectangle section only); the workbench preset hardcodes r=1..2, h=3; the general `(r,z)` builder exists but is internal to blends | `crates/kernel/src/section_revolve.rs` (`build_revolved_topology`), `apps/workbench/src/ribbon.rs` | The kernel can already build every surface a revolve needs, and the user cannot reach it |
| `Surface::Sphere` is `#[allow(dead_code)]`: handled by validator/transform/hash/measures/tessellation, but no public builder emits one, and `extract_rz_section` refuses sphere faces | `crates/kernel/src/topology.rs:622`, `section_revolve.rs` | Sphere-cornered bodies cannot re-blend; the vocabulary's fifth surface is stranded |
| The analytic Boolean engine reconstructs only bodies whose faces are all Plane or Cylinder | `analytic_boolean.rs` (`operands_in_engine_vocabulary`) | Any blended body Booleans only via the co-directional prism reduction |
| The intersection matrix refuses torus×cylinder, torus×cone, torus×sphere, cone×sphere even coaxially, though all reduce to 2D section intersections | `surface_intersection.rs` dispatch | Free domain is left on the table |
| STEP export is faceted (AP214 faceted surface model); no import of anything | `apps/workbench/src/export.rs` | An exact kernel that cannot exchange exact geometry |
| Zero benches, zero kernel timing instrumentation, compute-bench not CI-run, frame budgets measured-but-not-enforced on CI | survey of Cargo.tomls, `frame_budget.rs:193` | Optimisation without instruments is guesswork; regressions are invisible |
| Several workbench suites (assembly_ui, part_library_ui, catalog_store_acceptance, most sketch suites) run on no CI job | `.github/workflows/ci.yml` native job enumerates 7 of 25 test files | Green CI overstates coverage |
| `apps/workbench/src/lib.rs` is 19,965 lines — the largest compile unit in the repo by 1.35× | size survey | Incremental builds and code navigation pay for it on every edit |
| `crates/kernel/src/analytic_face_feature.rs` (1,286 lines) is not in any `mod` declaration — dead source | kernel survey | Confusion hazard |
| ADR 0024 was never written (numbers jump 0023 → 0025) | `docs/architecture/adr/` | This document takes 0026 and leaves the gap on record |

## 2. Design rules that bind every milestone below

1. **The vocabulary is the law.** Surfaces: plane, cylinder, cone, torus,
   sphere. Curves: lines and circles — until K1 deliberately and
   completely admits ellipses. Nothing enters topology that the
   validator, transforms, hashing, measures, and tessellation do not all
   handle. A milestone that adds a type touches all of them or does not
   merge.
2. **Certified or refused.** New operations fail closed with named
   terminal codes and land in the strategy ladders as new rungs; no rung
   ever guesses.
3. **Presentation may sample; modelling may not.** Silhouette curves,
   section-preview overlays, and shading normals may be evaluated
   per-frame at display density. Nothing sampled ever feeds a snapshot,
   measure, or export. This is the existing display/authoritative chord
   split, generalised.
4. **Approximation must be legible.** Wherever the faceted fallback
   remains reachable (K3 shrinks it), the feature it produced is marked
   in the timeline. The user always knows which features are exact.
5. **No milestone lands without its closed-form gates.** Volumes, areas,
   and centroids pinned against independent in-test derivations, at
   1e-9 relative, exactly as `face_overhang_probe` and `cyl_probe` do.

---

## Track P — Presentation: make what exists look like it costs money

### P1. Smooth shading from exact normals, and a real light rig

**Borrowed look:** Fusion 360's neutral studio shading; SolidWorks'
soft-key/fill balance.

Flat per-triangle shading is why cylinders and fillets band. The kernel
knows the exact analytic normal at every tessellation vertex — it just
throws it away.

- Extend the display scene triangle with per-vertex unit normals,
  evaluated from the carrier surface at the vertex's parameters during
  tessellation (plane: constant; cylinder/cone/sphere/torus: closed
  form). This stays inside the kernel's tessellators, where the carrier
  is in hand; the protocol scene type gains `normals: [Vector3; 3]`.
- Viewport shading becomes per-vertex: compute the lighting term at each
  vertex, emit coloured vertices, and let egui's Gouraud interpolation do
  the rest. Triangle counts do not change; banding disappears.
- Replace the single-term lambert with a four-term rig, all per-vertex
  scalar math:
  - key: existing directional light, weight 0.62;
  - fill: mirrored low-angle direction, weight 0.20;
  - hemisphere ambient: `0.5 * (1 + n·up)` mapped across a two-tone
    ambient (cool floor, warm sky), weight 0.18 — this is what makes
    downward faces read as "in shadow" without any shadow pass;
  - rim: `(1 − |n·view|)^2 * 0.12` — under orthographic projection
    `n·view` is just the camera-space z component, one multiply.
- Materials keep tinting exactly as today (`shaded_material_color`),
  applied to the combined term.

**Exactness gate worth writing:** two triangles sharing a vertex on the
same carrier must carry bit-identical normals at that vertex (they are
evaluated from the same closed form at the same parameters). That is a
cheap test and a strong regression tripwire against anyone "fixing"
normals by averaging mesh geometry.

Pixel gates: regenerate shaded baselines; add one cylinder-close-up
snapshot whose diff would expose banding regressions.

### P2. MSAA, exact silhouettes, and line quality

- Request 4× multisampling in `NativeOptions` (one line; eframe passes it
  to wgpu). Feathered egui strokes already look good; MSAA fixes the
  mesh-fill edges the feathering cannot reach.
- **Exact silhouettes.** Curved bodies currently have no outline where a
  smooth face rolls away from the camera — the biggest single "toy
  renderer" tell. Under orthographic view direction `v`:
  - cylinder: two generator lines at the angles where the radial normal
    is perpendicular to `v` — closed form;
  - cone: two generators, same condition solved on the unrolled angle;
  - sphere: one circle of radius r in the view plane through the centre;
  - torus: a quartic locus — evaluated by sampling the closed-form
    condition `n(u,v)·v = 0` per frame at display density (presentation
    may sample; rule 3).
  Implementation: the display scene gains a per-face carrier descriptor
  (enum mirroring the surface params — display metadata only, no
  authority). The viewport computes silhouette polylines per frame and
  feeds them into the **existing** hidden-line interval machinery as
  synthetic edges, so silhouettes occlude and get occluded correctly.
- Promote silhouette + boundary edges to a slightly heavier stroke than
  interior edges (SolidWorks does exactly this; it is most of why its
  viewport reads "crisp").

### P3. A GPU fill path, keeping the exact edge pass

**The architectural presentation milestone.** The CPU painter's
algorithm caps body count and is wrong for interpenetrating geometry.
The fix is narrow: move **only the shaded triangle fill** to the GPU via
`egui::PaintCallback` on the existing eframe wgpu backend. Everything
that makes Artificer's viewport special — exact hidden-line edges,
overlays, dimensions, chips — stays as egui painting layered on top.

- `crates/viewport/src/gpu.rs`: a `BodyMeshCache` keyed by body instance
  and scene identity (the scene is already cached in `DisplayedBody`;
  upload once, reuse until the body's scene changes or its LOD bucket
  moves). Vertex = position, normal, rgba.
- One WGSL shader implementing the P1 rig (identical constants, so CPU
  and GPU paths match pixel-close); depth24 buffer; MSAA 4.
- Per frame: write one uniform (view transform), issue one draw per
  body — no sorting, ever. Transform previews and turntable motion
  become uniform updates instead of re-meshing.
- The CPU path remains behind a `FillBackend` switch as the fallback and
  as the reference for a CPU-vs-GPU pixel-agreement test. kittest
  harnesses already run wgpu (`Harness::wgpu()`), and the
  focused-ui-pixels CI job already renders through lavapipe, so the GPU
  path is testable on every platform CI covers today.
- Once fills are GPU-side, a **perspective projection option** is a
  uniform change. Default stays orthographic (CAD convention); the
  toggle lives in Document Properties → DISPLAY.

Hidden-line during orbit stops degrading permanently in V4 (async
refinement); P3 does not change the edge pass at all.

### P4. View cube: edges, corners, and drag

The cube currently offers 6 face picks and roll buttons. Mainstream
cubes (Inventor set the standard) offer 26 pick regions and free drag.

- Hit-test the projected cube against 6 faces + 12 edge slabs + 8 corner
  spheres; each maps to a `StandardView`-composed orientation
  (e.g. front-top edge = the view whose forward is the normalised sum of
  the two face normals). All flights reuse `CameraTransition::to_view`.
- Drag on the cube orbits the camera with the cube as the handle
  (pointer delta → the same orbit math as the viewport gesture).
- Hover highlights the exact region under the pointer (face tint exists;
  add edge/corner tints).

### P5. Sketch canvas polish

- Constraint/relation glyphs (F1) drawn as small badges beside entities,
  SolidWorks-style; hover names the relation, click selects it for
  deletion.
- Dimension text gets a theme-consistent halo chip (the model-side
  extrusion chip pattern, reused) so dimensions stay legible over dense
  geometry.
- Grid: major/minor line weight split at the existing 1/2/5 coarsening;
  axis lines of the sketch frame slightly heavier and tinted per axis.
- Under-defined vs fully-defined entity colouring once F1 stage 3 lands
  (blue/black, the convention every SolidWorks user already knows).

### P6. Display modes and an honest section preview

- Add `Wireframe` (edges only, no fills) and `HiddenLinesRemoved`
  (fills in background colour, visible edges only — the drafting look)
  to `ModelDisplayMode`. Both are trivial recombinations of passes that
  already exist.
- **Section preview** (view-only): a GPU clip plane (fragment discard)
  plus an overlay of exact section curves where the matrix certifies
  them — plane∩plane lines and perpendicular plane∩cylinder circles
  today, ellipses after K1. Where a face's section is not certified, the
  cut simply shows the clipped fill with no curve, and the status line
  says so. No capping in v1: capping is a Boolean, and pretending
  otherwise is how approximate kernels leak. After K1, exact capping for
  the certified domain can ride the real Boolean engine as a preview
  snapshot.

---

## Track F — Features other CAD programs have taught users to expect

Each milestone names the borrowed behaviour, the implementation, the
gates, and the refusals.

### F1. Surface the constraint solver (it is already written)

**Borrowed from:** SolidWorks sketch relations; Onshape's inference.

`crates/sketch/src/constraints.rs` has Fixed, Coincident, Horizontal,
Vertical, Distance, Parallel, Perpendicular, EqualLength and a
deterministic bounded projection solver (192 iterations, 2,048
constraint cap) — with zero callers outside its own crate.

**Stage 1 — wire what exists.**
- Sketch-ui gains a RELATIONS tool family: the eight kinds, enablement
  driven by the current selection (two lines → Parallel/Perpendicular/
  EqualLength; point+point → Coincident; one line → Horizontal/Vertical/
  Fixed; point+entity → Distance).
- Applying a relation calls `add_constraint` + `solve_constraints`
  through the existing sketch transaction so undo works; a non-converged
  solve **rejects the transaction** with the solver's diagnostic — the
  certified-or-refused culture applied to sketching. No partial applies.
- Glyph presentation per P5. Constraints serialise with the document
  (the model layer already persists them; verify the v6 document round
  trip with a constraint-bearing fixture).
- The recipe boundary: recipe-owned geometry (patterns, slots, polygons)
  exposes only its anchor points to constraints in v1, preserving the
  existing "over-constraint guarantee for the recipe-based solver"
  (`sketch-ui/src/lib.rs:13524`). A constraint targeting a recipe member
  curve refuses with a named reason.

**Stage 2 — the missing kinds.** Each is one residual in the projection
solver, listed with its residual function:

| Kind | Residual |
|---|---|
| Tangent (line, circle) | `distance(centre, carrier line) − r` |
| Tangent (circle, circle) | `‖c₁−c₂‖ − (r₁+r₂)` or `‖c₁−c₂‖ − |r₁−r₂|` (branch chosen at creation from the current pose, then fixed) |
| Concentric | `‖c₁−c₂‖` (two residuals, x and y) |
| EqualRadius | `r₁ − r₂` |
| Midpoint | `p − (a+b)/2` (two residuals) |
| PointOnCurve | line: signed distance; circle: `‖p−c‖ − r` |
| Symmetric (about a centreline) | reflection residuals for the point pair (two per pair) |

Branch-at-creation (tangent inner/outer) is recorded in the constraint
so the solver never re-chooses — determinism over cleverness.

**Stage 3 — degrees of freedom.** Extend the solver to report per-entity
free directions (null-space dimension of the local Jacobian is
sufficient for colouring; an exact symbolic DOF count is not needed for
v1). Blue = movable, black = fully defined; the status bar reports
"Under defined · n DOF". Gate: a rectangle with two dimensions and one
Fixed vertex reports 0 DOF; deleting one dimension reports exactly the
freed count.

**Inference at draw time.** When a stroke lands within
`angular_agreement` of horizontal/vertical, or an endpoint snap fires on
another endpoint, persist the corresponding constraint automatically
(toggle in SNAPPING AND VIEW, on by default — this is the Onshape
behaviour that makes constrained sketching free). The snap system
already classifies these events (`SnapKind::Endpoint` etc.); inference
is a small mapping from accepted snap → persisted constraint.

### F2. The missing sketch tools

- **Mirror** (about a centreline): recipe-based like the existing
  patterns — members are reflections of sources, editable as one
  feature. Reflection of lines/arcs is exact and closed under the
  vocabulary.
- **Offset**: the certified mitred-offset already exists as
  `kernel::loop_offset`. The sketch crate must stay kernel-free
  (architecture audit boundary), so move the pure 2D core — segment
  offsetting, mitre intersection, clearance certification — into
  `artificer-geometry`, re-export from the kernel, and let sketch-ui
  call the geometry crate. One implementation, two consumers, boundary
  intact. Offset of an open chain and inward/outward closed-loop offset
  both come from the same core; `RadiusTooLarge`/`SelfIntersects`
  refusals surface as sketch diagnostics verbatim.
- **Project / convert entities** (Fusion's "Project", SolidWorks'
  "Convert Entities"): when sketching on a face, the body's edges are
  exact lines and circles in 3D; edges of the support body whose
  carriers are parallel to the sketch plane project to exact lines and
  circles in sketch space. Projection is a closed-form map; entities
  arrive as construction geometry with a `Projected` provenance mark and
  refuse when the carrier is oblique to the plane (a slanted circle
  projects to an ellipse — outside the 2D vocabulary until K1; refuse
  with that exact sentence). The face-sketch display context already
  computes these projections for the backdrop; this milestone makes them
  selectable data instead of pixels.
- **Move/Rotate/Copy** for selected sketch geometry with the standard
  drag behaviour (constraints re-solve live; a failed solve snaps back —
  transactional, like everything else).
- **Construction toggle** on any entity (Centreline stays a drawing
  convenience; construction-ness becomes a flag honoured by profile
  detection, which already excludes centrelines — generalise that
  exclusion).
- **Polar snap**: angle snapping at 15° increments during line/arc
  drawing when grid snap is on (the grid already coarsens; angles get
  the same 1/2/5-style treatment at 15/30/45/90).

**Not doing, and saying so in the UI:** ellipse and spline sketch
entities. Splines are permanently outside an analytic-exact kernel;
ellipses become possible only after K1, and even then only as **derived**
curves (sections, projections), not as free sketch input in v1 — a free
sketch ellipse extrudes to an elliptic cylinder, which is not in the
surface vocabulary and stays refused.

### F3. Revolve as a first-class feature (the kernel is already 80% built)

**Borrowed from:** every CAD package since Pro/E; the staging model is
Fusion's (profile + axis + angle).

- **Protocol:** `KernelCommand::RevolvePlanarProfile { frame:
  PlanarFrame3, profile: PlanarProfile2, axis: PlanarAxis2 (point +
  direction in frame space), angle: FullTurn (v1) }` — the angle is an
  enum so partial revolves later extend rather than reinterpret.
- **Validation:** profile regions strictly on one side of the axis, or
  touching it only along axis-collinear segments/isolated points
  (r = 0 ⇒ pole or cap-on-axis). One region in v1. Distance floors from
  `min_feature_size` as everywhere.
- **Construction:** map each profile segment to an `(r,z)` section
  segment (r = signed distance to axis — sign already validated
  positive; z = coordinate along axis) and hand the chain to
  `section_revolve::build_revolved_topology` **unchanged**: lines
  parallel to the axis → cylinders; perpendicular → planar caps/annuli;
  slanted → cones; arcs → torus bands; arcs whose centre lies on the
  axis → **sphere bands**, which finally gives `Surface::Sphere` its
  public builder and removes the `#[allow(dead_code)]`.
- **Close the loop:** teach `extract_rz_section` the sphere arm (sphere
  face ↔ arc with centre on the axis) so revolved bodies re-blend and
  the "sphere-cornered bodies cannot re-blend" refusal disappears. This
  is the same milestone, not a follow-up — a builder whose output the
  extractor rejects would be a one-way door.
- **Workbench staging:** exactly the Extrude pattern — select a closed
  region, pick an axis (a sketch centreline, or an origin axis), live
  preview computed on the worker at authoritative quality, confirm chip.
  The ribbon's hardcoded Revolve preset dies.
- **Gates:** cylinder/tube vs `MakeRevolvedAnnulus` (byte-compare the
  digests for the rectangle case — the old command becomes a special
  case and can be retired at the protocol's next major rev); Pappus for
  an offset circular section (`V = 2π R_c · πr²`); sphere `4πr³/3` from
  a semicircle touching the axis; cone frustum from a slanted line;
  stacked check: revolve then rim-fillet the resulting rims through the
  existing blend ladder.
- **Partial revolve (v2):** two planar wedge faces bound the sweep; the
  two-half-faces-per-carrier convention needs seam placement rules for
  sweeps ≥ π. Specified here so v1's `FullTurn` enum slot is not a
  design accident, deferred because full turns cover the overwhelming
  share of real revolved parts (shafts, bosses, grooves, flanges).

### F4. The coaxial Boolean engine — a new rung between prism and general

**The observation that makes this cheap:** two solids of revolution
about the *same* axis intersect exactly where their `(r,z)` sections
intersect, and every intersection curve is a full circle about that
axis. The kernel already owns everything needed: `extract_rz_section`
(both operands → section chains), the 2D regularized Boolean on
line/arc regions (`profile_boolean` — the same engine the prism rung
uses), and `build_revolved_topology` (result section → topology).

- `revolved_boolean(target, tool, op, precision)`: extract both
  sections about a common axis (axis agreement within
  `linear_agreement`/`angular_agreement`, the same checks
  `calculate_revolved_measures` applies); run
  `profile_boolean_multi` on the two section region sets in the
  half-plane (the axis is an ordinary boundary segment — sections
  already close along it); rebuild. Sphere arms from F3 make blended
  revolved bodies first-class citizens here.
- **Ladder position:** `execute_boolean` tries `prism_boolean` →
  `revolved_boolean` → `analytic_boolean` → refuse. Non-coaxial
  operands fall through untouched — `DomainUnsupported` as "not this
  strategy", per the culture.
- **What this unlocks in one milestone:** grooves and o-ring glands cut
  into shafts, coaxial counterbores with filleted rims, ball-ended
  shafts (sphere∪cylinder), stepped and chamfered holes through revolved
  parts, spherical caps — the whole axisymmetric machining catalogue,
  exactly, with no new geometry code.
- **Gates:** ball-end shaft volume = cylinder + hemisphere closed form;
  groove cut via Pappus; sphere∩cylinder coaxial lens volume; a
  refusal matrix pinning: non-coaxial → falls through, tangent section
  contact → `ProfileBooleanError::Unsupported` surfaces as
  `BOOLEAN_CONTACT_UNSUPPORTED`.

### F5. Exact mirror and patterns; retire their faceted routes

Mirror and linear pattern are today the only *constructive* commands
that route through the tessellated BSP path, and both refuse any
non-planar face. Both become exact with machinery already present:

- **Mirror:** extend `transform.rs` to orientation-reversing isometries.
  A reflection maps every carrier to a carrier of the same type (plane→
  plane, cylinder→cylinder with reflected axis, torus/cone/sphere
  likewise); pcurves and `angular_sign` flip handedness, and
  `reverse_face_orientation` / `reverse_shell_orientation` (built for
  the stacked-boss work) restore outward orientation. The faceted
  `mirror_scene` route is deleted; `MIRROR_DOMAIN_UNSUPPORTED`'s
  all-planar restriction disappears. v1 mirrors the whole body into a
  new solid; a touching mirror (join at the mirror plane) unions through
  the Boolean ladder and refuses only where the ladder refuses.
- **Patterns as replayed features, not snapshot stamps.** The
  parametric layer replays a feature's recipe N times with transformed
  placements: `PatternFeature { source_feature, placements:
  PatternPlacements }` where placements is `Linear { direction, spacing,
  count }` or `Circular { axis, count, angle_step }`. Each instance is
  the same exact `TargetedKernel`/Boolean recipe at a rigid transform —
  no faceting anywhere, and instances are individually suppressible
  (the SolidWorks behaviour users expect from "skip instances").
  `LinearPatternSnapshot` and `faceted_boolean::linear_pattern_scene`
  retire once the feature-level pattern covers the ribbon command.
- **Gates:** mirrored blended body validates and has bit-equal volume
  and mirrored centroid (x → 2p−x); circular pattern of a drilled hole
  ×6 has volume `V₀ − 6·V_hole`; pattern replay is digest-stable across
  process restarts (the existing replay contract applied to the new
  action).

### F6. Shell, from the pocket the kernel already knows how to cut

**Borrowed from:** Fusion's Shell (pick a face to open, give a wall).

The insight that keeps v1 honest: *a top-opened uniform shell of a
prismatic body is exactly a blind pocket whose profile is the mitred
inward offset of the cap and whose depth is height − wall.* Both halves
exist: `loop_offset` produces the certified offset (`RadiusTooLarge` /
`SelfIntersects` are precisely the "wall too thick / neck collapses"
refusals a shell needs), and the stacked blind-pocket builder in
`prism_boolean` cuts it.

- `ShellPrism { opened_cap: EntityRef, wall: f64 }` → validate the body
  is a line/arc prism (the `extract_prism` domain), offset the cap
  profile inward by `wall` (outer loop inward; holes outward), depth =
  height − wall, route through the existing pocket path.
- Closed hollows (no opened face) build the offset cavity and attach it
  as an inner shell via the interior-void builder — also already
  written.
- Revolved bodies: section inward offset in `(r,z)` (the same 2D offset
  core from F2) → cavity of revolution → F4 subtracts it. This lands
  after F4 and shares its gates.
- **Refusals:** wall ≤ `min_feature_size`; offset self-intersection;
  bodies outside the prism/revolved domains → `DomainUnsupported` falls
  through to (nothing — refuse; a faceted shell would be a lie about
  wall thickness). Fillet-aware shelling (walls following blended rims)
  is real work on offset surfaces of tori and is explicitly deferred
  with that sentence in the error detail.
- **Gates:** shelled box volume `bdh − (b−2w)(d−2w)(h−w)`; shelled
  cylinder via annulus closed form; wall-too-thick refusal at the exact
  `loop_offset` boundary.

### F7. Hole wizard and cosmetic threads

**Borrowed from:** SolidWorks Hole Wizard, the single most-missed
feature in lightweight CAD.

Every wizard hole is a coaxial stack of cylinders, cones, and planes —
squarely inside the vocabulary and inside F4's engine (or, for planar
caps, the existing stacked-pocket machinery).

- Protocol: `DrillHole` grows `profile: HoleProfile` — `Simple {
  diameter }` (today's behaviour, default), `Counterbore { diameter,
  bore_diameter, bore_depth }`, `Countersink { diameter, sink_diameter,
  sink_angle }`. Construction builds the revolved tool section (lines +
  one slanted line for the sink) and cuts through the coaxial engine;
  through vs blind exactly as today.
- The standards live UI-side, not in the kernel: a small table module
  (ISO 273 clearance fits, DIN 74 countersinks, common metric
  counterbores) feeding the staging panel with named presets ("M5
  clearance, normal"). The kernel sees only exact dimensions.
- **Cosmetic threads:** a display-only annotation (minor-diameter circle
  overlay on the cap + callout in the inspector), stored as feature
  metadata. No helix enters the kernel — a helix is not an analytic
  surface in this vocabulary, and modelled threads would be a
  perpetually-approximate lie. This is also what mainstream CAD does by
  default, which makes the honest choice free.
- **Gates:** counterbore volume = `π(d²h + D²t)/4` against the kernel
  measure; countersink frustum closed form; standards table spot-checks
  (an M5 normal clearance hole is 5.5 mm).

### F8. Assemblies: more joints, a pose solver, and exact interference

The document layer holds a joint forest (Fixed, Revolute) and states
"a later assembly solver derives poses". The ribbon already tells users
joint-coordinate editing "comes next".

- **Joint kinds** (document-layer recipes, no solver dependency):
  `Prismatic { origin, axis, limits }`, `Cylindrical { origin, axis,
  translation_limits, angle_limits }`, `Planar { origin, normal }`,
  `Ball { origin }`. Validation mirrors Revolute's (finite, canonical
  axes, ordered limits).
- **Pose solver v1 = forward kinematics only.** The joint graph is
  already a forest (rooted at world, no loops by construction), so poses
  derive deterministically: walk parent→child composing the parent pose
  with the joint's coordinate mapped through its kind. No iteration, no
  convergence question, fully testable. Closed kinematic loops (four-bar
  linkages) are **out of scope and refused at joint creation** (adding a
  joint that would close a cycle keeps failing, as it must today) —
  they need a numeric solver and deserve their own ADR.
- **Joint-coordinate editing:** the Move/Rotate gate on constrained
  components flips from "comes next" to a drag that edits the joint
  coordinate (angle for revolute, travel for prismatic), clamped to
  limits, previewed through the existing motion machinery, committed
  through the confirmation gate.
- **Exact interference check** (the demo that sells the kernel): for
  selected component pairs, run Boolean `Intersection` on their posed
  snapshots through the full ladder. Non-empty → report each
  interference volume exactly. Where the ladder refuses a pair, the
  report says *"could not certify: [terminal code]"* rather than
  guessing — an honest interference checker is rarer than an exact one.
  Results list in the inspector; offending pairs tint in the viewport.

### F9. Interchange worth the name

- **Analytic STEP export first** (AP214 `advanced_brep_shape_
  representation`). The mapping is mechanical because the topology is
  already STEP-shaped: plane/cylinder/cone/sphere/torus →
  the five STEP elementary surfaces; lines/circles → `line`/`circle`;
  coedges → `oriented_edge` with pcurve-free 3D curves (STEP permits
  geometry-on-surface to be omitted when 3D curves are exact — ours
  are); the two-half-faces-per-carrier convention and seam edges are
  legal STEP. Faceted export remains for mesh consumers; the dialog
  labels them "STEP (exact B-rep)" and "STEP (faceted)".
  **Gate:** round-trip through an offline OCCT oracle (the ADR 0001
  development-oracle pattern — a dev-machine script, not a CI
  dependency): imported volume/area agree to 1e-9; a fixture set
  covering every surface type, seams, inner shells (cavities), and a
  blended body.
- **STEP import v2 (after export ships):** accept
  `manifold_solid_brep` whose faces are the five elementary surfaces
  and whose curves are lines/circles; run the full validator; certify or
  refuse **per solid** with per-face diagnostics ("face 12:
  `b_spline_surface` — outside the certified vocabulary"). Ellipse
  curves become importable after K1. This makes Artificer able to
  consume the enormous corpus of prismatic/turned STEP parts without
  ever holding geometry it cannot certify. Import lands as a document
  feature (`ImportedBody`) so replay and digests behave.
- STL import stays out of the product proper — that is the scan
  addon's job (mesh → fitted analytic geometry), and blessing raw
  triangle soup as a "body" would poison every exactness claim
  downstream.

### F10. Small borrowings (each ≤ a day, all high-recognition)

| Borrowed behaviour | Source | Note |
|---|---|---|
| Double-click an edge → tangent chain selection | SolidWorks | `apply_tangent_edge_chain` exists; bind to double-click |
| Isolate selected body (dim others) | Inventor | presentation-only alpha on non-active bodies |
| Zoom to selection (F key already frames; add selection-aware framing) | all | frame on selected face/edge bounds |
| Named views (save/recall camera) | all | serialize `ViewState` list in document settings |
| Viewport screenshot to file/clipboard | all | render-to-image of the current frame |
| Appearance override separate from material | Fusion | per-body display colour that does not touch density |
| Repeat last command (Enter on empty selection) | SolidWorks | ribbon plumbing only |

---

## Track V — Velocity: measure, then make it fast, then keep it fast

### V1. Instruments before optimisation

- **Criterion benches** in `crates/kernel/benches/`: `booleans.rs`
  (prism union/cut at 4/64/1024-curve profiles; coaxial cases once F4
  lands), `blends.rs` (rim fillet on N-vertex prisms), `extrude.rs`,
  `validate.rs`, `tessellate.rs` (authoritative vs display budgets).
  Baselines committed as JSON; a nightly CI job runs them and fails only
  on >2× regressions (generous on shared runners; the point is catching
  catastrophes, not chasing noise).
- **`perf_span!`** — a feature-gated macro writing `(label, elapsed,
  items)` into the existing `ComputeMetric` ring buffer (512 entries,
  already surfaced in the COMPUTE ACTIVITY card). Zero dependencies, off
  in release-default, on under `ARTIFICER_PERF_REPORT`. Spans at the
  strategy-ladder rungs, validator, tessellators, and profile Boolean
  stages — the exact places a slow operation would hide.
- **Implement the KernelCase PERF stage** that
  `test-strategy.md` specifies and nothing implements: timing +
  operation counters recorded into the case journal, so every captured
  regression case doubles as a benchmark fixture.

### V2. Frame budgets that bite

CI runners cannot enforce 16.6 ms honestly (the existing skip is
correct). Instead: `scripts/perf-gate.sh` extends the existing
frame-budget script into the release ritual — release profile, all
fixtures, plus the compute-bench table — writing a dated evidence file
under `docs/architecture/geometry-kernel/evidence/` (the house pattern
that already exists for the M5 evidence). A release tag without a fresh
evidence file is the process violation to look for in review.

### V3. Incremental replay (the parametric-history speedup)

Full-history replay cost grows linearly with feature count; editing
feature k should cost O(n−k), not O(n). The digest infrastructure makes
this safe:

- `ReplayCache: Vec<CacheEntry { action_digest, input_snapshot_digest,
  output_snapshot }>` held by the rebuild transaction. During rebuild,
  while `(action_digest, input_digest)` matches the cache, reuse the
  stored snapshot **and verify its `SemanticDigest`** (the existing
  `SnapshotAssociation` check — the cache can never silently diverge
  from what replay would have produced, because the digest chain *is*
  the replay contract). First mismatch → drop the tail, replay live from
  there.
- Suppression/rollback interact for free: both change the action list,
  which changes digests at the edit point.
- **Gate:** a 100-feature synthetic document where editing feature 100
  replays exactly 1 command (assert via a replay counter), and editing
  feature 1 replays 100; digest-equality between cached and cold rebuild
  asserted for the whole matrix.

### V4. Viewport scaling levers, in order of leverage

1. **Shade-once caching (pre-GPU, immediate):** the light rig is
   world-fixed, so per-vertex/per-triangle colour is view-independent —
   compute colours once per scene (or LOD bucket change), and per frame
   only re-project positions and depth-sort. Sorting indices of
   pre-coloured triangles is markedly cheaper than re-shading; this is a
   week's change that buys time until P3.
2. **P3 GPU fills** remove sorting and re-meshing entirely (see Track P).
3. **Async exact hidden-line during orbit:** today orbiting drops to
   front-face-only edges permanently until the gesture ends. Instead,
   keep the cheap pass as the immediate frame, kick the exact interval
   computation onto the existing `JobScheduler` at
   `InteractivePreview` priority keyed by the memo fingerprint, and
   swap the exact result in when it lands (typically a frame or two
   later at rest). The memo (`EdgeFrameMemo`) already fingerprints
   exactly the inputs the job needs.
4. **Occurrence instancing (with P3):** catalog parts sharing a
   definition share one vertex buffer with per-instance transforms —
   assemblies of repeated hardware stop paying per-copy.

### V5. Decompose the two monoliths

`apps/workbench/src/lib.rs` (19,965 lines) and `crates/sketch-ui/src/
lib.rs` (14,810) are the worst incremental-compile and navigation costs
in the repo. Mechanical decomposition, no behaviour change, pixel
baselines as the referee:

- workbench → `app.rs` (state + eframe impl), `mode_model.rs`,
  `mode_sketch.rs` (incl. the orbit peek), `panels/{browser,inspector,
  timeline,document_properties}.rs`, `confirmation.rs`, `camera.rs`,
  `viewcube.rs`, alongside the existing `ribbon.rs`/`material.rs`/
  `export.rs`.
- sketch-ui → `canvas.rs`, `tools/`, `snaps.rs`, `dimensions.rs`,
  `regions.rs`, `toolbar.rs` (exists).
- The architecture-audit script's mutation-pattern loop already
  enumerates files explicitly — extend the lists as files split, and the
  audit keeps its teeth. Measure and record incremental-build time
  before/after in the commit message (the split-crates pass produced
  600 s → 118 s; this pass targets the inner loop after a one-line UI
  edit).

### V6. CI tells the truth

- Add the never-run suites to the native job (assembly_ui,
  part_library_ui, catalog_store_acceptance, sketch_delete_ui,
  sketch_toolbar_ui, sketch_compact_toolbar_ui, sketch_recipe_editing_ui,
  sketch_primitives_acceptance, sketch_profile_extrusion_ui/_matrix,
  sketch_native_v6_recipe_matrix) — either enumerate them or, better,
  drop `--exclude artificer-workbench` and skip only the pixel suites by
  name, so a newly added suite is in CI by default rather than by
  remembering.
- A `release` job on version tags: macOS arm64 zip, Linux x86-64
  tarball, alongside the existing Windows artifact — the releases page
  currently under-serves two of three platforms.
- The nightly job (V1 benches + K4 fuzz) on `schedule:`.

---

## Track K — the kernel frontier

### K1. The ellipse programme — one new curve, a much larger world

**Why ellipses and nothing else:** the intersection of a plane with a
cylinder or cone at any non-degenerate attitude is an ellipse; the
intersection curve of two *equal-radius* cylinders with perpendicular
intersecting axes lies in two planes (the classic Steinmetz solid) and
is therefore **two ellipses**. Admitting one curve type turns three of
the most common refusals — oblique cuts, angled holes, pipe tees — into
certified results. Nothing else at this cost horizon compares.

**Vocabulary additions** (rule 1 applies — all of these or none):

- `Curve3::Ellipse { centre, major_axis: Vector3, minor_axis: Vector3,
  major_radius, minor_radius }` with the axes orthonormal and the
  parameterisation `P(t) = c + a·cos t·û + b·sin t·v̂`.
- 2D pcurves: on a **plane**, `Curve2::Ellipse` (same shape, 2D); on a
  **cylinder or cone** in `(θ, z)`/`(θ, s)` parameter space the trace of
  a plane section is `z(θ) = m + A·cos(θ − φ)` — a new
  `Curve2::Harmonic { mean, amplitude, phase }`. Both are closed under
  the transforms the kernel performs (similarity transforms map
  ellipses to ellipses and preserve harmonics' shape).
- Validator: locus arms proving an ellipse edge lies on both adjacent
  carriers (plane: affine containment; cylinder: the harmonic identity
  `‖P(t) − axis‖ = R` reduces to trigonometric identities checked at the
  agreement tolerance across the standard 8-sample sweep, exactly as
  circles are checked today).
- **Measures.** Areas and volumes stay elementary: the plane face
  bounded by an ellipse uses the sector closed form (`½ab(t₂−t₁)` plus
  triangle corrections); a cylinder face bounded by harmonics integrates
  `R·∮ z(θ) dθ` — elementary in `cos`; volume terms via the divergence
  theorem contribute integrals of `cos`, `cos²` — elementary. **Edge
  length** of an elliptic arc is a complete/incomplete elliptic integral
  — computed by the arithmetic-geometric mean, which converges
  quadratically and deterministically to machine precision. The policy
  statement this ADR makes normative: *AGM-evaluated elliptic integrals
  count as closed forms for measures.* They are deterministic, bounded,
  and exact to the last ulp — unlike meshes, they do not approximate the
  geometry, only evaluate a transcendental exactly as `cos` does.
- Tessellation: ellipse chords under the same sagitta budgets
  (`sagitta(t) = distance to chord`, max at the semi-major axis —
  subdivision counts derive from `a`, conservatively reusing the circle
  formula with radius `a`).
- Hashing, transforms, presentation edge classification (a new arm in
  the seam/edge presentation logic — the half-face seam convention
  applies to elliptic seams identically).

**What it unlocks, staged inside the same milestone:**

1. `surface_intersection`: oblique plane×cylinder → ellipse (semi-axes
   `(R, R/|n·â|)`); oblique plane×cone → ellipse **only when the plane
   cuts every generator** (`cos∠(n,â) > sin(half-angle)`); parabolic and
   hyperbolic sections stay refused by that guard, named in the
   diagnostic.
2. The analytic Boolean engine accepts the new section curves on plane
   and cylinder faces → **angled holes, mitred cylinder ends, oblique
   cuts of round bodies** all become exact.
3. Equal-radius perpendicular intersecting cylinders: the intersection
   curve factors into the planes `z = ±y` (axes as x̂/ŷ, radii equal) —
   two ellipses with semi-axes `(R√2, R)`; pcurves on both cylinders are
   harmonics. **Pipe tees and cross-drilled shafts, exactly.**
   Volume gate: the Steinmetz intersection `16R³/3`; the union
   `2·πR²L − 16R³/3`.
4. Section preview (P6) upgrades from "certified curves where axis-
   aligned" to certified everywhere a plane meets planes, cylinders, and
   admissible cones.

**Stays refused, with named reasons:** unequal-radius or non-
intersecting-axis cylinder pairs (the curve is a non-planar quartic;
"never until a quartic-curve vocabulary exists, which is not planned");
parabolic/hyperbolic cone sections; torus sections by oblique planes
(quartic); free ellipse input in sketches (elliptic prisms are not in
the surface vocabulary — see F2).

**Gates:** angled-cut cylinder volume `πR²·h̄` (mean height — the
closed form that makes oblique caps easy to pin); mitred pipe area;
Steinmetz volumes; AGM perimeter of a circle degenerates to `2πR` at
1e-15; validator rejection fixtures for every new malformation
(harmonic pcurve amplitude disagreeing with plane attitude, etc.).

### K2. Analytic Boolean reconstruction beyond Plane|Cylinder

`operands_in_engine_vocabulary` is the single gate that keeps blended
bodies out of general-position Booleans. Widen it surface class by
surface class, never all at once:

1. **Cone faces.** Parameter space `(θ, s)`; the sections a cone
   contributes in the certified matrix are circles (`⟂` plane) and
   lines (generators) — pcurves are lines in `(θ, s)`, the same forms
   the cylinder engine already chains. Drafted prisms become
   Boolean-able bodies. Gate: cut a coaxial counterbore into a drafted
   boss; volume by frustum arithmetic.
2. **Sphere and torus faces bounded by latitude circles** (the coaxial
   cases F4 produces): pcurves are `v = const` lines in the revolved
   parameterisation. After F4 most of these Booleans route through the
   coaxial rung anyway; this step matters for *mixed* stacks (a
   revolved-blended body meeting a prism at right angles) and can be
   sequenced late, informed by which refusals users actually hit
   (`BOOLEAN_ANALYTIC_RECONSTRUCTION_PENDING` should log its face-class
   pair into the diagnostics case system — a one-line change that turns
   refusal telemetry into a roadmap).

### K3. Finish retiring the faceted path — and badge what remains

After F5 (mirror, pattern) the faceted survivors are: crossing-profile
cuts over curved voids, and the last-resort edge-finish rung.

- Crossing-profile cuts route through the analytic engine as K2 widens
  it; the `max_subdivisions = 4` BSP clamp stays only as the final
  fallback.
- The last edge-finish rung stays (a refusal-only ladder would delete a
  capability users have), **but**: any feature the faceted rung produced
  is marked in the parametric history (`HistoryMode` already
  distinguishes regularized paths; add `Approximate` and render an
  "approx" badge on the timeline chip with a tooltip naming the exact
  rung that refused). Rule 4 made enforceable.

### K4. The robustness programme the test strategy already promises

- **Property fuzzing** on the certified domain: generate random valid
  profiles (the sketch arrangement generator in compute-bench is a
  start), extrude/revolve, apply random certified operations, and assert
  the *conservation invariant* on every Boolean that certifies:
  `volume(A) + volume(B) = volume(A∪B) + volume(A∩B)` at 1e-9 relative
  — an oracle-free correctness check that catches classification bugs
  no fixture anticipates. Refusals are fine; wrong volumes are not.
- Failures shrink through the KernelCase minimizer into permanent
  regression cases (the pipeline `test-strategy.md` draws; the minimizer
  is the unbuilt stage).
- Runs nightly (V6), not per-push; the corpus grows monotonically.

### K5. Hygiene with compounding interest

- Delete `crates/kernel/src/analytic_face_feature.rs` (dead, uncompiled,
  1,286 lines) after confirming no test references it — or wire it in if
  its circular-boss local rewrite beats the current route; either way,
  stop carrying an ambiguous orphan.
- `docs/architecture/adr/README.md` index (number, title, status,
  superseded-by), and normalise the three status-line formats to the
  bare `Status:` line the recent ADRs use. Resolve ADRs 0002/0003 out of
  "Proposed" (they describe the shipped tolerance and identity models;
  they should read "accepted" with a pointer to the implementing code).
- The intersection-matrix documentation in ADR 0025 gains the K1/F4
  columns as they land — the published matrix is a user-facing contract
  and must never lag the dispatch table.

---

## Suggested execution order

Phases group by dependency and by keeping every pass shippable. Sizes
are relative t-shirt estimates for a working pass like the last several.

**Phase 1 — visible polish and unlocked value (S/M items, no new
vocabulary):**
P1 smooth shading (M) · P2 MSAA + silhouettes (M) · F1 stage 1
constraints (M) · F3 revolve (M — mostly reuse) · F10 small borrowings
(S) · V1 instruments (S) · V6 CI truth (S) · K5 hygiene (S).
*Rationale: shading + silhouettes transform screenshots; constraints and
revolve are the two loudest feature absences; instruments must precede
any Track V claim.*

**Phase 2 — engines (M/L, still no new vocabulary):**
F4 coaxial Booleans (M) · F5 exact mirror/patterns (M) · F7 hole wizard
(M, after F4) · V3 incremental replay (M) · V4.1 shade-once + V4.3 async
hidden-line (M) · F1 stage 2 constraint kinds (M).

**Phase 3 — the architectural pair (L):**
P3 GPU fills (L) · V5 monolith decomposition (L — mechanical but wide) ·
F6 shell (M, after F4) · F2 sketch tools (M) · F8 assemblies (M/L) ·
F1 stage 3 DOF (M).

**Phase 4 — the frontier (L/XL):**
K1 ellipse programme (XL — the flagship) · F9 analytic STEP export
(L) then import (XL) · K2 cone reconstruction (L) · K4 fuzzing (M) ·
K3 faceted retirement + badges (S/M) · P6 section preview upgrade
(S, rides K1).

Every phase ends the way passes here always end: full workspace suite,
pixel baselines regenerated and eyeballed, architecture audit green,
closed-form gates for anything that touched the kernel, and the README
screenshots refreshed when Phase 1 changes how the product looks.

## What this programme deliberately does not do

| Not doing | Why — the honest sentence users see |
|---|---|
| Splines / NURBS anywhere | Permanently outside an analytic-exact kernel; Artificer's bet is that certified analytic CAD covers real machined parts |
| Modelled helical threads | A helix is not an analytic surface here; cosmetic threads carry the intent without the lie (F7) |
| Free ellipse sketch input | Elliptic prisms are not in the surface vocabulary; ellipses exist only as certified sections/projections (K1) |
| Unequal-radius cylinder×cylinder Booleans | Non-planar quartic intersection curve; refused with that reason |
| Closed-loop assembly constraint solving | Needs a numeric solver with a convergence story; own ADR when the forest model is actually felt as a limit (F8) |
| Drawings/2D sheets | A future programme of its own; nothing in this ADR blocks it, and P6's section curves are its seed |

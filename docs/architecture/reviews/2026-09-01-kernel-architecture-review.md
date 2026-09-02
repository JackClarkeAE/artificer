# Architecture survey and code review — 2026-09-01

Status: review record, no code changed
Scope: whole workspace at `cdcf4db` (v0.9.5), with emphasis on `crates/kernel`,
the v0.9.2–v0.9.5 changes, the new `crates/api` / `apps/api-server`, and three
user-reported defects.

Every claim below was checked against the tree or reproduced with a throwaway
test; file and line references are anchors into `cdcf4db`. Reproduction code is
quoted where a regression test should be derived from it.

## 1. Verdict

The tree is healthy by its own gates: `cargo fmt --all --check` passes, the
kernel suite passes, and workspace `clippy --all-targets -D warnings` passes.
The architecture is still the disciplined one the ADRs describe: a
dependency-free predicate crate, a kernel with no UI dependency, a document
layer that owns history, and a "certified or refused" strategy ladder for every
operation.

The three reported defects are real, all three are reproducible headlessly,
and none of them is a numerical mystery. Each is a **domain gate that refuses
too early** or a **fallback that fails silently**:

| # | Report | Root cause | Where |
|---|---|---|---|
| 1 | Side cut through two holes fails | The faceted assembler drops sliver fragments at the seam where the cutter silhouette grazes a hole panel edge; the resulting shell holes exceed the heal span, so the candidate fails the solid validator. | `crates/kernel/src/faceted_boolean.rs:1508-1552`, `:1132-1136`, `:1765` |
| 2 | Fillets/chamfers across a hole are faceted | The exact rim-loop blend refuses any prism with holes, and hole rims are never grouped as a rim loop; polygonal holes go faceted, circular holes are refused outright because a semicircle tessellates to 994 chords against a 256-chord cap. | `crates/kernel/src/rim_loop_blend.rs:63-65`, `:1495`; `faceted_boolean.rs:508` |
| 3 | Intersecting sketch objects do not split into regions | A fail-closed "kissing junction" rule marks every junction with four or more departures and an authored endpoint ambiguous, so an inscribed polygon yields zero cells. Near-miss endpoints (no on-curve snap) are a second, UI-level cause. | `crates/sketch/src/arrangement.rs:1054-1064`; `crates/sketch/src/queries.rs:110-290` |

Defect 2 is **not a regression** in this repository's history: every gate
involved is unchanged since `34b137f` (0.2.0). What changed in v0.9.2 is how a
faceted result is *drawn* (`lib.rs:2461-2476`, threshold at `:2535`), so
facets that used to be smoothed now show as creases. Defect 1 is a robustness
band, not a two-hole problem: a single circular hole fails identically when
the cutter radius equals its offset from the hole axis.

Beyond the three reports, the most important findings are:

- The v0.9.5 tessellation rewrite turned a fail-closed triangulator into a
  fail-soft one with absolute epsilons and a fan fallback that can fill holes,
  and the same function feeds the faceted Boolean operand (`lib.rs:2120`,
  `:3679-3737`, `:3818-3940`).
- The `FinishEdges` dispatcher calls each exact rung up to three times per
  request (`lib.rs:1223-1361`).
- The faceted edge-finish path emits no approximation warning, so the UI
  cannot badge it (only the cut path warns, `lib.rs:819`).
- The new API crate has one crash-on-input bug, two features that cannot work
  over its own wire format, and a README that documents a TCP server and DSL
  calls that do not exist.

## 2. Tree health

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo test -p artificer-kernel` (all suites) | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `scripts/check-architecture-boundaries.sh` | not run here (needs `rg` and `cargo tree`); no new dependency edges observed in the Cargo manifests |

Test inventory: kernel 265 tests, model 118, sketch 128, sketch-ui 139,
workbench 403, viewport 64, api 12. Twenty `println!` calls live in kernel
integration tests; six kernel test files carry three or fewer assertions (see
§6.5).

## 3. Architecture survey

### 3.1 Crate graph as built

```
geometry ─┐
compute ──┼─► kernel ─► testkit ─► cli
protocol ─┘     │
   │            ├─► viewport ─┐
   ├─► sketch ──┼─► sketch-ui ┼─► workbench
   │     │      │             │
   └─────┴─► model ───────────┘
                 ▲
       api ──────┘ (declares model, sketch, geometry, testkit; uses none of them)
        └─► api-server
```

The boundary rules in `scripts/check-architecture-boundaries.sh` still hold
for the crates they name. Two observations:

- `crates/api` is a new `NativeKernel::execute` site (`session.rs:106`,
  `:223`) and the boundary script has no tripwire for it. ADR 0007 permits a
  headless automation entry, so this is a gap in the audit, not a violation.
- `crates/api/Cargo.toml` depends on `artificer-testkit`, a test-support
  crate, and on `model`, `sketch`, and `geometry`, none of which it imports.
  The unused edges hide that the API re-implements what those crates own
  (§7, D1).

### 3.2 What is strong

- **The strategy ladders.** Edge finish (`edge_finish` → `prism_edge_finish`
  → `section_revolve` → `rim_loop_blend` → faceted) and Boolean
  (`prism_boolean` → `analytic_boolean` → faceted) each refuse with a named
  code rather than guess. The exact rungs are well tested with closed-form
  measures (`rim_loop_probe.rs`, `sphere_from_rims_probe.rs`,
  `cyl_probe.rs`).
- **The document layer.** Persistent references resolve through operation
  history with structured `Missing | Ambiguous` outcomes
  (`crates/model/src/persistent.rs`), and multi-edge finishes rebind their
  whole target set (`:386-389`).
- **Sketch arrangement.** The rotation-system construction, T-junction
  splitting, and canonical region signatures are correct; the defect in §5 is
  one over-broad guard, not the algorithm.
- **Refusal culture.** `DomainUnsupported` consistently means "not this
  strategy". The one place that has drifted is the faceted tier (§6.3).

### 3.3 Structural debt

| Item | Evidence | Consequence |
|---|---|---|
| Three monoliths | `apps/workbench/src/lib.rs` 23,872 lines (ADR 0026 measured 19,965 and scheduled decomposition as V5); `crates/sketch-ui/src/lib.rs` 18,008; `crates/kernel/src/lib.rs` 10,423 | The kernel file mixes command dispatch, tessellation, ear clipping, history emission, semantic hashing, and presentation edge classification. Every one of the review findings in §6 lives in that file. |
| Presentation heuristics inside the kernel | `presentation_prismatic_feature_roles` (`lib.rs:2507-2550`, thresholds `1.0e-8`, `1.0e-6`, `normals.len() < 2`), `presentation_smooth_edge_flags` (`:2413-2500`) | Display smoothing decisions with magic tolerances sit in the crate whose rule is "no scattered epsilons". They belong in `viewport`, fed by exact carrier identity. |
| Magic epsilons | 70 literal `1e-n` constants in `kernel/src/lib.rs`, 22 in `faceted_boolean.rs`, 14 in `corner_blend.rs` | Contradicts the programme rule; the v0.9.5 additions (§6.2) are the worst offenders because they are absolute, not scale-relative. |
| Probe tests | §6.5 | Several "regression" files assert only that something was produced. |
| ADR gap | ADR 0024 never written; ADR 0026 records this | Cosmetic. |

## 4. Defect 1 — side cut intersecting two holes

### 4.1 Dispatch

`ExtrudeFacePlanarProfile { Cut }` has a two-rung ladder, not the four-rung one
the docs imply (`crates/kernel/src/lib.rs:715-947`):

1. `validate_exact_face_feature`. Any cylinder whose axis is not parallel to
   the sweep and whose bounds overlap the profile raises `SweepCollision`
   (`exact_face_feature.rs:175`, `:575-587`).
2. On `SweepCollision`, directly `faceted_boolean::subtract_crossing_profile`
   with the source scene clamped to `max_subdivisions = 4` (16-gon holes) and
   the cutter sampled at 64 panels (`lib.rs:787-812`).

`prism_boolean` and `analytic_boolean` are consulted only on
`ProfileOutsideFace` (`lib.rs:829-930`) and in the `Boolean` command. For a
perpendicular round cutter through a round hole that is correct: the
intersection is a quartic, outside the line/circle vocabulary, so the faceted
tier is the only tier that can answer. The bug is that it answers wrongly.

### 4.2 Reproduction

100×100×40 block extruded along +Z from the XY plane; cut from the `y = 0`
face with frame origin `(50, 0, 20)`, `u = (1,0,0)`, `v = (0,0,1)`, default
precision.

| Holes | Cutter | Outcome |
|---|---|---|
| circle (40,50) r8 + rect (60,50) 16² | circle r10, blind 60 or through 120 | **ValidationFailed**, `EDGE_USE_COUNT` ×21 |
| same | rect 20×20 | OK, faceted, approximation warning |
| circle + circle (x = 40, 60) | circle r10 | **ValidationFailed**, `EDGE_USE_COUNT` ×48 + `EULER_CHARACTERISTIC_INVALID` (12–13 s) |
| rect + rect | circle r10 | OK, faceted, 945 faces |
| circle + triangle, circle + L | circle r10 | **ValidationFailed** ×21 |
| **single** circle (40,50) r8 | circle r10 at x = 50 | **ValidationFailed** ×21 |
| single circle (50,50) r8 | circle r10 at x = 50 | OK, 3158 faces, 11.5 s |
| circle (30,50) + rect (70,50) — the layout in `crossing_cut_three_holes.rs` | circle r10 | OK, **exact rung**, 15 faces, no warning |

Offset sweep with one hole at x = 40, r 8, cutter r 10: offsets 43…49.9 and
50.5…55 succeed; **50.0 and 50.1 fail**. Radius sweep at offset 50: 9 and 9.9
succeed, **10 fails**, 10.1 succeeds. Off-grid hole (40.37, 50) r 7.3 with
cutter x = 49, r 9.7 fails with `EDGE_USE_ORIENTATION` ×3. The failure is a
band around "cutter silhouette generatrix passes through or near a hole panel
edge", which round user coordinates hit easily.

`crates/kernel/tests/crossing_cut_three_holes.rs` does not exercise a crossing
cut at all: the cutter occupies x ∈ [40, 60], holes 1 and 2 occupy [17, 33]
and [67, 83], and hole 3 sits at y = 75 beyond the 30 mm depth. It passes
through the exact rung.

### 4.3 Root cause

Traced with temporary instrumentation in
`topology_from_polygons_with_heal_limit` (`faceted_boolean.rs:1450`). For
hole at 40 / cutter at 50 the BSP subtraction yields 2531 polygons; the
assembler then discards eleven of them, all slivers at the seam where the
cutter's silhouette panels graze the hole's vertical panel edge (vertices
1e-4–3e-4 apart: above the 1.6e-5 weld epsilon, tiny relative to the polygon):

- `:1508-1515` — after `dedup_by` on raw distance (`:1469`), two non-adjacent
  vertices weld to one key and the whole polygon is skipped (5 polygons).
- `:1526-1552` — the face normal is taken from the first vertex triple whose
  cross product exceeds ε (a triangle with 4e-3 mm legs qualifies), so a
  sliver's normal is garbage; any polygon with more than three vertices and
  planar deviation above `max(ε·1e-2, 1e-7)` is then skipped. The comment at
  `:1540-1545` says such a fragment is "split into planar triangles"; the code
  discards it (6 polygons).

Each drop leaves a 1.4–2.5 mm boundary cycle that straddles a crease.
`heal_planar_boundary_cycles` (`:1691`) can close it only by planar ear clip
(fails, `:1763`) or by a non-planar fan, which is refused because
`maximum_healed_cycle_span` is `max(approximation_budget, modeling_resolution,
min_feature_size) · 512 = 5.12e-3 mm` (`:1132-1136`, `:1765`). Thirty-nine
single-use edges before healing, twenty-one after; the validator rejects the
candidate at `lib.rs:1431-1442`.

Second defect (the `EDGE_USE_ORIENTATION` case): sub-ε fragments are dropped
inside the BSP itself (`split_polygon`, `:163-168`), leaving sixteen tiny
cycles the healer does close, but its triangle winding follows
`SplitPlane::from_points(..).normal` (`:1749`, `:1801`, `:1811-1815`), whose
sign is arbitrary with respect to the cycle's recorded `missing_orientation`
(`:1709-1714`).

Commit `536f3d6` ("watertight BSP boolean subtraction gates") did not touch
`faceted_boolean.rs`; that wording refers to the tessellation fallbacks in
`lib.rs` (§6.2).

### 4.4 Fix

In `topology_from_polygons_with_heal_limit`:

1. Dedup on welded **keys**, not raw distance; if a key repeats
   non-consecutively, split the polygon into two loops at the pinch and
   requeue both instead of skipping.
2. Compute the normal with Newell's method over the whole loop instead of the
   first non-degenerate triple.
3. When a polygon is non-planar beyond tolerance, do what the comment
   promises: ear-clip it on the welded points and requeue the triangles.

In `heal_planar_boundary_cycles`: orient the projection plane by the cycle's
own Newell normal (flip `SplitPlane` when their dot product is negative) so
healed triangles wind with the missing coedge.

Hardening: use `Polygon::new_narrow` for fragments inside `split_polygon` so
sub-ε slivers are welded by the assembler rather than vanishing in the BSP,
and map a faceted candidate that fails validation to `Unsupported` with the
approximation explanation instead of a bare `ValidationFailed`.

Regression tests to add: hole (40,50) r8 + cutter (50, z 20) r10; the
circle+rect and circle+circle layouts; the off-grid (40.37, r 7.3) / (49, r
9.7) case. Assert success, `validate(.., Solid).valid`, and the approximation
warning. Rename or fix `crossing_cut_three_holes.rs` so it crosses something.

## 5. Defect 2 — fillets and chamfers across a hole

### 5.1 Which rung handles a hole rim today

None. For the `FinishEdges` ladder (`lib.rs:1213-1406`):

| Rung | Why it refuses a hole rim | Where |
|---|---|---|
| `edge_finish` | requires exactly six planar faces | `edge_finish.rs:57-65` |
| `prism_edge_finish` | vertical generator edges only | `prism_edge_finish.rs:286-298` |
| `section_revolve` | every planar normal parallel to one axis | `section_revolve.rs:93-96` |
| `rim_loop_blend` | **`if !prism.holes().is_empty() { DomainUnsupported }`** before targets are examined; `resolve_complete_cap_loop` compares against `prism.outer()` only | `rim_loop_blend.rs:63-65`, `:144` |
| `rim_loop_group` (selection expansion) | scans each cap's `outer_loop`, never `inner_loops` | `rim_loop_blend.rs:1495` |
| Cut-built holes and `MakeCuboid` bodies | `extract_prism` keys on `ExtrusionTop/Bottom` roles; a cuboid has `PositiveZ/NegativeZ` | `prism_edge_finish.rs:177-178`, `:241-255` |

The single-edge `FinishEdge` arm (`lib.rs:1125-1185`) has no `rim_loop_blend`
rung at all, so the two ladders are asymmetric.

### 5.2 Reproduction

100×100×20 block, hole radius 8 or side 16 at centre, distance 2.0, full rim
loop of the hole on the top cap:

| Hole | Chamfer | Fillet |
|---|---|---|
| circular (profile inner loop) | **refused** `EDGE_FINISH_BLEND_UNSUPPORTED` | refused |
| circular (Cut) | refused | refused |
| square (profile) | 90 planar faces, cap split into 17; no warning | 659 planar faces after four successive BSP passes; no warning |
| square (Cut) | refused | 749 planar faces |
| L-shaped | 137 planar faces | 1353 planar faces |
| D-shaped (line + arc) | refused | refused |
| control: outer rim of a plain box | 10 faces, exact | 18 faces: 4 cylinders, 4 spheres, exact |

`report.warnings` was empty in every faceted result.

### 5.3 Root cause

- Polygonal holes reach the faceted tier, which rebuilds the **entire body**
  from `scene.triangles` (`faceted_boolean.rs:372-386`) because
  `planar_topology_polygons` returns `None` for any face with inner loops.
  Per-chord planar facets with per-facet `FeatureSide(i)` roles are the
  "loads of little facets". For the fillet the first pass fails validation
  and `finish_logical_successor_edges` re-cuts the body once per edge
  (88 → 141 → 277 → 659 faces).
- Circular holes are refused because the authoritative tessellation of one
  semicircle edge has 994 chords (`arc_subdivisions`, `lib.rs:3035-3056`,
  with `approximation_budget = 1e-5`) and `edge_finish_cutters` caps a source
  edge at 256 segments (`faceted_boolean.rs:508`).
- Nothing on this path emits an approximation warning; the only one in the
  codebase is the cut path's `FACE_FEATURE_FACETED_APPROXIMATION`
  (`lib.rs:819`). ADR 0026 rule 4 ("approximation must be legible") is not
  met for edge finishes.

History: `git log -S` on the holes gate, the outer-only loop resolution, the
256-segment cap, and the approximation budget all point at `34b137f`. The
v0.9.2 commit `2563d38` altered only faceted internals and the presentation
smoothing rules (`lib.rs:2461-2476`, `:2535` threshold 8 → 2), and loosened
`pocket_finish_probe.rs` from 1 % to ±15–25 %. So the exact result the user
remembers was never produced by this ladder; what changed is that faceted
results now display their creases.

### 5.4 Fix

**A. Let `rim_loop_blend` own a hole loop.** Hole loops are already stored
clockwise (`analytic_extrusion.rs:237-239`), so "material to the left" holds
and `mitred_inward_offset` grows the hole correctly; concave arcs (torus at
`r + f`, cone slope `(spine_radius − radius)/d`) are already handled by both
builders. Remove the `holes().is_empty()` gate; make
`resolve_complete_cap_loop` return `(top, loop_index)` by comparing the target
edge set against each loop of the cap (`outer_loop` then `inner_loops`); pass
the target loop and the passive loops to the builders; emit each passive loop
unchanged (one full-height wall per segment, and the loop as an inner loop on
both caps, which needs `Builder::push_face` to accept `inner_loops`, currently
hard-coded empty at `:521`). This alone makes a circular hole exact: fillet →
two torus bands, chamfer → two cone bands.

**B. Reflex corners for chamfers only.** `loop_offset.rs:91-94` refuses right
turns. Correct for fillets (two cylinders meet in an ellipse) but over-strict
for chamfers, where two slant planes meet in a straight mitre line. Add a
`kind`-aware branch that computes the same mitre point for reflex line/line
corners, records `SpineVertexKind::SharpReflex`, and lets `retarget` extend
rather than trim. Keep refusing reflex corners with arcs and every reflex
fillet corner (until K1's ellipse programme lands).

**C. Selection.** `rim_loop_group` should iterate `face.value.loops()` and
return the containing loop, so the workbench's `ExactRimBlend` classification
(`apps/workbench/src/lib.rs:7933-7949`) triggers for holes.

**D. Honesty.** In both `FinishEdge`/`FinishEdges` faceted arms push an
`approximation_warning("EDGE_FINISH_FACETED_APPROXIMATION", ..)`, mirroring
the cut path, so the properties panel can badge it (the panel already quotes
the cut caveat, `apps/workbench/src/lib.rs:23797`).

**E. Follow-ups.** `extract_prism` should recognise a `MakeCuboid` body by
geometry (anti-parallel planar cap pair to which every other face is
parallel) rather than by role, so Cut-made holes reach the exact rungs; either
raise the 256 cap at `faceted_boolean.rs:508` or sample cutters at the display
budget; add hole-rim tests to `rim_loop_probe.rs` asserting carrier kinds (2
`Torus` / 2 `Cone`), face counts, and the closed-form volume.

## 6. Defect 3 — intersecting sketch objects do not split into regions

### 6.1 What is and is not broken

Region computation (`build_arrangement`), 2D picking
(`select_region_at_point`, `crates/sketch-ui/src/lib.rs:5718-5756`), the
finish-to-3D overlay (`finish_sketch_now`, `visible_sketch_overlays`), and 3D
region hit-testing all behave correctly when the geometry is exact. There is
no "largest region" heuristic, the overlay exposes every cell, and the v0.9.5
Shift-click change only added the additive branch of picking.

Arrangement results (`build_arrangement`, default limits):

| Input | Cells | Diagnostics |
|---|---|---|
| square + line ending exactly on both edges | 2 (areas 8, 8) | none |
| square + line extending beyond both edges | 2 | none |
| square + line ending 1e-7 inside the edges | 1 | `ZeroAreaCycle` |
| triangle or rectangle strictly inside a circle | 2 | none |
| **square inscribed in a circle** (vertices on the rim) | **0** | `KissingJunction` ×4 |
| **hexagon inscribed in a circle** | **0** | `KissingJunction` ×6 |
| square + four spokes from the centre | **0** | `KissingJunction` |
| square + "+" drawn as two full lines | 4 | none |

The workbench reproduces the same numbers: square + line → two regions,
`RegionSelectionRequired`, overlay `region_count = 2`; inscribed square in a
circle → `available = 0`, eligibility `UnsupportedProfile`, overlay empty.

### 6.2 Root causes

**Primary — `link_half_edges`, `crates/sketch/src/arrangement.rs:1054-1064`.**

```rust
if edges.len() >= 4 && has_authored_endpoint {
    diagnostics.push(ArrangementDiagnostic::KissingJunction { .. });
    ambiguous_junctions.insert(*junction);
}
```

Any junction with four or more departures that carries an authored endpoint is
declared ambiguous; at `:1069-1071` every half-edge touching it is left with
`next = None`, so every loop through it fails to close and the sketch gets zero
cells, including the outer one. That is exactly the topology of a polygon
drawn inside a circle the natural way (centre snapped to the circle's centre,
rim snapped to a quadrant → vertices on the rim, each a 4-departure junction),
of any polygon vertex snapped onto another curve, and of spokes from a shared
centre. The rule is over-broad: `sort_departures` orders distinguishable
tangents unambiguously, genuine coincidence is already diagnosed as
`AmbiguousJunctionOrder` (`:1046-1053`), and a genuinely pinched **union** is
already refused downstream by `compile_selected_profile`
(`ProfileCompileError::PinchedBoundary`, `crates/sketch/src/profile.rs:198-200`).
The arrangement already accepts the identical 4-departure topology when no
authored endpoint is present (`an_inscribed_circle_pinches_the_surround_into_four_corner_cells`).

**Secondary — no on-curve snap.** `query_snap_candidates`
(`crates/sketch/src/queries.rs:110-290`) offers endpoint, centre, midpoint,
quadrant, and intersection candidates only; `snap_point`
(`crates/sketch-ui/src/lib.rs:7357-7446`) then falls back to the visible grid,
which coarsens to 1/2/5 multiples when zoomed out (`:4676-4681`), or to the
raw pointer when Snap is off. A stroke that ends visually "on" an edge but
1e-7 inside it is a dangling bridge and yields one cell. T-junctions
themselves are handled correctly.

### 6.3 Fix

Remove the `edges.len() >= 4 && has_authored_endpoint` rule, keeping the
`departures_coincide` check. Update
`point_kissing_loops_are_rejected_without_suppressing_an_unrelated_cell`
(`crates/sketch/tests/analytic_arrangement.rs:255-269`) to expect three cells
and no `KissingJunction` (two corner-kissing squares become two selectable
cells; selecting both still fails with `PinchedBoundary`). Keep the enum
variant for serde compatibility and adjust the wording at
`docs/architecture/geometry-kernel/2d-sketch-tooling-plan.md:535`. Add the
inscribed-polygon and spokes cases as regressions.

For the secondary cause, add a lowest-priority nearest-point-on-curve snap
candidate (`SketchSnapKey::OnCurve { entity }`) to `query_snap_candidates`,
map it in `snap_point` ahead of the grid fallback, and surface the
`ZeroAreaCycle` diagnostic as a canvas hint.

## 7. Kernel review findings beyond the three reports

### 7.1 `FinishEdges` dispatcher executes each rung up to three times

`lib.rs:1223-1361` is a chain of `match` guards of the form
`Err(DomainUnsupported) if rung(..).is_ok() => rung(..).expect(..)` followed
by `Err(DomainUnsupported) if matches!(rung(..), Err(Specific)) => ..`. Per
request, `prism_edge_finish` runs up to twice, `section_revolve::build_rim_blend`
up to three times, and `rim_loop_blend` up to three times, each building a
full topology that is thrown away. The `faces.len() == 6` branch at
`:1363-1397` duplicates the `else` branch except for the validation filter and
successor fallback. Restructure as: call each rung once, bind its `Result`,
and match on it.

### 7.2 The v0.9.5 tessellation rewrite is fail-soft where it must not be

`536f3d6` replaced the ear clipper and hole-bridging in
`triangulate_face_boundaries` (`lib.rs:3616-3741`) and `ear_clip_polygon`
(`:3830-3940`):

- The bridge search now falls back to "closest candidate whose midpoint is
  inside the outer boundary" and then to **vertex 0** (`:3694-3705`),
  ignoring visibility. A bridge that crosses another hole produces
  overlapping triangles.
- When ear clipping stalls, the stitched (holed) polygon is fanned from
  vertex 0 (`:3732-3737`, `:3897-3905`). A fan over a non-simple polygon
  **fills the holes** the bridges were built to keep open. The comment that
  was deleted said exactly this: "Never fill a void".
- `1e-12` and `1e-10` are absolute area thresholds (`:3827`, `:3868`,
  `:3907`, `:3929`) on coordinates that may be metres or microns.
- `authoritative_scene` (`:2120`) uses the same function, so the faceted
  Boolean's operand and the STL export can inherit these triangles.

The rewrite is what made the two-hole cap display; the pre-rewrite clipper
dropped the whole face. Keep the display win but restore the contract: keep
the old fail-closed behaviour for `ChordBudget::Authoritative`, make any
display fallback relative to the polygon's bounding box, and never fan a
polygon that contains a bridge.

### 7.3 Approximation is not legible on the edge-finish path

See §5.3. One `approximation_warning` call site exists (`lib.rs:819`); the
faceted edge-finish arms (`:1165-1181`, `:1362-1406`) and
`finish_logical_successor_edges` (`:2852`) emit none.

### 7.4 Presentation heuristics live in the kernel

`presentation_prismatic_feature_roles` treats any feature role with two or
more distinct planar normals sharing an axis as one logical curved carrier
(`:2532-2547`), with `1.0e-8` and `1.0e-6` dot-product tolerances. The v0.9.2
change from eight normals to two is what makes faceted output display
differently. These decisions belong in `viewport`, keyed on exact carrier
identity from the topology, not on normal clustering inside the kernel.

### 7.5 Test quality

| File | Assertions | Note |
|---|---|---|
| `crossing_cut_three_holes.rs` | 2 (`> 0`, `!is_empty()`) | does not cross a hole (§4.2) |
| `three_holes_probe.rs` | 1 | |
| `intersecting_cut_chamfer_probe.rs` | 1 (`is_ok()`) | |
| `faceted_cylinder_triangular_cut_probe.rs` | 3 | |
| `finished_block_pocket_probe.rs` | 3 | |
| `pocket_finish_probe.rs` | volume within ±15–25 % (was 1 % before `2563d38`) | |

The programme rule is "no bug fix without a minimized permanent replay case";
these files are probes, not cases. None asserts surface kinds on a blend, and
no test finishes the rim of a hole.

## 8. New `crates/api` and `apps/api-server` (v0.9.5)

`cargo test -p artificer-api` passes (12/12) and clippy is clean. Findings in
priority order; each was confirmed against the built binary.

### 8.1 Bugs

1. **Process abort on nested input.** `scripting/parser.rs:257-261` and
   `scripting/mod.rs:154-203` recurse without a depth limit. A `script.run`
   with 100,000 nested parentheses overflows the stack and kills the server
   for every client. Add a depth counter to the parser and evaluator.
2. **Geometric selectors cannot round-trip.** `selectors.rs:16-31` and
   `:84-86` both use `#[serde(tag = "type")]`, and `ByGeometry` is a newtype
   variant, so serialization emits two `"type"` keys. `journal.export` after
   any `faces("+Z")` produces a journal that `journal` replay rejects with
   `duplicate field 'type'`, and every geometric selector sent over RPC is
   rejected as an unknown variant. This voids the "deterministic journal
   replay" claim for the DSL's main idiom.
3. **Sketch → extrude is unreachable.** `session.rs:367-370` returns
   `InvalidInput` for `ApiCommand::Sketch`, so it is never journaled, while
   `build_sketch_profile` (`:404-414`) looks the sketch up in the journal. The
   only test of this path (`tests/api_tests.rs:269-283`) pushes the entry into
   `session.journal` by hand.
4. **`top_face` is raw ordinal 1.** `selectors.rs:284-289` maps
   `"top_face"`/`"bottom_face"` to face ordinals 1/0 of the producing step.
   It works for a cuboid by construction order (`cuboid.rs:80-100`); after a
   drill it targets an arbitrary face. This is the raw-ordering dependency ADR
   0003 forbids, presented as the primary API idiom.
5. **Forward tracing returns stale entities.** `selectors.rs:343-368` keeps
   the previous target when a later step has zero candidates and re-stamps
   it with the current snapshot id; with several candidates it indexes a
   `BTreeSet` by history ordinal. `SelectorResolutionError::StaleReference`
   exists (`:161-162`) and is never constructed.
6. **Boolean `target` typo silently targets the current model**
   (`session.rs:186-190`, `unwrap_or(self.snapshot.id())`).
7. **Duplicate step labels accepted** (`session.rs:167-170`): `step_order`
   keeps both, the maps keep the last, `position()` finds the first.
8. **SVG depth order is inverted** (`snapshot.rs:298-306`): sorted
   front-to-back, so hidden faces overpaint visible ones.
9. **PNG returns SVG** (`snapshot.rs:195-198`); the server writes SVG text to
   a `.png` path. `Projection::Perspective`, `display_mode`, and
   `show_labels` are ignored.
10. **Malformed `snapshot` params are swallowed** (`server.rs:184`,
    `unwrap_or_default()`).
11. **`edges("|Z")` always fails**: the DSL builds `ByType { Edge }`
    (`scripting/mod.rs:445-448`) and the resolver rejects non-face `ByType`
    (`selectors.rs:587-592`). `ByType` ignores `surface_type` and returns the
    first triangle's face; `ByExtremum` ignores `metric`.
12. **Shipped example does not parse.** `examples/three_holes_and_cut.art`
    uses `=` arguments, `sketch()`, `.circle()`, `op="cut"`, none of which
    exist.
13. `MakeCylinder` frame is not orthogonal for tilted axes
    (`session.rs:249-264`), moot only because the kernel rejects a second
    constructor on a non-empty snapshot.
14. Sketch lowering concatenates every entity into one outer loop with no
    holes and ignores constraints, regions, operation, and revolve axis/angle
    (`session.rs:381-393`, `:447-502`).
15. JSON-RPC framing: notifications get responses, batches return `-32700`,
    `jsonrpc` is unchecked, `read_line` is unbounded, non-UTF-8 input
    terminates the server (`server.rs:89-106`, `:257-264`).
16. A second primitive silently replaces the model (`session.rs:173`).
17. `undo` pops the journal unconditionally (`session.rs:544`);
    `from_journal` ignores `schema_version`.

### 8.2 Design concerns

- **Duplication of the document layer.** `session.rs` re-implements
  history/undo, `selectors.rs` re-implements persistent references with
  string heuristics (versus `crates/model/src/persistent.rs`),
  `scripting/mod.rs` re-implements parameter evaluation (versus
  `model/src/parameters.rs`), and `journal.rs` re-implements
  `testkit::CommandJournal` (`crates/testkit/src/lib.rs:329-359`), whose
  `replay_journal` actually verifies snapshot ids. The API journal stores no
  expected digests, so replay checks nothing.
- **No concurrency or cancellation.** One `Mutex<Session>` is held for the
  whole kernel call; `CancellationToken::default()` is created per request
  and never exposed; there are no timeouts; a panic in a kernel op aborts the
  stdio server. There is no network binding at all: `serve` ignores `--port`.
- **Ties resolve by raw entity id.** `FaceByNormal`, `NearestTo`, and
  `ByExtremum` pick the lowest `EntityId` on ties instead of reporting
  ambiguity; each resolve re-tessellates the model.
- `Session` exposes every field as `pub`, so its invariants are unenforceable
  (the tests already mutate `journal` directly).

### 8.3 README claims not backed by code

- "`--port 9000` … on localhost": stdin/stdout only.
- `params.command.kind = "make_box"`: actual shape is flat `params` with
  `"type": "make_box"`.
- `base.top_face` and `drill_hole(...)`: the grammar has `base.face("top_face")`
  and `drill(...)`.
- `session.query().mass_properties(None)`: no such method.
- "STL, OBJ, and STEP interchange": STL and OBJ only; the roadmap itself lists
  STEP as unchecked.

## 9. Recommended order of work

1. **Sketch: delete the kissing-junction rule** (§6.3). One guard, one test
   update, highest user-visible value per line.
2. **Hole rims: extend `rim_loop_blend` to inner loops, group hole rims, warn
   on faceted finishes** (§5.4 A, C, D). Circular and tangent-continuous
   holes become exact for both fillet and chamfer; square-hole chamfers follow
   with B; square-hole fillets wait for ellipses.
3. **Faceted assembler: Newell normals, requeue instead of drop, orient the
   healer** (§4.4). This is the only tier that can answer a perpendicular
   round-on-round cut, so it has to stop losing slivers.
4. **Restore fail-closed authoritative tessellation** (§7.2) and make the
   display fallback scale-relative.
5. **Collapse the `FinishEdges` guard chain** to one call per rung (§7.1).
6. **API crate:** fix the abort, the selector serde, and the sketch path;
   then decide whether the crate should be rebuilt on `model` and `testkit`
   rather than beside them; correct the README.
7. **Tests:** convert the probe files to cases with closed-form assertions;
   add the reproductions in §4.2, §5.2, and §6.1 as permanent regressions.

# Simplicity and modularity review — 2026-09-23

Status: review record, no code changed
Scope: the whole workspace at `0942dbb` (v0.99.81): `crates/*`, `apps/*` and
the `addons/scan` workspace, about 240k lines of Rust. The question asked of
every file was: what is here that makes the code larger, or harder to change,
than what it does requires?

Seven reviewers each read one area whole (functions, not grep excerpts):
kernel core (`lib.rs`, `validator.rs`, `topology.rs`), kernel geometry and
construction modules, kernel `api/` and the small apps, the workbench, the
sketch/UI/viewport crates, the model and shared crates, and the scan add-on.
Their findings were merged and ranked here; a handful of the strongest claims
(the builtin-name gap, the unused 3-D geometry, the duplicated helpers) were
re-checked against the tree. Line numbers are anchors into `0942dbb`.

## 1. Verdict

The layering the ADRs describe is real and holds everywhere it was checked:
the boundary script passes, no presentation crate executes the kernel, the
model never sees the kernel, and every operation still goes through a
"certified or refused" ladder. Nothing found here is a boundary violation.

What has happened instead is accretion along two axes:

- **Files that were never split.** Five files carry a third of the tree:
  `apps/workbench/src/lib.rs` (35.9k lines, 20k of them one `impl` with 492
  methods, 10k of tests), `crates/sketch-ui/src/lib.rs` (22.9k, 6.7k tests),
  `crates/kernel/src/lib.rs` (14.2k, 3.9k tests), `crates/viewport/src/lib.rs`
  (12.1k, 3.9k tests) and `crates/model/src/lib.rs` (7.9k). Each already
  contains ten or more natural modules; the split is a move, not a redesign.
- **The same flow written once per feature.** The workbench's stage → preview
  → commit → edit-in-place sequence exists five times and its "publish the
  body" tail nine times; the kernel's Boolean ladder three times and its
  error-to-code tables eleven times; the model's recipe version/validation
  machinery seven times; a topology builder six times; a profile-space line
  fit five times in the scan add-on. Adding a feature today touches six to
  twelve places that should be one or two.

Two smaller patterns run through everything: the vocabulary is defined more
than once (`Point3`/`Vector3` in three crates with about 125 conversions and
some thirty private `dot`/`cross`/`unit` helpers; point–segment distance
thirteen times; point-in-polygon ten; ear clipping three; a hex digest type
four), and each new rung brought its own rule for "these two points agree"
(at least nine formulas in the kernel alone).

A rough total: on the order of 15–20k lines could be removed and another
30k moved into modules, at low risk for most of it, because the integration
suites pin behaviour closely. One correctness bug fell out of the review
(section 3.3, item 3): the script builtin list omits `loft`, `plane` and
`axis`, so a user `fn loft(...)` silently replaces the real one.

## 2. Cross-cutting themes

### 2.1 Monoliths, and tests inside them

| File | Lines | Of which tests | Natural modules already inside |
|---|---|---|---|
| `apps/workbench/src/lib.rs` | 35,937 | 9,989 | extrusion, edge finish, plane, timeline, documents/preferences, view cube, interference, transform, selection |
| `crates/sketch-ui/src/lib.rs` | 22,935 | 6,699 | context, geometry, recipe editor, dimension session, recipes, profile, snap, creation/pending/regions/relations/modifiers state, canvas, paint, dimension widgets, hit test |
| `crates/kernel/src/lib.rs` | 14,206 | 3,914 | tessellation, triangulation, presentation, history, digest, error tables |
| `crates/viewport/src/lib.rs` | 12,086 | 3,919 | camera input, projection, edges, picking, section, feature preview, datum, paint |
| `crates/model/src/lib.rs` | 7,873 | — | document setters, replay, load validation |
| `addons/scan/…/rebuild.rs` + `reconstruct.rs` | 10,100 | — | one 1,970-line `rebuild_sharp` and four 375–640-line stages |

Every one of these splits is a set of `impl` continuations in new files
(`loft.rs` in the workbench and `describe.rs` in the kernel already show the
pattern) plus `#[cfg(test)] mod tests;` pointing at `src/tests/*.rs`, which
keeps private access. The workbench's boundary tripwire counts execution
sites in `lib.rs`, so it is updated in the same change.

### 2.2 One flow, N copies

| Where | Copies | What is repeated |
|---|---|---|
| Workbench staged features (`loft.rs:274–623`, `revolve.rs:804–1156`, `sweep.rs:367–657`, plane `lib.rs:9621–9815`, axis `construction_axis.rs:331–521`) | 5 | eleven-method stage/preview/commit/edit/cancel protocol, ~900 lines, plus six dispatch sites per feature |
| Workbench "publish the body" tails (`lib.rs:7542, 9060, 12832, 13128, 14441, 14996`, three feature modules) | 9 | update bodies/archive/displayed/pivot/status, three then call `restore_runtime_from_document` which redoes it |
| Kernel Boolean ladder (`lib.rs:1263–1658` twice inline, `2296–2516`, `4499–4639`) | 3–4 | prism → coaxial → analytic → faceted with `ExactRouteDecline` labelling |
| Kernel error tables (`lib.rs:7270–7998`) | 11 enums | ~750 lines ending in the same `error(code, Preflight, …, simple_diagnostic(…))` tail; `shell.rs` already shows the `code()/message()` alternative |
| Kernel topology builders (`section_revolve.rs:1058`, `rim_loop_blend.rs:315`, `vertex_blend.rs:1402`, `exact_face_feature.rs:1649`, `analytic_extrusion.rs:2229`, `extrusion.rs:614`) | 6 | `allocate`, `vertex`, `edge`, `push_loop` (four byte-identical copies), `finish`; `merge_topologies` twice |
| Kernel prism extraction (`prism_edge_finish.rs:237`, `prism_boolean.rs:126`, `shell.rs:266`) | 3 | same cap-and-walls reading, three result types, three tolerances |
| Model recipe types (`loft.rs`, `revolve.rs`, `sweep.rs`, `datum.rs`, `datum_axis.rs`, `sketch_region.rs`, `sketches.rs`) | 7 | version constant + serde default + `UnsupportedVersion` check, all at version 1; region-selection validation four times; `ReplayAction` matched exhaustively in seven `lib.rs` functions |
| Model setters (`lib.rs:1004–2385`) | 17 | clone → check → mutate → `finish_user_edit`, 29 `state.clone()` |
| API compile-then-execute (`report.rs:349` canonical; `server.rs:705`, `api-server/main.rs` ×3, `script-studio/lib.rs:345`) | 6 | script-studio also re-derives face names, failures and label lines the session reports |
| Scan profile fits (`finalize.rs:490`, `consolidate.rs:388`, `reconstruct.rs:673`, `blend.rs:231`, `reconstruct.rs:281` with a private 4×4 solver) | 5 | area-weighted `rho = a + b·z` fit and the line→cone/cylinder conversion (×4) |
| Scan feature sampling (nine sites) and `FeatureRecord` bookkeeping (39 sites) | 9 / 39 | faces → datum-frame samples; `area`/`face_count` recomputed by hand |

### 2.3 Vocabulary defined more than once

- `Point3`/`Vector3`: `protocol/lib.rs:1257,1299`, `geometry/foundation.rs:68,109`,
  `kernel/topology.rs:44,85`; ~125 `Point3::new(p.x, p.y, p.z)` conversions in 31
  files; private `dot`/`cross`/`unit`/`normalise` in `model/datum.rs:571`,
  `revolve.rs:434`, `sweep.rs:241`, `sketch_region.rs:139`, `api/planes.rs:26`,
  `api/probe.rs:715`, `api/interference.rs:1122`, `edge_finish_apart.rs:688`,
  `kernel/revolve.rs:542`, `viewport/lib.rs:7402`, `workbench/lib.rs:26578`, and
  more. Protocol's types have serde and nothing else; every consumer either
  converts or reinvents.
- Planar helpers: point–segment distance in six kernel modules
  (`analytic_extrusion.rs:1113`, `extrusion.rs:404`, `planar_profile.rs:294`,
  `face_feature.rs:571`, `spline_profile.rs:854`, `vertex_blend.rs:2436`) and six
  UI sites; point-in-polygon in seven kernel modules and three UI ones (with
  two boundary conventions); segment intersection ×4; loop signed area ×7 (two
  conventions); ear clipping ×3; Newell normal ×2; `corner_blend.rs:143` and
  `loop_offset.rs:227` each define a private `Vector2` and the same three
  carrier intersections (~260 lines) although `topology::Vector2` and
  `artificer_geometry::Vector2` exist.
- `analytic_extrusion::Frame` duplicates `topology::Plane`; `face_feature::Basis`
  is a third; the `Point2::new(offset.dot(u), offset.dot(v))` projection is
  written 18 times although `Plane::project` exists.
- Hex digests: `SemanticDigest`, `ParameterBindingDigest`, `ComponentContentDigest`,
  `catalog::ContentDigest`, ~55 lines each; `ComponentDefinitionRevision` and
  `validate_definition_key` are copies of catalog's `PartRevision` and
  `validate_path_identifier`.
- Script vocabulary: the parse tables (`scripting/mod.rs:2752–2966`), the
  decompiler's inverse tables (`decompile.rs:644–766`), `BUILTINS`
  (`mod.rs:519`), the "Unknown function" message and the studio highlighter each
  spell the same words; three of the five have drifted.

### 2.4 Two implementations of one thing

- **Linear extrusion pipeline** (`extrusion.rs`, `planar_profile.rs`,
  `face_feature.rs`, ~3,000 non-test lines) beside the analytic pipeline that
  already handles lines; `planar_profile.rs` calls itself a compatibility
  surface and builds holes by running a face-feature cut through the outer
  prism.
- **Workbench replay**: `rebuild_document_from` (`lib.rs:6879–7339`) and
  `document_replay::hydrate_model_document` (`279–500`) replay the same action
  kinds; only the former resolves datum planes and remeasures, so a rebuilt
  document and an opened one take different paths.
- **Sketch-ui legacy layer**: `SketchTool`/`SketchGeometry` beside
  `ToolVariant`/`CoreRecipe`, bridged on every edit; and a second loop-finder and
  curve-intersection library (`lib.rs:11276–12440`, ~1,500 lines with its own
  tolerances) beside the sketch crate's arrangement, both run on every refresh.
- **Model load vs append**: `validate_loaded_state` (570 lines) re-implements
  the rules `append_feature` enforces, with 45 string errors instead of the
  typed ones.
- **Validator measures**: ~370 lines of planar measure strategies
  (`validator.rs:3375–3745`) that the early return at `2317` makes unreachable.
- **Geometry crate**: ~1,900 non-test lines of 3-D Bezier/B-spline/NURBS
  curves, surfaces, tessellation and unit types with no user outside the crate;
  the kernel has its own `bspline.rs`.
- **RANSAC**: `ransac::Primitive` (`ransac.rs:92–220`) is `SurfaceClass` with its
  `probe` copied line for line.

### 2.5 Tolerance conventions

Nine formulas for "same point" in the kernel's construction modules
(`1e-9·scale`; `linear_agreement.max(1e-9)·scale`; `…·(1+height)`;
`linear_agreement.max(1e-12)·scale`; `…·radius.max(1)`; `·32`; `·8`; `·128`;
bare `linear_agreement`), "scale" computed four ways, and angular agreement
floored at `1e-12` in seven places and `1e-9` in eight. A body one rung accepts
can be refused by the next for no geometric reason. The programme README's own
rule is "not a small epsilon everywhere".

## 3. Findings by area

Each item: what, where, the simpler shape, effort. Ranked within its area.

### 3.1 Kernel core (`lib.rs`, `validator.rs`, `topology.rs`)

1. One `boolean_ladder(input, tool, op, labels, faceted: Option<…>)`; the
   face-feature arm (`lib.rs:1263–1658`, two inline copies) drops to ~60 lines
   and `execute_boolean` passes no faceted tier. The face-feature faceted tier
   keeps `subtract_crossing_profile` with the request budget (the NOTE at
   `1408–1419`). 2–3 days.
2. A crate-private `Refusal { code, diagnostic, message }` trait beside each
   error enum; `lib.rs` keeps one `refuse(snapshot, &impl Refusal)`; ~650 lines
   leave `lib.rs` (`7270–7998`). Wire codes stay byte-identical. 1 day.
3. `execute` (`811–2228`, 1,417 lines): `preflight`, `commit` with one
   `Snapshot::sealed` constructor (five hand-built `Snapshot {..}`, two with
   default measures), `sub_request`, and one `build_<command>` per arm living
   in its module; `FinishEdge` delegates to `FinishEdges`. 2 days.
4. Move tessellation (`5204–6625`), triangulation (`6627–7126`), presentation
   (`3487–4135`), history (`8469–9786`) and digest (`9893–10290`) into their own
   files; `lib.rs` becomes a ~4k facade. 1 day.
5. `Face::parameter_bounds()` in `topology.rs` replacing seven copies
   (`lib.rs:5852` ≡ `validator.rs:3357`, five inlined). 2 hours.
6. Delete the unreachable planar measure strategies (`validator.rs:3375–3745`)
   after a coverage run confirms no planar body carries circle edges. ½ day.
7. `Topology::entities()`/`zip_entities()` replacing seven hand-unrolled
   seven-collection loops in history and identity (~350 lines). ½ day.
8. `ExactRouteDecline::refusal()` so `execute_boolean` stops re-deriving
   the carrier-pair prose (four copies of one sentence). 2 hours.
9. `From` impls between protocol and kernel point/vector types, `Vector3::unit()`,
   a `frame_axes` helper; delete `internal_protocol_point` (a duplicate of
   `internal_point`). ½ day.
10. `validator::certify(..) -> Result<Report, Report>` returned up the ladder;
    the winner is validated at commit again today (18 `validate(..).diagnostics.is_empty()` sites). ½ day.
11. Move the 41 black-box tests in `lib.rs` to `tests/`, keep the nine that need
    private items beside their modules; a `run(snapshot, command)` helper halves
    the 200-line test bodies. 1 day.
12. Retire narrative comments and stale `#[allow(dead_code)]` (`topology.rs:1022`
    says no sphere builder exists; ten modules build spheres). 2 hours.

### 3.2 Kernel geometry and construction modules

1. `topology::TopologyBuilder` and `Topology::append(other, Renumber)`
   replacing six builders, two `merge_topologies`, `pattern::merge_disjoint`
   and three `next_entity_id`s (~600 lines). Entity order preserved. 1–2 days.
2. One `planar.rs` (or additions to `artificer_geometry`): point–segment
   distance, `segments_intersect(collinear_counts)`, point-in-polygon with one
   chosen boundary rule, signed area, segment–segment distance, Newell normal;
   `Vector3::unit()`; `point_inside_loop(&[Segment])` so callers stop building
   fake `AnalyticLoop`s. ~700 lines. 1 day.
3. Merge `corner_blend.rs` and `loop_offset.rs`'s private vector types and
   carrier intersections into one `offset_carrier.rs`; `analytic_extrusion`'s
   third line/arc intersection uses it. 1 day.
4. Split `validate_analytic_profile_extrusion` into `parse_profile` and
   `certify_and_build(regions, frame, distance)` so `prism_edge_finish`,
   `prism_boolean`, `edge_finish` and `shell` stop converting `Segment`s to
   protocol and back (lossy for arcs; ~150 lines of converters). 1–2 days.
5. One `extract_prism(topology, PrismAxis::{ByRole, Along, ThroughCap})`
   replacing three extractors; with `Along` available, `edge_finish.rs`
   (412 lines, cuboid-only) becomes a redundant rung. 1 day.
6. Route `ExtrudePolygon`, `ExtrudeFaceProfile` and the linear arm of
   `ExtrudePlanarProfile` through the analytic pipeline and delete
   `extrusion.rs`, `planar_profile.rs`, `face_feature.rs` (~3,000 lines).
   High risk: `HistoryMode::Extrusion { profile_vertices }`, exit faces and
   canonical vertex rotation are visible to persistent references and
   digests. Needs a fixture-diff campaign. 1–2 weeks.
7. `PrecisionPolicy::point_agreement(scale)`, `angle_agreement()`,
   `Topology::coordinate_scale()`; weld multipliers become named constants.
   One rung at a time with the case runner. 2–3 days.
8. One `nest_loops(loops, SliverGate)` and one `chain_cycles` (the fixed-
   successor `halfedge_cycles`; `profile_boolean::chain_pieces` still walks the
   order-dependent way its own test warns about); delete two copies of
   `Segment::reversed`. 1–2 days.
9. Split the nine 300–580-line plan-and-build functions
   (`build_filleted_prism`, `build_hole_rim_blend`, `validate_revolve`,
   `topology_from_polygons_with_heal_limit`, `sweep_section`,
   `validate_face_push_pull_input`, `glue_layers`, `reparameterize`,
   `prism_boolean_along`) into `plan_*` + `build_*` as `vertex_blend` already
   does. ½–1 day each.
10. Delete `analytic_extrusion::Frame` in favour of `topology::Plane` (+`lift`),
    and the 18 hand-written projections. ½ day.
11. Dead code: `exact_face_feature::keys_height` (returns 0 from unused
    parameters), `push_cap_loop`'s discarded arguments, `SpineVertexKind`'s
    unread variants computed in `mitred_offset`, the discarded `overlap` in
    `prism_boolean_along:245`. 1–2 hours.
12. `Topology::edge_incidence()` replacing `lib.rs:3777`,
    `vertex_blend::incidence` and `push_pull::face_owners_of_edge`; let
    `vertex_blend` return its candidate rather than validating inside the rung.
    ½ day.

### 3.3 Kernel API, scripting and the small apps

1. One `StepRecord` per step instead of six parallel `pub` maps on `Session`
   (`session.rs:31–58`), one `commit_step`, and `Session::resolve(&selector)`
   replacing the three-argument tuple threaded through 13 sites in session.rs
   plus `query.rs`, `probe.rs`, `report.rs`. 1–2 days.
2. Six compile-then-execute loops become `session.run_script_with(..)`;
   script-studio drops `name_faces`, `RunError::from_step`, its own
   `line_of_label` (different rules from the report's) and `describe_face`
   (~220 lines). 1 day.
3. **Bug.** `BUILTINS` (`scripting/mod.rs:519–561`) lacks `loft`, `plane` and
   `axis`; `declare_function` refuses only names in that list and user functions
   are tried first, so `fn loft(...) {}` shadows the real builtin. Fix by one
   `BUILTIN_NAMES` table that `build_builtin`, `declare_function`, the
   "Unknown function" message and the studio highlighter all read, and
   `script_word()/from_script_word()` on each selector enum shared by the
   parser and decompiler. ½ day.
4. `impl Vector3 { dot, cross, length, normalized }` on protocol types (or an
   `api/geom.rs`), one `closest.rs` for the three copies of closest-point-on-
   triangle and two each of Möller–Trumbore and segment–segment. ~150 lines.
   1 day.
5. `SharedSession::dispatch` (`server.rs:255–755`): a `params<T>()` helper and
   arms returning `Result<Value, ApiError>`; ~500 → ~200 lines. ½ day.
6. Dead surface: `SelectorResolutionError`, `Session::labels`, the wire-level
   `SketchConstraint` that is always empty and never read, glob re-exports in
   `api/mod.rs`. 2–3 hours.
7. The decompiler's `Writer::direct` re-walks history that
   `Session::history_names` already indexes, with a different rule for
   edge-finish roles. 2–3 hours.
8. `Args::point2_or`, `bool_or`, `two_selectors`; move the `spline`, `pattern`
   and `revolve` arms out of `build_builtin` like `plane`/`axis`. 2–3 hours.
9. Split `pattern_instances` (285 lines) and `lower_command` (303). ½ day.
10. `query::target_point` recomputes centroids `describe_face` already gives;
    `features()` reports a `Debug` dump instead of `command.kind()`. 1–2 hours.
11. Testkit's second SVG renderer (~180 lines) versus `api/snapshot.rs`; only
    worth folding if goldens are cheap to regenerate. ½ day.

### 3.4 Workbench

1. A `StagedFeature` trait (recipe, kind, body kind, inputs, consumed
   sketches, label) with one generic refresh/commit/begin-edit/apply-edit/
   cancel, one `staged: Option<Box<dyn StagedFeature>>` field and one
   `PendingOperation::StageFeature`; loft, revolve, sweep, plane and axis keep
   their selection conversion, card and picks. ~900 lines. 2–3 days.
2. One `publish_committed_feature(appended, outcome, kind, status)` calling
   `restore_runtime_from_document`; one `ModelBodyKind::for_feature`. ~300
   lines. The extrusion path relies on selection surviving mid-flow, so it
   needs care. 1–1.5 days.
3. `#[cfg(test)] mod tests;` with one file per current test module
   (`extrusion_workbench_tests` alone is 6,638 lines). ½ day.
4. Drop `Copy` from `PendingOperation` so the extrusion editor's 16 loose app
   fields become the variant's payload and `sync_pending_sketch_extrusion_inputs`
   (a third copy of the mode rules) disappears; same for `EdgeFinishEditor`.
   ~30 fields fewer on `KernelLabApp`. 2 days.
5. One `replay_feature(ctx, feature, action)` in `document_replay.rs` used by
   both `hydrate_model_document` and `rebuild_document_from` (which repeats its
   failure block eleven times), so opening a file and rebuilding history take
   one path, with datum resolution and remeasurement in both. 2–3 days.
6. Module split of `lib.rs`: `extrusion.rs` (~2,600), `edge_finish.rs`,
   `plane.rs`, `timeline.rs`, `documents_io.rs`/`preferences.rs`, `view_cube.rs`,
   `interference.rs`, `transform.rs`, `selection.rs`; `lib.rs` lands at 8–10k.
   Update the tripwire's counts. 1–2 days.
7. `controls()` (510 lines) is twelve identical `if shows && matches!` card
   blocks; `sketch_inspector` guards cards with constants that are always
   true. ½ day.
8. Geometry computed in UI code: preset defaults (hole ring layout, shell
   wall, pattern spacing at `lib.rs:14768–14900`), `sample_planar_loop`
   (re-evaluates B-splines), point-in-loop, segment distance, face-support
   re-framing (~800 lines); move to `artificer_sketch`, `artificer_geometry`
   and the kernel API. 1–2 days.
9. `FeaturePreviewState` is a hand-maintained cache of `document.features()`
   that `sync_feature_preview_from_document` fully recomputes anyway; derive
   it. 1 day.
10. `handle_shortcuts` re-encodes keys the commands table already lists;
    route through `run_command` and `command_availability`. ½ day.
11. Six unreferenced `pub` methods (`save_native_document_to_path`,
    `load_native_document_from_path`, `drain_committed_part_insertions`,
    `sketch_view_quarter_turns`, `sketch_pending_geometry`,
    `sketch_canvas_instruction`); confirm whether the armed-tool path is live.
    30 minutes.
12. `gate_button`/`accessible_button` helpers for the ~60 repetitions of the
    button + `widget_info` + tick/cross idiom. ½ day.

### 3.5 Sketch, sketch-ui, ui-core, viewport, spacemouse

1. Split `sketch-ui/src/lib.rs` into the sixteen files listed in section 2.1
   (pure moves, ~120 `pub(crate)` markers). 1–2 days.
2. Derive `CertifiedProfileStatus`, `ProfileDiagnostics` and
   `certified_sketch_profile` from the `SketchArrangement` the state already
   builds; delete the legacy loop-finder, nesting and intersection library
   (`lib.rs:11276–12440`, `4923–5375`). Needs a comparison run of both
   classifiers over the fixtures first. 3–4 days.
3. Make `ToolVariant` the only tool enum and the presentation entity a view
   over `authoring`, retiring `SketchTool`/`SketchGeometry` and the two-way
   bridges (`core_recipe_for_entity`, `legacy_geometry_from_core`,
   `rebuild_presentation_from_authoring`, the 164-line `reshape_core_recipe`).
   1–2 weeks; after 1.
4. `viewport::show_document_impl` (1,111 lines, 25 parameters, 25
   `too_many_arguments` allows): a `SceneFrame` built once with four phase
   methods (input, project, pick, paint) and a module split; drop the
   `show → show_with_feature_preview → show_with_document_overlays →
   show_document` wrapper chain that only tests call. 2–3 days.
5. A `DraftShape` trait per creation phase replacing eight copies of the
   two-click state machine in `handle_creation_click` (354 lines) and the four
   parallel `(phase, geometry)` tables in `DimensionSession`. 2–3 days.
6. Move the 12k lines of in-file tests out of sketch-ui, viewport and ui-core;
   delete production items that exist only for tests (`#[cfg(test)]`
   triangulators, `prepare_feature_preview`, legacy preview fields). ½ day
   per crate.
7. Planar helpers into `artificer_geometry` (already a dependency of all
   three crates): point–segment distance (×6 across viewport, sketch-ui,
   workbench), ear clipping (×3, keep the viewport's fallback version),
   point-in-polygon, point-in-triangle, segment intersection; quaternion
   helpers from ui-core made public and the viewport/workbench copies deleted.
   1 day.
8. The GPU path builds the CPU face mesh it then discards and clones every
   body's `DebugScene` into the paint callback per frame; choose the backend
   first, `Arc` the scene, recompute colours only on revision change; delete
   the unused duplicate `viewport::FillBackend`. ½ day.
9. Replace `pub use module::*` in `sketch/lib.rs` (89 of 298 pub items unused
   outside), and mark sketch-ui's 75, ui-core's 29 and spacemouse's 12 unused
   pub items `pub(crate)`; make `viewport::gpu` private. ½ day.
10. `dimension_box`/`claim_keys` helpers for the three copies of the
    TextEdit + plate + accesskit + Enter/Escape block inside the 380-line
    `show_dimension_widgets`. 1 day.
11. Stale `#[allow(dead_code)]` "next integration step" markers on code the
    workbench now uses (`FeaturePreviewStyle`, `RigidOccurrenceTransform`,
    `DocumentBodyInstance`), and genuinely dead prism index builders. 1–2 hours.
12. `sketch::primitives::evaluate_recipe` (729-line match) into one function
    per recipe family. 1 day.

### 3.6 Model, protocol, geometry, catalog, compute

1. Delete the unused 3-D parametric module and foundation types in
   `crates/geometry` (~1,900 non-test lines); the crate becomes exact 2-D
   predicates plus 2-D splines. 2–4 hours.
2. Arithmetic (`Add/Sub/Mul/Div`, `dot/cross/length/unit`) on protocol's
   `Point3`/`Vector3`; geometry and the model re-use them; ~120 lines of model
   vector helpers and `sketch_region`'s second offset/height functions go.
   Kernel unification is a separate job because `mul_add` vs plain multiply
   shifts last-bit digests. 1 day for protocol + model.
3. One `ModelDocument::edit(|state| ..)` for the seventeen copy-mutate-commit
   setters; the seven boolean flags collapse to `set_flag(Target, Flag, bool)`.
   ½ day.
4. Load = replay: `from_native` feeds stored nodes through the same
   `append_node` as `append_feature`, then checks the reconciled state equals
   the stored one; `validate_loaded_state` shrinks from ~570 to ~80 lines with
   typed errors. 2–3 days.
5. A `RecipeVersion<const V>` newtype, one `persistent::check_reference`, one
   serde-helpers module: seven copies of version/lineage/serde machinery go.
   ½ day.
6. `SketchLoftSection::{validate, resolve}` as the profile field of extrusion,
   revolve, loft and sweep (`#[serde(flatten)]`); one `RegionSelectionError`
   instead of sixteen variants, one resolve prologue instead of five. 1 day.
7. A `Recipe` trait (`kind`, `validate`, `required_inputs`,
   `followed_expression`, `resolve`) so the seven exhaustive `ReplayAction`
   matches in `lib.rs` become one-line delegations and the five
   `Invalid*Feature` errors become one. 2 days.
8. Protocol: five hand-written bounded-sequence visitors are copies of the
   generic `BoundedVisitor`; the 280-line budgeted profile visitor duplicates
   caps the types and the model already check. ~550 → ~60 lines. 2–3 hours.
9. One `ParameterExpression::evaluate(lookup)` for the two identical walks;
   re-export the sketch crate's expression error and unit types instead of
   mirroring them. 2–3 hours.
10. One `Digest32` (export protocol's `fixed_hex_id!`) for four digest types;
    move `PartRevision` and the path-identifier rule to protocol so model and
    catalog share them and the workbench conversion disappears. ½ day.
11. Vestigial: seven document-version constants that gate nothing, a
    three-deep `resolve_sketch_regions*` wrapper chain, two uncalled `rebind_*`
    functions, `DatumAxisError` duplicating `DatumPlaneError`, and
    `ParameterizedKernel` (474 lines) which predates ADR 0052 and may be
    load-only legacy. 2–3 hours.

### 3.7 Scan add-on

1. `rebuild_sharp` (1,970 lines) and the 375–640-line stages of `rebuild.rs`
   and `reconstruct.rs` into a `rebuild/` directory with a `RebuildContext`
   and one file per emission stage. 3–4 days.
2. `profile.rs`: `ProfileSample`, `fit_line`, `fit_arc`, `fit_revolved_4p`,
   `Line::to_surface`, replacing five profile fits, four line→surface
   conversions, the private `solve_4x4` and a third circle fit. ~300 lines.
   1–2 days.
3. Delete `ransac::Primitive` and its copied `probe`; candidates build
   `SurfaceClass` directly. ~130 lines. ½ day.
4. `numeric` gains `scatter_3x3`, `NormalEquations<N>` and one `Rng`; delete
   the second eigen solver, the cofactor inverse and three SplitMix64 copies;
   use `orthonormal_basis` at its 18 hand-rolled sites. 1 day.
5. `FeatureSamples::gather(mesh, faces, frame, budget)` replacing nine
   sampling variants (one of which judges a locked refit on a different point
   set from the free fit it is compared with). 1–2 days.
6. `DeviationStats::weighted(..)` and `SurfaceClass::deviation()` replacing
   twelve hand-rolled `(Σw r², Σw, max)` sites. ½ day.
7. CLI: one `run_reverse` shell instead of five, a shared `Scorecard` for the
   coverage block the bench duplicates, one decimation and one `load_mesh`.
   ~250 lines. 1 day.
8. `Occupancy` as a type built once; the CLI currently transforms the scan
   five times and rebuilds the grid three times for one printout. ½ day.
9. `FeatureRecord::set_faces` recomputing area; replace the `face_count` field
   (which already drifts at `reconstruct.rs:1671`) with `faces.len()`. ½ day.
10. `SurfaceClass::describe(Detail)` and `axis()` replacing three label
    formatters and two direction accessors. 2–3 hours.
11. Delete `Region::vertex_points`/`face_normals`, `bspline::interior_knots`;
    `pub(crate)` the in-module items; an `unreachable_pub` pass over the 36
    fully-public modules. 1–2 hours.
12. The STEP writer and PNG encoder duplicate the kernel's on purpose (the
    add-on depends only on `geometry`); a small crate below the kernel for the
    dependency-free STEP entities and the PNG encoder would end that. Lowest
    priority. 1–2 days.

## 4. A plan

Ordered so that each step makes the next one smaller and safer. Line counts
are the reviewers' estimates.

**Phase A — mechanical, low risk (about two weeks).**
Tests out of production files in all five monoliths (3.1.11, 3.4.3, 3.5.6);
module splits of the workbench, sketch-ui, viewport and kernel `lib.rs`
(3.4.6, 3.5.1, 3.5.4's split, 3.1.4); delete what nothing uses (3.6.1, 3.1.6,
3.4.11, 3.5.11, 3.3.6, 3.2.11, 3.6.11, 3.7.11); explicit re-exports and
`pub(crate)` (3.5.9); the builtin-name table and the shadowing fix (3.3.3).
About 30k lines move and 3–4k go, with no behaviour change beyond the bug fix.

**Phase B — shared shapes (four to six weeks, medium risk, all pinned by
the existing suites).**
Kernel: the `Refusal` trait (3.1.2), `preflight`/`commit` (3.1.3), one
Boolean ladder (3.1.1), `TopologyBuilder` (3.2.1), planar helpers and
`Frame → Plane` (3.2.2, 3.2.3, 3.2.10), `extract_prism` (3.2.5), `certify`
once (3.1.10). Workbench: `StagedFeature` and one publish tail (3.4.1, 3.4.2),
the editor payloads (3.4.4), derived timeline state (3.4.9), shortcuts through
the table (3.4.10). Model: `edit()`, `RecipeVersion`, `SketchLoftSection`,
`Recipe` trait, bounded visitors, `Digest32` (3.6.3, 3.6.5–3.6.8, 3.6.10).
API: `StepRecord` and one run loop (3.3.1, 3.3.2). Scan: `profile.rs`,
`numeric`, `FeatureSamples`, `Occupancy` (3.7.2–3.7.6, 3.7.8). Sketch-ui:
`DraftShape` (3.5.5). Roughly 8–10k lines removed.

**Phase C — one implementation each (each a fixture-diff campaign, taken
one at a time).**
Retire the linear extrusion pipeline (3.2.6); one workbench replay path
(3.4.5); the sketch-ui legacy vocabulary and loop-finder (3.5.2, 3.5.3); load
= replay in the model (3.6.4); one agreement rule in the kernel (3.2.7);
`rebuild_sharp` (3.7.1); one `Point3`/`Vector3` across the kernel (3.6.2's
second half). Roughly 6–8k lines removed and the largest remaining risk of
divergence with them.

## 5. What was checked and found fine

The layering itself, everywhere: the boundary script passes and no reviewer
found a presentation crate executing the kernel, the model reaching the
kernel, or the kernel reaching UI. In the kernel, `topology.rs`, the
validator's four families and its divergence-theorem measure engine,
`surface_intersection.rs`, `cylinder_trace.rs`, `bspline.rs`, `ruled.rs`,
`coaxial_boolean.rs`, `pattern.rs`, `cuboid.rs`, the `ToolBoolean` label table
and `regularized_edge_finish` are the shapes the rest should copy. The
scripting lexer, parser and evaluator, `analysis.rs`, `sweep.rs`,
`interference.rs`, `journal.rs`, `diff.rs`, `export.rs` and the two thin CLI
fronts are proportionate. The workbench's `commands.rs`/`ribbon.rs` are the
table ADR 0028 promised; `invocation.rs`, `feature_editor.rs`, `browser.rs`,
`document_replay.rs`'s error and provenance handling, `documents.rs`,
`shell.rs`, `user_data.rs`, `export.rs`, `assembly.rs` and `material.rs` are
single-purpose. `crates/spacemouse`, `ui-core`'s navigation/theme/units/drag
modules and `viewport/src/gpu.rs` are clean; `sketch_toolbar.rs` is a
descriptor table; the sketch crate's `chain`, `ids`, `queries`, `trim`, `text`
and `expression` modules are the right size. `crates/compute`, `catalog`'s
store, `persistent.rs`'s resolver, `kinematics.rs`, the rebuild transaction
and the parameter table are single-sourced. In the scan add-on, `fit.rs`,
`numeric.rs`, `segment::classify_region`, `merge.rs`, `validate.rs`,
`register.rs`, `spatial.rs`, `mesh.rs`, `transform.rs`, `blend.rs`'s
discriminator and `scan-lab` are in good shape.

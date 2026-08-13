# ADR 0021: Editable sketch authoring and late-bound region replay

Status: Accepted and implemented for the declared S2D domain
- Date: 2026-07-30
- Decision owners: Artificer project

## Context

Native document v4/v5 stored a sketch frame, support, and compiled
`PlanarProfile2`. That was enough to replay an extrusion, but not enough to
reopen the sketch as the same editable design. Open curves, construction
geometry, primitive intent, pattern/modifier parameters, identities, and the
specific arrangement cell selected by a downstream feature were lost.

Persisting only a sampled display representation would also make topology and
replay depend on zoom, rendering tolerance, or UI implementation details.

## Decision

Native document v6 makes `artificer-sketch::SketchDefinition` the authoritative
sketch payload. It contains stable points, authored operations, exact evaluated
line/arc/circle outputs, semantic output roles, retired identities, allocator
high-water marks, and a monotonic sketch revision. The frame and support remain
owned by the model document. A compiled `PlanarProfile2` is an optional checked
cache, never the authoring source.

Every model-changing sketch gesture stages one private candidate definition.
The green tick or bare Enter publishes the complete transaction; the red cross
or Escape discards it. Draft allocations never advance live high-water marks,
and published identifiers are never reused after undo or retirement.

The first-pass recipe set covers point, line/polyline/centreline, corner and
centre rectangles, centre and two-point diameter circles, centre/start/end and
three-point arc gestures, inner/outer-diameter polygons, both slot gestures,
Trim, rectangular/circular patterns, every bounded no-extension line/arc/circle
fillet carrier pair, and equal/two-distance line-line chamfer. Higher-level
gesture variants may evaluate to the same exact core recipe; display sampling
is never persisted as their geometry.

Crossings are resolved by the analytic arrangement. Bounded cells are selected
explicitly in sketch space; merely clicking a region is session state and does
not mutate the authoring graph. A downstream profile feature stores the source
`SketchId` and its chosen canonical `RegionSignature` values.
During rebuild it reevaluates the current authoring graph, resolves the saved
regions, compiles a fresh exact `PlanarProfile2`, and submits the applicable
kernel command. A cached profile is not reused after an upstream edit.

If a signature is missing or ambiguous, the downstream feature becomes dirty
with a repair diagnostic and retains the last valid committed body. It never
retargets by nearest coordinate.

## Migration

Versions 4 and 5 migrate each exact profile to one deterministic
`LegacyImportedProfile` operation. This preserves every line, circular arc,
circle, loop, hole, region, frame, and support without claiming that the user
originally authored a rectangle, slot, constraint, or other higher-level
primitive. Versions 1 through 3 retain their explicit legacy payload omission;
missing geometry is not fabricated.

## Verification

Tests cover empty/open/construction sketches, every requested creation recipe,
multi-entity transactions, Trim, pattern cardinality and ID survival, the full
fillet carrier matrix, chamfer variants, save/load, v5 migration, tampered
caches, stable region signatures, explicit selection, downstream dirtying,
unresolved-region rollback, and exact standalone/face extrusion replay.
Semantic and visual tests prove that reloaded authored curves remain visible,
selectable, and editable. Property-style tests cover similarity equivariance,
hostile values, and the 256-instance/1,024-curve arrangement boundaries.
Frame-budget tests measure arrangement rebuild only when the sketch revision
changes; ordinary pointer frames consume the cached result.

## Consequences

Sketch authoring is UI-neutral and reusable by the desktop application, future
automation, and file translators. The model depends on the sketch crate, while
the sketch crate cannot depend on the model, UI, renderer, or B-rep kernel.
Display tessellation remains presentation-only. Adding a primitive or modifier
extends typed recipes and evaluation without changing the document envelope or
the downstream region-selection contract.

This ADR does not introduce a geometric/dimensional constraint solver,
degrees-of-freedom analysis, splines/NURBS, automatic region-reference repair,
or general feature reorder. Missing or ambiguous late-bound regions remain
explicit repair outcomes.

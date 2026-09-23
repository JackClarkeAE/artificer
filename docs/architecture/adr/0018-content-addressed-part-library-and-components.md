# ADR 0018: Content-addressed local Part Library and component occurrences

Status: Accepted for F2
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

Artificer needs a standard-parts workflow in which a fixed or parametric definition can be selected repeatedly without copying an untraceable body. A useful first slice must distinguish the immutable authored definition, the resolved parameter variant, and each occurrence placed in a workspace. It must also reject missing required inputs and corrupted local content before kernel execution.

F2 establishes those boundaries locally. It is a foundation for a later Vault-like product, not a claim of cloud collaboration or a complete assembly environment.

## Decision

### Immutable catalog packages

The pure-Rust `artificer-catalog` crate has no dependency on the model, kernel, renderer, or UI. A version-1 `artificer.catalog.part` package contains:

- a path-safe stable definition ID and semantic `MAJOR.MINOR.PATCH` revision;
- fixed or parametric kind, searchable metadata, tags, material, and part number;
- an ordered typed public parameter contract, including units, optional defaults, bounds, steps, and choices; and
- a canonical JSON embedded native part document with an explicit media type and schema version.

Sealing a definition computes a SHA-256 digest over its canonical authored content. Deserialization and every load recompute and verify that digest. A package therefore has an immutable content address independent of its filesystem location.

### Local store

`CatalogStore` keeps immutable package objects under `objects/` and small `(definition ID, revision) -> digest` records under `refs/`. Publication creates paths without overwriting them:

- republishing byte-identical content is idempotent;
- publishing different content under an existing ID and revision is a conflict; and
- authors must issue a new revision for changed content.

The in-memory search index is disposable and rebuilt from verified refs and objects. Corrupt, unsafe, oversized, or symlinked entries are rejected and reported rather than becoming searchable. Object writes and ref writes use same-directory temporary files and atomic create/compare behavior, so an interrupted publication cannot replace an existing revision.

At application startup the first built-in definition, `20 × 20 Aluminium Extrusion` revision `1.0.0`, is sealed and idempotently published into the local store. `ARTIFICER_CATALOG_DIR` overrides the store root; otherwise the application uses the operating system's local application-data location. If the store cannot open, the same verified built-in package remains available as an explicit in-memory fallback.

### Parameterized insertion

The first Part Library window exposes the aluminium extrusion's required `Length` in millimetres. Its header control sits beside Save/Open so it remains reachable at the supported 1040 × 700 minimum window. No default is authored, so `Add to current workspace` remains disabled until a finite value inside the declared range is entered. The staged insertion pins the exact definition ID, revision, package digest, and resolved parameter assignment.

`Add to current workspace` only creates presentation intent. The universal confirmation gate then resolves the exact package revision, evaluates and canonicalizes its typed parameters, derives a deterministic parameter-binding digest, resolves the embedded model-owned `ParameterizedKernel` recipe, and executes the resulting native kernel command. Cancellation and rejection leave document, body, component, and kernel history unchanged.

A successful insertion atomically appends one replayable feature, one separate body branch, and one stable `ComponentInstanceRecord`. The occurrence pins:

- definition ID, semantic revision, and content digest;
- canonical resolved parameter values and their binding digest;
- a rigid pose with no scale component;
- visibility, suppression, and grounded flags; and
- its creating feature and produced body IDs.

Equal parameter assignments create distinct occurrence IDs but the same binding digest. Different assignments create distinct binding digests and regenerated body geometry. The Browser and History project these records; the current UI can select and hide/show each inserted body. Browser component labels truncate within the available panel width and expose the complete label on hover instead of clipping the row or forcing horizontal layout growth.

## Verification

Catalog unit and acceptance tests cover canonical package bytes, deterministic digests, typed parameter validation, identifier/path safety, size limits, idempotent publication, exact revision resolution, deterministic search, store reopen, path-independent object bytes, corruption diagnostics, and index rebuild that excludes a corrupt object.

Part Library semantic/UI tests cover required-input gating, exact digest pinning through a persistent store, staging without mutation, red-cross cancellation with values retained, tick/`Enter` confirmation, exact 20 × 20 × Length volume, repeated equal and unequal variants, stable distinct component IDs, equal/different binding digests, Browser hide/show, fresh-process save/load, and history roll-forward. Checked-in pixel tests cover both the staged parameter window and a committed component in the workbench.

Model tests independently cover stable parameter and component IDs, bounded expression/type/range/cycle validation, dirty propagation, deterministic binding digests, atomic component/body creation, rigid pose and grounded behavior, suppression, undo/redo, archive validation, and rejection of a tampered component binding digest.

## Consequences and remaining boundaries

F2 gives Artificer a trustworthy local definition/variant/occurrence split and a first physically usable parametric standard part. It does not yet provide:

- a dynamic multi-definition browser in the Part Library window; the current UI exposes one built-in card even though the store and search APIs support more entries;
- authoring, importing, publishing, revising, deleting, garbage-collecting, or sharing definitions through the UI;
- server synchronization, permissions, checkout/locking, team metadata, or any other networked Vault behavior;
- general mates/assembly solving, configurations, component-to-component joint inference, or propagated hierarchy motion; [ADR 0019](0019-rigid-occurrence-placement-and-joint-forest.md) now adds confirmed rigid occurrence placement, deterministic non-overlapping insertion, and a persistent fixed/revolute joint forest;
- automatic variant-geometry sharing, replacement/update workflows, or “use latest revision” semantics; every occurrence deliberately pins an exact revision and digest; or
- collision/interference heatmaps, tolerance profiles, animation studies, or interactive sectional analysis.

Those assembly and analysis programmes can build on stable occurrences without being folded into kernel truth. General parameter editing and regeneration still belong to the unfinished M5 feature/document programme.

## Amendment: a part keeps its picture and rough size (2026-09-23)

The library list shows each part as a small picture with its version and
rough size, so a part can be recognised without reading its description.

- **When it is drawn.** A part's picture is drawn once, when the part is
  saved into the library, and kept beside its package. It is drawn again
  only when the store has none for that exact package. That happens when a
  store written by an older build is opened, or when the kept files cannot
  be read.
- **How it is drawn.** The workbench builds the part as an insertion would
  and draws it on the CPU, with no window or GPU involved. The view is the
  viewport's isometric one, fitted with a margin, at 96 px square on a
  transparent background. A parametric part is drawn at a sample value; the
  built-in extrusion is drawn at Length 100 mm.
- **What it measures.** The part's bounding box along X, Y and Z is kept
  with the picture. An extent that changes when a parameter changes is
  recorded as that parameter's rather than as the sample's number. The
  extrusion's size therefore reads `20 × 20 mm × Length`.
- **Where it lives.** `CatalogStore` keeps `previews/<digest>.png` and
  `previews/<digest>.json` (the measurements) beside `objects/` and `refs/`.
  A preview is derived rather than authored. It is not part of the
  package's content address, so it may be replaced, and each file is
  replaced atomically. The store validates both files and refuses a preview
  for a package it does not hold. A missing, damaged or oversized preview
  reads as none, so the answer is to draw it again rather than to fail the
  library. The index never mistakes a preview for a package.

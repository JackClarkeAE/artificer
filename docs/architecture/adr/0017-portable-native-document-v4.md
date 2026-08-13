# ADR 0017: Portable native document v4 and atomic fresh-process replay

Status: Accepted for F1
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

[ADR 0014](0014-m5a-parametric-document-foundation.md) established stable feature, body, and sketch identity, replay recipes, independent body branches, rollback, and persistent references. Its original saved form did not contain exact sketch geometry, and the kernel lab depended on session-owned snapshot, display, and operation-report archives. A document could round-trip as structured history without yet proving that a new process could reconstruct the supported part.

F1 closes that bounded portability gap. It does not claim a complete constrained-sketch or arbitrary-feature editing system.

## Decision

### Native schema v4

F1 established `artificer.native.document` version 4 as the portable-sketch writer. The current writer is now version 6 after the v5 joint graph and v6 editable sketch-authoring graph; versions 1 through 5 remain readable and are migrated in memory, while unknown newer versions fail closed. The v4 exact-profile payload requirements below remain mandatory derived-cache requirements in v6 documents.

Every newly authored sketch feature in v4 must carry one exact, revision-specific `SketchPayload`:

- the finite, non-degenerate `PlanarFrame3` used for authoring;
- the bounded exact `PlanarProfile2`, retaining line, circular-arc, and circle uses rather than display tessellation; and
- a stable support recipe: either an origin plane or a document `BodyId` plus a role-based `PersistentRef` to a planar face.

The payload must produce exactly one non-zero sketch geometry revision. Face support must agree with the feature's body branch, and every persistent-reference producer must precede the sketch. New v4 authoring cannot omit this payload. A v1-v3 sketch that never contained geometry is retained as an explicit legacy omission during migration; the loader never invents geometry for it.

The native document also persists the typed parameter table and component occurrence records introduced by the F1/F2 foundation. Parameters use stable IDs, dimensional types, canonical units, bounded declarative expressions, validation metadata, and deterministic binding digests. Runtime undo/redo journals remain deliberately outside the archive.

### Typed kernel-recipe binding

`ReplayAction::ParameterizedKernel` retains a serializable kernel-command template plus at most 16 explicit, command-specific scalar bindings. A binding names a stable `ParameterId` and a typed kernel field rather than an unvalidated JSON path or UI callback. Document validation requires the feature's declared parameter inputs to match the recipe exactly, requires every target to be unique and compatible with its command, and requires each current scalar target to consume a Length quantity.

The current bounded targets are cuboid X/Y/Z size, standalone planar-profile extrusion distance, and the distance fields of both supported face-profile extrusion command variants. Entity-targeting templates must also carry a document-owned persistent target. Immediately before execution, both ordinary rebuild and fresh-process hydration evaluate the document parameters and resolve the template into an ordinary independent or persistently targeted kernel action; missing, non-length, non-finite, non-positive, incompatible, or duplicate bindings reject before kernel execution. The first Part Library definition uses this model-owned mechanism to bind `Length` to extrusion distance, so catalog/UI code does not perform untyped command mutation.

### Atomic hydration

The kernel lab loads a document by replaying it into a private `HydratedDocument` before changing the current workspace. The stage owns regenerated immutable snapshots, operation reports, body-branch heads, suppression results, and history-cursor evidence. It handles independent roots and chained features, rebinds targeted commands only from operation reports regenerated during that load, and verifies every clean feature against its persisted input snapshot, output snapshot, and semantic digest.

Any parse, kernel, persistent-reference, or provenance error drops the complete stage and leaves the current document and viewport unchanged. Dirty features may regenerate, but their stale association is not accepted as provenance. To preserve forward history, the application privately hydrates retained features through the end and then restores the saved history cursor before publication.

The resulting runtime projects exact supported sketches back into the Browser and viewport. Origin and planar-face sketches retain their authored frame; face support is resolved against regenerated operation history. An unconsumed closed sketch can remain visible after restart and can be extruded separately.

### Save and Open interaction

`Save` writes the complete current native document through a same-directory temporary file, flushes it, and renames it into place. `Open` reads only a bounded regular, non-symlink file and is staged behind the universal green-tick/`Enter` and red-cross/`Escape` confirmation gate. Merely clicking `Open` cannot replace the workspace.

The application has one current document path. `ARTIFICER_DOCUMENT_PATH` may override it; otherwise it is `current.artificer.json` beside the application catalog directory.

## Verification

The model suite requires v4 sketch payloads for new authoring, validates exact origin and planar-face support, round-trips payloads by stable sketch revision, migrates explicit v1-v3 omissions without fabricating geometry, and rejects an unmarked v4 omission. Parameterized-recipe tests cover every current scalar target, exact parameter-input/type agreement, independent and persistent templates, bounded unique targets, deterministic resolution, archive round trips, and atomic rejection of incompatible edits.

Fresh-process replay tests cover independent roots, chained and persistently targeted features, component suppression, history-cursor restoration and roll-forward, clean-provenance tampering, and late kernel failure without partial publication. Kernel-lab acceptance tests additionally prove that:

- two parameterized component variants survive save/load with their component and binding identities;
- a hidden body remains hidden after restart;
- origin and face-supported sketches remain visible and separately extrudable;
- changing saved recipe geometry without updating provenance rejects atomically; and
- Save/Open uses a real file while Open confirmation and cancellation remain document-neutral until confirmed.

## Consequences and remaining boundaries

F1 makes current, supported closed-profile documents restartable without serializing B-rep storage or treating renderer data as authority. Snapshots and operation reports are regenerated from native recipes and checked against saved provenance.

It does not yet provide:

- entity-level editing of a reloaded sketch in the live sketch canvas; the exact reloaded profile can be displayed and extruded, but editable construction entities and future constraint graphs are a later representation;
- recovery of geometry that was never present in a legacy v1-v3 sketch payload;
- persisted undo/redo stacks, pending operations, camera/layout state, animation state, or display caches;
- a general file picker, recent-file list, autosave, recovery journal, document locking, or cross-platform replacement guarantees beyond the current atomic-rename path; or
- successful replay outside the kernel's declared command and persistent-reference domains.

Constrained sketch solving, arbitrary parameter editing, feature reorder/deletion, reference repair, and configuration management remain required before M5 is complete.

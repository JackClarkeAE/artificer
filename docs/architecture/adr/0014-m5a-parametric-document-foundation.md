# ADR 0014: M5a parametric document foundation

- Status: Accepted for the M5a foundation
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

The M4 workbench accumulated successful sketches and modeling operations in a presentation-owned history preview. That ledger was useful for orientation, but it had no stable document identity, dependency edges, replay payloads, dirty propagation, rollback, suppression, undo/redo, saved form, or product persistent-reference semantics. It also represented one displayed snapshot even as the workbench began retaining multiple independent bodies.

The native kernel already provides the other half of the boundary: immutable snapshots, transactional commands, complete `Generated | Modified | Deleted | Unchanged` operation history, and operation roles. Snapshot-scoped `EntityRef` values are intentionally unsuitable as saved feature intent under [ADR 0003](0003-entity-identity-and-persistent-references.md). M5 therefore needs a document layer above the protocol and kernel rather than more presentation state inside the UI or mutable feature state inside the B-rep.

This decision establishes the first bounded part of that layer. It is called M5a because it provides authoritative identity, persistence, replay planning, and reference-resolution foundations without claiming the complete M5 constraint and feature-editing product.

## Decision

### Crate ownership and dependency direction

The new pure-Rust `artificer-model` crate owns the parametric document. It depends on `artificer-protocol` data contracts but not on the native kernel implementation, egui, rendering, or the workbench application.

The boundaries are:

- the kernel owns immutable B-rep snapshots, command execution, validation, semantic provenance, and operation reports;
- `artificer-model` owns document identity, feature intent, dependencies, object state, replay planning, and persistent-reference recipes;
- the application owns orchestration: it resolves a replay recipe, constructs an `ExecuteRequest`, executes the kernel, retains runtime display/snapshot caches, and publishes the result to the document only after kernel success; and
- the Browser and History UI are projections of document state. UI widgets cannot become a second authoritative feature ledger.

Neither `artificer-model` nor the application may construct or mutate B-rep topology directly.

### Stable document identity

The document allocates separate monotonic `FeatureId`, `BodyId`, and `SketchId` values. They are document identities rather than kernel entity IDs. IDs are never reused during a document session, including when the edit which allocated them is undone.

Each ordered `FeatureNode` records:

- its stable feature ID, kind, and label;
- stable object inputs and explicit/derived feature dependencies;
- body or sketch outputs, including the sketch geometry revision represented by a sketch output;
- a serializable replay action;
- suppression, read-only, and clean/dirty state; and
- the last successful input/output snapshot and semantic digest association, if one exists.

Body and sketch records carry their own stable IDs, labels, creator/latest feature, visibility and read-only state, and committed snapshot association. A sketch record also carries its active geometry revision, an optional `support_body`, and optional `auto_hidden_by` provenance. `support_body` is `None` for an origin-plane sketch and identifies the exact document body for a body-supported sketch; it is not inferred from a content-identical kernel snapshot.

### Independent body branches

A native document may contain multiple independent bodies. Each body has its own committed snapshot chain; there is no fictitious combined global kernel snapshot. A New Body constructor begins from `SnapshotId::ZERO`, while a body-changing feature must consume and output exactly one existing body and chain from that body's committed snapshot. A body-supported sketch contributes its `support_body` to branch validation, so a feature cannot consume a sketch from Body 1 while modifying Body 2. Cross-body modeling features remain unsupported in M5a and fail explicitly during append and native-document loading.

Dirty propagation and rebuild planning follow feature dependencies rather than visual timeline position. Editing Body 1 therefore neither dirties nor replays an independent Body 2 branch. A rebuild plan carries the body-local snapshot immediately before replay and, for a root constructor, its required empty-snapshot input.

The document retains a most-recently-published snapshot cursor for activity/status reporting only. It is not model truth for every body and must not be used instead of `BodyRecord::committed_snapshot` when executing a body-local command.

### Atomic rebuild and rollback

A rebuild is an in-memory transaction tied to both the source document revision and the complete source document state. Its executable steps are ordered deterministically. Each recorded result must match the next feature and the expected body-local input snapshot; document-only marker steps must leave the snapshot unchanged.

`commit_rebuild` validates that every executable step succeeded, applies all feature and object associations to a cloned state, and publishes that state atomically. Suppressed features and dependants are skipped deterministically. Sketch snapshot and geometry-revision restoration is limited to sketches touched by the rebuilt branch, so rebuilding one body cannot rewrite an independent dirty sketch. A failed, incomplete, stale, cancelled, dropped, or explicitly rolled-back transaction cannot publish partial document state.

### Suppression, visibility, and undo/redo

Feature suppression marks only the affected dependency branch dirty. Successful rebuild removes the suppressed result and restores the latest active upstream body/sketch association. An already-committed feature cannot be appended through a dirty, suppressed, or otherwise uncommitted dependency.

Body and sketch visibility are document state and are independently undoable; visibility never executes the kernel. Consuming a sketch may apply a default auto-hide attributed to the exact Extrude, Add, or Cut feature in `auto_hidden_by`. That derived hide is coalesced into the consumer's existing undo checkpoint rather than creating a second user action: undoing the consumer restores the prior visibility, and redoing it restores the auto-hide. An explicit sketch visibility edit clears that auto-hide provenance and remains ordinary undoable document state.

The runtime document keeps bounded undo and redo journals, with a default limit of 128 and a hard limit of 1,024 states. A new user edit clears redo. Undo restores document content but never rolls stable-ID allocators backwards. Successful rebuild bookkeeping is derived state and does not add a second user-facing undo entry for the edit which caused it.

### Timeline rollback cursor

ADR 0015 extends M5a with a persisted `HistoryCursor`: `Start`, `After(FeatureId)`, or `End`. The cursor defines the evaluated prefix of the global ordered feature timeline and remains independent of explicit feature suppression. A feature after the cursor retains its recipe, suppression flag, and cached successful association, but it does not contribute an active body/sketch result.

Moving the cursor reconciles every body and sketch to its last output inside the active prefix. Independent branches therefore roll back to their own most recent active association rather than sharing a fictitious global snapshot. Rolling forward restores retained associations exactly. A cursor change is one bounded undoable document edit; the UI commits a slider drag only when the gesture ends. New feature append is rejected unless the cursor is at `End`, preserving the future timeline instead of deleting it implicitly.

### Versioned native document

`ModelDocument` serializes through a native envelope identified by `artificer.native.document`. This ADR introduced schema version 1; the current writer is now version 6. Versions 1 through 5 remain readable through explicit in-memory migration, while an unknown newer version fails closed. The envelope retains the document revision, allocator high-water marks, configured undo limit, ordered features, objects, replay intent, committed associations, typed parameters, component occurrences, exact sketch payloads beginning with v4, the assembly joint forest beginning with v5, and editable sketch authoring graphs beginning with v6. Runtime undo/redo stacks are deliberately not serialized.

Loading fails closed on an unknown format/version, exhausted or invalid allocators/revision, resource-limit violation, duplicate or forward identity, broken input/dependency/output relation, inconsistent body or sketch-support branch, raw entity-targeting command, invalid marker transition, object snapshot/revision mismatch, invalid parameter/component state, an invalid joint graph, an unmarked missing v4 sketch payload, or a v6 sketch whose evaluated cache disagrees with its authored graph. Versions 1 through 3 may retain an explicit legacy sketch-payload omission because those schemas never contained the missing geometry; migration never fabricates it. A v4/v5 exact profile becomes one `LegacyImportedProfile` operation without inventing rectangle, slot, or constraint intent. [ADR 0017](0017-portable-native-document-v4.md) records the v4 portability and fresh-process replay boundary, [ADR 0018](0018-content-addressed-part-library-and-components.md) records the catalog/component extension, and [ADR 0019](0019-rigid-occurrence-placement-and-joint-forest.md) records native v5 assembly state.

### Persistent entity references

An entity-targeting feature stores a versioned `PersistentRef` containing:

- the producing `FeatureId`;
- the exact kernel `OperationRole`, including its optional ordinal;
- the required `EntityKind`; and
- an optional upstream persistent lineage qualifier.

Immediately before execution, the application supplies ordered feature `OperationReport` values and the current body snapshot. The resolver seeds candidates from the producer's role and composes explicit kernel history through reachable snapshots. The workbench scopes recipe creation to features that output the active `BodyId`, in addition to checking the exact committed input/output/digest association, so content-identical body snapshots do not erase document-body ownership. Resolution returns exactly one of `Resolved`, `Missing`, or `Ambiguous`; split, deletion, absent report, unsupported recipe version, and lineage mismatch are never guessed through.

`ExtrudeFaceProfile` and `PushPullFace` are stored as `TargetedKernel` templates plus persistent recipes. The raw `target_face` inside either protocol command template is non-authoritative and is always overwritten after successful resolution. A plain `ReplayAction::Kernel` containing either entity-targeting command is rejected during append and native-document loading, preventing a saved snapshot-scoped `EntityRef` from masquerading as document identity.

### Kernel-lab integration

The kernel lab initializes an M5a document for the displayed base body, records successful Sketch, Extrude, Add, Cut, Transform, and confirmed library-component features, and projects document features and objects into History and Browser views. The History strip exposes the persisted cursor through step controls and a slider; modeling tools remain unavailable while the cursor is not at `End`. Face-sketch staging retains both the supporting `BodyId` and immutable snapshot, and rejects either changing before commit. Body and sketch eye controls modify document visibility. Native-v6 sketches retain the editable `SketchDefinition`, exact frame/support, selected-region evidence, and checked compiled-profile cache; open and construction geometry therefore survive Save/Open rather than being reduced to a closed profile. Multiple New Body or component insertions receive independent body IDs and visibility.

Kernel execution remains behind the existing confirmation coordinator. Staging, cancellation, and kernel rejection are document-neutral. Runtime B-rep/display archives and operation reports remain regenerated caches rather than saved authority. On Open, the application replays into a private stage, rebuilds those caches and persistent-reference evidence, verifies clean semantic provenance, restores the saved history cursor, and publishes only after the complete load succeeds.

## Verification

The M5a model suite covers:

- monotonic IDs, allocation across undo, bounded undo/redo, and versioned serde round trips;
- persisted cursor positions, exact multibranch rollback/roll-forward, suppression independence, append blocking away from `End`, cursor undo/redo, and rejection of corrupt cursor/object associations;
- ordered dependencies, read-only enforcement, visibility, suppression, unavailable-dependency rejection, and dirty propagation;
- two independent body roots, body-local snapshot chains, branch-local replay, body-owned sketch support, append/load rejection of cross-body sketch consumption, and cross-branch isolation;
- ordered rebuild results, stale transaction rejection, atomic commit, and failure rollback;
- root and marker snapshot invariants plus corrupt native-document rejection;
- sketch snapshot and geometry-revision restoration, including isolation from an unrelated dirty sketch, plus consumer-attributed auto-hide across undo/redo;
- persistent-role propagation through changed snapshots, lineage qualification, split ambiguity, deletion/missing results, and stale raw target replacement; and
- rejection of entity-targeting replay without a persistent recipe.

Kernel-lab semantic/UI tests separately cover the exposed behavior: `face_feature_ui` exercises confirmed Add/Cut and repeated supported face-feature chains; `model_document_ui` exercises body/sketch visibility, independent New Body results, direct or finished face-sketch extrusion, and fresh-process replay of origin and face-supported sketches; the editable-sketch suites exercise native-v6 Save/Open, reopened entity selection and modification, explicit arrangement-region selection, and late-bound downstream replay; `parametric_history_ui` exercises exact body/digest/visibility undo-redo, suppression/restoration, cursor rollback/roll-forward, and native-document invariants; and `part_library_ui` exercises typed variants, component occurrences, atomic Save/Open, provenance-tamper rejection, and cursor-preserving fresh-process replay. The dedicated hydration suite proves that replay failure never publishes a partial runtime. These suites still do not claim arbitrary feature reorder, automatic persistent-reference repair, or the full constrained-sketch regeneration UI.

## Consequences

### Benefits

- Feature, body, and sketch identity no longer depends on labels, timeline indices, or snapshot-local entity IDs.
- Multiple bodies can coexist without corrupting one another's snapshot chain.
- Regeneration can fail or be cancelled without replacing the last committed document.
- Persistent references build directly on native operation provenance and expose genuine ambiguity or deletion.
- Current supported sketches and body branches can be reconstructed in a fresh process without serializing B-rep storage as document truth.
- Typed parameters and digest-pinned component occurrences extend the same identity/replay boundary without making the catalog part of the kernel.
- The UI, kernel, and saved document now have a one-way ownership boundary that can expand without embedding a foreign kernel or a UI framework in model truth.

### Costs and limitations

M5a plus F1/F2/F3 is not full M5. In particular:

- there is no sketch constraint solver, degrees-of-freedom model, or persisted dimensional constraint graph; a typed parameter/equation foundation now exists, but the general user-facing parameter editor and constrained-sketch regeneration loop do not;
- native v6 persists and reopens entity-level editable sketch graphs; v4/v5 exact profiles migrate as `LegacyImportedProfile` operations without invented primitive intent, while a legacy v1-v3 omission cannot be recovered;
- arbitrary feature parameter editing, timeline reorder, feature deletion, cross-body features, branch merge, configurations, and a complete user-facing repair workflow are not implemented;
- moving the rollback cursor preserves the future timeline and therefore does not implement branch creation, tail truncation, or editing from an earlier marker;
- persistent matching currently uses producer role/ordinal and optional lineage, without the broader adjacency, orientation, and geometric qualifier set anticipated by ADR 0003;
- operation reports and B-rep/display archives are regenerated runtime caches rather than serialized stores; a replay or persistent-reference failure rejects the whole load;
- undo/redo journals are runtime-only and are not restored after save/load; and
- the current Part Library exposes one built-in definition and has no authoring/publish UI or networked Vault behavior; F3 adds rigid placement and a validated fixed/revolute joint forest, but no mate solver, component-to-component inference, configurations, or propagated assembly motion.

These are explicit later increments. M5 is complete only when constrained sketch parameters can be edited and deterministically regenerate the supported feature matrix through save/load while preserving or explicitly rejecting downstream references.

# ADR 0003: Entity identity across snapshots and regeneration

- Status: Proposed
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

Artificer needs several kinds of identity with different lifetimes:

- Efficient internal references between topology records.
- Serializable command targets inside one immutable model snapshot.
- Stable IDs in replay and failure diagnostics.
- User selections and feature references that survive regeneration into a different snapshot.

A generational arena handle is useful for internal memory safety but is process-local and tied to one storage layout. It cannot safely be serialized, sent remotely, or treated as a persistent face/edge name. Conversely, a document-level reference such as “the cylindrical wall produced by this hole feature” cannot be reduced to an arena index or geometry hash.

## Decision

Artificer separates four identities and one explicit remapping relation.

### 1. `StorageHandle<T>`

- Typed, generational, process-local handle into an arena/store.
- Valid only inside the owning in-memory snapshot implementation.
- Never serialized, logged as public identity, sent over the kernel protocol, or stored by the document/UI.

### 2. `SnapshotId`

- Protocol-visible identity of one immutable kernel snapshot.
- Every entity-targeting command states the expected `SnapshotId`.
- Executing against another snapshot returns `StaleSnapshot`; the kernel never guesses or silently retargets.
- The concrete deterministic construction is fixed with the replay schema in M0. It is identity, not proof of geometric equality.

### 3. `EntityId`

- Opaque, serialized identity unique within one `SnapshotId`.
- Stable for the lifetime and serialization of that snapshot, independent of arena slot/layout.
- The full protocol identity is `(SnapshotId, EntityId, EntityKind)`.
- IDs are not reused within a snapshot and have no implied order or geometric meaning.
- A matching numeric `EntityId` in two snapshots does not imply continuity unless history says so.

### 4. `DebugId`

- Stable label inside a case, replay trace, or failure bundle.
- Resolves to a snapshot/entity or an intermediate algorithm object such as an intersection branch.
- Designed for human diagnostics and visual highlighting, not for document semantics.

### 5. `PersistentRef`

- Document-layer, versioned recipe expressing design intent and context.
- May refer to a source feature/operation role, upstream persistent reference, generation relation, entity kind, orientation, and adjacency/geometric qualifiers.
- Resolved immediately before command construction to exactly one `(SnapshotId, EntityId)` or a structured `Missing | Ambiguous | Unsupported` result.
- Geometry signatures and neighborhood comparisons may disambiguate candidates but are never identity by themselves.

### History relation

Each successful operation returns an explicit relation from old snapshot entities to new snapshot entities:

```text
(old SnapshotId, old EntityId)
  -> Generated | Modified | Deleted | Unchanged
  -> zero, one, or many (new SnapshotId, new EntityId)
```

History also records operation/source roles for newly generated entities. Splits and merges are naturally many-to-many; no API assumes one old face maps to one new face.

The document reference resolver composes these relations through feature regeneration, then applies the persistent reference's context to select or reject candidates. Ambiguity is a first-class result.

## Protocol rules

- A command journal serializes `SnapshotId`/`EntityId`, never `StorageHandle`.
- The UI may cache snapshot-scoped selection IDs only while that snapshot remains displayed.
- On regeneration, selections are rebound through `PersistentRef` and history, not by retaining old IDs.
- Semantic golden digests ignore entity-ID allocation and output ordering.
- Deterministic mode assigns snapshot/entity/debug IDs reproducibly for equivalent replay of the same command journal, but semantic correctness never depends on that reproducibility.
- Importers create source/document references explicitly; imported file record numbers are metadata, not permanent kernel identity.
- Adapters translate their own handles into fresh Artificer snapshot/entity IDs at the boundary.

## Consequences

### Benefits

- Internal storage can change without invalidating documents or the protocol.
- Stale UI/remote commands fail safely instead of editing the wrong face.
- Replays and failure bundles remain inspectable across process boundaries.
- Persistent naming is built on operation intent/history and can represent split, merge, deletion, and ambiguity.
- The native kernel and external oracle can use unrelated internal identity models because only case-level semantics cross the test boundary.

### Costs and risks

- Every operation must emit complete history, including unchanged and deleted entities where relevant.
- Persistent resolution is a substantive document algorithm, not a UUID lookup.
- Long chains of history need compaction/checkpointing without losing explanation.
- Deterministic ID allocation requires controlled traversal in replay mode.
- Some edits are genuinely ambiguous; the product must ask for repair rather than silently choosing.

## Verification

- Stale-snapshot commands always fail with `StaleSnapshot` and leave state unchanged.
- Serializing/reloading a snapshot preserves its protocol IDs and semantic digest.
- Repacking/reordering internal storage changes `StorageHandle` values but not snapshot/entity protocol identity.
- Split, merge, deletion, suppression, rollback, and reorder cases exercise many-to-many history.
- Parameter-edit matrices assert persistent references resolve to the intended role or an explicit ambiguity/deletion.
- Native protocol identity tests run without any oracle. Optional oracle comparisons expose no oracle handles and do not claim protocol identity equivalence.

## Sources and precedent

- [Open CASCADE OCAF topological naming and operation history](https://dev.opencascade.org/doc/overview/html/occt_user_guides__ocaf.html)

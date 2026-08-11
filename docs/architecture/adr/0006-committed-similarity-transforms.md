# ADR 0006: Committed whole-snapshot similarity transforms

- Status: Accepted
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

ADR 0005 established Move, Rotate, and Scale as deterministic presentation state. That made direct manipulation safe and testable, but it could not change model truth, participate in journals, update bounds or mass properties, or supply operation history. Promoting display geometry directly would bypass the native kernel's immutable transaction and validation boundary.

The current experimental model contains exactly one solid and the declarative case format has no symbolic binding for “the solid produced by the previous step.” A command containing an allocator-derived entity ID would make fixtures brittle and contradict ADR 0003's snapshot-scoped identity rules.

## Decision

Protocol version 1 adds the experimental capability `native.transform_snapshot.v0`:

```text
TransformSnapshot {
  transform: SimilarityTransform3 {
    translation: Vector3,
    rotation: RotationQuaternion (Hamilton w, x, y, z),
    uniform_scale: f64
  }
}
```

Its canonical geometric meaning is:

```text
p' = uniform_scale * R(normalize(rotation)) * p + translation
```

The transform is active, right-handed, orientation-preserving, and evaluated about the world origin. Scale must be finite and strictly positive. Reflection, non-uniform scale, shear, per-body targeting, and local topology editing are outside v0. UI pivot state is explicit presentation intent and is converted before execution using:

```text
t_kernel = pivot + preview_translation - scale * R * pivot
R = Rz * Ry * Rx
```

Camera state and turntable phase never enter a kernel command.

### Authoritative B-rep transformation

The kernel clones the immutable input topology and preserves every record ID, storage key, incidence relation, loop order, coedge orientation, shell/solid ownership relation, and construction/source role. It then transforms every authoritative geometric representation:

- Vertex points and both stored 3D edge endpoints receive the full similarity.
- Face plane origins receive the full similarity.
- Face plane `u` and `v` axes receive rotation only and remain unit length; the normal is reconstructed from their cross product.
- Coedge p-curve coordinates receive the positive uniform scale so evaluation on the rotated unit plane still agrees with the 3D edge.

The candidate passes the existing full solid validator before publication. Bounds, surface area, signed volume, and centroid are recomputed from candidate topology; old AABB corners or measures are never treated as authority. Volume integration uses a body-local reference instead of the world origin so supported large translations do not introduce avoidable cancellation.

### Supported numerical domain

The boundary rejects:

- Empty or non-single-solid source snapshots as `Unsupported`.
- Non-finite values, a zero quaternion, and non-positive scale as `InvalidInput`.
- A scale that puts an edge at or below the active minimum feature size as `InvalidInput`.
- Derived world or p-curve coordinates outside the active coordinate envelope as `ResourceLimitExceeded`.
- Placements whose represented edge lengths cannot satisfy `linear_agreement` as `NumericallyIndeterminate`.
- Any candidate that fails complete topology/geometry validation as `ValidationFailed`.

Stale snapshot, precision-lineage, and cancellation checks retain their existing fail-before-publication semantics.

### Identity, history, and selection

The semantic digest is calculated from transformed authoritative content. A non-no-op normally receives a new content-derived `SnapshotId`; an exact identity transform retains the same digest and ID.

Every transformed entity receives a one-to-one history record from its old full `EntityRef` to its new full `EntityRef`. The relation is `Modified`, or `Unchanged` for a content no-op. Matching numeric entity IDs across snapshots are not continuity evidence without this history. Fixtures require unique input/output references, the correct input/output snapshots, kind- and ID-preserving pairs, and per-kind coverage equal to the reported topology; a repeated mapping cannot satisfy “complete” history. The lab rebinds a selected face through the returned relation rather than retaining its stale reference.

Case schema 1 binds each fixture to protocol version 1 and requires every command capability to be declared. The runner rejects missing, unsupported, or mismatched versions and capabilities rather than substituting its current protocol.

Journal JSON enables exact finite floating-point round-trip parsing. Every floating-point field in serialized protocol and case artifacts rejects NaN and infinity instead of allowing `serde_json` to rewrite them as `null`. Typed in-process requests can still carry non-finite adversarial values and receive a structured kernel `InvalidInput`, but those invalid requests are intentionally not journal-serializable. If finite inputs derive a non-finite diagnostic measurement, the numeric measurement is omitted and classified with finite strings so the structured rejection itself remains journalable. Replay compares the complete operation report, so serializing a transformed bound may not move it by one ULP even when the semantic digest is unchanged.

### Preview, confirmation, camera, and motion behaviour

Move, Rotate, and Scale continue to edit presentation preview state first. While dirty, the UI says `PREVIEW — NOT COMMITTED`, keeps the base snapshot ID, and blocks unrelated model-case commands. [ADR 0007](0007-universal-model-operation-confirmation.md) supplies the shared operation gate: the visible green tick or bare `Enter` executes the public kernel transaction, while the red cancel action or `Escape` clears only the preview without a kernel attempt or overwriting the last kernel transaction. Success replaces the displayed snapshot only with the validated outcome and clears the pending operation; rejection keeps the committed snapshot, base snapshot, and editable preview intact.

The camera owns a persistent world target and fit radius. It is not implicitly refit from new report bounds during confirmation. Turntable motion is composed after the preview and retained after commit. Consequently, confirming at a fixed animation phase leaves projected body pixels stationary even though the snapshot and digest change.

## Verification

- Protocol JSON shape and version are fixed by round-trip tests.
- Native invariant tests cover translation, rotation, uniform scale, combined measures, p-curve/plane validation, complete 58-entity history, identity no-op, equivalent quaternion normalization, large translations, malformed transforms, resource limits, and deterministic replay.
- Declarative two- and three-step cases cover successful similarity, unique and topology-complete 58-record one-to-one `Modified` history, and rejection followed by a valid command against the retained snapshot through an actual JSON disk round trip.
- Semantic UI tests prove preview isolation, visible confirmation and cancellation, selection rebinding, dirty-case blocking, camera/motion preservation, rejection rollback, bare `Enter`/`Escape` handling during numeric editing, and the supported minimum window layout.
- The paused fixed-phase pixel test compares the viewport before and after confirmation with a small anti-aliasing allowance and fails on a visible jump.
- The headless UI budget test continues to exercise fixed 1/60-second frames; the native app retains continuous vsync-paced animation.
- Architecture audits continue to reject OCCT/OpenCascade, C/C++ product sources, and UI/render dependencies inside the kernel.

## Consequences

- Users can now manipulate, inspect, validate, commit, replay, and physically test a real native model transform through one UI loop.
- Topology remains unchanged in this capability; only authoritative geometry and snapshot-scoped references change.
- The whole-snapshot target is honest for the current one-solid model and keeps fixtures independent of allocator IDs.
- Future body-targeted Move/Copy requires document-level or case-level symbolic reference resolution rather than extending this command implicitly.
- Reflection, non-uniform transforms, instances, copies, and general affine deformation require separate operation contracts and orientation/uncertainty policies.

# ADR 0019: Rigid occurrence placement and persistent joint forest

- Status: Accepted for F3
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

[ADR 0018](0018-content-addressed-part-library-and-components.md) separated immutable part definitions, resolved parameter variants, and component occurrences. Each occurrence already retained a finite rigid pose, but insertion used identity, the renderer ignored the pose, and the document had no assembly hierarchy. Equal standard parts therefore occupied the same visible space and could not participate in named motion.

F3 establishes the smallest assembly boundary that is useful without pretending a general mate solver exists.

## Decision

### Occurrence placement is not B-rep geometry

A component occurrence translates and rotates its definition-owned geometry. It cannot scale it. A confirmed component Move/Rotate updates only `RigidComponentPose`; it does not execute a kernel command, append a geometry feature, change a body snapshot, or change its semantic digest. Undo/redo treats the pose update as one document edit.

The workbench uses the universal confirmation rail for placement, grounding, and joint creation. During a placement preview the renderer composes:

1. snapshot-local geometry;
2. the committed occurrence pose;
3. the unpublished Move/Rotate delta about the already-placed pivot; and
4. presentation-only animation and camera projection.

Committed pose is applied consistently to bounds, triangles, depth ordering, culling, edges, face hit-testing, labels, component-bound sketch overlays, extrusion previews, arrows, and transform gizmos. Local B-rep coordinates and `EntityRef` values remain unchanged.

### Deterministic initial placement

Part Library insertion computes every occupied occurrence-aware world AABB, finds their maximum X extent, and places the new local AABB after it along +X with 10 mm clearance. The new part is laid on Y=0 and Z=0 with identity rotation. Separation on X proves non-overlap for every supplied occupied AABB and is independent of Browser ordering.

### Native document v5 joint graph

Native document schema v5 adds monotonic `JointId` allocation and a bounded ordered joint forest. Versions 1 through 4 migrate in memory with an empty joint collection and the next joint allocator set to one; serialization then writes v5.

Each joint has:

- a user-visible bounded name;
- `World` or a stable component occurrence as parent;
- one stable component occurrence as child;
- an enabled flag; and
- either `Fixed` intent or `Revolute` intent with a finite origin, canonical unit axis, and optional ordered angular limits.

Archive and mutation validation require existing endpoints, one incoming joint per child, no self-parent edge, no cycle, valid IDs, bounded resources, and exact canonical numeric data. Disabled joints remain structural edges: disabling motion does not silently change hierarchy.

The first workbench joint command creates a named world-Z revolute joint at the selected component pivot. Browser and Properties expose that persisted intent, and the existing time-based 60 FPS presentation loop can play the active occurrence about that axis. This playback is deterministic presentation evidence; it is not yet a constraint solution.

Grounding remains explicit occurrence state. Ground/release is confirmed and undoable, and a grounded component rejects placement. A movable component with an incoming joint may still be directly posed in F3 because no solver yet derives pose from joint coordinates.

### Linked component geometry is read-only in the assembly workspace

Scale, face sketching, push/pull, Add, and Cut are disabled for a catalog-linked occurrence in the current workspace. Its authored size and topology belong to the pinned definition/variant. A later source-part editor or explicit break-link workflow may widen this boundary; F3 does not mutate a shared definition accidentally through one occurrence.

## Verification

Model tests cover fixed/revolute serialization, origins, axes, limits, missing endpoints, single-parent enforcement, cycle prevention, enabled state, mutation, undo/redo, non-reused IDs, v4-to-v5 migration, resource ceilings, and corrupted archives.

Placement tests cover deterministic clearance, rotated conservative bounds, typed millimetre/radian/degree fields, pivot-correct preview composition and inversion, scale rejection, grounded rejection, and quaternion/Euler edge cases.

Viewport tests use identical snapshot-local geometry at independent poses and prove occurrence-aware bounds, rendering, selection, overlays, feature previews, gizmos, and a posed 32-body preparation budget compatible with the 60 Hz goal. Workbench acceptance tests prove staging/cancellation neutrality, pose-only confirmation with unchanged snapshot/digest/kernel-attempt count, undo/redo, grounding, named joint creation, Browser/Properties visibility, and fresh-process pose/joint hydration. Checked-in pixel regressions cover separate occurrences, placement preview, and a committed revolute joint.

## Consequences and remaining boundaries

F3 supplies stable assembly identity and independently visible/movable occurrences. It does not yet provide:

- a geometric mate/constraint solver or derived component poses;
- joint-coordinate editing, limits UI, several selectable animation studies, motion propagation through a hierarchy, or assembly configurations;
- component-to-component joint picking, face/axis inference, flexible subassemblies, or large-assembly solve/performance work;
- source-part editing in context, replacement/update workflows, or link breaking;
- collision/interference heatmaps, tolerance profiles, swept motion studies, or sectional analysis; or
- assembly events as kernel feature nodes. Placement and joints are document assembly state, while geometry History remains a replayable B-rep feature timeline.

Those capabilities must build on this graph without moving assembly presentation state into kernel truth.

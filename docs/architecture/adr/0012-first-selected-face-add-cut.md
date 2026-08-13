# ADR 0012: First selected-face Add and Cut use an exact rectangular scaffold

Status: Accepted for the historical M4c experimental topology-editing slice; extended by ADRs 0013 and 0015
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

The first M4 constructor can extrude a finished convex polyline into a new solid, but it cannot edit an existing body. A useful CAD loop needs to select a model face, sketch in that face's authoritative local coordinates, and choose whether an extrusion creates, adds, or removes material. Treating display triangles as sketch support or replacing the body with another constructor result would bypass the kernel's topology and transaction contracts.

A general Boolean, arbitrary face splitting, profile regions with holes, and persistent naming are much larger programmes. The first topology-editing step therefore needs a narrow domain that is exact, visibly useful, and honest about its limits.

## Decision

Protocol version 3 adds `ExtrudeFaceProfile`. The command binds a planar frame and four profile vertices to a snapshot-owned face reference and carries an explicit `Add` or `Cut` material operation. `ExtrudePolygon` retains its separate empty-snapshot constructor meaning.

The first supported domain is:

- one valid axis-aligned rectangular-prism solid;
- one axis-aligned outward face of that solid;
- one axis-aligned rectangular profile strictly inset from the selected face boundary;
- a positive outward boss for Add; or
- a blind inward pocket for Cut that retains at least the minimum back-wall thickness.

The kernel exposes `planar_face_support` as a read-only query returning the exact face-owned local frame, outer boundary, face reference, and semantic digest. The workbench uses that contract for selected-face sketch placement. Debug tessellation and painted face roles remain presentation evidence and never become modeling input.

### Exact topology scaffold

Both material modes rebuild a minimal conforming boundary with 16 vertices, 28 edges, 56 coedges, 14 loops, 14 convex quadrilateral faces, one shell, and one solid. The 14 faces are the opposite cap, four retained outer walls, four coplanar shoulder patches around the inset profile, four boss or pocket walls, and one boss end or pocket floor. The construction has Euler characteristic 2 and no display-only grid seams.

For profile area `A`, profile perimeter `P`, distance `D`, and source measures `V0` and `S0`, the exact expected measures are:

- Add: `V = V0 + A D`;
- Cut: `V = V0 - A D`; and
- both: `S = S0 + P D`.

The ordinary kernel validator remains the publication authority. It checks opposite coedge use, loop/edge continuity, shell connectivity, Euler characteristic, positive signed volume, bounds, and exact measures before an immutable output snapshot can replace the input. A former shell-centre/face-centre normal heuristic is not used because it is invalid for legitimate concave pocket walls.

### Workbench contract

Selecting a face and choosing Create Sketch starts a new face-supported local sketch with a non-editable reference boundary. A finished supported rectangle exposes New body only for origin support and Add/Cut only for face support. Add has a translucent green tool preview; Cut has a translucent red preview. The profile outline, direction arrow, mode, and distance remain presentation-only and do not execute the kernel per frame.

The complete snapshot, face, support digest, frame, sketch revision, mode, and distance are frozen in the pending intent. The universal green tick or bare `Enter` is the only publication route. Cancellation restores the previous state; a rejected command retains both the previous body and the complete pending intent.

## Verification

- Exact Add and Cut fixtures assert topology counts, Euler validity, bounds, volume, surface area, centroid, deterministic replay, and source-mapped debug geometry.
- Protocol tests assert the finite, snapshot-bound JSON shape and bounded profile deserialization.
- Rejection tests cover stale/wrong-kind/missing faces, crossed rectangle declarations, non-rectangular or out-of-bounds profiles, non-positive/tiny distances, unsupported source bodies, through cuts, and depth/profile rounding outside `linear_agreement` near the coordinate limit.
- Semantic history tests cover all 130 outputs exactly once and all 58 inputs, with 50 unchanged, 14 modified split/owner records, 66 generated records, and mode-specific boss/pocket roles.
- Workbench tests assert that face selection, sketch entry, input changes, and live preview do not publish a snapshot; only the shared confirmation dispatcher executes the command.
- Visual tests cover the selected-face reference boundary, green Add preview, red Cut preview, and clean committed results.
- Architecture audits continue to reject OCCT/OpenCascade, C/C++ product sources, and UI/render dependencies in kernel crates.

## Consequences

- Artificer now owns a real native topology edit and can exercise the first create-sketch-on-face workflow without an embedded third-party kernel.
- The small exact scaffold is easy to replay and inspect, and it creates a concrete foundation for later face splitting, region, Boolean, and naming work.
- The M4c implementation did not accept a feature result as the source for another feature. [ADR 0013](0013-repeatable-rectangular-face-features.md) later extends this boundary with repeatable local rectangular edits while retaining the M5 document/naming work as a separate concern.
- The schema-v1 declarative Add/Cut cases intentionally retain literal snapshot and face identifiers as exact golden fixtures, tagged `golden-face-ref`. ADR 0013 adds a separate chained fixture with test-only prior-step history resolution; neither mechanism is a persistent product reference.
- Rotated prisms, arbitrary planar faces, non-rectangular profiles, through holes, islands, multiple bodies, general Boolean union/difference, and curved surfaces remain explicit unsupported results.
- OCCT remains an optional, separately built offline comparison oracle only. It cannot provide product topology, preview, validation, or fallback behavior.

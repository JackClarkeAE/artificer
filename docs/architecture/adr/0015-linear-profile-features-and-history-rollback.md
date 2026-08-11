# ADR 0015: Linear-profile features use hole-aware faces and exact push/pull

- Status: Accepted for the M4e/M5a experimental slice; profile representation widened by ADR 0016
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

The M4a constructor accepted only strictly convex polygons. M4c/M4d then added repeatable rectangular Add and blind Cut by splitting the coplanar material around every feature into four separate shoulder patches. That bounded scaffold proved transactional topology editing, but it caused visible seams, rejected valid cuts that crossed an earlier boss/support interface, and could not represent a through hole. The workbench also required typed positive distances and used an extrusion arrow only as decoration. M5a had document-backed History, but no Fusion-style rollback boundary.

This pass widens one coherent native vertical slice without pretending that it is a general Boolean or curved-geometry kernel.

## Decision

### Certified linear profiles

`ExtrudePolygon` and `ExtrudeFaceProfile` accept one implicitly closed, simple **linear** polygon with three to 256 vertices. Convex, concave, and safe collinear turns are supported. Input winding and cyclic start are normalized deterministically. Non-finite coordinates, repeated vertices, self-intersection, insufficient edge or non-adjacent-edge separation, unresolved numerical classification, invalid frames, and coordinate/tolerance failures reject transactionally.

The existing profile-command schema already carries a bounded vertex sequence, so widening that sequence from convex to simple linear profiles does not require another profile wire format. Exact whole-face motion is declared separately as capability `native.push_pull_face.v0`.

Standalone extrusion remains an empty-snapshot constructor with a finite positive distance. Selected-face Add/Cut remains a local edit of one valid solid and shell. Its target may have a rectangular, triangular, or concave outer boundary and existing holes, but the target plane and sketch frame must be world-axis-aligned. The profile must lie strictly inside actual face material and must not touch or enter an existing inner loop.

At this decision point, analytic circles/arcs, multiple profile loops, and profile-owned holes/islands were capability-gated and were never discretized into product topology. [ADR 0016](0016-exact-planar-profile-curves-and-regions.md) subsequently introduces protocol-v4 exact regions, executes bounded linear regions/holes, constructs standalone circle or connected line/arc prisms with exact circular edges and cylindrical walls, and adds exact single-circle selected-face Add/blind/through Cut. Selected-face arcs/mixed loops or circle-profile holes, linear edits on analytic-faced owners, rotated/general supports, and general Boolean union/difference remain capability-gated.

### Hole-aware canonical faces

`Face` owns one outer loop and zero or more ordered inner loops. Inner loops use the opposite face-local winding and participate in ordinary edge/coedge incidence, validation, measures, semantic history, planar-support queries, and source-mapped display triangulation.

A strictly inset Add or blind Cut now keeps the coplanar shoulder as one face with a true inner boundary instead of four trapezoid patches. A rectangular feature on the canonical cuboid therefore produces 16 vertices, 24 edges, 48 coedges, 12 loops, 11 faces, one shell, and one solid. This is direct authoritative topology construction, not a visual merge or post-processing heal.

The face validator rejects intersecting loops, holes outside their outer loop, invalid winding/nesting, and inconsistent manifold incidence. Diagnostic tessellation bridges and ear-clips hole-aware or concave regions deterministically, but tessellation never authorizes topology publication.

### Add and Cut extent semantics

Add remains a collision-free outward local prism. Any material contact outside the supported local rewrite rejects without changing the source snapshot.

Cut supports:

- a blind pocket that terminates inside material;
- a pocket that passes through the void in a prior feature's coplanar shoulder and continues into supporting material;
- an exact through cut when one unambiguous opposite, axis-aligned planar exit face contains the complete footprint; and
- positive tool overtravel, canonicalized to that first certified exit so exact-depth and overtravel requests produce the same semantic body.

An intervening unsupported boundary, ambiguous exit, partial exit containment, or other contact still rejects transactionally. This is not a general `Through all`, `To object`, or multi-body Boolean implementation.

### Exact whole-face push/pull

`PushPullFace { target_face, distance }` uses the selected B-rep face itself as the profile; it does not duplicate the boundary through a sketch or diagnostic mesh. Distance is signed along the face's authoritative outward normal: positive extends/Adds and negative shortens/Cuts. The command supports one valid single-shell, single-solid, axis-aligned linear B-rep when the selected face is:

- planar, exterior, simply connected, and unholed;
- an extrusion cap with one perpendicular inward rail of equal support depth at every boundary vertex; and
- bounded by one orthogonal quadrilateral side face per boundary edge.

The cap outline may be any supported simple linear polygon, including triangular or concave profiles. Moving its vertices, cap plane, incident edge endpoints, and p-curves preserves exact topology cardinality and entity IDs. The operation report maps every output one-to-one as `Modified` or `Unchanged`, gives the selected face the unique `face_push_pull.target_face` role, and records modified side faces plus moved/preserved lower entities, shell, and solid. The document stores the command through the same persistent face resolver used by inset profile features.

Inward motion must leave more than the active minimum feature size before the common support plane. Reaching or crossing that plane would delete or merge topology and therefore rejects transactionally. Holed/annular faces, non-cap faces, rotated sources, unequal or non-orthogonal rails, contact/collision cases, and motions outside the coordinate/precision policy also reject. These restrictions distinguish exact topology-preserving push/pull from a general face-offset or Boolean operation.

### Exterior camera and signed direct manipulation

Creating a face sketch uses the face's outward normal for the smooth camera transition. For all six axis signs, the camera arrives on the exterior side, preserves face-local V as screen-up, and mirrors local U as required by physically moving around the body.

During an inset extrusion or whole-face push/pull preview, the arrow receives pointer priority and maps the drag to a signed distance along the stable outward face normal. Positive distance selects Add; crossing the profile plane automatically selects Cut with the absolute displayed depth; crossing back selects Add. A deterministic vertical fallback handles an end-on projected axis. Zero distance remains an invalid preview and cannot be confirmed.

Dragging changes presentation intent only. It never executes the kernel. The compact green tick or bare `Enter` remains the only publication path, while the red cross or `Escape` cancels the complete staged intent.

### Persisted history rollback cursor

`artificer-model` persists a `HistoryCursor` as `Start`, `After(FeatureId)`, or `End`. It defines the evaluated prefix of the global ordered timeline and is independent of per-feature suppression. Moving the cursor reconciles each body/sketch to its last active association while retaining future feature recipes and cached snapshot associations for an exact roll-forward. Cursor edits are bounded-undoable and validated during native-document loading.

The workbench exposes backward/forward steps and a rollback slider. A completed drag creates one history edit rather than one edit per rendered frame. New features and modeling tools are blocked while the cursor is not at `End`; this slice preserves the future timeline instead of silently truncating it.

## Verification

- Constructor tests cover concave and safe-collinear profiles, deterministic normalization, analytic measures, validation, replay, and every declared rejection class.
- Selected-face tests cover triangular and concave Add/Cut profiles, repeated features on generated ends/sides, actual-material containment around face holes, and all six axis signs.
- Regression tests cover a blind cut crossing a prior boss/support interface and an exact/overtravel through cut with an authoritative exit-face hole.
- Push/pull tests cover all six cuboid cap signs, arbitrary linear prism caps, repeated boss-end motion, exact measures, unchanged topology/identity, complete one-to-one history, persistent rebinding, deterministic replay, and transactional rejection at support contact, on annular/non-cap targets, and for stale/non-finite/tiny inputs.
- Topology tests cover outer/inner loop winding, nesting/intersection diagnostics, Euler characteristic with genus, mass properties, planar support, semantic history, and hole-aware source-mapped display triangulation.
- Presentation tests cover exterior-side camera alignment, concave preview triangulation, handle pointer priority, signed crossing through zero, end-on fallback, cancellation, and a 60 Hz interaction budget.
- Model and UI tests cover cursor serialization, corruption rejection, undo/redo, suppression independence, multibody prefix reconciliation, exact roll-forward, append blocking away from `End`, and slider/step controls.

## Consequences and explicit gates

- A useful family of exact prismatic parts can now be created and edited from arbitrary certified simple linear loops without artificial coplanar face seams.
- Cuts can cross supported prior-feature interfaces or exit an opposite planar face without embedding a general Boolean engine.
- Direct manipulation remains responsive and discoverable while preserving the universal confirmation boundary.
- Rollback is now document truth rather than a presentation-only selection.
- Selecting a certified extrusion cap and clicking **Extrude** now stages exact whole-face push/pull without requiring a surrogate sketch. This subset preserves topology; push/pull of holed faces or motion through the support plane remains gated until a topology-changing offset can trim/delete adjacent faces and emit split/deleted provenance.
- Exact circle/arc payloads, standalone analytic prism construction, region nesting, and single-circle selected-face Add/blind/through Cut are now defined by [ADR 0016](0016-exact-planar-profile-curves-and-regions.md). Selected-face arcs/mixed loops and analytic profile holes remain unsupported, and polygon approximation is never substituted silently.
- General rotated-body edits, holed/non-cap face offsets, support-plane crossing, touching/overlapping profiles, selected-face multi-region edits, multi-body combine/cut, and regularized Booleans remain later M4/M6 work.
- OCCT remains an optional separately built offline oracle. It is not linked, called, or used as a product fallback.

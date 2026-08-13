# ADR 0020: One regularized exact-profile path for selected-face features

Status: Accepted and implemented for the declared S2D domain
- Date: 2026-07-30
- Decision owners: Artificer project

## Context

Artificer already extruded exact standalone profiles containing lines, circular
arcs, complete circles, multiple regions, and holes. Before S2D, selected-face
Add/Cut was narrower: an all-linear implementation and a complete-circle
implementation were chosen from the shape of the incoming profile. That split
made the first native face features testable, but it could not scale to slots,
filleted profiles, trimmed arrangement cells, analytic holes, or several
selected regions without leaking implementation details into the UI and
document history.

The 2D sketch programme promises that any certified region assembled from the
supported line/arc/circle domain can feed an extrusion. Profile compilation and
B-rep consumption are separate certification gates; neither may polygonize an
analytic boundary merely to enter an older path.

## Decision

`ExtrudeFacePlanarProfile` is the sole semantic command for selected-face Add
and Cut. The UI and parametric document store the exact selected
`PlanarProfile2`; they never select a "linear", "circle", or "Boolean" backend.

The native implementation uses a regularized prismatic
imprint/classify/rebuild pipeline:

1. certify the target support, exact profile, frame agreement, sweep direction,
   resource bounds, and supported surface domain;
2. construct exact swept line/arc/circle carriers and their planar/cylindrical
   side surfaces;
3. intersect and imprint the profile and sweep contacts on affected source
   faces without modifying the input snapshot;
4. split affected faces into bounded cells and classify each cell and sweep
   volume against material/void using the active precision policy;
5. retain the regularized material boundary for Add or Cut, cancel coincident
   internal uses, rebuild loops/shells/solids, and preserve unaffected sibling
   bodies exactly;
6. validate topology, mass properties, and the contact policy; and
7. publish one immutable snapshot with complete one-to-many provenance, or
   reject without publication.

Existing rectangular/linear and circular constructors may remain as optimized
implementations only when they are proven equivalent fast paths behind this
same command, diagnostics, validation, and history contract. A failed fast
path may fall through to the regularized implementation; it may never weaken
preflight or silently change exact geometry.

The implemented positive domain is deliberately explicit:

- finite non-degenerate planar supports, including rigidly rotated frames;
- target owners composed of the existing exact prismatic plane/cylinder
  domain;
- one or more strict-inset exact line/arc/circle material regions, direct
  holes, and parity islands;
- disjoint bosses for Add that regularize into the selected owner only;
- blind Cut with material behind every retained floor;
- through Cut with one or more certified exits and complete owner-splitting
  history; and
- prior planar shoulders, parallel/transverse cylinders, and existing voids
  included in the contact regression matrix.

Tangential-only contacts, coincident sweep walls, zero-thickness remnants,
features at or below modeling resolution, cross-body fusion, contacts requiring
unsupported non-transverse surface splitting/merging, and contacts that need a
surface class outside the declared domain reject with stable typed diagnostics.
General Boolean reconstruction and NURBS/general trimmed-surface operations are
not hidden fallbacks. These are defined negative outcomes, not permission to
publish an uncertain solid.

## Transaction and replay contract

Preview and cancellation execute no kernel command. Tick or bare Enter submits
one exact request against the current immutable snapshot. Any validation,
classification, topology, resource, or numerical failure retains the previous
snapshot and authored intent.

A downstream extrusion feature stores the source sketch identity, selected
stable region signatures, operation, distance parameter binding, and persistent
target reference where required. Rebuild reevaluates the current sketch,
resolves those regions, compiles a fresh exact profile, and then issues
`ExtrudeFacePlanarProfile`. It never reuses a stale cached profile after an
upstream sketch edit.

## Verification gates

The positive matrix covers rectangles, arbitrary polygons, slots, trimmed
cells, line/arc fillets, chamfers, circles, annuli, holes, disjoint selected
regions, and parity islands for Add, blind Cut, and supported through Cut on all
six signed axis directions and rotated planar supports. Tests assert exact
carrier classes, topology, volume, area, centroid, bounds, deterministic replay,
and provenance.

The contact matrix includes prior shoulders, cylinders, and voids. Negative
tests cover tangent/coincident/zero-thickness contacts, unsupported surfaces,
cross-body contact, precision-boundary cases, and resource ceilings. Every
failure proves the input digest and active document body revision are unchanged.

OCCT may compare offline area, volume, and topology samples as a development
oracle. It is never linked, invoked as a product fallback, or used to generate
authoritative Artificer geometry.

## Consequences

New sketch tools remain additive: once their analytic outputs compile into a
certified region, no shape-specific UI or document extrusion branch is needed.
The implemented splitting, classification, regularization, and provenance
foundation is reusable by later Booleans and multi-body modeling, but does not
claim those broader operations.

| Delivered | Deferred or rejected |
|---|---|
| One exact selected-face command; line/arc/circle regions; holes and parity islands; multiple selected regions; rotated planar supports; Add, blind Cut, and certified through Cut; sibling preservation and split provenance | Tangent/coincident/zero-thickness topology, non-transverse contacts requiring unsupported split/merge, cross-body fusion, general Boolean reconstruction, NURBS/general trimmed surfaces |

Until the positive matrix for a contact class passes, the application exposes a
precise capability diagnostic and retains the last valid body. It does not
approximate or claim that a valid sketch necessarily implies a currently
supported face operation.

### 2026-08-07: the regularized path reads coordinates in the sketch's frame

The regularized path reasons in axis-aligned intervals and grids, which is what
keeps it tolerance-free. Those axes need not be the *world* axes, though — only
one frame used consistently. Resolving the axis index against the sketch frame
instead therefore lifts the operation onto arbitrarily oriented solids without
changing a single interval test, and the supported domain keeps its shape:
a solid that is box-like *in the sketch's frame*. The rejections rename
accordingly — `FACE_FEATURE_TARGET_NOT_AXIS_ALIGNED` and
`FACE_FEATURE_FRAME_NOT_AXIS_ALIGNED` become `..._TARGET_NOT_PLANAR`,
`..._TARGET_NOT_ALIGNED_TO_FRAME`, `..._FRAME_NOT_ORTHONORMAL`, and
`..._FRAME_OFF_TARGET_PLANE`. Whole-face push/pull needed only its gate
removed: it was already written in the target's own outward normal.

Two exactness details had been hiding behind the world frame, where the
arithmetic happens to be exact:

- A profile point is built as `origin + u·x + v·y`, so its height above the
  frame plane is a rounding of the origin's. On an axis-aligned frame that
  rounding is exactly zero; on a turned one it is a few ulps of the coordinates
  involved, so the residual bound scales with them rather than sitting at an
  absolute epsilon.
- The scaffold identifies vertices by exact coordinate equality. A grid node
  that came from an input point now keeps that point verbatim; reconstructing
  it from its own coordinates would round-trip through the frame and move it by
  ulps, which was enough to break the source-corner audit. Profile points carry
  their known plane index rather than re-deriving it.

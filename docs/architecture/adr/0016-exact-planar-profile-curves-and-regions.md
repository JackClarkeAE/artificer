# ADR 0016: Exact planar curves form deterministic material regions

Status: Accepted for protocol v4, native standalone analytic extrusion, and exact-circle selected-face editing
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

The first extrusion commands accepted one vertex array whose order simultaneously described sketch history, wire traversal, winding, and material. That proved a constructor, but it is not a viable CAD profile model. Users author sides in arbitrary order and direction, one sketch can contain several disjoint regions, nested boundaries alternate between material and void, and circles/arcs must remain analytic rather than becoming hidden display polygons.

M4e already gave a planar B-rep face an authoritative outer loop plus inner loops. The next boundary needs to carry exact sketch curves and explicit material regions from UI to kernel, construct useful curved product topology, retain bounded requests and deterministic replay, and reject every unsupported local edit transactionally.

## Decision

### Protocol-v4 profile contract

`PlanarProfile2` is an ordered deterministic representation of `PlanarRegion2` records. Each region has one outer `PlanarLoop2` and zero or more directly nested hole loops. An island inside a hole is another material region; it is not encoded as a hole of a hole. A loop is an ordered closed wire of exact `PlanarCurve2` uses:

- `Line { start, end }`;
- `CircularArc { center, start, end, direction }`, whose direction selects the unique non-zero sweep below one revolution; or
- `Circle { center, radius, direction }`, kept distinct so coincident arc endpoints cannot ambiguously mean an empty arc or a full revolution.

Outer loops travel counter-clockwise and holes clockwise when viewed along the profile-frame normal. The protocol payload is declarative input, not trusted topology: the kernel re-certifies finiteness, degeneracy, connectivity, closure, winding, intersections, containment, feature separation, and the active precision/coordinate policy.

Protocol v4 introduces these exact region commands and curve uses. Requests are bounded to 32 regions, 128 total loops, and 1,024 total curves. The custom deserializer spends that budget while reading nested sequences so an overflowing JSON element is rejected before it is retained; in-process kernel preflight repeats the limits. A v4 kernel still requires an exact v4 request version—native-document migration does not imply automatic migration of older command journals.

### Order-independent sketch analysis

Sketch insertion order and authored direction are not modeling semantics. The workbench builds connected components from exact line/arc endpoint keys, requires every vertex in a closed component to have degree two, and walks each component deterministically. Endpoint snapping may author identical coordinates, but extraction does not add an epsilon, move an endpoint, or bridge a gap. Open or branched components reject instead of being guessed closed.

A complete circle is one seam-free sketch loop. In the interactive analyser, an arc's finite start/end radii must agree within `1e-9` relative to their coordinate/radius scale, and its sweep must stay strictly between `1e-6` and `360 - 1e-6` degrees; a full circle must use `Circle`. These are application-certification bounds, not permission for the kernel to skip precision-policy checks. Wires are tested for self-intersection and pairwise intersection before containment. Coincident, crossing, or touching boundaries are not assigned regions heuristically.

Every certified loop receives a containment depth. Even depths represent material; odd depths represent void. Material loops become region outers and their directly contained next-depth loops become holes. Depth-two and later even loops become separate regions, preserving parity islands. The analyser reverses uses where necessary, rotates each multi-curve loop to a deterministic start, sorts holes and regions, and exports outer loops counter-clockwise and holes clockwise. Renderer sampling is permitted for fill and preview only; no display polygon may replace an exact command curve.

### Standalone exact extrusion

`ExtrudePlanarProfile` operates only on the empty source snapshot and publishes one solid per material region in one compound snapshot. Any failure rejects the complete request, so a multiregion operation cannot expose partial construction. That compound remains one document body association: the Browser labels it `Body group N · k solids`, and visibility, whole-body transform, and rollback/roll-forward address the group. Separate New Body operations still create independent body rows/branches; member solids inside one group do not yet receive independent Browser controls.

The all-linear path retains the established simple-polygon constructor: each loop has three to 256 line uses. It supports multiple pairwise-disjoint material regions, non-nested direct linear holes, and a parity island inside a hole as another output solid. Holed construction currently uses the exact axis-aligned face-cut route and therefore rejects a non-axis-aligned holed frame. Region/hole declaration order, winding, and cyclic start are canonicalized before construction in this path.

If any curve in the request is analytic, the complete request uses the analytic builder. It supports multiple material-disjoint regions whose outer and direct-hole boundaries are complete circles or exactly connected line/arc wires. Direct holes retain clockwise winding and the same exact curve types as outers; the positive matrix includes both a rectangular outer with a circular hole and a circular outer with a linear rectangular hole. Cross-region certification compares every loop pair for minimum boundary clearance and tests each outer representative against the other region's outer-minus-holes material. A separate depth-two region wholly inside an annulus void is therefore valid and becomes another solid, while an outer nested inside filled material rejects. Finite non-degenerate frames and positive representable distances are normalized and checked against the active precision/coordinate envelope.

`Curve3::Circle` and `Curve2::Circle` carry an exact carrier frame and parameter range. `Surface` carries either a plane or cylinder. A protocol `Circle` becomes two exact semicircle edges on each cap, two cylindrical wall patches, and two explicit vertical seam generators, so every loop has real vertices and every wall patch has a non-ambiguous UV rectangle. A circular-arc use produces one exact cap arc and one cylindrical wall patch. Cap and wall coedges own exact p-curves; display tessellation is never reused as topology.

Validation checks curve/cylinder frame orthonormality, positive radius, finite non-zero parameter ranges, topological endpoint agreement, p-curve endpoints, full p-curve locus/tangent agreement, loop orientation, edge-use incidence, shell connectivity, and positive analytic volume. Semantic hashing includes analytic carrier frames and ranges. Measures integrate exact line/circle p-curves for surface area, volume, and centroid; bounds include interior circle extrema. Similarity transforms update exact carriers, radii, cylinder axes, and metric UV components. Debug edges, planar caps, and cylindrical faces are tessellated only after publication according to the versioned precision policy and retain source entity mapping.

### Selected-face local editing and split history

`ExtrudeFacePlanarProfile` accepts two exact snapshot-bound, axis-aligned planar Add/Cut domains. The linear domain is exactly one connected all-linear material region with any number of non-nested direct linear holes, on a wholly linear-faced owning solid. The analytic domain is exactly one counter-clockwise complete-circle outer with no profile holes. That circle supports Add, blind Cut, and a resolved through Cut; it becomes exact semicircular edges, cylindrical walls, seams, and p-curves rather than a polygon. Its owning shell may already contain cylindrical siblings. The circle path certifies actual face-material containment and minimum boundary clearance, resolves a supported planar exit for through Cut, and runs a conservative exact sweep-contact preflight across the complete snapshot. Transverse planar faces are classified against their trimmed material, allowing a smaller concentric through-hole to cross the void in an earlier circular boss shoulder. Planes parallel to the sweep axis use exact distance; cylinders parallel to it use exact radial distance; an overlapping perpendicular or otherwise unsupported cylindrical contact rejects before topology changes. A circular Add that would enter a sibling solid therefore fails transactionally instead of creating a cross-solid Boolean. Topology validation, complete history, source mapping, and persistent target rebinding remain kernel-authoritative. Selected-face arcs/mixed loops, multiple regions, and analytic profile holes reject as `FACE_PROFILE_ANALYTIC_DOMAIN_UNSUPPORTED`; a linear edit on an analytic-faced owner rejects as `FACE_FEATURE_SOURCE_UNSUPPORTED`, and no fallback facets either request.

A compound snapshot may contain several independent solids. The selected-face route extracts only the target face's owner, applies the local edit, then merges the result with every sibling solid unchanged. A through cut may itself divide the owner into more than one solid. Operation history maps the source shell and solid to all resulting fragments while covering every input and output. Persistent resolution consequently returns `Ambiguous` for an unqualified reference crossing that one-to-many split instead of choosing a fragment.

The older polygon commands remain compatibility paths for their declared domains. New workbench region intent uses the v4 exact-profile commands so holes, disjoint regions, and curve identity are not lost.

### Document version and confirmation

Native document schema v2 can persist the v4 replay actions. The loader accepts document v1 through an in-memory migration and the next serialization writes v2; versions outside `[1, 2]` fail closed. This envelope migration does not make the kernel accept a request whose protocol version is not v4.

The feature preview consumes the same region structure as the command. Linear/analytic fills exclude hole interiors and keep disjoint material regions separate. Curves may be adaptively sampled for display, while the exact request stays untouched. Unsupported annular/mixed analytic selected-face profiles and linear profiles on an analytic owner are gated before preview staging; an exact circle on an analytic cap remains eligible. A preview remains presentation intent: only the shared green tick or bare `Enter` executes it, and any rejection retains the last valid snapshot.

## Verification

- Sketch tests permute and reverse separately authored line/arc entities, require canonical loops/regions, prove parity holes/islands, and ensure an idle line-continuation anchor cannot block Finish or Extrude.
- Graph tests reject open/branched components, exact-endpoint gaps, malformed arcs, self-intersections, coincident curves, and intersecting/touching loops without inventing closure or nesting.
- Protocol-v4 tests round-trip lines, arcs, circles, holes, and regions and exercise incremental failure at all three resource ceilings.
- Linear-kernel tests cover hollow prisms, disjoint multisolids, declaration/winding invariance, selected-face Add/blind/through Cut with holes, retained sibling solids, complete split history, persistent split ambiguity, and transactional overlap/frame/resource rejection.
- Analytic-kernel tests cover an exact disk, annulus, rectangular outer with circular hole, circular outer with linear rectangular hole, mixed line/arc region, asymmetric disjoint circles with aggregate centroid, and an annulus void containing a separate depth-two circular island. The island publishes two solids with exact `12π` volume at height two. These tests assert exact topology types/counts, validation, closed-form area/volume/centroid, and bounds.
- Selected-face analytic tests cover exact-circle Add, blind Cut, and resolved through Cut on every signed cuboid face. The public command path additionally proves exact measures, complete history, reusable planar support, and source-mapped curved display geometry. Chained tests cover a circular boss followed by a smaller concentric through-hole across its shoulder void and exact-circle Add/Cut on a prior linear rectangular boss. Negative tests cross an existing perpendicular cylinder and direct a circular Add into a sibling solid; both require `FACE_FEATURE_SWEEP_COLLISION`, and the compound case proves the source digest, measures, and debug scene remain unchanged. Arcs/mixed loops, multiple regions, and profile holes reject precisely.
- Validator mutations cover invalid analytic frames/ranges and a bowed p-curve whose endpoints match while its interior locus does not. Transform, semantic-digest, and debug-scene paths cover analytic carriers without changing modeling authority; a two-solid group-transform regression preserves cardinality/counts and complete one-to-one history while scaling aggregate measures.
- UI semantic and visual tests commit circular and annular extrusions through the shared confirmation gate, snapshot analytic preview/commit, and retain the 60 Hz interaction budget.
- Model tests round-trip v2, migrate v1 in memory and rewrite v2, reject other versions, require persistent targeting for `ExtrudeFacePlanarProfile`, and treat one-to-many face/shell/solid history as ambiguity.

## Consequences and explicit gates

- A sketch no longer has to imitate wire order. Separate line/arc entities describe a deterministic profile when their exact endpoints form valid degree-two components.
- Holes and parity islands are first-class profile topology rather than overlapping solids or flattened polygons.
- Exact standalone circular and mixed line/arc prisms are now product topology, not a future transport format. Tessellation remains a display product.
- Analytic-route direct holes use the same exact line/arc/circle wire model as outers. Deeper region ownership inside one `PlanarRegion2`, boundaries at or below minimum clearance, and region outers nested in another region's filled material remain gated. A parity island is instead represented as another region and may lie wholly inside a direct hole.
- Selected-face editing remains a one-region, axis-aligned local construction. Exact complete-circle Add/blind/through Cut includes conservative trim-aware contact preflight, but contacts requiring unsupported cylindrical splitting/merging still reject. Arc/mixed-loop or circle-with-hole profiles, linear edits on analytic-faced owners, selected-face multiregion operations, rotated/general supports, cross-solid Booleans, and regularized Boolean reconstruction remain later work.
- Exact endpoint identity is deliberately strict. A future repair/healing tool must expose its edits and uncertainty rather than silently changing certification.
- The workbench canonicalizes user-authored region order. The analytic kernel builder currently preserves the declared region sequence, and the snapshot digest remains storage-sensitive, so external protocol callers must canonicalize equivalent analytic region sets if they require identical snapshot IDs.
- The broader M1 filtered/exact predicate ladder still needs to replace remaining binary64 classification uncertainty with certified outcomes.
- A split shell/solid is genuinely ambiguous for persistent naming until a richer qualifier or explicit repair choice selects a fragment.
- A multiregion extrusion or split result is intentionally one UI/document body group. This exposes compound state honestly without pretending that per-solid body ownership has already been implemented.
- OCCT remains an optional separately built offline oracle. It is not linked, called, or used as a product fallback.

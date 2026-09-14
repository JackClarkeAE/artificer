# ADR 0034: Corner blends and band run-outs

Status: implemented — `edge-finish/vertex-blend`, the last exact rung of the
edge-finish ladder. Convex straight edges between flat faces blend at any
radius or bevel distance; a vertex where all three edges are chosen closes with
a sphere octant or a planar triangle, and a vertex where one is chosen runs the
band out into the face across it. The rest of the body — holes, slots, bores,
blends earlier features left — is copied through, carrier and p-curves intact.

- Date: 2026-09-14
- Decision owners: Artificer project

## Context

The ladder in `regularized_edge_finish` had four rungs and a gap. The exact
ones each rebuild a whole body from a recovered profile: `edge-finish/prism`
runs a 2D corner operation on a prism's profile and re-extrudes it,
`edge-finish/rim-blend` and `edge-finish/rim-loop-blend` offset a cap profile
inward and revolve or sweep the section it leaves. None of them can answer for
the three edges meeting at a corner of a box, because the result is no longer a
prism in any direction and no single profile describes it.

So the request fell to `edge-finish/faceted`, which rebuilds the body from a
tessellation and labels the result an approximation. For the most ordinary
request a CAD user makes — round the corner of a block — the kernel either
published a faceted body or, where the tessellated shell would not weld,
refused with `EDGE_FINISH_BLEND_UNSUPPORTED`: "not blending".

The geometry is not hard. It is the classical rolling-ball construction, and
every piece of it is already in this kernel's vocabulary.

## Decision

### What the rung constructs

A convex edge between planes `A` and `B` with interior dihedral `θ` carries a
ball of radius `r` whose centre runs along the line at distance `r` from both.
The ball touches each face along a line set back `r·cot(θ/2)` from the edge and
sweeps the `π − θ` cylinder between them — `Surface::Cylinder`, parameterized
with the edge direction as its axis and `A`'s normal as `radial_u`, so the two
tangency lines fall on `u = 0` and `u = π − θ` exactly. A chamfer replaces that
cylinder with the plane through the two lines set back by the chamfer distance,
whose normal is the outward bisector `sin(θ/2)·n_A − cos(θ/2)·(n_A × d)`.

Each band then has to end, and there are two ways.

**A corner patch, where all three edges at the vertex are chosen.** The ball's
centre is the single point at distance `r` from all three planes — Cramer's
rule on three unit normals, whose determinant is bounded directly by the
angular agreement. The patch is the piece of the sphere of radius `r` about it
bounded by the three cylinders' end circles, and each of those is a *great*
circle, because every cylinder's axis passes through that centre. A chamfer
closes the same corner with the triangle through the three points where the
set-back lines meet inside each face — planar by construction, with no
condition on the angles at all.

**A run-out, where one is.** The band stops against the third face at that
vertex, the one the selected edge does not border. The ball's end circle lies
in that face's own plane, so the run-out costs one arc in a face that was
already flat, and shortens the two edges beside it to the arc's feet. This is
what lets three edges meeting at one vertex blend without dragging the whole
body's edge graph in with them — on a box, the "closed" edge sets are all
twelve edges or nothing — and what lets a second corner blend on a body an
earlier feature already rounded.

`Surface::Sphere` and its pole handling were built for ADR 0023's rim-loop
corners and validated then; this is the first builder that emits one outside a
rim loop. The patch closes as a three-sided face: the equator between the two
faces the pole is square to, and a meridian rising from each of them to the
pole, where both meet at one vertex. No pole edge is needed, for the same
reason ADR 0023 found — the meridians *are* the adjacent bands' end arcs.

The body around the blend is not rebuilt. Every planar face the selection
insets has its boundary replaced edge by edge; every other face — a bore wall,
a slot end, a blend from an earlier feature — is copied with its carrier,
loops, p-curves and parameter ranges unchanged.

### What it refuses, by name

Nothing is approximated under this rung's name. Every result it publishes is
validated first, and a candidate that does not certify is handed on rather than
committed.

| Code | When |
|---|---|
| `VERTEX_BLEND_CORNER_NOT_SQUARE` | A fillet corner where no one of the three faces is square to the other two. The sphere patch is written with one face's normal as its pole and the other two tangencies on its equator; a corner without that squareness needs a general great circle in the sphere's own parameters, which the line-and-circle p-curve vocabulary cannot name. A chamfer there is fine. |
| `VERTEX_BLEND_RUNOUT_NOT_SQUARE` | A fillet run-out against a face not square to the edge. The end curve would be an ellipse on the cylinder — a `Curve2::Harmonic` p-curve the validator certifies only for the Steinmetz seam. A chamfer runs out on any flat face, because its end is a straight line. |
| `VERTEX_BLEND_CORNER_INCOMPLETE` | Two of a vertex's three edges chosen. The band would have to fade out along the third, which is a variable-radius surface, not a cylinder. |
| `VERTEX_BLEND_CORNER_UNSUPPORTED` | A vertex joining other than three edges and three flat faces, or three faces too nearly parallel for a ball to meet all of them. |
| `VERTEX_BLEND_CORNER_CURVED` | A corner meeting a curved face — a hole wall, a slot arc, a blend an earlier feature left. |
| `VERTEX_BLEND_RADIUS_MISMATCH` | That curved face is a cylinder of a different radius: a corner takes one blend size, not two, and saying so precisely beats the general sentence above. |
| `VERTEX_BLEND_MIXED_SELECTION` | Straight edges and rims that never touch each other in one selection, with the remedy: one feature each, either order. |
| `VERTEX_BLEND_DISTANCE_INVALID` | The blend does not fit — a band with no length between its two ends, an edge beside a run-out consumed entirely, a rebuilt face boundary that would cross itself, or an inset that would reach a hole or slot. |
| `VERTEX_BLEND_RUNOUT_UNSUPPORTED` | A run-out whose neighbouring edge is curved, so there is no straight edge to shorten. |
| `VERTEX_BLEND_CONSTRUCTION_FAILED` | The body was built and did not certify. It names the validator's own first diagnostic and its path. |

### Which refusals stop the ladder

A refusal carries a `certain` flag: whether no later rung can publish an honest
answer either. Only `VERTEX_BLEND_DISTANCE_INVALID` is certain, matching how
`PRISM_EDGE_FINISH_DISTANCE_INVALID` and `RIM_LOOP_DISTANCE_INVALID` already
stop the ladder — a request that does not fit does not fit for anyone, and
publishing an approximation of a blend that overruns its own body would be
worse than refusing.

Every other refusal is uncertain and the faceted tier still runs. A half
-selected corner, a corner that already carries a blend, a fillet that would
run out on a slant: these are statements about *this rung's* vocabulary, not
about the request, and a labelled approximation beats a refusal. Where the
faceted tier then fails as well, this rung's own sentence is published instead
of the general `EDGE_FINISH_BLEND_UNSUPPORTED`, because it says more.

One refusal holds its tongue rather than misleading. Advising a caller to split
a mixed selection in two is only sound when the two halves never touch: a
plate's box edges and a bore's rim are two features, but a prism cap's rim loop
is one chain of straight runs and arcs that an earlier rung owns and declined
for its own reason. So `VERTEX_BLEND_MIXED_SELECTION` is raised only when no
edge this rung can read shares a vertex with one it cannot.

### Certification

`build_vertex_blend` validates its own candidate before returning it, and a
candidate with any diagnostic is refused rather than committed. Beyond that the
usual gates apply unchanged, which is the point of staying inside the existing
vocabulary: `validate_face_pcurves` certifies each new p-curve against its
edge's analytic locus — cylinder generators and rings, sphere latitude circles
and meridians, plane lines and circles — `loop_parameter_area` certifies every
new loop winds counter-clockwise in its own parameters, and `validate_edge_uses`
certifies every edge is used exactly twice with opposite senses.

`crates/kernel/tests/vertex_blend_tests.rs` derives every expectation in the
test file itself and compares against the kernel's exact measures at `1e-9`
relative: the Minkowski closed form for a box rounded on all twelve edges, the
wedge-and-corner decomposition for one chamfered on all twelve, and for a
single corner the three prisms of cross-section `r²(1 − π/4)` plus the ball
octant a fillet leaves, or the three wedges of `t²/2` less their pairwise and
triple overlaps plus the tetrahedron the corner triangle takes.

## Consequences

Rounding or bevelling the corner of a block is now exact, and so is doing it a
corner at a time. A filleted cube is six planes, twelve cylinders and eight
sphere octants, and exports as that — `SPHERICAL_SURFACE` and
`CYLINDRICAL_SURFACE`, not a tessellation.

Work the faceted tier used to take now goes exact, and that has a cost worth
stating. A body the faceted tier produced was all planes, so a later faceted
finish could run on it; a body this rung produces carries cylinders and a
sphere, and the faceted tier cannot rebuild a face against those — it writes
p-curves that do not certify. So a corner blended exactly no longer accepts a
faceted finish along the edges beside it, where a corner blended by
approximation did. Those follow-on requests now refuse by name
(`VERTEX_BLEND_CORNER_CURVED`, `VERTEX_BLEND_CORNER_INCOMPLETE`) rather than
approximating, which is the correct behaviour for this kernel but is a
capability the ladder had and has lost until blend-to-blend corners land.

Two faults in the faceted tier were fixed on the way there, both of them
reached by handing it a body with exact curved faces — something the ladder
could always do, and now does far more often.

The first was a crash. Its BSP walked the tree with the thread's stack, and a
body carrying a tessellated sphere and three cylinders builds a tree deep
enough to overflow it, aborting the process rather than refusing. `build`,
`invert`, `clip_to`, `clip_polygons` and `all_polygons` now each carry their
own stack. The polygon order they produce is preserved exactly, because the
first polygon of a list becomes the root plane of the next tree built from it.

The second was the wait. Refusing a chamfer beside an exact corner blend took
12.5 seconds in release and 98 unoptimised — spent tessellating a ten-face
body into sixteen thousand triangles, running the BSP over them, failing to
certify the result, and then repeating the whole thing once per selected edge
through the logical-successor path. `MAX_SOURCE_POLYGONS` now declines a body
over four thousand polygons before any of that starts, and the refusal comes
back in 22 ms. It is a declared limit in the same spirit as the tier's
existing 64-target cap, not a judgement about the request: a body over it
might well have been rebuilt, given the seconds. What makes the limit the
right one is what the tier would have published if it had succeeded — every
face it emits is a plane, so its largest certified result here took a
seven-face body in and returned 544 faces. A body that needs thousands of
facets to answer is not an answer worth waiting for, and the exact rung's own
sentence is published instead.

The general blend frontier is unchanged and still deferred: variable radius,
blends along curved edges, blend–blend corners at different sizes, and corners
blended only part way. Each of those now refuses by its own name instead of
arriving as a faceted body, which is the difference this rung was built to
make.

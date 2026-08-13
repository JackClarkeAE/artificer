# ADR 0025: Analytic surface intersections and the Boolean domain oracle

Status: implemented — the intersection library, the domain oracle, the
  prism reductions (through cuts, blind pockets of any lateral shape,
  interior voids), and the general imprint/classify/regularize/sew engine
  for plane- and cylinder-faced operands at any relative orientation all
  ship; the faceted Boolean path is deleted; tangency and coincidence fail
  closed with gates
- Date: 2026-08-07, completed 2026-08-08
- Decision owners: Artificer project

## Context

Every modelling operation in this kernel is exact. Extrusions, revolves,
vertical-edge blends, and rim-loop blends all build their own topology
directly, so none of them has ever needed to ask where two arbitrary surfaces
meet. Booleans do, and that is why they are still the narrowest operation in
the system: `execute_boolean` rejected any operand with a single non-planar
face, and even for planar operands it combined *tessellated scenes* rather than
analytic carriers.

The consequence is sharp. The kernel can now build a cylinder with exact torus
rim fillets, sphere corner patches, and cone chamfer bands — and then cannot
subtract it from anything. Every exact surface added over the last several
milestones is stuck inside whichever solid created it.

M6 is the milestone that fixes this. Its first deliverable is the one this ADR
implements: the intersection graph.

## Decision

### A published matrix, not a runtime search

The general surface/surface intersection has no closed form and, for this
kernel, no representation: the curve vocabulary is `Curve3::Line` and
`Curve3::Circle`. An oblique plane through a cylinder meets it in an ellipse.
That is not a failure of the geometry and not a numerical problem — it is a
curve the kernel cannot yet name.

So the domain is stated as a matrix and refused outside it, rather than
approximated or discovered:

| | Plane | Cylinder | Cone | Sphere | Torus |
|---|---|---|---|---|---|
| **Plane** | line | circle ⟂ axis, generators ∥ axis | circle ⟂ axis | circle | circles ⟂ axis, circles through the axis |
| **Cylinder** | | coaxial, parallel axes | coaxial | centre on the axis | — |
| **Cone** | | | coaxial | — | — |
| **Sphere** | | | | any pair | — |
| **Torus** | | | | | coaxial, equal major radii |

Everything in the matrix is derived algebraically and is exact. Everything
outside returns `IntersectionError::Unsupported`.

Three results are distinguished, because reconstruction will treat them
differently:

- `Curves` — one or two unbounded lines or circles. Trimming them to the
  faces' own extents belongs to the caller, not to the carrier.
- `Empty` — the carriers do not meet, *or* meet at isolated points. A tangency
  point carries no curve to imprint, so it is empty rather than a degenerate
  curve.
- `Coincident` — the same surface twice. Overlap has to be resolved by
  classification, never by imprinting.

Each arm is verified by sampling: every returned curve is evaluated at eight
parameters and required to lie on *both* carriers to 1e-12. A wrong centre,
radius, or direction cannot survive that, and it tests the geometry rather
than the algebra that produced it.

### The oracle runs before reconstruction, not during it

`surface_intersection::first_unsupported_pair` walks every carrier pair
between two shells and names the first that leaves the matrix. Booleans call
it in preflight, which lets a curved operand earn one of two different
refusals:

- `BOOLEAN_SURFACE_PAIR_UNSUPPORTED`, naming the two surface classes — a limit
  of the curve vocabulary. This will not change until the vocabulary does.
- `BOOLEAN_ANALYTIC_RECONSTRUCTION_PENDING` — every pair intersects exactly,
  and only the rewrite stages are missing.

The distinction matters because one is "never" and the other is "not yet", and
a user staring at `Unsupported` cannot tell them apart. It also means the
reconstruction work below can be measured: as it lands, cases move from the
second refusal to a published result, and the first refusal stays put.

## The prism reduction (implemented)

The first inhabited corner of reconstruction is exact and complete in itself:
when both operands are prisms along one shared direction — which covers every
extrusion in the system, curved walls included — and their slabs line up, the
3D Boolean reduces *without approximation* to a regularized 2D Boolean of the
profiles. Union and intersection require the same slab; difference requires
the tool to pierce the target's full height. The result is itself a prism, so
it rebuilds through the certified analytic extrusion path and inherits its
nesting, disjointness, and minimum-feature gates, plus full validation.

The 2D engine (`profile_boolean.rs`) runs the four stages in the plane:

- **Imprint** — line/line, line/arc, and arc/arc crossings in closed form,
  with both operands' segments split at *the same* `Point2` bit for bit. A
  welding pass first gives each input loop exact junction identities, because
  committed pcurves evaluate a shared seam vertex to values an ulp apart.
- **Classify** — each piece crosses no boundary of the other operand, so one
  interior sample (confirmed by a second) decides its side by even-odd count.
- **Regularize** — directed-boundary selection: union keeps outside,
  intersection keeps inside, difference keeps the minuend outside plus the
  subtrahend inside reversed. Material stays on the left throughout, so
  result orientation is by construction, not by repair.
- **Sew** — chaining by exact endpoint identity, then even-odd nesting into
  outers and holes. Splitting cuts yield several regions and publish several
  solids; a subtracted annulus leaves its island as a separate solid.

Everything outside the transverse-crossing domain fails closed: coincident
carriers, tangencies, slivers below the feature floor, chaining ambiguity.
`execute_boolean` tries this path first; a full-height drill through a plate
now publishes exactly (`boolean_probe.rs` pins volumes against independent
rectangle/disc/lens/segment formulas, plus the drilled centroid). The
workbench's Combine/Subtract/Intersect commands ride it with no UI changes.

### The stacked builder: blind pockets

A difference whose tool stops inside the slab but pierces exactly one cap is
not a prism — but it is two prisms glued at the floor plane, and both of them
come from the certified extrusion builder: the full profile below the floor,
the profile with the tool as an extra hole above it. The glue is entity
surgery rather than geometry:

- shared rim vertices and edges at the interface are welded into single
  records (matched by position and carrier; a full circle's two semicircles
  share both seam vertices, so edges are disambiguated by their midpoints);
- both interface caps are deleted;
- one new planar floor face covers the tool region, its boundary loop
  reusing the pocket wall's existing bottom rim edges with pcurves along the
  tool profile itself.

Every invariant the surgery must preserve — edge use counts and senses,
pcurve loci, loop closure, the Euler characteristic — is then checked by the
validator on the merged result exactly as for any other candidate; nothing
about the gluing is trusted by construction. Both opening directions work:
the axis search tries each candidate direction in both senses, so a pocket
opening downward is built by the same top-piercing code in the flipped frame.

The stacked builder covers any lateral shape. The lower layer is the
target profile *imprinted* — split at every crossing with the tool, so its
rim edges align piece for piece with the upper layer's. The upper layer is
the 2D difference, possibly several regions: a holed tool's island survives
as a pillar standing on the floor, its own bottom cap dissolved into the
floor's inner loop. The floors are the 2D intersection, possibly annular,
their boundary loops borrowing the lower layer's split wall-top edges along
the target boundary and the upper layer's pocket-wall bottoms along the
tool's. All three derive from one deterministic imprint, so their shared
vertices are the same floats. A tool that swallows the whole profile
degenerates to the lower prism alone.

Landing the holed cases surfaced a genuine robustness bug in the even-odd
ray cast: `point_inside_loop` counted arc crossings by angular inclusion,
and a ray through an ulp-shifted seam vertex fell into the measure-zero gap
between a circle's two half-arcs — zero crossings where there should be
one. The arc arm now splits each arc into y-monotone pieces and applies the
same endpoint-straddle rule as the line arm, using the segments' stored
(bit-exact, seam-shared) endpoints, so vertex ties resolve exactly as they
do for polygons.

### Interior voids: inner shells

A tool strictly interior in both profile and height carves a closed cavity,
and the topology vocabulary gained what that needs: `Solid` now carries
`inner_shells` alongside its outer shell. A cavity shell is the tool prism's
own certified boundary with its material side flipped — and the flip follows
the vocabulary's standing convention (*reverse the surface, never the
loop*): a plane swaps its u and v, a cylinder negates its angular sign,
every pcurve maps through the same in-plane mirror, and each loop reverses
traversal so it stays positively wound in the mirrored frame. With cavity
faces oriented away from the material, every flux-based measure subtracts
the void with no special case, and the validator applies its ordinary
families — face orientation, edge use, pcurve loci, per-shell Euler — to the
inner shell exactly as to the outer. The prismatic measuring shortcuts
refuse cavity solids outright; the exact shell engine and the polyhedral
fallback (now iterating every shell of a solid) both measure them exactly.

Landing pockets exposed a latent mismeasure: solids of planes and cylinders
were still routed to a prismatic measuring strategy whose profile-times-
height shortcut silently assumed a pure extrusion — a pocketed plate came
back as if drilled through. The exact shell engine now owns every solid with
any curved face, at any orientation, and the prismatic strategies only ever
see pure planar solids.

## The general engine

Beyond the prism reductions, `analytic_boolean.rs` runs the four stages over
whole shells at any relative orientation. The reduction that keeps it exact
is per-face: every face's kept portion is a 2D Boolean *in that face's own
parameter space* between the face's region and the other solid's **section**
on the face's carrier. Sections are assembled from the intersection matrix —
each face of the other solid contributes its carrier-intersection curve,
clipped to its own parameter region by the shared chord machinery, and the
welded pieces chain into closed section loops. Faces the other solid never
touches classify wholesale by exact parity ray casting, with awkward fixed
directions retried before any refusal. The kept pieces from both operands
then sew (`sew.rs`): vertices weld by position, edges by endpoints and
midpoint, edge-connected components become shells, and a component with
negative enclosed volume attaches to its enclosing solid as a cavity.

The gates run crossed bars through all three operations against
inclusion-exclusion volumes, hollow a block with a tool rotated off every
axis, and pin the split difference publishing two solids. The old planar
Boolean gate — two axis-aligned boxes crossing — now passes through this
engine with identical volumes, which is what allowed stage 4 to complete:
`faceted_boolean::combine_scenes` is deleted, and no Boolean tessellates
anything anywhere.

Tangential and coincident contact fail closed at whichever stage first sees
them — a coincident carrier in the section builder, a tangency in the 2D
imprint, a degenerate ray in classification — and surface as
`BOOLEAN_CONTACT_UNSUPPORTED`, gated by shared-face and kissing-cylinder
tests. Out-of-matrix carrier pairs refuse as
`BOOLEAN_SURFACE_PAIR_UNSUPPORTED` before any geometry is built.

## The domain boundary

What refuses now does so by design, not by omission:

- **Out-of-matrix pairs** — an oblique plane through a cylinder, any torus
  pair off its axis — are curves the line/circle vocabulary cannot name.
  Widening this means widening the curve vocabulary (ellipses first), which
  is future work with its own ADR.
- **Tangential and coincident contact** is outside the regularized
  transverse domain and fails closed everywhere.
- **Blended operands** (torus, cone, sphere faces) participate in Booleans
  only through the co-directional prism reductions today; the general
  engine's sewing vocabulary carries planes and cylinders. In-matrix
  configurations beyond that — a coaxial torus pair across two solids —
  still answer `BOOLEAN_ANALYTIC_RECONSTRUCTION_PENDING`, and extending the
  engine's face vocabulary is the one named follow-on.

## Consequences

The intersection graph is reusable well beyond Booleans: imprint-only
operations, section curves, and the eventual offset and shell milestones all
need the same answers. Publishing it as a matrix rather than a best-effort
routine means every caller inherits the same honest domain boundary.

The cost is that the matrix is narrow, and widening it is not free — each new
pair is its own derivation and its own gate. Adding ellipses to the curve
vocabulary would open a large part of the empty half of the table at once, and
is the natural next question once reconstruction works.

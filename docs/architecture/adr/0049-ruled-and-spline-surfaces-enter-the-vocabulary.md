# ADR 0049: Ruled and spline surfaces enter the vocabulary

Status: accepted — K-A, the ruled surface and a loft between two planar
sections, is built next; K-B and K-C are the plan.

- Date: 2026-09-22
- Decision owners: Artificer project
- Supersedes: the "Splines / NURBS anywhere" row of
  [0026](0026-second-expansion-programme.md)'s *What this programme
  deliberately does not do*, and the sentence in its F2 that puts spline
  sketch entities out of reach
- Extends: [0025](0025-analytic-surface-intersections.md),
  [0045](0045-a-boolean-that-resolves-what-it-touches.md),
  [0047](0047-the-curve-two-cylinders-share.md)

## Context

ADR 0026 made analytic exactness the kernel's bet and wrote its boundary
into a table: splines and NURBS were "permanently outside an analytic-exact
kernel", on the reasoning that certified analytic CAD covers real machined
parts. F2 said the same of sketches: "Not doing, and saying so in the UI:
ellipse and spline sketch entities."

The bet has paid where it was placed. Planes, cylinders, cones, spheres and
tori, and the curves where they meet, carry every prismatic and turned part
the product makes, and every face it publishes is exact or labelled as an
approximation. The boundary is now what users meet first:

- **A loft between unlike sections.** A square to a circle is the loft
  every tutorial starts with. Each wall pairs a straight edge with an arc,
  and no wall is a plane, a cylinder or a cone. The loft ladder has had one
  rung — a section and its own offset, whose walls are planes and cones —
  and nothing past it.
- **A smooth loft through several sections**, which needs a surface that is
  continuous in its tangent across the sections. That surface is a B-spline.
- **Spline sketch input.** The protocol already carries
  `PlanarCurve2::Bspline`, and every consumer refuses it.
- **Surfaces fitted to scans.** The scan add-on segments a mesh into planes,
  cylinders, cones and spheres and leaves the rest as free-form regions
  with nothing to become.

Staying elementary-only leaves the product behind every mainstream system on
the first two and closes the door on the last two. The project owner has
decided to overturn the row.

## Decision

### The standard stays; the vocabulary grows

What made the elementary surfaces admissible was never that they are
elementary. It was that every claim the kernel makes about a face — that an
edge lies on it, which way it faces, its area, the volume it bounds — is
checked against an exact definition of the carrier within the model
tolerance, and that anything the kernel cannot check is refused by name or
labelled as an approximation. A carrier enters the vocabulary when it can be
held to that, and not before. The carriers this record admits are the ruled
surface now, and the B-spline curve and surface in the stages below.

### The ruled surface

A ruled surface is spanned between two exact rails:

```text
S(u, v) = (1 − v)·C₀(u) + v·C₁(u),   u, v ∈ [0, 1]
```

Each rail is a line, a circular arc or an elliptical arc — the curves the
edge vocabulary already holds — with its parameter range, and `u` is mapped
linearly onto each rail's range. On an arc that pairs points in proportion
to arc length, which is what a person lofting a square to a circle expects:
the middle of a side goes to the middle of its quarter circle. `Surface`
stays `Copy`; a rail is a small value, as a cylinder is.

What certification means for it:

- **Evaluation is exact.** A point of a ruled surface between two exact
  rails is a convex combination of two exact points, as exact as a point of
  a cylinder. So are both partial derivatives, and the normal
  `∂S/∂u × ∂S/∂v`, which follows the orientation convention every other
  carrier keeps: the builder orders the rails so it points out of the
  material, and a mirror reverses `u` to keep it there.
- **Inversion is numerical and bounded.** Finding `(u, v)` for a point has no
  closed form. It is Newton's method from a seed — the point's own
  parameters where the caller knows them, a coarse search where it does not
  — with a fixed iteration bound, and it refuses rather than returns a
  point it did not converge to.
- **The validator checks by inversion.** Every edge and vertex of a ruled
  face is inverted onto the surface and must lie within the model tolerance
  of it. A rail is also checked by identity: the edge's curve must be the
  rail's curve, frame, radius and parameter map, so an arc edge cannot
  stand in for a different arc through the same end points. A rung must be
  an iso-`u` line. The normal must not vanish on a grid over the face; a
  wall that pinches to a point is degenerate, and the loft refuses it before
  it is built.
- **Measures are quadrature, under ADR 0026's policy.** Area, volume and
  centroid come from the divergence theorem, as for every other carrier,
  with the face integrals evaluated over the parameter rectangle by
  composite Gauss–Legendre. ADR 0026 made this normative for the elliptic
  integral an ellipse's arc length needs: an integral so evaluated "counts
  as a closed form, exactly as `cos` does — it evaluates a transcendental
  exactly; it does not approximate the geometry", and ADR 0047 applied it
  to the trace. The same standing applies here. Along `v` the volume and
  centroid integrands are polynomials of degree at most three, which the
  rule integrates exactly; the area's integrand is the length of a normal
  linear in `v`, and every integrand is analytic along `u`, where the
  rule's convergence is exponential. The order is fixed, so the result is
  deterministic.
- **Intersections are numerical and bounded, and K-A has none.** The two
  exact Boolean engines decline a ruled face by name. The faceted tier
  answers, labelled as the approximation it is, exactly as it does today
  for the pairs the engines cannot carry.
- **STEP.** AP214 has no entity for a surface ruled between two arbitrary
  curves. Where both rails are lines the surface is a bilinear patch, which
  is exactly a B-spline surface of degree one by one, and is written as
  one. Otherwise it is written as a B-spline surface of degree three in `u`
  and one in `v`, whose two rows of control points are C¹ cubic fits of the
  two rails on one knot vector. Because the surface is linear in `v`, its
  distance from the ruled surface is at most the worse of the two rail
  fits, and each fit is held within `1e-7` mm, a tenth of the file's
  declared accuracy — the precedent ADR 0047 set for the trace. The rails
  themselves are named exactly: they are the face's own bounding edges,
  written as lines, circles and ellipses. As with the trace, the spline
  exists only in the file.

### A wall that is a plane, a cylinder or a cone is built as one

A ruled carrier is used only where no elementary carrier is exact. Two line
rails that lie in one plane span a plane. Two coaxial arcs on parallel
planes that cover the same angles about their axis span a cone, or a
cylinder when their radii agree — the same recognition the offset loft
already makes. A frustum lofts to planes, a coaxial circle-to-circle loft to
cones, and both stay inside the exact Boolean engines.

### K-A: a loft between two planar sections

`KernelCommand::LoftPlanarSections` takes two sections, each a planar
frame and a profile, and an operation: a new body, or an add or a cut
against the body it is given. `.art` scripts reach it as `loft(sections:
[s1, s2], operation: …)`, where each section is a sketch on a plane of its
own.

- **Sections.** Exactly two, on any two planes that are not the same plane:
  parallel, offset, tilted or not parallel at all. Each section is one
  region with an outer loop of lines, arcs and full circles; holes are
  allowed when both sections have the same number of them, matched by
  nearest centroid. A section may not reach through the other section's
  plane.
- **Direction.** The loft runs from section 0 toward section 1. Both loops
  are oriented about that direction, and a loop that disagrees is reversed.
- **Correspondence.** With equal segment counts the segments pair in order,
  from the cyclic start offset that minimises the summed squared rung
  length. With unequal counts each loop is split at the normalised
  arc-length positions of the other loop's vertices — the denser loop's
  first vertex held fixed, the sparser loop started from whichever of its
  own vertices makes the rungs shortest — so every wall pairs one segment
  with one segment. Splits are
  exact: an arc is split at an exact angle and a line at an exact point. A
  full circle is first split at the point nearest the other loop's first
  vertex. Two full circles have no vertex to be nearest to; they are split
  at one direction from their centres, so their rungs pair points at equal
  angles and no wall twists — cut at the nearest points instead, two
  circles offset by more than their radius would pair opposite points and
  pinch the wall to a waist.
- **Walls.** One face per segment pair, its carrier chosen as above. The
  edges between walls are straight rungs; the caps are the two section
  faces.
- **Refusals.** Rungs or walls that cross (`LOFT_RUNGS_CROSS`), a wall that
  pinches (`LOFT_WALL_DEGENERATE`), sections on one plane
  (`LOFT_SECTIONS_COPLANAR`), a section reaching through the other's plane
  (`LOFT_SECTION_CROSSES_PLANE`), holes that do not pair
  (`LOFT_HOLE_COUNT_MISMATCH`), more than two sections
  (`LOFT_MULTI_SECTION_UNSUPPORTED`), a B-spline in a section
  (`LOFT_SECTION_SPLINE_UNSUPPORTED`) and a body that fails the solid
  validator are all refused by name, and nothing invalid is published.
- **Add and cut** combine the loft with the body through the existing
  Boolean ladder: the prism reduction, then the analytic engine where it
  can carry both operands, then the faceted tier with its approximation
  label. The report names the rung that answered.

### K-B and K-C: the plan

**K-B — B-spline curves and surfaces.** B-spline curves enter the edge and
sketch vocabularies; spline sketch entities are drawn and extruded, and an
extruded spline profile sweeps a B-spline wall. B-spline surfaces carry the
smooth loft through several sections.

A B-spline owns a knot vector and a control net of unbounded size, which a
`Copy` enum cannot hold by value. The recommendation is an immutable,
content-interned store: a spline is hashed by its content and stored once,
and `Surface` and `Curve3` carry a `Copy` handle to it — a `&'static`
reference or an index into the store. Everything that matches on a carrier
keeps compiling as it is, a digest hashes the content rather than the
handle, and two snapshots that hold the same spline share it. The costs are
real. Interned data lives for the process: a long interactive session that
explores many shapes keeps every spline it ever made, bounded by the number
of distinct splines rather than by what is in use. The store is global and
needs a lock, or a concurrent map, on insertion. And a handle's numeric value
depends on insertion order, so nothing may ever hash, sort or compare
handles where a result is expected to be deterministic. The alternative —
an `Arc` in the carrier, giving up `Copy` — frees memory when the last
snapshot lets go, but touches every match on `Surface` and `Curve3` and puts
a reference count on every copy in the hottest loops the kernel has.

**K-C — Booleans past the faceted tier, and fitted surfaces.** The exact
engine learns the new carriers: surface–surface intersection by marching,
with bounded error, closing sections the way ADR 0047's closure does for
the trace. The scan add-on fits B-spline surfaces to the regions it
currently leaves free-form.

Only K-A is built by this record. K-B and K-C each get their own record when
they are built, and may revise the plan above.

## Consequences

`Surface::Ruled` is a first-class carrier. Evaluation, normals, inversion,
tessellation, the validator, similarity transforms and mirrors, the digest,
the measures, the report's face descriptions, the display silhouettes and
STEP export all carry it. The report's surface counts gain `ruled`.

The two exact Boolean engines decline a ruled face by name, as they decline
any pair outside the intersection matrix. A Boolean with a ruled face in
either operand — a loft added or cut, or a later cut that runs into a
lofted body's ruled wall — is answered by the faceted tier with its label.
A loft whose walls all came out as planes, cylinders or cones is combined
exactly (`loft/boolean-prism`, `loft/boolean-analytic`); one with a ruled
wall reports `loft/faceted` with `LOFT_FACETED_APPROXIMATION` and the
reason, `LOFT_EXACT_ROUTE_DECLINED`.

Presentation compares the normals of two walls along the whole of the rung
between them, not at one point. A rung where two ruled walls meet at a
crease at one end and tangentially at the other is a crease.

What still refuses, by name: a loft through more than two sections, which
needs B-spline surfaces and arrives with them; a section with a B-spline
curve; sections on one plane; a section through the other's plane;
sections with different numbers of holes; walls that cross or pinch.

The drafted extrusion stays on its own rung (`loft/offset-section`). Its
walls are planes and cones by construction, and a section that is the
profile's own offset needs no correspondence.

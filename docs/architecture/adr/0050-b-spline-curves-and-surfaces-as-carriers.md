# ADR 0050: B-spline curves and surfaces as carriers

Status: accepted and implemented — stage K-B of
[0049](0049-ruled-and-spline-surfaces-enter-the-vocabulary.md): B-spline
curves and surfaces are carriers of the kernel, spline profiles extrude,
lofts take splines in their sections, and a loft through three sections or
more is smooth.

- Date: 2026-09-23
- Decision owners: Artificer project
- Extends: [0049](0049-ruled-and-spline-surfaces-enter-the-vocabulary.md)
  (this is its K-B), [0026](0026-second-expansion-programme.md) (the
  standing of fixed-order quadrature as a closed form),
  [0047](0047-the-curve-two-cylinders-share.md) (a spline written to STEP)

## Context

ADR 0049 admitted the ruled surface and built the loft between two planar
sections (K-A). It left three things refused by name, each waiting on the
same carrier: a loft through more than two sections
(`LOFT_MULTI_SECTION_UNSUPPORTED`), a section drawn with a spline
(`LOFT_SECTION_SPLINE_UNSUPPORTED`), and every other use of the protocol's
`PlanarCurve2::Bspline`, which the sketcher could draw and no kernel
command would take. It planned K-B as B-spline curves and surfaces held in
an immutable, content-interned store, and said K-B would get its own record
and might revise the plan. This is that record. The plan stood; what
follows is what was built and the few places it had to decide something
the plan left open.

## Decision

### The carrier: a clamped, non-rational B-spline

A B-spline curve or surface in the kernel is non-rational and clamped:

- **Degree one to five.** Every measure is a fixed ten-point
  Gauss–Legendre rule on each knot span (below), which is exact for
  polynomials up to degree nineteen. The highest-order integrand a measure
  needs on a span — the first moment of a surface, or the polar moment of a
  contour — is of degree `4p − 1` in each parameter, which the rule
  integrates exactly while `p ≤ 5`. A higher degree is refused
  (`BSPLINE_DEGREE_UNSUPPORTED`) rather than measured less exactly.
- **Clamped knots.** Each end of the knot vector repeats `degree + 1`
  times, so the curve starts and ends on its end control points. Interior
  knots lie strictly inside the domain and repeat at most `degree` times,
  so the curve never comes apart at one. An unclamped vector is refused
  (`BSPLINE_UNCLAMPED_UNSUPPORTED`): it could be clamped by knot insertion,
  but that would quietly change the curve's domain, and every tool that
  draws a spline in this product draws it clamped. A vector of the wrong
  length, falling anywhere, or with an empty domain is refused
  (`BSPLINE_KNOTS_INVALID`).
- **No weights.** Weights that are all equal describe the non-rational
  curve and are accepted as it. Weights that differ are refused
  (`BSPLINE_RATIONAL_UNSUPPORTED`): a rational spline's measure integrands
  are rational functions, which the fixed rule does not integrate exactly,
  and the conics a rational spline is usually wanted for are carriers of
  their own already.

A coordinate or a knot that is not finite is refused as any non-finite
profile is.

Evaluation and its first and second derivatives are the Cox–de Boor
recurrence (Piegl and Tiller's algorithm A2.3), exact to the arithmetic. A
point at either end of a clamped domain is the end control point to the
bit, and a surface's boundary is evaluated from that row or column of its
net alone, exactly as the edge along it evaluates its own curve, so an edge
and the surface it bounds agree on every point they share.

### Where a spline lives: the interned store

The store ADR 0049 recommended is the one built. A spline's content — its
degree, knots and control points — is validated, made canonical (a
negative zero becomes a zero), and stored once for the life of the process
behind a `&'static` reference; `Curve3::Bspline`, `Curve2::Bspline` and
`Surface::Bspline` carry that reference, so the carrier enums stay `Copy`
and nothing that matches on them changed shape. The store is one map per
kind of spline from a fingerprint of the content to the values stored
under it, behind a mutex. The fingerprint only finds the bucket: a value
is compared with each value in its bucket in full, and stored only when
none is equal, so two splines with one content always share one reference.

The rules that keep results deterministic:

- **Handles are compared only for equality**, which is content equality,
  because equal content is always interned to one reference. Nothing
  orders a handle, and nothing hashes one.
- **The digest hashes content.** A B-spline edge, pcurve and surface each
  contribute their degree, knots and control points under their own tag,
  never an address. A test builds two bodies that share spline content in
  one order in a fresh process and in the other order in another, and
  their digests agree.
- **Debug output prints the spline's shape**, not its address, so two runs
  print the same text.

The costs are the ones ADR 0049 named, and they are accepted. Every
distinct spline a process makes stays until it exits: memory grows with the
number of distinct splines a session has explored, not with what its
snapshots hold. Insertion takes a lock; evaluation reads through the
reference and takes none. The alternative, an `Arc` in the carrier, would
have given up `Copy` and put a reference count on every copy of a carrier
in the kernel's hottest loops.

### What certification means for it

- **Inversion** is Newton's method on the two parameters from a seed — the
  point's own parameters where the caller knows them, otherwise the best of
  a coarse search over every span cell — with a fixed iteration limit, and
  it refuses rather than returns a point it did not converge to.
- **The validator** holds a B-spline face to the standard every other face
  meets. Its parameter rectangle lies in the surface's domain, and its
  normal vanishes nowhere on a sampling of every span cell; a wall that
  pinches or folds flat has no side to face. An edge of a B-spline face
  must lie along an iso-parameter line of it, since those are the curves
  the surface carries exactly. The isocurve there is read off the net
  exactly, and the edge is compared with it by control points: a B-spline
  edge must have its degree, its knots under the affine map between the
  two parameters and its control points; a straight edge must have the
  isocurve's control points on the line at the isocurve's Greville
  abscissae, which is exactly when the isocurve is that line walked at an
  even rate. Because the basis is a partition of unity, two splines whose
  control points agree within a tolerance agree everywhere within it, so
  this is a proof and not a sampling. Every edge sample is also inverted
  onto the surface, as a ruled face's are. A planar face bounded by a
  spline carries the spline's control points in the plane as its pcurve,
  and that pcurve must be the edge carried into the plane.
- **Measures** are the divergence theorem, as for every carrier. A B-spline
  face is integrated over its parameter rectangle by the ten-point rule on
  every span cell; the volume and centroid integrands are polynomials the
  rule integrates exactly, and the area's is the length of a polynomial
  normal, analytic on each cell, where the rule converges exponentially —
  the standing ADR 0026 gave an ellipse's arc length and ADR 0049 gave the
  ruled surface. A planar face bounded by a spline has its area, first
  moments and polar moment from contour integrals that are polynomials on
  each span, so they are exact.
- **Bounds.** A B-spline edge contributes its ends and the points where a
  coordinate's rate changes sign, bracketed on each span and bisected. A
  B-spline face contributes a fine sampling of every span cell, since a
  smooth loft can bulge past every edge it has.
- **Similarity transforms and mirrors** map the control points: a B-spline
  is carried to a B-spline exactly by any affine map. A mirror also
  reverses the surface's `u` parameter, so its normal still points out of
  the material, and reverses the pcurves with it.
- **Tessellation** cuts every knot span into a power of two of equal steps,
  as many as the span's curvature bound asks for within the chord budget,
  and a face's grid takes in the samples of its boundary edges, so where a
  face and an edge meet their samples meet to the bit. The report's chord
  deviation covers B-spline edges and faces.
- **Silhouettes.** The viewport draws where a B-spline wall turns from the
  viewer as the marching-squares contour of `n · view` on a grid over every
  span cell. This is presentation (ADR 0026, rule 3), and says so.
- **Reports.** A B-spline face is described as `bspline` with its degree in
  each parameter and the size of its net; a B-spline edge as `bspline` with
  its ends, degree and number of control points. The body's surface counts
  gain `bspline`. The published schema carries all three.
- **STEP.** A B-spline curve is written as `B_SPLINE_CURVE_WITH_KNOTS` and a
  B-spline surface as `B_SPLINE_SURFACE_WITH_KNOTS`, as the kernel holds
  them: degree, control points and knots, nothing fitted. The surface's
  natural normal `∂S/∂u × ∂S/∂v` is the kernel's, which the builders point
  out of the material, so every face is written with `same_sense` true.

The exact Boolean engines do not carry B-spline faces. Both decline them by
name, as they decline a ruled face, and the faceted tier answers with its
label. Intersecting B-spline surfaces exactly is K-C.

### Spline profiles

A planar profile may now mix splines with lines, arcs and circles, and a
loop may be one spline that closes on itself. A closed spline is cut in two
at the middle of its domain, as a whole circle is cut into semicircles, so
every loop has two vertices and every wall two rungs.

A spline has no closed-form intersection with anything, so a profile with
one is certified by subdivision. Every piece is Bézier segments, arcs of at
most a quarter turn, or chords, each inside a box and within a known
distance of its own chord, all from the convex hull property and nothing
sampled. A pair of pieces is split until either their boxes are further
apart than the threshold — the feature floor for pieces that are not
neighbours, the linear agreement for neighbours, which may meet only at
their shared vertex — or both are flat enough that the distance between
their chords, less and plus their flatness, falls wholly on one side of the
threshold. A pair that has not resolved within a fixed budget is refused
(`BSPLINE_CLEARANCE_INDETERMINATE`) rather than guessed. A spline that
stalls — its rate vanishing at a cusp, or at an end whose first two control
points coincide — would sweep a wall with no side to face there and is
refused (`BSPLINE_CURVE_DEGENERATE`); so is one no longer than the feature
floor. A spline that touches itself or the rest of its loop is refused as
any self-intersecting profile is.

- **A new body** (`ExtrudePlanarProfile`, rung `extrusion/spline-profile`).
  Each spline sweeps a B-spline wall of degree `p` by one, its two rows the
  spline's control points on the bottom and top planes; lines and arcs
  sweep planes and cylinders as they always have. The caps are planes
  bounded by the splines themselves. A straight loft with no draft
  (`LoftPlanarProfileOffset` with a zero offset) is the same body, on its
  own rung, `loft/straight`.
- **A draft** offsets the section, and the offset of a spline is not a
  spline of any degree, so a drafted loft of a profile with a spline has no
  exact carrier for its far section and is refused
  (`LOFT_OFFSET_SPLINE_UNSUPPORTED`).
- **Add and cut on a face** (`ExtrudeFacePlanarProfile`). The spline profile
  is extruded as a tool body from the face — a cut starts inside the body
  by the depth and overshoots the face — and combined through the same
  Boolean ladder a loft uses, named as a face feature: the exact prism
  reduction and the analytic engine decline the B-spline walls
  (`FACE_FEATURE_EXACT_ROUTE_DECLINED`), and the faceted tier answers
  (`face-feature/faceted`, `FACE_FEATURE_FACETED_APPROXIMATION`).
- **Other commands** that take a profile and have no B-spline wall to build
  refuse a spline by name (`PLANAR_PROFILE_SPLINE_UNSUPPORTED`): a revolved
  spline would need a surface of revolution with a spline generatrix, which
  this record does not admit.

### A two-section loft with a spline

The two-section loft keeps every rule of ADR 0049 — its correspondence, its
walls, its refusals — and takes spline pieces in either section. Splines
are split where the other loop has vertices, at the same fraction of the
way along by length, as arcs and lines are; the split itself is exact
(knot insertion). A wall between two pieces at least one of which is a
spline is a B-spline surface of degree `p` by one, ruled between its two
rows: each row is its piece as a B-spline over the unit interval — a spline
reparameterised, a line as itself — raised to one degree and refined to one
knot vector with the other row, so the rulings pair equal parameters. An
arc that pairs with a spline is carried by its cubic fit: equal pieces of
at most a quarter turn, each the Bézier cubic with the arc's end points and
end tangents, as many as it takes for the sampled radial error to be within
half the model's linear agreement (half a picometre by default). Two lines,
two arcs, or a line and an arc still make planes, cones, cylinders and
ruled walls exactly as before; a two-section loft without a spline is the
same body it was.

### A smooth loft through three sections or more

`LoftPlanarSections` with three sections or more builds a smooth loft (rung
`loft/skinned`). The sections obey what two do — each one region, holes
matched by nearest centroid from each section to the next, neighbouring
sections on distinct planes, each beyond its neighbours' — and the loft
runs from the first to the last through the others in order, each section
turned to face along the loft there.

- **Correspondence.** The rules of ADR 0049, applied along the chain. The
  first section with vertices of its own keeps its start; each section
  after it is turned to start at whichever of its vertices makes it lie
  closest to the section before, by the summed squared distance, both
  ways, between each loop's vertices and the other loop's point at the same
  fraction of its length; a whole circle is cut where its neighbour starts.
  Every loop is then cut exactly at every fraction of its length at which
  any loop has a vertex — a fraction closer to one of its own than sixteen
  feature floors along the shortest loop counting as that one — so all
  loops have one piece for every vertex any has.
- **Walls.** The pieces in one place along the loops make one column, and
  each column is one face: a B-spline surface through the column's pieces,
  each piece a row of its net as in the two-section case. Along the loft
  every column of the net is interpolated through the rows' control points
  at the sections' chord-length parameters — the distances between the
  outer loops' centroids — with degree `min(3, n − 1)` on knots averaged
  from those parameters: a single quadratic through three sections, a
  single cubic through four, and a cubic with interior knots, C² at them,
  through more. So each wall passes through every section and has one
  continuous tangent plane across the middle ones; there is no crease at a
  section. A wall's first column, the rung it shares with the wall before
  it, is interpolated through the same section vertices as that wall's
  last, so the two walls meet on one curve. The first and last sections are
  the caps.
- **What is checked, and refused by name.** A wall whose normal vanishes
  anywhere on a sampling of its span cells is refused
  (`LOFT_WALL_DEGENERATE`). A curved loft can do one thing a ruled one
  cannot: bend back on itself between two sections, when the sections are
  spaced so unevenly along the loft that the curve through them overshoots.
  The rate of every wall along the loft must keep a positive part along the
  loft's direction there, on a sampling of every span and of every interval
  between sections, and a loft that turns back is refused
  (`LOFT_SKIN_FOLDS`). Walls that cross, seen as the loops they trace at
  sampled heights along the loft, and rungs that come within the feature
  floor of one another, are refused (`LOFT_RUNGS_CROSS`), as are loops that
  cannot be brought to one correspondence. The body must then pass the
  solid validator, whose checks above are proofs. The crossing and folding
  checks sample, as the two-section loft's wall checks do; this record
  claims no more for them than ADR 0049 claimed for those.
- **Add and cut** combine the smooth loft with the body through the
  Boolean ladder, which answers on the faceted tier with the loft's label
  (`loft/faceted`, `LOFT_FACETED_APPROXIMATION`).

A two-section loft is not a smooth loft of two sections: it stays ruled,
because a curve through two sections is a straight line and ADR 0049's
walls are already exact.

### Scripting

`.art` sketches take `spline(points: [[x, y], …], closed: false)`, the curve
through the points — of degree `min(3, n − 1)` at their chord-length
parameters when open, and when closed the clamped cubic through them and
back to the first with one tangent either side of the seam, the curves the
sketch crate's fit-point tool draws — and `spline(control_points: […],
degree: 3, closed: false)`, a clamped spline on a uniform knot vector.
`loft(sections: [a, b, c, …])` takes any number of sections from two. A
journal with splines decompiles to a script that compiles back to the same
commands. `crates/kernel/examples/spline_vase.art` is a smooth loft through
four sections, one of them a closed spline, and a spline pocket.

### Refusals and warnings this record adds

| Code | When |
|---|---|
| `BSPLINE_DEGREE_UNSUPPORTED` | A spline of degree zero or above five. |
| `BSPLINE_RATIONAL_UNSUPPORTED` | Weights that are not all equal. |
| `BSPLINE_UNCLAMPED_UNSUPPORTED` | A knot vector whose ends do not repeat `degree + 1` times. |
| `BSPLINE_KNOTS_INVALID` | A knot vector of the wrong length, falling, with an interior knot outside the domain or repeated more than the degree, or an empty domain. |
| `BSPLINE_CURVE_DEGENERATE` | A spline in a profile that stalls: its rate vanishes at a cusp or at an end. |
| `BSPLINE_CLEARANCE_INDETERMINATE` | Two pieces of a profile the subdivision could not prove clear or in contact within its budget. |
| `PLANAR_PROFILE_SPLINE_UNSUPPORTED` | A spline in a profile for a command with no B-spline wall to build, such as a revolve. |
| `LOFT_OFFSET_SPLINE_UNSUPPORTED` | A drafted loft or extrusion of a profile with a spline. |
| `LOFT_SKIN_FOLDS` | A smooth loft whose walls would turn back between two sections. |

`LOFT_MULTI_SECTION_UNSUPPORTED` and `LOFT_SECTION_SPLINE_UNSUPPORTED` are
retired: what they refused is built. A face feature with a spline warns as
every faceted face feature does (`FACE_FEATURE_FACETED_APPROXIMATION`, with
the reason `FACE_FEATURE_EXACT_ROUTE_DECLINED`), and a loft as every
faceted loft does.

## Consequences

B-spline curves and surfaces are first-class carriers. Evaluation,
inversion, the validator, measures, bounds, transforms and mirrors,
tessellation, the digest, reports, silhouettes and STEP all carry them.
The public surface grew without changing shape elsewhere: the kernel's
`DisplaySurface` gains a `Bspline` variant holding an opaque
`DisplaySpline` with `DisplaySurface::spline_silhouette`, its face and edge
descriptions gain `bspline` variants and its surface counts a `bspline`
field, and the scripting API gains the two `spline` forms. The protocol's
types are unchanged; what `LoftPlanarSections` and `PlanarCurve2::Bspline`
now build is documented on them.

What still refuses, by name: rational and unclamped splines, degrees above
five, a revolved spline, a drafted spline, and every loft the rules above
refuse. What is still answered on the faceted tier, and labelled: every
Boolean with a B-spline face in either operand. What is deferred: exact
Booleans with B-spline faces and fitted scan surfaces (K-C), rational
splines, a spline swept along a path, and a smooth loft's continuity
chosen by the user rather than fixed at what interpolation gives.

The interned store keeps every distinct spline a process has made. A
long-lived session that explores many spline shapes will hold them all; if
that becomes a measured problem the store can be made collectable without
changing the carriers, because nothing outside it depends on a handle's
value.

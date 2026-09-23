# ADR 0047: The curve two cylinders share

Status: implemented — the curve is exact, carried through the vocabulary,
produced by the intersection matrix, closed into face boundaries by the exact
Boolean, mirrored and moved with the body, and written to STEP.

- Date: 2026-09-19
- Decision owners: Artificer project
- Extends: [0025](0025-analytic-surface-intersections.md),
  [0026](0026-second-expansion-programme.md),
  [0045](0045-a-boolean-that-resolves-what-it-touches.md)

## Context

ADR 0025 drew the intersection matrix around pairs whose curve is a line, a
circle or an ellipse. Two cylinders qualify when they are coaxial, parallel,
or of equal radius on crossing axes; everything else was refused by name, in
a comment that called it "a genuine space quartic".

That refusal is the one a user meets most often once a part has more than one
round feature: a bore crossing a bore of another diameter, a slot cut from a
sloped face across a bore, a boss meeting a fillet. The faceted tier answered
those, labelled as approximations, and the exact engine stood aside.

## Decision

### The quartic is a closed form

Write a point of cylinder `A` in `A`'s own parameters,
`P(x, y) = origin + r·radial(x) + y·axis`, and substitute it into `B`'s
implicit equation `|w|² − (w·e)² = r_B²`. Because `radial ⟂ axis`, the `y`
terms collect into a **quadratic**:

```text
a·y² + b(x)·y + c(x) = 0,   a = |n|² − (n·e)²
b(x) = b₀ + b₁cos x + b₂sin x
c(x) = c₀ + c₁cos x + c₂sin x + c₃cos 2x + c₄sin 2x
```

so the curve is `y(x) = (−b(x) ± √D(x)) / 2a`, with `D = b² − 4ac` a
second-order trigonometric polynomial. Two branches, meeting where `D`
vanishes, each exact to the last bit the arithmetic carries. It is a quartic
in space and a graph in the host's parameters, which is what makes it
tractable.

A pair whose `D` never rises above zero does not meet at all: that is
`Empty`, not a limit of the vocabulary, and saying so restored every pair the
matrix used to call unsupported for want of a curve that was never there.

### One parameter, both faces

A curve is shared by the two faces it separates, and the sewer welds their
two uses of it by comparing the midpoints they each compute. If the two faces
walked the curve by their own azimuths, those midpoints would be different
points of the same curve and the shells would come apart, non-manifold. So
the trace carries a **canonical host**, chosen from the pair by the same
axis ordering the equal-radius case already uses, and both faces read it over
that one parameter — the other face mapping each point into its own
coordinates. `intersect(a, b)` and `intersect(b, a)` therefore name one curve
with one parameterization.

### Where the azimuth stops being a parameter

`y(x)` has a vertical tangent at a branch point: the two branches meet and
the graph turns back, the way a semicircle does over its diameter. The curve
is smooth there; only this reading of it is not. Pieces are cut at the branch
points, and the two branches are given the **double root** `−b/2a` as their
shared endpoint rather than `±√D` evaluated from either side — those differ
by a few ulps, which is enough to leave a loop open by a millionth and fail
the weld.

### What is exact, and what quadrature means here

Evaluation, the derivative, the branch points and the implicit form are all
closed forms. Arc length and the area a trace bounds are not — `√(trig
polynomial)` has no elementary antiderivative — and are integrated by
composite Gauss–Legendre over spans the branch points split, where the
integrand is analytic and convergence is exponential.

ADR 0026 already made this policy normative for the elliptic integrals an
ellipse's arc length needs: such an integral "counts as a closed form,
exactly as `cos` does — it evaluates a transcendental exactly; it does not
approximate the geometry." The same standing applies here, and for the same
reason.

## Consequences

The intersection matrix's cylinder row no longer has a hole in it. The curve
is a first-class member of the vocabulary: `Curve3::Trace`, `Curve2::Trace`
and `Segment::Trace` carry it through tessellation, transforms, measures, the
digest and the validator, whose locus proof for the pair is the sampled and
tangent comparison — complete, because both descriptions run over the same
parameter, and a quartic has no frame to compare.

### Closing the section on a cylinder

A plane section always runs a cylinder face's whole period, so the older
closure could chain it from seam to seam. A trace need not: on a half-cylinder
face it can enter and leave by the same seam (a bite out of the face's edge),
make a closed lens between two branch points, or wind round as a ring. So the
section on a cylindrical face is closed the way a planar arrangement is:

- Every 2D stage reads a trace as a graph over its **own face's** azimuth.
  The sewer converts to the canonical host at the end (`read_on`), so each
  face's own arithmetic stays well conditioned.
- Both cylinders' branch points are **landmarks**. Every piece is cut at all
  of them, on both faces, so the two uses of an edge start and end at the
  same places.
- The pieces are laid on a window a turn wider than the face, lifted by whole
  turns and clipped to the face's azimuths. The face's own seam generators
  are offered at the neighbouring turns too. Crossings are cut on both pieces
  (`split_at_mutual_crossings`), and a weld keeps lines on their own
  abscissa or ordinate (`weld_aligned`).
- The arrangement's half-edge cycles are classified by the material on their
  left, which is how a lens, a bite and a ring all close without a separate
  rule for each.

A section still refuses in two cases, and says why. One is a curve that ends
inside the face (`BOOLEAN_TRACE_NOT_CLOSED`). The other is a tangency inside
the face, such as two cylinders that kiss (`Contact`). Anything the exact
route builds must also pass the solid validator before the ladder accepts it.
A candidate that fails is declined as `Invalid` and handed to the faceted
tier with its label, so an unsound exact body is never published.

The sewer also merges a vertex that no other face uses, joining the
same-carrier pieces either side of it. A face split where its neighbour is
not would otherwise leave the neighbour's edge used once. A trace is never
joined this way: its landmarks are where both faces cut it.

### Conditioning at the branch points

A branch point is a root of `D`, and the height there is a square root of
something that should be zero. So any error in `D`'s coefficients turns into
the square root of that error in height. Three things keep that error small
enough for the weld and the validator:

- The coefficients are built from the **feet of the axes' common
  perpendicular**, not from the cylinders' own origins. A cut's tool keeps
  its origin at the far end of its sweep, perhaps a thousand away. Written
  from there, the constant terms are a million that cancel to a hundred,
  which leaves the branch points picoradians adrift and the heights a
  hundred-thousandth out after a mirror. From the feet, every term is the
  size of the axes' distance and the radii. The host's height is moved back
  by the foot's own offset.
- A discriminant within a millionth of a millionth of its terms counts as
  zero. Every float that is the branch point then evaluates to the double
  root, and the double root and the clamped height share one `mul_add`, so
  they agree to the bit.
- A stretch read onto the other cylinder snaps an end to the branch point it
  sits on in space before reading the azimuth off it.

Arc length and the area a trace bounds are integrated through
`x = a + (b − a)(3t² − 2t³)`, whose rate vanishes at both ends. The square-
root cusp at a branch-point end becomes smooth in `t`, and the quadrature
converges exponentially right up to the branch point.

### Mirrors and moves

A similarity moves both cylinders a trace is written on, and the trace is
re-derived from them. A mirror also turns every face's azimuth round. On the
other cylinder's face the curve is still walked over the reflected host's
azimuth, and only the window it lies near is negated. On the host's own face
the curve is re-read as the same root over the reversed record, walked from
`−end` to `−start`. If that record is not the reflected host read backwards,
the mirror refuses by name instead of guessing a parameterisation.

### STEP

STEP has no entity for this quartic. It does have `intersection_curve`,
which says that the edge is where two named surfaces meet and carries a 3D
curve beside them. The exporter writes that entity. It names the two
cylinders and gives a cubic B-spline as the 3D curve, fitted to within
`1e-7` mm, a tenth of the confusion accuracy the file declares.

The spline is a C¹ chain of Hermite cubics through the curve's own points and
rates. Each piece is halved until it is within the tolerance at its quarter
points. Towards a branch-point end the curve is walked by a `t` whose azimuth
moves like `t²`, the same substitution the quadrature uses, so the spline
interpolates an analytic function all the way to the branch point.

What the file states exactly is the pair of surfaces. The spline is the
representation STEP asks for beside them, and it is within the file's own
accuracy of the curve. That is also how other kernels write intersection
curves they hold procedurally. The spline exists only in the file. The
model never holds one, so ADR 0026's rule that the kernel carries no splines
still stands.

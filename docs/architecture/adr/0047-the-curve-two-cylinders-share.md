# ADR 0047: The curve two cylinders share

Status: partly implemented — the curve is exact, carried through the
vocabulary, and produced by the intersection matrix; the section it leaves on
a face is not yet closed into a boundary.

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

### What is not done

The section a trace leaves on a face is not yet assembled into a boundary in
every configuration. On a half-cylinder face a trace can enter and leave by
the same seam — a bite out of the face's edge — where a plane section always
runs the whole period; that case is closed along the seam, but others remain,
and the closure reports `BOOLEAN_TRACE_NOT_CLOSED` rather than guessing.

Until that is finished, a cut across such a pair still reaches the faceted
tier and is labelled an approximation, as before. What changed is that the
reason given is now the true one — the curve is carried; the closure is
what is missing — instead of a claim that the vocabulary cannot name it.

Mirroring a body that already carries a trace edge refuses by name: the two
callers of the loop-reversal walk reflect their surfaces differently, and
guessing the handedness would publish a body that is not the mirror of the
one asked for. STEP export refuses one too, because AP242 carries an
intersection curve only as a surface curve with an approximating spline
beside its two pcurves, which this exporter does not build yet.

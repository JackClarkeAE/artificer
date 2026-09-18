# ADR 0043: Two edges of a corner meet in a seam

Status: accepted, not yet built — the geometry below is derived and pinned by
an oracle test; the plumbing through `vertex_blend` remains.

- Date: 2026-09-18
- Decision owners: Artificer project
- Extends: [0034](0034-corner-blends-and-band-run-outs.md)

## Context

ADR 0034 gave a vertex two endings. Choose all three of its edges and the
corner closes with a sphere octant or a planar triangle; choose one and the
band runs out into the face across it. Choose two and the feature is refused,
`VERTEX_BLEND_CORNER_INCOMPLETE`, and the code says why:

> Three of three is a corner patch and one of three is a run-out; two of three
> would have to fade the band out along the third edge, which this vocabulary
> cannot draw.

That reason is no longer true, and it is the commonest thing a user asks for:
rounding an edge across the top of a part and the one running down from it is
two edges of one corner, and it is refused. Adding the third edge works, which
is how the refusal reads as arbitrary rather than principled.

The vocabulary gained the ellipse when two equal crossing cylinders stopped
being approximated by a quartic. That is exactly the curve this case needs.

## The geometry

Take a convex corner at the origin with the solid in the first octant. Its
faces are the coordinate planes — `P: z = 0`, `Q: x = 0`, `R: y = 0` — and its
edges are the axes: `A` along x, shared by `P` and `R`; `B` along y, shared by
`P` and `Q`; `C` along z, shared by `Q` and `R`.

Select `A` and `B`, leaving `C` sharp. Both bands are tangent to `P`, so both
axes lie in the plane `z = r`, one along x at `y = r`, the other along y at
`x = r`. Two equal cylinders whose axes cross: their intersection is two
planar ellipses, and the one bounding the material lies in `x = y`.

Sampling it confirms all three claims to machine precision:

- it is **planar**, in `x = y`;
- it is an exact **ellipse**, semi-axis `r√2` along the `x = y` direction and
  `r` along z — the residual of the ellipse equation is `1.000000000000000` at
  every sample;
- it runs from `(r, r, 0)` to `(0, 0, r)`.

Those two endpoints are the whole answer. `(r, r, 0)` is the new corner of face
`P`, which the two bands trim to `x ≥ r, y ≥ r`. `(0, 0, r)` is the new end of
edge `C`, which the two bands trim back by exactly `r` — faces `Q` and `R` each
becoming `z ≥ r`. Nothing else moves, and nothing else is needed.

A chamfer is the same shape with a straight seam. The bevel planes are
`y + z = d` and `x + z = d`; they meet in the line `x = y, z = d − x`, which
runs from `(d, d, 0)` to `(0, 0, d)` — the same two points.

So the band does not fade out along the third edge. It stops against its
neighbour along one seam, and the third edge simply starts further along. No
new surface type, no new patch: one seam curve the vocabulary already carries,
and a trim the run-out path already knows how to do.

## Decision

### A vertex has a third ending

`EndKind` gains a `Mitre`: the two selected edges, the one left sharp, and the
seam between the bands — a `Curve3::Ellipse` under a fillet, a `Curve3::Line`
under a chamfer. `read_end` builds it where exactly two of three edges are
selected and all three faces are planar, instead of refusing.

### The seam is derived, never sampled

Both endpoints are closed forms of the corner's own planes and the blend size,
as above. The ellipse's centre, axes and extent follow from the two cylinder
axes; the chamfer's line from the two bevel planes. An approximation here would
be a chord where the model promises a curve, and the validator would be right
to reject it.

### The refusal stays for what is still refused

`VERTEX_BLEND_CORNER_INCOMPLETE` remains for a vertex joining other than three
edges and three flat faces, and for a corner whose third edge is curved. What
it must stop saying is that the vocabulary cannot draw this, because it can.

### The oracle comes first

`two_edges_of_a_corner_*` in `crates/kernel/tests/vertex_blend_tests.rs` states
the volume this must produce, from closed forms verified against numeric
integration to ten digits:

- a chamfer removes `½d²(Lᴀ + Lʙ) − d³/3`;
- a fillet removes `r²(1 − π/4)(Lᴀ + Lʙ) − r³(5/3 − π/2)`.

The subtracted term is the corner both bands would otherwise claim twice. The
tests are ignored until the ending exists, so the number is settled before the
construction that has to hit it.

## Consequences

Rounding across and down becomes one feature. The selection a user makes
without thinking — two edges meeting at a corner — stops being the one the tool
refuses.

Taking a corner's edges *one feature at a time* is a different problem and is
not solved by this. The first feature commits a run-out, and meeting it would
mean reworking a finished feature rather than building a new one. The remedy
there is to add the edge to the feature that already rounded that corner, which
belongs in the workbench, not the kernel.

Nothing here touches the 1-of-3 or 3-of-3 endings, which stay exactly as
ADR 0034 built them.

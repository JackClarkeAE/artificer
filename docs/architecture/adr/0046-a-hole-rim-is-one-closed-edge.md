# ADR 0046: A hole rim is one closed edge

Status: implemented — fillets and chamfers around the rim of a hole through
any wall, built in place.

- Date: 2026-09-19
- Decision owners: Artificer project
- Extends: [0023](0023-carrier-unified-rims-and-exact-rim-blends.md),
  [0034](0034-corner-blends-and-band-run-outs.md)

## Context

The edge a user rounds most is the rim of a hole they just drilled. Every
exact rung declined it unless the hole went through a prism *cap*: the
rim-loop blend finishes a cap rim by re-extruding an inward offset of the
prism's profile, which needs the body to be a prism about that face's normal,
and a hole through a side wall, a sloped face, or any body that has stopped
being a prism at all fell through to the faceted tier, which cannot weld a
torus. The panel promised an exact rim blend and the ladder handed back a
sentence about regularized corner blends.

## Decision

### The construction is local

A hole rim is one closed circle where a plane meets a bore whose axis is the
plane's normal. A rolling ball of radius `d` sitting inside the material
touches the wall along the circle of radius `r + d` and the bore at depth
`d`, and between those two circles it sweeps a quarter torus — major radius
`r + d`, minor radius `d`, centred on the axis a depth `d` below the wall. A
chamfer is the cone through the same two circles.

Nothing else is rebuilt. The wall's hole loop grows to `r + d`; the bore's
rim ring sinks to depth `d`, carrying its vertices with it so every generator
that ran up to the rim shortens by moving one end; and the band is one face
per rim arc, with a seam — a minor circle of the torus, a line of the cone —
at every rim vertex. A rim the bore's own seam already splits keeps its
split. A rim that is one closed edge keeps one seam edge, used twice by the
one band face, as a full cylinder's is.

### No corner is closed

The rim is a closed edge and a closed edge has no ends, so there is no corner
patch, no run-out and no seam to derive: the two boundaries of the band are
the circles the ball touches, and both are in vocabulary. That is why this is
a smaller piece than either existing rim rung, and why it is exact to the
last bit: a fillet of radius `2` round a bore of radius `8` removes
`2π(r + c)·d²(1 − π/4)` with `c` the centroid of the corner region, and the
kernel's measure agrees with that closed form to `1e-9`.

### Where it stands in the ladder

After the cap-rim blends and before the corner blend: the rungs that own a
prism's rim keep their answers, and a rim that is not a prism's reaches this
one before anything is approximated. It takes exactly the edges of one whole
circle between one plane and the bores on its normal; a rim that is the
wall's *outer* loop is a boss, and the rim-loop blend's business. The
finish must fit — `d` of bore beyond the rim, and every other loop of the
wall clear of the grown hole — and says so by name when it does not.

## Consequences

Fillet and chamfer round a drilled hole answer exactly on any wall, which is
the case entry 7 of the fix log reported and the one users reach every time
they drill. The preview's promise of an exact rim blend for such a rim is now
kept by the ladder rather than contradicted by it.

The bore's two half-faces and the band's two faces meet the wall in tangent
rails, which the presentation draws as the rails they are; the band's own
seams are periodic-parameter seams and are not drawn.

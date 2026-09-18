# ADR 0045: A Boolean that resolves what it touches

Status: implemented — tangential contact and coincident boundaries are
resolved rather than refused, in the 2D pipeline and so in every reduction
built on it.

- Date: 2026-09-18
- Decision owners: Artificer project
- Extends: [0025](0025-analytic-surface-intersections.md),
  [0044](0044-a-corner-already-finished-asks-before-it-answers.md)

## Context

ADR 0025 drew the Boolean's domain around *transverse* crossings. Everything
else failed closed: a tangency, where two boundaries touch and part without
crossing, and a coincident carrier, where they share a stretch rather than a
point. The reasoning was that the pipeline classifies each piece of boundary
by an interior sample, and neither contact is a crossing, so neither could be
trusted to the sample.

That gate cost more than it was meant to. Three things a user does constantly
land on it:

- **A fillet standing apart from anything.** A band is tangent to the two
  walls it rolls between; that is what makes it a fillet, not an accident of
  modelling. Its removal solid touches the body along two lines, always.
- **Two solids meeting on a whole face.** Union them and the answer is one
  solid — the commonest assembly there is — but the shared face is a
  coincident boundary, so it refused.
- **Patterned copies that overlap**, and **interference studies** of parts
  that share a face plane: both reduced to "the engine cannot say".

## Decision

### A tangency is imprinted, not refused

The invariant the classify stage actually needs is that no piece *crosses* the
other operand's boundary, so that one interior sample decides the whole piece.
At a tangency none does: the boundaries touch and part again, and every piece
either side is wholly in or wholly out. The touch is therefore imprinted like
any other crossing — splitting both operands at a shared point — and which
side each piece lies on stays a question for the sample.

This holds wherever the pipeline finds a tangency: a line grazing a circle,
two circles touching, and the sampled path for section chords, where the trace
of one cylinder on another touches a face's boundary.

The result is exact, not merely close. A cube of side `L` with one edge
rounded by `r` measures `L³ − r²(1 − π/4)L` to the last bit the closed form
carries.

### A coincident boundary is classified by which way it runs

Where two boundaries share a stretch, the interior sample is genuinely
useless — both sides of the piece are the other operand's edge. Orientation
decides it instead, by the standard regularized rule. Material lies to the
left of an oriented loop, so two stretches either run *with* each other,
their materials on the same side, or *against* each other, materials on
opposite sides:

| operation | first operand keeps the shared stretch | second operand |
|---|---|---|
| difference | when they run against each other | never |
| intersection | when they run with each other | never |
| union | when they run with each other | never |

The second operand never contributes its copy, because two copies of one
curve is not a boundary — it is a seam the chain cannot walk. The shared
stretch's two ends are imprinted on both sides first, with the same points, so
the pieces align bit for bit.

Two squares meeting along an edge now union into one rectangle whose boundary
walks straight through where the shared edge was, which is the shortest
statement of what this buys.

### A crossing inside the feature floor is the vertex it is near

A crossing closer to a segment's end than the smallest feature the precision
policy admits *is* that end. Splitting there makes the sliver the check was
named for; refusing turns a vertex the operands already share into a reason to
give up. Snapping to the vertex avoids both, and needs no cut at all — which
is what a tangency landing exactly on a region's own corner looks like, and
what two bands springing from one wall produce.

### A decision about shapes may not turn on the last bit

Two rules in the pipeline decided geometry by arithmetic that the platform
gets to choose, and both had to go.

A tangency was recognised when a *sample* of the signed distance came within
the agreement. Whether any sample lands that near the touch depends on where
the fixed grid falls and on the last bits of a sine — which the C library
decides, and glibc and the MSVC runtime decide differently. The extremum is
now found by refinement first, and *it* is what the reach is measured against;
the samples only say where to look.

Vertex-on-vertex contact was legal only when the two vertices were identical
bit for bit. But two ends that are the same corner reached by different
arithmetic — one computed from a carrier, one refined from a touch — agree to
the last few bits and not to all of them, and how many depends on the machine.
Agreement decides it now, which is what `Tolerances::agreement` is defined to
mean, and it is exactly what the sew stage will weld into one point anyway, so
the two stages tell the same story.

Both were found the same way and neither would have been found by a single
case. A fillet cut at one radius passed; the same cut swept across forty radii
refused twelve of them, scattered, with nothing geometric separating the
twelve from the rest. Scattered failures across a smooth parameter are the
signature of a decision resting on arithmetic rather than on shape, and the
sweep is now a test, because that signature is invisible to any one example.

### What still fails closed



Contacts of no width that would weld two solids at a seam. A cylinder kissing
a plate along one line is not a solid and still refuses: the change is about
contacts that *bound material*, not about admitting non-manifold results.

## Consequences

ADR 0044's second answer — a finish standing apart from the corner it reaches
— works for both kinds and in both directions, including against a band an
earlier feature left. Three bands off one corner, each stood apart, land on
the union of three quarter-round prisms whichever order they are taken in.

Face-on-face union, overlapping pattern copies, and interference studies of
parts sharing a face plane all answer now where they used to refuse. Three
tests that pinned those refusals were rewritten to pin the answers instead,
which is the honest record of a domain that grew.

One hazard found in the building, worth writing down: an earlier attempt
deduplicated section curves by their endpoints. Two crossing bores meet in two
branches that share both endpoints and are not the same curve, so that welded
them into one and published a solid that was *wrong* rather than refused — the
worst failure a kernel has. Sameness of a curve is the whole curve, and
interior samples are what tell branches apart. The Steinmetz oracle is what
caught it.

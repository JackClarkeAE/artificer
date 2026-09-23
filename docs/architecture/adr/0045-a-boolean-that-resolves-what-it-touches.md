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

### A profile may touch what is already on the face

The face-feature gate refused any profile boundary within the feature floor
of the face's outline or a hole's rim, and called it "outside face
material". An annular boss whose inner rim *is* the bore's rim — the
commonest boss there is — was refused as though it had been drawn off the
face, and the enclosed area between two drawn circles and the face's own
outline was not a region at all, because the outline took no part in
closure.

Both halves now go through the same door. The face's outline and its hole
rims travel with the sketch as *support curves* and join the arrangement
that closes regions, with reserved entity ids so a document replays the
same regions its canvas showed. A profile coincident with the face's
boundary or a void's rim is reformulated as the Boolean the general engine
already resolves — a cut becomes the difference, an add the union — and
only a profile that *crosses* out of the face is still refused. An add
whose profile misses the face has no interface; the union of two solids
that never meet is two solids, and that still refuses by name.

### A carrier pair is refused only where the faces could meet

The intersection matrix answers for carriers, which are unbounded, and it
refuses a pair it cannot trace — two bores of unequal radius crossing at an
angle — whether or not the two bounded faces ever come near each other. A
boss on one end of a block is nowhere near the bore through the other end,
and a refusal about their carriers was refusing the Boolean.

Each face now has an extent: a box it cannot leave, a plane face's
parameter box mapped to the plane, a cylinder face's whole drum over its
height range. A pair the matrix refuses is skipped when the extents are
apart, and is a refusal only when they are not. The extent is a superset of
the face, so a pair it separates is a pair the faces separate; it is never
used to *skip a pair the matrix can answer*, because a section is closed by
pieces from every face the carrier crosses, near this face or not, and a
loop with a piece missing does not close.

### The skin counts as covered

Where the other solid has a face on this very carrier, the probe that
decides which side of a section chain the other solid lies on lands on that
skin, and a ray's parity there is a coin toss — one half of a bore answered
one way and the other half the other, and the sewn shell had four open
edges. A section is a closure. A probe on a face of the other solid is
covered, and only a probe off every coincident face is asked of the solid's
interior.

Three smaller things the same case taught, each a seam problem in
disguise. A generator on a cylinder's seam maps its two ends to `π` and
`−π` by the arctangent and read as no generator at all; the azimuth is the
same, and the first end's branch is kept. A loop carried from one face onto
a coincident cylinder came back torn at the seam for the same reason; it is
now carried as a loop, each piece brought by whole turns onto the branch the
previous one ended on, and the whole brought onto the window the receiving
face's own region uses. And two section chords sharing one oblique trace on
a cylinder — the edge where a bore meets a wall, and a tool wall that
continues it — had no overlap rule, so the clip refused what it should have
kept: a region contains its own boundary.

### The faceted tier may not publish a wrong volume

The faceted fallback closed a counterbore into a perfectly valid shell
around the wrong material: a *cut* that left the body with more volume than
it started with, published as an approximation. Closedness is what the
validator certifies, and a tessellated rebuild can be closed around the
wrong answer, which is the failure named at the end of this document.
Volume is the cheapest invariant that catches it: a cut may not gain volume
and an add may not lose it, beyond the approximation budget, and a
candidate that does is refused rather than published.

### Two things the wider domain uncovered downstream

Both were found by the same signature as the last section: a body that
validated and measured wrong.

The exact measure counted a planar face's elliptical arc in the `∮x dy`
form of Green's theorem while it counted every chord in the symmetric
`½∮(x dy − y dx)` form. The forms agree around a closed chain, which is why
a bore's ellipse — two arcs that close on each other — measured exactly, and
disagree along an open one, which a bevel plane cutting through two fillet
bands is: two arcs ending on straight edges. That face measured at half
again its area and the body gained volume under a cut. The arc terms now
use the chords' form, and the stand-apart test measures the body twice,
once exactly and once from its tessellation, and asks the two to agree.

An add's tool used to overshoot into the body so that no cap lay on the face
plane, on the reasoning that an overshoot inside the body changes nothing. It
changes nothing under the part of the profile that lies over the face; under
a part that hangs past the face's edge it pokes out again and publishes a
sliver as material. An add's tool now stands exactly on the face, and its
cap is a coincident face the overlay rule resolves.

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

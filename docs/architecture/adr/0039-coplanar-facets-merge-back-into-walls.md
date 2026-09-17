# ADR 0039: Coplanar facets merge back into walls

Status: implemented — a wall the faceted Boolean cut into a fan of panels is
rebuilt as one face before it becomes topology, and the merge stops wherever
the union would need something a face here cannot say.

- Date: 2026-09-17
- Decision owners: Artificer project

## Context

The faceted tier splits a face every time a cutter plane passes through it.
The pieces are all still the same flat wall, differing in nothing but where
they were cut, and nothing put them back together. Two crossing bores in a
block measured **2,959 faces, 2,873 vertices and 5,802 edges** for a shape
whose exact form has fewer than twenty.

The cost is not only memory. Every stage downstream is a function of face
count: validation, the display scene, hit testing, the history replay. The
reproducer's own wall clock was **14.4 seconds**. And the seams between panels
are edges the presentation has to classify, so a flat wall arrives as a fan of
creases across geometry that has none — the user's report of a body that "went
faceted on one cut direction" is partly this.

ADR 0026's P-series named the merge. P1 measured what it could be worth and
was careful to bound the claim: of the 3,259 faces in that body, 2,356 are
oblique chords of the bore walls and only 903 are axis-aligned, so coplanar
merging can touch about a quarter of the faces rather than the order of
magnitude first estimated.

## Decision

### Facets of one plane are one face, dissolved pairwise

Group the facets by plane and role, then merge two at a time: collect the
directed edges of both, cancel the pairs that appear once each way — those are
the seam between them — and chain what is left. If the result is one simple
loop, the pair becomes one facet. Repeat until nothing more merges.

Nothing moves, and nothing is dropped. Every vertex of a merged outline is a
vertex the group already had, so the merge cannot change what the body
occupies.

### A corner is dropped only where every facet agrees it is not one

A merged outline walks the outsides of the panels it replaced, so the two ends
of each dissolved seam stay on it as points where the boundary goes straight
on. Those have to go — a vertex joining two edges and two faces is one the
blend preflight refuses, which it should, because it is not a corner — but
they cannot go from one facet alone.

Dropping them per-facet is what a first attempt does and it is wrong twice
over. A neighbouring facet still ends at such a point, so removing it here
leaves a T-junction, and conforming that back changes what meets at the corner
beside it: filleting an outer edge of a crossed body then found four faces at a
corner that has three and refused. It also merges *less*, because an outline
stays simple more often with its points in — 349 faces against 229.

So the dissolve asks every facet first. A point that runs straight through on
all of them is a corner to nobody, and removing it from all of them at once
leaves no junction behind. That gets both: 229 faces *and* 391 vertices, where
per-facet dropping gave 349 and 503, and keeping everything gave 229 and 1,837.

### The merge stops where a face cannot follow

A whole group rarely becomes a single facet, and the reason is structural
rather than a limitation of the algorithm. A box face with a bore through it
is a ring, and a ring needs an inner loop. A facet in this tier is one vertex
list. It cannot say "with a hole in it", so the ring is left as a handful of
pieces rather than forced into one.

Every refusal is of that kind: a pair that meets only at a point, a pair that
meets in two places, a pair whose union would need an inner loop, a pair that
overlaps rather than meets. Each is left exactly as it was. This is what makes
the merge safe to run on every faceted Boolean without a flag: the worst case
is the fan it started with.

The merge also refuses to turn a wall over. A merged outline whose normal
disagrees with the facets it came from was chained the wrong way round, and a
face pointing into the material is worse than a fan of panels pointing out of
it.

### What it is worth, measured

On the two-crossing-bore body, as it stood when this was written. That
particular body — two bores of **equal** radius on crossing axes — no longer
reaches this tier at all: ADR 0026 K1 stage 3 landed the same week and it is
now exact, at ten faces. The measurements below are kept because they are what
the merge does to a fan, not because this is still the body that produces one.
A crossing pair of *unequal* radii is a quartic, is still refused by name, and
still arrives here; it is what the merge's own gates measure now.

| | before | after |
|---|---|---|
| faces | 2,959 | 229 |
| vertices | 2,873 | 391 |
| edges | 5,802 | 624 |
| reproducer wall clock | 14.4 s | 1.5 s |

The estimate the gate used to carry was roughly 140, on the reasoning that the
box's six planar faces would come back as six. They do not, for the ring
reason above, and recording why is more useful than recording the number.

## Consequences and limits

**A cylinder's fan is untouched.** The panels of a faceted bore wall are each
on their own plane, so they are never in one group. That is the 2,356 faces P1
measured, and it is the majority of what remains. Only an exact cylindrical
wall removes them, which is ADR 0026 K1 stage 3 and ADR 0025's amendment — and
for bores of equal radius on crossing axes that is exactly what happened: the
wall is two half-cylinders and there is no fan. It remains true for every
faceted body, which is now the ones outside the exact vocabulary rather than
every crossing bore.

**The cutter's subdivision budget stays as it was.** The faceted cut path
carries a note that handing the cutter the clamped budget halves fragmentation
but drops a bore's panel fan below the eight-normal threshold that recognises
it as one logical cylinder, and says to change it together with the coplanar
merge that removes the fan altogether. This merge does not remove *that* fan —
it is not coplanar — so the note's precondition is unmet and the budget is
unchanged.

**One gate had to be restated rather than retuned.** The crossing-cut
presentation test bounded visible edges at a seventh of the total, as a proxy
for "the fan is not being drawn". With the fan gone the ratio measures nothing:
the smooth interior seams are what it was counting, and removing them raises
the ratio while improving the drawing.

Measuring that body both ways gave a better statement than either a ratio or a
bare bound. It publishes 4,470 edges without the merge and 834 with it, of
which 519 and 366 respectively are drawn. The drawn count falls without any
line going missing: a line that arrived as several collinear pieces now
arrives as one edge, the points between them having been dissolved as corners
to nobody. The gate bounds the total and pins the direction — the merge takes
seams out and joins collinear runs, and neither can put a new line on the
screen. The assertions that already mattered, that each bore still publishes
one coherent prismatic carrier and that every edge within one is smooth, are
unchanged and still pass.

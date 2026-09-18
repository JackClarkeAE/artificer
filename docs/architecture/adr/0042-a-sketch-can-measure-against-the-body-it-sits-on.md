# ADR 0042: A sketch can measure against the body it sits on

Status: implemented — the dimension tool accumulates picks, a host-body edge is
projected into the sketch as pinned reference geometry when a pick names it,
and an ordinate dimension moves the located point rather than refusing itself.

- Date: 2026-09-18
- Decision owners: Artificer project

## Context

ADR 0038 gave the dimension tool a distance between two points. ADR 0041's own
"what is not done" section recorded that sketch operands are sketch entities
rather than model selections. Both were true, and together they left the most
ordinary thing a sketch says impossible to say: *this hole, twelve millimetres
from that edge*.

Three separate faults produced the same silence.

**The dimension tool only accumulated points.** `take_dimension_point_pick`
tested for an endpoint and nothing else. A second click that landed on a curve
returned "did not take the click", fell through to the tool's ordinary path,
and armed that curve's own length — silently discarding the point named first.
So the point-to-line relation added alongside ADR 0038 was reachable only from
the Distance *constraint button*, never from the tool named Dimension. A user
who chained two clicks saw one of them vanish and had no way to know which.

**A host-body edge was not sketch geometry, and nothing could make it so.**
The body under a face sketch is drawn as `SketchContextTriangle`, and the
comment on that type says what it is for: *"Projected tessellation is
presentation-only. It is never considered by sketch snapping, selection,
dimensions, profile certification, or kernel input."* Both operand hit tests
searched the authoring graph. A cube's edge has no entity there, so it could
never become a `RelationOperand` — the user's pick landed on something they
could see, could snap to, and could not name.

**An ordinate dimension anchored both of its ends.** `stage_relation_measurement`
anchored the caller's held point *and* the relation's datum points. For a
separation that is right: the ends are equals, so the caller says which one
stays. For anything measured from a datum it pins both sides of the equation
and leaves the solver nothing to move, so every typed value but the one already
held came back as `constraint system is conflicting`. This was latent — the
ordinate relations shipped without a retype ever being exercised against a
pinned datum, which is precisely the case a projected edge creates.

A bridge for the second fault was already half-built and unused.
`SketchViewportContext::snap_curves` carries the host face's *analytic*
boundary in the sketch's own `(u, v)` frame — snapping has read it since face
sketches existed. And `SketchEntityRole::Reference` existed, documented as
"read-only projected/support geometry; never a material boundary", with nothing
in the tree ever creating one.

## Decision

### A pick that lands on a host edge projects it, there and then

Naming is what projects. There is no separate "convert entities" chore to
discover, because the moment a user wants to measure to an edge is the moment
they click it, and a tool that answers "you must first do something else" is
the fault this ADR exists to remove.

`SketchRecipe::ProjectedEdge { start, end }` carries two endpoints in the
sketch frame and takes the `Reference` role. It is a *copy*, not a live link:
the body's topology is not the sketch's to own, and a persisted reference into
B-rep identity would be a much larger promise than measuring against an edge
needs. If the body changes, the projection is stale sketch geometry the user
can see and delete, not a dangling handle that fails at rebuild.

### A projection is pinned, and pinning is the whole point

Two `Fixed` constraints, one per endpoint, staged as the projection is made.
`Fixed` outranks anchoring (ADR 0035), so the projected edge holds whatever any
later relation asks of it.

This is what makes the dimension mean what the user thinks it means. Typing
`2` into an offset from a borrowed edge has to move the *sketch* — the solid is
not the solver's to push around. Without the pins the solver would be free to
satisfy the number by sliding the reference line, quietly decoupling it from
the edge it was copied from and leaving the drawing lying about the part.

### Reference geometry is never a material boundary, and never an axis

`Reference` was chosen over `Construction` deliberately. Construction geometry
is dashed layout that profile compilation ignores, which would have been enough
on its own — but a single construction line is also how a sketch names a
revolve axis, and `centreline_axis` returns nothing when it finds several.
Projecting an edge would then have silently broken revolve on any sketch that
used one. `Reference` carries the exclusion from profile compilation without
carrying that second meaning.

It is drawn dashed in the support colour, so it reads as borrowed rather than
drawn, and it refuses to be dragged: moving it would be the sketch claiming an
authority over the solid that it has not got.

### Naming the same edge twice is one edge

A projection is matched against the ones already in the sketch, on endpoint
positions within modelling resolution and in either direction. Two dimensions
to the same edge share one reference curve rather than stacking pinned
duplicates on top of each other.

### The dimension tool accumulates, and a curve's first pick is shared

Picks accumulate until two objects are named; the pair is then whichever
relation fits — point to point, point to line, or line to line — built by the
same code the Distance constraint button uses, because "the distance between
these two" means one thing however the user asked for it.

Single-object dimensioning is untouched, and the mechanism is worth stating
because it is what makes the change safe. A bare point has no numbers of its
own, so naming one is the whole gesture and the click ends there. A curve does
have numbers, so its first pick is *shared*: remembered as half a pair, and
passed on to the path that opens the curve's own value box exactly as before.
"Click a line, type 3, Enter" never reaches a second click and so never
changes behaviour. The pair only forms when a second object follows instead of
a typed number.

Anything that ends the gesture forgets the half-made pair — clicking empty
space, changing tool, or committing what was typed — so a stale first pick
cannot ambush the next dimension the user places.

### A datum is the only anchor a relation measured from it has

`stage_relation_measurement` now holds the caller's point only when the
relation has no datum. Where there is one, the datum is anchored and nothing
else is, so the located point is free to move and the typed value is reachable.

This is the general statement, not a patch for projected edges: an ordinate
relation locates one thing against another, and anchoring both ends of it can
only ever refuse every value but the current one.

## Consequences

A sketch drawn on a face can now position its geometry against that face,
which is what face sketches are for. The three ordinate relations added
alongside ADR 0038 became reachable from the tool they were built for.

A projection is committed geometry, so it is undoable, persisted, selectable,
and deletable like anything else in the sketch — and it appears in the sketch
the user did not explicitly ask for it to appear in. That is the cost of
naming-as-projection, and it is paid visibly: the dashed support-coloured line
is exactly where the edge is, and can be removed.

Arcs are skipped. The constraint vocabulary has no point-to-arc offset, so
offering a curved host edge as an operand would only refuse itself a moment
later; a straight edge is what can be measured against today.

The projection is refused while an edit waits at the confirmation gate.
Committing underneath a staged transaction would leave the gate holding one
built against a definition that no longer exists.

Nothing here touches the ADR 0041 invocation model. A sketch operand is still
a `SketchEntityId` rather than a model `SelectionItem`; what changed is that a
host edge can *become* a sketch entity. The `SKETCH_RELATION` appetite remains
declared and inert.

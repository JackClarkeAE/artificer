# ADR 0035: Drawn connections hold, and a deliberate edit outranks the solver

Status: implemented — an endpoint that lands on another endpoint persists a
coincidence, and dragging or retyping either line carries its partner the whole
way rather than half of it. The behaviour is one preference in SNAPPING AND
VIEW, on by default. This delivers the "inference at draw time" paragraph of
ADR 0026 F1 for the endpoint case.

- Date: 2026-09-15
- Decision owners: Artificer project

## Context

The sketch crate has had a real relation solver since it was written: the
`Coincident` kind is persisted, transactional and undoable, and
`constraints::solve` converges it by projection. Nothing drawn on the canvas
ever created one. The only producer was the Coincident relation tool, which the
user has to reach for deliberately, so two strokes that met at a corner met
only in the sense that their coordinates agreed. Drag one and the corner came
apart.

Persisting the coincidence at the moment the endpoint snap fires closes half of
that, and it is the half ADR 0026 already specified. The other half is harder
and is the reason this record exists.

The solve is a read-time operation. A point's recipe literal is a seed, and
`evaluated_curve` runs the solver over every active point before returning
geometry, so what reaches the screen and the profile compiler is the solved
answer, not the authored one. `Coincident` projects a free pair onto their
midpoint, which is the fair answer when nothing distinguishes them. It is the
wrong answer for an edit. Dragging one end of a joined pair rewrites that
operation's recipe and leaves the partner's alone, so the solve would find one
point moved and one not, and put both half way between: the endpoint the user
is holding travels half as far as the pointer does, and lets go of it again on
the next frame. A relation would have made dragging worse than no relation at
all.

## Decision

### An edit is an authority; a relation is not

`constraints::solve` takes a set of anchored points alongside its seeds, and
holds them exactly where the seeds put them. `Fixed` still wins over an anchor,
because a pinned point is pinned whoever is pulling at it — the anchor set is
merged into the same `pinned` map and the `Fixed` entries are written over it.
Every existing projection already asks that map whether a point may move, so no
constraint kind needed changing to respect an anchor.

The editing paths anchor the points the edit authored. That is the whole rule:
the operation whose recipe the user just rewrote says where its own points are,
and everything joined to them follows.

### The follower's new position becomes its own authored intent

Anchoring alone would only last for one solve. The next ordinary read —
a repaint, a profile compile, a reopened document — would solve again with
nothing anchored and go back to the midpoint. So
`stage_replace_pulling_followers` solves once with the edit anchored, and then
writes each point the solver had to move back into its own operation's recipe,
inside the same transaction. The sketch that results satisfies its relations
outright, so the next unanchored solve has nothing left to average and every
later reader agrees with the one that made the change.

This is the first time the solver's output becomes authored intent rather than
a view of it. It is deliberate. A drag that silently depended on a transient
pin would be a sketch whose geometry is a function of which path last touched
it, and this tree does not have geometry like that.

Settled followers join the anchor set, so a chain of joined lines straightens
out from the edit outwards over successive passes, bounded at eight.

### Both edit paths, and one preference

A drag (`reshape_selected`) and a typed length or angle
(`set_selected_recipe_parameter_text`) are the same act stated two ways, and
both route through the same pulling replacement. A drag additionally re-reads
every presentation curve through `refresh_presentation_geometry` afterwards,
because the curve the solver moved is not the one the pointer is holding and
the drag loop only knows about the latter.

`SnapSettings::keep_points_connected` governs both what drawing infers and
whether an edit carries followers. Off, the canvas behaves exactly as it did
before this record: a snap places a coordinate and nothing more. Relations the
user made by hand are untouched by the preference in either direction — it
governs inference and the pull, not the relation system.

## Consequences and limits

**Only endpoint-on-endpoint snaps infer anything.** A grid snap, an on-curve
snap and a support-edge snap produce a coordinate and no identity, and a stroke
that finishes on a gridline which happens to pass through another endpoint was
not aimed at it. `SnapResult` carries the exact `SketchPointId` for the one
case that has one, so the staging path is never guessing from coordinates.

**A follower its recipe cannot state is left behind.** Writing a point back
needs a literal in the owning recipe to write it into, and several points do
not have one: a rectangle's three derived corners, a polygon's vertices, a
circle's radial point, a pattern's copies, a trim's fragment ends, a fillet's
tangencies. Those are refused by `SketchRecipe::set_authored_point` rather than
approximated, the edit goes through regardless, and the ordinary solve goes on
splitting the difference for that one relation. A rectangle joined by its
*authored* corner does move, and it moves as a whole, because width and height
are measured from that corner and a deformed rectangle is not something this
recipe can say.

**An edit whose own points cannot all be held falls back.** Dragging a line
that also carries a distance relation between its two ends asks for both to be
anchored and the distance to hold, which may be a contradiction; the anchored
solve fails and the plain replacement is used instead. The drag still happens.

**A conflicting inferred coincidence is dropped, not raised.** If the
coincidence a snapped stroke implies cannot be satisfied, the stroke still
commits without it. The user asked for a line.

The horizontal and vertical half of ADR 0026's inference paragraph — a stroke
that lands within `angular_agreement` of an axis persisting a `Horizontal` or
`Vertical` — is not done here and remains open.

# ADR 0044: A corner already finished asks before it answers

Status: accepted; joining is implemented, and standing apart is implemented
for both kinds where one axis reduces the cut to a prism against a prism.
Standing apart from a band an earlier feature left is refused by name.

- Date: 2026-09-18
- Decision owners: Artificer project
- Extends: [0034](0034-corner-blends-and-band-run-outs.md),
  [0043](0043-two-edges-of-a-corner-meet-in-a-seam.md)

## Context

A vertex takes one patch. Round all three of its edges together and a sphere
octant closes it; bevel them and a planar triangle does; round two and
ADR 0043's seam does. Every one of those is a single feature that owns the
whole corner.

So a user who finishes some of a corner's edges and then comes back for
another is refused, and the refusal is correct — the committed feature already
decided what that corner looks like, and a second feature arriving later
cannot re-cut it. `VERTEX_BLEND_CORNER_CURVED` says so where a fillet left a
cylinder behind; `VERTEX_BLEND_RADIUS_MISMATCH` says so where the sizes
disagree.

Correct, and useless. The user is told what they cannot do and left holding a
selection with nowhere to put it. Worse, the thing they want is available:
had they chosen all the edges at once it would simply have worked, so the
tool is refusing an outcome it can build, on the grounds of the order the
clicks arrived in.

There are two different things they might have meant, and the tool cannot
know which:

- **one rounded corner**, as though the edges had been chosen together; or
- **bands that merely meet there**, each edge finished as if on its own body,
  the corner keeping a point of its own.

Both are ordinary things to want. Guessing is worse than asking.

## Decision

### The refusal becomes a question with two answers

When a finish reaches a corner an earlier one already shaped, the panel stops
refusing and puts two options above the size, because which one is meant
changes what the size is for.

### Join this fillet or chamfer to the others

The edge is added to the committed feature that owns the corner, and that
feature is replayed. Nothing new is built: the corner becomes the 3-of-3 patch
or the 2-of-3 seam the kernel already had, and the body is exactly the one the
edges chosen together would have made. The feature tree grows a feature rather
than gaining one, which is what the history should say — there is one corner
and one feature that made it.

A joined corner has **one size**. The rolling-ball patch is a single sphere
and the bevelled one a single triangle, so the corner takes the earlier
feature's distance and the panel says so rather than silently ignoring what
was typed. A corner also takes one **kind**: a fillet does not join a chamfer,
and that selection stays two features whatever the panel offers.

### Keep independent

The earlier feature is left as it stands and the new band is built beside it,
the two meeting along a seam, with the corner's own point surviving where all
of them meet. This is the shape a user means by "as though they were extruded
separately", and it is genuinely different from the joined corner — not a
worse approximation of it. Three bevels off one corner of a cube of side `L`,
taken one at a time, leave

```text
L³ − (3·½d²L − d³ + d³/4)
```

where the joined answer's corner patch removes more: the `d³/4` those three
planes leave behind, meeting at `(d/2, d/2, d/2)`, is exactly the material a
patch would have taken.

Stated as a solid rather than as a patch, standing apart is simply the body
less that band's own removal. So it is built as a Boolean, not as topology
surgery on faces a previous feature owns — which is what makes it a bounded
change rather than a second blend engine.

#### How it is cut, and what it still cannot reach

A bevel's removal is a half-space. A fillet's is the curvilinear triangle
between the two walls and the band — the corner a rolling ball cannot reach.
Both are prisms swept along the edge, so a finish standing apart is a prism
against a prism: ADR 0025's first reduction, and the one that now carries a
tangential contact.

That last part is new, and it is the substance of this half. A fillet's band
*touches* each wall rather than crossing it — that is what makes it a fillet —
and the regularized 2D Boolean underneath the prism reduction used to refuse
any tangency, on the grounds that its pipeline classifies pieces by an interior
sample and a tangency is not a transverse crossing. But the invariant that
pipeline actually needs is only that no piece crosses the other operand's
boundary, and at a tangency none does: the boundaries touch and part again. So
a tangency is now *imprinted* like any other crossing, and which side each
piece is on stays a question for the sample, which is exactly what the
classifier is for. Coincident carriers — boundaries that share a stretch rather
than a point — are a different question and still fail closed.

The result is exact, not merely close. A cube of side `L` with one edge rounded
by `r` measures `L³ − r²(1 − π/4)L` to the last bit the closed form carries.

The tangency has to be *exact* to be a tangency at all, which turns out to be
the delicate part of building the tool. Taking the band's contact with each
wall as `r/tan(θ/2)` along the face puts it a bit or two off the true foot, and
a flank plane 4e-16 outside the band does not graze it — it misses, and every
stage after that is entitled to believe the miss. The contacts are therefore
taken as the feet of the perpendiculars from the band's own axis, where they
are exact by construction.

What is still not cut is a finish standing apart from a *band*: a corner an
earlier feature rounded, or a second edge running across the first. There no
single axis reduces the pair to prisms, so the general analytic engine has to
answer, and it carries no tangency of its own. Those are refused by name, and
what they need is the same reasoning carried from the 2D pipeline into the 3D
one — a larger piece of work, because the section assembly meets its own
degeneracies there: a chord lying along a face's boundary, one curve arriving
twice from two faces that share a tangency, and faces that abut rather than
cross.

### How a finished corner is recognised

Two signals, because one is not enough.

A corner whose edges were finished **together** has been cut back: where three
edges met, four now do — the part of the third edge that survived, the two
lines the bands lie along, and the seam between them. A valence above three at
the end of a selected edge is that mark, and counting it is one pass over the
scene, cheap enough to ask on every frame.

A corner finished **one edge at a time** keeps its three, because the first
band ran out into the face beside it rather than cutting anything back.
Nothing in the shape of the body gives it away and only the kernel can tell.
So its refusal counts as the same question being asked, and the options appear
with it.

Which committed feature owns the corner is not decided by either signal. The
candidates are tried newest first and the first that replays is kept: a
feature that does not own the corner refuses the joined set for exactly the
reason the user is here, so a wrong guess costs a replay and never a wrong
body.

### An edge a feature has already shortened

Joining needs the new edge as the committed feature's own input knew it, and
history cannot say. A finish does not *modify* the edge running into the
corner it shapes — it regenerates a shorter one, recorded as `Generated` with
no input to walk back through — so the walk that names persistent targets
runs out.

The link is made where it is still visible, in the geometry: the survivor is
straight, collinear with what it was cut from, and lies inside it, which names
one edge of that feature's input and no other.

## Consequences

The commonest follow-up in the tool — round an edge, then round the one next
to it — stops being a dead end. What the user gets is the body they would have
had by selecting both at once, which is the answer they expected before they
knew there was a question.

Joining edits a committed feature, so everything after it replays. That is
already how a sketch or an extrusion is edited, and the history reads better
for it: one corner, one feature.

The second answer ships for both kinds on the shapes one axis reduces, and is
refused by name on the rest — so the panel never has to pretend the first is
the only thing anyone could have meant.

Nothing here changes what the kernel refuses. The vocabulary is untouched; the
workbench stopped treating a refusal as the end of the conversation.

# ADR 0044: A corner already finished asks before it answers

Status: accepted; joining is implemented, standing apart is specified and
gated — the panel offers it only where the kernel can cut it, which is
nowhere yet.

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
worse approximation of it.

Whether that seam is drawable depends on what meets what:

| new band | against | seam | drawable |
|---|---|---|---|
| chamfer | chamfers | plane ∩ plane → line | always |
| chamfer | fillets | plane ∩ cylinder → ellipse | always |
| fillet | chamfers | cylinder ∩ plane → ellipse | always |
| fillet | fillets, same size | crossing equal cylinders → planar ellipse | always |
| fillet | fillets, different size | crossing unequal cylinders → quartic | **no** |

Four of the five are curves the vocabulary already carries. The fifth is not,
and per ADR 0002 that case is refused by name — an independent fillet there
must match the size already on the corner, or join it — rather than
approximated into a curve the validator would be right to reject.

Under a fillet at one size the three bands meet at a point of their own, a
third of the way in from the vertex along each axis: for radius `r` on a
square corner the triple point is at `r(1 − 1/√2)` on all three, and for a
chamfer of setback `d` the three bevel planes meet at `d/2`. Those points are
what the joined corner removes and the independent one keeps.

None of this is built. The panel shows the option greyed with that reason, so
the choice is visible and honest rather than absent.

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

The second answer is written down and visible before it is built, so the panel
never has to pretend the first is the only thing anyone could have meant.

Nothing here changes what the kernel refuses. The vocabulary is untouched; the
workbench stopped treating a refusal as the end of the conversation.

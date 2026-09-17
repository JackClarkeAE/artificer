# ADR 0037: Where a side of an extrusion ends

Status: implemented — a side ends at the plane of a face, the destinations are
discovered from the solid rather than from what the camera is showing, and a
reference that stops resolving is reported rather than guessed around.

## Context

ADR 0032 gave each side of an extrusion its own extent: a typed distance, or a
face the side ends at. The face route was wired from the panel inward, and in
the running application it did nothing at all. The user's report was simply
"the to-face tool for extrusions and cuts still doesn't work (we tried it here
and got nothing)".

Three separate things were wrong, and they shared a cause.

**The destinations came from the picture, not from the solid.** Arming the
option printed "Pick a face in the viewport" and then waited for a click. The
viewport draws only the facets turned towards the camera and culls the rest
during projection, so the faces it offers to a click are, by construction, the
near side of the material. The face a through cut ends at is the far side.
Asking for a click was asking for a click on something that was not on the
screen, and the side sat armed until the user gave up and typed a number.

**A destination had no meaning beyond a face number.** The panel showed
`Face #89`, the status line said the same, and nothing said whether the length
being measured was to that bounded face or to the plane it lies on. On a
tessellated body the same wall is hundreds of faces, so "which face" is not
even a well-formed question.

**A reference that stopped resolving was silent.** The rebuild measured the
stored reference again and, if it could not be found, left the stored length in
place and carried on. That is the right thing to do with the geometry; it is
the wrong thing to do with the user, because a side that has quietly stopped
following its face looks exactly like one that is still following it. Worse,
for a body raised up to a face of *another* body the reference could never
resolve: a new body replays from an empty snapshot, so the face was not in the
snapshot the reference was being resolved against and never had been.

## Decision

### Discovery, validation and selection are three jobs

Discovery asks the solid. `extrusion_targets(side)` walks every face of every
body the document holds and keeps the ones this side can actually reach.
Nothing in it reads the camera, which is what makes the set of destinations a
property of the model rather than of the view. Validation is the same rule that
judges a face clicked in the viewport — one function, `measure_extent_to_face`
— so the two routes can never disagree about what is reachable. Selection is
what the user does, and it is the panel's list.

Discovery walks every face of every body, which on a faceted design is measured
in thousands, so the answer is remembered for as long as the bodies on screen,
the sketch being swept and the direction of the sweep are unchanged. Those are
the whole of what the answer is made from, which is what makes comparing them
enough.

### A side ends at a plane, not at a bounded face

What is measured is the height of the face's supporting plane above the sketch
frame. The recipe stores a face because a face is what survives a rebuild, but
the meaning is the plane. Two faces at the same reach are therefore one
destination, and the list says how many faces of the design lie on it; the
largest of them is the one named, because on a tessellated wall the rest are
slivers of the same answer.

This is the supported meaning, and it is deliberately narrower than "up to
face". A parallel face on a ledge elsewhere on the body has a plane the profile
may never reach, and a single recorded length cannot describe a termination
that differs across the profile. Up to a face's own boundary, and through all,
are separate constructions with separate recipes; they are not this one wearing
a different label.

### One destination is not a choice

Where exactly one plane can end a side, arming the option takes it and says
which. Where several can, the side stays armed, the count is named and the
panel lists them, each with the length it would sweep and the kernel's own
description of the face. Where none can, saying so beats a prompt that can
never be satisfied. Clicking a face in the viewport still works and is the
shortcut for a destination that happens to be on screen.

Suggesting the only candidate while a feature is being made is a convenience.
It must never become choosing a different candidate behind the user's back
while the feature rebuilds, so discovery runs when the user arms the pick and
never during a replay. A replay resolves the reference the feature stored, and
nothing else.

### The reference is a dependency

A side that ends at a face depends on the feature that made that face, even
when the face belongs to another body. That edge is recorded on the feature, so
the document replays the dependent when the target changes, and skips it by
name when the target is suppressed. Without it the target could be changed or
removed with the dependent never replayed at all, going on holding a length
measured against geometry that had moved.

Because the target may be in another body, the reference is resolved against
every body the document holds rather than only against the feature's own input
snapshot. A reference names one producing feature and a feature's outputs live
in one body, so at most one of them can answer: the search is for which body
holds the answer, never for a face that will do instead.

### Losing a target is reported, never guessed around

When the reference cannot be measured again — missing, ambiguous, no longer
flat, no longer parallel, or no longer ahead of the sketch — the side keeps the
length it last had. The feature rebuilds at the size it had rather than failing
the document, and it does not attach itself to whichever other face happens to
be nearby. Each such side is named in the history strip until a rebuild finds
its face again, because the status line is overwritten by the next thing that
happens and this needs to outlive that.

Reopening a committed extrusion measures its to-face sides again before the
editor shows them, so the number in the editor is the number in the model. The
recipe's stored length is the last one measured and replay recomputes it, so
the stored value is a fallback rather than the authority.

## Consequences

To face works from the panel with no camera manipulation, including for the
through cut that motivated the report, and a body can be raised to a face of
another body and follow it.

Two limits are accepted. A destination is a plane, so a feature cannot yet end
against a bounded face or against different geometry across its profile. And
suppressing the feature that made a target face takes the dependent feature
with it; that is explicit and reversible, but it is a heavier answer than
leaving the dependent at its last length, and if that trade proves wrong it is
the dependency edge that should change, not the reporting.

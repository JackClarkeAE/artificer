# A committed feature is edited where it was made

Status: Accepted and implemented (0.98.2)

## Context

A parametric history is only worth keeping if it can be changed. The document
layer could always swap one feature's action for another and replay from
there, and the properties card drew a row per number for the features whose
actions are plain kernel commands — a hole's diameter, a rib's height, a
pattern's count. Two things were missing, and between them they meant that in
practice a finished extrusion could not be changed at all.

The first is that the card had no arm for the one action every extrusion
actually uses. A workbench extrusion is not a kernel command in history: it is
a late-bound `SketchRegionExtrusion`, resolved against the sketch as it then
stands. The scalar table matched `Kernel` and `TargetedKernel` and returned
nothing for anything else, so the card never appeared for an extrusion and a
typed change had nowhere to land.

The second is that a number is not the whole feature. An extrusion is a set of
regions, a direction, one or two sides, an operation and a face it may end at.
Those were picked in a 3D editor with a live preview and a drag handle, and
there was no way back into it. Changing your mind meant deleting the feature
and building it again, which loses everything downstream that depended on it.

## Decision

### The history's own right-click menu opens the editor

Each entry in the parametric history strip takes a secondary click, which
opens the same floating menu the viewport and the Browser already use. The
entry under the pointer is the subject, so an extrusion can be reopened
without first selecting it somewhere else. The menu offers Edit for a feature
that has an editor, and Suppress or Restore for any feature that is not
read-only; those two also have buttons in the same strip, and are repeated
here because a menu with one item reads as an accident.

Only a sketch-region extrusion has an editor to reopen. Anything else is
refused by name and changed in the properties card instead, because a menu
item that does nothing is worse than one that is not offered.

### Reopening rolls the history back, and everything else follows from that

Edit moves the history cursor to just before the feature. That is not a
gesture: it is what puts the viewport, the bodies and the reports into the
state the feature was built from, through the same rollback the slider uses.
The body on screen is then the body the extrusion swept into, rather than the
body it produced.

The sketch comes back from the recipe's own region set rather than from the
cached profile, because the recipe is what replay resolves, so it is what the
editor should show picked. The editor's fields are seeded from the recipe: the
signed distance, the draft, the second side and its symmetry, and the
operation, which is taken from the feature rather than inferred from the sign
so that the first drag cannot quietly flip it back.

A side that ended at a face reopens as a face pick when that face still
resolves through the reports of the features that built it. One that no longer
resolves reopens at the length it last measured and says so, rather than
presenting a face it cannot find.

### Confirming rewrites the feature; abandoning restores the model

Confirming does not run an extrusion command. The document already knows how
to replay a sketch-region recipe and how to carry the rebuild of every feature
after it, so an edit is a new recipe in the same slot: the feature's action is
replaced, the cursor returns to the end, and the branch rebuilds. The feature
count is unchanged, which is the property that distinguishes an edit from
building the thing again.

Abandoning has to put the whole model back. The rollback that made the editor
meaningful would otherwise leave the user looking at a design missing
everything from that feature onward, so cancelling restores the cursor as well
as clearing the operation.

### The properties card covers the recipe too

`action_scalars` reads a `SketchRegionExtrusion` as well as a kernel command.
The distance is offered as a plain length rather than as the signed quantity
the recipe stores: the sign is which way the sweep goes, which the feature
settled when it was made, and a field that refused a negative number would
otherwise refuse to shorten an extrusion that happens to point the other way.
A side that ends at a face is not offered at all, because its length is
measured again on every rebuild and a typed one would be overwritten by the
next replay rather than held.

The write is matched on the field's own name rather than on its index alone,
because which lengths a recipe offers depends on whether its sides end at
faces; keying both halves on the same name is what stops the two drifting
apart as the recipe grows.

## Consequences

- `PendingOperation::ExtrudeSketch` carries the feature being edited, so the
  preview, the drag handle, the panel and the confirmation gate are the ones
  the extrusion was made with rather than a second set that resembles them.
- Activating a committed sketch no longer requires the history cursor to be at
  the end. The guard moved to the caller that needs it; re-entering a feature's
  editor is the one case that must work with the cursor rolled back.
- An edit that would not validate as a recipe is refused before the document
  is touched, so a rejected change leaves the staged editor standing rather
  than half-applying.
- Features other than extrusions still have only their numbers. Reopening a
  hole or a pattern in an interactive editor is the same shape of work and is
  not done here.

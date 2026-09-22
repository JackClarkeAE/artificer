# ADR 0048: A construction plane is a feature

Status: accepted — planes are document features with a recipe, placed with a
live preview and handles, edited from the history, followed by the sketches
built on them, and usable wherever a face's plane is.

- Date: 2026-09-22
- Decision owners: Artificer project
- Extends: [0008](0008-plane-profile-workbench.md),
  [0014](0014-m5a-parametric-document-foundation.md),
  [0036](0036-editing-a-committed-feature.md),
  [0037](0037-where-a-side-of-an-extrusion-ends.md),
  [0041](0041-tools-and-selections-meet-in-either-order.md)

## Context

The Plane command could put a plane on one face, halfway between two parallel
faces, or on top of an origin or existing plane. That is where it stopped.

**A plane was a copy, not a definition.** The workbench kept each plane in the
workspace envelope as a fixed frame, with a marker feature in the history that
carried no geometry. The face it came from was a snapshot-local entity handle
that nothing ever looked at again. When the body under it changed, the plane
stayed where it was.

**It could not be placed anywhere useful.** There was no offset, so a plane 20
mm above a face, or 30 mm out along an origin axis, could not be made at all.
There was no angle, so a plane leaning off an edge could not be made either.
The command committed the moment it was confirmed, with nothing on screen to
say where the plane would go and nothing to move it with.

**Once made, it could not be changed.** The history chip offered Suppress, and
suppressing it did not even hide it. There was no Edit, no Rename, no Delete.

**It was not a reference.** A sketch on a plane copied the plane's frame and
forgot the plane, so moving the plane, had that been possible, would have
moved nothing. A side of an extrusion could end at a face's plane (ADR 0037)
but not at a construction plane, which is the one plane whose only purpose is
to be such a reference. Scripts had no planes at all beyond the three origin
planes.

## Decision

### The definition is the authority, and the frame is derived from it

A plane is a feature whose replay action is `ReplayAction::DatumPlane`. Its
recipe names what the plane is made from and how it sits relative to that:

| Base | Made from | What it adds |
|---|---|---|
| Origin | XY, YZ or XZ | offset along the normal |
| Face | one planar face, by persistent reference | offset along the face's outward normal |
| Plane | another construction plane, by feature | offset along that plane's normal |
| Midplane | two parallel planar faces | offset from the plane halfway between them |
| Edge | a straight edge and a planar face that holds it | an angle turned about the edge, measured from the face |
| Fixed | nothing; a frame | nothing |

Every base takes an offset and a flip; the edge base also takes the angle.
`Fixed` exists for planes whose origin cannot be named any more, which is
exactly the planes files written before this change carry (below).

The recipe also keeps the frame it last resolved to. That frame is a cache,
not a definition: it is what the plane shows while its base cannot be
resolved — the history rolled back past the face it sits on, say — and the
history strip marks the plane as standing on its last position when that
happens, the way ADR 0037 reports a side that kept its last length.

The arithmetic that turns a base frame, an offset, a flip and an angle into a
plane lives in the document crate (`artificer_model::datum`), where it has no
kernel to lean on and is tested on its own. What a face or an edge *is* at the
moment of replay is asked of the kernel by the workbench, through the same
persistent-reference resolution every face-targeted feature already uses.

### The plane follows its base, and what is built on the plane follows it

A plane on a face takes that face's body as an input and depends on the
feature that last built it. When that body is rebuilt, the plane is in the
rebuild plan, and at its step it is resolved again against the body as it now
stands. A plane on another plane depends on that plane.

A sketch on a plane records `SketchSupportRecipe::DatumPlane { plane }` and
depends on the plane feature. Its geometry is authored in the plane's own
coordinates, so replay reads the plane's current frame instead of the frame
the sketch was first drawn in, and the sketch — and every extrusion built
from it — moves rigidly with the plane. The workbench refreshes the cached
frames after the rebuild commits, so what is drawn and what was built agree.

### Placing a plane is an editor, not a click

The Plane command stages a plane on whatever is picked, in either order (ADR
0041): a face gives a plane on that face; an origin or construction plane
gives a plane on top of it; two parallel faces give their midplane; a
straight edge gives a plane through that edge, leaning from the planar face
beside it (or from the face picked with it). Nothing picked gives a plane on
the selected origin plane.

While staged, the plane is drawn where it will go: a translucent card on the
picked surface, sized to it, with its name. Two handles move it. An arrow
along the plane's normal drags the offset, exactly as the extrusion arrow
drags a distance; on an edge plane, an arc around the edge turns the angle.
The contextual card on the right carries the same values as typed fields in
the document's length unit — Offset, Angle, and Flip — beside a line saying
what the plane is built from. Dragging and typing drive one value. Enter or
the tick commits; Escape abandons and leaves the document untouched.

### A plane lives in the history like everything else

The plane's chip sits in the history strip in the order it was made. Its
right-click menu offers:

- **Edit this plane**, which rolls the history back to just before the plane
  and reopens the staging editor on the recipe's own values (ADR 0036).
  Confirming rewrites the recipe in the same slot and replays everything
  after it, so the sketches and features on the plane move with it.
- **Rename**, which is the feature's label; the Browser shows the same name.
- **Suppress** and **Restore**. A suppressed plane is not drawn, and the
  features that depend on it are skipped by the usual dependency rule rather
  than left standing on a plane that is not there.
- **Delete**, only when nothing depends on the plane. Otherwise it is refused
  by name — "Sketch 3 is drawn on Plane 2" — rather than deleting what the
  user built on it.

Visibility is part of the recipe, so hiding a plane is an edit the document
records and can undo, and saving is offered after it.

### A plane is a reference wherever a face's plane is

- **Sketch.** Sketch with a plane selected opens the sketch on it, whether or
  not other sketches exist. The earlier behaviour, where the Sketch button
  fell back to the origin plane once one sketch was committed, is removed.
- **Extrude.** A sketch on a plane extrudes as a new body, or adds to or cuts
  from a body exactly as an origin-plane sketch already can.
- **To face.** A side of an extrusion may end at a construction plane. ADR
  0037 already defines a side's end as the plane of a face; a construction
  plane is that plane with no face around it. The recipe records the plane by
  feature, and replay measures to its current frame.
- **Mirror** keeps using a selected plane, as before.
- **Scripts.** `plane(...)` builds a plane from a world frame, an origin
  plane and offset, a face and offset, two faces, or an edge, a face and an
  angle, and `sketch(on: …)` accepts it.

### Files written before this change

The native document schema moves to version 7. A version 6 file's plane
markers become `Fixed` planes carrying the frame the workspace envelope held
for them, with their names and visibility. The snapshot-local handles those
planes were made from cannot be turned into persistent references after the
fact, so they are not guessed at: a migrated plane stays where it was, and
Edit offers its offset. Sketches in those files keep the copied frame they
already had. Planes made from now on are associative from the start.

## Consequences

- The workspace envelope no longer owns planes. It still reads the old list,
  once, to migrate it.
- `FeatureKind::DatumPlane` carries a real recipe, so replay, save and load,
  undo and the digest all see it; a plane edit is an ordinary document edit.
- The model crate gains `remove_feature` for a feature nothing depends on. It
  is the first deletion the history supports, and it is deliberately that
  narrow.
- The viewport gains a staged-plane overlay with an offset arrow and an angle
  arc, reporting drags through `DocumentViewportOutput` beside the extrusion
  arrow's.
- An edge plane needs a straight edge and a planar face that contains it.
  Anything else is refused by name when it is staged, not when it is
  committed.

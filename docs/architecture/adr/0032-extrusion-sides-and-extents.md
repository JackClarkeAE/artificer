# An extrusion says what it does, how far, and where it stops

Status: Accepted and implemented (0.98.1)

## Context

The extrusion editor offered one signed distance and, on a sketch drawn on
a face, an Add/Cut/Auto switch. Three things it did not offer:

- A sketch drawn on a plane could only make a new body. Adding to or
  cutting the body it was drawn over meant extruding a second body and
  running a Boolean by hand, which is two operations and two history
  entries for what a user reads as one.
- An extrusion went one way from its plane. A part that is symmetric about
  its sketch plane had to be drawn on an offset plane, or built twice.
- Every side ended at a typed distance. "Up to that face" is how a
  drafter describes a boss that has to meet a wall, and it has to keep
  meeting that wall when the wall moves.

## Decision

### The operation is a first-class choice, and the sweep is the same either way

New body, Add and Cut stand side by side for any sketch that has a body to
combine with — a sketch on a face, or a sketch on a plane over a visible
body. A sketch on a face is an add or a cut to the kernel, as before, and
keeps the Auto switch that reads the operation from the sign.

A sketch on a plane is not: the kernel has no "cut with a profile in mid
air". The sweep makes a body of its own, and a Boolean folds it into the
target. That is two kernel steps and two features in history — the
extrusion, then the Boolean — which is exactly what the model already
replays, so nothing new has to be persisted to rebuild it. A refused
Boolean leaves the swept body standing and says so: the sweep is committed
either way, and the user can retry or undo rather than lose the work.

### A second side is one sweep from behind the plane

Two sides give each direction its own length, with a Symmetric lock for the
common case of equal ones. There is no second kernel command: the frame
moves back along its own normal by the second side's length and the depth
covers both sides, which is what `SketchRegionExtrusion` replays from
`second_distance`, so the preview, the commit and the rebuild all describe
the same slab. A draft has no single side to lean from, so a second side
clears it; a feature on a face grows from that face and has no second side
at all.

### A side may end at a face instead of at a distance

Each side chooses Distance or To face. To face arms a pick; the next face
clicked in the viewport is taken by that side and nothing else sees the
click. The face must be planar and parallel to the sketch plane, and lie on
that side of it; anything else is refused by name, and the side stays armed
so the next click is still a pick.

What is stored is the face — as a persistent reference, the same kind every
other feature uses to survive a rebuild — together with the length last
measured to it. Replay measures again against the body as it then stands,
which is what makes the feature follow the face rather than freeze the size
it first had. A face that has gone, or is no longer parallel, leaves the
stored length in place: the feature rebuilds at the size it last had rather
than failing the document, and says so.

The recipe's first-side distance keeps carrying its sign. A measurement is
a length; the sign is which way the sweep goes, and the two stay separate
so reversing an extrusion re-measures rather than inverts.

## Consequences

- `SketchRegionExtrusion` grows `second_distance`, `up_to_face` and
  `second_up_to_face`, all optional and all skipped when absent, so a
  document written before 0.98.1 reads unchanged and one written now stays
  readable by a build that ignores them — as a one-sided extrusion at the
  distance it last measured.
- Measuring a face needs the kernel, which the model crate does not depend
  on; the workbench measures and hands the recipe its lengths before the
  regions resolve. The model owns the geometry of the frame (moving it
  along its normal, and the height of a plane above it) and its tests.
- The feature preview draws both sides: the prism starts at the back
  offset rather than at the profile plane, and the drag arrow still starts
  on the plane the profile was drawn on, so the handle stays where the
  sketch is.

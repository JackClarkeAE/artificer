# ADR 0053: A part can be saved into the library, and placed at any values

Status: implemented — File ▸ Save to Part Library… (or the library's own
"Save current part…") saves the part being worked on, with its variables as
the values it takes, and the library places any part as many times as wanted,
each at its own values.

- Date: 2026-09-23
- Decision owners: Artificer project
- Extends: [0017](0017-portable-native-document-v4.md),
  [0018](0018-content-addressed-part-library-and-components.md),
  [0052](0052-an-extrusion-distance-can-follow-a-variable.md)

## Context

The library held one part, the built-in extrusion, published by the app
itself. There was no way to put your own part in it, and the library's
window, its validation and its insertion path were all written for that one
part and its one Length.

## Decision

### Saving

A saved part is the document it was drawn in, sealed into an immutable
catalog package (ADR 0018):

- **The embedded document.** Its media type is
  `application/vnd.artificer.saved-part+json`. It holds the document at the
  end of its history, without undo history, and names which body is the
  part.
- **Which body.** It is the only visible body, or the selected one when
  several are visible. With several and none selected, the save window says
  so and saves nothing.
- **Parameters.** Every variable that holds a plain length, angle or number
  becomes a parameter, unless the save window's box for it is cleared. Its
  default is the value it has now, and any range the variable has comes
  with it. A variable written as an expression over others is derived, so
  it is not offered.
- **Key and version.** The key is `user.` plus the name in lower case, with
  anything but letters and digits made a hyphen. Saving under a name the
  library already has adds the next major version of that part (v1.0.0,
  v2.0.0, …); the older versions stay in the store.
- **It must build.** Before it is published, the package is evaluated once
  at its defaults. A part that does not build is not saved.
- **Its picture.** The picture and rough size (ADR 0018, amended) are drawn
  when it is saved. An extent that moves when a length parameter moves is
  named after that parameter, so a bar extruded by `length` reads
  `4 × 2 mm × length`.

### Placing

Placing a saved part evaluates its document again at the given values. It
sets each exposed variable to its value, then runs the same replay that
opens a file (`hydrate_model_document`), so every feature that follows a
variable comes out at the new value. An extrusion typed as `length`
(ADR 0052) is the everyday case.

The component does not keep the part's document. It keeps what that replay
ran for the part's body: the chain of kernel commands, with every sketch
region, parameter and target resolved (`ReplayAction::KernelChain`, or
`Kernel` when the chain is one command). Replaying the chain from nothing
gives the same body, which is checked before the part is placed. A document
with placed parts therefore opens and rebuilds with no library present,
exactly as one with the built-in part always has.

Each placement is its own insertion intent with its own values and its own
component, so one part can be placed any number of times at different
values.

For now, a body that a Boolean combines from others, or one built on
another body's result, is refused with a message saying so. Its chain would
need a second branch.

### The library window

The window lists every part: the built-in under STANDARD COMPONENTS, and
the newest version of each saved part this build can read under MY PARTS.
Each row shows its picture, version, kind, rough size and category.

The selected part's card has one field per parameter:

- Lengths are read in the document unit or the unit typed.
- Angles are read in degrees or the unit typed.
- Numbers are read as numbers.

Each part keeps its own typed values. A field left empty takes its default.
A value outside its range is refused with the limit, in the part's own
terms.

## Consequences

- `PartInsertionEligibility` names the parameter that is wrong, rather than
  assuming the one Length.
- File-open replay records the kernel commands each feature ran
  (`HydratedFeature::commands`), which is how a saved part's chain is
  captured.
- A saved part's parameters are only as parametric as its features. Today
  that is extrusion distances (ADR 0052) and the built-in parameterised
  kernels. Sketch dimensions typed as variables are still read once, so a
  profile's own size does not change per placement yet.

# Sketch strokes and typed dimensions commit on acceptance

Status: Accepted and implemented

## Context

[ADR 0007](0007-universal-model-operation-confirmation.md) put every sketch
insertion behind one shared pending-operation gate. Four records state that
rule, in four places:

- 0007 line 15: "Every user-triggered operation that can execute a kernel
  command, replace model truth, or commit a workbench-owned modeling artifact
  enters one shared pending-operation state before execution."
- 0007 line 35: "Move, Rotate, and Scale previews use this contract, as do
  user-selected diagnostic cases, each point/line/rectangle/circle/arc sketch
  insertion, `Finish Sketch`, and all later interactive model-changing
  operations."
- [0008](0008-plane-profile-workbench.md) line 20: "Every completed entity
  gesture creates a provisional sketch edit and enters the universal
  confirmation gate from [ADR 0007]. The green tick or bare `Enter` commits it
  to the sketch revision; the red cancel action or `Escape` discards it."
- [0009](0009-live-sketch-dimensions.md) line 23: "An `Enter` consumed by a
  dimension editor may apply the value and stage the resulting sketch edit, but
  it may not also confirm the global operation in the same event. A subsequent
  bare `Enter` or the green tick commits the staged entity."

The tree already stopped doing this for strokes. `commit_sketch_stroke`
(apps/workbench/src/lib.rs) has published a completed drawing gesture directly
since sketching became fluent, and no record said so — undocumented drift that
this record now captures alongside the change it is paired with.

For typed dimensions the gate produced a visible defect. Windows testing
reported it: changing a rectangle's width drew the old rectangle in red beside
a new one in green and asked for a second confirmation, for an edit the user
considered already made. The red half came from the canvas painting the
retired original underneath the candidate; the second confirmation came from
promoting the keystroke to `PendingOperation::SketchEdit`.

## Decision

A sketch gesture that only authors sketch geometry — a drawn stroke, and a
typed value in the selected-feature parameter editor — commits at the point of
acceptance.

For a typed parameter:

- A keystroke stages one private candidate definition and nothing else. It
  costs no sketch revision, no entity identity, and no undo entry, and the
  shared confirmation rail is not shown for it.
- Acceptance is bare `Enter`, or the field losing the keyboard for any reason
  (clicking a different field, the canvas, a ribbon button, or anything else).
  Acceptance publishes exactly one sketch revision and one local undo step.
- `Escape` discards the candidate and restores every retained buffer to what
  the committed recipe says, including text that never parsed.
- Text that does not parse is never published. Accepting it reverts instead,
  because the last candidate that parsed is not what the field says and
  committing it silently would commit a number the user never saw.
- Because the candidate re-authors existing geometry rather than adding to it,
  the canvas paints it as the entity it will be — one shape, committed styling
  — and paints no red original underneath. Every other staged edit keeps its
  red retirement overlay, because there the removal is the point.

Acceptance is settled once per frame, before any panel renders, so the ribbon,
rail, browser, and canvas all read committed truth and the click that ended the
edit still lands on whatever it hit.

ADR 0021's formula is preserved exactly: one private candidate definition is
staged, bare `Enter` publishes it, `Escape` discards it. Only the rail's
visible tick and cross stop participating for these two gestures. A keystroke
still never both stages and confirms in one event, because acceptance is a
distinct later event.

Everything that reaches the kernel or the document keeps ADR 0007 unchanged:
`Finish Sketch`, extrusion, selected-face features, Booleans, transforms, and
library insertion.

## Verification

- `typed_dimension_applies_on_enter_with_no_confirmation_rail` — the reported
  case: one revision on `Enter`, no rail, no confirm button in the tree.
- `typed_dimension_applies_when_the_canvas_takes_the_click` — clicking away
  both commits the value and clears the selection in the same frame.
- `accepted_dimension_is_one_local_undo_step` — one accepted value, one Cmd+Z.
- `invalid_selected_parameter_keeps_last_preview_and_escape_reverts_neutrally`
  — unparseable text never publishes; `Escape` restores the committed text.
- `rectangle_recipe_edit_applies_on_accept_and_drives_exact_new_body_volume`
  — the accepted value drives an exact downstream body volume.
- `edited_extruded_sketch_rebuilds_in_place_and_escape_stays_neutral` — a
  reverted edit leaves the document, its snapshot, and its feature count alone.
- `in_place_parameter_preview_supersedes_only_its_own_original`
  (crates/sketch-ui) — a staged delete keeps its red retirement overlay.
- `workbench_typed_dimension_live_preview_1040.png` — the pixels: exactly one
  rectangle at the typed size while typing.

## Consequences

- The sketch rail shows Finish and Exit during a dimension edit rather than a
  tick and a cross; there is no operation for them to confirm.
- Any pointer press outside the field accepts, including a canvas pan or an
  orbit peek. That is the intended reading of "you moved on", but it does make
  the commit reachable from gestures a user may not think of as confirmation.
- Hit-testing during a live preview still uses committed geometry, so hovering
  a moved edge mid-typing can highlight where it used to be. The window is one
  acceptance long; routing picking through the pending presentation is a larger
  change than this defect warrants.
- Every keystroke replays the authoring graph from the edited operation
  onward. That was already true; instant apply makes it a per-keystroke cost on
  the largest sketch a user has. If the sketch frame budget regresses, debounce
  the staging, not the acceptance.

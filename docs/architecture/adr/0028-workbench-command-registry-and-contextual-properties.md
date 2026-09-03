# The workbench command registry and contextual properties

Status: Accepted and implemented; the workspace-switching half of the tab strip is superseded by [ADR 0030](0030-ribbon-tabs-are-views.md)

## Context

Half the workbench had a command model and half did not.

`SketchToolRegistry` has always been a table: 30 tool descriptors carrying
icon, accessible name, tooltips, shortcut, cursor, inputs, selection and
capability requirements, and a commit contract. The sketch ribbon's icon grid,
its split buttons and its group captions all fall out of those descriptors.

The model half had no such table. `model_command_groups` was 293 lines of
hand-written layout where every label, enablement predicate, tooltip and action
was a literal at the call site. Everything the model ribbon lacked followed
from that:

- No icon field to draw, so model commands were text where sketch commands were
  icons.
- One row for every command, so the row ate itself: Move, Rotate and Scale
  rendered as the single letters `M`, `R` and `S` because the shortcut key was
  the shortest label available.
- Ten commands — Revolve, Hole, Rib, Mirror, Pattern, Chamfer, Fillet, Combine,
  Subtract, Intersect — reachable only by opening a dropdown named
  `Features...` or `Boolean...`.

Separately, a 172 px properties column was reserved on every frame in every
workspace. On the frame a user spends the most time looking at — nothing
selected, nothing staged — two of its three sections were placeholders for
state that did not exist.

## Decision

### Commands are a table, and the ribbon renders it

Every model-workspace command is a row in `apps/workbench/src/commands.rs`
carrying where it lives (tab, group), how it is drawn (icon, size, label) and
what it is called (visible label, accessible name, tooltip, shortcut). The
ribbon is a renderer over that table and knows nothing about any individual
command.

Enablement stays out of the table. Whether a command can run is a question
about live state, so `command_availability` answers it — once per command,
returning either `Enabled` or `Disabled` with the reason in plain words. A
disabled control that cannot say why is a dead end.

The visible label and the accessible name are separate fields. Shortening text
on screen must not silently rename a control somebody has learned or scripted.

### Two taxonomy levels, and two button weights

Tab → group → command. Tabs are the level the workbench previously lacked, and
they are what stops the row eating itself. `Model` and `Sketch` were the
workspace — picking one entered it, and they kept the accessible names the
separate workspace buttons they replaced already had. `View` is a ribbon tab
only: it changes what is shown, never the workspace, and so stays reachable
while an operation is pending. [ADR 0030](0030-ribbon-tabs-are-views.md) makes
every tab work the way `View` does here, for the reason this record's own rule
predicts: one command cannot live on two tabs under one name, and Extrude did.

Exactly two button weights: a large icon with the name underneath for primary
commands, a small icon with the name beside it for secondary ones. One size for
everything says nothing about what matters. Every button is at least 24 px.

A tab with no commands is not created. An empty tab is worse than no tab, and
adding one is now a row in the table.

### The model workspace has no persistent properties panel

A pixel that is always occupied has to earn it on the frame where nothing is
happening. The properties column did not, so the model workspace opens without
it and the viewport runs to the right edge.

In its place, one contextual card appears when — and only when — there is a
subject to describe: a staged operation, an active component occurrence, a live
measurement, or a selection. It describes the one thing it is titled after.

The card floats over the viewport and never docks. A panel that appeared on
selection would reflow the model at the exact moment the user is pointing at
something. It sits between the underside of the view cube and a matching margin
at the bottom, clamped into the viewport so a long card is pushed up rather
than off the window taking its controls with it.

The commands it offers are `model_context_commands` — the same answer the
right-click menu gives — so a command cannot be offered in one place and
missing from the other. The two never show together, because two controls with
the same name on screen is a name that identifies nothing.

The operation's acceptance controls are the foot of the card, so the inputs an
operation needs and the tick that commits it are one surface. No bar exists
solely to hold a tick and a cross: the docked palette grows the same foot, and
the sketch workspace's Finish and Exit live there. Whichever right-hand surface
is showing owns the controls; the floating chip is the fallback for the frames
when none is.

**One surface owns the right-hand slot at a time.** The workbench had grown
three things that wanted it: the docked palette, this card, and a standalone
`EXTRUSION` window that existed only because the palette used to be closable.
That window carried a subset of the palette's extrusion controls in the same
place, so with the card over it, `Auto` was reachable from nowhere and clicks
aimed at `Cut` landed on the card — present in the accessibility tree, pressed
by the test, and swallowed.

The rule that follows: when two surfaces contend for one slot, ask first
whether one is a subset of the other. If it is, delete it rather than
arbitrating between them. The window is gone and the card carries the complete
controls under a `Feature` subject — a sketch ready to extrude, or a face that
can be pushed and pulled. Only genuinely different surfaces earn a suppression
entry.

**The card is suppressed whenever another surface owns the screen** — the
docked palette, the document dialog, the context menu, the Part Library — and
the confirmation controls fall back to a floating chip on those frames. This is
not belt and braces: the Part Library covers the bottom-right corner, and
without the fallback a staged insertion had a tick that could be seen and not
pressed.

### What this reverses in ADR 0011, and what it does not

[ADR 0011](0011-expandable-workbench-shell.md) made the confirmation rail a
shell invariant. Three of its clauses no longer describe the model workspace,
and the disagreement belongs here rather than in a reader's head:

- Line 27: "a fixed confirmation rail at the outer bottom edge." In the model
  workspace the rail is the foot of the contextual card, or a floating chip
  over the canvas when the card is suppressed. The sketch workspace keeps its
  docked rail, which is why the invariant still reads true there.
- Line 37: "the confirmation rail always reserves the same layout space,
  including when no operation is pending." The model rail stopped reserving
  idle space before this change — an always-present strip under the timeline
  with nothing in it read as a layout bug — and this record captures that drift
  as well as extending it.
- Line 44: "The rail is intentionally separate from operation-specific input."
  This one is reversed deliberately. The separation put an operation's inputs
  and the tick that commits them at opposite ends of the screen; they are one
  surface now, because they are one decision.

What ADR 0011 was protecting is unchanged. Every interactive modelling
operation still stages one shared pending intent, invalid intent still cannot
be confirmed, no panel-local button bypasses the central dispatcher, and
collapsing chrome still cannot cancel, execute or alter a staged operation —
which is exactly why the floating chip has to exist. Presentation of the gate
moves with the surface that owns the screen; the gate itself, from [ADR
0007](0007-universal-model-operation-confirmation.md), does not move at all.

### Everything else has one home

- **Document facts** — material, mass, units, parameters, navigation,
  diagnostics — live in File ▸ Document properties. They are true whether or
  not anything is selected.
- **Status** — the last transaction's outcome, the native-only claim, the
  preview caveat — lives in the viewport status chip, visible whichever surface
  is up. That chip floats over the model, so it grows sideways rather than down.
- **Commands** live on the ribbon. Display toggles and motion are View-tab
  commands, not panel checkboxes.
- **File actions** live in one File menu. They were previously split between
  the header and the document dialog, the same two actions under two names in
  two places, with exports filed under "properties".

The rule these follow: a fact appears in exactly one place, and that place is
decided by what the fact is about — the moment, the document, or the session.

### The sketch workspace keeps a docked palette, and this is not provisional

A contextual card cannot float over a sketch canvas. This was recorded as
"until the sketch inputs move onto the canvas" and then tested by building it,
which is how the real reason turned up: a floating panel eats the part of the
canvas it covers, and on a drawing surface that part is not spare.

Concretely, with the card in place at x 792–1028, a sketch click at (804, 408)
landed on the card rather than the sketch, and
`both_polygon_and_both_slot_variants_commit_atomic_closed_profiles` committed
16 entities where it should have committed 20.

The model viewport can afford a floating surface because the user points *at a
body* in it, and a body is somewhere specific. A sketch canvas is a drawing
surface everywhere, so the space a panel occupies has to be space the canvas
never had. Reserving it is exactly what guarantees that the canvas you can see
is the canvas you can draw on.

So the two workspaces are deliberately different, and their difference is a
property of what they are for rather than a stage on the way to consistency.
They hold separate palette-visibility flags for the same reason.

`ACTIVE TOOL`, `LIVE DIMENSIONS`, `SELECTED FEATURE` and `PROFILE DIAGNOSTICS`
are relevant on every frame while drawing, so the palette is not dead weight
there either.

### Themes are values, not constants

The palette was 18 `const Color32` values, which is a theme that cannot change.
It is a `Palette` struct with named themes now, read through accessors backed by
one atomic, so no call site knows which theme is active and a third theme is a
third value.

A theme is not an inversion of another. The dark palette keeps its neutrals
biased toward the accent, lifts the accent and state colours until they carry on
a dark ground, and keeps the viewport darker than the panels so the model still
reads as the lit surface.

Every theme is measured, not just the one that ships active.

## Verification

- `commands::tests::every_command_has_a_unique_stable_key`,
  `every_command_is_named_and_described`, `each_group_appears_once_per_tab`,
  `every_tab_has_commands` — the table's own invariants.
- `commands::tests::accessible_names_are_unambiguous_within_a_tab` — a name
  that appears twice in one tab is not a name.
- `commands::tests::every_solid_feature_and_boolean_is_on_the_surface` — the
  ten commands that used to be reachable only through an ellipsis.
- `confirmation_slot_preserves_viewport_geometry_at_the_supported_minimum_window`
  — walks every tab, not just the one that opens first, and holds every ribbon
  button to a 24 px hit target at 1040×700.
- `theme::tests::chrome_text_meets_wcag_aa_contrast_in_every_theme`,
  `state_colors_stay_legible_in_every_theme`,
  `every_theme_separates_its_chrome_surfaces` — a dark mode nobody has measured
  is how "supports dark mode" becomes "has a dark mode nobody can read".
- `theme::tests::choosing_a_theme_changes_what_every_accessor_returns`.
- `grounding_and_named_revolute_joint_share_the_confirmation_gate_and_persist`
  and `assembly_placement_and_joint_snapshots` — both found the Part Library
  occlusion, as a control that could be seen and not pressed.
- `canonical_cuboid_snapshot` — commit stays visually stationary over the model
  region, which is what caught the status chip growing over the geometry.
- `repeated_face_add_cut_add_chain_uses_the_ribbon_and_global_confirmation` —
  counts the solid rather than checking a button exists, which is how a covered
  control was caught: it committed 25.25 mm³ where the chain should have given
  24.875, because `Cut` and its distance had been clicked into a surface that
  was underneath another one.
- `face_operation_override_preserves_direction_and_auto_restores_sign_inference`
  — the `Auto` override, which the standalone window never carried.
- `both_polygon_and_both_slot_variants_commit_atomic_closed_profiles` and
  `both_arc_variants_remain_exact_open_profile_curves` — sketch clicks reach the
  canvas. These are what refuted a floating card in the sketch workspace, by
  counting the entities a covered canvas failed to commit.
- `workbench_ribbon_model_tab_1280.png`, `workbench_ribbon_view_tab_1280.png`,
  `workbench_ribbon_sketch_tab_1280.png`, `workbench_ribbon_dark_theme_1280.png`
  — every tab and both themes under pixel review.

## Consequences

- The ribbon is taller than the single row it replaced: two levels of structure
  cost vertical space, and the reference this was measured against pays the
  same. The viewport gets that back and more from the removed palette.
- Adding a tab, a group or a command is a row in a table. Adding a theme is a
  value. Neither is a layout change.
- A user who wants everything at once still has it: Properties opens the docked
  palette, holding the full stack.
- Mass properties are no longer visible while orbiting without the document
  dialog open. A pinnable card would answer that, but "pin" is how a contextual
  panel grows back into a permanent one, so it should be added only if the need
  proves real.
- Contextual surfaces are discovered rather than taught. A panel that is always
  there teaches itself; a card that appears on selection does not, so the File ▸
  Document properties route has to stay findable.

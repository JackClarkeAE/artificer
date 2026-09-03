# Ribbon tabs choose a view, and the sketch grid has three blocks

Status: Accepted and implemented

## Context

[ADR 0028](0028-workbench-command-registry-and-contextual-properties.md) gave
the workbench a command table and a tab strip, and made Model and Sketch do two
jobs at once: they named a branch of the command taxonomy *and* they entered a
workspace. That was defensible while the two branches were disjoint. It stopped
being defensible the moment a command was wanted from both sides.

Extrude was that command. It appeared twice in the registry — `model.extrude`
on the Model tab and `sketch.extrude` on the Sketch tab — under one name,
because a sketch is the thing Extrude most often consumes and the Sketch tab
had to be able to reach it. Two rows for one command is a duplication the table
exists to prevent: `command_availability` already enumerated both paths, the
two rows shared an accessible name, and a `SOLID` group existed on the Sketch
tab solely to hold the copy.

Deleting the copy was not possible while a tab changed the workspace. Pressing
Model from inside a sketch called `enter_model_mode`, which left the sketch
workspace; the sketch survived, but the canvas the extrusion was about to
consume was no longer on screen and the creation draft was cleared. So the only
route to a model command from a sketch was to leave the sketch first, and the
duplicate row existed to avoid making anyone do that.

The sketch tool grid had a second, unrelated pressure. It is three rows of
80 px tiles inside a 76 px ribbon content box, and its width budget is what the
1040 × 700 minimum window leaves after the other groups on the Sketch tab. Two
blocks — twelve drawing families, a divider, eleven constraints — fitted, and
there was no room for a third. Meanwhile `Pattern` sat at the end of the
drawing block making a promise it does not keep: every other tile in that block
draws or reshapes one piece of geometry, and Pattern repeats geometry that
already exists.

## Decision

### A tab chooses what the ribbon shows, and nothing else

`ribbon_tab_strip` sets `ribbon_tab` and never calls `enter_sketch_mode` or
`enter_model_mode`. Entering and leaving the sketch workspace is what
`Create ▸ Sketch` and `Complete ▸ Finish`/`Exit` are for — commands, in the
groups a user already looks in, with the enablement reasons the table already
carries.

Three things follow.

- **Extrude is one row again**, on the Model tab, in the `SOLID` group with
  Revolve. From inside a sketch it is one click away: Model tab, Extrude, and
  the sketch is still open behind it. `RibbonGroupId::SketchSolid` is deleted.
- **The Sketch tab is contextual.** Its twelve drawing families and its
  Finish/Exit pair all act on an open sketch, so it is offered only while one
  is open — `RibbonTab::needs_a_sketch`, filtered in `available_ribbon_tabs`. A
  tab full of controls that cannot do anything is worse than no tab.
- **Every tab is always enabled**, including while an operation is pending.
  Looking at a different set of commands was never the thing that needed
  gating; changing workspaces was, and a tab no longer does that.

A tab pick is scoped to the workspace it was made in. `ribbon_tab` holds
`(WorkbenchMode, RibbonTab)` and is cleared when the mode moves, so reopening a
sketch shows the sketch tools whatever was last looked at.

Model and Sketch are named `"Model ribbon tab"` and `"Sketch ribbon tab"` like
the other three. ADR 0028 kept them as `"Model mode"` and `"Sketch mode"`
because they were the same control the workspace buttons had been, and the name
said what they did. They no longer do it, so the name changes with the
behaviour rather than outliving it.

### The sketch grid is three blocks, not two

The width Extrude gave back — a 62 px large button, its separator, and the
group's spacing — pays for a third divided column. The grid is now:

| Block | Columns | Contents |
|---|---|---|
| Drawing | 4 | Select, Point, Line, Rectangle, Circle, Arc, Polygon, Slot, Trim, Fillet, Chamfer, Offset |
| Generators | 1 | Pattern |
| Constraints | 4 | The eleven relation and dimension tiles |

The generator block is its own promise: what takes geometry that already exists
and makes more of it, unchanged, somewhere else. Pattern is alone in it today,
with room for the mirror and circular arrays that belong beside it.

Offset takes the cell Pattern left, at the end of the drawing block's last row,
beside Trim, Fillet and Chamfer. It belongs with them: it reads a chain that is
already drawn and produces another chain from it, which is the same kind of
work, and not the same kind as repeating a shape wholesale.

Nine columns of the previous 84 px tile did not fit the minimum window, so a
tile is 80 px and a block divider is 8. `TILE_LABEL_ROOM` and
`CONSTRAINT_LABEL_ROOM` still clear the longest label on each side —
`Rectangle` at 40 px with a chooser, `Perpendicular` at 56 px without one — and
both the compile-time assertions beside the constants and
`every_tile_label_fits_the_column_the_tile_geometry_leaves_it` still hold.

Offset's registry entry — descriptor, icon, phases, inputs, the `O` shortcut
Fusion uses — ships with this change; its engine does not. The tile says so
rather than lighting up and doing nothing, and the work it names is specified
in
[the sketch offset plan](../geometry-kernel/sketch-offset-plan.md).

## Verification

- `the_model_tab_is_readable_from_inside_a_sketch_without_leaving_it`
  (`apps/workbench/tests/sketch_ui.rs`) covers the whole decision: the Sketch
  tab is absent with no sketch open; reading the Model tab from a sketch leaves
  the workspace, the canvas and the kernel untouched and finds Extrude enabled;
  the sketch tools are the other tab's and are gone while it shows; Exit is the
  Sketch tab's own command, not a side effect of a tab; and reopening a sketch
  shows its own tools whatever was looked at last.
- `expanded_compact_ribbon_is_unclipped_at_1040_by_700`
  (`apps/workbench/tests/sketch_compact_toolbar_ui.rs`) and
  `compact_toolbar_is_three_blocks_of_named_tiles_parted_by_dividers`
  (`apps/workbench/tests/sketch_toolbar_ui.rs`) hold every tile of all three
  blocks to its row, its column origin and the minimum window.
- `minimum_window_keeps_critical_sketch_controls_visible_and_canvas_fixed`
  measures the two tiles at the far end of the grid — the last drawing column
  and the generator column beyond the divider — rather than Extrude, which is
  no longer on that tab.
- The workbench snapshot suites are regenerated: the tab strip is one tab
  shorter in the model workspace, and the sketch ribbon carries the three-block
  grid.

## Consequences

Reaching a model command from a sketch costs one click and gives up nothing.
The reverse — reaching sketch tools without a sketch — is not offered, because
there is nothing for them to act on.

A user who expects the tab strip to be how they leave a sketch has to learn
Finish and Exit. They are two large buttons at the head of the Sketch tab's
`COMPLETE` group, they were always the published way to end a sketch, and the
tab never said which of the two it was doing.

Every test that entered a sketch by clicking the tab now clicks the command
that opens one, and its accessible name is state-dependent by design
(`Create sketch`, `New sketch`, `Edit sketch`, `Sketch on selected face`). That
is a sharper coupling than a fixed tab label, and it is the same coupling every
other command in the table already has.

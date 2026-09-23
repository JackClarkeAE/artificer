# ADR 0052: An extrusion distance can follow a variable

Status: implemented — an extrusion's distance typed as an expression over
document variables stays linked to them, and changing a variable rebuilds
the extrusion.

- Date: 2026-09-23
- Decision owners: Artificer project
- Extends: [0014](0014-m5a-parametric-document-foundation.md),
  [0017](0017-portable-native-document-v4.md),
  [0021](0021-editable-sketch-authoring-and-region-replay.md)

## Context

Document variables could be typed by name into any numeric field, but only
as a way of entering a number. The field kept what the expression came to,
so changing the variable afterwards moved nothing. The only geometry that
followed a variable was the built-in library extrusion, whose kernel command
is a parameterised template. That left no way to author a part whose size is
set by a variable, which is what a parametric library part needs.

## Decision

### The recipe carries the expression

`SketchRegionExtrusion` gains `distance_expression`, an optional
`ParameterExpression` over document variables (`length`, `depth * 2 + 5mm`).
When it is present:

- **Replay.** Replay evaluates the expression against the document's
  evaluated variables and uses its value, sign and all, as the first side's
  distance. `distance` holds what it last came to, so the recipe still reads
  sensibly on its own.
- **Inputs.** The feature lists exactly the variables the expression reads
  as its parameter inputs. Changing one of them marks the extrusion for
  rebuilding, and none can be deleted while the extrusion reads it. The
  model refuses a feature whose declared inputs differ from what its
  expression reads.
- **Type.** The expression must come to a length. An angle or a bare number
  is refused when the feature is made, not when it is replayed.
- **Ended sides.** A side that ends at a face or a plane has no distance to
  follow, so a recipe with both is refused.

The second side stays a plain number. Sketch dimensions were unchanged by
this record: a variable typed into one was still read once. ADR 0054 has
since linked them too, by having the sketch keep the entry and regenerate
its geometry when a variable changes.

### The editor records the link

In the extrusion card, the distance field reads its text with the shared
expression grammar (see the model's `parse_parameter_entry`). When the text
names variables, the editor keeps the parsed expression beside the value it
came to, and the card says "Follows length". The link holds only while the
distance is still that value: dragging the handle or typing a plain number
lets it go. Reopening a linked extrusion to edit it shows the expression
again, so confirming the edit keeps the link.

### Files

The native document schema moves to version 9, so an older build refuses a
file with a linked distance rather than silently freezing it. Version 9 also
adds the kernel chain that a saved library part replays as (ADR 0053).

## Consequences

- Replay decides whether to evaluate the variables from what the actions
  read (`ReplayAction::reads_parameters`), not from whether any action is a
  parameterised kernel template.
- `ModelDocument::replace_feature_recipe` rewrites a feature's action, its
  feature inputs, and its parameter inputs together, which is what an edit
  that adds or drops a link needs.
- The built-in part's revision follows the schema (ADR 0018, Fix 1), so it
  becomes 1.9.0; earlier revisions stay in a store beside it.

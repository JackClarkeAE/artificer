# ADR 0054: A sketch dimension can follow a variable

Status: implemented — a sketch value typed as an expression over document
variables stays linked to them. This covers a recipe field (a rectangle's
width, a circle's diameter) and a dimension drawn between two objects.
Changing a variable reshapes every sketch that follows it and rebuilds what
was made from those sketches, and a library part places at the sketch sizes
it is given.

- Date: 2026-09-23
- Decision owners: Artificer project
- Extends: [0021](0021-editable-sketch-authoring-and-region-replay.md),
  [0052](0052-an-extrusion-distance-can-follow-a-variable.md),
  [0053](0053-a-part-can-be-saved-into-the-library.md)

## Context

After ADR 0052 an extrusion distance could follow a variable, but a sketch
dimension could not. Typing `depth * 2` into a rectangle's width box worked
the expression out once, and the recipe kept the number it came to. That was
a copy: change `depth` and the rectangle kept its old width. In practice most
of a part's size is set in its sketch, so a saved library part could vary
its length but not its width.

A sketch recipe can already name a model input (`SketchValue::Input`), but
that path supplies only one number per input. It does not do arithmetic,
does not know about units, and the sketch UI never fills it. It also cannot
express `depth / 2 + 5mm`.

## Decision

### The sketch keeps the entry beside the number

`SketchDefinition` gains `value_links`: a list of `SketchValueLink`s, one per
value that follows the variables. Each link records a target and the entry
the value was typed as. The target is one of:

- **a recipe field** (`SketchValueTarget::RecipeField`): an operation and a
  recipe field's key (`width`, `diameter`, `angle`, …);
- **a relation** (`SketchValueTarget::Relation`): the measurement of a
  dimension drawn between objects, which the sketch holds as a relation
  rather than a recipe number. This covers a distance between two points,
  from a point to an edge or to its midpoint, and between two parallel
  edges.

The list is ordered by target, with one link per value.

- **The recipe still holds the number.** The recipe is unchanged: it holds
  the number the entry came to, so a sketch replays exactly without any
  variables in reach.
- **The entry is written with its units.** An entry is stored as
  `expression::written_entry` returns it. Every number that took the
  field's unit is written with that unit: `w + 5` typed in an inch
  document is kept as `w + 5in`. A kept entry therefore means the same
  whatever unit the document is later shown in.
- **Links belong to edits.** A link is set inside the transaction that
  typed its value (`SketchTransaction::set_value_link`). It is confirmed,
  cancelled and undone with that value, and the journal's snapshots carry
  it.
- **A link retires with its value.** Committing drops the links of
  operations that are no longer active, and removing a relation removes its
  link.
- **Links are validated.** A link must be on a value the sketch has: an
  operation it holds, or a relation that holds a measurement. It must read
  and must name at least one variable. Validation also bounds how many links
  a sketch has and how long each one is.
- **A link can change on its own.** Typing an entry that comes to the value
  already held changes nothing in the geometry, but still links it
  (`stage_value_link`).

The sketch never evaluates a link. It keeps each link with its value,
through edits, undo and saving.

### The canvas records, shows and follows links

- **Recording.** When a recipe field's text names a variable, the field
  keeps the written entry. This applies to the Properties field and to the
  Dimension tool's box on committed geometry. Every field of the editor
  writes its link into the replacement transaction, so editing a second
  field does not drop the first field's link. A plain number clears the
  link.
- **Showing.** A linked field shows the entry rather than the number. The
  panel adds that the value follows its variables and that a plain number
  unlinks it.
- **Distances between objects.** A dimension drawn between two objects,
  typed over a variable, keeps the entry the same way. Its box reopens on
  the entry, and the plate shows the entry under the value.
- **Links typed while drawing.** A dimension box typed over a variable while
  drawing becomes a link on the operation the draft inserts. A box and a
  recipe field do not always hold the same number. For example, a two-point
  rectangle drawn leftwards keeps a negative width behind a box that shows
  its size. So an entry is linked only where the field holds exactly what
  the entry comes to, or its negation, which is then kept negated.
- **Following.** `regenerate_linked_values(authoring, names, keep_connected)`
  works every link out again. For each linked operation it:
  1. rebuilds the recipe with the same editor a typed value uses;
  2. restages it through the same replacement, carrying joined neighbours
     when that preference is on;
  3. commits the result.

  Relations are solved over what the recipes place, so relation links
  follow after the recipe links. Each one is restated the way retyping its
  dimension would restate it, holding the end it is measured from.

  The outcome is a sketch the canvas itself could have produced. If a link
  no longer reads, names a variable that is not there, or comes to a value
  its field refuses, the call returns an error naming the field and changes
  nothing.
- **The live canvas.** The canvas follows the named values it is given
  whenever they change, and again after a local undo or redo, which restores
  values from before. The selection stays on the operation it was on.
  While a sketch is open, Undo steps back through the sketch's own edits,
  not through the document's variable changes, just as it did before.

### The document knows what a sketch reads

- **Parameter inputs.** A sketch feature's parameter inputs are derived
  from the variables its links name, when it is appended and whenever its
  payload is replaced. Because of this:
  - changing one of those variables marks the sketch and everything
    downstream for rebuilding;
  - a variable a sketch follows cannot be deleted.
- **Loading.** A file is refused when a sketch follows a variable the
  document does not have, or when a sketch declares inputs that are not the
  ones its links name.
- **Renaming.** Links name variables, so renaming a variable renames it in
  every sketch revision that follows it.
- **Undo.** `ModelDocument::follow_variables_in_sketch` replaces a sketch's
  payload as part of the variable change that caused it. It adds no undo
  step of its own, so one undo takes back the variable and every sketch
  that followed it.

### The workbench makes them follow

When a variable change is confirmed, the workbench:

1. works every linked sketch out again at the new values
   (`sketch_links::follow_variables`);
2. writes the result into the document;
3. rebuilds from the first feature marked for rebuilding.

A region extrusion resolves its regions from the sketch's current payload,
so the extruded body takes the new size. The payload's profile is recompiled
from the same regions: a region keeps its signature when its sides move.

If any sketch cannot take the new value, the whole change is abandoned with
`ModelDocument::abandon_last_edit`. The document is left exactly as it was,
nothing is left to redo, and the refusal gives the reason.

A saved library part runs the same step after the values of a placement are
bound and before it is evaluated. A sketch dimension is therefore as much a
part's parameter as an extrusion distance.

### Files

Links are saved with the sketch they belong to. The native document stays at
version 9. That version has not been released, and it already exists so that
a build which cannot follow a linked value refuses the file rather than
freezing the value (ADR 0052). Sketch links join it.

## Consequences

- ADR 0052's note that a variable typed into a sketch dimension is read once
  no longer holds. The Variables panel now says that sketch dimensions and
  extrusion distances both follow their variables.
- The shared expression grammar gained the pieces links need: printing an
  entry back with the fewest parentheses, writing units in, listing names,
  and renaming. The model and the sketch still read entries with the same
  grammar.
- A relation that moves its far end past what that end's own geometry can
  take (a line folded to nothing) cannot rewrite the geometry it pulls. The
  solver then places both ends, as it already did for a typed value.
- A value that follows a variable can still be retyped at any time. Typing
  a number unlinks it, and typing another expression relinks it.

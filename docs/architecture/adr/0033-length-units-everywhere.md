# The document's length unit is what every field reads and every readout shows

Status: Accepted and implemented (0.98.1)

## Context

The document properties offered a length unit since 0.6, and the measure
panel honoured it. Nothing else did: the extrusion distance, the fillet
radius, the sketch dimension boxes, the tool fields, the part library's
length, the interference readouts and the volume rows were all millimetres
whatever the setting said, and the help text under the picker said so. A
person working in inches typed `25.4` where they meant `1` and read
`25.400 mm` where they wanted `1.000 in`. Scientific notation was accepted
by the script language and the parametric expressions but not by the
fields of the main application.

## Decision

### One type does every conversion

`artificer_ui_core::units::LengthUnit` is the unit a person reads and types
in: micrometre, millimetre, centimetre, metre, inch and foot, with exact
factors (25.4 mm to the inch). It formats a millimetre value in the unit
(`format`, trimmed to the unit's precision; `format_readout`, fixed-width
for live values; `format_area`, `format_volume`), and it reads typed text
back to millimetres (`parse`): a number in plain or exponent form (`12.5`,
`1e-3`, `2.5E+2`), optionally followed by a unit symbol. Without a symbol
the number is in the field's unit; with one it is in that unit, so `10mm`
means the same in an inch document as in a millimetre one. `parse_entry`
adds a second step for fields that take arithmetic: what is not a number
is handed to the caller's evaluator and its answer is taken to be in the
field's unit, like a bare number. The type also builds an egui `DragValue`
or `Slider` over a millimetre quantity that shows and reads the unit, so
a numeric widget is unit-aware in one call and the underlying value never
leaves millimetres.

The workbench's `DisplayLengthUnit` is this type under its old name; it
serialises in the snake-case spelling workspace files have always carried,
so no file changes meaning.

### The document owns the unit, and a preference seeds new documents

The unit is document state, saved in the workspace file as before. A
second setting, kept with the user preferences beside the navigation
profile, names the unit a new document opens in; the document properties
show both, the document's own in the picker and the preference behind a
"Use for new documents" button, so a person who always works in inches
sets it once. A file from before the preference existed means
millimetres, as it always did.

### Every field and readout follows it

Once a frame the workbench hands the unit to the sketch canvas and the
part library, and formats its own readouts with it: the measure panel and
the measurement annotations, the mass properties, the edge-finish
distance, the extrusion distance, the feature editor's lengths, the
projected-context depth and the section offset, the sketch panel's live
dimensions and grid step, the interference study and clearance sweep
readouts and status lines. The sketch canvas shows its dimension boxes in
the unit (`W 1.575 in`, still `W 40.00 mm` at two decimals in millimetres,
so the millimetre baselines are unchanged), opens a box on the value in
the unit, reads a typed dimension in it, and re-renders every retained
tool field and recipe parameter when the unit changes, so a `40` typed as
millimetres does not sit there reading as forty inches. Document
variables are published to the canvas with lengths in the unit, so that
`plate_width / 2` means what `40 / 2` does beside it; angles stay
degrees.

The rules of what a field accepts are unchanged: a length that must be
positive, a count that must be whole, a coordinate within range, are
judged in millimetres after the reading, exactly as before.

## Consequences

- The kernel, the files and the interchange formats never see a unit;
  every conversion is in one type with its own tests, and nothing in the
  workbench multiplies by 25.4.
- A typed suffix always wins, which is the behaviour every mainstream
  package has and the only one that lets a person paste a millimetre
  value into an inch document without arithmetic.
- The dimension readouts keep fixed decimals so a live value does not
  change width as it moves; the panel readouts trim zeros, as the
  measure panel always has.
- Script Studio and the scripting language are unchanged: the language's
  own unit suffixes already decide what a number means there.

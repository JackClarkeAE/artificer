# ADR 0040: A part has a colour, and the colour travels

Status: implemented — a body can be given a colour in the Assembly tab, it is
saved with the document, and it leaves in a STEP export as the presentation
style AP214 has for it.

- Date: 2026-09-17
- Decision owners: Artificer project

## Context

Materials already carried a colour. That was enough to tell steel from brass
and not enough for the thing people actually want, which is to tell *this* part
from *that* one — two brackets of the same aluminium, in an assembly, shaded
identically. Colour as a consequence of material cannot say that, because the
two parts are made of the same thing.

A colour that only lives in the viewport is also half a feature. The reason to
colour parts is to hand the model to someone else and have them see what you
saw, and every other CAD system reads colour out of STEP.

## Decision

### A colour is its own property, and it outranks the material's

A body carries `Option<[u8; 3]>` of its own. The colour it is shaded with is
that if it has one, its material's if it does not, and the viewport's default
if it has neither. Clearing a body's own colour falls back to the material
rather than to nothing, so taking a choice back leaves the body looking like
what it is made of.

The two are separate because they answer different questions. A material says
what a body is made of and its colour is a consequence; a colour says how this
one body should look, whatever it is made of.

### Bytes, not keys

Materials persist by stable key so the library can grow without invalidating
saved documents. A colour has no library to be a key into, so it persists as
the three sRGB bytes a picker produces. That also means every colour a document
names is restorable whatever this build's material library holds.

### The picker is a group, not a command

The Assembly tab's COLOUR group carries no commands: the group *is* the picker,
the way the Boolean group becomes its operand panel while one is staged. A
colour is a value you set, not an action you invoke, and giving it a button
that opens a dialog would add a click to say nothing.

The swatch shows what the body is actually shaded with, so opening the picker
starts from what is on screen rather than from black.

### The colour leaves in the file

STEP carries it as AP214's presentation style, which is the chain other CAD
reads:

```
COLOUR_RGB → FILL_AREA_STYLE_COLOUR → FILL_AREA_STYLE
           → SURFACE_STYLE_FILL_AREA → SURFACE_SIDE_STYLE
           → SURFACE_STYLE_USAGE(.BOTH.) → PRESENTATION_STYLE_ASSIGNMENT
           → STYLED_ITEM(solid)
```

with one `MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION` gathering
every styled item in the same geometric context as the shapes it styles. The
styled item names the solid it colours, so a file with several bodies colours
each one rather than all of them.

`.BOTH.` is deliberate: a cavity's wall is as much the body's colour as its
outside is.

## Consequences and limits

**An uncoloured body writes no style at all.** Not a default grey — nothing.
The receiving system keeps its own default rather than being told a colour
nobody chose, and that is gated by a test rather than left to chance.

**Colour is per body, not per face.** STEP can style a face as readily as a
solid, and the chain above would carry it, but nothing in the workbench can
select a face and mean it as an appearance rather than as an operand. When
something can, this is the layer it extends.

**It is opaque.** There is no transparency, because the viewport's shading does
not have it either and a value that persisted and exported but did not draw
would be worse than its absence.

**No material is inferred from an imported colour.** Reading STEP back is not
in this record; when it is, a colour arriving without a material must stay a
colour, or a body would start quoting a mass from a guess about its shade.

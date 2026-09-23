# ADR 0051: A loft is a feature

Status: implemented in the workbench — the Loft command stages a loft
through profiles picked in sketches on different planes, previews the solid
the kernel builds, and commits it as a history feature that follows its
sketches and their planes.

- Date: 2026-09-23
- Decision owners: Artificer project
- Extends: [0036](0036-editing-a-committed-feature.md),
  [0041](0041-tools-and-selections-meet-in-either-order.md),
  [0048](0048-a-construction-plane-is-a-feature.md),
  [0049](0049-ruled-and-spline-surfaces-enter-the-vocabulary.md),
  [0050](0050-b-spline-curves-and-surfaces-as-carriers.md)

## Context

ADR 0049 gave the kernel a loft between planar sections on any two planes,
with ruled walls where no plane, cylinder or cone is exact, and gave scripts
`loft(sections: …)`. ADR 0048 made construction planes features that
sketches can be drawn on. Between them, the pieces a person needs to loft a
square up to a circle all existed, and none of them could be reached from the
workbench: the only thing called a loft there was the drafted extrusion,
which lofts a profile to its own offset.

## Decision

### The recipe names sketches and regions, never geometry

A loft is a feature of kind `Loft` whose replay action is
`ReplayAction::SketchLoft`. Its recipe is an ordered list of sections and an
operation:

- **A section** is a sketch and the signatures of the regions it takes from
  that sketch — exactly what an extrusion stores for its profile. Each
  section comes from a sketch of its own, because two sections in one sketch
  would lie in one plane.
- **The operation** is New body, Add or Cut. An add or a cut names the body it
  changes as an input, which is its branch; a new body creates one.

Replay compiles every section from its sketch as the sketch now stands and
places it on the sketch's plane as the plane now stands — for a sketch on a
construction plane, the frame the rebuild has just resolved for that plane.
It then runs one `KernelCommand::LoftPlanarSections`. Region compilation is
the one extrusions use (`compile_sketch_regions`), so a region that has gone,
or that now names two cells, fails a loft exactly as it fails an extrusion.

The recipe allows as many sections as the protocol carries. How many the
kernel can loft through is the kernel's to say: two sections loft with
ruled walls (ADR 0049), and three or more loft smoothly on B-spline walls
(ADR 0050), with no change to the recipe either way.

### It follows what it was built from

The feature takes every section's sketch as an input, so it depends on the
sketches, and each sketch on a construction plane depends on its plane.
Moving a plane from the history rebuilds the sketch on it and then the loft;
editing a sketch's geometry rebuilds the loft through the same path.

### Placing a loft is an editor

Loft opens an editor that works in either order (ADR 0041): a region of a
committed sketch that is already picked becomes the first section. While it
is open, a click on a sketch region in the model view is a pick:

- a region of a sketch that is not a section yet adds a section at the end;
- a region of a sketch that already is one replaces that section's regions,
  and Shift adds a region to it or takes one out.

The card lists the sections in order, each with a remove button and an
Earlier button, and offers New body, Add and Cut. Every change asks the
kernel for the result at once. The viewport draws that result: a new body is
shaded beside the others in a colour no material uses, and an add or a cut is
drawn in place of the body it changes. When there is no result the card says
why, with the kernel's refusal code — sections in one plane are
`LOFT_SECTIONS_COPLANAR`, not a silent nothing — and confirming builds
nothing and leaves the editor open. When the result came from the faceted
tier, the card's title says so.

Confirmed, the loft is a chip in the history. Its sections are spent the way
an extruded profile is: hidden, and shown again when the loft is suppressed
or undone.

### It is edited in place

The chip's menu offers **Edit this loft**, which rolls the history back to
just before the loft (ADR 0036) and reopens the editor on its own sections
and operation. Confirming rewrites the recipe and inputs in the same slot and
replays what follows. An edit keeps the body the loft builds or changes;
turning a new body into a cut is a different feature, and is refused rather
than rewritten under the same name. Rename and Suppress work as for any
feature.

### Files

The native document schema moves to version 8, the first that can hold a
loft. Nothing earlier needs migrating.

## Consequences

- `FeatureKind::consumes_sketches` names the feature kinds that spend a
  sketch (extrude, add, cut and loft), so the auto-hide rule and the archive
  validator agree on one list.
- `sketch_region_at` turns a point in a sketch's own coordinates into the
  region that holds it, which is how a pick in the model view becomes a
  section.
- The Browser names a sketch on a construction plane by the plane's name
  rather than its feature number.
- What the loft can build is exactly what the kernel's loft can build:
  sections on different planes drawn with lines, arcs, circles and splines,
  holes that pair, ruled walls between two sections and smooth B-spline
  walls through three or more (ADR 0050).

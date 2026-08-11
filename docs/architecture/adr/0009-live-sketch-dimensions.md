# ADR 0009: Live sketch dimensions are editable construction intent

## Status

Accepted.

## Context

The plane/profile workbench needs useful dimensional feedback before the later constrained-sketch solver exists. A measurement painted after placement is not enough: while drawing, users need to see exact values, move between them with the keyboard, type a replacement, and still retain the universal model-operation confirmation contract.

Treating these values as permanent algebraic constraints would overstate the current document model. The workbench has neither a constraint graph nor regeneration yet.

## Decision

Every sketch primitive exposes live measurements in plane units while it is being created:

- a point exposes its two plane coordinates;
- a line exposes length and angle, with coordinate deltas derived;
- a rectangle exposes width and height;
- a circle exposes diameter;
- an arc exposes radius and counter-clockwise sweep.

The measurements are drawn next to the geometry using fixed overlays that do not participate in workbench layout. The active value is a real accessible numeric editor. `Tab` and `Shift+Tab` apply the current valid text and cycle editable values. Pointer motion continues to drive unlocked values; accepted numeric values lock their corresponding degree of freedom for the remainder of the gesture.

An `Enter` consumed by a dimension editor may apply the value and stage the resulting sketch edit, but it may not also confirm the global operation in the same event. A subsequent bare `Enter` or the green tick commits the staged entity. Invalid text retains the last valid geometry and blocks confirmation. `Escape` first abandons an active numeric edit; the next `Escape` follows the existing draft or global-operation cancellation path.

Pending geometry keeps its stable sketch entity identity while dimensions are adjusted. Committed selected geometry displays read-only measurements. Committing, cancelling, changing tool or plane, or leaving Sketch mode clears the transient dimension session. A continuing line starts a fresh session from its retained endpoint.

These values are construction intent, not persisted constraints. Full dimensional and geometric constraints, equations, solver diagnostics, and regeneration remain in the parametric-document milestone.

## Consequences

- Numeric entry stays responsive and kernel-free while the user is drawing.
- The keyboard arbiter must use explicit dimension key claims because the workbench intentionally observes raw `Enter` and `Escape` events for its global gate.
- Geometry rules and validation are deterministic and independently unit tested.
- UI tests must prove keyboard ordering, invalid-input retention, accessible labels, fixed viewport layout, visual leaders/readouts, and the existing 60 FPS frame-cost goal.


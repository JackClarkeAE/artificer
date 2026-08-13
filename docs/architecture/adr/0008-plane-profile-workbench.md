# ADR 0008: Plane and profile workbench boundary

Status: Accepted
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

M1 needs an interactive way to exercise owned planar geometry before M4 can construct a solid from a profile. Waiting for a constraint solver, document feature graph, and complete B-rep extrusion pipeline would make the numerical and interaction work unnecessarily difficult to inspect. Conversely, treating UI-drawn entities as kernel topology would overstate the current architecture and create an accidental second source of model truth.

The former cuboid-focused kernel lab also needs to grow into a stable product shell without obscuring which capabilities are native kernel operations and which are development workbench artifacts.

## Decision

The native Rust application becomes the Artificer workbench with two explicit modes:

- **Model** presents committed native-kernel snapshots, transforms, diagnostics, source selection, and animation.
- **Sketch** presents a workbench-owned planar profile lab on the XY, YZ, or XZ origin plane.

Sketch mode uses an orthographic grid with pan and zoom. Snapping is deterministic and gives an existing endpoint priority over the configured grid lattice. Rendering may skip an integer number of fine lattice intervals for readability, but every visible grid line remains a real snap coordinate. The initial drawing tools are point, line, rectangle, circle, and a three-click arc whose stored endpoint is canonicalized onto its displayed radius. Every completed entity gesture creates a provisional sketch edit and enters the universal confirmation gate from [ADR 0007](0007-universal-model-operation-confirmation.md). The green tick or bare `Enter` commits it to the sketch revision; the red cancel action or `Escape` discards it. Pan, zoom, tool selection, and plane selection are presentation actions and remain immediate when they do not conflict with a pending edit. Leaving Sketch mode clears an incomplete click sequence rather than carrying an invisible draft across modes.

The profile display is diagnostic, not a promise of a kernel face. `artificer-geometry` owns the conservative exact polyline decisions used here: closure, winding, and certified self-intersection. A closed simple polyline can receive a fill diagnostic only when those decisions are certified. No arbitrary epsilon closes gaps or converts an indeterminate result into success.

`Finish Sketch` is itself a gated modeling operation. Its current accepted subset is:

- one certified simple closed polyline; or
- one analytic circle candidate.

Open paths, certified self-intersections, numerically indeterminate polylines, arcs, mixed curve/line profiles, and multiple loops remain inspectable but are not certified or finishable. This is a deliberately narrow supported domain, not an inference that these cases are invalid CAD profiles in general.

Committed sketch entities and the finished-profile marker belong to application/workbench state. They do not alter a kernel B-rep, do not publish or replace a native kernel snapshot, and are not yet part of the document/feature DAG. The shared confirmation gate provides consistent user intent and revision semantics across both ownership domains without collapsing them.

## Verification

- Semantic UI and canvas unit tests cover Model/Sketch mode switching, draft cancellation, XY/YZ/XZ ownership, orthographic presentation, grid/snap agreement, every tool and accessible entity target, canonical arc endpoints, provisional edits, confirmation, cancellation, and finish rejection/acceptance.
- Geometry tests cover exact closure, winding reversal, certified crossings, open profiles, invalid repeated points, and indeterminate cases.
- Visual tests cover the workbench shell, grid, certified profile fill, self-intersection diagnostics, a visible confirmation rail, and exact viewport stability at a fixed 1280×800 test size.
- Architecture checks continue to prohibit UI/rendering dependencies in the native kernel and prohibit OCCT/OpenCascade dependencies in product code.
- Non-finite derived analytic radii fail closed, and committed entities cannot be silently reinterpreted on another origin plane.
- Confirming or finishing a workbench sketch does not change the native kernel snapshot or its attempt counter.

## Consequences

- Planar algorithms become physically testable before solid construction is ready.
- The workbench can evolve toward a CAD shell while preserving a visible boundary between application artifacts and kernel truth.
- Arc and mixed-loop drawing can land before their region-classification algorithms, because unsupported certification remains explicit.
- ADR 0010 now carries the first convex-polyline profile through an M4a declarative extrusion constructor. General profiles and extrusion of a subsequently selected solid face come later, after native region, face/profile extraction, and topology-editing work can preserve validation and provenance.
- The profile lab is not yet a constrained sketch product. ADR 0009 adds transient live measurement entry as construction intent; persisted dimensional/geometric constraints, solving, feature history, regeneration, and persistence remain M5 work.

## Oracle boundary

OCCT remains an optional, separately built development oracle for offline comparison evidence only. It is not linked into the workbench or kernel, does not classify or finish interactive profiles on behalf of Artificer, and cannot broaden the native supported domain.

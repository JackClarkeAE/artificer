# ADR 0005: Display transforms and deterministic motion

- Status: Accepted; committed-transform path completed by ADR 0006 and confirmation generalized by ADR 0007
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

The first cuboid slice needs direct manipulation and visible motion so its geometry, source mapping, camera, and native UI can be exercised as a coherent tool. Those interactions must not blur the boundary between presentation state and model truth. A viewport drag is not yet a kernel operation: it has no transactional command, validation result, provenance, or newly published snapshot.

Continuous animation also creates a testing problem. Driving tests from wall-clock time would make UI state and pixel output depend on machine load, while presenting a requested cadence as guaranteed performance would give misleading evidence about the renderer.

## Decision

The native kernel lab owns a presentation layer that is separate from its immutable B-rep snapshot. It contains:

- A display transform with translation, rotation, and positive uniform scale.
- Camera orbit and zoom state.
- The active interaction tool.
- Turntable motion state, including phase, requested speed, minimum frame-rate goal, and measured UI cadence.

Presentation transforms are applied only while projecting the snapshot's diagnostic geometry into the viewport. They do not alter topology, geometric definitions, bounds, provenance, the snapshot identifier, or its semantic digest. Resetting the presentation restores the display transform and camera without executing a kernel command.

The initial native controls are:

| Input | Behaviour |
|---|---|
| `V` | Activate source-face selection |
| `O` | Activate left-drag camera orbit |
| `M` | Activate left-drag display translation |
| `R` | Activate left-drag display rotation |
| `S` | Activate left-drag positive uniform display scale |
| Right mouse drag | Orbit the camera regardless of the active tool |
| Mouse wheel | Zoom the camera |
| `Space` | Play or pause turntable motion |
| `Home` | Reset camera orientation and zoom |
| `Enter` | Confirm a pending transform through the shared operation gate |
| `Escape` | Cancel a pending transform without executing it |
| `F` | Frame the visible body and preview |

Face hit targets are active only while Select is the current tool, so selection controls do not consume manipulation drags.

### Motion and frame-rate contract

Playback advances a time-based animation phase and requests continuous native repaints. The window backend and vsync pace those frames; a high-refresh display may therefore repaint above 60 Hz. The phase depends on measured elapsed time rather than frame count, with long suspension gaps clamped only for motion continuity.

`60 FPS` is a minimum responsiveness goal, not a fixed scheduler rate or proof of 60 frames per second on every device. The UI distinguishes that goal from its smoothed repaint-start cadence. Cadence is unknown until playback produces a valid timing sample, and paused UI is labelled as paused instead of retaining a misleading `live` badge. A slow cadence is useful UI timing evidence; it does not change animation semantics or B-rep state, and it is not GPU presentation telemetry. Later GPU-backed rendering and representative-hardware benchmarks must earn any stronger performance claim.

### Deterministic test policy

Automated UI construction starts with motion paused. Interaction tests advance a synthetic clock in fixed 1/60-second steps and explicitly play or pause motion when that behaviour is under test. Pixel regressions set a known animation phase before rendering. Tests therefore compare stable state and stable pixels rather than sampling an arbitrary wall-clock instant.

Tests for display manipulation must also assert that the displayed snapshot identifier and semantic digest do not change. Kernel invariant tests remain independent of camera, animation, UI, and rendering dependencies.

### Path to committed transforms

A transform becomes model truth only through a Rust-owned kernel command. ADR 0006 implements that transition for proper whole-snapshot similarity transforms: the preview remains presentation-only, while explicit confirmation constructs and validates authoritative geometry, publishes a new immutable snapshot, and returns complete one-to-one history. Cancellation remains presentation-only. [ADR 0007](0007-universal-model-operation-confirmation.md) places this transition behind the same visible green tick/bare `Enter` and red cancel/`Escape` contract used by every interactive model operation.

OCCT remains absent from the application and kernel. It may compare declarative cases only as a separately built offline development oracle; it cannot supply display transforms, animation, committed transforms, or product geometry.

## Consequences

- The cuboid can be inspected, selected, moved, rotated, scaled, and animated immediately in the native app without weakening snapshot semantics.
- Rendering and interaction can iterate quickly while the kernel remains deterministic and UI-independent.
- Screenshots and semantic UI tests are repeatable despite animation support.
- The minimum performance goal and measured UI cadence are intentionally different concepts in the UI and documentation.
- Preview manipulation is not model truth and cannot be promoted implicitly.
- ADRs 0006 and 0007 make an explicitly confirmed preview replayable as a kernel journal and usable as changed body geometry, while retaining this ADR's presentation/model boundary.

# ADR 0022: Development incidents and privacy-conscious input tracing

Status: accepted; session recorder and first modeling span implemented.

## Context

Geometry failures are often sequence-dependent. A final invalid body is not enough to explain whether the user selected the wrong region, crossed a history boundary, changed an extrusion sign, or encountered a slow kernel fallback. At the same time, a CAD application can handle commercially sensitive models and typed names. Diagnostics must therefore be useful without becoming an indiscriminate keylogger or model exporter.

The application also must distinguish four outcomes which currently look similar to a user:

1. a typed, expected modeling rejection;
2. a long-running but healthy operation;
3. a cancelled or resource-limited operation;
4. an internal panic or process-level crash.

## Decision

Artificer will use one bounded JSON Lines event stream per application session plus a small crash-safe incident summary. The privacy-filtered, local recorder runs for every application session in both debug and release builds. It never blocks the UI: producers use a bounded `try_send` queue, and overload drops events with a later `trace.dropped` count rather than consuming the frame budget. A future Diagnostics preference may increase detail, but must not disable the basic incident trail.

### Storage

- macOS root: `~/Library/Logs/Artificer/`.
- Current session: `sessions/<UTC timestamp>-<random session id>.jsonl`.
- Incident summaries: `incidents/<incident id>.json`.
- Rotate at 8 MiB or 24 hours, retain the newest 10 sessions, and cap the directory at 64 MiB.
- Flush on operation rejection, caught panic, cancellation, and orderly shutdown. Ordinary pointer motion may remain buffered.
- Never write inside the document or silently upload diagnostics.

### Event envelope

Every record uses a stable schema:

```json
{
  "schema": 1,
  "session": "random-id",
  "sequence": 184,
  "monotonic_ms": 9312,
  "wall_utc": "2026-08-03T12:34:56.789Z",
  "thread": "ui",
  "kind": "operation.finish",
  "payload": {}
}
```

Sequence numbers, not wall-clock timestamps, define ordering. Each modeling job receives an operation/incident ID shared by UI, scheduler, kernel, validation, and history events.

### Input trace policy

Record semantic and physical actions needed for reproduction:

- pointer button, press/release, viewport-local quantized position, and selected semantic target ID;
- drag start and drag finish containing the quantized start/end positions and duration; intermediate pointer motion is not retained;
- one coalesced wheel/trackpad gesture containing accumulated zoom delta and duration;
- key identity for shortcuts and navigation keys plus modifiers;
- command/button identity (`Extrude`, `Confirm`, `Cancel`, `Sketch.Circle`) rather than only screen coordinates;
- workbench/mode transitions and history cursor changes.

Do **not** record:

- `Text`/IME payloads, clipboard contents, document names, filesystem paths, parameter names, or free-form annotations;
- raw pointer motion when no gesture is active;
- complete model geometry by default.

Numeric modeling values may be recorded because they are required for deterministic replay, but the user can export a metadata-only incident that hashes these values instead. Coordinates are quantized to 0.5 logical pixels and stored relative to the active viewport. The recorder favors state transitions and completed semantic results over raw activity: one useful gesture record is preferable to hundreds of motion samples.

### Modeling spans

Each kernel operation logs:

- request ID, command variant, input snapshot ID/digest, precision policy digest, profile counts, and topology counts;
- queue wait, preflight, construction, validation, tessellation, and publication durations;
- cancellation checks and resource counters (BSP polygons/splits, solver iterations, subdivisions, peak temporary allocation where available);
- result code and diagnostic codes;
- output snapshot ID/digest and topology counts only after atomic publication.

Long work runs outside the UI thread. The UI shows progress after 250 ms, a “complex operation” explanation after 2 seconds, and always remains cancellable. Cancellation retains the last valid snapshot.

### Error boundary hierarchy

1. **Geometry boundary:** invalid/numerically indeterminate input returns `KernelError`; no mutation occurs.
2. **Resource boundary:** every potentially explosive algorithm has explicit counters and cooperative cancellation. It returns `ResourceLimitExceeded` with the measured limit rather than exhausting memory.
3. **Worker boundary:** scheduler jobs catch unwinds and return `JobError::Panicked`; the worker remains usable.
4. **UI boundary:** a failed job becomes a non-destructive error card with incident ID and “Copy diagnostic summary”.
5. **Process boundary:** a panic hook writes the last 256 in-memory events and current operation summary using only pre-opened/best-effort files. On next launch, Artificer offers to open the incident folder; it never auto-sends data.

### Reproduction bundle

“Export development incident” produces a zip containing:

- the incident JSON and bounded event tail;
- build, OS, GPU, thread-count, and feature-flag metadata;
- kernel command/request and precision policy;
- snapshot semantic digests and topology counts;
- optionally, only with a second explicit consent, the native document or minimized geometry fixture.

A headless replay tool consumes semantic command events, verifies snapshot digests after each commit, and reports the first divergence. Raw screen coordinates are a fallback for UI replay, not the primary reproduction contract.

## Delivery sequence

1. **Implemented:** session writer, rotation, retention, redaction tests, operation IDs, panic-tail ring buffer, non-blocking bounded ingestion, and start/end gesture coalescing.
2. Semantic command/selection/drag events and the Diagnostics UI for opening/exporting logs.
3. Kernel stage spans and algorithm resource counters.
4. Deterministic headless replay and automatic failing-sequence minimization.
5. Opt-in performance telemetry summaries; no network transport without a separate ADR and consent design.

## Current incident: crossing circular cuts

The August 2026 investigation found no macOS crash report. The circle-through-circle case instead drove the regularized faceted BSP fallback with a display mesh of roughly three thousand triangles, causing combinatorial fragmentation on the UI thread. The immediate hardening is:

- committed sketch extrusion executes as a cancellable background commit and publishes atomically;
- caught worker panics no longer kill the worker or UI;
- the crossing-curve construction mesh has an independent bounded subdivision policy;
- the crossing-curve construction mesh is decoupled from high-density display tessellation so BSP input growth is explicitly bounded;
- a perpendicular circle-through-circle regression replaces the weaker rectangle-through-circle fixture.

The bounded faceted result is an interim capability. A future exact cylinder/cylinder intersection and trimming feature should replace it while preserving the same job, error, tracing, and publication contracts.

# Sketch CPU frame-construction evidence — 2026-07-30

## Scope and result

This is first-pass **CPU frame-construction evidence** for the sketch and workbench fixtures. It measures the wall time of the deterministic headless `egui` harness step used to build an application frame. It does **not** measure command submission, GPU execution, compositor scheduling, display presentation, input-to-photon latency, or dropped presented frames. It must not be cited as end-to-end 60 FPS evidence.

All eight fixtures that emit distribution metrics passed the automated p95 threshold of **16.67 ms**. The largest observed p95 was **0.436458 ms**; the largest single observed sample was **1.438000 ms**. The separate dense-sketch closing-edge interaction gate also passed, but the current test emits only pass/fail and therefore supplies no p95 or maximum value.

## Reproduction identity

| Field | Recorded value |
|---|---|
| Captured | `2026-07-30T11:17:26Z` (`2026-07-30 12:17:26 BST`) |
| Repository revision | `bae46ebe5fd168a94bb4b0517f2c070699b3af9f` |
| Branch | `main` |
| Worktree | Dirty development worktree at capture: 46 tracked paths changed, 47 untracked paths, no staged changes |
| Host class | Apple M5 MacBook Air, 10 logical CPU cores, 16 GB memory |
| OS | macOS 26.5.2, build 25F84 |
| Rust | `rustc 1.95.0 (59807616e 2026-04-14)`, host `aarch64-apple-darwin`, LLVM 22.1.2 |
| Cargo | `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` |
| Build profile | Cargo `release`, optimized |
| Power state | Battery power, 17% and discharging when the run began |
| Exact command | `./scripts/check-sketch-frame-budget.sh` |
| Test result | 9 passed, 0 failed; process exit 0 |

The dirty-worktree state is recorded deliberately: these measurements describe the named revision plus the active first-pass sketch-tool changes, not a clean historical checkout of the revision alone.

## Measurement policy

- The timed distribution fixtures run 100 unmeasured warm-up frames, then 500 measured frames.
- Each sample wraps one headless harness `step`; samples are sorted and the p95 is the nearest-rank 95th percentile used by the checked-in test.
- The simulated time step is `1 / 60 s`, with one pixel per point and the dark theme.
- The maximum-pattern and maximum-visible-curve fixtures use the minimum 1040×700 harness. The current workbench and legacy dense-sketch fixtures use 1280×800.
- Automated acceptance requires both average and p95 CPU construction time to be below `1 / 60 s`. The table reports p95 and maximum; maximum is diagnostic rather than the present automated gate.
- The dense closing-edge gate separately measures five fresh 256-edge construction attempts and requires the median geometry-changing stage operation to be below 16,667 µs. The test does not print its individual timings.

## Fixture results

| Fixture | Harness | Frames | p95 | Maximum | p95 threshold | Result |
|---|---:|---:|---:|---:|---:|---|
| `active rectangle dimension overlay` | 1280×800 | 500 | 0.051667 ms | 0.078167 ms | 16.67 ms | pass |
| `sketch.pattern.256 pending preview` | 1040×700 | 500 | 0.056875 ms | 0.070250 ms | 16.67 ms | pass |
| `sketch.visible_curves.1024 pending preview` | 1040×700 | 500 | 0.157334 ms | 0.179958 ms | 16.67 ms | pass |
| `collapsed model workbench UI generation` | 1280×800 | 500 | 0.152250 ms | 0.190167 ms | 16.67 ms | pass |
| `model workbench UI generation` | 1280×800 | 500 | 0.312666 ms | 1.438000 ms | 16.67 ms | pass |
| `21-face Add/Cut/Add workbench UI generation` | 1280×800 | 500 | 0.433208 ms | 0.542625 ms | 16.67 ms | pass |
| `21-face body-context sketch workbench UI generation` | 1280×800 | 500 | 0.436458 ms | 0.586167 ms | 16.67 ms | pass |
| `256-edge certified sketch UI generation` | 1280×800 | 500 | 0.108375 ms | 0.173125 ms | 16.67 ms | pass |

Raw nanosecond values emitted by the test, in the same row order, were:

```text
p95_ns=51667  max_ns=78167
p95_ns=56875  max_ns=70250
p95_ns=157334 max_ns=179958
p95_ns=152250 max_ns=190167
p95_ns=312666 max_ns=1438000
p95_ns=433208 max_ns=542625
p95_ns=436458 max_ns=586167
p95_ns=108375 max_ns=173125
```

## Resource-limit and architecture checks

The following supporting checks were run against the same working tree:

| Exact command | Result |
|---|---|
| `cargo test -p artificer-sketch --test property_limits -- --nocapture` | 5 passed, 0 failed. This includes exact curve/event ceiling acceptance, next-value rejection, checked hostile pattern counts, stable semantic IDs, exact maximum-pattern cardinality, and the timing smoke test. |
| `./scripts/check-architecture-boundaries.sh` | Passed: `architecture boundaries are clean`. The product workspace dependency tree contains no OCCT/OpenCascade dependency; the kernel/sketch dependency boundaries and staged-operation call-graph tripwires also passed. |

The property target reported that the debug-profile maximum 256-instance/1,024-curve pattern staged in 33.906666 ms. That number is a non-flaky debug smoke observation, not a release frame-construction measurement and not GPU presentation evidence.

## Native presentation checklist

This remains required before any release candidate is described as sustaining 60 FPS. Use a native optimized build on the named reference machine and record real presented-frame telemetry; do not substitute the CPU figures above.

| Check | Required action | Status | Evidence field |
|---|---|---|---|
| Reference stack | Record exact Mac model class, macOS build, display refresh rate, scale factor, window size, graphics backend/device, and power state. | `pending_manual` | `pending_manual` |
| Baseline orbit | Run the model view for at least 30 s while continuously orbiting a representative multi-face body; watch the in-app cadence display and collect presented-frame timestamps. | `pending_manual` | `pending_manual` |
| Face-sketch focus | Enter a face-hosted sketch with the solid body visible, continuously pan/zoom and move the pointer for at least 30 s. | `pending_manual` | `pending_manual` |
| 1,024 visible curves | Exercise the maximum visible-curve sketch while continuously moving the pointer and editing a handle for at least 30 s. | `pending_manual` | `pending_manual` |
| 256-instance preview | Drag both pattern spacing/count controls through the maximum live preview for at least 30 s. | `pending_manual` | `pending_manual` |
| Trim interaction | Hover and trim spans in the representative dense mixed-curve fixture for at least 30 s. | `pending_manual` | `pending_manual` |
| Acceptance | From presented-frame telemetry, report sample count, median, p95, maximum, dropped/missed presentation count, and longest consecutive miss. Require p95 ≤ 16.67 ms and no multi-frame synchronous recomputation spike. | `pending_manual` | `pending_manual` |

The existing in-app cadence display is useful as a live operator warning and should be captured in the run notes, but a screenshot or repaint-start cadence alone is not presented-frame telemetry.

# ADR 0013: Repeatable rectangular face features use local boundary rewrites

- Status: Accepted for the historical M4d experimental slice; profile and topology domains are widened by ADR 0015
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

M4c proved one native rectangular Add or blind Cut on one axis-aligned face of a rectangular-prism solid. Its constructor deliberately rebuilt a fixed fourteen-face result and rejected every source that was not the original eight-vertex, twelve-edge, six-face prism. Consequently, the first feature was valid and deterministic, but its output could never be the input to a second feature.

M4d must support a useful feature chain without claiming a general Boolean engine or the M5 parametric document. Each operation still consumes one immutable snapshot and publishes a new snapshot only after complete validation. The selected face reference remains snapshot-owned, and failed, stale, colliding, or unsupported intents must retain the previous committed snapshot.

## Decision

M4d supports repeated local rectangular Add and blind Cut operations within this explicit domain:

- one finite, valid, connected solid and shell;
- finite axis-aligned planar quadrilateral boundary patches;
- one selected boundary patch certified as an axis-aligned rectangle;
- one axis-aligned rectangle strictly inset from that selected patch;
- an outward Add sweep through empty space or an inward Cut sweep through uninterrupted material;
- a positive feature depth above the active minimum feature size; and
- no touching, crossing, breakthrough, multi-body, rotated, curved, holed, or ambiguous case.

The kernel preserves every non-target boundary patch and locally replaces only the selected rectangle. The target becomes four shoulder patches, while the operation adds one end or floor and four walls. The topology is rebuilt deterministically from exact shared coordinates and then passes through the ordinary solid validator before publication. Under this scaffold, each successful feature adds eight vertices, sixteen edges, thirty-two coedges, eight loops, and eight faces while retaining one shell and one solid.

Input certification derives containment from the selected face boundary rather than whole-body bounds. Sweep certification is conservative and fails closed whenever the proposed prism is not provably empty for Add or wholly material with retained material behind its floor for Cut. This is a bounded local topology edit, not an implicit general union or difference algorithm.

### Identity and history

Every command targets an exact face reference owned by its input snapshot. The operation report covers every input entity and covers every output entity exactly once with `Unchanged`, `Modified`, `Generated`, or `Deleted` relations. A split input may therefore appear in several records. Strictly inset features preserve source vertices and edges, preserve every non-target face/loop/coedge, map the target face and loop to four modified patches, modify the shell and solid, and classify the new feature boundary as generated.

Those mappings allow a test or application to resolve a newly generated end or floor in the latest snapshot and use it in the next command. They do not provide M5 persistent naming: editing an earlier feature, rebasing downstream references, regeneration, suppression, and ambiguity resolution remain document-layer work.

### Workbench command contract

The command-ribbon Extrude control starts the extrusion preview directly. It does not merely reveal a second execute button. The preview remains presentation state, its mode and distance remain editable, and only the shared green tick or bare `Enter` may execute the kernel command. The red cross or `Escape` cancels without publishing. Each successful feature begins from a fresh support query against the latest snapshot and appends one session-local History entry.

## Verification

- Kernel tests cover base to Add to Cut to later Add chains, all supported axis directions, deterministic replay, exact topology increments, analytic measures, complete history coverage, and retained state after stale, collision, unsupported-target, and breakthrough rejection.
- Declarative test cases resolve a later target from a prior operation-history role before recording the concrete snapshot-owned command in the journal.
- Semantic workbench tests cover direct Extrude staging, editable preview intent, cancellation, confirmation, Browser/History numbering, and sketching on a newly generated face.
- Visual tests cover staged and committed multi-feature parts, and frame-budget tests retain the 60 FPS goal with a representative repeated-feature body.

## Consequences

- Artificer can perform several useful native operations on successive immutable versions of one part.
- The implementation remains small enough for exhaustive invariant and replay testing while creating the feature-history evidence M5 will later consume.
- Faces created by prior features can be selected directly in their current snapshot, but they are not stable names across edits to older commands.
- Multiple sketch regions, holes, analytic circle/arc topology, general tessellation, BVH ray queries, revolve, arbitrary orientations, and regularized Booleans remain later M4/M6 work.

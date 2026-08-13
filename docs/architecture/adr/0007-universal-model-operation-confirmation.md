# ADR 0007: Universal confirmation for interactive model operations

Status: Accepted
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

ADR 0005 separated presentation state from model truth, and ADR 0006 introduced the first UI path that can publish changed model geometry. That path used transform-specific Apply and Discard controls. As construction, edit, diagnostic, import, and repair operations arrive, giving each widget its own commit behaviour would make the transaction boundary inconsistent and easy to bypass.

The lab also needs a visible answer to “will this action change my model?” Keyboard activation must remain useful when a numeric editor or another control has focus, without allowing the same key event to both stage and commit a new operation.

## Decision

Every user-triggered operation that can execute a kernel command, replace model truth, or commit a workbench-owned modeling artifact enters one shared pending-operation state before execution. A pending operation records its kind, visible description, editable intent where applicable, and the relevant immutable base revision or snapshot. Only one modeling operation can be pending at a time.

The shared confirmation slot exposes:

- A compact green square containing only a tick, with bare `Enter` as its keyboard equivalent.
- A compact red square containing only a cross, with `Escape` as its keyboard equivalent.

The visible controls are intentionally icon-only to keep the invariant rail compact. They retain the accessible names `Confirm operation` and `Cancel operation`, plus hover text that exposes the keyboard equivalents.

The slot occupies stable layout space even when no operation is pending, so staging, confirming, rejecting, or cancelling an operation does not move the viewport. When nothing is pending, `Enter` and `Escape` are model-operation no-ops.

Operation-specific controls only stage or edit intent. They do not call the kernel, publish a snapshot, or append a committed sketch entity directly. At the end of a UI frame, one central dispatcher interprets confirmation. Kernel-backed operations pass through the public native-kernel transaction path; workbench-owned sketch operations pass through their revisioned sketch commit path. A keyboard event may confirm only an operation that was already pending at the start of that frame; pressing `Enter` on a focused case button or drawing the final point of an entity cannot both stage and confirm it in one event.

Confirmation has transactional semantics:

- Success publishes only the validated outcome and clears the pending operation.
- Rejection preserves the last valid committed snapshot and retains the pending operation, including its base snapshot and editable intent, so the reason remains visible and the user can revise, retry, or cancel it.
- Cancellation clears the pending operation without executing a kernel command and without replacing the last kernel report.
- A stale base snapshot is handled as an ordinary structured rejection; it is never silently rebound to newer model truth.

Move, Rotate, and Scale previews use this contract, as do user-selected diagnostic cases, each point/line/rectangle/circle/arc sketch insertion, `Finish Sketch`, and all later interactive model-changing operations. The words “every operation” in the workbench mean every interactive modeling operation, not every interaction. Presentation-only actions remain immediate: selection, camera orbit and zoom, sketch pan and zoom, framing, non-conflicting view-tool choice, animation playback, and other view state neither require confirmation nor become kernel commands. Modeling-operation tools that conflict with an existing pending intent remain disabled until confirmation or cancellation.

The M1a profile lab deliberately keeps its sketch entities outside the native kernel snapshots. Using the shared gate for those workbench-owned artifacts preserves one interaction contract without pretending that sketch geometry is already a B-rep or kernel command. [ADR 0008](0008-plane-profile-workbench.md) records that ownership boundary and the current finish-profile acceptance rule.

Startup/bootstrap construction, journal replay, command-line case execution, and explicit headless automation are non-interactive execution paths. They call the same public kernel protocol and retain its validation and rollback rules, but they do not manufacture a UI confirmation step. These exceptions must remain named entry points; a UI widget cannot use them to bypass the pending-operation gate.

## Verification

- Semantic UI tests assert that staging any supported model operation does not execute a kernel request or change the snapshot.
- Pointer and keyboard tests exercise the green tick, bare `Enter`, red cancel action, and `Escape`, including while numeric editors and case buttons hold focus.
- Tests assert that one confirmation causes at most one kernel attempt, an idle `Enter` causes none, and cancellation causes none.
- Rejected transform and diagnostic operations retain both committed model truth and their pending state; successful operations clear it.
- Sketch tests assert that a completed drawing gesture creates only a staged entity; confirmation appends it to the sketch revision, while cancellation appends nothing. `Finish Sketch` is likewise staged and rejects any profile outside its certified subset without changing kernel snapshots.
- Accessibility queries can find both confirmation actions whenever an operation is pending.
- Clean, pending, and rejected states fit the supported minimum window and retain the same viewport rectangle; pixel tests cover the visible pending state and stationary commit.
- Source review and architecture checks keep direct widget-to-kernel execution outside the allowed paths.

## Consequences

- The user gets one consistent, visible commit gesture as the operation catalogue grows.
- A green tick or bare `Enter` always means “validate and attempt this pending modeling operation”; it never means “promote display pixels into kernel geometry.”
- Rejection is inspectable and recoverable because neither committed state nor staged intent disappears.
- New interactive kernel features must integrate with the pending-operation type and central dispatcher before exposing an execute control.
- View and animation controls stay responsive because presentation state is deliberately outside the confirmation boundary.

# ADR 0041: Tools and selections meet in either order

Status: implemented — every stage below has shipped. Revised after external
review; see *What review changed*.

- Date: 2026-09-17
- Decision owners: Artificer project

## The invariant

> **A command must never be disabled merely because one or more of its operands
> have not yet been selected.**

That is deliberately narrower than "no gating". A command may still be
unavailable because the history marker is not at the tip, because another
operation is staged, because the geometry is a library component's and
immutable, or because the workspace fundamentally cannot execute it. None of
those is "you have not picked the thing yet".

The regression test that expresses it:

> Changing **only** the current selection must never move a command from
> available to blocked, when its missing operands are ones the user could pick.

## Context

Pick a face, then press Extrude. Pick two edges, then press Fillet. That is how
the workbench works, and it is half of how people work. Press Fillet first and
the button is dead until you have guessed what it wanted.

Every ribbon button passes through `command_availability`, and underneath the
two legitimate gates `preset_feature_availability` hard-gates on selection:

| Command | Requires | Says when it has not got it |
|---|---|---|
| Hole, Rib, Hole pattern | `selected_face.is_some()` | "Select a planar face first." |
| Mirror, Linear pattern, Shell | `active_body_id().is_some()` | "Activate a body first." |
| Chamfer, Fillet | `!selected_edges.is_empty()` | "Select at least one edge first." |
| Revolve | nothing | — |

A greyed button teaches nothing about *order*, only about *state*. And the two
orders are not equivalent in what they can express: tool-first can narrow
picking to eligible edges and say "pick the edges to round"; selection-first
cannot, because by the time the tool knows what it wants the picking is over.

### It already exists, three times, three ways

- **Sketch relations** arm and then collect operands
  (`SelectionRequirement::RelationOperands`).
- **Body Booleans** stage with the active body as target, then collect tool
  bodies into `boolean_tools`.
- **Extrude** already carries the intent in a comment in its own availability
  function: *"Extrude is still the right button to press: it hands the canvas
  to Select and says where to click, instead of greying out behind a tooltip."*
- **To face** likewise arms a face pick and says what to click.

Four precedents, no shared code. This is a generalisation of something the
codebase keeps rediscovering.

## Decision

### 1. Applicability and operand completeness are different questions

`command_availability` currently mixes "this operation cannot happen" with "you
have not selected its input yet". They split:

```rust
enum Applicability { Available, Blocked(Cow<'static, str>) }

enum OperandResolution {
    Complete(OperandBindings),
    NeedsOperands { role: OperandRoleId, prompt: &'static str },
    InvalidSelection(ResolutionDiagnostics),
}
```

**The ribbon consults `Applicability` only. Invocation consults
`OperandResolution`.** That division is the whole architecture; everything
below follows from it.

`Applicability` must stay cheap. It runs for every ribbon control every frame,
so it may never enumerate topology. "Are there any fillet-able edges on this
body?" is not an availability question — press Fillet and be told *"No
compatible edges exist on this body"* rather than scanning the B-rep sixty
times a second to decide whether to grey an icon.

### 2. An appetite describes eligibility, not entity kind

`OperandKind × Arity` is too weak to drive tool-first picking. Hole does not
want "a face"; it wants *one planar, supported face on an editable body*, which
`stage_preset_feature` only discovers later via
`NativeKernel::planar_face_support`. Fillet does not want "edges"; it wants a
*compatible finish set* on one body — the existing implementation already
checks body agreement, rim completeness and set compatibility.

An appetite that says `Edge` and lets Fillet rediscover afterwards that it was
the wrong edge is precisely the class of bug this ADR exists to remove. So a
role carries a predicate:

```rust
struct OperandRoleSpec {
    id: OperandRoleId,
    prompt: &'static str,
    cardinality: Cardinality,
    accepts: fn(&InvocationContext, &OperandBindings, &SelectionItem) -> OperandEligibility,
    source: OperandSourcePolicy,
}

enum OperandEligibility {
    Accept,
    WrongKind,
    Incompatible(&'static str),
    Immutable(&'static str),
    Stale,
}
```

One predicate then drives all five consumers: harvesting preselection, filtering
the viewport picker, answering whether the appetite is satisfiable, wording the
prompt, and the final pre-stage validation. Tool-first Hole makes only valid
planar faces pickable, rather than offering every face and refusing a cylinder
afterwards.

`accepts` takes the bindings so far, because eligibility is often relative: a
Boolean tool body is any body *except the target*, and the second edge of a
two-distance chamfer must share a body with the first.

### 3. Alternatives are not roles

Roles are operands a tool needs *together*; alternatives are different readings
of the same request. Conflating them cannot express Mirror, whose plane may come
from a selected construction plane, a planar face, or an origin plane:

```rust
struct ToolInvocationSpec { alternatives: &'static [OperandSchema] }
struct OperandSchema  { roles: &'static [OperandRoleSpec] }
```

Alternatives are tried in order; the first whose roles all resolve wins, and the
readout names which reading was taken.

### 4. One ordered selection, with per-kind views

"One ordered set per kind" destroys global order. Clicking Face A, Edge B, Face
C, Edge D and storing `faces=[A,C]`, `edges=[B,D]` makes `A→B→C→D`
unrecoverable. So:

```rust
enum SelectionItem { Body(..), Face(..), Edge(..), Vertex(..), ConstructionPlane(..),
                     SketchRegion(..), SketchCurve(..), SketchPoint(..) }

struct SelectionEntry { item: SelectionItem, sequence: u64, source: SelectionSource }
struct SelectionModel { ordered: Vec<SelectionEntry> }
```

Per-kind collections become **views**, not separate authorities.

This also fixes a live defect rather than merely tidying. The singular field is
`selected_face: Option<EntityRef>`; the plural is
`selected_faces: Vec<DocumentFaceSelection>`, and only the latter carries
`BodyInstanceKey`. `EntityRef` is `{snapshot, entity, kind}` with no occurrence
identity, so with two occurrences of one part in an assembly the singular
selection cannot say which one is meant. **A selected face always means body +
face reference.** The `EntityRef`-only form goes away.

### 5. Deterministic order may resolve symmetric operands; it must never invent asymmetric ones

The current Boolean code carries the lesson already:

> *Tools start empty on purpose. Guessing an operand was the old behaviour and
> it silently picked the wrong body once a third existed.*

Resolving roles from selection order contradicts that. For Fillet's edge set,
order is irrelevant and any deterministic ordering is fine. For Difference,
which body is the target is semantically load-bearing, and "first selected body"
is a guess wearing a rule's clothing. Rubber-band selection makes it worse:
deterministic ordering by document reference is *repeatable*, and repeatably
choosing an arbitrary target is still wrong.

> Deterministic ordering may resolve **symmetric** operands. It must never
> invent meaning for **asymmetric** roles.

`OperandSourcePolicy` says where a role may come from — the active body, an
explicit pick, or the general selection. Where an unordered selection cannot
establish an asymmetric role, the tool arms and asks for that role by name.

### 6. "Ignored" has three meanings, and only one of them is silent

Dropping a face from a Fillet's selection is right. Dropping an *edge* Fillet
cannot round is how you ship the wrong part.

| Class | Meaning | Behaviour |
|---|---|---|
| **Extraneous** | wrong kind for every role | ignore; count it in the readout |
| **Rejected** | right kind, incompatible with this operation | never silently omitted; blocks automatic staging |
| **Stale** | reference no longer resolves | removed and reported explicitly |

So Fillet with a face selected ignores it; Fillet with an unsupported curved
edge selected does **not** quietly round the other two; Fillet with a deleted
edge reference drops it and says so.

The readout carries the tally — `Fillet · 2 edges · 1 ignored` — and while a
tool is resolving, the viewport also draws accepted operands in the tool
highlight, rejected ones in a warning treatment, and extraneous ones in ordinary
selection styling. Ambiguity resolved without a modal.

### 7. Bindings belong to the tool; the selection belongs to the user

```rust
struct ArmedTool {
    tool: ToolId,
    schema: &'static OperandSchema,
    bindings: OperandBindings,
    resolved_at: DocumentRevision,
}
```

Because the tool never owns the selection, there is nothing to restore:

| Event | `ArmedTool` | `SelectionModel` |
|---|---|---|
| Escape | dropped | unchanged |
| another tool pressed | replaced | unchanged |
| stage | bindings frozen into `PendingOperation` | unchanged |
| cancel staged operation | — | unchanged |
| commit | dropped | **consumed operands removed** |

Commit removes what the tool consumed, not the whole selection: after a Fillet
over edges A and B with face X also selected, X stays selected, because the
Fillet never consumed it.

### 8. Armed bindings are revision-invalidated, not revalidated every frame

`resolved_at` is a document revision. Re-resolution happens when the document
revision changes, the history cursor moves, the active body or context changes,
or visibility/mutability changes eligibility — and once more immediately before
staging. Not because egui painted another frame. The codebase already has this
shape in `prune_stale_measured_edges`.

### 9. Filter before hit ranking

An armed tool masks the hit test to eligible candidates and ranks only those. It
does not hit-test generically, find a face nearest, and reject the click — that
filter would not actually improve picking. This is the same principle the To
face work established: what is rendered or picked must not decide what
modelling entities exist.

### 10. Share the resolver; do not force one state machine

Sketch relations live in the sketch-tool state machine, Extrude's profile pick
interacts with sketch mode, and Booleans work on body identities. They should
share `OperandSchema`, the resolver, eligibility, bindings and diagnostics —
and keep their own adapters. Sharing semantics matters; forcing every UI state
into one enum does not.

## Acceptance

The invariant at the top, plus the one that makes it real. For every converted
command:

> Preselect the operands, then invoke → **exactly** the same staged intent as
> invoke, then pick the same operands.

Not "looks the same": the same `PendingOperation`, the same feature arguments,
the same persistent references. Around that, test mixed preselection, partial
preselection, stale references and operand permutations. Permutations of
symmetric operands must give equivalent bindings; permutations must only change
asymmetric roles where the user explicitly established the order.

## Stages

Reordered after review. The armed path exists **before** any gate is flipped, so
no intermediate commit ships a live-but-dead button — which is the very defect
this ADR removes.

1. ✅ **Canonical selection**, per-kind views, body identity on every face. The
   singular and plural forms were not merely redundant: a push-pull remapped one
   and left the other holding the pre-operation reference, and finishing a
   sketch cleared one and left the other populated.
2. ✅ **`ToolInvocationSpec` + pure resolver**, with the agreement test.
3. ✅ **`ArmedTool`, bindings, prompt**, unused until stage 4. Taking a pick and
   waiting for one turned out to be different questions — a fillet wanting "one
   or more edges" is satisfied by the first and still wants the rest.
4. ✅ **Fillet and Chamfer**, the first gates to move.
5. ✅ **Hole, Rib, Hole pattern.**
6. ✅ **Shell, Mirror, Pattern** — which needed no conversion, because a body is
   the workspace's active body rather than an operand they ask for.
7. ✅ **Boolean and Extrude appetites** in the shared vocabulary, keeping their
   own state machines.
8. ✅ **Sketch relations**, likewise.

### What is not done

The **filtered picker** is specified and its predicate exists — `ArmedTool`
answers whether a pick is eligible — but the viewport does not yet mask its hit
test to it, so an armed tool prompts without narrowing what can be clicked.
`accepts` is the seam that does it.

Two appetites are **declared but not enforced**: `SKETCH_EXTRUSION` and
`SKETCH_RELATION` carry `WrongKind` predicates because sketch operands are
`SketchEntityId`s rather than the model `SelectionItem`s the resolver speaks.
Widening `SelectionItem` to carry them is what would let those two resolve
rather than merely describe.

## What review changed

Recorded because the first draft shipped as "proposed" and the differences are
instructive rather than cosmetic: availability split from operand completeness;
eligibility predicates instead of kind and arity; alternative schemas made
explicit; heterogeneous selection order preserved instead of one order per kind;
asymmetric roles never inferred from arbitrary or rubber-band order; rejected
operands separated from extraneous ones; bindings made tool-local and
revision-invalidated instead of revalidated per frame; and the rollout reversed
so the armed path precedes any gate change.

## Consequences and limits

The ribbon gets quieter and less instructive: buttons that explained what to
select now work, and the explanation moves to the prompt after the press.

Not every command has an appetite — Orbit, Frame, Theme and the panel toggles
consume nothing and stay outside this model rather than being bent into it.

This does not make tools composable: arming Fillet and then Chamfer replaces the
armed tool rather than queuing two operations. Sequencing is the history's job.

And it changes what no tool *does*. The kernel sees the operands it sees today;
all of this is about how they are gathered.

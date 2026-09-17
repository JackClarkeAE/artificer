# ADR 0041: Tools and selections meet in either order

Status: proposed — a design for review. Nothing here ships yet.

- Date: 2026-09-17
- Decision owners: Artificer project

## Context

Pick a face, then press Extrude. Pick two edges, then press Fillet. That is how
the workbench works, and it is only half of how people work. The other half —
press Fillet, then pick the edges — is not available, and the button is greyed
out until you have guessed what it wanted.

This is not one bug in one command. It is the shape of
`command_availability`, which every ribbon button goes through, and it repeats
across every tab.

### What happens now, exactly

Two conditions gate nearly everything and are answered once, in the same words:
a staged operation ("Confirm or cancel the pending operation first.") and a
history marker that is not at the end ("Move the history marker to the end
before creating another feature."). Those are real reasons and are not what
this record is about.

Underneath them, `preset_feature_availability` hard-gates on what is selected:

| Command | Requires | Says when it has not got it |
|---|---|---|
| Hole, Rib, Hole pattern | `selected_face.is_some()` | "Select a planar face first." |
| Mirror, Linear pattern, Shell | `active_body_id().is_some()` | "Activate a body first." |
| Chamfer, Fillet | `!selected_edges.is_empty()` | "Select at least one edge first." |
| Revolve | nothing | — |

The button is dead until the condition is met. The tooltip explains the rule,
which is better than silence, but it is still a door that tells you where the
key is instead of opening.

### Three places already do it the other way

The pattern the user is asking for is not foreign to this codebase. It exists
three times, built three different ways, and none of them is reusable:

- **Sketch relations.** `SelectionRequirement::RelationOperands`: arm the
  relation, then pick its operands on the canvas. This is the cleanest of the
  three and is the closest thing to a reference implementation.
- **Body Booleans.** Combine stages `PendingOperation::BooleanBodies` with the
  active body as target, and the *tool* bodies are picked afterwards, into
  `boolean_tools`, by `toggle_boolean_tool`. Tool-first for its second operand,
  selection-first for its first.
- **Extrude.** Its availability function already carries the intent in a
  comment: *"With bounded profiles drawn but none picked — or a pick that
  cannot become a solid — Extrude is still the right button to press: it hands
  the canvas to Select and says where to click, instead of greying out behind a
  tooltip."* That is exactly this ADR, implemented once, for one command.

So the decision below is less an invention than a generalisation of something
the codebase keeps rediscovering.

### Why it matters beyond convenience

A greyed button teaches nothing about *order*, only about *state*. A user who
has not yet learned that Fillet wants edges finds a dead control and no way in.
A user who has learned it still pays for the lesson every time they change
their mind about which edges — deselect, reselect, press again.

And the two orders are not equivalent in what they can express. Tool-first can
prompt: "pick the edges to round" narrows the viewport's picking to edges and
says what it is waiting for. Selection-first cannot, because by the time the
tool knows what it wants, the picking is over.

## Decision

### A tool declares an appetite; invocation resolves it

Every tool that consumes geometry declares what it eats, rather than each
command hand-writing a gate:

```rust
/// What a tool consumes, and how much of it.
struct Appetite {
    /// In preference order. A tool that can take either edges or a whole face
    /// lists edges first if that is the reading it prefers.
    roles: &'static [OperandRole],
}

struct OperandRole {
    name: &'static str,          // "edges to round", "target body", "tool bodies"
    kinds: &'static [OperandKind], // Face | Edge | Vertex | Body | SketchRegion | SketchCurve | SketchPoint
    arity: Arity,                // Exactly(1) | AtLeast(1) | Between(1, 2) | Any
    optional: bool,
}
```

Roles rather than a flat set, because several tools need to tell their operands
apart. A Boolean's target is not its tools. A two-distance chamfer's first edge
is not its second. A mirror's body is not its plane.

### The two orders are one code path

This is the part that makes the change tractable rather than a rewrite of every
command. Invoking a tool is:

```
invoke(tool):
    harvest, dropped = partition(current_selection, tool.appetite)
    if harvest satisfies tool.appetite:
        stage(tool, harvest)          # what selection-first does today
    else:
        arm(tool, harvest)            # keep what fits, wait for the rest
```

Selection-first and tool-first stop being two behaviours. They are one
resolution with two outcomes, chosen by whether what is already selected is
enough. A tool pressed with a full selection stages immediately, exactly as it
does now; the same tool pressed with nothing selected arms and prompts; pressed
with a partial selection it arms holding what it already has.

The user's example resolves without a special case: two lines and a face
selected, Chamfer pressed, appetite is `Edge × AtLeast(1)` — the two lines are
harvested, the face is dropped, and because the harvest satisfies the arity it
stages straight away.

### Dropping is silent in the result and loud in the readout

"Unsupported features would just be dropped" is right about the geometry and
wrong about the user. A selection that silently shrinks is how someone chamfers
two of the three edges they thought they had picked.

So: dropped operands never block, never warn modally, and never appear in the
result — and the confirmation readout says what happened, in the same line that
says what the tool is about to do:

> Chamfer · 2 edges · 1 face ignored

The count is the honest part. Naming *which* face was ignored costs a sentence
nobody reads; saying that one was is what stops a wrong result being accepted.

### Armed is a state, and it is not a staged operation

A tool waiting for operands must not lock the ribbon. Today
`pending_operation.is_some()` disables every command, which is right for a
staged operation awaiting confirmation and wrong for a tool that is merely
waiting to be told what to work on.

So `armed: Option<ArmedTool>` is separate from `pending_operation`, and:

- Pressing another tool **replaces** the armed one. Changing your mind is one
  click, not a cancel and a click.
- Escape **disarms** and leaves the selection alone. Disarming and clearing are
  two different intentions and must not share a key.
- Arming changes no document state, so it needs no undo entry and no
  confirmation. Nothing has happened yet.

### The armed tool filters picking and says what it wants

While armed, the viewport's hit testing is narrowed to the kinds the current
role accepts, and the status line names the role: "Pick the edges to round".
This is the capability tool-first has and selection-first cannot: it is
impossible to pick the wrong kind of thing, rather than merely futile.

When a role is satisfied and the appetite has another, the prompt advances to
it. When the last required role is satisfied, the tool stages itself and the
existing confirmation rail takes over unchanged.

### An empty selection is no longer a reason to disable a tool

This is the rule that replaces the table above. A command is disabled only for
reasons that arming cannot fix:

- a staged operation is awaiting confirmation
- the history marker is not at the end
- the geometry is immutable (a library component occurrence)
- **the appetite is unsatisfiable in principle** — Fillet with no body in the
  document at all has nothing to arm for, and still says so

That last one matters. Arming into a state nothing can ever satisfy is a worse
dead end than a greyed button, because it looks live.

## Robustness, edge cases and error handling

These are the cases that decide whether this is an improvement or a new class
of bug. They are listed because an outside reviewer should push on them.

**Stale operands.** `selected_edges` holds persistent document references. A
history replay, an undo, or a rebuild can leave a reference that no longer
resolves. The armed set is therefore re-validated every frame against the
current document, and anything that stops resolving is dropped with a readout
line. A tool must never stage against a reference it cannot resolve, and must
never silently substitute a different one.

**Selection is consumed on commit, not on stage.** If a commit left the
operands selected, the next tool press would silently re-use them. If a cancel
cleared them, changing your mind would cost the selection. So: staging leaves
the selection intact, committing clears it, cancelling restores it.

**Order is preserved.** The harvest keeps click order, because roles are
assigned in order and several tools care which operand came first. A selection
made by rubber band, which has no click order, is ordered deterministically by
document reference so that the same selection always resolves the same way.

**Two selection representations.** There are currently both singular
(`selected_face`, `selected_edge`, `selected_vertex`) and plural
(`selected_faces`, `selected_edges`, `selected_vertices`) fields, cleared
together by `clear_model_entity_selection`. The resolver must read one model,
not two, or it will harvest from the plural while the viewport writes the
singular. **Unifying these is a prerequisite, not part of the change.**

**Ambiguous appetites.** A tool that accepts either of two readings of the same
selection — a face *or* its edges — must resolve deterministically and say which
reading it took. Preference order in `roles` decides, and the readout names it.

**Partial harvests that cannot complete.** Selecting one edge and pressing a
tool that needs exactly two arms holding that edge; the prompt says "pick one
more". If the document contains no second candidate, the tool disarms and
explains rather than waiting forever.

**Modal conflicts.** Arming while a sketch is open, while a Boolean is staged,
or while the history marker is back in time must be refused with the existing
words. Arming is new, but it does not get to bypass gates that exist for other
reasons.

**Every dropped operand is a test.** The failure mode this change introduces is
a tool quietly acting on less than the user believed. So the gate for each
converted command is a test that a mixed selection produces the right harvest
*and* the right ignored-count, not merely a result that happens to be correct.

## How this would be done

Sequenced so that each stage is independently reviewable and none of them is a
flag day.

1. **Unify the selection model.** Collapse the singular and plural fields into
   one ordered set per kind. Pure refactor, no behaviour change, gated by the
   existing UI tests.
2. **Introduce `Appetite` and the resolver, unused.** Declare the appetite for
   every command beside its existing gate, and add a test that the resolver's
   verdict agrees with what `command_availability` decides today. Still no
   behaviour change; this is the stage that proves the model describes the
   software as it is.
3. **Flip the gates, one family at a time.** Fillet and Chamfer first — they are
   the clearest case and have the most edges to get wrong. Then Hole, Rib and
   Hole pattern; then Mirror, Pattern and Shell. Each is its own change with
   its own tests.
4. **Build the armed-state UI once**: filtered picking, the role prompt, the
   ignored-count readout, Escape.
5. **Retire the three bespoke paths.** Booleans' `boolean_tools`, Extrude's
   `awaiting_profile_pick`, and relations' `RelationOperands` all become the
   shared one. This stage removes more code than it adds, and it is the stage
   that proves the abstraction was the right one — if any of the three will not
   fit, the model is wrong and stages 1–4 are still individually sound.

## Consequences and limits

**The ribbon gets quieter and less instructive.** Buttons that used to explain
what to select now simply work, and the explanation moves to the prompt after
the press. That is better for someone who knows what they want and a change for
someone learning by hovering.

**Not every tool has an appetite.** Orbit, Frame, Theme and the panel toggles
consume nothing. They keep their current availability and are outside this
model rather than bent into it.

**This does not make tools composable.** Arming Fillet and then Chamfer
replaces the armed tool; it does not queue two operations. Sequencing features
is the history's job.

**This does not change what any tool does.** The kernel sees the same operands
it sees today. Every change here is about how the operands are gathered.

# Verification-driven CAD: the session report and probes

Artificer 0.97 gives a program the same account of a build that a person
gets from the console, and lets it ask questions of the model without
changing it. This page is the reference for both. It assumes
[`docs/art-scripting.md`](art-scripting.md) for the scripting language and
the README for the kernel API.

## 1. The session report

A session report is the machine-readable account of everything a session
did. It has a versioned shape, published as a JSON Schema in
[`docs/report-schema.json`](report-schema.json), and a test in the kernel
keeps the schema in step with the code: every report the kernel produces
validates against it, and its list of diagnostic codes is the list the
kernel source emits, no more and no fewer.

Get one:

```sh
# Run a script and print the report instead of prose
cargo run --release -p artificer-api-server -- report part.art --param width=120
cargo run --release -p artificer-api-server -- run part.art --json

# Over JSON-RPC: run a script into the session and get its report, or
# report on whatever the session has done so far
{"jsonrpc":"2.0","id":1,"method":"script.report","params":{"source":"...","params":{"width":120}}}
{"jsonrpc":"2.0","id":2,"method":"report"}
```

```rust
let mut session = Session::new();
let outcome = session.run_script(&source, &overrides, &CancellationToken::default());
let report = session.report_with(outcome.failure);
```

A failed run still produces a report. `status` is `failed`, `failure` names
the step, its command, the API error code, the kernel's diagnostic codes and
the script line, and `steps` and `body` show everything that committed
before it. From the command line a failed run also exits non-zero.

### What it contains

| Field | What it says |
| --- | --- |
| `schema_version`, `kernel_version` | `1`, and the kernel crate's version. |
| `status`, `tier` | `ok` or `failed`; `exact` unless any step fell to the faceted tier. |
| `precision` | The precision policy the session ran under. |
| `parameters` | Every numeric `param` with the value it took: the override, or the evaluated default. |
| `steps[]` | One record per committed step, in order: label, command kind, **rung**, tier, time, snapshot id and digest, topology counts, bounds, exact volume and surface area after the step, warnings, and the faces and edges it reported by role. |
| `failure` | The first failure, when there was one. |
| `body` | The current body: digest, topology, bounds, exact volume, surface area and centroid, faces by carrier kind, tier, how many steps were approximate, and **every face and edge described**. |
| `names[]` | Every name that resolves on the body: the script's `let` names first, then history names of the form `step.role[ordinal]`, each with the entity and a one-line summary. |
| `elapsed_ms` | Kernel time across the steps. |

### Rungs

Every operation in the kernel is a ladder of constructions tried in order:
the exact prism path, then the analytic Boolean engine, then the faceted
tier, for example. The **rung** names which one certified the step. Names
are stable, slash-separated paths:

| Rung | Meaning |
| --- | --- |
| `primitive/cuboid`, `primitive/revolved-annulus` | A box or a cylinder. |
| `extrusion/polygon`, `extrusion/linear-profile`, `extrusion/analytic-profile` | A new body from a sketch; the last two carry arcs and circles exactly. |
| `revolve/full-turn` | A revolved section. |
| `loft/straight`, `loft/offset-section` | An extrusion, drafted or not, built as a loft. |
| `face-feature/exact-prism` | An add or cut on a face that the exact prism path owns. |
| `face-feature/analytic-boolean` | A cut that crossed earlier geometry, rebuilt exactly by the analytic Boolean engine. |
| `face-feature/faceted` | A cut the exact rungs could not own, built on the faceted tier. |
| `drill/exact-prism`, `rib/exact-prism`, `push-pull/planar` | The kernel's own drill, rib and push/pull. |
| `edge-finish/analytic`, `edge-finish/prism`, `edge-finish/rim-blend`, `edge-finish/rim-loop-blend`, `edge-finish/logical-successor` | Exact fillets and chamfers, by the rung that carried them. |
| `edge-finish/faceted` | A fillet or chamfer on the faceted tier. |
| `boolean/prism`, `boolean/analytic` | A union, difference or intersection by the prism reduction or the general engine. |
| `mirror/exact` | A mirror: every carrier reflected as itself, faces reversed to face outward. |
| `pattern/replay` | A feature pattern; the instance steps `<label>/<n>` under it carry the rungs that built each instance. |
| `pattern/exact-instances`, `pattern/boolean` | A whole-body pattern: copies that clear one another placed as solids of one body, or copies that overlap joined through the Boolean ladder. |
| `shell/open-prism`, `shell/closed-prism` | A shell of a prism: the open face's inward offset cut as a pocket, or a core one wall in from every face enclosed as a void. |
| `shell/open-revolve`, `shell/closed-revolve` | A shell of a solid of revolution, offset in its own section. |
| `transform/similarity` | A rigid transform. |

A rung ending in `/faceted` is the approximate tier; the step also carries
a `*_FACETED_APPROXIMATION` warning. Everything else is exact: the body's
faces are analytic carriers, its volume is a closed-form integral, and its
digest is a function of that geometry alone.

### Face and edge descriptions

Every face in `body.faces` carries its carrier with the numbers that define
it (`surface`: `plane` with `origin`; `cylinder` with
`origin`, `axis`, `radius`; `cone` with `apex`, `axis`, `half_angle_degrees`;
`sphere`; `torus`), an exact `area`, a `centre` (the area centroid of a
planar face; a point at the parametric centre of a curved one), the outward
`normal` there, the number of `loops` (one, plus one per hole), a one-line
`summary`, and its `names`. Edges carry their curve (`line`, `circular_arc`,
`elliptical_arc`), exact `length`, `midpoint`, `summary` and `names`.

The same description is available for one selected entity through the
JSON-RPC method `query.describe`, which takes a selector, and through
`QueryHandle::describe` in Rust.

### Reading it as an agent

- Decide from `status` and `failure.diagnostics[].code` before reading
  anything else. The codes are the kernel's refusal vocabulary and are
  enumerated in the schema.
- Trust exact numbers as exact. `volume`, `surface_area`, `area` and
  `length` on an `exact` body are closed-form; comparing them to a
  specification needs no tolerance beyond the precision policy's
  agreement (`1e-9` mm by default).
- Treat `approximate` as a flag to act on: the geometry is a faceted
  approximation within the precision policy's budget, so a downstream
  comparison should use that budget and a person may want to know.
- Address faces by `names`. A script name is stable across parameter
  changes as long as its selector keeps finding the face; a history name
  is stable as long as the step that made the face keeps its label.

## 2. Probes

A probe asks a question of the session and answers with a number, its
unit, the tier of that number, and the method behind it. Probes never
change the session: no step is journaled, the current snapshot does not
move, and the semantic digest is the same afterwards. A test holds the
kernel to that.

```json
{"jsonrpc":"2.0","id":3,"method":"probe","params":{"probe":"volume","step":"hub"}}
{"jsonrpc":"2.0","id":4,"method":"probe","params":{"probe":"intersection_volume","a":"bracket","b":"clearance"}}
```

```rust
use artificer_kernel::api::probe::{probe, ProbeRequest};
let answer = probe(&session, &ProbeRequest::MinWall { step: None })?;
```

Every step-scoped probe takes an optional `step` label and reads the body
that step left behind; without one it reads the current body.

| Probe | Answer | Tier |
| --- | --- | --- |
| `clearance` `{a, b}` | The closest approach of two bodies, mm, with where it is and whether they are apart, touching or inside one another. | Exact between planar bodies; otherwise facet-derived, with the chord bound named in `method`. |
| `volume` `{step?}` | Exact volume, mm³. | Exact unless the body is faceted. |
| `surface_area` `{step?}` | Exact surface area, mm². | As above. |
| `area` `{face}` | Exact area of one face, mm². | As above. |
| `length` `{edge}` | Exact length of one edge, mm; an elliptic integral for an elliptical edge. | As above. |
| `distance` `{from, to}` | Minimum distance between two entities or points, mm. | Exact between points, straight-edged planar faces and straight edges; approximate, read off display facets, when a curved carrier is involved. |
| `intersection_volume` `{a, b}` | Volume common to two steps' bodies, mm³, by a committed intersection Boolean the session does not keep. `0` with a note when the bounds do not overlap or the Boolean finds no overlap. | Exact when both bodies and the Boolean are. |
| `contains` `{point, step?}` | `1` inside, `0` outside. | Exact for a polyhedral body; approximate otherwise. |
| `min_wall` `{step?}` | The thinnest wall: the shortest inward ray from any facet centre to the far side, mm, with where it is in `detail`. | Approximate. |

`from` and `to` for `distance` are measurement targets:
`{"type":"entity","<selector>"...}` or `{"type":"point","x":..,"y":..,"z":..}`,
the same shape `query.measure` takes.

If the Boolean engine cannot carry a pair of bodies, `intersection_volume`
fails with the engine's own diagnostics (`BOOLEAN_SURFACE_PAIR_UNSUPPORTED`,
`BOOLEAN_CONTACT_UNSUPPORTED`) rather than guessing.

## 3. Parameters without running

`artificer-api params part.art --json` and the JSON-RPC method
`script.params` list every `param` with its type, unit, range, default and
description, without executing the script; the report's `parameters` field
shows the values a run actually used, so the two round-trip through
`--param` overrides. See section 17 of the scripting reference.

## 4. Per-step results

Every `CommandResult` from `Session::execute`, and every entry `script.run`
returns over JSON-RPC, now carries `rung`, `tier` and `warnings` alongside
the validation diagnostics, so a caller that drives the kernel one step at
a time sees the same certification the report summarises.

## 5. Journals back to scripts, and scripts compared

A session's journal, the command list the JSON-RPC server, the command
line and Script Studio all record, can be written back as a `.art` script:

```sh
cargo run --release -p artificer-api-server -- journal session.journal.json --art session.art
```

```json
{"jsonrpc":"2.0","id":5,"method":"journal.art"}
```

```rust
let script = session.to_art(&DecompileOptions::default())?;
```

Every step becomes a `let` bound to its feature call under its own label,
so the rebuilt session has the same step labels and the same snapshot
digest. Dimensions (distances, diameters, radii, heights, spacings,
counts, drafts) become `param`s named `<label>_<field>` with their current
values (`ParamPolicy::None` writes them inline). Selectors are written as
the language spells them; a snapshot-bound reference, which no script can
write, becomes the history selector of the step that produced the entity.
A step the faceted tier built is preceded by an `// approximate` comment
and rebuilds on the same tier. The script's own `let` names, when the
session ran a script, are written after the steps.

Two scripts are compared by what they compile to, not by their text:

```sh
cargo run --release -p artificer-api-server -- diff a.art b.art --json
```

```json
{"jsonrpc":"2.0","id":6,"method":"script.diff","params":{"a":"...","b":"...","params_a":{},"params_b":{},"modules":{}}}
```

The result lists parameters added, removed or changed with old and new
values; steps added, removed, moved or changed, the last with the fields
that differ; names added, removed or renamed (the same selector under a
new name), and names that now select differently. A script diffed against
itself is empty; the command line exits non-zero when the scripts differ.

In Script Studio, **File → Pull journal into script…** decompiles a journal
file and shows this diff against the open script before replacing it;
**File → Export journal…** writes the current run's journal for another
tool. The workbench's own document model is not yet bridged to the API
journal, so a document built interactively reaches the studio through the
JSON-RPC server or the command line rather than directly.

## 6. Exact STEP

`artificer-api export part.art part.step` writes the body as AP214
`advanced_brep_shape_representation`: every face keeps its analytic
carrier (plane, cylinder, cone, sphere, torus as the five STEP elementary
surfaces), every edge its exact curve (`line`, `circle`, `ellipse`), every
coedge an `oriented_edge`, cavities as `brep_with_voids`, in millimetres.
Nothing is tessellated, so a reader recovers the volume and area the
kernel measures. `--faceted` writes the display triangles as a STEP
surface model instead, for mesh consumers. Over JSON-RPC the methods are
`export.step` and `export.step_faceted`; in Rust, `export_step`,
`export_step_bodies` (several bodies as one product) and
`export_step_faceted`. The workbench's Export dialog offers "STEP (exact
B-rep)" and "STEP (faceted)".

The exporter's own test reads every fixture back as a B-rep: references
resolve, loops chain and close, every edge is used by exactly two faces in
opposite senses, every vertex lies on its edge's curve, and every face's
`same_sense` agrees with the kernel's outward normal at the face centre.
The independent check is the OpenCascade oracle of ADR 0001:
`tools/oracle-occt/step_measure.py` imports a file with OCCT and prints
its volume and area, and with `ARTIFICER_STEP_ORACLE` pointing at it the
test compares them with the kernel's exact measures at one part in a
billion, over a fixture set covering every surface type, seams, a cavity,
a blended body and an oblique cylinder section. That oracle is a
development-machine tool, never a build dependency.

## 7. Exact mirror and feature patterns

`mirror` is exact for any body. Every carrier is reflected as itself and
every face is then reversed by the kernel's own convention, so the mirror
of a filleted part is a filleted part: the same face, edge and vertex
counts, the same volume and surface area to the last place, and the
centroid reflected through the plane. The rung is `mirror/exact`, and
features built on the mirrored body afterwards take the rungs they would
on the original.

A feature pattern (`pattern(step: ...)` in a script,
`feature_pattern` on the wire) repeats a drilled hole or a face-sketch
extrusion at rigid placements on its face: a row, or a turn about an axis
normal to the face. It does not copy geometry. Each instance is the same
feature replayed by the kernel, committed as a step `<label>/<n>` with
its own rung, so the report shows six `face-feature/exact-prism` drills
under one `pattern/replay` step, the volume of a plate with six patterned
holes is the plate less six holes in closed form, the replay is
digest-stable across sessions and journal replays, and a rim fillet on
any instance certifies through the ordinary blend ladder. A placement
that would carry an instance off its face is refused by instance number,
and a refused or failed pattern leaves nothing behind.

A whole-body pattern (`pattern` without `step:`) is exact too. Each copy
is the body under a rigid translation, so a cylinder patterns as
cylinders and a blend as blends: the report shows two caps and two half
walls per copy of a cylinder, never a facet. Copies that clear one
another along the pattern direction become solids of one body
(`pattern/exact-instances`), and the volume is the body's times the
count. Copies that overlap are merged material and go through the
Boolean ladder (`pattern/boolean`); where that ladder refuses, so does
the pattern, rather than falling to a tessellation. Stepping a prism
along one of its own axes leaves the other face planes shared between
the copies, which is that refusal. The face the
feature was built on is followed by history through every later step, so
a boss the feature itself added does not capture a `faces(">Z")`
selector.

### Shell

`shell` (`shell_snapshot` on the kernel wire) hollows a body to one
uniform wall, open at one face, at two opposite faces, or closed. It
composes constructions the kernel already certifies: the mitred loop
offset the rim blends use, the exact face cut, the prism and revolve
constructors, and the Boolean engine.

The body is read as a prism about the open face first, and as a coaxial
solid of revolution second, so a two-diameter turned hub or a tapered
post hollows in its own section with the wall measured square to the
surface. A closed shell needs no Boolean at all: the core is the body's
own boundary offset inward, which an offset that does not self-intersect
places inside the material by construction, so the core is enclosed
directly as a void and the ordinary solid validator certifies the
result. The answers are closed-form and the tests hold the
kernel to them: a `b × d × h` box open at the top has volume
`bdh − (b−2w)(d−2w)(h−w)`, a cylinder open at the top is the annular cup
`πr²h − π(r−w)²(h−w)`, a closed box keeps a void `(b−2w)(d−2w)(h−2w)`
and reports two shells, a hole through the open face keeps a wall around
it, and `probe.min_wall` reads the wall back to `1e-9` on planar walls
(to the facet chord on curved ones). A wall that leaves no floor or no
core, a body neither reading owns, two open faces that are not opposite,
or a wall thicker than half the narrowest neck are refused by name:
`SHELL_WALL_INVALID`, `SHELL_DOMAIN_UNSUPPORTED`,
`SHELL_OPEN_FACES_UNSUPPORTED`, `SHELL_SELF_INTERSECTS`.

Two refusals mark where the release stops. `SHELL_BLEND_UNSUPPORTED`
says the wall would run under a blend or a dome: its inner surface is the
offset of a torus or a sphere, and the material would lie on the far side
of the tube from where those carriers put it, which is a property of the
surface vocabulary rather than of the offset. `SHELL_OPEN_REVOLVE_UNSUPPORTED`
says the wall at an open cap could not be taken away, because that case
alone rests on the Boolean engine's analytic domain; it carries the
engine's own diagnostic underneath, and the same body shells closed.

## 8. Interference studies

An interference study is the machine-readable answer to whether an
assembly fits. It names its subjects, measures every unordered pair, and
publishes a versioned document with its own schema in
[`docs/analysis-schema.json`](analysis-schema.json), guarded by the same
kind of test the session report has: every study the kernel can produce
validates against it.

```json
{"jsonrpc":"2.0","id":1,"method":"analysis.interference","params":{"subjects":["plate","post","pin"]}}
```

```rust
use artificer_kernel::api::analysis::{interference_study, Subject};
let report = interference_study(&subjects, precision, &CancellationToken::new());
```

Each pair says whether the bodies are `clear`, `touching` or
`interfering`, how close their surfaces come, and where: a witness point
on each body. A pair that interferes also carries the volume the two
share, when the Boolean engine can carry those operands; when it cannot,
the pair keeps its measured clearance and records the engine's refusal
code, because "they overlap and this is how much" and "they overlap and
the engine could not say how much" are different answers.

No Boolean runs to find the interference itself. The work is a descent
through a bounding-volume hierarchy of each body's facets, so a study
answers for pairs the Boolean engine refuses outright — two parts sharing
a face plane, or meeting in a curve outside the line and circle
vocabulary — and a pair of parts with several thousand facets each is
measured in milliseconds rather than by comparing every facet against
every other.

The tier means what it means everywhere else. Between bodies whose faces
are all planar, the facets are the surfaces and the distance is exact.
Where a surface is curved, its facets are chords of it: the measured gap
is never smaller than the true gap and never larger than it by more than
the `bound` the pair publishes, which is one chord budget per curved
body. A study is `approximate` if any pair in it was.

Two distinctions the geometry has to get right. Touching is not
interfering: a point on a shared boundary is inside neither body, which
ray parity alone cannot decide, so the distance to the surface is
measured first. And a body wholly inside another never brings their
surfaces close at all, so containment is tested whenever the bounds meet
rather than only when the surfaces do; such a pair keeps a positive
distance, the gap to the wall around it, with the state saying it is
inside.

### Clearance profiles

A measurement is not yet an answer. "0.42 mm" says nothing on its own;
"0.42 mm, and this press fit wants 0.10 to 0.20" says the part is loose,
and "0.02 mm" against the same window says it will not go together. A
clearance profile is that window — a minimum gap that passes, and a
maximum beyond which the fit is looser than it needed to be — and a study
run against one carries a verdict for every pair.

```json
{"jsonrpc":"2.0","id":1,"method":"analysis.interference",
 "params":{"subjects":["hub","shaft"],"profile":"fdm-press"}}
```

```rust
use artificer_kernel::api::analysis::built_in_profile;
report.judge(built_in_profile("fdm-press"));
```

The kernel ships five, and none of them is privileged — a design with its
own numbers passes its own `ClearanceProfile`, and the `fit` parameter
takes one over the wire:

| Key | Window | For |
| --- | --- | --- |
| `machined-running` | 0.02–0.08 mm | A milled or turned part that has to turn or slide in service. |
| `resin-fine` | 0.05–0.15 mm | Masked stereolithography, where the layer is thin and the part is stiff. |
| `fdm-press` | 0.10–0.20 mm | A fused-filament part meant to be pushed together and stay together. |
| `fdm-sliding` | 0.30–0.50 mm | A fused-filament part that has to move after it is assembled. |
| `assembly` | 0 mm and over | No fit at all: parts must simply not occupy the same space. |

Each pair earns one of three verdicts. `pass` is the gap the fit asked
for. `too_close` is nearer than the profile allows, or an overlap
outright, and it is the only verdict that fails a study — `failing` counts
them and `worst_fit` names the tightest, whose witness points are where on
the two bodies that reading was taken. `loose` is clear by more than the
fit needed: not a failure, but a part that was meant to be held and is
not.

Two edges are worth stating. Contact is judged on the measurement rather
than on the state, so two bodies that touch have a gap of zero and zero is
below every window whose minimum is positive; the assembly check, whose
minimum is zero, is the one that passes them. And the open-ended profile
publishes no upper bound rather than an infinite one, because infinity is
not a JSON number and this document is published.

Judging is not measuring. `judge` re-reads the closest approach each pair
already has, so changing the fit changes the verdicts, the heat map's
window and nothing else; withdrawing it leaves every measurement standing.

### The heat map

The pair table says which pairs fail. The heat map says where on the
parts they fail, and it is the same measurement read at a different
resolution: for every corner of every display facet, the signed clearance
to the nearest of the other bodies.

```rust
use artificer_kernel::api::analysis::clearance_fields;
let fields = clearance_fields(&subjects, &CancellationToken::new());
```

Positive is a gap; negative is penetration, and its magnitude is how far
inside the nearest other body that corner sits. A body with nothing to
measure against reads infinite rather than zero.

Two things about the sampling are worth stating plainly. Readings are
taken at facet corners rather than facet centres, so the renderer
interpolates the measurement across each facet and a tessellated cylinder
shows the clearance over its wall rather than showing its own
tessellation. And corners alone would miss a collision that falls wholly
inside a facet — a pin driven through the middle of a disc has every
corner of that disc out on its rim — so each facet's centre is read too,
and a facet whose centre is inside another body is painted as a collision
throughout. That over-states a collision by at most one facet and never
hides one, which is the direction a fit check has to err in.

The workbench paints the readings over the bodies through the viewport's
per-vertex colour channel, on both the software and the GPU fill paths,
with a legend naming the bands. A collision takes a colour no gap can
take, so "these parts pass through one another" never reads as "these
parts are close". The readings are bound to the tessellation they were
measured on: a body rebuilt by an edit simply has no reading until the
study is run again.

The scale is the profile's own window when there is one, so the picture
and the table agree — green on the model means the same thing as `pass` in
the pair list, red is too close, blue is looser than needed. Without a
profile there is no window to draw, so the readings are ramped over their
own range instead, from the tightest to the ninetieth percentile rather
than to the largest: one body parked far across the workspace would
otherwise stretch the scale until every real fit read as tight.

## 9. Joints, and where the parts actually are

A document stores each component's *assembled* pose: where it sits when
every joint is at zero. A joint says how that pose is allowed to change.
The solver in `artificer_model::kinematics` turns the two into the third
thing the viewport, an interference study and an export all need — the
world pose of every component at a given set of driver values.

```rust
use artificer_model::kinematics::{solve, JointDriver};
let posed = solve(&document, &[JointDriver::new(hinge, angle)])?;
```

A revolute joint carries an origin and an axis in the assembled world
frame, which is where they were picked. Driving it by θ turns the child
about that world line, and the child's whole subtree with it: turning a
hinge carries the door, and the handle on the door. So each component gets
a *motion* — the rigid transform between where it was assembled and where
the drivers have put it — and its world pose is that motion applied to its
assembled pose. Every motion is the identity at zero, which is what makes
the assembled document the thing the drivers move away from rather than a
separate configuration nobody authored.

A fixed joint contributes the identity, which is exactly what "fixed"
means: the child follows its parent and adds nothing of its own. A
disabled joint does the same — it is still a structural edge, it simply
has no coordinate to set. A component with no parent joint is a root and
stands where it was put; grounding is what forbids a component a parent,
not what pins it.

Five things are refused by name rather than guessed at: a driver for a
joint that is not in the document, for a fixed one, or for a disabled one;
a second driver for a joint that already has one; and a driver outside the
joint's own limits. That last one is a refusal rather than a clamp on
purpose — a limit is a promise the model makes about the mechanism, and a
sweep that silently stopped at the stop would report clearances the
mechanism never reaches. Cycles are refused too. The document's own
editing rules already prevent them, but a document that arrived from disk
has not been through those rules, and a solver that loops forever on one
is worse than a solver that names it.

In the workbench each drivable joint gets a coordinate, and posing one is
not an edit: the document's stored poses do not move, only where the parts
are drawn. The animation drives those coordinates — a joint with limits
sweeps between them and back, one without turns continuously — and while
it holds them the coordinates are its to set, which resetting the phase
hands back. The turntable that spun the active body on the spot is what a
document with no joints still gets, because on such a document there is no
mechanism to animate.

## 10. Not in this release

The report does not yet carry an inertia tensor or principal axes; the
measures are volume, surface area, centroid and bounds. An interference
study is static: it measures the poses the bodies are in, and driving a
mechanism through its travel while it measures is the next slice. Joints
are fixed and revolute; sliding, cylindrical and ball joints are not in
the vocabulary yet, and a mechanism whose loop closes is refused rather
than solved. Shell covers
prisms and solids of revolution; a blended or domed body is refused
rather than approximated, and opening the cap of a cone waits on the
Boolean engine. Probes read the
current session only; comparing two reports is a job for the caller until
the semantic diff lands.

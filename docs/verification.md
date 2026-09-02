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
| `mirror/faceted`, `pattern/faceted` | Mirror and linear pattern, which are faceted in this release. |
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

## 6. Not in this release

The report does not yet carry an inertia tensor or principal axes; the
measures are volume, surface area, centroid and bounds. Probes read the
current session only; comparing two reports is a job for the caller until
the semantic diff lands.

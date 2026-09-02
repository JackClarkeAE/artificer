<div align="center">

# Artificer

**An exact boundary-representation geometry kernel, and the parametric CAD workbench built on it. Pure Rust, from scratch.**

[![CI](https://github.com/JackClarkeAE/artificer/actions/workflows/ci.yml/badge.svg)](https://github.com/JackClarkeAE/artificer/actions)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Commercial licence available](https://img.shields.io/badge/License-Commercial-purple.svg)](#licensing)
[![Rust: 1.95+](https://img.shields.io/badge/Rust-1.95+-orange.svg)](https://www.rust-lang.org)

[The Kernel](#the-artificer-kernel) •
[Built for AI-driven CAD](#built-for-ai-driven-cad) •
[The Workbench](#the-artificer-workbench) •
[Quickstart](#quickstart) •
[Architecture](#architecture) •
[Roadmap](#roadmap) •
[Licensing](#licensing)

<br/>

![Artificer Workbench](apps/workbench/tests/snapshots/workbench_two_visible_bodies.png)

</div>

---

## Three products, one repository

Artificer is three things, deliberately kept apart:

| | What it is | Where it lives | Depends on |
|---|---|---|---|
| **Artificer Kernel** | A standalone exact B-rep modelling kernel with its programmatic API built in: a Rust API, a JSON-RPC 2.0 server, the `.art` scripting language, headless PNG/SVG rendering, and STL/OBJ export. | [`crates/kernel`](crates/kernel) | Nothing but its own geometry, compute, and protocol crates. No UI, no GPU, no C or C++. |
| **Artificer Workbench** | A native desktop parametric CAD application: sketching, features, assemblies, a part library, and a parametric history. | [`apps/workbench`](apps/workbench) | The kernel, through the same protocol every other client uses. |
| **Artificer Script Studio** | A live `.art` visualiser in the OpenSCAD shape: the script on the left, the exact model on the right, a customizer built from the script's parameters, and a console that points at the failing line. | [`apps/script-studio`](apps/script-studio) | The kernel, through its API session, and the workbench's viewport and theme. |

The separation is enforced, not aspirational. The CI architecture audit fails the build if a UI or rendering dependency enters the kernel crate, and the kernel is exercised end to end by a headless test suite that never opens a window. You can embed the kernel in your own application, drive it from another language over JSON-RPC, or script it from a file, and you get exactly the same geometry the workbench would build.

---

## The Artificer Kernel

Every surface, curve, and boundary in an Artificer model is analytic. There are no mesh approximations standing in for solids, no tolerance stacking, and no healing heuristics that quietly change your geometry. An operation either produces a certified manifold solid or refuses with a named, structured reason.

### Exact by construction

- **Analytic geometry only.** Planes, cylinders, cones, spheres, and tori as surfaces; lines and circles as curves. A fillet on a cylinder's rim at its own radius is an exact sphere, not a patch of triangles.
- **Closed-form calculus.** Volume, surface area, centroid, and inertia are integrated analytically over the true surfaces. Test gates pin them against independent derivations at one part in a billion.
- **Transactional validation.** Every result passes Euler–Poincaré, edge-use, loop-orientation, locus, and self-intersection checks before it is published. A snapshot that fails is never returned.
- **Certified or refused.** Each operation is a ladder of exact strategies. When none applies, the kernel says which rung refused and why, with a diagnostic code, rather than guessing. The one remaining approximate tier, for cuts that cross curved voids, is labelled as an approximation in its report.
- **Deterministic and content-addressed.** Snapshots carry a semantic digest. The same commands produce bit-identical models on every platform, so replays, caches, and audits agree.

### The API is part of the kernel

The programmatic surface lives in `artificer_kernel::api` and ships with the kernel, not beside it. Three entry points cover most uses:

**Rust.** Embed the kernel directly:

```rust
use artificer_kernel::CancellationToken;
use artificer_kernel::api::{ApiCommand, Session};
use artificer_protocol::Point3;

let mut session = Session::new();
let token = CancellationToken::default();

session.execute(ApiCommand::MakeBox {
    label: "cube".into(),
    origin: Point3::new(0.0, 0.0, 0.0),
    size: [50.0, 50.0, 50.0],
}, &token)?;

let measures = session.snapshot.measures();
println!("Exact volume: {:.6} mm³", measures.volume);
println!("Bounds: {:?}", session.query().bounds()?);
```

**JSON-RPC 2.0.** Run the kernel as a headless service on stdin/stdout, one request per line, batches and notifications included:

```sh
cargo run --release -p artificer-api-server -- serve
```

```json
{
  "jsonrpc": "2.0",
  "id": "1",
  "method": "execute",
  "params": {
    "type": "make_box",
    "label": "base_block",
    "origin": { "x": 0.0, "y": 0.0, "z": 0.0 },
    "size": [100.0, 50.0, 25.0]
  }
}
```

Domain errors come back as JSON-RPC error `-32000` with the structured `ApiError` in `error.data`: a code, a plain-language message, a suggestion, candidate entities where a selector was ambiguous, and the kernel's own diagnostics.

**`.art` scripts.** A small parametric language evaluated straight into kernel commands:

```text
// bracket.art — parametric bracket with mounting holes
param width: f64 = 100.0;
param depth: f64 = 50.0;
param thickness: f64 = 10.0;

let base = box(origin: [0, 0, 0], size: [width, depth, thickness], label: "base");
let top = base.face("top_face");

// Hole centres are in the face's own frame, whose origin is the face centre.
drill(face: top, center: [-30.0, 0.0], diameter: 8.0, depth: thickness, label: "hole_l");
drill(face: faces(">Z"), center: [30.0, 0.0], diameter: 8.0, depth: thickness, label: "hole_r");

// Fillet every vertical edge of the block at once.
fillet(edges: edges("|Z"), radius: 2.0, label: "soften");
```

```sh
cargo run --release -p artificer-api-server -- run bracket.art --param width=120
cargo run --release -p artificer-api-server -- snapshot bracket.art bracket.png
cargo run --release -p artificer-api-server -- export bracket.art bracket.stl
```

The whole API is reachable from a script, one builtin per command, with named arguments and angles in degrees:

| Builtin | Makes |
|---|---|
| `box(size:, origin:, label:)`, `cylinder(radius: or diameter:, height:, center:, axis:, label:)` | A new body. |
| `sketch(on: "XY" \| "XZ" \| "YZ" \| face, entities: [...], label:)` with `line(start:, end:)`, `circle(center:, radius:)`, `arc(center:, radius:, start_angle:, end_angle:)`, `rect(origin: or center:, width:, height:)` | A profile on a plane or a face. Lines and arcs chain into loops; nested loops become holes. |
| `extrude(sketch:, distance:, operation: "new" \| "add" \| "cut", draft:, regions:, label:)` | A prism, or a drafted loft for a new body. |
| `revolve(sketch:, axis:, axis_origin:, angle:, operation:, label:)` | A solid of revolution. |
| `drill(face:, center:, diameter:, depth:)`, `push_pull(face:, distance:)`, `fillet(edges:, radius:)`, `chamfer(edges:, distance:)` | Face and edge features. |
| `mirror(origin:, normal:)`, `pattern(direction:, spacing:, count:)` | Whole-body operations. |
| `union(target:, tool:)`, `difference(target:, tool:)`, `intersection(target:, tool:)` | Booleans between two steps. |
| `faces(">Z")`, `edges("\|Z")`, `nearest(point:, kind:)`, `step.face("role")`, `step.edge("role")` | Selectors. |
| `sqrt abs floor ceil round min max clamp pow hypot sin cos tan asin acos atan atan2`, `pi` | Arithmetic. |

Errors name their line and column, so an editor can point at them. Open the same file in **Artificer Script Studio** to edit it live against the kernel's viewport, with the `param` lines as a customizer.

### What the kernel does today

- Primitives, planar-profile extrusion with holes and islands, revolve, drafted extrusion as an exact loft to the profile's offset section, push/pull, holes, ribs, mirror, and linear patterns.
- Exact chamfers and constant-radius fillets, including fillets that run around a whole hole rim, with spherical corners where the rim turns in and elliptical mitre seams where it turns out.
- Regularized Boolean union, difference, and intersection, with an exact engine for plane and cylinder operands and a faceted fallback that says so.
- Analytic surface–surface intersections across the vocabulary, published as a supported-domain matrix.
- Geometric selectors (`faces(">Z")`, `edges("|Z")`, by extremum, by type, parallel to a direction) that resolve deterministically or refuse with candidates.
- Headless tessellation at display or authoritative chord budgets, SVG and PNG snapshots from any camera, and STL/OBJ export.

---

## Built for AI-driven CAD

Language models and agents are good at saying what a part should be and poor at nudging triangles. A kernel that serves them well has to be declarative, honest about failure, and inspectable without a screen. Artificer was shaped by those requirements:

- **A closed, typed command vocabulary.** Every operation is a serialisable command with named fields and documented domains. There is no hidden UI state to reproduce; a model is its command journal.
- **Refusals are data.** An operation that cannot be certified returns a diagnostic code, the reason, and where it applies a suggestion or the list of candidate entities. An agent can read the refusal and try the next thing instead of inheriting broken geometry.
- **Stable references.** Faces and edges are addressed by geometric selectors and by persistent, provenance-tracked references, so a plan written before the model exists still resolves after it is built.
- **Deterministic replay.** Journals replay to bit-identical snapshots with content digests, which makes results cacheable, diffable, and safe to verify independently.
- **Headless eyes.** Snapshots render to PNG or SVG from standard or explicit cameras, so a vision-capable model can look at what it built without a GPU or a window.
- **Exact measurements.** Volumes, areas, centroids, bounds, and distances are closed-form answers, not mesh estimates, so a planner can trust a number it reads back.

The same properties are what make the kernel a sound foundation for any programmatic CAD: generative design, automated tooling, cloud pipelines, and your own front end.

---

## The Artificer Workbench

The desktop application is the reference client for the kernel and a complete single-part and small-assembly modeller in its own right.

<div align="center">

![Sketching Mode](apps/workbench/tests/snapshots/workbench_sketch_xy_rectangle.png)

*Sketching with live profile detection: every bounded region is selectable the moment it closes.*

</div>

- **Sketching.** Lines, rectangles, circles, arcs, polygons, slots, splines, text set from a bundled typeface as exact outlines, fillets, chamfers, trims, patterns, relations, and dimensions. Intersecting geometry splits into separately selectable regions. Live dimensions edit in place.
- **Features.** Extrude, drafted extrude, revolve, push/pull, holes, ribs, mirror, patterns, chamfers, and fillets, each staged behind one confirmation gate and recorded in an editable parametric history.
- **Face sketches.** Sketch on any planar face with the body always in view; project the geometry hidden below the surface as an x-ray when you need to line up with it.
- **Assemblies and library.** A content-addressed part library, rigid placements, grounding, and revolute joints with live motion.
- **Documents.** Several documents open at once in tabs along the top of the window; a portable native document format with a versioned schema.
- **Viewport.** Exact silhouettes, hidden-line rendering, smooth shading from analytic normals, a view cube, and themes.

---

## Artificer Script Studio

The third program is for people who would rather type a model than draw one. Script Studio is a live `.art` editor in the shape OpenSCAD made familiar, built on the same kernel, viewport, and theme as the workbench.

<div align="center">

![Script Studio](docs/images/script-studio.png)

*The flanged hub example: the script, the exact model it builds, its parameters as a customizer, and every step in the console.*

</div>

- **Live.** Every edit re-runs the script on a worker thread after a short pause; a run that an edit supersedes is cancelled rather than waited for, and the last good model stays on screen while you type.
- **Customizer.** The script's `param` lines become a panel of values you can drag. A dragged value re-runs the script without touching the text, and one click puts the script's own default back.
- **Console.** Every step lists its label, topology, and time. A parse or evaluation error names its line and column, a failing step names the line that labels it, and clicking the error puts the cursor there.
- **Editor.** Syntax colouring for the `.art` vocabulary, line numbers, and the error's line washed in red.
- **Files.** Open and save scripts, drop a file onto the window, export the model as STL or OBJ, and start from the bundled examples.

```sh
cargo run --release -p artificer-script-studio -- crates/kernel/examples/flanged_hub.art
```

---

## Quickstart

### Prerequisites

- Stable Rust 1.95 or newer.
- For the workbench only: a GPU toolchain (Metal on macOS, Vulkan or DX12 on Windows, Vulkan with Wayland or X11 on Linux). The kernel and its server need none.

### Build and run

```sh
git clone https://github.com/JackClarkeAE/artificer.git
cd artificer

# The kernel and its API: the headless test suite
cargo test -p artificer-kernel

# The JSON-RPC server on stdin/stdout
cargo run --release -p artificer-api-server -- serve

# Run, render, or export an .art script
cargo run --release -p artificer-api-server -- run crates/kernel/examples/bearing_mount.art

# The desktop workbench
cargo run --release -p artificer-workbench

# The live .art visualiser, on a script of your own
cargo run --release -p artificer-script-studio -- crates/kernel/examples/flanged_hub.art

# Everything
cargo test --workspace
```

### Prebuilt binaries

Installers are published on the [Releases page](https://github.com/JackClarkeAE/artificer/releases). Each one carries the workbench and Script Studio side by side:

- Windows: `Artificer-Setup.exe` installs `Artificer.exe` and `ArtificerScriptStudio.exe`
- Linux: `Artificer.AppImage`, with `ArtificerScriptStudio` alongside it in the plain archive
- macOS (Apple Silicon): `Artificer-macOS-arm64.zip` with `Artificer.app` and `Artificer Script Studio.app`

---

## Architecture

| Layer | Crate | Purpose |
|---|---|---|
| **Kernel** | [`crates/kernel`](crates/kernel) | The exact B-rep kernel: topology, analytic surfaces, strategy ladders, validation, measures, tessellation, and the `api` module (sessions, selectors, JSON-RPC server, `.art` scripting, snapshots, export). |
| **Geometry** | [`crates/geometry`](crates/geometry) | Certified predicates, interval arithmetic, planar and spatial intersection mathematics. |
| **Compute** | [`crates/compute`](crates/compute) | The work pool, cancellation, and performance spans the kernel runs on. |
| **Protocol** | [`crates/protocol`](crates/protocol) | The serialisable command, snapshot, and diagnostic vocabulary shared by every client. |
| **Sketch** | [`crates/sketch`](crates/sketch) | Exact 2D authoring: recipes, constraints, the arrangement into regions, profile compilation, text outlines. |
| **Model** | [`crates/model`](crates/model) | The parametric document: features, persistent references, parameters, journals, and the native file schema. |
| **Kernel server** | [`apps/api-server`](apps/api-server) | The command-line front for the kernel API: `serve`, `run`, `snapshot`, `export`, `journal`. |
| **Presentation** | [`crates/viewport`](crates/viewport), [`crates/sketch-ui`](crates/sketch-ui), [`crates/ui-core`](crates/ui-core) | The 3D viewport, the sketch canvas, and the shared theme and widgets. None of these can see the kernel's internals. |
| **Workbench** | [`apps/workbench`](apps/workbench) | The desktop application. |
| **Script Studio** | [`apps/script-studio`](apps/script-studio) | The live `.art` visualiser: editor, customizer, console, and the shared viewport, driving the kernel through its API session. |
| **Test kit** | [`crates/testkit`](crates/testkit), [`apps/cli`](apps/cli) | Deterministic cases, journals, and the conformance runner. |

The dependency rules between these layers are checked by `scripts/check-architecture-boundaries.sh` on every CI run. Design decisions are recorded as ADRs under [`docs/architecture/adr`](docs/architecture/adr), and the kernel programme itself in [`docs/architecture/geometry-kernel`](docs/architecture/geometry-kernel).

---

## Roadmap

- [x] Analytic B-rep topology, primitives, planar profiles, exact calculus.
- [x] Regularized Booleans, chamfers, fillets, and hole-rim blends.
- [x] Parametric documents, assemblies, part library, joints.
- [x] The kernel API: Rust, JSON-RPC, `.art` scripts, headless snapshots and export.
- [x] Multi-document workbench, sketch text, drafted extrusion as the first loft rung.
- [ ] Sweeps along paths and lofts between arbitrary sections; draft on existing faces; shell.
- [x] The ellipse curve, first slice: the mitre seam of a fillet turning a sharp corner, so fillets round square holes and L-shaped rims are exact.
- [x] Oblique plane sections of cylinders on the same curve, through the analytic Boolean: angled holes, mitred cylinder ends, oblique cuts of round bodies.
- [ ] Oblique cone sections, and pipe tees (equal cylinders crossing) on the same ellipse.
- [ ] Native STEP read and write with exact surfaces, IGES import, DXF drawing sheets.

---

## Licensing

Artificer is dual-licensed.

**Open source: AGPL-3.0-or-later.** The kernel and the workbench are free software under the [GNU Affero General Public License, version 3 or later](LICENSE). You may use, study, modify, and redistribute them, including for commercial purposes, provided you honour the licence: derived works and network services built on Artificer must themselves be released under the AGPL, with their source available to their users.

**Commercial licence.** Organisations that want to build proprietary or closed-source products, cloud services, or internal tools on the Artificer Kernel or Workbench without the AGPL's copyleft obligations can obtain a commercial licence. It covers embedding the kernel in your own software, running it as a service, and shipping the workbench under your own terms, with the same code and the same guarantees.

| You are | You need |
|---|---|
| An individual, a student, a researcher, or an open-source project | Nothing more: the AGPL applies. |
| A company whose product or service will itself be released under the AGPL | Nothing more: the AGPL applies. |
| A company shipping or hosting a proprietary product built on Artificer | A commercial licence. |

To enquire about a commercial licence, open an issue on GitHub titled "Commercial licence enquiry" or contact the maintainers. Contributions to the repository are accepted under the AGPL-3.0-or-later.

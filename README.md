<div align="center">

# Artificer

**The exact boundary-representation geometric modeling kernel and parametric CAD suite, written from scratch in pure Rust.**

[![CI](https://github.com/JackClarkeAE/artificer/actions/workflows/ci.yml/badge.svg)](https://github.com/JackClarkeAE/artificer/actions)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Dual License: Commercial](https://img.shields.io/badge/License-Commercial-purple.svg)](#licensing)
[![Rust: 1.95+](https://img.shields.io/badge/Rust-1.95+-orange.svg)](https://www.rust-lang.org)

[Key Features](#key-features) •
[The Kernel](#the-exact-geometry-kernel) •
[Programmable API & Server](#programmable-api--headless-engine) •
[Interactive Workbench](#desktop-cad-workbench) •
[Quickstart](#quickstart) •
[Architecture](#architecture) •
[Roadmap](#roadmap)

<br/>

![Artificer Workbench](apps/workbench/tests/snapshots/workbench_two_visible_bodies.png)

</div>

---

## What is Artificer?

Artificer is an industrial-grade, pure-Rust mechanical CAD ecosystem powered by its own custom **exact boundary-representation (B-rep) geometry kernel**.

Unlike traditional CAD software that relies on legacy C++ kernels with floating-point tolerance approximations and mesh-healing heuristics, Artificer represents every surface, curve, and boundary analytically. Operations either evaluate to a mathematically certified manifold solid or refuse cleanly with a deterministic reason.

Whether you are designing physical mechanisms in the **desktop CAD workbench**, automating parametric model generation via the **headless JSON-RPC server**, or embedding the **geometry kernel** directly into your own Rust applications, Artificer guarantees reproducible, bit-identical precision across platforms.

---

## Key Features

- 📐 **100% Exact Analytic Geometry** — Lines, circles, planes, cylinders, cones, tori, and spheres are evaluated directly from closed-form equations. No chord approximations, no tolerance accumulation, and no "nearly watertight" solids.
- ⚡ **Closed-Form Calculus** — Volume, surface area, center of mass, and moments of inertia are integrated analytically over true surface topologies rather than estimated from facet meshes.
- 🔄 **Deterministic Parametric History** — All modeling operations are recorded as pure, transactional journal steps. Every model state is content-addressed with cryptographic digests for bit-identical rebuilds.
- 🌐 **Headless Automation & API Server** — First-class JSON-RPC 2.0 API server, Rust client SDK, and `.art` scripting DSL for cloud CAD workflows, generative engineering, and automated test pipelines.
- ✏️ **Flow-State 2D Sketching** — Instant planar loop and multi-region detection, live in-canvas dimension editing, parametric sketch recipes, and automatic profile classification.
- 🔩 **Assemblies & Part Library** — Integrated component catalogs, rigid 3D spatial placements, grounding semantics, and kinematic revolute joints with live motion simulation.
- 📦 **Dual-Tier Faceted & Exact Booleans** — Robust topological difference, union, and intersection operations across arbitrary orientations and non-manifold interactions.
- 🎨 **Modern Native GPU Desktop App** — Ultra-responsive UI built with `wgpu` and `egui`, featuring dynamic view cube orientation, realtime edge-contrast rendering, and custom theme engines.

---

## The Exact Geometry Kernel

At the core of Artificer is `crates/kernel`, a self-contained B-rep engine designed without external C/C++ dependencies:

```
                  ┌─────────────────────────────────────────┐
                  │          Geometry Kernel API            │
                  └────────────────────┬────────────────────┘
                                       │
            ┌──────────────────────────┴──────────────────────────┐
            ▼                                                     ▼
┌───────────────────────────────┐             ┌───────────────────────────────────┐
│     Authoritative B-rep       │             │       Certified Predicates        │
│  • Half-Edge Topology         │             │  • Exact Orientation Filters      │
│  • Analytic Carrier Surfaces  │             │  • Closed-Form Intersections      │
│  • Manifold Validation Gates  │             │  • Interval Arithmetic Arithmetic │
└───────────────┬───────────────┘             └─────────────────┬─────────────────┘
                │                                               │
                └──────────────────────┬────────────────────────┘
                                       ▼
                  ┌─────────────────────────────────────────┐
                  │    Exact Calculus & Solid Operations    │
                  │  • Analytic Surface Integrals (Mass)    │
                  │  • Toric / Spherical Variable Blends    │
                  │  • Faceted & Exact BSP Boolean Engines  │
                  └─────────────────────────────────────────┘
```

### Analytic Topology
In Artificer, topological faces point to exact mathematical surfaces:
- **Planes**: $P(u, v) = O + u\vec{U} + v\vec{V}$
- **Cylinders**: $C(u, v) = O + R(\cos(u)\vec{U} + \sin(u)\vec{V}) + v\vec{W}$
- **Tori**: Swept circular cross-sections with exact major and minor radii.
- **Spheres**: Exact spherical quadrics.

When you fillet a cylinder's rim at its own radius, Artificer produces an exact mathematical sphere carrier, preserving analytic surface continuity throughout downstream operations.

### Transactional Solid Validation
Every kernel mutation must pass rigorous topological manifold checks:
- **Euler-Poincaré Formula Verification**: $V - E + F - (L - F) = 2(S - G)$
- **Edge-Use Counting**: Exactly two coedges per manifold edge with opposite loop orientations.
- **Closed Loop Orientation**: Counter-clockwise outer bounds and clockwise inner voids.
- **Self-Intersection Free**: All faces, edges, and vertices are verified non-overlapping.

---

## Programmable API & Headless Engine

Artificer is built from the ground up to be scriptable, automatable, and embeddable.

### 1. JSON-RPC 2.0 API Server (`apps/api-server`)
Run headless CAD pipelines on servers, in Docker containers, or inside CI/CD workflows:

```sh
# Start the API server on localhost
cargo run --release -p artificer-api-server -- --port 9000
```

Send commands using standard JSON-RPC:
```json
{
  "jsonrpc": "2.0",
  "id": "1",
  "method": "execute",
  "params": {
    "command": {
      "kind": "make_box",
      "origin": [0.0, 0.0, 0.0],
      "size": [100.0, 50.0, 25.0],
      "label": "base_block"
    }
  }
}
```

### 2. Parametric Scripting DSL (`.art`)
Write clean, readable parametric scripts evaluated directly by the kernel:

```rust
// bracket.art — Parametric bracket with mounting holes
param width: f64 = 100.0;
param depth: f64 = 50.0;
param thickness: f64 = 10.0;

let base = box(origin: [0, 0, 0], size: [width, depth, thickness], label: "base");
let top = base.top_face;

drill_hole(face: top, center: [-30.0, 0.0], diameter: 8.0, depth: thickness, label: "hole_l");
drill_hole(face: top, center: [ 30.0, 0.0], diameter: 8.0, depth: thickness, label: "hole_r");
```

### 3. Rust Native API (`crates/api`)
Embed CAD capabilities directly inside your Rust crates:

```rust
use artificer_api::{Session, ApiCommand, CancellationToken};
use artificer_protocol::Point3;

let mut session = Session::new();
let token = CancellationToken::default();

session.execute(ApiCommand::MakeBox {
    label: "cube".into(),
    origin: Point3::new(0.0, 0.0, 0.0),
    size: [50.0, 50.0, 50.0],
}, &token)?;

let measures = session.query().mass_properties(None)?;
println!("Exact Volume: {:.6} mm³", measures.volume);
```

---

## Desktop CAD Workbench

The interactive studio (`apps/workbench`) provides an agile, professional CAD environment:

<div align="center">

![Sketching Mode](apps/workbench/tests/snapshots/workbench_sketch_xy_rectangle.png)

*Continuous 2D sketch constraint solver with real-time profile classification*

</div>

- **Dynamic Sketch Profiling**: Sketching automatically discovers bounded planar cells, holes, and island regions without manual profile closures. Shift-click to select and extrude multiple regions simultaneously.
- **Parametric Feature Timeline**: Reorder, suppress, or modify dimensions on historical steps with automated atomic dependency rebuilds.
- **Direct Surface Modeling**: Push/pull faces, create midplanes and datum planes, add bosses, blind pockets, through-cuts, and chamfers directly from model surface selections.
- **Live 3D Viewport**: Zero-lag GPU rendering with hidden line culling, silhouette boundary classification, dynamic view cube, orthographic/trimetric projection modes, and customizable UI palettes.

---

## Quickstart

### Prerequisites
- Stable Rust (1.95 or newer)
- Modern C/GPU toolchain (Metal on macOS, Vulkan/DX12 on Windows, Vulkan/Wayland/X11 on Linux)

### Installation & Launch

```sh
# Clone the repository
git clone https://github.com/JackClarkeAE/artificer.git
cd artificer

# Launch the interactive desktop CAD workbench
cargo run --release -p artificer-workbench

# Run the full kernel & workbench test suite
cargo test --workspace

# Start the headless API server daemon
cargo run --release -p artificer-api-server -- --port 8080
```

### Prebuilt Desktop Binaries
Standalone packages are published on the [Releases Page](https://github.com/JackClarkeAE/artificer/releases):
- **Windows**: `Artificer-Setup.exe`
- **Linux**: `Artificer.AppImage`
- **macOS**: `Artificer-macOS-arm64.zip` (Apple Silicon)

---

## Architecture

The Artificer workspace is organized into modular, independently testable crates:

| Layer | Crate | Purpose |
|---|---|---|
| **Core Kernel** | [`crates/kernel`](crates/kernel) | Authoritative analytic B-rep modeling, Euler topology verification, torus/sphere blends, mass properties |
| **Geometry** | [`crates/geometry`](crates/geometry) | Certified interval arithmetic predicates, orientation filters, ray/surface intersection math |
| **Compute** | [`crates/compute`](crates/compute) | Hardware-accelerated spatial classifiers and SIMD/GPU evaluation primitives |
| **Sketch Engine** | [`crates/sketch`](crates/sketch) | Exact 2D authoring, arrangement cell decomposition, loop stitching, and profile extraction |
| **Protocol** | [`crates/protocol`](crates/protocol) | Zero-copy serializable command vocabulary connecting front-ends, APIs, and the kernel |
| **Parametric Model** | [`crates/model`](crates/model) | Content-addressed feature DAG, parameter bindings, journal replay, and document schema |
| **API & Exporters** | [`crates/api`](crates/api) | Programmable Rust API, `.art` script parser, headless renderer, STL, OBJ, and STEP interchange |
| **API Server** | [`apps/api-server`](apps/api-server) | Standalone JSON-RPC 2.0 daemon and headless batch runner |
| **Viewport Engine** | [`crates/viewport`](crates/viewport) | 3D rendering pipeline, silhouette curves, screen-space depth sorting, and gizmo manipulators |
| **Sketch UI** | [`crates/sketch-ui`](crates/sketch-ui) | 2D canvas interactions, snap systems, live dimension boxes, and geometry tool widgets |
| **Workbench** | [`apps/workbench`](apps/workbench) | Complete native desktop CAD studio (egui/wgpu) |
| **CLI & Testkit** | [`apps/cli`](apps/cli) / [`crates/testkit`](crates/testkit) | Deterministic test harnesses, headless journal regression suites, and verification tools |

---

## Roadmap

- [x] **M1–M5: Core Geometry & Primitives** — Analytic B-rep topology, planar profiles, primitive solids, exact calculus.
- [x] **M6: Booleans & Blends** — Robust faceted boolean tier, toric/spherical fillets, and chamfers.
- [x] **M7: Assemblies & Parametric Architecture** — Content-addressed part library, joint kinematics, parametric journal.
- [x] **M7.5: Headless API & Scripting** — JSON-RPC daemon, `.art` DSL, programmatic query engine.
- [ ] **M8: Advanced Kinematic Construction** — Guide-rail path sweeps, multi-section lofts, draft angles, and surface offsets.
- [ ] **M9: Direct B-rep Deformations** — Local surface replacement, face twisting, and freeform NURBS trimming.
- [ ] **M10: Production Interoperability** — Native STEP AP203/AP214/AP242 reader and writer, IGES import, and DXF drawing sheets.

---

## Licensing

Artificer is offered under a **dual-licensing model**:

- **Open Source (AGPLv3)**: The default public license is the [GNU Affero General Public License v3.0](LICENSE) (`AGPL-3.0-or-later`). You are free to use, modify, and redistribute Artificer under open-source copyleft terms.
- **Commercial / Enterprise Licensing**: For organizations wishing to integrate Artificer, its geometry kernel, or the headless API into proprietary software, cloud platforms, or closed-source commercial applications without copyleft obligations, commercial licenses are available. Contact the maintainers or open an inquiry on GitHub.

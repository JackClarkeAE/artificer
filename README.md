# Artificer

**Exact parametric CAD, in pure Rust.**

Artificer is a mechanical CAD application built on its own boundary-representation kernel. Every curve and surface is carried as analytic geometry — lines, circles, planes, cylinders, cones, tori, spheres — and every operation either produces a certified result or refuses cleanly, leaving your model untouched. There is no tolerance slider and no "nearly closed" solid: volume, area, and centre of mass come from closed-form integrals over the real surfaces, so a measurement is the measurement, not a mesh estimate.

![Artificer workbench](apps/workbench/tests/snapshots/workbench_two_visible_bodies.png)

## Why exact?

Mainstream kernels approximate: surfaces meet within a tolerance, booleans heal gaps, and a model that looks solid can fail downstream in CAM, simulation, or 3D printing. Artificer takes the opposite bet. Geometry is exact by construction, validation is transactional, and anything the kernel cannot certify is refused with a named reason instead of patched over. If Artificer says your part is a valid solid with a volume of 199.010 mm³, both halves of that sentence are provable.

## Highlights

- **Sketching that flows** — strokes commit as you draw them, with live dimensions on the canvas, exact closed-profile detection, fillets, chamfers, trims, patterns, and compound primitives (polygons, slots) that stay editable as recipes.
- **Analytic modelling** — extrusions, face Add/Cut, whole-face push/pull, booleans between solids at any relative orientation, and blends that sweep true torus and sphere carriers. Fillet a cylinder's rims at its own radius and you get a mathematically exact sphere.
- **Parametric history** — every feature is an exact command in a deterministic journal. Documents replay to the same solid, byte for byte, verified by content digests. Suppress, restore, and edit features with atomic rebuilds.
- **Assemblies and parts** — a content-addressed part library, rigid component placement, grounding, and revolute joints with motion preview.
- **Materials and mass** — assign a material and get mass and centre of mass from the kernel's exact volume and centroid. Anything the kernel cannot certify is reported as unavailable, never invented.
- **Interchange** — STL and STEP export at authoritative tessellation quality, never from the display mesh.

![Exact sketching](apps/workbench/tests/snapshots/workbench_sketch_xy_rectangle.png)

## Download

Prebuilt binaries are on the [releases page](https://github.com/JackClarkeAE/artificer/releases):

- **Windows** (x86-64) — unzip and run `Artificer.exe`. The binary is unsigned, so SmartScreen will ask once: *More info → Run anyway*.
- **macOS** (Apple Silicon) — unzip and open `Artificer.app`. Gatekeeper requires right-click → Open on first launch.
- **Linux** — build from source below.

## Build from source

Requires stable Rust (1.95+).

```sh
cargo run --release -p artificer-workbench     # launch the workbench
cargo test --workspace --all-targets           # full test suite
./scripts/build-standalone.sh                  # packaged app bundle
```

## How it fits together

| Crate | What it holds |
|---|---|
| `crates/kernel` | The exact B-rep core: topology, validation, booleans, blends, measures |
| `crates/geometry` | Certified predicates — interval-filtered orientation, closure, self-intersection |
| `crates/sketch` | Exact 2D profile authoring, independent of any UI |
| `crates/protocol` | The command vocabulary connecting front ends to the kernel |
| `crates/model` | Parametric documents: features, parameters, replay |
| `apps/workbench` | The desktop application (egui/wgpu), with `ui-core`, `viewport`, and `sketch-ui` beside it |
| `apps/cli` | Runs and replays recorded kernel cases deterministically |
| `addons/scan` | Scan-to-CAD: from triangle meshes to aligned analytic geometry |

The deeper design record — architecture decisions, the kernel roadmap, and the exactness contracts each feature is pinned by — lives in [`docs/architecture`](docs/architecture).

## Status

Artificer is young and moving quickly. The exact kernel, sketching, extrusions, booleans, blends, parametric history, part library, and assemblies all work today and are held to the test gates described above; plenty of everyday CAD surface area is still to come. Issues and pull requests are welcome.

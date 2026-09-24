# ADR 0057: CAM from the history — automatic toolpaths, tools and an exact stock simulation

Status: implemented for turned parts and 2.5D milled parts — `crates/cam`
(recognition, turning, milling, the tool library, the post and the
interpreter, the exact section stock and the heightmap stock), five kernel
queries in `crates/kernel/src/cam_queries.rs`, and the CAM tab
(`apps/workbench/src/cam.rs`) with its card, timeline and export. Deviations
from the text below: (1) the kernel exposes a fifth query,
`NativeKernel::profile_boolean`, because §2.3's stock update is the planar
Boolean and it was `pub(crate)`; (2) the lathe finish pass is programmed on
the theoretical tip point with `G42` asked of the control for the nose
radius, and simulated as a sharp tip, because a nose radius applied in CAM
leaves fillets at every sharp inside corner and gate 2 asks for the exact
section; (3) a flat on a cylinder recognises as `Milled` along the cylinder's
axis rather than `MillTurn`, since one mill setup reaches it, while a radial
hole is `MillTurn` and is refused by name at planning; (4) a confirmed plan
is kept in the tab's state for the session rather than in the document file,
and mill-turn, arcs in the lathe stock (chorded to 5 µm) and the drill's cone
in the heightmap are the approximations the card and the report name.
Date: 2026-09-24
Extends: [0007](0007-universal-model-operation-confirmation.md) (one pending-operation
gate), [0028](0028-workbench-command-registry-and-contextual-properties.md) (commands are a table),
[0053](0053-a-part-can-be-saved-into-the-library.md) (the user data folder),
[0055](0055-revolve-and-sweep-are-features.md) (the `(r, z)` section),
[0056](0056-the-general-geometry-programme.md).

A CAM tab that, from one button, decides whether a part is turned or milled,
chooses stock, tools and operations, generates the toolpaths and tool
changes, posts G-code, and shows the tool taking material off the billet
until the stock is the part. Minimal, visual, and genuinely usable for
turned parts and 2.5D milled parts; refused by name for everything else.

## 1. Why this fits the kernel

Two things every CAM system has to build, Artificer already has:

- **The lathe stock model is the `(r, z)` section.** `extract_rz_section`
  (`crates/kernel/src/section_revolve.rs:127`) returns the exact half-section
  of any solid of revolution, and `profile_boolean` subtracts one planar
  region from another exactly. A turning pass is "subtract the insert's
  swept region from the stock section". The remaining stock is a section;
  `build_turned_region` revolves it into an exact solid for display. The
  lathe simulation is therefore exact, and its final state can be checked
  against the part to 1e-9. No commercial CAM makes that claim.
- **Milled-part recognition is the history.** A part whose features are
  extrusions, cuts and drills along one axis is 2.5D millable, and the
  history already carries each pocket's floor, each hole's diameter and
  depth, each boss. Where there is no history (an imported body), prism
  extraction (`extract_prism`, `prism_edge_finish.rs:237`) and
  `describe_faces` recover the same facts from the geometry.

Everything else — offsetting a loop by the tool radius (`loop_offset.rs`),
tessellation for display, the timeline and preview machinery of the
workbench — exists.

## 2. Decision

### 2.1 Shape

- A new crate **`crates/cam`** depending on `artificer-kernel`,
  `artificer-model` and `artificer-protocol`. It takes a `Snapshot` (and the
  document's feature history when there is one) and returns data: a
  `Setup`, a `Plan` of `Operation`s, `Toolpath`s, a `StockState` per time,
  and G-code text. It never draws. Everything it does is testable
  headlessly.
- The **kernel exposes** what CAM reads, additively in `lib.rs`:
  `NativeKernel::turned_section(&Snapshot) -> Option<TurnedSection>` (the
  `(r, z)` segments as protocol `PlanarCurve2`s plus axis and centre),
  `NativeKernel::prism_profile(&Snapshot, axis) -> Option<PrismProfile>`
  (frame, height, outer loop and holes as `PlanarLoop2`s),
  `NativeKernel::point_in_solid(&Snapshot, Point3) -> Option<bool>`, and
  `NativeKernel::offset_loop(&PlanarLoop2, distance) -> Result<Vec<PlanarLoop2>>`
  (the mitred inward offset). Ten to thirty lines each, wrapping what is
  `pub(crate)` today.
- The **workbench** gains `RibbonTab::Cam` in `commands.rs` (the seventh
  entry of `RibbonTab::ALL`), a `cam.rs` module owning the tab's state and
  cards, and viewport overlays for the stock, the tool and the path. It
  goes through the pending-operation gate like every other tab: "Auto-CAM"
  stages a plan; Confirm keeps it as document data; nothing here executes
  the kernel except through the public queries above (the boundary script's
  rules hold; `cam.rs` is listed with `material.rs` and `ribbon.rs` as a
  file that never calls `NativeKernel::execute`).
- The **tool library** is a file in the user data folder (ADR 0053's
  `user_data::data_directory()`), `tools.json`, seeded on first run with
  the built-in set below and editable by hand.

### 2.2 Recognition (the first half of the button)

`cam::recognise(snapshot, history) -> Setup`:

| Result | When | Stock | Work origin |
|---|---|---|---|
| `Turned` | `turned_section` succeeds | a bar: max radius + allowance, length + facing allowance both ends | the axis, at the right-hand face |
| `Milled { axis }` | every face is a plane or a cylinder parallel to one axis; the axis chosen exposes the most faces from above | bounding box + allowance per side | a stock corner (top-left-front) or the stock centre, user's choice |
| `MillTurn` | a turned body with radial holes or flats | the bar | turn first, then mill |
| `Unsupported { faces }` | anything else (lofts, sweeps, off-axis curved faces) | — | refused by name, faces listed |

The setup card says what it decided and why, and highlights the faces it
used. Allowances default to 2 mm radial / 1 mm facing (lathe) and 2 mm per
side / 1 mm top (mill), editable.

### 2.3 Turning

Operations, in order: **face** (one pass across the right end), **rough**
(Z-parallel passes at a depth of cut, from the stock radius down to the
profile plus the finish allowance, each pass retracting along the profile),
**drill** (a centre drill and a drill for an axial bore, if the section has
one on the axis), **bore** (roughing and finishing inside with a boring
bar), **finish** (one pass along the exact section with nose-radius
compensation: the section offset by the nose radius, `offset_loop`
outward), **groove** (plunges for grooves narrower than the insert), **part
off**.

Every pass is a 2D tool region swept along a line in `(r, z)`. The stock
after each pass is `profile_boolean(stock, sweep, Difference)`. The
`StockState` is the section; the viewport shows `build_turned_region` of it.

Gate: the final stock section equals the part's section to 1e-9 in area and
in every vertex; roughing never cuts inside the finish allowance; every
rapid stays outside the current stock section.

### 2.4 Milling (2.5D)

Operations: **face** (a raster over the top with stepover), **pocket**
(contour-parallel: repeated inward offsets of the pocket loops by the
stepover, the last at the tool radius, depth passes by stepdown, helical or
ramp entry), **profile** (the outer loop offset outward by the radius,
depth passes, optional tabs), **slot**, **drill** (from drill features or
cylindrical through/blind holes: a peck cycle by depth; a hole larger than
the largest drill becomes a helical bore with an end mill), **chamfer** is
out of scope in this slice.

Stock is a **heightmap** over the stock top (cell size from the finest
feature, capped at a 512×512 grid): each tool position lowers every cell
inside the tool footprint to the tool tip's z. This is exact for flat
end mills on 2.5D geometry and is the model every hobby simulator uses.
The heightmap is drawn as a quad mesh.

Gate: stock volume minus the heightmap's removed volume equals the part's
volume within one cell's volume times the boundary cell count; no tool
position ever enters the part (the swept bottom disc sampled against
`point_in_solid`); no rapid passes through remaining stock.

### 2.5 Tools, selection, ordering, feeds

Built-in library: flat end mills 2, 3, 4, 6, 8, 10, 12 mm (2 and 4 flutes),
ball end mills 3, 6, 10 mm, drills 1–12 mm in 0.5 mm steps plus a centre
drill, a CNMG roughing insert, a DNMG finishing insert, a 3 mm parting
blade, a boring bar. Each tool: number, kind, diameter, corner or nose
radius, flute count, flute length, max depth of cut, and a per-material
feed table.

Selection: a pocket takes the largest end mill whose radius is at most the
smallest inside corner radius and whose diameter fits the narrowest slot;
a hole takes the drill of its diameter, else helical boring; turning takes
the roughing then the finishing insert, and the boring bar inside.

Order: face → rough → drill → finish → part off (lathe); face → pockets and
profiles by tool, largest first → drills → finish passes (mill). Operations
are grouped by tool; each change emits a retract to the safe plane, `M6
Tn`, and `M3 Sn`. Feeds and speeds from a material table (aluminium, mild
steel, brass, ABS): rpm from surface speed capped by the spindle, feed from
chip load × flutes × rpm, `G96` constant surface speed on the lathe.

### 2.6 Post and simulation contract

A post-processor writes G-code in the LinuxCNC/Fanuc dialect: `G0/G1/G2/G3`,
`G81/G83` cycles, `G96/G97`, `M3/M5/M6/M8/M9`, `G54`, a header naming the
setup and tools, one file per setup. **The simulation consumes the G-code,
not the internal path**: `crates/cam` carries a small interpreter (modal
groups, arcs in the three planes, cycles, feed/rapid distinction) whose
output drives the stock model and the tool position. What the user watches
is what the machine would do.

### 2.7 The tab

Ribbon: Auto-CAM · Setup · Operations · Simulate · Export. The setup card;
the operation list (reorderable, each with its tool, feeds and an estimated
time); a timeline scrubber with play, pause and speed; the viewport showing
the stock ghost (semi-transparent), the remaining stock (revolved section
or heightmap mesh), the tool as a semi-transparent cylinder or insert at the
current position, rapids and cuts in two colours, collisions in red and
listed; a total time readout; Export G-code to a file. Headless kernel-lab
tests drive the tab as the extrusion tests do.

### 2.8 Later (not this slice)

3-axis surfacing for lofts and sweeps by the drop-cutter and waterline
algorithms over the tessellation; rest machining; mill-turn with live
tooling; ball-end finishing; 4- and 5-axis; probing; fixtures.

## 3. Borrowed and owned

Written in Rust in-house (the build is dependency-free and offline). The
algorithms are borrowed from the literature and named in the code:
contour-parallel pocketing by repeated offsetting, the heightmap stock
model, drop-cutter and waterline (OpenCAMLib) for the later slice,
feeds-and-speeds from machining handbooks, the G-code dialect from
LinuxCNC's documentation. Owned and worth advertising: the exact lathe
simulation from the kernel's section machinery, and recognition from the
parametric history rather than from a dumb B-rep.

## 4. Gates for the slice

1. Recognition table: every body in the kernel and workbench fixture set
   classified as the test's table says.
2. Lathe: a stepped shaft with a groove and a chamfer roughs, finishes and
   parts off; the final section equals the part's to 1e-9.
3. Mill: a plate with two pockets (one with an inside corner radius that
   forces a smaller tool), a through hole and a profiled outline; heightmap
   volume matches; two tool changes in the right order.
4. G-code from both re-simulates identically through the interpreter; an
   arc, a drilling cycle and a `G96` block each have an interpreter test.
5. UI: the tab stages, previews, scrubs, and exports headlessly; the
   semi-transparent tool and stock render in the parity snapshots.

## 5. Consequences

CAM lives below the workbench and is scriptable for free (`cam_plan()`,
`cam_gcode()` builtins are a follow-up). The kernel gains four public
queries and no new construction. The document gains a stored `CamPlan`
only if Confirm is used; nothing else changes on disk.

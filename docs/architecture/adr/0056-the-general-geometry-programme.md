# ADR 0056: The general-geometry programme — Booleans, blends, surfacing, scale, and STEP import

Status: proposed
Date: 2026-09-24
Extends: [0002](0002-numerical-correctness-model.md),
[0025](0025-analytic-surface-intersections.md),
[0026](0026-second-expansion-programme.md) (Track K),
[0045](0045-a-boolean-that-resolves-what-it-touches.md),
[0047](0047-the-curve-two-cylinders-share.md),
[0050](0050-b-spline-curves-and-surfaces-as-carriers.md),
the geometry-kernel README's milestones M7, M9, M10 and M11, and the
[2026-09-23 review](../reviews/2026-09-23-simplicity-and-modularity-review.md).

This is the plan for the five capabilities that separate Artificer's kernel
from the ones under Fusion, SOLIDWORKS, Onshape and FreeCAD: general
Booleans, general fillets, surfacing, robustness at scale, and STEP import.
Each is a track with numbered stages, and every stage has a closed-form gate
before it counts as done. Line references are into `097c103`.

## 1. Where the kernel stands

**Surfaces and curves.** `Surface::{Plane, Cylinder, Cone, Sphere, Torus,
Ruled, Bspline}`; `Curve3::{Line, Circle, Ellipse, Trace, Bspline}`;
`Curve2::{Line, Circle, Harmonic, Ellipse, Trace, Bspline}`. B-splines are
non-rational, clamped, degree 1–5, with evaluation, derivatives, knot
insertion, splitting, elevation, refinement, Bézier extraction, curvature
bounds, arc-length and surface inversion (`bspline.rs`); ruled surfaces have
inversion and exact measures (`ruled.rs`).

**Intersections** (`surface_intersection.rs:99–131`, ADR 0025's published
matrix): plane against every analytic class; cylinder × cylinder (coaxial,
parallel, equal radii on crossing axes, and the general quartic as a `Trace`
over a host cylinder, ADR 0047); cylinder × sphere with the centre on the
axis; cylinder × cone coaxial; sphere × sphere; cone × cone coaxial; torus ×
torus coaxial with equal major radii. Every other pair is
`IntersectionError::Unsupported`, including anything with a ruled or
B-spline surface.

**Booleans.** `execute_boolean` and `tool_boolean` run prism → coaxial →
analytic → (tools only) faceted. The analytic engine admits only bodies
whose faces are all planes or cylinders (`operands_in_engine_vocabulary`,
`analytic_boolean.rs:2329`); the coaxial rung is exact for two solids of
revolution about one axis. Everything else falls to a BSP over
tessellations capped at 4,096 polygons (`faceted_boolean.rs:570`) with
tolerances derived from a fixed `epsilon`, labelled `Tier::Approximate`.

**Blends.** Six exact rungs (`regularized_edge_finish`, `lib.rs:4792`):
prism edge finish, rim blend, rim-loop blend (reflex corners through the
Steinmetz seam), hole-rim blend, vertex blend (convex edges between planes,
sphere or triangle corners), standing-apart (a removal tool cut by the
general Boolean), then the faceted BSP rung. No concave edge on a
non-prism body, no edge between two curved faces, no variable radius, no
face–face blend, no setback corner.

**Surfacing.** Ruled and B-spline walls arise from lofts, sweeps and spline
extrusions, always inside a solid. There is no sheet body, trim, extend,
stitch, offset of an arbitrary surface, or surface editing.

**Scale.** `MAX_PLANAR_PROFILE_CURVES = 1_024`, `MAX_PLANAR_PROFILE_LOOPS =
128`; the largest test bodies have tens of faces; the analytic engine
visits every face pair; nine formulas for "these two points agree" (review
§2.5); validation runs up to three times per operation.

**Interchange.** Export only: `step_export.rs` writes AP203-style
`MANIFOLD_SOLID_BREP` with planes, cylinders, cones, spheres, tori,
B-spline surfaces and curves, and a faceted fallback. The scan add-on has a
Part 21 tokenizer and entity graph (`addons/scan/…/step.rs:960
read_step`) that reads planes, cylinders, cones and B-spline faces into a
triangle mesh; nothing reads STEP into the kernel's B-rep.

## 2. Rules that bind every stage below

1. **Three answers, never a fourth.** A result is *exact* (closed-form
   carriers and curves), *certified within tolerance* (a stated bound the
   operation met and reports, as the skinned sweep does today with
   `SWEEP_APPROXIMATION_TOLERANCE`), or *approximate* (the faceted tier,
   badged). This programme adds the middle answer as a first-class tier:
   `Tier::Certified { deviation }` in the report and the timeline chip.
   A stage may not move a case from exact to certified silently.
2. **Refuse by name.** Every new domain boundary gets a code in
   `docs/report-schema.json` and a sentence that says what to change.
3. **A closed-form gate per stage.** Volumes by Pappus, frustum
   arithmetic, or an independent high-precision quadrature (the test
   strategy allows arbitrary-precision reference code as an oracle); the
   conservation invariant `V(A)+V(B) = V(A∪B)+V(A∩B)` on every Boolean that
   certifies; validator clean; digest deterministic on Linux and Windows.
4. **The published matrix never lags the dispatch table** (ADR 0025).
   Every widened pair lands in ADR 0025's table in the same change.
5. **Foundations before frontier.** The stages in section 3 are
   prerequisites, not options: every track below reads the one agreement
   model and one builder, and is fuzzed by the one harness.

## 3. Shared foundations (G)

### G1. One agreement model — the tolerance ledger as code

Implement ADR 0002's four ideas as one type read everywhere:

- `Agreement::from(PrecisionPolicy)` with `point(scale) -> f64`,
  `angle() -> f64`, `parameter(curve) -> f64`, and named weld multipliers
  (`SEAM_WELD`, `SECTION_WELD`, `SEW_WELD`) replacing the `·8/·32/·128`
  literals; `Topology::coordinate_scale()` as the one "scale".
- Replace the nine formulas in the construction modules (review §3.2.7),
  one rung at a time, running the case corpus after each.
- The faceted tier derives its `epsilon` from the same source, scaled by
  the body, so its tolerances stop being fixed numbers.
- **Gate:** an invariance test: every kernel case, scaled by 10⁻³ and 10³
  and translated by 10⁵, yields the same topology counts and the same
  tier, and measures scale as they should to 1e-9 relative.
- Effort: 2–3 weeks. This is the first thing to do; B, F and I all read it.

### G2. One builder, one prism reader, one planar toolkit

The review's Phase B items that the frontier tracks would otherwise copy
again: `TopologyBuilder` and `Topology::append` (§3.2.1), `extract_prism`
(§3.2.5), the planar helper module (§3.2.2), `Frame → Plane` (§3.2.10),
`Topology::edge_incidence()` (§3.2.12), and `validator::certify` returned
up the ladder (§3.1.10). Effort: 2–3 weeks. Done before Track B widens the
engine so that the new rungs are written once.

### G3. The conservation fuzzer and the minimizer (K4, unbuilt)

- `proptest` generators (the test strategy already names it): profiles
  from the sketch arrangement generator in `compute-bench`, extrude/revolve
  with random frames and transforms, random certified operations.
- Invariants on every Boolean that certifies: conservation of volume at
  1e-9 relative; `A ∪ A = A`, `A ∩ A = A`, `A − A = ∅`; commutativity of
  union and intersection; Euler characteristic of every shell; validator
  clean; digest identical across two runs and across platforms.
- Refusals are fine; a wrong volume fails. Failures shrink through a
  geometry-aware `KernelCase` minimizer (delete curves, snap coordinates,
  drop operations) into permanent regression cases.
- Runs nightly on the corpus; per-push on a fixed seed set.
- Effort: 3–4 weeks. Every later stage adds its operation to the
  generator the day it lands.

### G4. The curve and surface toolkit

What B4, F1 and S2 all need and none has:

- Conservative bounds: axis-aligned and oriented boxes for Bézier segments
  and patches (from control nets), for analytic patches over a parameter
  rectangle, and for `Trace` curves over an azimuth span.
- Patch subdivision (`SplineSurface::split` in u and v; Bézier patch
  extraction, the surface analogue of `bezier_segments`).
- Certified root isolation: interval arithmetic over Bézier coefficients
  (the coefficients bound the polynomial), interval Newton on
  `f(u,v)=0` systems, with the result an enclosure, not a point.
- Offsets: `Surface::offset(d)` exact for plane, cylinder, cone, sphere and
  torus; for ruled and B-spline surfaces an approximation fitted to a
  stated tolerance (`SURFACE_OFFSET_APPROXIMATION`, the deviation
  measured, never assumed).
- Ray casting per surface class for `point_in_solid`
  (`analytic_boolean.rs:2166`): quadratic in the ray parameter for
  quadrics, quartic for the torus, subdivision for splines.
- `Curve3::Bspline` fitting of a sampled curve to tolerance with the
  deviation returned (the sweep's skinning already has the pieces).
- Effort: 4–6 weeks, delivered in the order the tracks consume it.

## 4. Track B — general Booleans

The target is the M7 milestone: any two bodies in the vocabulary combine,
exactly where the curves are exact, certified within tolerance otherwise,
and the faceted tier becomes a last resort that is badged and rare.

### B1. Widen the vocabulary gate over the matrix that exists (K2)

`operands_in_engine_vocabulary` admits cones, spheres and tori; every pair
the matrix already answers works through the analytic engine, and a pair
it refuses is refused *only if the two faces can meet* (`faces_apart`
already gates this). Pcurves on the revolved parameterisations are lines
and circles, which `curve_chords` and the chainers already carry.

- Also: coincident overlays for same-carrier cone/sphere/torus faces (the
  `coincident_overlays` table gains the three classes), and tangential
  contact per ADR 0045 for plane–sphere and cylinder–sphere.
- **Gates:** a coaxial counterbore into a drafted boss, volume by frustum
  arithmetic; a torus-blended block cut by a perpendicular bore that
  clears the blend; a sphere seated on a plate joined by union; the
  conservation fuzzer over bodies with all seven classes.
- Effort: 3–4 weeks.

### B2. Quadric traces over a ruled host

ADR 0047's idea generalised: a straight generator of a cylinder or cone
meets any quadric in at most two points, so the intersection of a cylinder
or cone with a cylinder, cone or sphere is the root of a quadratic in the
generator parameter over the host's azimuth. `Trace { host: Cylinder | Cone,
other: Quadric, branch }` replaces `CylinderTrace`; landmarks (discriminant
zeros, branch joins, tangencies) are found by the same closed-form
analysis; the pcurve on the host is the trace itself, on the other surface
it is obtained by exact inversion.

- Covers: cylinder × cone at any attitude, cylinder × sphere off-axis,
  cone × sphere, cone × cone non-coaxial, cylinder × cylinder (already),
  and an oblique plane × cone/sphere is already a conic.
- **Gates:** a bore through a cone at an angle (volume by quadrature
  oracle), a sphere cut by an off-centre bore (closed form: cylinder-sphere
  intersection volume by Pappus-free quadrature), the crossing-bore
  fixtures rerun with one cone.
- Effort: 5–6 weeks.

### B3. Quartic traces: the torus against a ruled host or a plane

A generator meets a torus in at most four points: a quartic in the
generator parameter. Torus × cylinder and torus × cone use the same trace
machinery with a quartic root (closed-form quartic with certified sign
handling, or interval Newton from G4 when the discriminant is near zero);
an oblique plane × torus (a spiric section) is traced along a family of
parallel lines in the plane.

- Deferred to B4: torus × torus non-coaxial, torus × sphere off-axis
  (neither carrier has straight generators).
- **Gates:** a filleted block (torus bands) drilled off-axis; a torus
  cut by a slanted plane; the conservation fuzzer over blended bodies.
- Effort: 5–6 weeks.

### B4. General surface–surface intersection (SSI)

The long pole. For ruled, B-spline and the residual analytic pairs:

- Candidate starts from G4's box hierarchies (patch pairs whose boxes
  overlap, refined by subdivision until each box pair is small or
  provably apart); interval Newton certifies each start.
- Marching along the curve in both parameter spaces with step control
  from curvature bounds, snapping to patch and seam boundaries, with
  branch tracking and singular-point detection (parallel normals) that
  splits the curve rather than guessing across.
- The result is `Curve3::Bspline` fitted to the marched points within a
  stated tolerance, with a `Curve2::Bspline` pcurve on each face from the
  marched parameters; the deviation measured against the true surfaces is
  reported and the result is `Tier::Certified`.
- Overlaps (coincident spline patches) resolved by classification as ADR
  0045 does for planes; tangential crossings refused by name in the first
  slice (`BOOLEAN_TANGENT_CONTACT_UNSUPPORTED`) and admitted in a second.
- **Gates:** no false "no intersection" on a corpus of patch pairs with
  known crossings; two lofts unioned and intersected satisfy conservation;
  a sweep cut through a loft matches a high-resolution quadrature to the
  stated tolerance; the faceted route's answer for every existing
  `sweep/faceted` and `loft/faceted` fixture now comes back certified with
  a smaller error than the faceted one had.
- Effort: 10–14 weeks. This is where commercial kernels spent years; the
  plan is to ship it narrow (transverse crossings, non-rational,
  non-periodic) and widen behind the fuzzer.

### B5. Classification, sewing and the ladder

- `point_in_solid` uses G4's per-class ray casting; `face_sample_inside`
  samples inside curved-boundary regions, not only on chords.
- `sew_shells` welds B-spline edges by endpoints and a midpoint from the
  same 3D curve both faces carry, so certified curves sew as exact ones do.
- The ladder becomes prism → coaxial → analytic (now the general engine)
  → faceted, with the faceted tier reached only when a stage above refuses
  by name; `HistoryMode::Approximate` and the timeline badge from K3 land
  here.
- **Gate:** every Boolean fixture that reaches the faceted tier today is
  listed, and each is either moved up a tier or has its refusal code
  recorded as intended.
- Effort: 3–4 weeks, interleaved with B2–B4.

## 5. Track F — general fillets

The target is the M9 milestone: constant-radius rolling-ball fillets and
chamfers on any edge chain between faces in the vocabulary, convex or
concave, with corners resolved; then variable radius and face–face blends.
The exact rungs that exist stay as fast paths, since they produce the same
result in closed form.

**Status (first landing, 0.99.82).** F2's gates landed ahead of F1, as two
rungs rather than the one construction: `edge-finish/concave-rim-blend`
builds the torus or cone band in place along a concave rim (a boss on its
plate, a counterbore's floor, a blind pocket's floor rim), exact by Pappus
across twenty-four radii; `edge-finish/concave-fill` fills a concave
straight edge between flat faces on any planar body by unioning its own
corner region through the Boolean ladder (flat sides inset into the
material, ends flush with square caps, the added volume certified against
the closed form), and `EDGE_FINISH_REFLEX_UNSUPPORTED` is retired for
planar bodies. F3's first slice landed with it: a selection mixing concave
and convex edges fills the concave ones first, cuts a convex edge that meets
a fill standing apart with its tool bounded at the band's tangency plane
(the planar-cap corner, exact), and finishes the rest by the ladder or
standing apart; the cube with all twelve edges was found to build already
(vertex blend). F5 has a first slice, approximate and labelled: a radius
running linearly along a convex straight edge, reached through
`NativeKernel::finish_edge_variable_radius` (no protocol command yet),
whose band is lofted as flat facets between the two end sections and cut
exactly, under `variable-radius/faceted` with the chords' sagitta in the
warning and the volume certified against the cone's closed form. Not
landed: F1's `Surface::Pipe` and single construction, the rolling-ball
patch at a concave–convex corner (planar cap only), a concave edge ending
on a leaning face (`CONCAVE_EDGE_END_UNSUPPORTED`), F4, F6, F7, and the
rest of F5. Regressions: `crates/kernel/tests/fillet_frontier.rs`.

### F1. The rolling-ball blend as one construction

A fillet of radius `r` along an edge between faces A and B is the envelope
of a ball tangent to both. Its **spine** is the intersection of the offset
surfaces `A±r` and `B±r` (signs by convexity); its **contact curves** on A
and B are the spine's foot points (`spine ∓ r·n`); its **surface** is the
tube of radius `r` about the spine; A and B are trimmed at the contact
curves and the tube fills the gap.

Every piece maps onto something the kernel has or this programme builds:
offsets of analytic carriers are analytic (G4); spines are intersections
from Track B (a plane–plane spine is a line and the tube a cylinder; a
rim spine is a circle and the tube a torus; a cylinder–cylinder spine is a
trace and the tube a pipe surface); trimming and re-sewing are the
Boolean's imprint and sew stages.

- New carrier `Surface::Pipe { spine: Curve3, radius }` (a canal surface
  about an exact spine): evaluation, normal, pcurves, validator frame and
  locus arms, tessellation, measures (Pappus-style over the spine's
  arc-length and curvature, exact for analytic spines), STEP as a
  `SURFACE_OF_REVOLUTION`/B-spline equivalent. For a B-spline spine the
  tube is itself a B-spline approximation to tolerance (certified tier).
- The construction as `plan_blend` (spine, contacts, corner plan) and
  `build_blend` (through `TopologyBuilder`), replacing `standing_apart`'s
  cut-a-tool approach for convex edges and adding material for concave
  ones.
- **Gates:** every existing edge-finish fixture reproduced by the general
  construction to 1e-9 (the six exact rungs become its oracle); a G1 check
  in the validator (`SMOOTH_EDGE` normals agree along contact curves to
  the angular agreement).
- Effort: 6–8 weeks.

### F2. Concave edges

The same construction with offsets outward and material added: the first
gate is the one the product's own "Not in 0.3" list names, a fillet where
a boss meets its plate (plane × cylinder, concave), volume by Pappus on
the torus segment; then a concave edge between two planes on an L-block
(the reflex case refused since 0.99.81), a concave rim in a stepped bore,
and chamfers on all three.

- `EDGE_FINISH_REFLEX_UNSUPPORTED` retires.
- Effort: 2–3 weeks after F1.

### F3. Chains and corners

- Tangent chains (`apply_tangent_edge_chain` exists) run one spine per
  chain with the tube continuous across smooth edges.
- Corners where three blends meet: equal radii on a convex planar corner
  is the sphere patch the vertex blend already writes; mixed convex/
  concave and unequal radii use a **setback**: each tube stops a computed
  distance short and a B-spline Coons patch, G1 to the three tubes, fills
  the corner (certified tier for the patch, exact tubes).
- Corners with four or more edges: refuse by name in the first slice
  (`BLEND_CORNER_VALENCE_UNSUPPORTED`), admit with setbacks in a second.
- **Gates:** the cube with all twelve edges filleted (a case the review
  lists as still refused, task #27), the L-block with its concave and
  convex edges filleted together, a radius sweep over forty values as the
  0.99.7 test did.
- Effort: 5–6 weeks.

### F4. Edges between curved faces

Spine = a trace (B2) or an SSI curve (B4) between the offsets; tube = a
pipe over it. First gate: the fillet along a crossing-bore seam (spine =
offset-cylinder trace, which ADR 0047 already computes); then a fillet
where a boss meets a cylindrical wall, and where a torus blend meets a
plane (a fillet on a fillet).

- Effort: 3–4 weeks after B2, more after B4 for spline faces.

### F5. Variable radius

The ball radius as a law along the spine (linear or a spline through
control values). The spine is no longer an intersection of fixed offsets:
it is marched by solving `dist(P, A) = dist(P, B) = r(s)` with B4's
machinery, and the surface is a variable-radius canal fitted to tolerance
(certified). Effort: 4 weeks after B4.

### F6. Face–face blends and full rounds

A blend between two faces that share no edge (spine = offsets'
intersection, independent of an edge; the material between is removed or
added), and a full round replacing a face by a tube tangent to its two
neighbours. Both are F1's construction with a different trim plan.
Effort: 4 weeks after F3.

### F7. Retire the faceted edge-finish rung to a badged last resort

Every fixture that reaches `edge-finish/faceted` today is re-run; each
either lands in F1–F4 or gets a named refusal. Effort: 1 week, at the end.

## 6. Track S — surfacing

The target is a sheet body as a first-class object, with trim, extend,
stitch and thicken (M10's shell for arbitrary bodies falls out of the
last), and free-form editing behind it.

### S1. Sheet bodies

- `Topology` already carries shells; a sheet is a solid-less open shell.
  Add `ValidationProfile::Sheet` (closedness not required; every edge has
  one or two uses; boundary edges listed), measures (area, no volume),
  display with boundary edges drawn, STEP export as
  `SHELL_BASED_SURFACE_MODEL` (the entity exists in the exporter).
- Constructions: surface extrude of an open profile, surface revolve,
  surface loft and sweep from open sections (the solid versions minus the
  caps), a planar face from a closed sketch loop, a patch filling a closed
  wire (planar exact, else a Coons B-spline patch, certified).
- Model and workbench: `FeatureKind::Surface*`, a Surfaces group in the
  Browser, sheet selection.
- **Gates:** areas by closed form; a sheet from an open profile and its
  solid twin from the closed one agree on every shared face.
- Effort: 3–4 weeks.

### S2. Trim, split, untrim, extend

- Trim/split a sheet by a plane, another sheet, a projected sketch curve
  or an imprinted intersection: Track B's imprint on a single face,
  keeping the chosen side. Untrim restores the carrier's natural bounds.
- Extend: analytic carriers extend by widening their parameter bounds;
  B-spline surfaces extend by Bézier extrapolation of the boundary strip
  with G1 (exact for the extrapolated polynomial, certified against the
  requested length).
- **Gates:** trim a cylinder sheet by a plane, area by closed form; split
  a loft by a plane and check the two areas sum; extend and re-trim
  restores the digest.
- Effort: 4–6 weeks; spline-by-spline trims wait for B4.

### S3. Stitch and sew to a solid

`sew_shells` generalised: match edges within the agreement, report gaps
by edge (`SEW_GAP_EXCEEDS_TOLERANCE` with the measured gap), close a
sheet set into a solid when every edge pairs. Free-edge display for what
did not stitch. Effort: 2–3 weeks.

### S4. Offset and thicken

Offset surfaces from G4 (exact for analytic, certified for splines), side
walls as ruled surfaces between the two boundaries, sewn into a solid.
`shell` then handles any body: offset every face, resolve the offset
intersections (Track B), keep the openings. Self-intersection of an
offset (radius smaller than a concave curvature) is detected from the
curvature bounds and refused by name.

- **Gates:** thicken a cylinder sheet, volume by closed form; shell a
  loft and check the wall by probing; the `shell` fixtures that are
  refused today (`SHELL_DOMAIN_UNSUPPORTED`, blended bodies) now build.
- Effort: 4–6 weeks.

### S5. Free-form editing

Control-vertex handles on B-spline faces in the viewport (the 2D CV tools
exist), the surface feature editable in history; a "fit surface" feature
that takes the scan add-on's fitter into the workbench; boundary-matching
constraints (G1/G2) between adjacent patches solved as a small linear
system on the boundary control rows. Effort: 4–6 weeks; needs its own ADR
for the UI.

### S6. Surface blends

F1's construction applied between two sheets (boundary blend) and along a
sheet's edge. Effort: 2 weeks after F1 and S1.

## 7. Track R — robustness at scale

The target: bodies of 10⁴ faces built, combined, blended, validated and
displayed in interactive time, deterministic on every platform, with the
caps replaced by budgets.

### R1. The agreement model (G1) and 3-D certified predicates

G1 is the first stage here as well. Beyond it: `orient3d` and `insphere`
with floating-point filters and adaptive exact fallback in
`artificer_geometry` (the crate has `orient2d`), used wherever a 3-D
sidedness decision is made (ray casting, sewing, classification), so no
topology decision rests on a library sine (the 0.99.7 lesson). Effort:
2 weeks after G1.

### R2. Complexity

- A face extent index (AABB tree or a grid over `FaceExtent`) so the
  analytic engine visits candidate pairs, not every pair; the section
  pieces a face needs are those whose carriers cross its extent.
- Profile Boolean crossings by a sweep line or grid instead of every
  segment pair; sewing by hash-grid welding (the faceted tier's
  `welded_key` generalised); validator and history walks over
  `edge_incidence()` and `entities()` (G2).
- One validation per operation (G2's `certify`).
- **Gates:** criterion benches (`benches/booleans.rs`, `blends.rs`,
  `validate.rs`, `tessellate.rs` exist) over generated bodies with 10²,
  10³ and 10⁴ faces (a plate with a grid of holes; a patterned boss
  array); targets: a Boolean of two 10³-face bodies under 1 s in debug
  and 100 ms in release, validation of a 10⁴-face body under 200 ms
  release; a CI perf gate that fails on a 20 % regression (V6).
- Effort: 5–6 weeks.

### R3. Caps become budgets

`MAX_PLANAR_PROFILE_CURVES` 1,024 → 16,384 once R2's sweep line lands;
the faceted tier's 4,096-polygon cap becomes a time and memory budget
checked through `CancellationToken`, refusing with
`FACETED_BUDGET_EXCEEDED` rather than silently declining; every long
operation polls cancellation (some already do). Effort: 2 weeks.

### R4. The fuzzer (G3) and the corpus

G3, plus: a body corpus (every workbench and script fixture, every
imported STEP part from Track I) replayed nightly with measures compared
to the last accepted values; a `kernel fuzz` target as the test strategy
draws. Effort: G3 plus 1 week.

### R5. Determinism

One `Point3`/`Vector3` across kernel, protocol and geometry with one
`mul_add` policy (review §3.6.2), deterministic single-threaded reductions
in measures and digests, and the Windows CI job comparing digests of the
whole corpus against Linux. Effort: 3–4 weeks.

### R6. Incremental replay and budgets in the document

V3 from ADR 0026: replay only the features downstream of an edit, with
per-feature time budgets surfaced in the timeline. Effort: 4 weeks;
independent of the kernel tracks.

## 8. Track I — STEP import

The target is M11: a STEP file from any mainstream CAD system opens as an
exact kernel body where its geometry is in the vocabulary, as a certified
body where a curve or surface had to be approximated, and as a labelled
reference mesh where it could not be read, with every refusal named per
face.

### I1. A shared STEP crate

Move the scan add-on's Part 21 tokenizer and entity graph into
`crates/step` (dependency-free below the kernel, as the review's §3.7.12
suggests), with complex-entity instances, typed argument access, units,
and a writer for the entity primitives the kernel's exporter and the scan
add-on both spell today. Both consumers switch to it.

- **Gates:** every file the exporter writes parses back to the same
  entity graph; a conformance set of Part 21 files (AP203, AP214, AP242;
  ASCII quirks: continuation lines, comments, `*` and `$` arguments,
  user-defined entities) parses or fails by name.
- Effort: 1–2 weeks.

### I2. B-rep import into the kernel's vocabulary

`MANIFOLD_SOLID_BREP`, `BREP_WITH_VOIDS`, `SHELL_BASED_SURFACE_MODEL` and
`FACETED_BREP` to `Topology`, through a conforming stage that is the bulk
of the work:

- Surfaces: `PLANE`, `CYLINDRICAL_`, `CONICAL_`, `SPHERICAL_`,
  `TOROIDAL_SURFACE` to the kernel's classes; `SURFACE_OF_REVOLUTION` and
  `SURFACE_OF_LINEAR_EXTRUSION` recognised as analytic where their
  generatrix is a line or circle, else built as ruled or B-spline;
  `B_SPLINE_SURFACE_WITH_KNOTS` (and the `BOUNDED_SURFACE` complex forms)
  non-rational directly; rational (`RATIONAL_B_SPLINE_SURFACE` weights)
  approximated to tolerance with `STEP_RATIONAL_APPROXIMATED` and the
  deviation, until I2c.
- Curves: `LINE`, `CIRCLE`, `ELLIPSE`, `B_SPLINE_CURVE_WITH_KNOTS`;
  B-spline edges lying on analytic surfaces are recognised as lines,
  circles or ellipses where they fit within the file's uncertainty and
  snapped to exact; `SURFACE_CURVE`/`PCURVE` used when present, otherwise
  pcurves by exact inversion for analytic carriers and by
  `SplineSurface::invert` for splines.
- Conforming to the kernel's conventions: periodic faces split at the
  canonical seams (azimuth 0 and π, ADR 0016), faces reversed where
  `same_sense` says so, edge orientation from `ORIENTED_EDGE`, loops
  ordered outer-first, vertices welded at the file's
  `UNCERTAINTY_MEASURE_WITH_UNIT`, units converted (`SI_UNIT` prefixes,
  inch).
- I2c, later: native rational B-splines in `bspline.rs` (weights), which
  ADR 0050 deferred; conics from other kernels arrive as rational splines
  often enough to justify it.
- **Gates:** export → import → export is digest-stable for every kernel
  test body; the measured volume of an imported part matches the source
  system's reported volume within the file's uncertainty; the conformance
  corpus (parts exported from FreeCAD/OCCT, Fusion, SOLIDWORKS, Onshape:
  the same bracket, hub and housing from each) imports exactly or with
  named refusals, and the count of each refusal code is tracked as the
  roadmap ADR 0026's K2 asked for.
- Effort: 6–8 weeks (I2a/b), plus 3–4 for I2c.

### I3. Healing and the reference fallback

Imported geometry meets the kernel's agreement only after sewing at the
file's tolerance (S3's generalised `sew_shells`, using G1's model with
the file's uncertainty as the scale). Gaps beyond it, unreadable faces, or
a shell that does not close produce a per-face report
(`STEP_FACE_UNSUPPORTED`, `STEP_GAP_EXCEEDS_TOLERANCE`,
`STEP_SHELL_OPEN`) and the part opens as a **reference body**: the mesh
the scan add-on's reader already produces, displayable, measurable,
usable as a faceted-tier Boolean operand, badged `Tier::Approximate`.
Never a silently approximated solid. Effort: 3–4 weeks.

### I4. Product integration

`ReplayAction::ImportedBody { path, digest }` as a base feature whose
persistent references use the STEP entity ids as roles (`face("#412")`),
re-import on file change with reference repair through the existing
resolver; File → Import in the workbench; `import_step(path)` in scripts;
the part library storing imports; STEP assemblies
(`NEXT_ASSEMBLY_USAGE_OCCURRENCE`, placements) opening as components with
poses. Effort: 2–3 weeks.

### I5. Conformance, continuously

The corpus from I2 runs nightly; every new file a user reports is added
with its expected outcome. Effort: continuous; 1 week to set up.

## 9. Order, dependencies and effort

```
G1 agreement ─┬─ G2 builder/prism/planar ─┬─ B1 gate widen ── B2 quadric traces ── B3 quartic traces ─┐
              │                           │                                                             ├─ B4 SSI ── B5 ladder
              ├─ G3 fuzzer ───────────────┤                                                             │
              │                           └─ F1 rolling ball ── F2 concave ── F3 corners ── F4 curved ─┤─ F5 variable ── F6 face-face
              └─ G4 toolkit ──────────────────────────────────────────────────────────────────────────┘
I1 step crate ── I2 B-rep import ── I3 healing ── I4 product ── I5 corpus      (I2c rational after G4)
S1 sheets ── S3 stitch ── S2 trim/extend ── S4 offset/thicken ── S5 editing ── S6 surface blends
R1 predicates ── R2 complexity ── R3 budgets ── R4 corpus ── R5 determinism ── R6 incremental replay
```

| Track | Stages | Depends on | Effort (one engineer) |
|---|---|---|---|
| G foundations | G1–G4 | review Phase A/B | 11–16 weeks |
| B Booleans | B1–B5 | G1, G2, G4; B4 is the long pole | 26–34 weeks |
| F fillets | F1–F7 | G4, B2 (F4), B4 (F5) | 26–33 weeks |
| S surfacing | S1–S6 | G4, B (S2 splines), F1 (S6) | 19–27 weeks |
| R scale | R1–R6 | G1, G2, G3 | 17–21 weeks |
| I STEP import | I1–I5 | G1, S3 (sewing) | 15–21 weeks |

Sequentially that is about two and a half years for one person; the
tracks are independent enough that two engineers finish in fourteen to
eighteen months and three in about a year, with B4 the critical path
throughout. The order that pays back soonest:

1. **G1, G2, G3** (the review's Phase B and the fuzzer): everything else
   is cheaper and safer after them. ~3 months.
2. **I1–I3 and B1** in parallel: STEP import makes Artificer usable in
   someone else's workflow, and B1 lets imported analytic bodies combine.
   ~3 months.
3. **F1–F2 with B2**: the rolling-ball construction and the quadric
   traces share offset-surface intersections; together they deliver the
   two fillets users ask for first (boss-to-plate, cross-bore) and drafted
   Booleans. ~4 months.
4. **B3, F3, S1, R2**: quartic traces, corners, sheet bodies, the
   complexity work. ~4 months.
5. **B4**, then everything gated on it (F4 on splines, F5, S2 on splines,
   I2c). ~4–5 months.
6. **S4–S6, F6, R5–R6, F7, B5's badge**: the long tail. ~3–4 months.

## 10. What this programme does not do

- It does not claim exactness for SSI curves. They are certified within a
  stated tolerance and reported as such; that is what every commercial
  kernel does and what rule 1 makes honest.
- It does not add non-manifold topology, PMI/GD&T from AP242, colours
  beyond the exporter's, or sheet-metal, threads and drawings (product
  features that sit on top of these kernel capabilities and get their own
  ADRs).
- It does not remove the faceted tier. It makes reaching it rare, named,
  and visible.
- It does not start any track before G1: a second decade of tolerance
  formulas is the one outcome this document exists to prevent.

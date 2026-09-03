# Sketch offset implementation plan

Status: stages 1-4 implemented; self-intersection pruning and projected body edges remain proposed
Last reviewed: 2026-09-03
Programme position: the first sketch modifier that reads a whole connected chain rather than one curve or one corner

Offset takes a curve, walks every curve connected to it, and produces a second
chain that holds a set distance from the first, on the side the user chooses.
It is the tool that turns a drawn outline into a wall, a gasket, a clearance,
or a toolpath allowance without redrawing it, and it is the last of the
common sketch modifiers Artificer does not have.

This document is the specification, and the record of how much of it is built.
It states what the reference implementation does and why, what Artificer's
existing machinery already supplied, the exact geometry the first pass attempts
and refuses, and the staged work with its tests. Stages 1 to 4 have landed:
Offset draws, follows its sources, and refuses by name. Stages 5 and 6 —
self-intersection pruning, and offsetting an edge of a body through a
projection — have not.

## What the reference does

Fusion's `Modify ▸ Offset` (keyboard `O`) is the closest reference, and its
behaviour is worth stating precisely because several of its choices are load
bearing.

- **What it selects.** A sketch curve, a chain of connected curves, or a whole
  profile. *Chain Selection* is a checkbox in the dialog: with it on, picking
  one segment of a continuous shape takes every connected segment. ([Autodesk:
  Offset sketch geometry](https://help.autodesk.com/view/fusion360/ENU/?guid=SKT-OFFSET),
  [Autodesk: How to create and modify sketch geometry](https://www.autodesk.com/products/fusion-360/blog/how-to-create-and-modify-sketch-geometry-in-fusion-360/))
- **How the distance is set.** Drag a manipulator handle in the canvas, or type
  a distance in the dialog; the drag direction chooses the side. `Enter` or OK
  completes it. ([Autodesk: Offset sketch geometry](https://help.autodesk.com/view/fusion360/ENU/?guid=SKT-OFFSET),
  [tech&espresso: complete guide to the Offset tool](https://www.techandespresso.com/blog/fusion-360-offset-tool-guide))
- **It is associative.** The result is not a detached copy: the API models it
  as an `OffsetConstraint` carrying `parentCurves`, `childCurves` and the
  parameter controlling the distance, created by
  `GeometricConstraints.addOffset2`. Move the source and the offset follows;
  change the parameter and the offset moves.
  ([Autodesk: `OffsetConstraint`](https://help.autodesk.com/cloudhelp/ENU/Fusion-360-API/files/OffsetConstraint.htm),
  [Autodesk: `GeometricConstraints.addOffset2`](https://help.autodesk.com/cloudhelp/ENU/Fusion-360-API/files/GeometricConstraints_addOffset2.htm))
  It is also a well-known source of sketches that never reach *fully
  constrained*, because the offset distance is a degree of freedom the user did
  not knowingly add. ([Autodesk community: sketch offset not fully
  constrained](https://forums.autodesk.com/t5/fusion-design-validate-document/sketch-offset-not-fully-constrained/td-p/6271590))
- **`isTopologyMatched`.** The offset input can demand that the result match
  the topology of the source — the same number of curves, joined the same way —
  rather than whatever the geometry collapses to. It exists because re-offsetting
  an offset otherwise fails.
  ([Autodesk: `OffsetConstraintInput.isTopologyMatched`](https://help.autodesk.com/cloudhelp/ENU/Fusion-360-API/files/OffsetConstraintInput_isTopologyMatched.htm),
  [Autodesk: cannot offset a previously offset sketch](https://www.autodesk.com/support/technical/article/caas/sfdcarticles/sfdcarticles/Cannot-offset-sketch-that-was-previously-offset-in-Fusion-360.html))
- **It refuses body edges.** Sketch Offset works on sketch geometry only. To
  offset an edge of a solid you first *Project* it (`P`) into the active sketch;
  the projection is associative and updates when the body changes, and the
  offset then acts on the projected curve like any other.
  ([Autodesk: cannot select lines/edges to Offset in a sketch](https://www.autodesk.com/support/technical/article/caas/sfdcarticles/sfdcarticles/Cannot-select-edges-to-offset-in-Fusion-360-Sketch.html),
  [Autodesk: project geometry](https://help.autodesk.com/cloudhelp/ENU/Fusion-Sketch/files/GUID-850061C4-71F5-4418-AF9C-F0D232022F9C.htm),
  [Autodesk community: offsetting from the edge of a body](https://forums.autodesk.com/t5/fusion-design-validate-document/how-to-use-the-offset-tool-to-offset-from-the-edge-of-an/td-p/13756987))
- **Where it fails.** An offset into a concave region larger than the local
  radius of curvature self-intersects and collapses; acute corners make the
  join ambiguous.
  ([Autodesk: what to do when a sketch fails to offset](https://www.autodesk.com/support/technical/article/caas/sfdcarticles/sfdcarticles/What-to-do-when-sketch-fails-to-offset-in-Fusion-360.html))

The user's description — *all connected lines within a certain loop are
selected and can be offset in or out, including both sketch objects and edges
on a body* — matches this in every particular, with the body-edge half being
the project-then-offset route rather than a direct pick.

For the algorithm itself the clearest published account is
[CavalierContours](https://github.com/jbuckmccready/CavalierContours), which
offsets a polyline of lines and arcs in seven stages: generate raw offset
segments; trim or join them into a raw offset polyline; for open or
self-intersecting input, also compute the dual (opposite-sign) offset; find all
self-intersections; slice at them; discard the slices that come closer to the
source than the offset distance; stitch what remains. It joins convex corners
with a **round** arc, which keeps every point of the result at a constant
distance — the right choice for a cutter path, which is what that library is
for, and the wrong one for a sketch, where the offset of a rectangle has to
stay a rectangle. The staged shape of the algorithm is what is borrowed here,
not that choice. The same shape appears in the polyline-offset literature
([Liu et al., *An offset algorithm for polyline
curves*](https://www.sciencedirect.com/science/article/abs/pii/S0166361506001060)).

## What ships

| Piece | Where |
|---|---|
| The control surface: family, variant, icon, the `O` shortcut, the two phases, the signed `distance` and the `chain_selection` switch | `crates/sketch-ui/src/sketch_toolbar.rs` |
| The offset geometry and its refusals | `crates/sketch/src/offset.rs` |
| The chain walk | `crates/sketch/src/chain.rs` |
| The associative recipe, its output roles, its evaluation | `SketchRecipe::Offset`, `CurveOutputRole::OffsetCurve`/`OffsetJoin`, `crates/sketch/src/primitives.rs` |
| The canvas gesture and the typed distance | `SketchCanvasState::stage_offset` in `crates/sketch-ui/src/lib.rs` |

The tile sits at the end of the drawing block's last row, beside Trim, Fillet
and Chamfer ([ADR 0030](../adr/0030-ribbon-tabs-are-views.md)), and is live.

## What the existing machinery already supplies

Almost every part of this feature has a shaped precedent in the tree, which is
what makes it a bounded piece of work rather than a new subsystem.

| Need | What already does it |
|---|---|
| A recipe that reads other entities' evaluated curves | `SketchRecipe::RectangularPattern` and `pattern_sources` in `crates/sketch/src/primitives.rs` |
| Stable identity for many generated curves from many sources | `CurveOutputRole::PatternCurve { instance, source }` and `PointOutputRole::PatternPoint` |
| Emitting a transformed copy of any evaluated curve | `RecipeBuilder::add_pattern_curve`, which already switches over `Line`, `CircularArc`, `Circle` and `Bspline` |
| Connectivity between curves | `build_arrangement` in `crates/sketch/src/arrangement.rs` for regions; `crates/sketch/src/chain.rs` for the entity-level walk a recipe can name |
| Adjacent-span reasoning under a pick | `crates/sketch/src/trim.rs` (`TrimJunction`, `select_trim_span`) |
| Picking a curve under the pointer | `queries::hit_test_curves` |
| A live drag handle with a typed alternative | the rectangular-pattern direction manipulator, and the dimension-label drag |
| Staging behind the confirmation gate | `SketchTransaction::stage_modifier_with_inputs` |
| Body edges in sketch coordinates | `SketchContextCurve` (segments and arcs), already fed to the session as `support_curves` for snapping |
| Resource ceilings with a named reason | `SketchValidationError::ResourceLimit` and the `MAX_*` constants in `crates/sketch/src/definition.rs` |

The two genuinely new things are the chain walk and the offset geometry itself.

## The exact domain of the first pass

Offset acts on `EvaluatedCurve2::Line`, `CircularArc` and `Circle`. Splines are
refused by name; offsetting a B-spline exactly is not possible in general —
the offset of a degree-*n* NURBS is not a NURBS — and an approximation would
break the exactness promise the sketch crate makes everywhere else.

Per-curve offset at signed distance `d`, where the chain's traversal direction
defines the left normal:

- **Line** `start → end`: translate both endpoints by `d · n̂`, where `n̂` is the
  unit left normal of `end − start`. Exact.
- **Circular arc**, centre `c`, radius `r`, direction `dir`: same centre, radius
  `r ± d` — plus for an arc curving away from the offset side, minus for one
  curving towards it, which the arc's own direction decides. Endpoints move
  radially. Exact. The arc **degenerates** when the new radius reaches zero;
  that is a refusal, not a clamp.
- **Circle**: the same, and it is a chain of one. A circle has no ends, so
  neither does its offset.

Corners between consecutive offset curves. **The rule is one rule: the two
offsets meet where their carriers meet.** What differs between a convex and a
concave corner is only whether reaching that point extends the curves or trims
them.

- **Tangent corner** — the source curves meet tangentially (their directions
  agree at the shared endpoint within `angular_agreement_radians`). The two
  offsets already meet exactly; nothing is done but snapping them watertight.
  This is the common case for filleted outlines.
- **Convex corner** (the corner turns away from the offset side) — the offsets
  open a gap. Both **extend** along their own carriers to the meeting point
  nearest the source corner.
- **Concave corner** (the corner turns towards the offset side) — the offsets
  overlap. Both **trim** back to the same meeting point.

Carriers, not the bounded curves: a convex corner's meeting always lies past
both offsets' ends, and a concave one's routinely lies past a short neighbour's.
Line/line is a closed-form intersection; line/arc and arc/arc are the circle
forms in the same module.

This is what makes the offset *topology-matched to its parent*: four lines in,
four lines out, so a rectangle offsets to a rectangle and the result can be
dimensioned, constrained and offset again. It is `OFFSETGAPTYPE 0` in AutoCAD
and what SolidWorks' Offset Entities does; Fusion's rounding is the outlier
among CAD sketch offsets, and following it here would have meant every
rectangle acquiring four fillets nobody drew.

The cost, stated plainly: a convex corner's meeting point sits `|d| / sin(θ/2)`
from the source corner, where θ is the interior angle — `d√2` at a right angle,
and growing without bound as the corner sharpens. So the *corner point* is not
on the constant-distance locus, even though every *curve* is exactly `d` from
its own parent. There is no arbitrary mitre limit: the coordinate bounds
`validate_position` already holds every derived point to, and the direction
check below, are the guards, and a spike is the exact offset of what was drawn.

Two corners cannot be reached by extending:

- **Carriers that never meet** — two offset arcs whose circles have separated,
  or a line that misses one. Only at a convex corner: there the arc centred on
  the source corner at radius `|d|` is the one curve that closes the gap and
  stays exact, and it is emitted as `OffsetJoin { corner }`. At a concave
  corner it is a collapse, and refused.
- **A curve the meeting turns round.** Extending or trimming must never reverse
  a curve's direction or wrap an arc's sweep. That is a curve the distance has
  consumed, and removing it and rejoining its neighbours is the
  self-intersection pass below. Until that exists, it is a refusal naming the
  corner.

Global validity, after the per-curve and per-join pass:

- **Self-intersection pruning.** Concave stretches of the chain can eat each
  other well away from any single corner. Following CavalierContours, the
  offset chain is intersected with itself, sliced at every intersection, and
  each slice is tested against the source: a slice whose closest approach to
  the source chain is less than `|d| − linear_agreement` is discarded, and the
  survivors are stitched. This is the stage that makes a large inward offset of
  a narrow pocket return a shorter valid chain rather than a bow tie.
- **Topology matching.** `isTopologyMatched` is the reference's answer to
  re-offsetting an offset, and it is unconditional here: corners meet at their
  carriers, so the result always has one curve per source curve. Only a corner
  whose carriers never meet adds one, and only where nothing else can close the
  gap. If pruning would change that, the operation is refused with a reason
  saying the distance is too large for the shape, rather than silently
  producing something the user cannot re-offset.

Refusals, all named, all with the shape the rest of the crate uses:

| Condition | Refusal |
|---|---|
| A spline in the chain | `UnsupportedOffsetSource { entity }` |
| An arc or circle whose offset radius reaches zero | `OffsetCollapsesCurve { entity }` |
| A concave corner whose offsets do not meet | `OffsetCornerCollapses` |
| Pruning would change the chain's topology | `OffsetSelfIntersects` |
| Distance below `min_feature_size` | `FeatureTooSmall` (existing) |
| More generated curves than the sketch may hold | `ResourceLimit { resource: "offset_curves", .. }` (existing) |

## Chain selection

The chain is the transitive closure of *shares an endpoint within
`linear_agreement`* over the active profile curves of the sketch, starting from
the curve under the pointer.

It is a walk over whole entities, not over the arrangement. `build_arrangement`
knows more about connectivity than anything else in the crate, but what it
produces are fragments split at every crossing, and a fragment has no entity
identity for a recipe to store. The chain has to name the curves the sketch
actually holds, so `crates/sketch/src/chain.rs` walks those directly.

Three properties matter and are each a test:

- **Deterministic order.** The chain is returned in traversal order from an
  endpoint (for an open chain) or from the lowest stable entity id (for a
  closed one), so the generated curves' identities do not depend on pick order
  or hash iteration.
- **A junction of three or more curves ends the chain.** A T-junction has no
  single continuation, and guessing one is worse than stopping. The chain stops
  there and the offset is of what was walked; the highlight shows exactly that
  before the click.
- **Chain selection is a switch.** With `chain_selection` off, the chain is the
  single curve under the pointer — which is what a user wants when offsetting
  one wall of an outline.

Hovering paints the whole prospective chain in the relation-highlight style
that already exists, so what a click will take is visible before the click.

## The recipe

```rust
Offset {
    /// The chain in traversal order, with the reversal flags that make it read
    /// head to tail. Both are intent: which way round the chain is walked is
    /// what decides which side "left of travel" names.
    sources: Vec<ChainMember>,
    closed: bool,
    distance: SketchValue<SignedLength>,
}
```

It reads its sources as evaluated curves, never as recipes, exactly as the
pattern recipes do — so it respects the recipe boundary of
[ADR 0026](../adr/0026-second-expansion-programme.md) and works over
recipe-owned shapes (a rectangle's side, a polygon's edge) without touching the
owning recipe. It is not a modifier: it retires nothing, and stages through the
ordinary creation path.

Associativity falls out of that for free and is the point: the source moves,
the recipe re-evaluates, the offset moves with it. An edit that breaks the
chain the offset was taken from refuses the whole transaction rather than
offsetting a gap. Re-authoring the distance goes through the selected-recipe
editor, which replaces the operation in place, so a typed distance edits the
offset that exists rather than adding another one.

Identity: `CurveOutputRole::OffsetCurve { source: u16 }` for a curve derived
from the *n*th source, and `CurveOutputRole::OffsetJoin { corner: u16 }` for an
inserted round join, with matching `PointOutputRole` variants. Both are keyed
on where a curve came from rather than where it landed in the result: a
distance edit changes how many joins there are, and a curve whose identity
moved with the join count would take every dimension that referenced it with
it.

Persistence: a new `SketchRecipe` variant is additive under the existing
`#[serde(tag = "kind")]`, so documents written before it still load, and it
does **not** bump `CURRENT_DOCUMENT_VERSION`. The version constants in
`crates/model/src/lib.rs` mark structural schema changes — portable sketch
payloads at 4, the joint forest at 5, the editable sketch graph at 6 — and Trim,
Text and both patterns all landed inside 6 without one. Bumping to 7 would
stamp every save, including the overwhelming majority with no offset in them,
and refuse them all on an older build for nothing.

The cost of not bumping is that an older build meeting an unknown `kind` fails
with a serde error rather than the version refusal it has a message for. If
that is judged too sharp, the fix is a conditional stamp — write 7 only for a
document that actually contains an offset recipe — not an unconditional bump.
A round-trip test that a v6 document with an offset still loads, and a v6
document without one is untouched, is the gate either way.

Degrees of freedom: the offset distance is a real degree of freedom, and the
constraint solver must know it. `rigid_point_groups` treats each offset
instance as one body — the offset chain translates with its source rather than
shearing — matching what the pattern instances already do.

## Body edges

A body edge is not a sketch entity, so it cannot be a source. The reference
solves this with Project, and so does this plan, because the alternative — an
offset recipe that stores a face/edge reference — puts kernel topology
identities inside a sketch recipe and makes every sketch replay depend on the
body's edge numbering.

`SketchRecipe::ProjectedEdge { support: SupportCurveKey }` turns one
`SketchContextCurve` into a sketch curve with `SketchEntityRole::Reference`,
evaluated from the support geometry the session already receives. Offset then
treats it like any other source, and the chain walk crosses freely between
projected and drawn curves.

`SupportCurveKey` is the identity question this defers, and it is the reason
Project is its own stage: the key must survive a body rebuild, and the honest
first answer is the same one Fusion gives — a projection whose source has gone
is a broken projection, reported as such, not silently resolved to the nearest
edge.

## Interaction

1. `O` or the tile arms Offset. The prompt is the `chain` phase:
   *Hover a curve to highlight every curve connected to it, then click to take
   the chain.*
2. Hovering highlights the prospective chain. Clicking takes it.
3. The `distance` phase begins. Moving the pointer offsets live to whichever
   side the pointer is on; the magnitude follows the pointer's distance from
   the chain, snapped like every other live dimension. `Tab` opens the typed
   `distance` field, whose sign is the side. The `chain_selection` boolean is
   in the same palette.
4. A click, or `Enter`, stages the operation. The universal tick/`Enter` gate
   commits it. `Escape` discards.

Live preview draws the candidate offset in the staged-geometry style; a refusal
draws nothing and puts its named reason on the canvas instruction line, which
is what Trim and Fillet already do for their own refusals.

## Stages

Each stage is independently shippable and ends green.

**Stage 1 — offset geometry, headless. Done.** `crates/sketch/src/offset.rs`:
per-curve offset for line, arc and circle; the three join kinds; the corner
meetings; the named refusals. Pure functions over `EvaluatedCurve2`, tested
against hand-computed geometry — a square outward into a larger square and
inward into a smaller one, a filleted outline whose tangent corner stays
tangent, a step that trims one corner and extends the other, a step the
distance eats, the chain walked the other way, and the four refusals.

**Stage 2 — the chain walk. Done.** `crates/sketch/src/chain.rs`, with the
determinism, T-junction and construction-geometry properties as tests in
`crates/sketch/tests/connected_chains.rs`. No UI.

**Stage 3 — the recipe. Done.** `SketchRecipe::Offset`, its evaluation through
the recipe builder, `CurveOutputRole::OffsetCurve`/`OffsetJoin` and their point
roles, the persistence round trip, and tests in
`crates/sketch/tests/core_offsets.rs` that edit the distance, move a source and
watch the offset follow, and refuse an edit that breaks the chain the offset
was taken from.

**Stage 4 — the gesture. Done, in its first form.** One click: the chain is
what the picked curve is joined to, the side is which side of it the click
fell, and the magnitude is the palette's `distance`, which `Tab` retypes. The
result commits on acceptance like every other sketch stroke
([ADR 0027](../adr/0027-sketch-edits-commit-on-acceptance.md)).
Hovering a curve lights the whole chain a click would take, because "every
connected curve" is a claim about geometry the user cannot check by looking at
one of them. `apps/workbench/tests/sketch_offset_ui.rs` offsets a drawn
rectangle outward and inward from the canvas, drives the typed distance, and
holds the highlight to the chain; two snapshots record the highlight and the
outline it makes. The live drag that sets the distance by pointing rather than
by typing is the part of this stage still to come.

**Stage 5 — self-intersection pruning.** The slice-and-discard pass, replacing
stage 1's conservative whole-chain refusal for the cases it can resolve. Tested
on a narrow pocket offset past its own width and on a chain whose two ends
approach each other.

**Stage 6 — projection.** `SketchRecipe::ProjectedEdge`, `SupportCurveKey`, the
Project command, and an end-to-end test that projects a body edge and offsets
the projected chain. Until this lands, Offset acts on sketch geometry only —
which is exactly where the reference stands before Project.

## Deferred

- Splines. An exact offset of a B-spline is not a B-spline; when Artificer
  offsets one it will be by an approximation the precision policy governs and
  the document records, not by silence.
- A choice of join style. Corners meet at their carriers, full stop; a round
  join appears only where no extension can reach. Offering mitre/round/chamfer
  as an option means a field on the recipe and a migration, which is a larger
  price than one correct default.
- Variable-distance and two-sided offset.
- Offsetting a whole profile region as a region, rather than as the chain of
  its boundary.
- Automatic repair of a chain the offset breaks. A refusal that names the
  problem is the first pass's answer; the reference's own answer is the same.

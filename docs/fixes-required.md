# Fixes required

A running log of reported defects, each with the short investigation that
found where it lives. An entry records what was seen, what was looked at, and
the lead — enough for whoever picks it up to start at the right line rather
than at the symptom. Entries are removed when the fix lands.

Started 2026-09-19 from the 0.99.7 round of reports.

---

## 1. A dimension from a circle's centre to the edge of the face the sketch sits on does not take

**Seen.** In a sketch on a face (Face #79), pick a circle's centre, then the
face's own boundary edge. No dimension results.

**Looked at.** `crates/sketch-ui/src/lib.rs`: `RelationOperand` (line ~10436),
the distance-relation resolver (~9596), `support_segment_hit` (~8994),
`project_support_segment` (~9017).

**Lead.** A face's boundary is not sketch geometry. It reaches the sketch only
as *support curves* — snap references published by `set_support_curves` —
and a relation operand is only ever a sketch `Point` or a sketch `Curve`. The
path that bridges the gap is `project_support_segment`: a pick near a host
edge copies that edge into the sketch as a pinned `ProjectedEdge` entity so
there is a `Curve` to dimension against. Three places that can fail silently:

- `project_support_segment` returns `None` whenever `self.pending.is_some()`
  — a staged edit of any kind blocks the projection with no message.
- `support_segment_hit` only offers an edge within a pick radius scaled by
  `SUPPORT_EDGE_RADIUS_RATIO`; a pick on the face outline that lands outside
  it resolves to nothing.
- After the projection, the second pick must be re-resolved as the new
  entity; if the tool keeps the original (unresolved) pick, the pair never
  completes.

Reproduce with the two-circle face sketch and step through which of the
three it is. The mechanism is sound; it is the failure that is silent.

## 2. A line appears that was never drawn, and survives leaving the sketch

**Seen.** Inside the sketch, a solid vertical line the user did not draw.
After leaving, an orange line along the face's left boundary is drawn over
the body (image 13), in the same colour as the circles.

**Looked at.** `project_support_segment` and `pin_projected_edge` in
sketch-ui; `visible_sketch_overlays` in `apps/workbench/src/lib.rs` (~6829).

**Lead.** This is the same mechanism as entry 1, seen from the other side.
The dimension attempt projected the host edge into the sketch as a real,
visible, pinned `ProjectedEdge` entity. The model-mode overlay then draws
*every* active visible entity of the sketch in the sketch colour — it does
not distinguish a projected reference edge from a drawn curve — so the
reference shows up over the body as though it were geometry. Inside the
sketch it is drawn solid, indistinguishable from a user line.

Two things to decide, not one:

- The overlay should skip (or draw distinctly) `ProjectedEdge` entities.
- A projected edge that never ended up in a relation is an orphan and should
  be reverted, or shown as construction. It also feeds
  `refresh_profile_analysis`, so a projected line along a face border can
  change which regions the sketch closes — worth checking it cannot.

Note `project_support_segment` also reuses an existing matching projection
(`projected_edge_matching`), so repeated attempts will not stack, but the
first copy stays.

## 3. Tool-first invocation does not work: Extrude then click a face, Chamfer/Fillet then click edges

**Seen.** Extrude is greyed in Model mode with Sketch 2 selected (image 13).
Chamfer and Fillet can be pressed, but clicking edges afterwards does
nothing; they only work with a prior selection.

**Looked at.** `apps/workbench/src/lib.rs` `invoke_tool` (~6644) and the
functions immediately after it; `ribbon.rs` `extrude_availability` (~998)
and `preset_feature_availability` (~940).

**Lead.** Two separate causes.

*Picks never reach the armed tool.* The plumbing from ADR 0041 exists and is
unit-tested — `ArmedTool`, `offer_to_armed_tool`, `armed_tool_accepts`,
`armed_tool_prompt`, `disarm_tool` — but every one of those four is marked
`#[cfg_attr(not(test), expect(dead_code, reason = "wired up by ADR 0041
stage 4"))]` and has no production caller. Pressing Chamfer arms the tool
and sets the status prompt; the viewport's click handler then routes the
edge pick through ordinary selection, never to the armed tool. Stage 4 —
wiring `offer_to_armed_tool` into the viewport's vertex/edge/face pick
path, and `armed_tool_accepts` into its hit test — was never done. This is
task #23, still pending.

*Extrude's own availability rule has a gap.* In `extrude_availability`, the
sketch branch only permits "awaiting a profile pick" while
`workbench_mode == Sketch`, and the push-pull branch requires
`active_sketch_consumed`. Model mode with an *unconsumed* active sketch and
no profile picked — exactly image 13 — satisfies neither, so the button
greys. The ADR 0041 comment above the push-pull branch describes the
intended behaviour; the sketch branch was not brought in line with it.

## 4. View cube side arrows tilt when looking straight down

**Seen.** From FRONT (image 14) the left/right arrows are horizontal. From
TOP (image 15) they are rotated about 30° off horizontal.

**Looked at.** `view_cube_orbit_ring` in `apps/workbench/src/lib.rs` (~21883),
in particular the side-arrow placement after `nearest_azimuth`.

**Lead.** By design the side arrows sit on the orbit ring at
`nearest_azimuth ± VIEW_CUBE_SIDE_ARROW_AZIMUTH` and point along the ring's
*tangent* there. From the front the ring is edge-on, so the tangent projects
to pure screen-horizontal and looks right. From the top the ring is a full
circle face-on, so the tangent at ±θ from the nearest point is tilted by θ —
which is the tilt seen. It is the tangent rule doing what it says; it only
reads wrongly when the ring is open.

The arrows mean "turn left / turn right", which is a screen-horizontal
intention regardless of how the ring projects. Suggested fix: keep the
positions on the ring but blend the pointing direction toward screen
horizontal as `visibility` rises (the ring's own openness measure is already
computed a few lines below), or simply use screen ±x for the side arrows'
direction and leave the ring to convey travel.

## 5. The extrusion "To face" option cannot be used

**Seen.** In the extrusion panel with an Add preview staged (image 16),
"To face" cannot be selected — it appears to not register the click.

**Looked at.** `extrusion_extent_row` (~18520) and `arm_extrusion_face_pick`
(~9363) in `apps/workbench/src/lib.rs`.

**Lead.** The button is clickable; the click sets the intent to
`PickingFace` and calls `arm_extrusion_face_pick`, which counts the
`remembered_extrusion_targets` for that side. When there are **zero**, it
immediately sets the intent back to `Distance` and writes a status-line
message. The button therefore reads as selected for one frame and snaps
back, which is indistinguishable from an unresponsive button — and the
explanation goes to the status line, where it is easy to miss.

In image 16 the sketch is on the front face and the operation is *Add*
outward, so there genuinely is no parallel face ahead of it; the zero case
is correct for that direction. Two fixes worth making together:

- When there are no targets, keep the button visibly pressed and show the
  reason inline in the panel (next to the row), not in the status line.
- Consider whether target discovery should look in *both* directions when
  the operation is Auto, since a Cut inward from that face would find the
  back faces and is probably what the user meant to try.

The earlier report that "you could click the button and select a face"
matches the ≥2-targets branch, which still works: the difference is the
geometry, not a regression in the control.

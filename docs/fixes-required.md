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

---

## 6. A chamfer or fillet along an edge whose corner is not square is refused

**Seen.** Two edges of a ridge selected (image 17): the top face bends, so the
ridge is two edges meeting at a vertex where the crease across the top is the
third edge. The preview draws; applying refuses with
`EDGE_FINISH_BLEND_UNSUPPORTED`.

**Looked at.** The edge-finish ladder in `crates/kernel/src/lib.rs` (~3236):
rim blend → rim-loop blend → vertex blend → faceted → logical successor.
`crates/kernel/src/vertex_blend.rs` module doc and the seam check (~1054).

**Lead.** This is a two-of-three corner, which ADR 0043 closes with a seam —
but the seam construction requires *three mutually square faces*, checked at
`vertex_blend.rs:1054` by `normals[i]·normals[j] ≤ angle_tolerance`. A corner
that leans returns `DomainUnsupported`, the faceted tier then fails to weld,
and the generic sentence is what the user sees. The comment on that check
already names the case ("a hexagonal pocket's, where the two walls meet at
120°").

The squareness condition exists for a *fillet*, whose seam trace on a band is
a pure harmonic only when the band axes cross at the ball centre. A
**chamfer's seam is planar** — two chamfer planes meet in a straight line —
and needs no squareness at all; the module doc says as much for the corner
patch ("A chamfer's corner is planar and has no such condition"). Applying
the fillet's gate to chamfers is what refuses image 17. Fixing that is a
closed-form, general case, not coverage: intersect the two chamfer planes.

For a fillet at a leaning corner, the seam is the intersection of two
cylinders whose axes cross at the ball centre but at angle ≠ 90°: still an
ellipse in the plane of the axes, so still in vocabulary — the derivation
just needs the general angle rather than assuming it. Worth doing at the
same time; it is the robust answer the user asked for.

## 7. A fillet on the rim of a hole through a side wall refuses

**Seen.** Hole rim on the front face, two edges selected (the circle is split
at the cylinder seam). Preview shows an "exact rim blend"; applying gives
`NOT APPLIED — the selected edge neighbourhoods could not form a certified
regularized corner blend` (image 18).

**Looked at.** Same ladder; `section_revolve.rs` and `rim_loop_blend.rs`
(~66–92) target domains; `vertex_blend.rs` module doc.

**Lead.** No exact rung owns this edge. `rim_loop_blend` finishes the rim of
a *prism cap* — it resolves a cap via `resolve_cap_rim`, needs
`prism.height()`, and re-extrudes an offset profile; hole loops on the cap
are fine, but the front face here is a *side wall*, not a cap. `section_revolve`
is a revolved body's rim. `vertex_blend` takes convex edges between *planar*
faces and this edge borders a cylinder. Every exact rung returns
`DomainUnsupported`; the faceted tier fails to weld the torus; the message is
the ladder's last resort.

Two things worth noting. The preview reports `EXACT RIM BLEND` because the
preview's classification and the ladder's dispatch disagree about which rung
will answer — the panel promises a route that then declines. And the fix is a
new exact construction, not a bug: a concave circular rim between a plane and
a cylinder is a torus patch (or a cone for a chamfer) added along one closed
edge, with no corners to close. That is a smaller, fully closed-form piece than
either existing rim rung, and it is the one the user reaches every time they
round a drilled hole.

## 8. Regions closed by the face's own outline or a hole rim are not recognised, and a profile touching a void is refused

**Seen.** New sketch on a face with two through-holes; two circles drawn
around the holes. The area between the drawn circles and the face's outer
boundary is not selectable though enclosed (image 19). Extruding the annuli
inward is refused: `FACE_FEATURE_PROFILE_OUTSIDE_FACE — the profile must lie
strictly inside selected face material and outside its voids` (image 20).

**Looked at.** `crates/sketch-ui/src/lib.rs` `refresh_profile_analysis`
(~8029); `crates/kernel/src/face_feature.rs`
`profile_region_is_strictly_inside_face` (~982) and
`polygon_boundaries_touch_or_too_close`.

**Lead.** Two halves of one design decision, and both are now stricter than
the engine underneath them.

*Regions.* `refresh_profile_analysis` builds regions from `self.entities`
only. The face's outline and its hole rims reach the sketch as *support
curves* — snap references — and take no part in closure. So "the rectangle
minus two discs" is not a region, because its outer boundary is the face's
outline, not a sketch curve. This is why the enclosed area is unselectable.

*The gate.* `profile_region_is_strictly_inside_face` refuses any profile
boundary within `min_feature_size` of the face's outer loop or any inner
loop. An annulus whose inner boundary *is* the hole rim is refused as
"touching a void". There is no gap between the circles — the user's guess of
an infinitesimal distance is close, but it is a *coincidence*, refused
because it is exact, not because it is small.

The gate predates ADR 0045. The Boolean now resolves coincident boundaries
by orientation, so a profile boundary coincident with a face boundary or a
void rim is an operand the engine can take; the gate is refusing what the
engine would answer. Suggested fix, and it is general: let support curves
participate in region closure (a region closed by the face's own boundary is
the commonest face feature there is), and relax the gate from "strictly
inside, not touching" to "inside or coincident with", leaving *crossing* the
boundary as the refusal. Coincidence is exactly the case ADR 0045 certified.

This will recur everywhere a sketch on a face references what is already
there — a boss around a hole, a pocket to a wall, a rib to an edge — which is
why the user flagged it as the important one. They are right.

## 9. A slot cut from a sloped face across two bores refuses

**Seen.** A stadium slot sketched on the sloped top face (Face #76), Cut
through the part, crossing both bores. Refused:
`FACE_FEATURE_FACETED_UNRESOLVED — the faceted cut could not be regularized
into a closed solid`, with Euler / face-loop / orientation / pcurve
diagnostics (image 21).

**Looked at.** The crossing-cut path in `crates/kernel/src/lib.rs`
(~1036–1098): prism reduction → `analytic_cut` → faceted
`subtract_crossing_profile` → `certify_faceted_candidate`;
`crates/kernel/src/surface_intersection.rs` (~520 and the domain table at ~20).

**Lead.** The analytic engine refused first and the faceted tier is what
failed loudly. Cylinder–cylinder intersection is exact only for coaxial,
parallel, or *equal-radius crossing* axes (P5); `surface_intersection.rs:520`
says the rest plainly: "Unequal radii, or skew axes: a genuine space quartic"
— refused by name. The slot's end-cylinders have the slot's half-width as
radius, not the bore's, and because the slot is cut from a sloped face its
axis is tilted to the bores, so the axes are skew as well. Both conditions
hold; the analytic route returns `None`.

Two defects here, one of diagnostics and one of domain.

*Diagnostics.* The `analytic_cut` closure maps the engine's error with
`.ok()` and `.flatten()`, so *why* the exact route declined is discarded and
the user is shown only the faceted tier's welding failure — a message about
tessellation for a problem that is about vocabulary. The refusal reason
should survive to the report; the ladder in edge-finish already does this
("the exact rung that owned this request said why").

*Domain.* The general cylinder–cylinder trace is algebraic in one cylinder's
azimuth for any radii and any axes — on cylinder A, height is a closed-form
function of θ. The engine already carries a sampled analytic section chord
(`Harmonic`, with refinement) for the equal-radius case. Generalising that
chord to the full trace — sampled and refined exactly as harmonics are now,
with the tangency and coincidence handling of ADR 0045 — is the robust route,
and it removes the equal-radius and crossing-axes restrictions together
rather than adding another special case. The faceted tier is the wrong
fallback for this shape and should not be the one that speaks.

# Fixes required

A running log of reported defects, each with the short investigation that
found where it lives. An entry records what was seen, what was looked at, and
the lead — enough for whoever picks it up to start at the right line rather
than at the symptom. Entries are removed when the fix lands; the numbers are
the report's and are not reused.

Started 2026-09-19 from the 0.99.7 round of reports. Entries 1–5 and 8 have
landed.

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

Since entry 8 landed, a carrier pair the intersection matrix refuses is only
a refusal when the two *faces* could meet (`faces_apart` in
`analytic_boolean.rs`); the slot's end-cylinders do meet the bores, so this
entry stands on the domain half.

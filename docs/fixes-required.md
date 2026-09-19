# Fixes required

A running log of reported defects, each with the short investigation that
found where it lives. An entry records what was seen, what was looked at, and
the lead — enough for whoever picks it up to start at the right line rather
than at the symptom. Entries are removed when the fix lands; the numbers are
the report's and are not reused.

Started 2026-09-19 from the 0.99.7 round of reports. Entries 1–8 have
landed, and the diagnostics half of 9.

---

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

Two defects here, one of diagnostics and one of domain. The diagnostics
half has landed: the engine names the carrier pair the faces bring together
(`AnalyticBooleanError::CarrierPair`), and every face-feature outcome that
the exact route stood aside from carries
`FACE_FEATURE_EXACT_ROUTE_DECLINED` with that reason — a warning beside an
approximation, the first diagnostic of a refusal. What remains is the
domain half.

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

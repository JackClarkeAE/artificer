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

**Where it stands.** Both halves of this entry have moved, and one is done.

*Diagnostics — done.* The exact route's reason travels with every outcome as
`FACE_FEATURE_EXACT_ROUTE_DECLINED`, and the engine names what it met.

*Domain — the curve is in, the closure is not.* ADR 0047 put the general
cylinder–cylinder trace into the vocabulary: on either cylinder's azimuth it
is the root of a quadratic whose coefficients are trigonometric polynomials,
so it is exact, and it is now carried by `Curve3::Trace`, `Curve2::Trace` and
`Segment::Trace` through tessellation, transforms, measures, the digest and
the validator. The intersection matrix produces it instead of refusing, and
cylinders that simply miss now answer `Empty` rather than "unsupported".

What is left is the last step: assembling the section a trace leaves on a
face into a face boundary. On a half-cylinder face a trace can enter and
leave by the same seam — a bite out of the face's edge — which a plane
section never does; that case is closed along the seam, and others remain.
`close_periodic_sections` in `analytic_boolean.rs` is where this lives, and
it reports `BOOLEAN_TRACE_NOT_CLOSED` rather than guessing. Until it is
finished these cuts still reach the faceted tier and are labelled
approximations, exactly as before — no behaviour has regressed; the reason
given is simply the true one now.

The reproducer to work against is
`crates/kernel/tests/exact_route_decline.rs`: two bores of unequal radius
crossing. Dumping the pieces that reach `close_periodic_sections` for the
wider bore's half-face shows the shapes that need closing.

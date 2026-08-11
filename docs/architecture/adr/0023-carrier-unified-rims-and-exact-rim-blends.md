# ADR 0023: Carrier-unified rims and exact rim blends

Status: implemented — carrier-unified rims, torus rim fillets, cone rim
chamfers (single and double rims). The deferred frontier is largely closed:
vertical-edge blends on arbitrary prisms, a general (r, z) section revolve
that makes blends stack, and rim-loop chamfers on straight prisms all ship.
Rim-loop fillets ship too, with quarter-cylinder bands, sphere corner
patches, and the flat ledges those imply, measured by a general shell engine
that needs no common axis. Deriving the corner showed the sphere is tangent to
the cap — it borders a ledge rather than a cap arc — and that its meridians
are exactly the adjacent bands' end arcs, so the patch closes as a
three-sided face and the pole-edge vocabulary sketched earlier is unnecessary.
Arcs in a rim-loop profile are supported too, convex and concave alike: they
sweep torus bands under a fillet and cone bands under a chamfer, about their
own centre. A tangent junction shares one seam arc between neighbours instead
of needing a corner at all, and a sharp junction touching an arc sets its band
back along the arc rather than along a chord, so the sphere patch spans the
whole normal turn including the arc's own. A concave carrier is traversed
clockwise, so its wall and band surfaces reverse — the cylinder through its
angular sign, the torus through a flipped axis — keeping every parameter loop
counter-clockwise. Either cap rim finishes: the bottom is the top of the same
prism mirrored through its far cap, so one builder serves both. What remains
is holes, and sharp junctions between a line and an arc under a *chamfer*,
whose two slants would meet in a plane/cone conic outside the curve
vocabulary; those reject transactionally.
Date: 2026-08-07

## Context

Full circles are represented as two exact semicircle edges with seam vertices,
and cylindrical walls as two half-faces with seam generators (ADR 0016). That
representation is sound for construction and validation, but it leaked into
interaction: selecting a cylinder rim highlighted half the circle, and any
future blend feature needs "the rim" as one addressable entity. Separately,
Fillet/Chamfer supports only straight prism edges; circular rim edges reject
as `EDGE SET UNSUPPORTED` because curved blends require torus/cone blend
surfaces the kernel does not yet model.

## Part 1 (implemented): carrier-unified logical edges

`NativeKernel::carrier_edge_group(snapshot, edge)` returns every edge sharing
the seed's analytic circular carrier (equal centre, radius, and plane normal
within presentation agreement, `1e-9` scale-relative). The decision is made on
authoritative curve data, never on sampled display chords — the previous
chord-tangency heuristic could never certify tangency across a seam vertex at
any sampling density. The workbench expands edge selection through this group,
so a rim selects, highlights, and enters edge-set features as one closed
logical edge; seam generators and straight edges remain their own groups.
`crates/kernel/tests/cyl_probe.rs` pins the contract: a rim groups to exactly
its two semicircles with circumference `2πr` to 1e-12, and a seam stays alone.

The same leak has a display half. `presentation_edge_classification` hides the
boundary between two half-faces of one carrier, so a seam never draws as a
model edge; that test originally recognised cylinders only, and the blend
surfaces added in Part 2 are split the same way. A torus, cone, or sphere
carrier is therefore compared on its own analytic parameters — coincident
origin and axis, equal radii, and for a cone the slope negated when the axis is
anti-parallel — and its seam is hidden on the same terms. The comparisons stay
strict (`1e-9` scale-relative): a false negative only draws a line that should
not be there, while a false positive would erase a genuine rail.
`crates/kernel/tests/revolve_stack_probe.rs` pins it — on a doubly-filleted
cylinder every edge that still presents as hard is a horizontal ring, and the
two cap tangency rings are still present as selectable rails.

## Part 2 (fillet implemented): exact rim blends without general intersection

The key observation making rim blends tractable now: **for a fillet or chamfer
on the rim of an upright extruded cylinder, every trimming curve is an exact
circle.** A blend surface tangent to both the cylindrical wall (radius `R`,
axis `n`) and the planar cap (height `h`) touches each in a tangency circle;
no general surface–surface intersection is required.

Fillet of radius `f` (require `0 < f < R` and `f <` wall height):

- Wall shortens to `z ≤ h − f`; new tangency circle: radius `R` at `z = h − f`.
- Cap shrinks to radius `R − f`; new rim circle: radius `R − f` at `z = h`.
- Between them, a quarter-torus band: centre-circle radius `R − f` at
  `z = h − f`, minor radius `f`, minor angle `θ ∈ [0, π/2]`. Split into two
  half-band faces along the existing seam plane so the semicircle-pair
  representation stays uniform. The band's seam generators are quarter-circle
  arcs of radius `f` in the axial seam plane — still `Curve3::Circle` with a
  bounded parameter range, so **no new curve type is needed**.
- New surface variant `Surface::Torus { center, axis, radial, major_radius,
  minor_radius }` mirrors the vocabulary already present in `brep.rs`.
  `f < R` keeps it a ring torus (no self-intersection).

Chamfer of distances `(d_axial, d_radial)`: identical topology with a cone
frustum band (`Surface::Cone`) from circle `(R, h − d_axial)` to circle
`(R − d_radial, h)`; seam generators are straight slant lines.

Exact measures (validation gates, no tolerance):

- Quarter-torus band area: `2πf · ((R − f)·π/2 + f)` (Pappus with the
  centroid-offset correction; validate against the analytic surface integral).
- Filleted-cylinder volume: `πR²(h − f) + π(R − f)²·f +` the quarter-torus
  solid of revolution `2π · [ (R − f)·(πf²/4) + f³·(2/3)... ]` — derive the
  final closed form in the implementation test from the divergence theorem and
  pin it, as the existing annulus tests do.

Validator additions: torus/cone frame checks (unit axis ⊥ radial, radius
ordering), p-curve loci on the new surfaces (both parameterize linearly, so
existing line p-curves suffice), and unchanged Euler/use-count families (the
band insertion replaces one rim pair with two circle pairs plus two seam arcs
and two half-band faces; `V − E + F` is preserved).

Protocol: extend the existing `FinishEdge` command domain rather than adding a
command. The kernel resolves the target through `carrier_edge_group`; a full
circular rim on an upright prism cap↔wall junction enters the new exact rim
path, straight prism edges keep the existing exact path, and everything else
still rejects transactionally. Rejection matrix to pin: `f >` R, `f ≥` wall
height, partial-arc rims, tilted junctions, rims shared by more than two
faces. (`f = R` was in this list until Part 3 showed it builds a dome.)

## Part 3 (implemented): poles, where a rim blend reaches the axis

A fillet whose radius equals the wall radius consumes the cap exactly. The
arc it leaves is centred *on* the axis, so it sweeps a sphere — filleting one
rim of a cylinder domes it, and filleting both rims of a cylinder exactly
twice as tall as it is wide turns it into a sphere. This was previously
refused as an oversized radius, which rejected a constructible solid; only
overshoot, where the tangency foot falls off the cap entirely, has no
certified answer, and that still rejects.

Three facts make it work, and each was a separate refusal:

1. **Consumption is not overshoot.** `corner_blend` reports which neighbours a
   blend consumed exactly rather than refusing them, because the two are
   different geometric facts. Callers decide: a revolved section drops the
   collapsed piece, while `prism_edge_finish` still rejects, since a prism
   profile is rebuilt segment by segment and has no representation for a piece
   that is gone. A remnant too small to use but not gone is still refused —
   exact consumption is legal, slivers are not.
2. **The sweep direction comes from the carrier, not the remnant.** The
   connector's sense was read off the trimmed incoming segment, which is
   undefined once that segment closes to a point. It is now read off the
   incoming carrier at the tangency foot; the two agree wherever a remnant
   survives.
3. **An arc centred on the axis sweeps a sphere.** Emitting a torus of zero
   major radius instead would be a carrier whose parameterization collapses
   onto its own spine.

**Pole closure.** Where a section curve ends on the axis the ring it sweeps has
zero radius, so the face needs a fourth side to close in parameter space. One
degenerate edge stands in for the whole singular iso-line — the vocabulary the
validator already certifies. Both half-patches share that single edge with
opposite senses, which is forced: `validate_edge_uses` requires every edge to
be used exactly twice with opposite orientations and grants poles no
exemption, so neither one edge per half (used once each) nor a three-sided
loop would certify. A pole edge is excluded from the Euler characteristic: it
is a parameter-space device, not a topological 1-cell, and the point set it
stands for is the vertex at both its ends, already counted. Counting it would
drive a sphere's characteristic to one.

Still refused, deliberately: a slanted section line reaching the axis, which
would sweep a cone apex — a sharp singularity rather than a pole — and a
chamfer that consumes a neighbour exactly, which would leave the connector
spanning the whole piece. `crates/kernel/tests/sphere_from_rims_probe.rs`
pins volume, area, and centroid against the classical closed forms, and
checks every sampled surface point lies on the sphere, which a
zero-major-radius torus would fail while still closing topologically.

## Consequences

Interaction now treats rims as single closed edges without changing the
persisted representation, so no document migration is needed. Part 2 stays
within the analytic curve vocabulary and the transactional validation
contract; its cost is concentrated in `Surface::Torus`/`Surface::Cone`
support across the validator, measures, transforms, semantic hash, and
display tessellation, plus the rim-finish builder itself. The general
curved-blend problem (blends along non-circular edges, blend–blend corners
needing sphere patches, variable radius) remains explicitly deferred.

One convention changed to let a band bound material on the inside of its own
carrier. `Surface::Cone` previously required `radial_u × radial_v = axis ·
angular_sign`, which pinned its normal outward and made a concave cone band
inexpressible under the counter-clockwise loop rule. It now matches
`Surface::Cylinder`: the frame stays right-handed and the angular sign alone
decides which way the surface faces. Every existing cone carries
`angular_sign = 1`, where the two conditions coincide, so nothing already
built changes meaning.

Exact shell measures are now orientation-aware: each face's flux contribution
carries the sign of its parameter loop's signed area and of its angular sign,
so a reversed carrier integrates correctly instead of silently subtracting.
The loop-winding family still rejects a clockwise face outright, so in
practice the loop sign is always positive; carrying it keeps the measures
honest for candidates that have not yet been validated.

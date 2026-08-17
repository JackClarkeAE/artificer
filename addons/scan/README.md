# Artificer Scan-to-CAD

The first Artificer add-on package: turns triangle meshes from 3D scanners
into aligned, segmented, analytic geometry, following the
estimate-then-polish structure of commercial metrology pipelines
(Creaform VXmodel, Zeiss GOM family).

This is a **standalone workspace** — deliberately not a member of the root
workspace, so it builds with its own lockfile and target directory while the
main tree compiles. It depends on `crates/geometry` (read-only path
dependency) so all types are kernel-native.

## Pipeline

1. **Import** — binary/ASCII STL and ascii/binary PLY, welded into indexed
   meshes (`stl`, `ply`, `mesh`).
2. **Align** (`register`) — PCA pre-alignment plus trimmed point-to-plane
   ICP ("best fit" alignment), with Tikhonov regularization so scans that do
   not constrain every degree of freedom never diverge. 3-2-1 datum
   alignment via `datum_alignment`.
3. **Segment** (`segment`) — sharp-edge region growing over face normals,
   followed by **RANSAC peeling** (`ransac`): Schnabel-style
   detect-and-subtract over whatever the region growing left freeform.
   Minimal point+normal candidate constructions (1-sample plane, 2-sample
   sphere/cylinder, 3-sample cone) are drawn from local BFS neighbourhoods,
   scored by inlier support (distance band + normal agreement, area
   weighted, parsimony biased), extracted as the largest connected
   component, refined, and peeled — deterministically (seeded RNG, sorted
   adjacency). This is what makes real scans work: scanners round every
   physical edge, so region growing alone cannot isolate analytic patches.
4. **Fit** (`fit`) — plane / sphere / cylinder / cone: algebraic initial
   estimates (PCA plane, Kåsa sphere/circle, normal-covariance axes, tangent
   plane apex intersection) refined by Levenberg–Marquardt, with
   RMS/max deviation statistics for tolerance-driven model selection.
5. **Fragment merge and absorption** (`merge`) — RANSAC extracts connected
   components, so one physical surface interrupted by keyways or slots
   returns as several fragments. Compatible fragments (coaxial cylinders,
   coplanar planes, concentric spheres) accrete greedily, and each merge
   is accepted only when a least-squares refit over the union of faces
   still meets tolerance — a genuine 46.8 mm step never swallows a
   47.3 mm relief. Absorption then handles what parameter compatibility
   cannot: a small noisy patch on a big surface may have fit as a tilted
   plane, a huge sphere, or stayed freeform — its parameters are noise,
   but its **point membership** (distance band plus normal agreement
   against the anchor surface) is decisive, so anchors claim on-surface
   patches regardless of the patch's own kind and refit once grown.
   Coplanar disconnected lands — a gear's tooth tops against its face —
   deliberately count as one plane feature. A significance filter then
   demotes analytic patches below `--min-feature` (default 25 mm^2) to
   freeform: a few square millimetres of "cone" on a large part is
   transition geometry, not a design feature.
6. **Auto-datum** (`datum`) — cluster feature directions (cylinder/cone
   axes, plane normals, area weighted, sign insensitive): the dominant
   cluster becomes +Z, the strongest perpendicular cluster +X, and the
   origin lands where the dominant axis meets the largest perpendicular
   plane. All features are re-expressed in this frame, so canonicalization
   works on unaligned scans.
7. **Stitch and recognize** (`reconstruct`) — with the datum known,
   near-axis cylinders re-fit with the axis *locked* (an exact 2D circle
   fit), which stops small noisy patches from wobbling and lets a second
   merge pass stitch interrupted bands into one surface. Remaining
   freeform patches project into profile space `(radial distance, height)`
   where a fillet ring is a plain circle: rings with real area and wide
   angular coverage that fit inside tolerance become recognized blends.
8. **Canonicalize** (`snap`) — snap axes to datum directions, dimensions to
   a round grid, harmonize coplanar planes and coaxial cylinders; every
   adjustment is recorded as a note so the metrology story stays honest.
9. **Revolved-band extraction** (`reconstruct`) — interrupted surfaces
   of revolution (a gear's tooth-tip and root bands, a spline's lands, a
   synchro taper) defeat per-region fitting: every land patch bleeds
   into its neighbours through edge rounds, so its own fit tilts and
   fails the axis lock. In profile space `(radial distance, z)` a
   cylinder is a vertical ridge and a cone is a slanted line. Phase one
   histograms radially-facing donor faces by radius and claims dominant
   vertical ridges as axis-locked cylinders; phase two detects slanted
   lines with a deterministic line RANSAC and claims them as axis-true
   cones. Donors include freeform patches, small scraps, **tilted**
   cylinders/cones (a tilted axis on a revolved part is a misfit, not a
   feature) and spheres (a shallow cone reads as an absurd giant
   sphere). Every claim is gated by the locked fit meeting tolerance,
   and the master tooth cross-section exports as a sweepable profile
   polyline (`--profile-out` plots it; the history proposal carries it
   as a `helical_sweep_pattern_proposal`).
10. **Pattern detection** (`reconstruct`) — the freeform residue's densest
   band is autocorrelated azimuthally with two complementary signals: the
   area histogram (sparse patterns: lugs, bosses) and the mean-radius
   profile (dense bands: gear teeth, where the circumference is fully
   covered and only the root-to-tip radius oscillation carries the
   pattern). Signals are built per z-slab so helical patterns — whose
   azimuth drifts with height — still sum coherently, and the peak picker
   climbs the harmonic ladder to undo subharmonic aliasing. A coverage
   metric reports how much of the scan the plan explains, and stays
   honest: unmodelled geometry counts against it.
11. **Master-pattern recognition** (`reconstruct`) — the detected n-fold
    band becomes a single design feature: every member face's azimuth is
    unwrapped by an estimated helix rate (scanned for the rate that folds
    tightest — measuring the helix angle from the scan) and folded into
    one sector, where all instances collapse onto a master height-field
    `radius(folded azimuth, z)`. Residuals are trimmed against the median
    (tooth-end chamfers are not part of the repeated surface and honestly
    stay outside), the master rebuilds from survivors, and the surviving
    faces are claimed into one `Pattern` feature. The fold RMS and
    worst-instance RMS are reported — they are the tooth-to-tooth error.
12. **Reconstruct** (`reconstruct`) — level planes and on-axis cylinders
   assemble into a revolved profile: a stack of annulus segments (bores
   from inward-facing walls, bosses from outward), with fillet and chamfer
   proposals attached to the profile corners they round. `--history`
   emits the plan as replay operations: `make_revolved_annulus` entries
   are wire-exact for the Artificer protocol's `KernelCommand`;
   `finish_edge_proposal` entries carry geometric edge descriptors until
   an executing kernel can resolve entity references. Where the true
   boundary is not a surface of revolution (gear teeth), the plan says so
   in a note instead of inventing geometry.

13. **Feature instances** (`instance`) — what several surfaces are
    *together*, which per-surface recognition can never say. A hex boss
    is six planes and a designer's single extrude; a tilted pump stub is
    a cylinder, a cone and a cap and one revolve about an axis the
    datum-locked pipeline cannot express. The commercial packages settle
    this with a human — the operator picks a region and a wizard fits the
    extrusion — and the deliverable is the ordered feature tree, with the
    surfaces as scaffolding. This is that wizard layer without the
    operator.

    Grouping is by **invariance**, not by fitting something new: a
    surface belongs to an extrusion along `d` exactly when sliding along
    `d` maps it onto itself — a plane whose normal is perpendicular to
    `d`, a cylinder whose axis is parallel to it. Candidates come from
    the surfaces themselves (cylinder axes, the datum axis, and the cross
    of every adjacent plane pair — two planes sharing an edge extrude
    along the line where they meet), membership is cut by the part's own
    mesh adjacency so two unrelated bosses on a shared axis stay two
    instances, and each group's **pooled samples must then survive the
    kinematic classifier**: the linear line complex is the judge of
    whether the group really is swept by the motion that nominated it.

    Direction agreement alone is not acceptance. The pump chained 54
    casting walls into a "273 mm deep extrusion" whose direction matched
    perfectly and whose path-normal residual was 7.75 mm at a 0.2 mm
    tolerance — 98 such instances in total. Gating on the residual as
    well cut that to 4, and the distribution says the threshold is
    honest rather than tuned: accepted instances run 0.39–0.77 mm, the
    cap sits at 0.8 mm, and the *best* refusal is 1.03 mm with a median
    of 5.42 mm. There is a real gap under the line.

    Sketches come out exact, from the carriers rather than the mesh: a
    member plane meets the sketch plane in a line whose direction is
    `normal × d` and whose extent its measured corners bound; a member
    cylinder is a circle at its own fitted radius, carrying the share of
    the full circle the scan actually covers. Revolve profiles run in
    (radius, height-along-axis) the same way. Both export as
    `extrude_instance_proposal` and `revolve_instance_proposal` history
    operations beside the existing revolved-annulus ones.

    Extents come from **occupancy, not bounding boxes**. The merge stage
    unifies coaxial fragments, so one cylinder record can hold two stubs
    at opposite ends of a part and a raw min/max spans the empty gap
    between them: the pump's 10 mm pipe stub first reported a 179 mm
    profile run. Heights are binned by area, bins holding real material
    are kept, and the longest unbroken run of them gives the extent —
    the same occupancy test `split_disjoint_bands` uses on the datum
    axis, and it belongs on any extent taken over a feature's faces.

    Neither test part is extrusion-dominated, and the results say so
    honestly: the gear finds **nothing** — it is a pure revolve about the
    datum, already expressed by the revolved-profile plan, and instances
    deliberately skip datum-axis revolves — while the cast pump yields 4
    extrusions and 2 revolves against 705 refusals. The stage reports its
    refusals even when it finds nothing, because "we looked and found
    none" and silence are different statements.

14. **Feature tree** (`tree`) — the recovered operations put in replay
    order. Everything upstream produces an unordered bag; a CAD model is
    a *sequence*, and that sequence is what a designer edits and what
    makes the model replayable at all.

    Two questions are answered from evidence rather than assumed. **Which
    way does a feature act?** A wall whose measured normals point away
    from its own axis has material on the inside and is a boss; one whose
    normals point inward is a bore. The revolved-profile stage already
    reads bosses and bores exactly this way, and the same test settles an
    extrusion by asking whether its walls face away from the sketch's own
    centre. A surface square to the axis — a flat cap — has no radial
    opinion, and says so rather than being made to guess. **What is the
    base?** The largest thing that can stand alone: the revolved stack if
    there is one, otherwise the biggest additive instance, or the
    measured body where a casting leaves surface with no analytic form —
    which is precisely the hybrid the commercial packages build, an
    organic solid with machined features booleaned into it.

    Fillets and chamfers come last, always. They reference edges that
    exist only once the solids they round have been combined, so a tree
    that applies them earlier cannot be replayed. The gear orders as:
    base revolved profile → measured body → pattern band → three fillets
    → a chamfer. The tree is exported as a `feature_tree` operation
    naming each step's role and the operation it refers to.

15. **Deviation map** (`coverage::deviation_bands`, viewer) — the
    colour map the commercial packages show live while a model is built,
    and the number a metrologist signs off on: not *does* the model
    explain the scan, but **by how much, and where**. The viewer's right
    pane cycles segmentation → sharp rebuild → deviation, painting every
    emitted face by how far it sits from the scan in whole multiples of
    the tolerance.

    The distance is quantised rather than exact, because the structure
    that answers it is an occupancy grid one tolerance across — a face in
    band 2 lies between one and two tolerances out. That is the honest
    resolution of the evidence and also the resolution anyone reads off a
    colour map. Geometry with **no counterpart at all** gets its own
    colour rather than "very red": "there is nothing here to be off from"
    is a different statement from "this is off by a lot", and a reader
    who cannot tell them apart cannot act on either.

16. **Mesh hygiene** (`hygiene`) — what is wrong with a scan before
    anything is fitted to it. Every commercial workflow opens here; this
    one has managed without, and both halves of that are worth stating.
    It has managed because every stage was built robust to bad input:
    fits are median-trimmed, RANSAC peels rather than assumes, coverage
    is measured against the scan rather than believed. What no amount of
    robustness recovers is a defect that *removes evidence*, because a
    surface that was never measured cannot be fitted.

    So the first job is **measurement, not repair**: `info --health`
    counts degenerate and duplicate triangles, boundary loops and their
    sizes, non-manifold edges, and neighbours wound back-to-front. The
    repair pass then acts only on what the report found — dropping junk
    triangles and closing holes below a limit expressed in *boundary
    edges* rather than millimetres, because that is a statement about the
    hole and not the part: a gap a scanner leaves in a face is a handful
    of edges however large the part, while the open end of a tube is
    hundreds however small. An open rim is left open and reported, since
    closing it would invent a face the part does not have.

    **On both test scans it finds nothing.** The gear (500k triangles)
    and the pump (8.1M) are watertight, manifold, consistently wound and
    free of junk. That is the honest result for this stage — and it also
    disproves the guess that the topology pass's open edge ends come from
    scan holes. There are no scan holes; those ends are footprints
    thinning at feature boundaries, which is ours to fix, not the
    scanner's.

### Sewing: where it stops, and why

The loops are walked, orientated and tested (`sew::Shell`). A shell is a
solid only when every edge is used exactly twice in opposite directions,
so that is the test: sewn, free, disagreeing and branching edges are
counted, and the watertight fraction is their ratio.

**The gear's shell is 7.7% sewn, and the instrumentation says why.**
Every walked loop is two edges long — a pair between the same two
corners, enclosing nothing and orientable in no direction. Two fixes
that sounded right did not move it, and both are worth recording as
*not* the cause: orienting each face by its measured material normal
rather than its fitted one (a fit's normal sign comes from its
orientation hint, not from which side the solid is), and choosing the
next edge at each corner by angular order rather than greedily (a greedy
walk closes the shortest cycle available). Both are more correct and
both left the number where it was.

The measurement points somewhere else. A loop can only use an edge whose
**both** ends sit on corners, and only 275 of 426 ends resolve — the
other 151 are two-face clusters, where an edge dies mid-curve with no
third surface to pin a corner against, and tangent junctions where a
blend meets its face and no corner geometrically exists. Most edges
therefore cannot participate in a loop at all, so the walks have almost
nothing to walk and close on the only cycle available.

**Tangent boundaries** (`extract_tangent_boundaries`) are a second kind
of edge, found a second way. A fillet runs into the face it rounds with
matching normals, and that junction bounds both faces — a loop has to
walk it — but it is invisible to the intersection machinery, because two
tangent surfaces do not *cross*, they touch. There is no sign change to
find, and the crossing extractor deliberately skips the pair rather than
chase a root that is not there. Every blend on both parts therefore
contributed ends that could never resolve: the boundary existed
physically and nowhere in the model.

It is recovered the only way it can be, by **ownership** rather than
intersection: the scan gave each face its own patch, and the tangent
boundary is where one footprint stops and the other begins. That
evidence is weaker than a solved intersection, so the edge is marked
`tangent` — located to about a cell, and never able to carry a corner,
because three surfaces meeting with two of them tangent have no isolated
common point. The gear gains 11 such boundaries totalling 587 mm, a
quarter more curve, and three more faces close their boundary.

Sewing is not blocked on the sewing. It is blocked on corner coverage —
so that is what was fixed next, in two parts.

**A cluster of ends bordering only two faces is not a corner.** Every
edge in it separates the same pair, so what was found is one
intersection curve broken where the footprint had a gap. A corner needs
three surfaces, so those ends could never resolve and stayed open
forever — and they were the largest share of the unresolved ones.
`join_fragments` concatenates them into whole curves before any corner
is sought.

**An edge can also stop in the middle of another one.** End-to-end
clustering cannot see a T junction, because the other edge has no end
there to cluster with. Each still-open end now searches nearby edges for
the closest point on their *curve*, and where that edge borders a third
face, the three surfaces give a corner the Newton solve can land on
exactly.

Together: gear corners **62 → 112**, open ends **151 → 96**, and 49 edge
fragments joined into whole curves.

The orientation test needed correcting too, and the reason is worth
keeping. Signed area about the face's own normal is blind on a curved
face: a cylinder's boundary ring encloses its area about the *axis*,
while the face's normal there is radial, so the test reads zero and both
faces sharing the ring keep the same sense — an automatic disagreement.
A loop must instead keep the face's material on its left, and the
material's direction comes from the centroid of *all* that face's loops.
One loop cannot supply it: a cylinder's top ring is centred on the axis,
which is the very direction the test is blind to. Averaging over both
rings puts the reference at mid-height, where the axial direction the
loop needs finally has a sign. Gear watertightness 7.7% → **17.1%**.

### Decisions the operator can take back

The commercial packages put a human at three points: repairing the
segmentation, choosing the datum, and confirming dimensions. This
pipeline automates all three, which is the harder problem and the right
default — but an automatic decision that cannot be seen or overridden is
a worse deal than a manual one, so two of the three are now published
and overridable.

**The datum** is the decision every later stage is expressed in: band
extraction, patterns and the revolved profile all ask whether a surface
is "about the datum axis", so choosing differently changes what the
pipeline can recognize at all. The ranked candidates are printed with
the area backing each, the chosen one marked, and `--datum-candidate N`
takes another. That the flag is not cosmetic is easy to show: on the
gear, taking candidate 1 (570 mm² of support) instead of candidate 0
(11,121 mm²) drops classification from 99.9% to 90.3% — which is also
the evidence that the automatic choice was right.

**Canonicalization** already records every adjustment as a note
("diameter 47.022 snapped to 47.000 (-0.022)", "axis snapped to
(+0 +0 +1), was 0.051 deg off"), so nothing is changed silently.
`--snap-max MM` bounds how far a measured dimension may be moved, which
is a judgement about the part rather than about the algorithm: a casting
tolerates more than a ground bore.

**Segmentation repair** stays automatic for now; the picking viewer is
where it would belong.

`report::reverse_engineer` chains 3–5 and emits a JSON/text report.

## CLI

```
artificer-scan info    <mesh>
artificer-scan align   <source> <target> [--out aligned.stl]
artificer-scan reverse <mesh> [--tolerance MM] [--max-dihedral DEG]
                              [--min-faces N] [--no-snap] [--json out.json]
artificer-scan view    <mesh> [reverse options] [--out viewer.html]
artificer-scan snapshot <mesh> [reverse options] [--top] [--out snapshot.png]
artificer-scan sections <mesh> [reverse options] [--meridians N] [--levels N]
                               [--panel PX] [--gap MM] [--out sections.png]
artificer-scan simulate <mesh> [--density MM] [--smooth MM] [--noise MM]
                               [--dropout N] [--dropout-size MM] [--seed N]
                               [--out scan.stl] [--snapshot cmp.png]
artificer-scan demo    [--out scan.stl]
```

`simulate` is the scanner in reverse: an ideal mesh (CAD export,
synthetic part) goes in and the scan a real scanner would return comes
out — refined to sample density, creases rounded by the spot integral
(moving-least-squares over the spot radius), Gaussian noise along the
normals, dropout holes with ragged rims. Deterministic under `--seed`,
so a fixture is a command line rather than a file that can be lost —
which is also the answer to the two reference scans this pipeline was
tuned on no longer existing on any machine we have. On the demo part a
simulated scan drops sharp-edge segmentation to 4.6% and RANSAC peeling
recovers 89.2% — the same shape as a real scan's numbers, which is the
point: simulated fixtures exercise the pipeline's real path, with
ground truth attached.

**`scan-lab`** (`apps/scan-lab`) is the same simulator with knobs: load
a mesh (argument or drag-and-drop), drag sliders for density, spot
radius, noise, dropout and seed, and watch original and scan side by
side under one orbiting camera. The preview simulates a decimated copy
at interactive rates; **Save** runs the full mesh through the identical
deterministic path, so the saved STL is exactly what `simulate` prints
with the same options. Rendering is `scan-core::render` — the same
rasterizer the snapshots use, so the lab shows what CI would.

**Drilled holes are recognized and opened (2026-08-16).** A lone
cylinder is the commonest extrusion there is — a drilled hole — and the
pooled kinematic gate could never license one (a single cylinder's
normals satisfy the translation and the rotation reading alike), so
single-wall components now become extrusion instances on their own
evidence: a mostly-complete circumference (≥55% of 24 azimuth bins)
with real axial extent. The rebuild then *consumes* cut instances:
each inward-facing off-datum bore, with its coaxial chamfer cones,
forms a radius envelope, extended along its axis for as long as the
tube's interior holds no scan at all — void is the license to cut, and
a blind hole's floor is scan inside the tube that ends the extension
by itself. Geometry whose centroid lies inside the envelope is removed
unless clearly-interior scan sits coplanar with it (the blind-floor
guard). On the wheel-spacer fixture at σ=0.01 this opened all four lug
holes the revolved annulus had entombed and cut invention 5.6% → 0.6%.
Each recognized hole then *emits exactly*: the bore wall as a true
cylinder about its own axis and each chamfer as a true cone ring, tube
ends meeting their cones or snapping to the lid they pierce, while the
features so expressed skip the measured-patch floor — hole interiors
are crisp single surfaces instead of patch mosaics, and explained rose
90.4% → 98.8% on the fixture. The companion gate: a 45-degree cone
only proposes a *datum* chamfer ring when its apex sits on the datum
axis and its material fills the ring azimuthally — without that, every
lug-hole chamfer (a 45° cone merely parallel to Z) became a phantom
"chamfer ring" proposal at the hole circle's diameter, eight strong on
the spacer's feature tree.

**The pipeline measures its own noise (2026-08-17).** Every run
estimates the scan's noise sigma from the mesh itself — four hundred
small plane fits grown over adjacency, and the 25th percentile of
their residuals is the floor, because a flat patch's residual *is* the
noise while curvature only ever adds. The number prints on every run,
lands in the JSON as `noise_sigma`, and the blend discriminator scales
by it: its curvature windows widen by `√σ̂` (anchored so a quiet scan
behaves exactly as before), because noise curvature in a window falls
like 1/r² — at σ=0.07 the old fixed 1–3 mm windows read pure scatter
as "curves hard at every scale" and declared entire machined parts
organic. That was the recognition cliff in both ladder tables.

`rebuild --sew-triage triage.png` draws the watertightness work list:
the rebuild dimmed to a backdrop and every unresolved edge end marked
by *why* it is open — orange for two-face ends (a curve died in a
footprint gap), cyan for tangent boundaries (no corner exists to find),
magenta for singular triples, red for runaway roots. The same census
prints as text and lands in the rebuild notes. On the sewing-machine
base it settles the priority question in one image: 1,702 of 2,458
open ends are tangent boundaries — watertightness on moulded parts is
blocked on the blend layer, not on better corner solving.

`--z-from/--z-to/--z-step` walks one feature at a fixed increment instead
of sampling the whole part, and `--fixed-scale` gives every cut one
mapping so a sweep reads as a sequence — without it each panel is blown
up to its own extents and every slice looks the same size, which is an
easy way to misread a stack of them.

`rebuild` prints the two numbers that matter, both from `coverage`:
**explained** (scan area within tolerance of emitted geometry) and
**invented** (emitted area lying nowhere near the scan). Both are
necessary — a rebuild that sweeps a full annulus where the part has three
arms scores well on the first and is badly wrong. Note that neither is
the report's `classified` fraction, which counts a face as a success once
it belongs to *any* feature, including a catch-all: on the test pump that
read 99.4 percent while the rebuild explained 1.7 percent of the surface.

`sections` is the diagnostic for *completeness*. A shaded view of a
rebuilt part cannot tell you whether a wall exists — the hole is behind
something, or facing away, or a neighbour reads as the surface you are
looking for. So it cuts the scan and the rebuild with the same plane and
draws three panels per cut: the scan filled solid (scanline parity, which
is meaningful because the scan is closed), the rebuild's outline over a
ghost of that fill, and a **missing** panel where every run of the scan's
outline the rebuild does not account for is drawn in red, captioned with
the percentage. Meridian cuts make this sharpest — about the datum axis a
cylinder is a vertical line, so a missing one is unmistakable. It also
prints the same finding as text, clustering unmatched outline and naming
what each cluster implies ("flat annulus, 5.9 mm of outline at radius
39.26..44.90, z +13.47..+13.53"), because reading numbers beats reading
pixels.

`view` writes a self-contained WebGL viewer: original scan on the left, the
classified segmentation on the right, orbiting in lockstep. Display geometry
is decimated by vertex clustering; all reported numbers come from the
full-resolution mesh. `snapshot` renders the same side-by-side image to PNG
without a browser (built-in z-buffered rasterizer and dependency-free PNG
writer) — for chat, CI, and documentation.

13. **Finalize** (`finalize`) — the decomposition completes: freeform
    faces lying on a recognized surface join it face-by-face; faces
    within reach of *two* features become the round along their shared
    edge, grouped per feature pair as `EdgeRound` features with named
    adjacency ("round along the edge between plane z +17.3 and cylinder
    d 75.9"); implausible fits (a sphere centred outside the part)
    demote first; and whatever truly remains collapses into one residue
    record. Every face ends up owned by exactly one feature — areas tile
    the mesh exactly — and `--labels` exports the face-to-feature map as
    a little-endian u32 per triangle.

    Being bordered by two features is not evidence of being the round
    between them, so before a component is labelled it is **asked what it
    is** (`blend`). Taking the border at its word had made a third of the
    test pump — 43,243 mm², 2.6 million faces — into a single "edge
    round" with a 77 mm span and 22.6 mm of deviation. A round that wide
    is not a round; the rough cast surface of a casting is simply one
    connected sheet, and the sheet had been swallowed whole.

    Three things live in that bucket and each needs a different answer: a
    surface the region pass **missed**, a genuine **blend**, and **cast or
    organic** surface that has no analytic form and never will. Curvature
    at one scale cannot separate them — a scanner rounds every crease it
    measures, so close up every edge looks like a small fillet. What
    separates them is how curvature *behaves* as the measuring window
    widens: a blend of radius R answers 1/R at every scale up to R because
    it genuinely is that circle, while a crease answers a curvature that
    decays as the window grows, since the turn it measures is fixed and
    the window is not. So the discriminator fits the local quadratic form
    at five radii from 1 to 3 mm and reads the trend. Two details earned
    themselves: it must report the *larger* principal curvature and not
    the mean, or a band that curves one way and not the other reads at
    twice its true radius (a 3 mm band came back as 4.9 mm); and the
    readings must agree *with each other* across scales, because a window
    that only reaches an edge at its widest settings reports a curvature
    that climbs, whose slope looks flat while its readings disagree
    threefold.

    A sheet is too mixed to be any one thing, so a component that reads
    freeform gets taken apart. **RANSAC goes first**, scoped to the
    component alone: it already ran over the whole mesh, where a boss of
    a few hundred faces cannot outvote a housing and falls under the
    global support floor, but run inside the component it finds what the
    global pass could not afford to look for — 208 surfaces out of the
    pump's largest sheet in one call. The support floor deliberately does
    *not* scale with the component, or a small feature becomes invisible
    precisely because it sits in a large casting, which is the case this
    exists to catch. What stops a casting being shredded is not the floor
    but the requirement itself: rough skin cannot hold 400 connected
    faces flat to 0.2 mm.

    Only what RANSAC leaves is cut where it turns — at 15°, then 8°,
    then 4° — and each piece asked again. That ordering matters and was
    originally the other way round. Cutting where a surface turns only
    helps when it turns, and a cast housing curves smoothly through every
    angle: the cut lands nowhere in particular and shatters the sheet
    into thousands of shards each too small to name, too small to keep as
    a feature, and too small to be asked again. They collapse into the
    residue, so the stage reports one enormous freeform blob and looks
    like it failed to split anything at all — 23,840 mm² of it. Peeling
    first cut the pump's unnamed area from 28.9% to 18.2% and lifted the
    share of the model that is genuinely analytic from 76.8% to 86.4%.
    From the second level of splitting down a piece must still reach
    20 mm² to be named, or the same shattering returns as micro-planes
    that are each genuinely flat to tolerance and none of them a feature.

    Recovered surfaces are marked, because a fragment earned the right to
    carry its own measured area and nothing more. Left unmarked they fed
    the ring-pattern recognizer, which folded thousands of them into a
    120-fold pattern and invented 19,903 mm² of material that was never
    scanned — tripling the gear's invention in a single run.

    A component that votes blend but fits none of the four surfaces gets
    one more question: is it a **torus**? The construction is the
    definition — a blend is what a ball of radius r sweeps while touching
    both faces, so stepping inward from every measured point along its own
    normal by exactly r lands on the path the ball's centre took. Those
    centres collapse onto a circle, and that circle's axis, radius and r
    are the torus. Nothing is approximated and the axis is free, so a
    fillet rolled around a boss on the side of a casting is recovered as
    exactly as one about the datum. `RevolvedBlendFit` already described
    a general torus despite its name, so this needed no new surface type,
    only a carrier to draw it on.

    It fires on neither test part, and that is the honest result rather
    than a gap: a blend with a straight spine *is* a cylinder and has
    already been recovered as one, the gear's true fillets are modelled
    exactly by the revolved-arc path, and the pump's remaining rounds are
    cast — variable-radius and rough, so no constant-radius torus fits
    them to 0.2 mm. The capability is verified against a synthetic fillet
    on a tilted axis and waits for a part that has one.

    **Fragments are grouped when the report is printed, never demoted in
    the model.** Two thousand sub-25 mm² recoveries hold under five
    percent of the pump's area between them and bury the hundred features
    anyone wants to read, so the listing folds them into one line — 2,984
    entries become 846 and a summary. Demoting them for real was tried
    and reverted: it cost the gear a phantom 120-fold ring, 3.8 points of
    invention and seven times its triangle count. Which features exist
    and how they are printed are different questions, and only the second
    one was ever the problem.

13b. **Constrain** (`constrain`) — the relationships the part was built
    to, recovered and re-solved against.

    Every surface up to here is fitted on its own evidence, so two walls
    a machinist set parallel come back a degree apart and a reamed bore
    comes back at 42.003. Neither error is large and both are wrong in a
    way that matters: a model whose faces are only *nearly* square cannot
    be dimensioned, cannot be edited, and will not sew. What a designer
    specified is a small set of directions — a frame — with every face
    either along one or across it, and recovering that is worth more than
    any individual fit because it corrects every member at once.

    Directions that are square to one another — parallel *or*
    perpendicular, which are the same relationship as far as a frame is
    concerned — are grouped transitively, and each group is given the
    best right-handed frame by alternating between assigning directions
    to axes and re-averaging the axes over what was assigned, which
    settles in a few passes.

    **The constraint is offered, never imposed.** This pipeline has paid
    for that lesson: the gear's hub cones run a genuine 0.37 mm eccentric
    to its bore, and forcing them concentric inflated deviation from
    0.06 mm to 0.29 and lost 2,600 mm². So each surface is refitted with
    its direction locked and joins only if it still explains its own
    samples. On the gear 17 surfaces over 13,256 mm² accept a single
    frame, and **11 surfaces over 6,076 mm² refuse it** — the part is
    genuinely skew there, and the report says so rather than quietly
    squaring it.

    Squareness chains, and that bites twice. Membership of a group does
    not mean a surface was ever near a *particular* axis — A parallel to
    B, B square to C, and drift accumulates along the chain — so the gear
    first reported a surface turned 13.9 degrees to join its frame. A
    frame may only claim what was already square to it, so the correction
    is capped at the same tolerance that discovered the relationship.

    The second bite is worse: transitivity collapses a whole part into
    *one* group — all 524 of the pump's directions chained together — so
    one frame gets built and everything not near its three axes is simply
    lost. Two things follow, and both were needed:

    Frames are **peeled rather than assigned**: build the best frame the
    remaining directions support, let it claim what is already square to
    it, ask the rest again. Same idiom as the residue peeling, same
    reason. Anything offered a frame leaves the pool whether it accepted
    or refused, which is what makes the loop terminate.

    And a frame is refined **by its own inliers, not by its group**. A
    chained group is not a consensus, so averaging an axis over all 524
    directions drags it somewhere generic that fits none of them — the
    peeling then stalls, still finding one frame of six surfaces over
    473 mm² out of 131,700. Restricting the average to directions already
    within tolerance of an axis — exactly the distinction RANSAC draws
    between a candidate's support and the whole point set — took the pump
    to **71 frames**, the largest holding 54 surfaces over 27,637 mm² on
    axes that come out as the datum's to four decimal places, with the
    rest at the genuine angles of the pump's arms. The gear's primary
    frame went from a 1.426° worst correction on skewed axes to 0.209° on
    the true ones, and two further frames appeared behind it.

14. **Consolidate** (`consolidate`) — the advanced rungs. Merging is
    decided by description length (BIC), not thresholds: two features
    collapse exactly when the union's residual growth costs less than a
    second parameter set, with tolerance as a hard safety cap.
    Candidates come from the feature adjacency graph built off mesh
    edges — including pairs joined only through an edge round, whose
    seam dissolves into the merged surface. A joint solve then produces
    **shared parameter entities**: one axis solved over every coaxial
    cylinder's samples at once, one direction for the level planes, one
    radius per equal-radius group. The report's `stages` table records
    feature count and classified coverage after every stage, and
    `parameters` lists the shared entities — the measure of success is
    that the parameter list, not the feature list, is what shrinks.

14b. **Blend-chain unification** (`reconstruct::unify_blend_chains`) — a
    fillet sliced into concentric strips, put back together.

    A narrow band of a torus is fitted very well by a cone, so a fillet
    cut into rings is not a failure any *single* fit can detect: every
    strip is a genuinely good cone and both the region pass and RANSAC
    accept them, so no blend is ever proposed. The gear's rim came back
    as cones of half-angle 9.86, 9.44 and 9.24 degrees with their apexes
    marching up the axis — the signature of one curved surface cut into
    rings, the slope changing a little at a time while the apex slides.
    The fillet recognized beside them held 122 of the 1,132 mm² its own
    revolution covers; the other 89 percent was in the strips.

    So the judgement is made over a *chain*. In profile space a cylinder
    is a vertical segment, a cone a slanted one and a level plane a
    horizontal one; abutting segments that lie on one circle are one arc,
    and the arc is the fillet. Chains grow a strip at a time and are kept
    only while the arc still holds, because adjacency alone is transitive
    and would sweep every coaxial surface on the part into a single chain
    that fits no circle at all.

    Three guards, each from a failure: an arc must **turn** (20°) and
    must **stop turning** (190°) — a chain reporting 360° had swallowed
    eight of the dog ring's flat lands; and a strip may cover at most
    0.8 of the arc's own length, judged against the arc rather than the
    radius, which is what stops a plate and the boss standing on it
    chaining straight through their fillet and being rewritten as one.
    A chain whose circle agrees with an already-recognized partial fillet
    merges with it rather than either rejecting the other.

    Consuming strips has a consequence worth naming: those strips were
    *bounding* their neighbours in the corner solve, and once they are
    gone a face can run through where the round is. On the gear a plane
    at z +29.4 did exactly that and invented 491 mm². The tangency trim
    only fires where a face meets a fillet's **end**, so a face crossing
    the arc part-way along was never asked. Faces are therefore also
    clamped out of the round's own tube — no material can sit inside a
    rolling ball's path — which only ever shrinks a face, and only when
    a single edge is inside the tube.

15. **Round refinement** (`finalize::refine_rounds`) — circumferential
    edge rounds become parametric blend features: in profile space a
    revolved fillet is an arc and a revolved chamfer is a line, chosen
    by description length. Non-revolved rounds (a tooth edge) stay
    rounds. Each model is fitted twice — once over the whole bucket and
    once over the core its own median identifies — and the better fit
    wins, because an edge-round bucket is not a pure arc: the claiming
    pass hands it whatever lay within reach of two features, which at a
    corner where several rounds meet is a mixture. Fitting over all of it
    measures the mixture's spread rather than the round's, and the test
    gear's rim round reported 3.4 mm against a 0.15 mm tolerance until it
    was judged on the arc it actually has. Together with the topological
    residue resolution (unowned components read their identity off the
    mesh adjacency — a pocket bordered by one feature joins it, one
    bordered by two is their edge round), coverage reaches ~100 percent.
16. **Kinematic classification** (`kinematic`) — which motion sweeps a
    surface. The revolved path asks "is this a revolution about the datum
    axis?" and answers yes or no; that is a special case of a better
    question, because revolution, extrusion and helical sweep are one
    object with different motion parameters. A rigid velocity field
    `v(x) = c̄ + c × x` names the motion — `c = 0` is a translation,
    `c · c̄ = 0` a rotation, otherwise a helical sweep of pitch `c·c̄/c²` —
    and a surface normal, taken as a *line*, is a path normal of that
    motion exactly when `c · n̄ + c̄ · n = 0`. So fitting the motion means
    fitting a linear line complex to the normals, which reduces to a
    symmetric 3×3 eigenproblem and returns the classification and its
    axis, direction or pitch together.

    Draft falls out of the same fit. For a translation the normals lie on
    a great circle of the Gauss sphere when the walls are parallel to the
    sweep and on a small circle offset by `sin δ` when they lean; the mean
    of `n · direction` is that offset and the scatter about it is the
    fit's quality. Splitting the two matters — a drafted wall is a good
    extrusion with a non-zero mean, not a bad one — and draft is
    otherwise unmeasured anywhere in the pipeline.

    A translation is only *determined* when the normals span two
    dimensions. A single plane is swept by any translation lying in it,
    so the fit declines rather than returning whichever direction the
    arithmetic happened to produce.
17. **Axis-locked revolved refit** (`reconstruct::lock_revolved_surfaces`)
    — cylinders have always been re-fit with their axis locked to the
    datum; cones and spheres never were, and they need it more. A cone
    fitted freely over interleaved arcs of one taper has six parameters
    and too little azimuthal spread to pin them, so its axis tilts, and a
    tilt of under a degree throws a shallow cone's apex — hundreds of
    millimetres away — far off the datum axis. Locking only the axis
    *direction*, and letting its position float exactly as a cylinder's
    does, reduces the problem to four linear parameters in profile space,
    `rho = a + b·z + cx·cos θ + cy·sin θ`. The last two terms matter:
    real parts are not perfectly concentric, and the test gear's hub
    cones run a genuine 0.37 mm eccentric to its bore. It runs twice —
    after band extraction (never before, since that stage reads a tilted
    axis as its signal for what to dismantle) and again after
    consolidation, where a surface that fitted an absurd sphere while
    scattered across fragments reads as the plain cone it is.
18. **Coaxial-family unification** (`consolidate::unify_coaxial_families`)
    — rung 2.5. RANSAC and stitching can leave one interrupted surface
    of revolution as several azimuthal arcs whose per-arc fits drift
    apart (an 8.5, a 9.5 and a 10.5 degree cone tiling one taper); the
    arcs never share a mesh edge, so adjacency-driven consolidation
    cannot see the pair. Here the candidate screen is geometric —
    axis-true features whose (z, radius) bands overlap — and the union
    is judged axis-locked in profile space, with the usual MDL decision
    and tolerance cap.
19. **Secondary ring patterns** (`reconstruct::recognize_ring_patterns`)
    — interrupted revolved rings (a synchro dog-tooth ring, a
    castellated flange) betray themselves by solidity: measured area
    far below the full revolution's. Low-solidity rings cluster into
    z-bands, each band's repeat count comes from dual-signal azimuthal
    autocorrelation, and the band folds like the primary pattern —
    radially when `rho(theta, z)` is single-valued, axially onto a 2D
    sector height-field `z(theta, rho)` when the band is only
    single-valued seen from above (castellations: gap floors span many
    radii at one height). The fold claims the whole band box, so
    surfaces running beneath the gaps keep their outside portions and
    rebuild to their new extents.

    An axial band is still stored as the height field it was measured
    on, and rebuilt cell by cell. That is a known weakness — the ring
    comes out visibly rougher than the cleanly swept main toothing —
    and three replacements have been tried and rejected: lattice
    thinning with one azimuth set shared by every ring (rebuilds as
    fins, because a flank whose position shifts with radius cannot be
    described by one set of azimuths), per-level contour extraction
    (would terrace the roof — these teeth rise through two intermediate
    levels to a peak rather than being flat-topped prisms), and lofting
    per-ring simplified profiles with breakpoints merged pairwise. The
    last of those held coverage and cut the rebuild from 201k triangles
    to 74k, but looked worse, so the height field stands until something
    demonstrably beats it. Bands are attempted one at a time
    with membership re-detected after every fold, and a low-solidity
    ring lying inside an already-recognized pattern's band transfers
    into that pattern instead of becoming a band of its own.

    A band must also **fill its own annulus** — at least a third of it,
    measured on the top-facing material. A pattern is swept the whole way
    round without ever facing the solidity test that guards ordinary
    revolved surfaces, which makes a bad ring detection the most
    expensive mistake available: the test pump accepted a 120-fold ring
    across a band spanning radius 7 to 128 mm from 3,287 mm² of scattered
    material and swept 49,000 mm² of geometry that is not on the part,
    five sixths of everything that rebuild invented. Real rings clear the
    bar comfortably — the gear's toothing fills 2.4 times its annulus and
    its dog ring 1.4 times, against 0.06 for the phantom.
20. **Rebuild** (`rebuild`) — the idealized model. Every revolved surface
    extends to its exact intersection with its neighbours, and the
    toothing regenerates as the master profile swept helically
    — phase-exact, using the fold's own azimuth convention and z
    reference, so the rebuilt pattern lands at the scanned azimuths
    (verified to 0.003 degrees on the test gear's dog ring). Axial ring
    patterns regenerate from the folded sector height-field: empty
    cells fill from their neighbours, corners average so steps render
    as near-vertical quads, and rims drop walls to the base face.
    **Recognized revolved fillets are modelled**, not dropped: each is
    emitted as the arc it is in profile space and its neighbours are
    trimmed back to tangency, so the model carries the round instead of
    the sharp corner underneath it. The arc's angular span is measured
    from the scan — and only from the points that actually lie on the
    fitted circle, because the finalize pass claims on-surface faces onto
    a blend after it was fitted. A round whose radius is within a couple
    of noise widths is left sharp, since its "circle" is fitted to
    scatter. A quarter turn is the usual fillet and a half turn a rim
    bullnose; past that the fit has wrapped its own circle and is
    rejected. Rounds that are not surfaces of revolution — a tooth edge
    follows the toothing — stay callouts.

    Interrupted-ring candidates get three honest verdicts: a family of
    co-annular arcs jointly covering most of a revolution emits as one
    surface (joint locked fit, noted); an isolated ring whose gaps hold
    no other primary surface is a scan hole and emits full (noted);
    a ring whose gaps are genuinely occupied (a pattern band, the bore
    behind a sliver) is skipped with its solidity, because a full
    revolution would entomb real geometry. "Occupied" means material
    lying *off* the candidate surface — what usually sits in an
    interrupted surface's gaps is another arc of that same surface, and
    counting those made co-annular arcs veto each other so that neither
    was ever emitted.

    Anything the revolved path cannot express is emitted as a **measured
    trimmed patch** instead of being skipped. Solidity is a property of
    the *face*, not the surface: in every B-rep — Parasolid, ACIS, STEP,
    and this project's own kernel — a face is an unbounded carrier plus
    trimming loops, so a face covering two percent of a cylinder's
    parametric domain is an ordinary sliver rather than a degenerate
    revolve. The patch rasterizes the feature's own faces into the
    carrier's parameter domain (arc length against axial distance, both
    in millimetres so cells stay square) and emits a quad per occupied
    cell, evaluated exactly on the fitted surface. It needs no neighbour
    to intersect against, so it cannot fail, and it works at any
    orientation rather than only about the datum axis — which is what it
    is for, since on the test pump 47 percent of the area was already
    correctly fitted and merely inexpressible. The revolved path keeps
    priority where it applies, because it produces sharp, mutually
    intersected geometry and a measured trim does not.

    Before any patch is emitted, adjacent planar faces are **grown to the
    line where their planes meet**, so two flats join at a sharp edge
    rather than leaving a fillet's width of gap. The line is exact — two
    planes meet in a straight line and nothing is approximated — but the
    growth has to be bounded twice, because the line itself is infinite:
    a face may only reach across a gap the size of an edge break (1.2 mm),
    and only where the neighbour still has measured material within
    0.6 mm of the line. With just the first bound the pump reached for
    every non-parallel plane in the model and invented a quarter of its
    own surface.

    The same line taken from the other side **trims**, and it needs no
    intersection curve written out for any pair: every carrier answers
    `probe` with a signed distance, so a cylinder cutting a plane, a cone
    cutting a cylinder and two planes meeting are one test on that sign.
    Tangent neighbours — a blend and the face it runs into — do not cut
    each other. A footprint is
    rasterized over whatever its own measured faces covered, and
    measurement does not stop politely at an edge: a face keeps a fringe
    of cells belonging to the round beyond it or to the neighbour itself,
    and the patches interpenetrate. Which side to cut is read from the
    evidence — a face's own cells sit predominantly on one side of its
    neighbour's plane and that side is its material — so nothing has to
    know whether the solid is inside or outside. A face that straddles
    its neighbour has no side and is left alone rather than guessed at.

    **Coverage cannot see this at all**, and that is the point worth
    recording: the fringe is measured surface, so it counts as explained
    and never as invented, while being exactly what makes the model read
    as a pile of overlapping sheets instead of a solid. The trimmed area
    is therefore reported directly as its own number.

    Once the faces stop in the right place, the curve they stop **on** is
    the model's topology, and it comes out of the same field: the
    neighbour's signed distance is zero exactly on the intersection, so
    only its sign change has to be located. Where two neighbouring cells
    disagree, the crossing interpolates between their centres and lands
    on the curve to within a fraction of a cell — no intersection has to
    be derived per surface pair, and none of the special cases (a circle
    where a cylinder meets a perpendicular plane, an ellipse where it
    meets a slanted one, a line between two planes) needs its own code.

    Footprints are **grown before the edges are read, and only for
    reading them**. A footprint is rasterized from the faces the scan
    gave this feature, and near a physical edge the scan gives them to
    somebody else — the round takes a strip, the neighbour takes another
    — so it ends a millimetre inside the boundary it should reach. The
    extractor looks for a sign change in the neighbour's signed distance,
    and a footprint that stops short has every cell on one side: the
    crossing does not exist to be found, and the edge dies there. That
    was the whole cause of the unresolved ends, and the scans themselves
    are watertight, so the gap was this pipeline's own.

    Growing a copy doubled the recovered topology. Growing what is
    *drawn* also laid 2,032 mm² of material exactly where the physical
    round is — invention buying nothing the edge does not already give —
    so the geometry is emitted from the un-grown footprints and only the
    probe is grown. Gear edges went 142 → 233 and corners 28 → 62 with
    coverage and invention **unchanged to the decimal**.

    Bounding falls out for free. The field exists only where the face has
    material, so the curve is already clipped to the face: an infinite
    plane-plane line never appears, only the piece the part actually has.
    Crossings arrive in cell order, which is no order, and are chained
    nearest-to-nearest from the end furthest from their centroid; a gap
    wider than three cells ends the curve rather than leaping across it.

    One pair of surfaces can share several separate curves — a plane
    cutting clean through a cylinder meets it in two parallel lines — and
    chaining once per pair kept the first run and silently threw the rest
    away; on the gear the discarded runs were half the total curve.
    Chaining now continues until the crossings are exhausted, one edge
    per run.

    **Corners are solved, not estimated** (`sew`). The point where three
    surfaces meet is the root of three signed-distance equations, and
    Newton's method on the carriers' own `probe` gradients lands on it to
    machine precision — one step for planes, a handful for curved
    carriers, and a singular system for degenerate triples, which is the
    correct answer for surfaces that meet in no single point. The
    research pipelines mesh their surfaces and intersect triangles
    precisely because freeform patches make this calculation unstable; a
    kernel that forbids freeform surfaces gets the exact corner almost
    for free. The seed comes from evidence — edge ends finishing near one
    another nominate a corner between the faces they collectively border
    — and a root that runs away from its seed is rejected as the phantom
    it is, since three surfaces extended far enough always meet
    *somewhere*, and somewhere is not good enough.

    Ends adopted by a corner have the exact point appended, so drawn
    curves reach the corner rather than stopping a cell short. Loops then
    fall out by walking: edges whose both ends sit on corners become arcs
    between them, and a walk around one face that returns to its starting
    corner is a closed boundary. Rings — edges that close on themselves,
    a bore's rim — are loops already and skip the walk. The summary
    admits failure by construction: it counts open ends and unbounded
    faces alongside the loops (gear: 28 corners, 45 closed loops, 21 of
    35 edged faces bounded; pump: 2,059 corners, 3,556 closed loops, 584
    of 687 bounded).

    The walk is over **pairs, not faces**. An edge is one curve shared by
    two faces, so reading it from each side gives two chains of the same
    thing — and worse, each side dies wherever *its own* footprint has a
    hole, which returned the gear's edges as forty disconnected fragments
    totalling 215 mm. Pooling the crossings from both footprints closes
    the holes only one of them has (the pooled points are thinned to one
    per half-cell, or the chain zigzags between near-duplicates), and the
    pair yields a single curve: **54 curves totalling 490 mm**, including
    a complete closed circle where a cylinder meets a plane.
    `RebuiltModel::edges` carries them and `--edges out.obj` writes them
    as polylines.

    Kept, but it has not yet earned its keep: at a defensible reach the
    gear is unchanged and the pump gains 0.2 points of coverage for 0.6
    of invention. The gaps between flats are simply not where the missing
    area is — and extended material lands exactly where the scan has a
    round, so a proximity metric scores a sharp edge as invention by
    construction. This pass pays off once the blend layer exists and the
    extension can terminate on a real tangent line.

    What has **no analytic form** is emitted as what it is: the measured
    surface itself, decimated to the working tolerance and marked so no
    reader mistakes it for something the kernel can certify. ADR 0026
    rules out splines, and it is right to — but the alternative to a
    spline is not a hole. A casting's rough surface is genuinely not
    analytic, and a scan-to-CAD model that silently omits a third of the
    part is worse than a hybrid one that says which parts are exact. So
    the coverage figure is reported split: on the test pump the model
    explains 95.0% of the scan, **of which analytic surfaces account for
    86.4%** and the remainder is measured. Pooling the two would flatter
    the result and hide exactly the number worth improving.

    A fillet is swept the whole way round, and until recently it faced
    **none of the solidity test** that guards every other revolved
    surface — the same hole that once let a pattern invent 49,000 mm².
    Its *tube* extent was measured honestly while its azimuth was assumed
    complete, so the gear's rim bullnose, measured over 122 mm² of a
    revolution covering 1,132 mm², was drawn as a whole ring: eleven
    percent of a fillet, emitted as all of one. It now answers the same
    70% threshold as everything else, and an interrupted fillet falls
    through to be drawn as a measured patch on the exact torus it was
    fitted to — which is what the torus carrier above is for. Gear
    invention 14.7% → 13.5%.

    Invention is reported **by location, not just by feature**
    (`coverage::invented_patches`). A total is a score; a location is a
    work list. Grouping invented faces by the feature that emitted them
    and the height they sit at turns "this model invents 5,617 mm²" into
    "965 mm² of sloped band material at radius 30.41..40.53, z +14.58" —
    which named all three of the gear's remaining causes in a single run,
    the same way the missing-side report once found its bare end-face
    ring after days of renders had not.

    The stage **accounts for every feature**: an analytic surface above
    100 mm² must emit geometry, be explicitly covered by another
    feature's emission, or appear in the skip list with a reason, and a
    final sweep reports anything that fell through in silence. A feature
    that vanishes without comment is a hole in the model that nothing
    reports, and that accounting is what surfaced the defects above.

    `rebuild --out model.stl --snapshot cmp.png` writes the sharp STL
    and a scan-versus-rebuild comparison image.

Import formats: STL (binary/ascii), PLY (ascii/binary-LE), OBJ.

## Known limits / next milestones
- Fillet/blend recognition (small-radius cylinders adjacent to two planes)
  and boundary-line extraction feed feature reconstruction in the kernel.
- Feature export into the parametric history (extrude/revolve candidates
  from plane+cylinder families).

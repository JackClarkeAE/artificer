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
5. **Fragment merge** (`merge`) — RANSAC extracts connected components, so
   one physical surface interrupted by keyways or slots returns as several
   fragments. Compatible fragments (coaxial cylinders, coplanar planes,
   concentric spheres) accrete greedily, and each merge is accepted only
   when a least-squares refit over the union of faces still meets
   tolerance — a genuine 46.8 mm step never swallows a 47.3 mm relief.
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
9. **Reconstruct** (`reconstruct`) — level planes and on-axis cylinders
   assemble into a revolved profile: a stack of annulus segments (bores
   from inward-facing walls, bosses from outward), with fillet and chamfer
   proposals attached to the profile corners they round. `--history`
   emits the plan as replay operations: `make_revolved_annulus` entries
   are wire-exact for the Artificer protocol's `KernelCommand`;
   `finish_edge_proposal` entries carry geometric edge descriptors until
   an executing kernel can resolve entity references. Where the true
   boundary is not a surface of revolution (gear teeth), the plan says so
   in a note instead of inventing geometry.

`report::reverse_engineer` chains 3–5 and emits a JSON/text report.

## CLI

```
artificer-scan info    <mesh>
artificer-scan align   <source> <target> [--out aligned.stl]
artificer-scan reverse <mesh> [--tolerance MM] [--max-dihedral DEG]
                              [--min-faces N] [--no-snap] [--json out.json]
artificer-scan view    <mesh> [reverse options] [--out viewer.html]
artificer-scan demo    [--out scan.stl]
```

`view` writes a self-contained WebGL viewer: original scan on the left, the
classified segmentation on the right, orbiting in lockstep. Display geometry
is decimated by vertex clustering; all reported numbers come from the
full-resolution mesh.

## Known limits / next milestones
- Fillet/blend recognition (small-radius cylinders adjacent to two planes)
  and boundary-line extraction feed feature reconstruction in the kernel.
- Feature export into the parametric history (extrude/revolve candidates
  from plane+cylinder families).

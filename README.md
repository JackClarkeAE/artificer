<div align="center">

# Artificer

**An exact boundary-representation geometry kernel, and the parametric CAD workbench built on it. Pure Rust, from scratch.**

[![CI](https://github.com/JackClarkeAE/artificer/actions/workflows/ci.yml/badge.svg)](https://github.com/JackClarkeAE/artificer/actions)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![Commercial licence available](https://img.shields.io/badge/License-Commercial-purple.svg)](#licensing)
[![Rust: 1.95+](https://img.shields.io/badge/Rust-1.95+-orange.svg)](https://www.rust-lang.org)

[The Kernel](#the-artificer-kernel) •
[Built for AI-driven CAD](#built-for-ai-driven-cad) •
[The Workbench](#the-artificer-workbench) •
[Quickstart](#quickstart) •
[Architecture](#architecture) •
[Roadmap](#roadmap) •
[Licensing](#licensing)

<br/>

![Artificer Workbench](apps/workbench/tests/snapshots/workbench_two_visible_bodies.png)

</div>

---

## What is new since 0.99.7

- **A construction plane is a feature you can place, move and edit.** The Plane command puts a plane on whatever is picked — a face, two parallel faces, a straight edge, an origin or construction plane — and draws it there before anything is committed. An arrow drags it off its base and, on a plane through an edge, an arc turns it about the edge; the card on the right carries the same Offset and Angle as typed fields, and a Flip. Confirmed, it is a chip in the history like any other feature: right-click to edit it where it was made, rename it, suppress it, or delete it when nothing is built on it. It is linked, not copied: a plane on a face follows the face when the body under it is rebuilt, a sketch on a plane moves with the plane and so does everything extruded from it, and a side of an extrusion can end at a plane and follows it too. Planes from older files open where they were. ADR 0048 records the design.
- **Loft.** The Loft command builds a solid through profiles drawn on different planes — a square on the ground up to a circle on a plane above it is the first one anyone tries. Click a profile in each sketch, in the order the loft runs; the card lists the sections, lets you reorder or drop them, and chooses New body, Add or Cut, and the viewport shows the solid the kernel builds before you confirm, or the card says by name why there is none. Walls are exact: a plane, cylinder or cone where one fits, a ruled surface between two sections otherwise (ADR 0049), and through three sections or more one smooth B-spline surface per wall that carries on through the middle sections without a crease (ADR 0050). The loft is a history feature that follows its sketches and their planes — move the plane and the loft moves with it — and its chip reopens it in its editor. ADR 0051 records the design.
- **Revolve, Axis and Sweep.** Revolve is now a history feature. It turns a sketch's profile about a centreline, the sketch's own axes, an origin axis, or a new construction axis. It can make a full turn or turn through an angle typed in, or written over a variable, one way, the other way or both ways. It builds a new body, adds to one or cuts from one. The Axis command places a construction axis along a straight edge, through a curved face's centre, or where two planes meet, and it follows them when the model rebuilds. Sweep carries a sketch's profile along a path drawn in another sketch: lines, arcs and splines joined end to end and tangent where they meet. By default the profile turns with the path, twisting as little as possible, or it can keep the orientation it was drawn in. A straight path gives an exact solid, and so does a single arc whose axis lies in the profile's plane. Any other path is skinned through copies of the profile, added where the skin strays, until it lies within 0.00001 mm of the true sweep; the result states the gap it met. Each feature has a chip that reopens its editor. ADR 0055 records the design.
- **Spline sketch curves.** The Line chooser now offers a fit-point spline, which passes through every point you click, and a control-vertex spline, which is pulled toward them. Double-click, Enter or Finish spline ends one; clicking the first point closes it into a smooth loop with no corner where it started. A region a spline bounds extrudes into an exact solid whose walls are B-spline surfaces, and splines can be loft sections too. B-spline curves and surfaces are now carriers of the kernel in their own right, checked to the same standard as every other surface and written to STEP exactly (ADR 0050).
- **Scan to CAD fits spline surfaces.** The scan add-on now fits trimmed B-spline patches where no plane, cylinder, cone, sphere or torus describes the scan, and then refines the fit — re-projecting every sample onto the new surface and solving again, and adding knots only where the residual is still above the scanner's noise. On a synthetic freeform block with a known top, the rebuilt surface lands within 0.0055 mm RMS of the truth, down from 0.149 mm when the top was carved into forty-odd flat facets. An analytic surface that fits always wins, and patches are written to STEP as B-spline faces (not yet sewn into a closed solid).

## What is new in 0.99.7

A correctness fix for 0.99.6. Two decisions inside the Boolean engine turned
on arithmetic the platform gets to choose rather than on the shapes
themselves, so the same model could cut on one machine and be refused on
another — which is what happened: the Windows build refused corners the Linux
build cut.

- **A finish standing apart now cuts at every size, on every platform.** Whether two bands *touch* was decided by how near a sampled point came to the other surface. Where those samples fall is fixed, but how near they come depends on the last bits of a sine — and the maths library that supplies it differs between Windows and Linux. The touch is now located exactly, by refining to it, and only then measured. Likewise, two corners meeting at a point had to match to the last bit to count as the same corner; they now count when they agree to within the document's own precision, which is what that setting means and what the later stages already assume.
- **Found by sweeping, not by example.** One fillet at one radius passed. The same cut swept across forty radii refused twelve of them, scattered, with nothing about the shapes separating the twelve from the rest. Scattered failures across a smooth range are the fingerprint of a decision resting on arithmetic instead of geometry. That sweep — eighty corners, two bands and three, checked against the closed form — is now part of the test suite, because no single example can show it.

## What is new in 0.99.6

The Boolean engine used to draw its domain around crossings that pass clean
through, and fail closed on everything else. Two of those "everything else"
cases turn out to be things you do constantly, and both are answered now.

- **A finish standing apart works against a corner that was already rounded.** Not just against a flat one. Take two edges of a corner as a joined blend, then round the third beside them rather than into them, and it cuts — landing exactly on the union of three quarter-round prisms, whichever order the three are taken in. What made this possible is that a *tangency* is now imprinted like any other crossing rather than refused. A band touches the walls it rolls between; that is what makes it a band, not an accident. The pipeline only ever needed each piece of boundary to be wholly inside or wholly outside the other shape, and at a touch it is — the two meet and part without either passing through.
- **Two solids that meet on a whole face union into one solid.** Previously a shared face was a coincident boundary and the engine declined to guess which side the material was on. It now reads that from which way each boundary runs — material lies to the left of an oriented loop, so two stretches either run together or against each other, and that settles it. Two boxes sharing a face become one box, with the shared face gone from the result.
- **Patterned copies that overlap merge**, instead of refusing. A box stepped along its own length is now one continuous run of material.
- **An interference study can say how much two parts overlap** when they share a face plane, rather than only that they interfere.
- **Contacts of no width still refuse, deliberately.** A cylinder touching a plate along a single line would weld two solids at a seam with no thickness, which is not a solid at all. This release is about contacts that bound material, not about admitting results that cannot be manufactured.

## What is new in 0.99.5

A release about one word in the Boolean engine: tangency. A fillet's band
touches the walls it rolls between rather than crossing them, and that touch
is now something the engine can cut rather than something it refuses.

- **A finish can stand apart from a corner as a fillet, not only as a chamfer.** The regularized Boolean underneath used to refuse every tangential contact, on the grounds that it classifies each piece of a boundary by an interior sample and a tangency is not a transverse crossing. But the invariant that pipeline actually needs is only that no piece *crosses* the other operand's boundary, and at a tangency none does: the boundaries touch and part again. A tangency is now imprinted like any other crossing, and which side each piece lies on stays a question for the sample, which is what the classifier is for. Boundaries that share a stretch rather than a point are a different question and still fail closed. The result is exact rather than close: a cube of side `L` with one edge rounded by `r` measures `L³ − r²(1 − π/4)L` to the last digit the closed form carries.
- **A tangency is only a tangency if it is exact.** Taking the band's contact with each wall as `r/tan(θ/2)` along the face lands a bit or two off the true foot, and a flank plane four ten-thousandths of a millionth of a millionth outside the band does not graze it — it misses, and every stage after that is entitled to believe the miss. The contacts are now taken as the feet of the perpendiculars from the band's own axis, where they are exact by construction. This was the whole difference between a fillet that cuts and one that is refused.
- **The cut goes through the engine's front door.** Standing a finish apart is a prism against a prism, which is the reduction that carries it; the code had been reaching past the dispatch into the general engine, which takes a route that refuses the shape.
- **What is still refused says so by name.** A finish standing apart from a *band* — a corner an earlier feature rounded, or a second edge running across the first — has no single axis that reduces it to prisms, so the general engine has to answer it and carries no tangency of its own. ADR 0044 records the reasoning and what that route would need.

## What is new in 0.99.4

A release about corners: the ones a finish leaves behind, and what happens
when you come back for the edge you did not take the first time.

- **Two edges of a corner round and bevel in one feature.** Rounding across the top of a part and down the side it meets is the selection a user makes without thinking, and it was refused — the reason given being that the vocabulary could not fade a band out along the third edge. It never had to. The two bands stop against each other along one seam, and the edge left sharp simply starts a blend's width further along. A chamfer's bevel planes meet in a line, a fillet's two equal crossing cylinders in a planar ellipse. Both volumes were written down and checked against their integrals before the construction that had to hit them existed, and both oracles now run. ADR 0043 has the derivation.
- **A corner that is already finished asks instead of refusing.** Come back for a third edge at a corner two others already shaped and the panel now puts two options above the size. **Join this fillet or chamfer to the others** adds the edge to the feature that owns the corner and replays it, so one patch closes all three and the body is exactly the one the edges chosen together would have made — the tree grows a feature rather than gaining one, and the corner keeps that feature's size, because a rolling-ball corner is a single sphere. **Keep independent** builds the new band beside the others, meeting along a seam with the corner's own point surviving, as though each edge had been finished on its own body. Standing apart is cut for a chamfer; for a fillet the option shows closed with its reason, the band being tangent to the walls it rolls between in a way this release's Boolean will not take. ADR 0044 records both answers and the gap.
- **A bore's bottom rim rounds again.** Two independent faults in the mirrored profile — a reversed sweep and a mirror line that could fall inside the profile it was reflecting — each produced the same p-curve locus error, so fixing one left the other. Both are fixed, and a rim now rounds the same whichever way its loop is wound.
- **Selecting two bores at once says so.** A selection mixing a round rim with a straight edge, or holding two rims, used to stage a preview and then hand over a refusal that named neither the cause nor the remedy. Both now say what they are and how to split them.
- **The edge-finish preview follows the edge it is previewing.** Bands that meet at a shared point now agree on the basis their surfaces are built on, so the preview stops showing a crease where the committed body has none.
- **A sketch can measure against the body it sits on.** A dimension may now be taken from a point to an edge of the face the sketch was drawn on, and the ordinate's datum may be a side a recipe owns. Where a region cannot be formed the status line says which of the reasons it was rather than going quiet.
- **No terminal behind the window.** A release build no longer opens a console it never writes to. Debug builds keep theirs, that being where panics and backend chatter go.

## What is new in 0.99.3

A release about how tools and selections meet, and one picking bug that had
made edges unreachable at some camera angles.

- **A tool can be pressed before its operands are picked.** Pressing Fillet with nothing selected used to be impossible: the button was dead until you had guessed it wanted edges, and the tooltip told you so — a door that tells you where the key is instead of opening. It now enters Fillet and asks for edges. Picking the edges first and pressing it still stages immediately, because the two orders are one code path whose only difference is whether what was already selected was enough. The same is true of Chamfer, Hole, Rib and Hole pattern. A command is now disabled only for reasons that picking something cannot fix — a staged operation, a history marker away from the tip, immutable library geometry, or a workspace with nothing to act on. ADR 0041 records the design and what it does not yet do.
- **A click picks what it highlights.** Every visible face carries an invisible hit box at its label position so assistive technology can name it, and that box won the hit test against the canvas — so wherever one sat, the edge under the pointer was neither highlighted nor clickable, and the face answered the click instead. Label positions move with the camera, which is why it came and went with the angle. Hover is now read from the pointer rather than from that contest, and a box stops sensing clicks while a vertex or edge is highlighted under it. Naming a face still selects it, and right-clicking for its context menu is unchanged.
- **A picked face knows which occurrence it belongs to.** There were two representations of every pick, kept in step by hand, and they drifted apart in both directions: a push-pull remapped one and left the other holding the reference from before the operation. There is now one, and a face carries the body instance it came from rather than a bare entity reference that could not say which of two occurrences was meant.

## What is new in 0.99.1

A point release about saying what you mean: reaching a relation in one click
instead of three, dimensioning to an edge rather than only to its corners, and
telling one part from another by colouring it.

- **Every relation is a button, not a menu behind one.** The relations were one tile with a chooser behind it, so picking perpendicular meant clicking a tile, reading a menu and clicking again — for a tool you reach for knowing exactly which one you want. All eleven of them and the dimension tool now stand in a grid of their own beside the drawing tools, each its own button, each armed in one click. They are glyphs rather than labelled tiles, which is what lets twelve fit where one tile and its dropdown stood; every one carries its name in its tooltip and its accessible name. The supported minimum window grows from 1040 to 1120 wide to make room for them.
- **A dimension can measure to an edge, to its middle, or to another edge.** A distance between two points is a radius: it leaves the point it locates anywhere on a circle, so two of them meet in two places or in none, and neither is what a drawing means by "twenty from that edge". Three relations say it properly — an offset from an edge's line, a distance to an edge's midpoint, and the separation of two parallel edges. Two offsets from two edges land a point in exactly one place, which is the ordinate a drawing is mostly made of. The offset is measured from the edge's line rather than its segment, it keeps the side the point is already on, and retyping it moves the thing being located while leaving the edge it is measured from where it is. Edges that are not parallel are refused by name rather than answered with one of the many numbers their distance could be. ADR 0038's amendment records the design.
- **A part can be given a colour, and the colour travels.** Materials already carried one, which told steel from brass and could not tell this bracket from that one — two parts of the same aluminium shaded identically is exactly the case an assembly needs colour for. A body now has a colour of its own, set from the RGB picker in the Assembly tab's COLOUR group, and it outranks the one its material implies; clearing it falls back to the material rather than to nothing. It is saved with the document and it leaves in a STEP export as the presentation style AP214 has for it, which is the chain other CAD reads. A body with no colour writes no style at all, so the receiving system keeps its own default rather than being told a colour nobody chose. ADR 0040 records the design.

## What is new in 0.99

A kernel release. Two bores that cross used to leave the exact domain and come
back as a tessellation, and now they do not. A wall the Boolean cut into a fan
of panels is one wall again. And two things that were wired but never finished
— ending an extrusion at a face, and dimensioning between two points — do what
they always said they would.

- **Two bores that cross are exact.** Drilling a second hole across the first used to take the whole body out of the exact domain: it was rebuilt from a tessellation, published a warning saying so, and quoted a volume about a tenth of a percent wrong. Two cylinders of the same radius whose axes cross meet in two ellipses rather than a curve nothing here could name — the classic Steinmetz solid, and the derivation is short enough that ADR 0025 states it — so the seam is now two plane sections of a kind the kernel already had. The crossed block is ten faces and one shell, and its volume is the closed form to the last digit, where the approximation reached the same body as 2,959 faces. Bores of *unequal* radius, or on axes that miss one another, really are a quartic; those still take the approximate route and still say so.
- **A wall the Boolean cut into a fan of panels is one wall again.** Every cutter plane through a face split it, and nothing put the pieces back. Flat walls arrived as fans of creases across geometry that has none, and every stage downstream — validation, the drawn scene, hit testing, history replay — is a function of face count. Facets on one plane are now dissolved back into one face, and a corner is dropped only where every facet agrees it is not one. The merge stops wherever a single face could not state the result, a ring around a bore being the common case, and leaves those pieces exactly as they were; the worst case is the fan it started with. ADR 0039 records it, with what it is worth measured rather than estimated.
- **An extrusion ends at the face you stop it at, and you pick it from a list.** To face shipped in 0.98.2 and in the running application still did nothing. The deepest reason was that destinations came from the picture: the viewport culls the facets turned away from the camera, so the faces it offers a click are the near side of the material, and the face a through cut ends at is the far side. The prompt was asking for a click on something that was not there. Discovery now walks the solid instead, keeping every face this side can reach and grouping them by the plane they lie on — because a plane is what a side actually stops at, and one tessellated wall is hundreds of faces on one plane. The panel lists them with the length each would sweep and the kernel's own name for the face, and nothing in it reads the camera. ADR 0037 records where a side of an extrusion ends.
- **A dimension between two points can be seen and changed.** Asking for a dimension between two sketch objects got nothing back, and every piece of it was already in the tree: the solver had held a distance between two arbitrary points since the sketch crate was written, and the relation tool's own tooltip said the dimension tool would edit it afterwards. Nothing drew it and nothing could edit it — the dimension tool resolved a pick to a recipe's number, and a distance between two points is not a number any recipe carries. It is drawn now, with its witness lines and arrowheads, and typing a new value moves the points. ADR 0038 records the design.
- **A refused operation owes you the body you already had.** A construction the kernel cannot certify is refused rather than published, and that was true by construction and untested — which is how such guarantees quietly stop being true. It is now pinned by tests from both ends: nothing invalid reaches the document, and the snapshot you already had keeps its identity, its volume and its validity, because a snapshot is an immutable value. A sketch also never stores a relation saying one thing while its geometry measures another.

## What is new in 0.98.2

A point release about the sketch tools, and about changing your mind: trim
that works on the edge you drew over, a dimension that answers the question
you asked, an extrusion that reaches the face you click, and an extrusion you
can reopen and change after the fact.

- **An extrusion can be reopened in the editor it was made in.** Right-click it in the parametric history and choose Edit. The model rolls back to the body the feature was built on, its sketch comes back with the regions it swept still picked, and the editor opens on the lengths, sides and operation it was given, with the same preview and the same drag handle. Confirming rewrites that feature's own recipe and replays everything after it rather than leaving a second extrusion beside the first; abandoning it puts the whole model back. The properties card covers extrusions too now, which is the one feature it never did — every extrusion the workbench makes is a sketch-region recipe and the card had no arm for one, so it never appeared and a typed change had nowhere to land. ADR 0036 records the design.
- **Two strokes drawn onto one another stay joined when either moves.** A corner where two lines met was only a coincidence of coordinates: drag either line and it came apart. An endpoint that lands on another endpoint now persists a relation, and an edit outranks that relation — the solver holds the points the edit authored exactly where it put them and carries the partner the whole way onto them, rather than splitting the difference and letting go of the pointer. A drag and a retyped length or angle behave the same. One preference in SNAPPING AND VIEW governs it, on by default. ADR 0035 records the design and names what it does not do.
- **Trim works on an edge a wedge is drawn over.** Two faults, both reachable from one drawing and both silent. A limit lying along the target aborted the whole trim; where the overlap starts and ends are perfectly good bounds and are used as such now, so a wedge whose base runs along a rectangle's edge no longer makes that edge untrimmable. And a span covering the whole target retained nothing, so the staging layer dropped the edit for want of an identity; the retired entity is that identity, and trimming an edge with nothing bounding it inside takes the edge.
- **Dimensioning an edge asks about that edge.** Clicking one side of a rectangle brought the whole recipe back up, burying the answer among the shape's other numbers and drawing a bare leader with no arrowheads beside the dimension that had them. A pick names one kind and only that kind gets a box. The chip that sits over each entity carries a side identity too, which is what a rectangle needs once its first edit or a reload explodes it into four segments.
- **An extrusion can actually end at the face you click.** To face was wired from the panel inward and never into the viewport's click router, so every face click during a staged extrusion was dropped and the panel just kept asking for one. Three faults behind it: a cut preview showed a body whose faces could not be measured at all, a sketch drawn on a face dropped the picked face from its recipe and froze at its first measurement, and the extents outlived the extrusion that set them.
- **An open sketch keeps its way out on every ribbon tab.** Finish and Exit are the Sketch tab's own commands, so opening View or Parametric mid-sketch left the canvas up with no way to finish or leave it. They ride along on whatever tab is showing. The tab picked inside a sketch no longer outlives it either, so the next sketch opens on the drawing tools rather than behind whatever was last chosen.

## What is new in 0.98.1

A point release about the things you touch: corners that finally blend, the
extrusion editor, the units every field reads and writes, where the assembly
tools live, and what the view cube's arrows mean.

- **The corner of a block rounds and bevels exactly.** Three edges meeting at a vertex had no exact rung: the request fell to the faceted tier and came back approximated, or refused to blend at all. `edge-finish/vertex-blend` builds the classical rolling ball — a cylinder along each edge, a sphere octant where all three are chosen, a run-out into the face across the end where one is — so a filleted cube is six planes, twelve cylinders and eight octants, and exports as those rather than as a tessellation. Doing it a corner at a time works too. ADR 0034 records the construction, what it refuses by name, and the one capability the exactness costs: a corner blended exactly no longer accepts a *faceted* finish along the edges beside it, because no tier can rebuild a face against the curved surfaces it leaves, and that request now refuses instead of quietly approximating. Two faults in the faceted tier were fixed on the way: a stack overflow that aborted the process on a body carrying a tessellated sphere, and a refusal that took twelve seconds to arrive and now takes twenty milliseconds.

- **An extrusion says what it does, how far, and where it stops.** New body, Add and Cut stand together for any sketch with a body to combine with, a sketch on a plane included: the sweep becomes a body and a Boolean folds it in, as its own step in history. Two sides give each direction its own length, with a symmetric lock, built as one sweep from behind the sketch plane. And either side may end at a face you pick rather than a distance you type, stored as the persistent face it reached and measured again on every rebuild, so the feature follows the face when the face moves. ADR 0032 records the design.
- **The document's length unit reaches every field.** It was a setting the measure panel honoured and nothing else did. Now every readout is formatted in it and every typed length is read in it — the extrusion distance, the fillet radius, the sketch dimension boxes and tool fields, the part library's length, the mass properties, the interference and clearance readouts. A typed value may carry its own suffix (`10mm`, `0.5in`, `1e3um`), which always wins, and a preference names the unit new documents open in. Geometry, files and interchange stay in millimetres. ADR 0033 records the design.
- **Assembly tools have their own tab.** Move, Rotate, Scale and Insert part left the crowded Model tab for an Assembly tab, with the Select group riding along so picking a part never means changing tabs.
- **The view cube's turn arrows ride an orbit ring.** Four flat triangles placed along the adjacent faces' normals read as the faces pointing somewhere. The cube now sits inside the ring the camera actually travels on, drawn in the cube's own projection so it tilts with the view, and the side arrows sit on that ring pointing the way the camera goes.
- **A whole rim highlights as one.** A circular edge on a faceted-tier body no longer lights up one chord at a time under the pointer: coplanar fragments of the same rim are grouped into the logical edge they belong to, slot half-circles included.
- **A wheel that glides and a 3D mouse that is smooth.** Wheel zoom eases to its target instead of stepping, anchored where the pointer is, and the SpaceMouse's axes pass through a shaped, low-pass filter that settles to rest rather than jittering.
- **`1e3` in the main application's fields,** as the scripting language already accepted.

## What is new in 0.98

This release is about trust: in the numbers the analysis publishes, in a server that outlives whatever is sent to it, and in two desktop applications that behave the way desktop applications are expected to. It also gives the view cube its corners and reads a 3D mouse.

- **An honest clearance bound.** `analysis.interference`, `analysis.sweep` and `probe clearance` measured on display facets while publishing a bound of 0.00002 mm, so two curved parts with a true 0.020 mm gap could read 0.0295 mm once the closest approach fell between vertices, and a machined running fit passed that should have refused. The kernel now reports the sagitta it spent on every display chord, face by face, and a second descent through the facet hierarchies finds the least the true surfaces can be apart. `bound` is that difference, every state and verdict is judged on `distance - bound`, and a sweep stops a bound short of contact rather than a vertex past it. [`docs/verification.md`](docs/verification.md) and the schemas say what the number now means.
- **A server that survives its callers.** Block and array-type nesting join the expression depth limit, an import chain is capped at 16 and a script at 256 modules, a snapshot refuses more than 8192 pixels a side before allocating, `edges(count:)`, strings and arrays have ceilings, and every request runs under `catch_unwind` on a 256 MiB-stack thread, so a kernel panic is answered as an internal error while the session keeps its work. `session.reset` starts over without a new process, since `script.run` adds to the session it has, and a study with too few subjects names the steps it could have been given.
- **`.art` reads `1e-3`, names its tokens, and selects rims.** Exponent literals parse; a parse error says ``expected `)` but found end of file`` rather than naming the lexer's types; `faces(">Z").edges()` is every edge bounding a face, `.rim()` its outer loop, `edges(">Z")` the same as sugar, and `cyl.edges()` every edge of a step, all of which take the exact blend rungs and round-trip through journals and decompiled scripts.
- **The CLI is `artificer-api`,** as its help and this README always said. `snapshot part.art out.png` writes a PNG, an undeclared `--param` is refused with the declared names listed, a run whose reader goes away (`report part.art | head`) ends quietly, and [`docs/art-scripting.md`](docs/art-scripting.md) describes the flags that exist.
- **Native file dialogs, and no work lost silently.** Both applications open and save through the desktop's own dialog, with the typed path kept as a fallback. A document is dirty when its revision differs from the one last saved, the header says so, and closing the window, closing a tab, opening another document, loading an example or dropping a file asks first. ADR 0030 records the policy.
- **Refusals lead with the message.** A rejection card in the workbench and the Script Studio console now show the kernel's plain-language message first, then its suggestion and any candidate entities, then the code.
- **A view cube with corners and edges, that flies.** The cube's twelve edges and eight corners are click targets, so any isometric or half-turned view is one click with world Z kept upright, and every cube click flies the camera there with the same short turn a normal-to-face view takes rather than cutting to it.
- **A 3D mouse.** A 3Dconnexion SpaceMouse is read as plain USB HID by the new `artificer-spacemouse` crate, with no vendor driver and no C source, and orbits, pans and zooms through one camera mapping in the workbench and Script Studio alike. Device status and a sensitivity slider sit under About; the udev rule Linux needs ships as [`packaging/linux/70-spacemouse.rules`](packaging/linux/70-spacemouse.rules). ADR 0031 records the design.
- **A release is never rebuilt.** The release jobs run only when a tag is created, and a published release is refused rather than deleted, so `scripts/publish-release.sh retag` can move the tags that point off main without replacing the binaries anyone already runs.

## What is new in 0.97

This release is about verification-driven CAD: letting a program, not only a person, read what the kernel did and check the result — and then asking the question a drawing cannot answer, which is whether the parts actually go together.

- **The session report.** `artificer-api report part.art` (or `run --json`) prints a versioned JSON document: every step with the strategy **rung** that certified it (`face-feature/exact-prism`, `edge-finish/rim-blend`, `boolean/analytic`, ...) and whether it was exact or fell to the faceted tier, the body's exact volume, area and centroid, every face and edge described from its analytic carrier, the names the script gave, and the failing step with its diagnostic codes and script line when a run stops short. The shape is published as a JSON Schema in [`docs/report-schema.json`](docs/report-schema.json) and a test keeps its list of diagnostic codes equal to what the kernel source emits. The JSON-RPC methods `script.report`, `report` and `query.describe` give the same over the wire.
- **Interference studies.** `analysis.interference` measures every pair of named bodies and publishes a versioned document: apart, touching or overlapping, how close, where on each body, and the shared volume where the Boolean engine can supply it. Where the Boolean engine cannot carry the operands the pair keeps its measured clearance and records the engine's refusal code beside it, so a study never fails because a Boolean did. The workbench runs the same study over its visible bodies from View ▸ Interference and lists the pairs worst first. Its schema is [`docs/analysis-schema.json`](docs/analysis-schema.json), held to the kernel by a test.
- **Clearance between bodies.** `probe clearance` answers how close two bodies come, where, and whether they are apart, touching or inside one another. It runs over a bounding-volume hierarchy of each body's facets rather than through a Boolean, so it answers for bodies the Boolean engine refuses. Facets are chords, so alongside the measured distance it publishes a `bound` the kernel earns rather than assumes: the sagitta of every display chord that comes as near, summed over the two bodies by a second descent through the same hierarchy. The true gap is never below `distance − bound`, and every judgement — apart, touching, inside, and a fit profile's verdict — is made on that pessimistic figure, so a pair is never called clear, and a running fit never passed, on the strength of where a chord happened to fall.
- **Probes that change nothing.** `probe` answers volume, surface area, face area, edge length, minimum distance, the overlap volume of two bodies, point containment and thinnest wall, each with a tier and the method behind it, and leaves the session's digest untouched. The reference is [`docs/verification.md`](docs/verification.md).
- **Clearance profiles, so a measurement becomes an answer.** `0.42 mm` says nothing until a fit says what it wanted. `analysis.profiles` publishes a catalogue an agent can discover rather than guess — a machined running fit at 0.02–0.08 mm, masked stereolithography at 0.05–0.15, an FDM press fit at 0.10–0.20, an FDM sliding fit at 0.30–0.50, and plain assembly, which asks only that nothing shares space — and a study run against one earns every pair a verdict. `too_close` is the only verdict that fails; `loose` still reports a part meant to be held that is not. A fit of your own goes in inline, with no upper complaint if you omit one.
- **A heat map of where it is tight.** `analysis.clearance_field` reads the signed clearance at every corner and centre of every display facet — positive a gap, negative how far inside — and the workbench paints it straight onto the body, so the tight spot is somewhere you look at rather than a number you correlate. The palette is a measured ramp rather than a fixed scale, and the legend names what the colours are worth.
- **Joints that move.** A revolute joint on an occurrence gives it a coordinate. The solver poses every component from the drivers, carrying each child's whole subtree, and refuses by name rather than guessing: an unknown or fixed or disabled joint, a driver outside the joint's limits, a duplicate, a non-finite value, a cycle in the tree.
- **Sweeping a mechanism through its travel.** The harder question is not whether the parts fit where they sit, but whether they fit *anywhere they can go*. `analysis.sweep` measures every pair at every position and stops at the first collision, because past that the parts have already passed through one another and nothing beyond is a pose the real thing reaches. It reports how much of the travel it answered for against how much it was offered, so an interrupted sweep reads as unmeasured rather than clear. Its schema is [`docs/sweep-schema.json`](docs/sweep-schema.json). The workbench runs it over the joints the play button animates, off the UI thread, with a progress count.
- **A move gizmo you can grab.** Three arrows on X, Y and Z at the tool's origin. Grab one and the drag is constrained to that axis alone, with the other two dimmed so it is clear what you have hold of; the distance follows the cursor along the axis rather than raw pixels.
- **Insert a part into the design you have open.** In the Model tab's Create group: a catalogue part arrives as its own body and occurrence, which is how an assembly is built up and what the joint solver then poses.
- **Everything above is reachable over the wire**, not only from Rust, and [`docs/art-scripting.md`](docs/art-scripting.md) — the reference written to be handed to an AI agent as-is — now covers the analysis surface end to end, with the request an agent would actually send for each and a section on what the numbers are worth: facets are chords, so an approximate distance can be off by the published `bound` — the sagitta of the chords it was read from — and the kernel's own verdicts already subtract it; a caller reading `distance` alone should do the same before concluding a part fits. `session.reset` returns a server to a fresh session, since `script.run` adds to the one it has.
- **Every step result** now carries its rung, tier and construction warnings, and Script Studio prints them in the console.
- **Exact STEP.** `artificer-api export part.art part.step` (JSON-RPC `export.step`, the workbench's "STEP (exact B-rep)") writes the body as AP214 `advanced_brep_shape_representation`: planes, cylinders, cones, spheres and tori as the five STEP elementary surfaces, lines, circles and ellipses as themselves, cavities as `brep_with_voids`, nothing tessellated. The exporter's tests read every file back as a manifold B-rep and check each face's sense against the kernel's own normals; `tools/oracle-occt/step_measure.py` is the OpenCascade oracle a development machine runs to confirm imported volume and area to one part in a billion. Faceted STEP stays for mesh consumers (`--faceted`, "STEP (faceted)").
- **Journals back to scripts, and scripts compared.** `artificer-api journal session.json --art out.art` (JSON-RPC `journal.art`, `Session::to_art`) writes a session's journal as a `.art` script that rebuilds the same digest, with dimensions as `param`s, snapshot-bound references regenerated as history selectors, and faceted-tier steps annotated. `artificer-api diff a.art b.art --json` (JSON-RPC `script.diff`) compares two scripts semantically: parameters, steps added, removed, moved or changed, names renamed or retargeted. Script Studio pulls a journal into the open script behind that diff, and exports its own.
- **Shell.** `shell(open: faces(">Z"), wall: 3)` hollows a body to one uniform wall, open at one face, at two opposite faces, or closed with a void. A body is read as a prism about the open face first and as a solid of revolution second, so a box, a cylinder, a slot, a two-diameter turned hub and a tapered post all hollow exactly, with the wall measured square to the surface. The answers are closed-form: a shelled box open at the top has volume `bdh − (b−2w)(d−2w)(h−w)`, and `probe.min_wall` reads the wall back. A closed shell needs no Boolean at all, because the core is the body's own boundary offset inward and is enclosed directly as a void.
- **Exact mirror, and patterns of features.** `mirror` reflects any body exactly: every carrier is reflected as itself, blends included, so the mirrored part keeps its face count, volume and area with its centroid reflected, and takes exact features afterwards. `pattern(step: hole, axis: [0, 0, 1], axis_origin:, count: 6)` and `pattern(step: hole, direction:, spacing:, count:)` repeat a drilled hole or a face-sketch extrusion around an axis or along a row by replaying the same exact feature at each placement, each instance a step of its own under one journal entry, so a rim fillet on a patterned hole certifies through the same blend ladder as on the original. A whole-body `pattern(direction:, spacing:, count:)` is exact as well: every copy is the body under a rigid translation, so a cylinder patterns as cylinders rather than facets.
- **`.art` 0.3: functions, modules and typed parameters.** `fn standoff(on: face, at: [f64; 2], height: f64, label: str) -> body { ... return boss with faces { top: boss.top }; }` packages steps that recur, with labels scoped to the call so a loop of calls stays unique without string arithmetic, and exported faces that keep resolving after later steps. `use "lib/standoffs.art";` shares functions and constants between scripts. `param wall: f64 [mm] in 1.2..4.0 = 2.0 "wall thickness";` carries a unit, a range and a description, and `artificer-api params part.art --json` (or JSON-RPC `script.params`) lists them without running the script. Unbound names, arity and type mismatches, recursion and import cycles refuse with a line and column.

## What is new in 0.96

This release turns the kernel's scripting language into a product of its own and pairs it with a live editor.

- **`.art` scripting, version 0.2.** The language now reaches the whole kernel: sketches from lines, circles, arcs and rectangles on world planes or on faces; extrude with add, cut and draft; revolve; drill, push/pull, fillet, chamfer, mirror and pattern; union, difference and intersection between bodies; face and edge selectors by direction, position and history; the trigonometry in degrees. Errors name their line and column, and parameters have defaults a host can override. The full reference, written for people and for AI agents, is [`docs/art-scripting.md`](docs/art-scripting.md).
- **Artificer Script Studio.** A third program in the shape OpenSCAD made familiar: the script on the left, the exact model on the right, the `param` lines as a customizer, and a console that points at the failing line. It re-runs as you type, keeps the last good model on screen through an error, sections the model on any origin plane, and exports STL and OBJ.
- **Section analysis.** The workbench and Script Studio clip the model to one side of a plane and cap the cut, so the inside of a part can be checked for the solid it should be.
- **Oblique sections of cylinders are exact.** Angled holes, mitred cylinder ends and oblique cuts of round bodies meet on the ellipse curve through the analytic Boolean engine, with no faceting.
- **Sketch constraints from the canvas.** Coincident, horizontal, vertical, parallel, perpendicular, equal, tangent and collinear relations are applied by clicking geometry, from a constraint group on the sketch bar.
- **Named faces and loops, for a person in the loop.** A `let` bound to a selector names a face. Script Studio lists the names, shows one when its face is clicked, and describes the face in plain words, so a request such as "six bolt holes on `flange_top`" needs no guessing. `for` loops with `"bolt_" + i` labels make counts into parameters an agent can change. The agent workflow is written up in the language reference.
- **Selectors that mean what they say.** `faces(">Z")` is the highest upward face on a stepped part, and the nearest-face selector measures to the surface, so a point placed on a face finds it.
- **Presentation.** The three origin planes read as translucent datum cards with corner labels; the camera no longer zooms in when an extrusion commits; the outline of a revolved body no longer breaks at its seam.

---

## Three products, one repository

Artificer is three things, deliberately kept apart:

| | What it is | Where it lives | Depends on |
|---|---|---|---|
| **Artificer Kernel** | A standalone exact B-rep modelling kernel with its programmatic API built in: a Rust API, a JSON-RPC 2.0 server, the `.art` scripting language, headless PNG/SVG rendering, and STL, OBJ and STEP export. | [`crates/kernel`](crates/kernel) | Nothing but its own geometry, compute, and protocol crates. No UI, no GPU, no C or C++. |
| **Artificer Workbench** | A native desktop parametric CAD application: sketching, features, assemblies, a part library, and a parametric history. | [`apps/workbench`](apps/workbench) | The kernel, through the same protocol every other client uses. |
| **Artificer Script Studio** | A live `.art` visualiser in the OpenSCAD shape: the script on the left, the exact model on the right, a customizer built from the script's parameters, and a console that points at the failing line. | [`apps/script-studio`](apps/script-studio) | The kernel, through its API session, and the workbench's viewport and theme. |

The separation is enforced, not aspirational. The CI architecture audit fails the build if a UI or rendering dependency enters the kernel crate, and the kernel is exercised end to end by a headless test suite that never opens a window. You can embed the kernel in your own application, drive it from another language over JSON-RPC, or script it from a file, and you get exactly the same geometry the workbench would build.

---

## The Artificer Kernel

Every surface, curve, and boundary in an Artificer model is analytic. There are no mesh approximations standing in for solids, no tolerance stacking, and no healing heuristics that quietly change your geometry. An operation either produces a certified manifold solid or refuses with a named, structured reason.

### Exact by construction

- **Analytic geometry only.** Planes, cylinders, cones, spheres, and tori as surfaces; lines and circles as curves. A fillet on a cylinder's rim at its own radius is an exact sphere, not a patch of triangles.
- **Closed-form calculus.** Volume, surface area, centroid, and inertia are integrated analytically over the true surfaces. Test gates pin them against independent derivations at one part in a billion.
- **Transactional validation.** Every result passes Euler–Poincaré, edge-use, loop-orientation, locus, and self-intersection checks before it is published. A snapshot that fails is never returned.
- **Certified or refused.** Each operation is a ladder of exact strategies. When none applies, the kernel says which rung refused and why, with a diagnostic code, rather than guessing. The one remaining approximate tier, for cuts that cross curved voids, is labelled as an approximation in its report.
- **Deterministic and content-addressed.** Snapshots carry a semantic digest. The same commands produce bit-identical models on every platform, so replays, caches, and audits agree.

### The API is part of the kernel

The programmatic surface lives in `artificer_kernel::api` and ships with the kernel, not beside it. Three entry points cover most uses:

**Rust.** Embed the kernel directly:

```rust
use artificer_kernel::CancellationToken;
use artificer_kernel::api::{ApiCommand, Session};
use artificer_protocol::Point3;

let mut session = Session::new();
let token = CancellationToken::default();

session.execute(ApiCommand::MakeBox {
    label: "cube".into(),
    origin: Point3::new(0.0, 0.0, 0.0),
    size: [50.0, 50.0, 50.0],
}, &token)?;

let measures = session.snapshot.measures();
println!("Exact volume: {:.6} mm³", measures.volume);
println!("Bounds: {:?}", session.query().bounds()?);
```

**JSON-RPC 2.0.** Run the kernel as a headless service on stdin/stdout, one request per line, batches and notifications included:

```sh
cargo run --release -p artificer-api-server -- serve
```

```json
{
  "jsonrpc": "2.0",
  "id": "1",
  "method": "execute",
  "params": {
    "type": "make_box",
    "label": "base_block",
    "origin": { "x": 0.0, "y": 0.0, "z": 0.0 },
    "size": [100.0, 50.0, 25.0]
  }
}
```

Domain errors come back as JSON-RPC error `-32000` with the structured `ApiError` in `error.data`: a code, a plain-language message, a suggestion, candidate entities where a selector was ambiguous, and the kernel's own diagnostics.

**`.art` scripts.** A small parametric language evaluated straight into kernel commands:

```text
// bracket.art — parametric bracket with mounting holes
param width: f64 = 100.0;
param depth: f64 = 50.0;
param thickness: f64 = 10.0;

let base = box(origin: [0, 0, 0], size: [width, depth, thickness], label: "base");
let top = base.face("top_face");

// Hole centres are in the face's own frame, whose origin is the face centre.
drill(face: top, center: [-30.0, 0.0], diameter: 8.0, depth: thickness, label: "hole_l");
drill(face: faces(">Z"), center: [30.0, 0.0], diameter: 8.0, depth: thickness, label: "hole_r");

// Fillet every vertical edge of the block at once.
fillet(edges: edges("|Z"), radius: 2.0, label: "soften");
```

```sh
cargo run --release -p artificer-api-server -- run bracket.art --param width=120
cargo run --release -p artificer-api-server -- snapshot bracket.art bracket.png
cargo run --release -p artificer-api-server -- export bracket.art bracket.stl
```

The whole API is reachable from a script, one builtin per command, with named arguments and angles in degrees:

| Builtin | Makes |
|---|---|
| `box(size:, origin:, label:)`, `cylinder(radius: or diameter:, height:, center:, axis:, label:)` | A new body. |
| `sketch(on: "XY" \| "XZ" \| "YZ" \| face, entities: [...], label:)` with `line(start:, end:)`, `circle(center:, radius:)`, `arc(center:, radius:, start_angle:, end_angle:)`, `rect(origin: or center:, width:, height:)`, `spline(points:, closed:)` | A profile on a plane or a face. Lines and arcs chain into loops; nested loops become holes. |
| `extrude(sketch:, distance:, operation: "new" \| "add" \| "cut", draft:, regions:, label:)` | A prism, or a drafted loft for a new body. |
| `revolve(sketch:, axis:, axis_origin:, angle:, operation:, label:)` | A solid of revolution. |
| `plane(from: "XY", offset:)`, `plane(origin:, normal:, x_axis:)`, `plane(on: face, offset:)`, `plane(between: [a, b])`, `plane(through: edge, face:, angle:)` | A plane for `sketch(on:)`: in space, or placed by the body's faces and edges. |
| `loft(sections: [a, b, …], operation: "new" \| "add" \| "cut", label:)` | A solid through sketches on different planes, with exact walls: planes, cylinders and cones where they fit, ruled surfaces between two sections otherwise, smooth B-spline surfaces through three or more. |
| `drill(face:, center:, diameter:, depth:)`, `push_pull(face:, distance:)`, `fillet(edges:, radius:)`, `chamfer(edges:, distance:)` | Face and edge features. |
| `shell(open:, wall:)` | Hollows the current body to one wall, open at one face, two opposite faces, or none; prisms and solids of revolution. |
| `mirror(origin:, normal:)` | Reflects the current body exactly. |
| `pattern(step:, axis:, axis_origin:, count:, angle:)`, `pattern(step:, direction:, spacing:, count:)` | Repeats a drilled hole or a face-sketch extrusion around an axis or along a row. |
| `pattern(direction:, spacing:, count:)` | Copies the whole body along a row, exactly. |
| `union(target:, tool:)`, `difference(target:, tool:)`, `intersection(target:, tool:)` | Booleans between two steps. |
| `faces(">Z")`, `edges("\|Z")`, `nearest(point:, kind:)`, `step.face("role")`, `step.edge("role")`, `faces(">Z").edges()`, `faces(">Z").rim()`, `edges(">Z")`, `step.edges()` | Selectors. `.edges()` on a face is every edge bounding it and `.rim()` its outer loop; `edges(">Z")` is short for `faces(">Z").edges()`; `step.edges()` is every edge a step made. |
| `fn name(a: f64, on: face, label: str) -> body { ... return step with faces { top: ... }; }` | Functions with typed arguments, call-scoped labels and exported faces. |
| `use "lib/parts.art";` | Modules of functions and constants. |
| `param wall: f64 [mm] in 1..4 = 2 "wall";` | Parameters with a unit, a range and a description. |
| `sqrt abs floor ceil round min max clamp pow hypot sin cos tan asin acos atan atan2`, `pi` | Arithmetic. |

Errors name their line and column, so an editor can point at them. `artificer-api report part.art` prints the machine-readable session report instead of prose, with every step's rung and tier, the body's exact measures and every face described. Open the same file in **Artificer Script Studio** to edit it live against the kernel's viewport, with the `param` lines as a customizer. The complete language reference, with every function, argument, selector and method — including the analysis surface: clearance, interference studies judged against a clearance profile, the per-facet heat map, and sweeping a mechanism through its travel — is [`docs/art-scripting.md`](docs/art-scripting.md); it is written to be handed to an AI agent as-is.

### What the kernel does today

- Primitives, planar-profile extrusion with holes and islands, revolve, drafted extrusion as an exact loft to the profile's offset section, push/pull, holes, ribs, shell, exact mirror, circular and linear patterns of face features, and exact whole-body patterns.
- Exact chamfers and constant-radius fillets, including fillets that run around a whole hole rim, with spherical corners where the rim turns in and elliptical mitre seams where it turns out.
- Regularized Boolean union, difference, and intersection, with an exact engine for plane and cylinder operands and a faceted fallback that says so.
- Analytic surface–surface intersections across the vocabulary, published as a supported-domain matrix.
- Geometric selectors (`faces(">Z")`, `edges("|Z")`, by extremum, by type, parallel to a direction) that resolve deterministically or refuse with candidates.
- Headless tessellation at display or authoritative chord budgets, SVG and PNG snapshots from any camera, and STL, OBJ and STEP export.

---

## Built for AI-driven CAD

Language models and agents are good at saying what a part should be and poor at nudging triangles. A kernel that serves them well has to be declarative, honest about failure, and inspectable without a screen. Artificer was shaped by those requirements:

- **A closed, typed command vocabulary.** Every operation is a serialisable command with named fields and documented domains. There is no hidden UI state to reproduce; a model is its command journal.
- **Refusals are data.** An operation that cannot be certified returns a diagnostic code, the reason, and where it applies a suggestion or the list of candidate entities. An agent can read the refusal and try the next thing instead of inheriting broken geometry.
- **Stable references.** Faces and edges are addressed by geometric selectors and by persistent, provenance-tracked references, so a plan written before the model exists still resolves after it is built.
- **Deterministic replay.** Journals replay to bit-identical snapshots with content digests, which makes results cacheable, diffable, and safe to verify independently.
- **Headless eyes.** Snapshots render to PNG or SVG from standard or explicit cameras, so a vision-capable model can look at what it built without a GPU or a window.
- **Exact measurements.** Volumes, areas, centroids, bounds, and distances are closed-form answers, not mesh estimates, so a planner can trust a number it reads back.
- **A report, not prose.** A run ends in a versioned JSON report naming the rung that certified each step, whether it was exact, the body's measures, every face and edge, and the first failure with its codes; probes answer questions about the model without changing it. See [`docs/verification.md`](docs/verification.md).

The same properties are what make the kernel a sound foundation for any programmatic CAD: generative design, automated tooling, cloud pipelines, and your own front end.

---

## The Artificer Workbench

The desktop application is the reference client for the kernel and a complete single-part and small-assembly modeller in its own right.

<div align="center">

![Sketching Mode](apps/workbench/tests/snapshots/workbench_sketch_xy_rectangle.png)

*Sketching with live profile detection: every bounded region is selectable the moment it closes.*

</div>

- **Sketching.** Lines, rectangles, circles, arcs, polygons, slots, splines, text set from a bundled typeface as exact outlines, fillets, chamfers, trims, patterns, relations, and dimensions. Intersecting geometry splits into separately selectable regions. Live dimensions edit in place.
- **Features.** Extrude, drafted extrude, revolve, push/pull, holes, hole patterns, ribs, shell, mirror, patterns, chamfers, and fillets, each staged behind one confirmation gate and recorded in an editable parametric history. A shell's wall and its open face are both parametric, so the face survives a replay like any other feature target.
- **Face sketches.** Sketch on any planar face with the body always in view; project the geometry hidden below the surface as an x-ray when you need to line up with it.
- **Assemblies and library.** A content-addressed part library, rigid placements, grounding, and revolute joints with live motion. The assembly tools have a ribbon tab of their own: Insert part, and the Move, Rotate and Scale placement tools.
- **Documents.** Several documents open at once in tabs along the top of the window; a portable native document format with a versioned schema.
- **Viewport.** Exact silhouettes, hidden-line rendering, smooth shading from analytic normals, a view cube, and themes. The cube's corners and edges are clickable as well as its faces, so an isometric or a half-turned view is one click with world Z kept upright, and every cube click flies the camera there with the same short turn a normal-to-face view takes rather than cutting to it. A 3Dconnexion SpaceMouse orbits, pans, and zooms the model directly (button 1 fits the view, button 2 flips between the two most recent views), with device status and a sensitivity slider under About Artificer; on Linux the raw HID node is root-only until a udev rule such as `KERNEL=="hidraw*", ATTRS{idVendor}=="256f", MODE="0660", TAG+="uaccess"` (and the same for `046d` with `ATTRS{idProduct}=="c62?"`) in `/etc/udev/rules.d/70-spacemouse.rules` grants it; the rule ships as [`packaging/linux/70-spacemouse.rules`](packaging/linux/70-spacemouse.rules) — see ADR 0031.

---

## Artificer Script Studio

The third program is for people who would rather type a model than draw one. Script Studio is a live `.art` editor in the shape OpenSCAD made familiar, built on the same kernel, viewport, and theme as the workbench.

<div align="center">

![Script Studio](docs/images/script-studio.png)

*The flanged hub example: the script, the exact model it builds, its parameters as a customizer, and every step in the console.*

![Script Studio section](docs/images/section.png)

*The same part under section analysis, cut through the axis: the cut faces are capped, the bore and a bolt hole show in the caps, and the FACES panel lists the names the script gave.*

![Filleted flange](docs/images/fillet.png)

*`filleted_flange.art`: every rim of the hub rounded with exact torus blends, each fillet naming both half-circle edges of its rim.*

</div>

- **Live.** Every edit re-runs the script on a worker thread after a short pause; a run that an edit supersedes is cancelled rather than waited for, and the last good model stays on screen while you type.
- **Customizer.** The script's `param` lines become a panel of values you can drag. A dragged value re-runs the script without touching the text, and one click puts the script's own default back.
- **Console.** Every step lists its label, topology, and time. A parse or evaluation error names its line and column, a failing step names the line that labels it, and clicking the error puts the cursor there.
- **Editor.** Syntax colouring for the `.art` vocabulary, line numbers, and the error's line washed in red.
- **Files.** Open and save scripts, drop a file onto the window, export the model as STL or OBJ, and start from the bundled examples.

```sh
cargo run --release -p artificer-script-studio -- crates/kernel/examples/flanged_hub.art
```

---

## Quickstart

### Prerequisites

- Stable Rust 1.95 or newer.
- For the workbench only: a GPU toolchain (Metal on macOS, Vulkan or DX12 on Windows, Vulkan with Wayland or X11 on Linux). The kernel and its server need none.

### Build and run

```sh
git clone https://github.com/JackClarkeAE/artificer.git
cd artificer

# The kernel and its API: the headless test suite
cargo test -p artificer-kernel

# The JSON-RPC server on stdin/stdout
cargo run --release -p artificer-api-server -- serve

# Run, render, or export an .art script
cargo run --release -p artificer-api-server -- run crates/kernel/examples/bearing_mount.art

# The desktop workbench
cargo run --release -p artificer-workbench

# The live .art visualiser, on a script of your own
cargo run --release -p artificer-script-studio -- crates/kernel/examples/flanged_hub.art

# Everything
cargo test --workspace
```

### Prebuilt binaries

Installers are published on the [Releases page](https://github.com/JackClarkeAE/artificer/releases). Each one carries the workbench and Script Studio side by side:

- Windows: `Artificer-Setup.exe` installs `Artificer.exe` and `ArtificerScriptStudio.exe`
- Linux: `Artificer.AppImage`, with `ArtificerScriptStudio` alongside it in the plain archive
- macOS (Apple Silicon): `Artificer-macOS-arm64.zip` with `Artificer.app` and `Artificer Script Studio.app`

---

## Architecture

| Layer | Crate | Purpose |
|---|---|---|
| **Kernel** | [`crates/kernel`](crates/kernel) | The exact B-rep kernel: topology, analytic surfaces, strategy ladders, validation, measures, tessellation, and the `api` module (sessions, selectors, JSON-RPC server, `.art` scripting, snapshots, export). |
| **Geometry** | [`crates/geometry`](crates/geometry) | Certified predicates, interval arithmetic, planar and spatial intersection mathematics. |
| **Compute** | [`crates/compute`](crates/compute) | The work pool, cancellation, and performance spans the kernel runs on. |
| **Protocol** | [`crates/protocol`](crates/protocol) | The serialisable command, snapshot, and diagnostic vocabulary shared by every client. |
| **Sketch** | [`crates/sketch`](crates/sketch) | Exact 2D authoring: recipes, constraints, the arrangement into regions, profile compilation, text outlines. |
| **Model** | [`crates/model`](crates/model) | The parametric document: features, persistent references, parameters, journals, and the native file schema. |
| **Kernel server** | [`apps/api-server`](apps/api-server) | The command-line front for the kernel API: `serve`, `run`, `report`, `params`, `snapshot`, `export`, `journal`, `diff`. |
| **Presentation** | [`crates/viewport`](crates/viewport), [`crates/sketch-ui`](crates/sketch-ui), [`crates/ui-core`](crates/ui-core) | The 3D viewport, the sketch canvas, and the shared theme and widgets. None of these can see the kernel's internals. |
| **Workbench** | [`apps/workbench`](apps/workbench) | The desktop application. |
| **Script Studio** | [`apps/script-studio`](apps/script-studio) | The live `.art` visualiser: editor, customizer, console, and the shared viewport, driving the kernel through its API session. |
| **Test kit** | [`crates/testkit`](crates/testkit), [`apps/cli`](apps/cli) | Deterministic cases, journals, and the conformance runner. |

The dependency rules between these layers are checked by `scripts/check-architecture-boundaries.sh` on every CI run. Design decisions are recorded as ADRs under [`docs/architecture/adr`](docs/architecture/adr), and the kernel programme itself in [`docs/architecture/geometry-kernel`](docs/architecture/geometry-kernel).

---

## Roadmap

- [x] Analytic B-rep topology, primitives, planar profiles, exact calculus.
- [x] Regularized Booleans, chamfers, fillets, and hole-rim blends.
- [x] Parametric documents, assemblies, part library, joints.
- [x] The kernel API: Rust, JSON-RPC, `.art` scripts, headless snapshots and export.
- [x] Multi-document workbench, sketch text, drafted extrusion as the first loft rung.
- [x] Shell.
- [x] Construction planes as history features, placed on faces, edges and planes, and followed by what is built on them.
- [x] The ruled surface, and a loft between two sections on any two planes, in the kernel, scripts and the workbench.
- [x] B-spline curves and surfaces: spline sketch profiles extrude, and lofts run smoothly through several sections.
- [x] Revolve and sweep as history features, and construction axes to turn about.
- [ ] Draft on existing faces, exact Booleans between B-spline faces (they use the faceted tier today).
- [x] The ellipse curve, first slice: the mitre seam of a fillet turning a sharp corner, so fillets round square holes and L-shaped rims are exact.
- [x] Oblique plane sections of cylinders on the same curve, through the analytic Boolean: angled holes, mitred cylinder ends, oblique cuts of round bodies.
- [ ] Oblique cone sections, and pipe tees (equal cylinders crossing) on the same ellipse.
- [x] Native STEP write with exact surfaces.
- [ ] Native STEP read, IGES import, DXF drawing sheets.

---

## Licensing

Artificer is dual-licensed.

**Open source: AGPL-3.0-or-later.** The kernel and the workbench are free software under the [GNU Affero General Public License, version 3 or later](LICENSE). You may use, study, modify, and redistribute them, including for commercial purposes, provided you honour the licence: derived works and network services built on Artificer must themselves be released under the AGPL, with their source available to their users.

**Commercial licence.** Organisations that want to build proprietary or closed-source products, cloud services, or internal tools on the Artificer Kernel or Workbench without the AGPL's copyleft obligations can obtain a commercial licence. It covers embedding the kernel in your own software, running it as a service, and shipping the workbench under your own terms, with the same code and the same guarantees.

| You are | You need |
|---|---|
| An individual, a student, a researcher, or an open-source project | Nothing more: the AGPL applies. |
| A company whose product or service will itself be released under the AGPL | Nothing more: the AGPL applies. |
| A company shipping or hosting a proprietary product built on Artificer | A commercial licence. |

To enquire about a commercial licence, open an issue on GitHub titled "Commercial licence enquiry" or contact the maintainers. Contributions to the repository are accepted under the AGPL-3.0-or-later.

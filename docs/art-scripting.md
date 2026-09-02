# The `.art` scripting language, version 0.2

A reference for people and for AI agents writing Artificer scripts. Everything
here is what the kernel implements today (Artificer 0.96, `.art` 0.2); nothing
is aspirational. Where a feature has a limit, the limit is stated.

A `.art` script is a list of steps. Each step names a kernel command, and the
kernel executes the steps in order against one session. The same script
produces the same geometry through the JSON-RPC server, the command-line
runner, the Rust API and Script Studio, because all four compile it with the
same function into the same commands.

```sh
# Run a script headless and print every step's result
cargo run --release -p artificer-api-server -- run part.art

# Render or export the result
cargo run --release -p artificer-api-server -- snapshot part.art --format png --output part.png
cargo run --release -p artificer-api-server -- export part.art --format stl --output part.stl

# Edit it live
cargo run --release -p artificer-script-studio -- part.art
```

---

## 1. Syntax

```art
// A comment runs to the end of the line.

param width: f64 = 40.0;        // a parameter with a default
param depth = width / 2;        // the type is optional; later params may use earlier ones

let plate = box(size: [width, depth, 6], label: "plate");   // a step, bound to a name
drill(face: plate.face("top_face"), center: [0, 0], diameter: 5, depth: 6, label: "hole");
```

- **Statements** end with `;`. There are three kinds: `param`, `let`, and a
  bare call.
- **`param name[: f64] = expr;`** declares a parameter. Parameters are numbers.
  A host (the customizer, the CLI's `--set name=value`, the JSON-RPC `compile`
  call) may override a parameter's value; the default is used otherwise.
  Defaults are evaluated in order, so a default may use earlier parameters.
- **`let name = expr;`** binds any value to a name.
- **Calls** are `name(arg: value, ...)`. Feature calls take named arguments
  only; math functions and `faces("...")`/`edges("...")` take positional
  arguments.
- **Methods** are `step.face("role")`, `step.edge("role")` and
  `step.edges("role", count: n)`.
- **Expressions**: numbers (`12`, `1.5`), strings (`"top"`), arrays
  (`[1, 2, 3]`), identifiers, unary minus, `+ - * /`, parentheses, calls and
  methods. `pi` is predefined.
- **`for name in start..end { ... }`** runs its body once for each whole
  number from `start` up to but not including `end`. Both bounds are
  expressions, so a `param` can set the count. A script may run at most
  10 000 loop iterations in total.
- **`+` joins text.** When either side is a string, `+` concatenates:
  `"bolt_" + i` is `bolt_3` for `i = 3` (whole numbers print without a
  fraction). This is how a loop gives each step its own label.
- A step is *executed* when it is a statement, whether or not it is bound
  with `let`. A call that only appears inside another call's arguments (a
  `line(...)` inside `sketch(...)`, a `nearest(...)` inside `drill(...)`) is
  a value, not a step.

### Labels

Every step has a label, and every label in a script must be unique. The label
is the `label:` argument; when it is omitted the function's own name is used,
which means a second unlabeled `drill` is an error. Labels are how later
steps refer to earlier ones: `union(target: base, tool: boss)` names two
steps, and `plate.face("top_face")` names a face by the step that made it.

### Units and angles

Lengths are in the document's unit, millimetres by convention. **Every angle
is in degrees**: `arc(start_angle: 0, end_angle: 90)`, `revolve(angle: 360)`,
`extrude(draft: 5)`, and the trigonometry (`sin(30)` is `0.5`, `atan2(1, 1)`
is `45`).

### Errors

A script that fails to parse or evaluate reports one error with its **line
and column**. A step the kernel refuses reports the step's label and the
kernel's reason. Script Studio shows both in the console and marks the line;
the CLI prints them; the JSON-RPC server returns them as the error object.

---

## 2. Values

| Value | Written as | Notes |
|---|---|---|
| Number | `12`, `-0.5`, `width * 2` | 64-bit float. |
| String | `"XY"`, `"top_face"` | Used for plane names, roles, selectors, operations, labels. |
| Array | `[1, 2, 3]` | A 2-array is a 2D point; a 3-array is a 3D point or vector. |
| Step | `let b = box(...)` | The bound name of an executed step; passed to Booleans and used for methods. |
| Sketch entity | `line(...)`, `circle(...)`, `arc(...)`, `rect(...)` | Only valid inside `sketch(entities: [...])`. |
| Selector | `faces(">Z")`, `nearest(...)`, `b.face("...")` | Names a face or edge of the current body, resolved when the step runs. |

---

## 3. Bodies, steps and the current body

The session holds one **current body**. Each step either makes a new body or
edits the current one:

- **New body**: `box`, `cylinder`, `extrude` with the default `operation:
  "new"`, and `revolve`. The result becomes the current body. Every earlier
  body stays in the session under its step label.
- **Edit the current body**: `drill`, `push_pull`, `fillet`, `chamfer`,
  `mirror`, `pattern`, and `extrude` with `operation: "add"` or `"cut"`.
- **Combine two bodies**: `union`, `difference`, `intersection` take the
  bodies two earlier steps left behind, by label, and the result becomes the
  current body.

So a multi-body part is written as: make body A, make body B, `union(target:
A, tool: B)`, then keep editing.

A `sketch` is not a body. It is recorded as a step and consumed by the
`extrude` or `revolve` that names it.

---

## 4. Primitives

### `box`

```art
let b = box(size: [40, 30, 10], label: "b");
let c = box(origin: [-20, -15, 0], size: [40, 30, 10], label: "c");
```

| Argument | Required | Meaning |
|---|---|---|
| `size` | yes | `[x, y, z]` extents, all positive. |
| `origin` | no | The minimum corner. Default `[0, 0, 0]`. |
| `label` | no | Step label. |

Face roles for `.face(...)`: `top_face`, `bottom_face`, `front_face`,
`back_face`, `left_face`, `right_face`. Edge role for `.edge(...)`/
`.edges(...)`: `edge` with ordinals 0 to 11.

### `cylinder`

```art
let c = cylinder(center: [0, 0, 0], axis: [0, 0, 1], radius: 8, height: 30, label: "c");
let d = cylinder(diameter: 16, height: 30, label: "d");
```

| Argument | Required | Meaning |
|---|---|---|
| `radius` or `diameter` | one of them | The cross-section. |
| `height` | yes | Along the axis. |
| `center` | no | Centre of the base circle. Default `[0, 0, 0]`. |
| `axis` | no | Direction of the height. Default `[0, 0, 1]`. |

---

## 5. Sketches

```art
let profile = sketch(on: "XY", label: "profile", entities: [
    rect(center: [0, 0], width: 40, height: 20),
    circle(center: [10, 0], radius: 4),
]);
```

| Argument | Required | Meaning |
|---|---|---|
| `on` | yes | `"XY"`, `"XZ"`, `"YZ"`, or a face selector such as `faces(">Z")` or `plate.face("top_face")`. |
| `entities` | yes | An array of `line`, `circle`, `arc`, `rect`. |
| `label` | no | Step label. |

**Coordinates.** On a world plane, 2D coordinates are that plane's axes with
the origin at the world origin: `"XY"` maps `[x, y]` to world `(x, y, 0)`;
`"XZ"` maps `[x, z]` to `(x, 0, z)`; `"YZ"` maps `[y, z]` to `(0, y, z)`. On
a face, the origin is the **centre of the face** and the axes are the face's
own; a circle at `[0, 0]` is centred on the face.

**Regions.** Closed loops become regions. A loop inside another loop is a
hole in it, so the sketch above is a plate with a hole. Intersecting loops
are not split into regions: draw closed, non-crossing loops. Lines must
chain end to end into a closed loop; a loop may close along the revolve axis.

### Entities

| Entity | Arguments | Notes |
|---|---|---|
| `line(start: [x, y], end: [x, y])` | both required | A segment. Chain segments into loops. |
| `circle(center: [x, y], radius: r)` | `radius` or `diameter`; `center` defaults to `[0, 0]` | A closed loop. |
| `arc(center:, radius:, start_angle:, end_angle:)` | `radius` or `diameter`; angles in degrees, counter-clockwise | Part of a loop with lines or other arcs. |
| `rect(width:, height:, origin: [x, y])` | `origin` is the minimum corner | A closed loop. |
| `rect(width:, height:, center: [x, y])` | centred | `rect(width:, height:)` alone is centred on `[0, 0]`. |

---

## 6. Features

### `extrude`

```art
let base = extrude(sketch: profile, distance: 6, label: "base");
extrude(sketch: pocket, distance: 3, operation: "cut", label: "pocket_cut");
let boss = extrude(sketch: boss_profile, distance: 12, operation: "add", label: "boss");
let frustum = extrude(sketch: square, distance: 10, draft: 5, label: "frustum");
```

| Argument | Required | Meaning |
|---|---|---|
| `sketch` | yes | The sketch step. |
| `distance` | yes | Extrusion length along the sketch plane's normal. |
| `operation` | no | `"new"` (default), `"add"` (also `"join"`, `"union"`), `"cut"` (also `"subtract"`). |
| `draft` | no | Draft angle in degrees; the section shrinks toward the far end. New bodies only. |
| `regions` | no | Which regions to extrude, by index, when a sketch has several. Default: all. |

`"add"` and `"cut"` need a sketch drawn **on a face** of the current body
(`sketch(on: faces(">Z"), ...)`). A sketch on a world plane can only make a
new body; join it with `union` afterwards if that is what you want.

### `revolve`

```art
let section = sketch(on: "XZ", label: "section", entities: [
    rect(origin: [10, 0], width: 5, height: 4),
]);
let ring = revolve(sketch: section, axis: [0, 0, 1], label: "ring");
```

| Argument | Required | Meaning |
|---|---|---|
| `sketch` | yes | A sketch whose plane contains the axis. |
| `axis` | no | Axis direction. Default `[0, 0, 1]`. |
| `axis_origin` | no | A point on the axis. Default `[0, 0, 0]`. |
| `angle` | no | Degrees. Default and only supported value: `360`. |
| `regions` | no | As for `extrude`. |

The section must lie on one side of the axis (touching it is fine). A section
drawn on `"XZ"` about `[0, 0, 1]` is the usual `(r, z)` half-section: `x` is
the radius, `y` of the sketch is `z` of the world.

### `drill`

```art
drill(face: faces(">Z"), center: [0, 0], diameter: 6, depth: 10, label: "bore");
drill(face: top, center: [12, 0], radius: 2, depth: 4, label: "pin");
```

| Argument | Required | Meaning |
|---|---|---|
| `face` | yes | The planar face to drill into. |
| `center` | no | In the face's own 2D frame (origin at the face centre). Default `[0, 0]`. |
| `radius` or `diameter` | one of them | The hole. |
| `depth` | yes | Into the material. A depth through the whole body makes a through hole. |

### `push_pull`

```art
push_pull(face: plate.face("top_face"), distance: 3, label: "raise");
```

Moves a planar face along its normal: positive adds material, negative
removes it.

### `fillet` and `chamfer`

```art
fillet(edges: [edges("|Z")], radius: 3, label: "round_corners");
chamfer(edges: [b.edge("edge", ordinal: 0), b.edge("edge", ordinal: 1)], distance: 1, label: "break");
fillet(edges: [nearest(point: [0, 45, 8], kind: "edge"), nearest(point: [0, -45, 8], kind: "edge")], radius: 1.5, label: "rim");
```

| Argument | Required | Meaning |
|---|---|---|
| `edges` | yes | An array of edge selectors. Set selectors such as `edges("|Z")` expand to every matching edge. |
| `radius` (fillet) / `distance` (chamfer) | yes | The blend size. |

Exact blends cover straight edges, the rims of round and polygonal holes,
and the rims of cylinders and revolved bodies (torus bands). A circular rim
is two half-circle edges; name both, as above. Concave junctions between a
boss and a plate are not filleted in 0.2.

**Order matters.** A rim next to a drilled hole has no exact blend in 0.2,
and a hole drilled through a face that a blend already borders falls to the
faceted tier, which is slow. So: draw round features that can be part of a
revolve into the section, fillet the rims, and drill afterwards only on
faces the blends do not touch. `crates/kernel/examples/filleted_flange.art`
rounds every rim of a flanged hub; `flanged_hub.art` shows the order that
keeps rim fillets and bolt holes on the same part exact.

### `mirror` and `pattern`

```art
mirror(origin: [0, 0, 0], normal: [1, 0, 0], label: "other_half");
pattern(direction: [1, 0, 0], spacing: 25, count: 4, label: "row");
```

`mirror` reflects the current body across the plane and joins the two.
`pattern` repeats the current body `count` times along `direction` with the
given `spacing`; `count` includes the original.

### `union`, `difference`, `intersection`

```art
let plate = extrude(sketch: profile, distance: 6, label: "plate");
let boss = cylinder(center: [20, 10, 6], radius: 4, height: 10, label: "boss");
let joined = union(target: plate, tool: boss, label: "joined");
```

| Argument | Meaning |
|---|---|
| `target` | The step whose body is kept or cut. |
| `tool` | The step whose body is added, subtracted, or intersected. |

Both arguments are steps (bound names or their labels). The result becomes
the current body.

---

## 7. Selectors

A selector names one face or edge of the **current body** and is resolved
when the step that uses it runs, so it survives earlier steps changing the
topology.

### By direction: `faces("...")`

| Spelling | Means |
|---|---|
| `">Z"`, `"top"` | The face pointing most along +Z; among equals, the highest. |
| `"<Z"`, `"bottom"` | The face pointing most along −Z; among equals, the lowest. |
| `">X"`, `"right"`, `"<X"`, `"left"`, `">Y"`, `"back"`, `"<Y"`, `"front"` | The same for the other axes. |
| `"largest"`, `"smallest"` | By area. |
| `"planar"`, `"cylindrical"` | By surface type; must be unique. |

On a stepped part, `faces(">Z")` is the top step, not the plate under it.

### By direction: `edges("...")`

| Spelling | Means |
|---|---|
| `"|X"`, `"|Y"`, `"|Z"` | Every straight edge parallel to the axis (a set). |
| `"longest"`, `"shortest"` | By length. |

### By position: `nearest`

```art
nearest(point: [x, y, z])                  // the face whose surface passes closest to the point
nearest(point: [x, y, z], kind: "edge")    // the nearest edge
nearest(point: [x, y, z], kind: "vertex")
```

The distance is measured to the entity itself, so a point placed on a face
finds that face even when another face's centre is nearer. Choose points
that stay on the feature you mean after later cuts: a point on a hole's rim
finds the hole wall once the hole exists.

### By history: `step.face(...)`, `step.edge(...)`, `step.edges(...)`

```art
let b = box(size: [40, 30, 10], label: "b");
let top = b.face("top_face");
let one_edge = b.edge("edge", ordinal: 3);
let ring = b.edges("edge", count: 12);   // every edge the step made under that role
```

Roles are what the step reported when it ran. Boxes report the six face
roles listed under `box`; drills report `FeatureSide` walls and a
`FeatureEnd` floor; extrusions report their side and end faces. When a role
has several entities, `ordinal:` picks one and `.edges(count:)` lists them.

### Naming faces: `let name = <selector>`

```art
let flange_top = nearest(point: [pitch * cos(between), pitch * sin(between), flange_thickness]);
let hub_top = faces(">Z");
```

A top-level `let` bound to a selector is a **named face**. The name works in
the script wherever a selector does, and it is also reported to the host:
Script Studio resolves every such name against the finished body, lists
them in its FACES panel, and shows the name when the face is clicked in the
viewport, together with a description read off the geometry ("planar,
facing up, centre (0.4, 0.1, 8.0)"). Faces the script did not name are
listed by the step and role that made them, such as `hub.face[3]` or
`bolt_0.face_extrude.pocket.wall_face[2]`, which `hub.face("face", ordinal: 3)`
would select.

Name every face you expect to talk about. A name is resolved when it is
used, so it must still find the right face after every earlier step: put
the reference point of a `nearest` where later holes will not land.

---

## 8. Math

All positional. Angles in degrees.

| Function | Meaning |
|---|---|
| `sqrt(x)`, `abs(x)`, `floor(x)`, `ceil(x)`, `round(x)` | As usual. |
| `sin(a)`, `cos(a)`, `tan(a)` | `a` in degrees. |
| `asin(x)`, `acos(x)`, `atan(x)`, `atan2(y, x)` | Return degrees. |
| `pow(x, y)`, `hypot(x, y)` | Power and hypotenuse. |
| `min(a, b, ...)`, `max(a, b, ...)` | Any number of arguments. |
| `clamp(x, low, high)` | Bound. |
| `pi` | The constant. |

---

## 9. Reading the result

Each executed step reports: its label, the topology counts of the current
body (`V` vertices, `E` edges, `C` coedges, `L` loops, `F` faces, `Sh`
shells, `S` solids), the bounds, the entities it produced by role, any
diagnostics, and the time it took. A step that falls to the faceted tier
(a cut that crosses earlier geometry the exact ladder cannot yet handle)
carries an approximation warning; the model is still watertight, but its
faces are facets rather than analytic surfaces.

The kernel measures the final body's volume, surface area and centroid, and
exports it as binary or ASCII STL and as OBJ. Script Studio shows the volume,
area and size in its console.

---

## 10. Examples

### A bracket: plate, boss, bore, rounded corners

```art
param length: f64 = 60.0;
param width: f64 = 40.0;
param thickness: f64 = 6.0;
param boss_diameter: f64 = 16.0;
param boss_height: f64 = 12.0;
param bore_diameter: f64 = 8.0;

// The plate, as a sketch on the XY plane extruded upward.
let outline = sketch(on: "XY", label: "outline", entities: [
    rect(center: [0, 0], width: length, height: width),
]);
let plate = extrude(sketch: outline, distance: thickness, label: "plate");

// The boss grows from the plate's top: a sketch on that face, added.
let boss_profile = sketch(on: faces(">Z"), label: "boss_profile", entities: [
    circle(center: [0, 0], diameter: boss_diameter),
]);
let boss = extrude(sketch: boss_profile, distance: boss_height, operation: "add", label: "boss");

// The bore goes through boss and plate from the boss top.
drill(face: faces(">Z"), center: [0, 0], diameter: bore_diameter,
      depth: boss_height + thickness, label: "bore");

// Round the plate's vertical corners.
fillet(edges: [edges("|Z")], radius: 5, label: "corners");
```

### A flanged hub: a revolved section, drilled and bolted

```art
param hub_radius: f64 = 20.0;
param hub_height: f64 = 40.0;
param flange_radius: f64 = 45.0;
param flange_thickness: f64 = 8.0;
param bore_diameter: f64 = 12.0;
param bolt_diameter: f64 = 6.5;
param bolt_count: f64 = 4;

// One (r, z) section on the XZ plane, with the bore as its inner wall.
let bore_radius = bore_diameter / 2;
let section = sketch(on: "XZ", label: "section", entities: [
    line(start: [bore_radius, 0], end: [flange_radius, 0]),
    line(start: [flange_radius, 0], end: [flange_radius, flange_thickness]),
    line(start: [flange_radius, flange_thickness], end: [hub_radius, flange_thickness]),
    line(start: [hub_radius, flange_thickness], end: [hub_radius, hub_height]),
    line(start: [hub_radius, hub_height], end: [bore_radius, hub_height]),
    line(start: [bore_radius, hub_height], end: [bore_radius, 0]),
]);
let hub = revolve(sketch: section, axis: [0, 0, 1], label: "hub");

// Exact torus blends on the hub's rim and the bore's rim, before any hole
// is drilled: a round rim is two half-circle edges, so each names both.
fillet(edges: [nearest(point: [0, hub_radius, hub_height], kind: "edge"),
               nearest(point: [0, -hub_radius, hub_height], kind: "edge")],
       radius: 1.5, label: "hub_rim");
fillet(edges: [nearest(point: [0, bore_radius, hub_height], kind: "edge"),
               nearest(point: [0, -bore_radius, hub_height], kind: "edge")],
       radius: 1, label: "bore_rim");

// Named faces, for people and agents to refer to.
let hub_top = faces(">Z");
let flange_bottom = faces("<Z");
let pitch = (hub_radius + flange_radius) / 2;
let between = 180 / bolt_count;
let flange_top = nearest(point: [pitch * cos(between), pitch * sin(between), flange_thickness]);

// bolt_count bolt holes on a pitch circle, evenly spaced. The flange top
// is found by a point between two holes, so it stays findable as holes
// appear, whatever the count.
for i in 0..bolt_count {
    let angle = 360 * i / bolt_count;
    drill(face: flange_top, center: [pitch * cos(angle), pitch * sin(angle)],
          diameter: bolt_diameter, depth: flange_thickness, label: "bolt_" + i);
}
```

### Two bodies joined, then pocketed

```art
param size: f64 = 20.0;
let plate = sketch(on: "XY", label: "plate", entities: [
    rect(origin: [0, 0], width: size * 2, height: size),
]);
let base = extrude(sketch: plate, distance: 5, label: "base");
let boss = cylinder(center: [size, size / 2, 5], radius: 4, height: 10, label: "boss");
let joined = union(target: base, tool: boss, label: "joined");
let pocket = sketch(on: faces(">Z"), label: "pocket", entities: [circle(center: [0, 0], radius: 2)]);
extrude(sketch: pocket, distance: 6, operation: "cut", label: "pocket_cut");
```

---

## 11. Working with a person in the loop

The names in a script are the vocabulary a person and an agent share. The
intended workflow, with the flanged hub open in Script Studio:

1. The person clicks a face. The console shows its name, `flange_top`, and
   what it is: planar, facing up, centre at z = 8. The FACES panel lists
   every name the script gave, and every other face by the step that made
   it.
2. The person asks: *"increase the number of bolt holes on `flange_top` to
   six."*
3. The agent reads the script, finds the loop that drills on `flange_top`,
   and sees that its count is the parameter `bolt_count`. It changes
   `param bolt_count: f64 = 4;` to `6`, or, if the count were a literal,
   introduces the parameter. Nothing else moves: the reference point for
   `flange_top` is written in terms of `bolt_count`, so it still lands
   between holes.
4. Script Studio re-runs the script on save; the person sees six holes and
   the same face still named `flange_top`.

For an agent, the rules that make this reliable:

- Address faces by their script names. If a request names a face that has
  only a history name (`hub.face[3]`), first give it a script name with a
  `let` and a selector that will keep finding it, then use that name.
- Prefer changing a `param` to rewriting geometry; add a `param` when a
  request implies one ("how many", "how thick", "how far apart").
- Keep every label unique; in a loop, build labels from the loop variable.
- Preserve the order the script already has, particularly fillets before
  holes, and re-run before answering: the console names the failing step
  and line if the change did not build.

## 12. Writing scripts that work first time

- Give every step a label; make labels unique.
- Prefer a face sketch plus `operation: "add"` or `"cut"` for features on an
  existing body; use `union` only to join separately built bodies.
- Draw closed, non-crossing loops. Chain `line` segments end to end.
- Pick selector points that stay on the face or edge you mean after every
  earlier step has run. `faces(">Z")` means the highest upward face.
- For a fillet on a round rim, name both half-circle edges.
- Keep cuts inside the face you drill from; a cut that runs into other
  features may fall to the faceted tier and take much longer.
- Angles are degrees, everywhere.
- Test with `cargo run -p artificer-api-server -- run part.art`; the output
  names the failing step and why.

## 13. Not in 0.2

Partial revolves, sweeps and lofts between arbitrary sections, shells,
concave fillets between a boss and its plate, text as sketch geometry from a
script, threads, and assemblies. The workbench has several of these; the
scripting surface follows the kernel API as it grows.

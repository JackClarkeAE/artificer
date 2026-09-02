//! Exact STEP export: the file is checked as a B-rep in its own right,
//! against the kernel's own geometry, and, when a development machine
//! provides one, against an OpenCascade oracle.

use std::collections::{BTreeMap, HashMap};

use artificer_kernel::api::export::{export_step, export_step_faceted};
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{Point3, Vector3};

/// The fixture set: every surface kind, seams, a blend with toric and
/// spherical faces, a cone from a drafted extrusion, an elliptical edge
/// from an oblique cylinder section, and a body with a cavity.
fn fixtures() -> Vec<(&'static str, String)> {
    vec![
        (
            "block_with_hole",
            "let b = box(size: [40, 30, 20], label: \"b\");\ndrill(face: faces(\">Z\"), center: [5, 0], diameter: 8, depth: 20, label: \"hole\");\n".to_owned(),
        ),
        ("cylinder", "let c = cylinder(radius: 10, height: 30, label: \"c\");\n".to_owned()),
        (
            "flanged_hub",
            include_str!("../examples/flanged_hub.art").to_owned(),
        ),
        (
            "filleted_flange",
            include_str!("../examples/filleted_flange.art").to_owned(),
        ),
        (
            "filleted_cube",
            include_str!("../examples/filleted_cube.art").to_owned(),
        ),
        (
            "drafted_block",
            "let s = sketch(on: \"XY\", entities: [rect(width: 40, height: 30)], label: \"s\");\nlet d = extrude(sketch: s, distance: 20, draft: 10, label: \"d\");\n".to_owned(),
        ),
        (
            "drafted_post",
            "let s = sketch(on: \"XY\", entities: [circle(radius: 10)], label: \"s\");\nlet d = extrude(sketch: s, distance: 20, draft: 8, label: \"d\");\n".to_owned(),
        ),
        (
            "domed_post",
            "let c = cylinder(radius: 5, height: 10, label: \"c\");\nfillet(edges: [nearest(point: [5, 0, 10], kind: \"edge\"), nearest(point: [-5, 0, 10], kind: \"edge\")], radius: 5, label: \"dome\");\n".to_owned(),
        ),
        (
            "square_hole_rim",
            "let s = sketch(on: \"XY\", entities: [rect(origin: [0, 0], width: 40, height: 40), rect(origin: [15, 15], width: 10, height: 10)], label: \"s\");\nlet plate = extrude(sketch: s, distance: 10, label: \"plate\");\nfillet(edges: [nearest(point: [20, 15, 10], kind: \"edge\"), nearest(point: [25, 20, 10], kind: \"edge\"), nearest(point: [20, 25, 10], kind: \"edge\"), nearest(point: [15, 20, 10], kind: \"edge\")], radius: 2, label: \"rim\");\n".to_owned(),
        ),
        (
            "oblique_bore",
            "let s = sketch(on: \"XY\", entities: [rect(width: 60, height: 40)], label: \"s\");\nlet d = extrude(sketch: s, distance: 30, draft: 15, label: \"d\");\nlet side = sketch(on: faces(\">X\"), entities: [circle(center: [0, 0], diameter: 12)], label: \"side\");\nextrude(sketch: side, distance: 20, operation: \"cut\", label: \"bore\");\n".to_owned(),
        ),
        (
            "cavity",
            "let outer = box(size: [40, 40, 40], label: \"outer\");\nlet inner = box(origin: [10, 10, 10], size: [20, 20, 20], label: \"inner\");\ndifference(target: outer, tool: inner, label: \"hollow\");\n".to_owned(),
        ),
    ]
}

fn build(source: &str) -> Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

// ---------------------------------------------------------------------------
// A reader for the files this crate writes
// ---------------------------------------------------------------------------

/// One entity: its type name and raw argument list, with `#n` references
/// left as text.
#[derive(Clone, Debug)]
struct Entity {
    kind: String,
    args: Vec<String>,
}

fn parse(step: &str) -> HashMap<u64, Entity> {
    let data = step
        .split("DATA;")
        .nth(1)
        .expect("a DATA section")
        .split("ENDSEC;")
        .next()
        .unwrap();
    let mut entities = HashMap::new();
    for line in data.lines().filter(|line| line.starts_with('#')) {
        let (id, body) = line.split_once('=').expect("#id=");
        let id: u64 = id[1..].parse().unwrap();
        let body = body.trim_end_matches(';');
        // Complex entities `(A()B())` are opaque here.
        let (kind, rest) = if body.starts_with('(') {
            ("COMPLEX".to_owned(), String::new())
        } else {
            let open = body.find('(').unwrap();
            (
                body[..open].to_owned(),
                body[open + 1..body.len() - 1].to_owned(),
            )
        };
        entities.insert(
            id,
            Entity {
                kind,
                args: split_args(&rest),
            },
        );
    }
    entities
}

/// Splits a Part 21 argument list at top-level commas.
fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut current = String::new();
    for character in text.chars() {
        match character {
            '\'' => {
                quoted = !quoted;
                current.push(character);
            }
            '(' if !quoted => {
                depth += 1;
                current.push(character);
            }
            ')' if !quoted => {
                depth -= 1;
                current.push(character);
            }
            ',' if !quoted && depth == 0 => {
                args.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(character),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_owned());
    }
    args
}

fn reference(text: &str) -> u64 {
    text.trim().trim_start_matches('#').parse().unwrap()
}

fn references(list: &str) -> Vec<u64> {
    split_args(list.trim().trim_start_matches('(').trim_end_matches(')'))
        .iter()
        .filter(|item| !item.is_empty())
        .map(|item| reference(item))
        .collect()
}

fn triple(list: &str) -> [f64; 3] {
    let values: Vec<f64> = split_args(list.trim().trim_start_matches('(').trim_end_matches(')'))
        .iter()
        .map(|value| value.parse().unwrap())
        .collect();
    [values[0], values[1], values[2]]
}

struct Reader {
    entities: HashMap<u64, Entity>,
}

impl Reader {
    fn get(&self, id: u64) -> &Entity {
        self.entities
            .get(&id)
            .unwrap_or_else(|| panic!("#{id} is referenced but never defined"))
    }

    fn point(&self, id: u64) -> [f64; 3] {
        let entity = self.get(id);
        assert_eq!(entity.kind, "CARTESIAN_POINT");
        triple(&entity.args[1])
    }

    fn direction(&self, id: u64) -> [f64; 3] {
        let entity = self.get(id);
        assert_eq!(entity.kind, "DIRECTION");
        triple(&entity.args[1])
    }

    /// (origin, axis, reference) of an axis2_placement_3d.
    fn placement(&self, id: u64) -> ([f64; 3], [f64; 3], [f64; 3]) {
        let entity = self.get(id);
        assert_eq!(entity.kind, "AXIS2_PLACEMENT_3D");
        (
            self.point(reference(&entity.args[1])),
            self.direction(reference(&entity.args[2])),
            self.direction(reference(&entity.args[3])),
        )
    }

    fn vertex(&self, id: u64) -> [f64; 3] {
        let entity = self.get(id);
        assert_eq!(entity.kind, "VERTEX_POINT");
        self.point(reference(&entity.args[1]))
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn unit(a: [f64; 3]) -> [f64; 3] {
    let length = norm(a);
    [a[0] / length, a[1] / length, a[2] / length]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

const TOLERANCE: f64 = 1.0e-9;

/// Checks one exported file as a B-rep: references resolve, loops chain,
/// every edge is shared by exactly two faces with opposite senses, every
/// vertex lies on its edge's curve, and every face's `same_sense` agrees
/// with the kernel's outward normal at the face's centre.
fn check_brep(name: &str, snapshot: &Snapshot, step: &str) {
    assert!(step.starts_with("ISO-10303-21;\nHEADER;"), "{name}");
    assert!(
        step.contains("FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));"),
        "{name}"
    );
    assert!(step.ends_with("ENDSEC;\nEND-ISO-10303-21;\n"), "{name}");
    let reader = Reader {
        entities: parse(step),
    };
    // Every reference resolves.
    for entity in reader.entities.values() {
        for arg in &entity.args {
            for token in arg.split(|c: char| !(c.is_ascii_digit() || c == '#')) {
                if let Some(id) = token.strip_prefix('#')
                    && let Ok(id) = id.parse::<u64>()
                {
                    reader.get(id);
                }
            }
        }
    }
    let representation = reader
        .entities
        .values()
        .find(|entity| entity.kind == "ADVANCED_BREP_SHAPE_REPRESENTATION")
        .unwrap_or_else(|| panic!("{name}: no advanced_brep_shape_representation"));
    let items = references(&representation.args[1]);
    let solids: Vec<&Entity> = items
        .iter()
        .map(|id| reader.get(*id))
        .filter(|entity| entity.kind == "MANIFOLD_SOLID_BREP" || entity.kind == "BREP_WITH_VOIDS")
        .collect();
    assert_eq!(solids.len(), snapshot.counts().solids as usize, "{name}");

    let described = NativeKernel::describe_faces(snapshot);
    let mut faces_seen = 0;
    let mut edges_seen = 0;
    for solid in solids {
        let mut shells = vec![reference(&solid.args[1])];
        if solid.kind == "BREP_WITH_VOIDS" {
            for oriented in references(&solid.args[2]) {
                let oriented = reader.get(oriented);
                assert_eq!(oriented.kind, "ORIENTED_CLOSED_SHELL");
                assert_eq!(oriented.args[3], ".T.");
                shells.push(reference(&oriented.args[2]));
            }
        }
        for shell in shells {
            let shell = reader.get(shell);
            assert_eq!(shell.kind, "CLOSED_SHELL", "{name}");
            // Edge use across the shell: each edge exactly twice, once each
            // way.
            let mut uses: BTreeMap<u64, Vec<bool>> = BTreeMap::new();
            for face_id in references(&shell.args[1]) {
                let face = reader.get(face_id);
                assert_eq!(face.kind, "ADVANCED_FACE", "{name}");
                faces_seen += 1;
                let same_sense = face.args[3] == ".T.";
                let surface = reader.get(reference(&face.args[2]));
                // The kernel's outward normal at the face centre against the
                // STEP surface's own normal there.
                let centre = kernel_face_centre(&described, face_id, &reader, face);
                if let Some((centre, kernel_normal)) = centre {
                    let natural = natural_normal(&reader, surface, centre);
                    let agreement = dot(natural, kernel_normal);
                    assert!(
                        (agreement > 0.999) == same_sense && agreement.abs() > 0.999,
                        "{name}: face #{face_id} ({}) same_sense {same_sense} but normals agree {agreement:.6}",
                        surface.kind
                    );
                }
                for (index, bound_id) in references(&face.args[1]).iter().enumerate() {
                    let bound = reader.get(*bound_id);
                    assert_eq!(
                        bound.kind,
                        if index == 0 {
                            "FACE_OUTER_BOUND"
                        } else {
                            "FACE_BOUND"
                        },
                        "{name}"
                    );
                    let edge_loop = reader.get(reference(&bound.args[1]));
                    assert_eq!(edge_loop.kind, "EDGE_LOOP", "{name}");
                    let mut previous_end: Option<u64> = None;
                    let mut first_start: Option<u64> = None;
                    for oriented_id in references(&edge_loop.args[1]) {
                        let oriented = reader.get(oriented_id);
                        assert_eq!(oriented.kind, "ORIENTED_EDGE", "{name}");
                        let forward = oriented.args[4] == ".T.";
                        let edge_id = reference(&oriented.args[3]);
                        let edge = reader.get(edge_id);
                        assert_eq!(edge.kind, "EDGE_CURVE", "{name}");
                        uses.entry(edge_id).or_default().push(forward);
                        let (start, end) = (reference(&edge.args[1]), reference(&edge.args[2]));
                        let (start, end) = if forward { (start, end) } else { (end, start) };
                        if let Some(previous) = previous_end {
                            assert_eq!(previous, start, "{name}: loop #{bound_id} does not chain");
                        }
                        first_start.get_or_insert(start);
                        previous_end = Some(end);
                        check_edge_geometry(name, &reader, edge);
                        edges_seen += 1;
                    }
                    assert_eq!(
                        first_start, previous_end,
                        "{name}: loop #{bound_id} is open"
                    );
                }
            }
            for (edge, senses) in uses {
                assert_eq!(
                    senses.len(),
                    2,
                    "{name}: edge #{edge} used {} times",
                    senses.len()
                );
                assert_ne!(
                    senses[0], senses[1],
                    "{name}: edge #{edge} used the same way twice"
                );
            }
        }
    }
    assert_eq!(
        faces_seen,
        snapshot.counts().faces as usize,
        "{name}: faces"
    );
    // A revolved pole is a zero-length edge the kernel keeps and STEP has
    // no word for; every other edge is written and used twice.
    let real_edges = NativeKernel::edges(snapshot)
        .into_iter()
        .filter(|edge| {
            NativeKernel::describe_edge(snapshot, *edge).is_ok_and(|edge| edge.length > 1.0e-12)
        })
        .count();
    assert_eq!(edges_seen, 2 * real_edges, "{name}: coedges");
}

/// The kernel's centre and outward normal for the STEP face written at
/// `face_id`: faces are written in topology order, so the n-th
/// ADVANCED_FACE is the n-th kernel face.
fn kernel_face_centre(
    described: &BTreeMap<u64, artificer_kernel::FaceDescription>,
    _face_id: u64,
    reader: &Reader,
    face: &Entity,
) -> Option<([f64; 3], [f64; 3])> {
    // Match by geometry rather than by order: the surface placement origin
    // and kind identify the carrier; the centre is taken from the kernel
    // face whose carrier matches and whose outer loop's first vertex lies
    // on the face's first edge.
    let surface = reader.get(reference(&face.args[2]));
    let first_bound = reader.get(references(&face.args[1])[0]);
    let edge_loop = reader.get(reference(&first_bound.args[1]));
    let first_oriented = reader.get(references(&edge_loop.args[1])[0]);
    let first_edge = reader.get(reference(&first_oriented.args[3]));
    let start = reader.vertex(reference(&first_edge.args[1]));
    let kind = match surface.kind.as_str() {
        "PLANE" => "plane",
        "CYLINDRICAL_SURFACE" => "cylinder",
        "CONICAL_SURFACE" => "cone",
        "SPHERICAL_SURFACE" => "sphere",
        "TOROIDAL_SURFACE" => "torus",
        other => panic!("unexpected surface {other}"),
    };
    // The kernel faces whose carrier is this surface: same kind, centre on
    // the surface. Several faces can share a carrier (the two halves of a
    // cylinder, coplanar patches), so among those take the one whose
    // centre is nearest the loop's start vertex.
    assert!(
        on_surface(reader, surface, start),
        "the loop's first vertex lies on the face's surface"
    );
    let mut candidates: Vec<&artificer_kernel::FaceDescription> = described
        .values()
        .filter(|description| description.geometry.surface_kind() == kind)
        .filter(|description| {
            on_surface(
                reader,
                surface,
                [
                    description.centre.x,
                    description.centre.y,
                    description.centre.z,
                ],
            )
        })
        .collect();
    candidates.sort_by(|a, b| {
        let da = norm(sub([a.centre.x, a.centre.y, a.centre.z], start));
        let db = norm(sub([b.centre.x, b.centre.y, b.centre.z], start));
        da.partial_cmp(&db).unwrap()
    });
    let face = candidates.first()?;
    Some((
        [face.centre.x, face.centre.y, face.centre.z],
        [face.normal.x, face.normal.y, face.normal.z],
    ))
}

/// Whether a point lies on the STEP surface, within tolerance.
fn on_surface(reader: &Reader, surface: &Entity, point: [f64; 3]) -> bool {
    let (origin, axis, _) = reader.placement(reference(&surface.args[1]));
    let relative = sub(point, origin);
    match surface.kind.as_str() {
        "PLANE" => dot(relative, axis).abs() < 1.0e-6,
        "CYLINDRICAL_SURFACE" => {
            let radius: f64 = surface.args[2].parse().unwrap();
            let radial = sub(relative, scale(axis, dot(relative, axis)));
            (norm(radial) - radius).abs() < 1.0e-6
        }
        "CONICAL_SURFACE" => {
            let radius: f64 = surface.args[2].parse().unwrap();
            let semi_angle: f64 = surface.args[3].parse().unwrap();
            let height = dot(relative, axis);
            let radial = sub(relative, scale(axis, height));
            (norm(radial) - (radius + height * semi_angle.tan())).abs() < 1.0e-6
        }
        "SPHERICAL_SURFACE" => {
            let radius: f64 = surface.args[2].parse().unwrap();
            (norm(relative) - radius).abs() < 1.0e-6
        }
        "TOROIDAL_SURFACE" => {
            let major: f64 = surface.args[2].parse().unwrap();
            let minor: f64 = surface.args[3].parse().unwrap();
            let height = dot(relative, axis);
            let radial = sub(relative, scale(axis, height));
            let ring = norm(radial) - major;
            ((ring * ring + height * height).sqrt() - minor).abs() < 1.0e-6
        }
        _ => false,
    }
}

/// The STEP surface's own normal at a point on it.
fn natural_normal(reader: &Reader, surface: &Entity, point: [f64; 3]) -> [f64; 3] {
    let (origin, axis, _) = reader.placement(reference(&surface.args[1]));
    let relative = sub(point, origin);
    match surface.kind.as_str() {
        "PLANE" => axis,
        "CYLINDRICAL_SURFACE" => unit(sub(relative, scale(axis, dot(relative, axis)))),
        "CONICAL_SURFACE" => {
            let semi_angle: f64 = surface.args[3].parse().unwrap();
            let radial = unit(sub(relative, scale(axis, dot(relative, axis))));
            unit(sub(radial, scale(axis, semi_angle.tan())))
        }
        "SPHERICAL_SURFACE" => unit(relative),
        "TOROIDAL_SURFACE" => {
            let major: f64 = surface.args[2].parse().unwrap();
            let height = dot(relative, axis);
            let radial = unit(sub(relative, scale(axis, height)));
            unit(sub(relative, scale(radial, major)))
        }
        other => panic!("unexpected surface {other}"),
    }
}

/// Both vertices of an edge lie on its curve.
fn check_edge_geometry(name: &str, reader: &Reader, edge: &Entity) {
    let start = reader.vertex(reference(&edge.args[1]));
    let end = reader.vertex(reference(&edge.args[2]));
    let curve = reader.get(reference(&edge.args[3]));
    match curve.kind.as_str() {
        "LINE" => {
            let origin = reader.point(reference(&curve.args[1]));
            let vector = reader.get(reference(&curve.args[2]));
            let direction = reader.direction(reference(&vector.args[1]));
            let magnitude: f64 = vector.args[2].parse().unwrap();
            assert!(norm(sub(start, origin)) < TOLERANCE, "{name}: line start");
            let far = [
                origin[0] + direction[0] * magnitude,
                origin[1] + direction[1] * magnitude,
                origin[2] + direction[2] * magnitude,
            ];
            assert!(norm(sub(end, far)) < 1.0e-6, "{name}: line end");
        }
        "CIRCLE" => {
            let (centre, axis, _) = reader.placement(reference(&curve.args[1]));
            let radius: f64 = curve.args[2].parse().unwrap();
            for vertex in [start, end] {
                let relative = sub(vertex, centre);
                assert!(
                    (norm(relative) - radius).abs() < 1.0e-6,
                    "{name}: circle radius"
                );
                assert!(dot(relative, axis).abs() < 1.0e-6, "{name}: circle plane");
            }
        }
        "ELLIPSE" => {
            let (centre, axis, reference_direction) = reader.placement(reference(&curve.args[1]));
            let a: f64 = curve.args[2].parse().unwrap();
            let b: f64 = curve.args[3].parse().unwrap();
            let minor_direction = cross(axis, reference_direction);
            for vertex in [start, end] {
                let relative = sub(vertex, centre);
                let x = dot(relative, reference_direction) / a;
                let y = dot(relative, minor_direction) / b;
                assert!(
                    (x * x + y * y - 1.0).abs() < 1.0e-6,
                    "{name}: ellipse equation"
                );
                assert!(dot(relative, axis).abs() < 1.0e-6, "{name}: ellipse plane");
            }
        }
        other => panic!("{name}: unexpected curve {other}"),
    }
}

#[test]
fn every_fixture_exports_as_a_closed_manifold_brep_with_the_kernel_orientation() {
    let mut kinds = std::collections::BTreeSet::new();
    let mut curves = std::collections::BTreeSet::new();
    let mut voids = 0;
    for (name, source) in fixtures() {
        let snapshot = build(&source);
        let step = export_step(&snapshot, name).unwrap_or_else(|error| panic!("{name}: {error}"));
        check_brep(name, &snapshot, &step);
        for entity in parse(&step).values() {
            match entity.kind.as_str() {
                "PLANE"
                | "CYLINDRICAL_SURFACE"
                | "CONICAL_SURFACE"
                | "SPHERICAL_SURFACE"
                | "TOROIDAL_SURFACE" => {
                    kinds.insert(entity.kind.clone());
                }
                "LINE" | "CIRCLE" | "ELLIPSE" => {
                    curves.insert(entity.kind.clone());
                }
                "BREP_WITH_VOIDS" => voids += 1,
                _ => {}
            }
        }
    }
    // The fixture set covers the whole vocabulary.
    assert_eq!(kinds.len(), 5, "{kinds:?}");
    assert_eq!(curves.len(), 3, "{curves:?}");
    assert!(voids >= 1, "a cavity fixture");
}

#[test]
fn the_faceted_variant_wraps_the_display_triangles() {
    let snapshot = build("let c = cylinder(radius: 10, height: 30, label: \"c\");\n");
    let step = export_step_faceted(&snapshot, "c");
    assert!(step.contains("MANIFOLD_SURFACE_SHAPE_REPRESENTATION"));
    let reader = Reader {
        entities: parse(&step),
    };
    let faces = reader
        .entities
        .values()
        .filter(|entity| entity.kind == "FACE")
        .count();
    assert_eq!(faces, NativeKernel::debug_scene(&snapshot).triangles.len());
    let _ = (Point3::new(0.0, 0.0, 0.0), Vector3::new(0.0, 0.0, 1.0));
}

/// The development oracle: `ARTIFICER_STEP_ORACLE` names a command that
/// prints `{"solids", "valid", "volume", "area"}` for a STEP file (see
/// `tools/oracle-occt/step_measure.py`). Without it the test says so and
/// passes; with it, every fixture's imported volume and area must agree
/// with the kernel's exact measures to 1e-9 relative.
#[test]
fn the_occt_oracle_agrees_with_the_kernel_measures() {
    let Ok(oracle) = std::env::var("ARTIFICER_STEP_ORACLE") else {
        eprintln!("ARTIFICER_STEP_ORACLE is not set; skipping the OCCT oracle round trip");
        return;
    };
    let directory =
        std::env::temp_dir().join(format!("artificer-step-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, source) in fixtures() {
        let snapshot = build(&source);
        let path = directory.join(format!("{name}.step"));
        std::fs::write(&path, export_step(&snapshot, name).unwrap()).unwrap();
        let mut parts = oracle.split_whitespace();
        let program = parts.next().unwrap();
        let output = std::process::Command::new(program)
            .args(parts)
            .arg(&path)
            .output()
            .unwrap_or_else(|error| panic!("{name}: could not run the oracle: {error}"));
        let text = String::from_utf8_lossy(&output.stdout);
        let report: serde_json::Value = serde_json::from_str(text.trim())
            .unwrap_or_else(|error| panic!("{name}: oracle output {text:?}: {error}"));
        assert!(report["valid"].as_bool() == Some(true), "{name}: {report}");
        assert_eq!(
            report["solids"].as_u64(),
            Some(snapshot.counts().solids),
            "{name}: {report}"
        );
        let measures = snapshot.measures();
        let volume = report["volume"].as_f64().unwrap();
        let area = report["area"].as_f64().unwrap();
        assert!(
            ((volume - measures.volume) / measures.volume).abs() < TOLERANCE,
            "{name}: volume {volume} vs {}",
            measures.volume
        );
        assert!(
            ((area - measures.surface_area) / measures.surface_area).abs() < TOLERANCE,
            "{name}: area {area} vs {}",
            measures.surface_area
        );
    }
}

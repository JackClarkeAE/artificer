//! STEP import (ADR 0056, Track I): the exporter's files read back into the
//! kernel's own B-rep, files in the style other systems write read exactly,
//! and every refusal named.

use std::collections::BTreeMap;

use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::export::export_step;
use artificer_kernel::api::selectors::EntitySelector;
use artificer_kernel::api::server::SharedSession;
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityKind, ExecuteRequest, KernelCommand,
    PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy, RequestId, SolidOperation, SweepOrientation, SweepPath3, SweepSegment3, Tier,
    ValidationProfile, Vector3,
};
use artificer_step::{Writer, ids, quoted, real};

const RELATIVE: f64 = 1.0e-9;

fn build(source: &str) -> Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

/// Imports STEP text into a fresh session and returns the session with the
/// import committed as step `part`.
fn import(text: &str) -> Session {
    let mut session = Session::new();
    let result = session
        .import_step_text("part", text, &CancellationToken::default())
        .unwrap_or_else(|error| panic!("import failed: {error:?}"));
    assert!(result.success, "{result:?}");
    session
}

fn assert_relative(actual: f64, expected: f64, what: &str) {
    assert!(
        ((actual - expected) / expected).abs() < RELATIVE,
        "{what}: {actual} should be {expected} (relative {:.3e})",
        ((actual - expected) / expected).abs()
    );
}

/// Every face of `original` has a face of `imported` with the same carrier
/// kind, area and centre; the file carries faces, not their order.
fn assert_faces_agree(original: &Snapshot, imported: &Snapshot, name: &str) {
    let expected = NativeKernel::describe_faces(original);
    let mut found: Vec<_> = NativeKernel::describe_faces(imported)
        .into_values()
        .collect();
    for face in expected.values() {
        let scale = face.area.abs().max(1.0);
        let position = found.iter().position(|candidate| {
            candidate.geometry.surface_kind() == face.geometry.surface_kind()
                && (candidate.area - face.area).abs() <= 1.0e-8 * scale
                && (candidate.centre.x - face.centre.x).abs() <= 1.0e-7
                && (candidate.centre.y - face.centre.y).abs() <= 1.0e-7
                && (candidate.centre.z - face.centre.z).abs() <= 1.0e-7
                && (candidate.normal.x - face.normal.x).abs() <= 1.0e-7
                && (candidate.normal.y - face.normal.y).abs() <= 1.0e-7
                && (candidate.normal.z - face.normal.z).abs() <= 1.0e-7
        });
        let Some(position) = position else {
            panic!("{name}: no imported face matches {}", face.summary);
        };
        found.remove(position);
    }
    assert!(
        found.is_empty(),
        "{name}: imported faces nobody wrote: {found:?}"
    );
}

/// Export → import → export → import: the imported body validates, matches
/// the original's measures and faces, and re-exports to the same body.
fn round_trip(name: &str, original: &Snapshot) -> Session {
    let text = export_step(original, name).unwrap_or_else(|error| panic!("{name}: {error}"));
    let session = import(&text);
    let imported = &session.snapshot;
    assert!(
        NativeKernel::validate(imported, ValidationProfile::Solid).valid,
        "{name}: the imported body does not validate"
    );
    assert_eq!(
        session.tier(),
        Tier::Exact,
        "{name}: an exact file imports exactly"
    );
    let report = &session.step_reports["part"];
    assert_eq!(report.rung.as_deref(), Some("step-import/exact"), "{name}");
    assert!(report.warnings.is_empty(), "{name}: {:?}", report.warnings);
    let (expected, actual) = (original.measures(), imported.measures());
    assert_relative(actual.volume, expected.volume, &format!("{name}: volume"));
    assert_relative(
        actual.surface_area,
        expected.surface_area,
        &format!("{name}: area"),
    );
    assert_eq!(
        imported.counts().solids,
        original.counts().solids,
        "{name}: solids"
    );
    assert_eq!(
        imported.counts().faces,
        original.counts().faces,
        "{name}: faces"
    );
    assert_eq!(
        imported.counts().edges,
        original.counts().edges,
        "{name}: edges"
    );
    assert_eq!(
        imported.counts().vertices,
        original.counts().vertices,
        "{name}: vertices"
    );
    assert_eq!(
        NativeKernel::surface_counts(imported),
        NativeKernel::surface_counts(original),
        "{name}: surface kinds"
    );
    assert_faces_agree(original, imported, name);
    // The imported body's own export reads back to the same body: the
    // conforming stage is a fixed point.
    let again = export_step(imported, name).unwrap();
    let second = import(&again);
    assert_eq!(
        second.snapshot.semantic_digest(),
        imported.semantic_digest(),
        "{name}: import → export → import is digest-stable"
    );
    session
}

// ---------------------------------------------------------------------------
// I1: the exporter's files parse back into an entity graph
// ---------------------------------------------------------------------------

fn fixtures() -> Vec<(&'static str, String)> {
    vec![
        ("box", "let b = box(size: [40, 30, 20], label: \"b\");\n".to_owned()),
        ("cylinder", "let c = cylinder(radius: 10, height: 30, label: \"c\");\n".to_owned()),
        (
            "drilled_filleted_block",
            "let b = box(size: [40, 30, 20], label: \"b\");\ndrill(face: faces(\">Z\"), center: [5, 0], diameter: 8, depth: 20, label: \"hole\");\nfillet(edges: [nearest(point: [20, 0, 20], kind: \"edge\"), nearest(point: [20, 30, 20], kind: \"edge\")], radius: 3, label: \"round\");\n".to_owned(),
        ),
        ("flanged_hub", include_str!("../examples/flanged_hub.art").to_owned()),
        ("filleted_flange", include_str!("../examples/filleted_flange.art").to_owned()),
        (
            "spline_extrusion",
            "let s = sketch(on: \"XY\", entities: [spline(points: [[20, 0], [5, 14], [-18, 6], [-12, -12], [8, -15]], closed: true)], label: \"s\");\nlet e = extrude(sketch: s, distance: 12, label: \"e\");\n".to_owned(),
        ),
        (
            "smooth_loft",
            "let a = sketch(on: \"XY\", entities: [rect(width: 20, height: 20)], label: \"a\");\nlet b = sketch(on: plane(from: \"XY\", offset: 15), entities: [spline(points: [[-9, -8], [9, -8], [9, 8], [-9, 8]], closed: true)], label: \"b\");\nlet c = sketch(on: plane(from: \"XY\", offset: 30), entities: [circle(radius: 6)], label: \"c\");\nlet l = loft(sections: [a, b, c], label: \"l\");\n".to_owned(),
        ),
        ("cavity", "let outer = box(size: [40, 40, 40], label: \"outer\");\nlet inner = box(origin: [10, 10, 10], size: [20, 20, 20], label: \"inner\");\ndifference(target: outer, tool: inner, label: \"hollow\");\n".to_owned()),
        (
            "domed_post",
            "let c = cylinder(radius: 5, height: 10, label: \"c\");\nfillet(edges: [nearest(point: [5, 0, 10], kind: \"edge\"), nearest(point: [-5, 0, 10], kind: \"edge\")], radius: 5, label: \"dome\");\n".to_owned(),
        ),
        (
            "drafted_post",
            "let s = sketch(on: \"XY\", entities: [circle(radius: 10)], label: \"s\");\nlet d = extrude(sketch: s, distance: 20, draft: 8, label: \"d\");\n".to_owned(),
        ),
        (
            "oblique_bore",
            "let s = sketch(on: \"XY\", entities: [rect(width: 60, height: 40)], label: \"s\");\nlet d = extrude(sketch: s, distance: 30, draft: 15, label: \"d\");\nlet side = sketch(on: faces(\">X\"), entities: [circle(center: [0, 0], diameter: 12)], label: \"side\");\nextrude(sketch: side, distance: 20, operation: \"cut\", label: \"bore\");\n".to_owned(),
        ),
        (
            "square_hole_rim",
            "let s = sketch(on: \"XY\", entities: [rect(origin: [0, 0], width: 40, height: 40), rect(origin: [15, 15], width: 10, height: 10)], label: \"s\");\nlet plate = extrude(sketch: s, distance: 10, label: \"plate\");\nfillet(edges: [nearest(point: [20, 15, 10], kind: \"edge\"), nearest(point: [25, 20, 10], kind: \"edge\"), nearest(point: [20, 25, 10], kind: \"edge\"), nearest(point: [15, 20, 10], kind: \"edge\")], radius: 2, label: \"rim\");\n".to_owned(),
        ),
    ]
}

fn fixture(name: &str) -> Snapshot {
    let (_, source) = fixtures()
        .into_iter()
        .find(|(fixture, _)| *fixture == name)
        .unwrap();
    build(&source)
}

/// A sphere with both poles reached from a fillet's dome: pole edges and
/// meridians, and every one of them a seam the import lays down itself.
#[test]
fn a_domed_post_with_sphere_poles_round_trips() {
    let original = fixture("domed_post");
    assert!(NativeKernel::surface_counts(&original).spheres >= 2);
    round_trip("domed_post", &original);
}

#[test]
fn a_drafted_post_with_a_cone_round_trips() {
    let original = fixture("drafted_post");
    assert!(NativeKernel::surface_counts(&original).cones >= 2);
    round_trip("drafted_post", &original);
}

/// A bore square into a drafted wall: a cylinder meeting slanted planes.
#[test]
fn a_bore_through_drafted_walls_round_trips() {
    let original = fixture("oblique_bore");
    assert!(NativeKernel::surface_counts(&original).planes >= 7);
    round_trip("oblique_bore", &original);
}

/// A cube with every edge filleted: twelve cylinders meeting in eight
/// spherical corners, each a patch of a sphere with a pole.
#[test]
fn a_filleted_cube_with_spherical_corners_round_trips() {
    let original = build(include_str!("../examples/filleted_cube.art"));
    assert_eq!(NativeKernel::surface_counts(&original).spheres, 8);
    round_trip("filleted_cube", &original);
}

/// The rim fillet of a square hole mitres at the corners along ellipses
/// (the seams of two equal cylinders), which sit on the fillet cylinders
/// as harmonic pcurves.
#[test]
fn a_square_hole_rim_blend_with_elliptical_mitres_round_trips() {
    let original = fixture("square_hole_rim");
    let elliptical = NativeKernel::edges(&original)
        .into_iter()
        .filter(|edge| {
            NativeKernel::describe_edge(&original, *edge)
                .is_ok_and(|edge| edge.geometry.curve_kind() == "ellipse")
        })
        .count();
    assert!(elliptical >= 4, "the fixture has elliptical edges");
    round_trip("square_hole_rim", &original);
}

/// Two cylinders crossing meet in the quartic ADR 0047 writes as an
/// `INTERSECTION_CURVE` over a spline: no exact carrier this slice reads,
/// so the part opens as a reference mesh that names the faces.
#[test]
fn crossing_bores_fall_to_the_reference_mesh_by_name() {
    let original = build(include_str!("../examples/three_holes_and_cut.art"));
    let traced = NativeKernel::edges(&original)
        .into_iter()
        .filter(|edge| {
            NativeKernel::describe_edge(&original, *edge)
                .is_ok_and(|edge| edge.geometry.curve_kind() == "trace")
        })
        .count();
    assert!(traced >= 1, "the fixture has a cylinder-cylinder trace");
    let text = export_step(&original, "crossing").unwrap();
    let session = import(&text);
    let report = &session.step_reports["part"];
    assert_eq!(session.tier(), Tier::Approximate);
    assert_eq!(report.rung.as_deref(), Some("step-import/faceted"));
    let codes: Vec<&str> = report
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    assert!(codes.contains(&"STEP_FACE_UNSUPPORTED"), "{codes:?}");
    assert!(codes.contains(&"STEP_FACETED_APPROXIMATION"), "{codes:?}");
    // The label carries how far the mesh may stand off the part: its chord.
    let label = report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "STEP_FACETED_APPROXIMATION")
        .expect("the label");
    let deviation = label.measurement.as_ref().expect("a measured deviation");
    assert!(
        deviation.measured > 0.0 && deviation.measured < 1.0,
        "{deviation:?}"
    );
    // The mesh is the part to within its chord: a per-mille of the volume.
    let (expected, actual) = (
        original.measures().volume,
        session.snapshot.measures().volume,
    );
    assert!(
        ((actual - expected) / expected).abs() < 5.0e-3,
        "reference volume {actual} against exact {expected}"
    );
}

#[test]
fn every_exported_file_parses_back_into_an_entity_graph_with_the_expected_counts() {
    for (name, source) in fixtures() {
        let snapshot = build(&source);
        let text = export_step(&snapshot, name).unwrap();
        let file = artificer_step::parse(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(file.header.application_protocol(), Some(214), "{name}");
        assert!(file.graph.dangling_references().is_empty(), "{name}");
        assert_eq!(file.units.length_to_mm, 1.0, "{name}");
        assert_eq!(file.units.uncertainty_mm, Some(1.0e-6), "{name}");
        let counts = snapshot.counts();
        assert_eq!(
            file.graph.count("ADVANCED_FACE"),
            counts.faces as usize,
            "{name}: faces"
        );
        assert_eq!(
            file.graph.count("MANIFOLD_SOLID_BREP") + file.graph.count("BREP_WITH_VOIDS"),
            counts.solids as usize,
            "{name}: solids"
        );
        let real_edges = NativeKernel::edges(&snapshot)
            .into_iter()
            .filter(|edge| {
                NativeKernel::describe_edge(&snapshot, *edge)
                    .is_ok_and(|edge| edge.length > 1.0e-12)
            })
            .count();
        assert_eq!(file.graph.count("EDGE_CURVE"), real_edges, "{name}: edges");
        assert_eq!(
            file.graph.count("ORIENTED_EDGE"),
            2 * real_edges,
            "{name}: coedges"
        );
        assert_eq!(
            file.graph.count("CLOSED_SHELL"),
            counts.shells as usize,
            "{name}: shells"
        );
        // Every loop of the file is a bound of exactly one face.
        let bounds = file.graph.count("FACE_OUTER_BOUND") + file.graph.count("FACE_BOUND");
        assert_eq!(bounds, counts.loops as usize, "{name}: loops");
    }
}

// ---------------------------------------------------------------------------
// I2: export → import for the kernel's own bodies
// ---------------------------------------------------------------------------

#[test]
fn a_box_round_trips_exactly() {
    let original = build("let b = box(size: [40, 30, 20], label: \"b\");\n");
    let session = round_trip("box", &original);
    assert_relative(session.snapshot.measures().volume, 24_000.0, "volume");
}

#[test]
fn a_cylinder_round_trips_through_its_two_half_faces() {
    let original = build("let c = cylinder(radius: 10, height: 30, label: \"c\");\n");
    let session = round_trip("cylinder", &original);
    assert_relative(
        session.snapshot.measures().volume,
        std::f64::consts::PI * 100.0 * 30.0,
        "volume",
    );
    assert_eq!(NativeKernel::surface_counts(&session.snapshot).cylinders, 2);
}

#[test]
fn a_drilled_and_filleted_block_round_trips() {
    let (_, source) = fixtures()
        .into_iter()
        .find(|(name, _)| *name == "drilled_filleted_block")
        .unwrap();
    let original = build(&source);
    let session = round_trip("drilled_filleted_block", &original);
    let counts = NativeKernel::surface_counts(&session.snapshot);
    assert!(
        counts.cylinders >= 4,
        "the bore's halves and the two rounds: {counts:?}"
    );
}

#[test]
fn a_revolved_hub_with_a_torus_rim_round_trips() {
    let original = build(include_str!("../examples/filleted_flange.art"));
    let counts = NativeKernel::surface_counts(&original);
    assert!(counts.tori >= 1, "the fixture has a toric rim: {counts:?}");
    round_trip("filleted_flange", &original);
    let hub = build(include_str!("../examples/flanged_hub.art"));
    round_trip("flanged_hub", &hub);
}

#[test]
fn a_smooth_loft_with_bspline_walls_round_trips() {
    let (_, source) = fixtures()
        .into_iter()
        .find(|(name, _)| *name == "smooth_loft")
        .unwrap();
    let original = build(&source);
    assert!(NativeKernel::surface_counts(&original).bspline >= 4);
    round_trip("smooth_loft", &original);
}

#[test]
fn a_spline_extrusion_round_trips() {
    let (_, source) = fixtures()
        .into_iter()
        .find(|(name, _)| *name == "spline_extrusion")
        .unwrap();
    let original = build(&source);
    assert!(NativeKernel::surface_counts(&original).bspline >= 1);
    round_trip("spline_extrusion", &original);
}

#[test]
fn a_body_with_a_cavity_round_trips_as_a_brep_with_voids() {
    let (_, source) = fixtures()
        .into_iter()
        .find(|(name, _)| *name == "cavity")
        .unwrap();
    let original = build(&source);
    let text = export_step(&original, "cavity").unwrap();
    assert!(text.contains("BREP_WITH_VOIDS"));
    let session = round_trip("cavity", &original);
    assert_relative(
        session.snapshot.measures().volume,
        64_000.0 - 8_000.0,
        "volume",
    );
}

fn swept(segments: Vec<SweepSegment3>) -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 4.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    // The profile lies in the YZ plane at the origin and sweeps along +X.
    let frame = PlanarFrame3::new(
        Point3::new(0.0, 0.0, 0.0),
        Vector3::new(0.0, 1.0, 0.0),
        Vector3::new(0.0, 0.0, 1.0),
    );
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("pipe"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::SweepPlanarProfile {
            frame,
            profile,
            path: SweepPath3 { segments },
            orientation: SweepOrientation::RotationMinimising,
            operation: SolidOperation::New,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .unwrap_or_else(|error| panic!("{error:?}"))
        .snapshot
}

/// A circle swept a quarter turn about an axis in its plane: the exact
/// sweep (ADR 0055), a torus band between two planar caps.
#[test]
fn a_swept_pipe_bend_round_trips() {
    let bend = swept(vec![SweepSegment3::Arc {
        center: Point3::new(0.0, 20.0, 0.0),
        start: Point3::new(0.0, 0.0, 0.0),
        normal: Vector3::new(0.0, 0.0, 1.0),
        sweep: std::f64::consts::FRAC_PI_2,
    }]);
    let counts = NativeKernel::surface_counts(&bend);
    assert!(counts.tori >= 1, "{counts:?}");
    let session = round_trip("pipe_bend", &bend);
    // Pappus: the disk's area times the path its centroid travels.
    assert_relative(
        session.snapshot.measures().volume,
        std::f64::consts::PI * 16.0 * (20.0 * std::f64::consts::FRAC_PI_2),
        "volume by Pappus",
    );
}

/// A circle swept along a line and then a quarter turn: the skinned sweep,
/// whose walls are B-spline surfaces within a stated tolerance. The file
/// carries those surfaces exactly, so the import is exact of what was
/// written, and the walls' iso-line edges conform (ADR 0050).
#[test]
fn a_skinned_sweep_with_bspline_walls_round_trips() {
    let pipe = swept(vec![
        SweepSegment3::Line {
            start: Point3::new(0.0, 0.0, 0.0),
            end: Point3::new(30.0, 0.0, 0.0),
        },
        SweepSegment3::Arc {
            center: Point3::new(30.0, 20.0, 0.0),
            start: Point3::new(30.0, 0.0, 0.0),
            normal: Vector3::new(0.0, 0.0, 1.0),
            sweep: std::f64::consts::FRAC_PI_2,
        },
    ]);
    let counts = NativeKernel::surface_counts(&pipe);
    assert!(counts.bspline >= 1, "{counts:?}");
    round_trip("skinned_pipe", &pipe);
}

// ---------------------------------------------------------------------------
// Files in the style other systems write
// ---------------------------------------------------------------------------

/// A Part 21 file under construction the way OCCT and SolidWorks lay one
/// out: shared vertices and edges, loops of oriented edges, one seam on
/// each periodic face.
struct Part {
    writer: Writer,
    unit_scale: f64,
    inches: bool,
}

impl Part {
    fn new(inches: bool) -> Self {
        Self {
            writer: Writer::new(),
            unit_scale: if inches { 25.4 } else { 1.0 },
            inches,
        }
    }

    fn point(&mut self, p: [f64; 3]) -> u64 {
        let s = self.unit_scale;
        self.writer.entity(&format!(
            "CARTESIAN_POINT('',({},{},{}))",
            real(p[0] / s),
            real(p[1] / s),
            real(p[2] / s)
        ))
    }

    fn direction(&mut self, d: [f64; 3]) -> u64 {
        self.writer.entity(&format!(
            "DIRECTION('',({},{},{}))",
            real(d[0]),
            real(d[1]),
            real(d[2])
        ))
    }

    fn placement(&mut self, origin: [f64; 3], axis: [f64; 3], reference: [f64; 3]) -> u64 {
        let origin = self.point(origin);
        let axis = self.direction(axis);
        let reference = self.direction(reference);
        self.writer.entity(&format!(
            "AXIS2_PLACEMENT_3D('',#{origin},#{axis},#{reference})"
        ))
    }

    fn vertex(&mut self, p: [f64; 3]) -> u64 {
        let point = self.point(p);
        self.writer.entity(&format!("VERTEX_POINT('',#{point})"))
    }

    fn line_edge(&mut self, start: u64, end: u64, from: [f64; 3], to: [f64; 3]) -> u64 {
        let origin = self.point(from);
        let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
        let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let direction = self.direction([d[0] / length, d[1] / length, d[2] / length]);
        let vector = self.writer.entity(&format!(
            "VECTOR('',#{direction},{})",
            real(length / self.unit_scale)
        ));
        let line = self.writer.entity(&format!("LINE('',#{origin},#{vector})"));
        self.writer
            .entity(&format!("EDGE_CURVE('',#{start},#{end},#{line},.T.)"))
    }

    fn circle_edge(
        &mut self,
        start: u64,
        end: u64,
        center: [f64; 3],
        axis: [f64; 3],
        reference: [f64; 3],
        radius: f64,
    ) -> u64 {
        let placement = self.placement(center, axis, reference);
        let circle = self.writer.entity(&format!(
            "CIRCLE('',#{placement},{})",
            real(radius / self.unit_scale)
        ));
        self.writer
            .entity(&format!("EDGE_CURVE('',#{start},#{end},#{circle},.T.)"))
    }

    /// A full circle as the exact rational B-spline every NURBS kernel
    /// spells it with: nine control points and weights of one and √2/2.
    fn rational_circle_edge(&mut self, vertex: u64, center: [f64; 3], radius: f64) -> u64 {
        let r = radius;
        let corners: [[f64; 2]; 9] = [
            [r, 0.0],
            [r, r],
            [0.0, r],
            [-r, r],
            [-r, 0.0],
            [-r, -r],
            [0.0, -r],
            [r, -r],
            [r, 0.0],
        ];
        let points: Vec<u64> = corners
            .iter()
            .map(|[x, y]| self.point([center[0] + x, center[1] + y, center[2]]))
            .collect();
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let weights: Vec<String> = [1.0, w, 1.0, w, 1.0, w, 1.0, w, 1.0]
            .iter()
            .map(|w| real(*w))
            .collect();
        let curve = self.writer.entity(&format!(
            "(BOUNDED_CURVE()B_SPLINE_CURVE(2,({}),.UNSPECIFIED.,.T.,.F.)B_SPLINE_CURVE_WITH_KNOTS((3,2,2,2,3),(0.,1.,2.,3.,4.),.UNSPECIFIED.)CURVE()GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_CURVE(({}))REPRESENTATION_ITEM(''))",
            ids(&points),
            weights.join(",")
        ));
        self.writer
            .entity(&format!("EDGE_CURVE('',#{vertex},#{vertex},#{curve},.T.)"))
    }

    fn oriented(&mut self, edge: u64, forward: bool) -> u64 {
        self.writer.entity(&format!(
            "ORIENTED_EDGE('',*,*,#{edge},{})",
            if forward { ".T." } else { ".F." }
        ))
    }

    fn edge_loop(&mut self, uses: &[(u64, bool)]) -> u64 {
        let oriented: Vec<u64> = uses
            .iter()
            .map(|(edge, forward)| self.oriented(*edge, *forward))
            .collect();
        self.writer
            .entity(&format!("EDGE_LOOP('',({}))", ids(&oriented)))
    }

    fn bound(&mut self, edge_loop: u64, outer: bool) -> u64 {
        self.writer.entity(&format!(
            "{}('',#{edge_loop},.T.)",
            if outer {
                "FACE_OUTER_BOUND"
            } else {
                "FACE_BOUND"
            }
        ))
    }

    fn plane(&mut self, origin: [f64; 3], normal: [f64; 3], reference: [f64; 3]) -> u64 {
        let placement = self.placement(origin, normal, reference);
        self.writer.entity(&format!("PLANE('',#{placement})"))
    }

    fn face(&mut self, bounds: &[u64], surface: u64, same_sense: bool) -> u64 {
        self.writer.entity(&format!(
            "ADVANCED_FACE('',({}),#{surface},{})",
            ids(bounds),
            if same_sense { ".T." } else { ".F." }
        ))
    }

    fn solid(&mut self, faces: &[u64], name: &str) -> u64 {
        let shell = self
            .writer
            .entity(&format!("CLOSED_SHELL('',({}))", ids(faces)));
        self.writer
            .entity(&format!("MANIFOLD_SOLID_BREP({},#{shell})", quoted(name)))
    }

    fn finish(mut self, solid: u64, ap242: bool) -> String {
        let (length, angle) = if self.inches {
            let mm = self
                .writer
                .entity("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))");
            let measure = self.writer.entity(&format!(
                "LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#{mm})"
            ));
            let exponents = self
                .writer
                .entity("DIMENSIONAL_EXPONENTS(1.,0.,0.,0.,0.,0.,0.)");
            let inch = self.writer.entity(&format!(
                "(CONVERSION_BASED_UNIT('INCH',#{measure})LENGTH_UNIT()NAMED_UNIT(#{exponents}))"
            ));
            let radian = self
                .writer
                .entity("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))");
            let degree_measure = self.writer.entity(&format!(
                "PLANE_ANGLE_MEASURE_WITH_UNIT(PLANE_ANGLE_MEASURE(0.0174532925199433),#{radian})"
            ));
            let no_exponents = self
                .writer
                .entity("DIMENSIONAL_EXPONENTS(0.,0.,0.,0.,0.,0.,0.)");
            let degree = self.writer.entity(&format!("(CONVERSION_BASED_UNIT('DEGREE',#{degree_measure})NAMED_UNIT(#{no_exponents})PLANE_ANGLE_UNIT())"));
            (inch, degree)
        } else {
            (
                self.writer
                    .entity("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))"),
                self.writer
                    .entity("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))"),
            )
        };
        let solid_angle = self
            .writer
            .entity("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())");
        let uncertainty = self.writer.entity(&format!(
            "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-7),#{length},'distance_accuracy_value','')"
        ));
        let context = self.writer.entity(&format!(
            "(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncertainty}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{length},#{angle},#{solid_angle}))REPRESENTATION_CONTEXT('',''))"
        ));
        let origin = self.writer.entity("CARTESIAN_POINT('',(0.,0.,0.))");
        let z = self.writer.entity("DIRECTION('',(0.,0.,1.))");
        let x = self.writer.entity("DIRECTION('',(1.,0.,0.))");
        let placement = self
            .writer
            .entity(&format!("AXIS2_PLACEMENT_3D('',#{origin},#{z},#{x})"));
        let representation = self.writer.entity(&format!(
            "ADVANCED_BREP_SHAPE_REPRESENTATION('',(#{placement},#{solid}),#{context})"
        ));
        let application = self
            .writer
            .entity("APPLICATION_CONTEXT('managed model based 3d engineering')");
        let product_context = self
            .writer
            .entity(&format!("PRODUCT_CONTEXT('',#{application},'mechanical')"));
        let product = self
            .writer
            .entity(&format!("PRODUCT('part','part','',(#{product_context}))"));
        let formation = self
            .writer
            .entity(&format!("PRODUCT_DEFINITION_FORMATION('','',#{product})"));
        let definition_context = self.writer.entity(&format!(
            "PRODUCT_DEFINITION_CONTEXT('part definition',#{application},'design')"
        ));
        let definition = self.writer.entity(&format!(
            "PRODUCT_DEFINITION('design','',#{formation},#{definition_context})"
        ));
        let shape = self
            .writer
            .entity(&format!("PRODUCT_DEFINITION_SHAPE('','',#{definition})"));
        self.writer.entity(&format!(
            "SHAPE_DEFINITION_REPRESENTATION(#{shape},#{representation})"
        ));
        let schema = if ap242 {
            "AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF { 1 0 10303 442 1 1 4 }"
        } else {
            "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }"
        };
        self.writer
            .finish(&["a part in another system's style"], "part.step", schema)
    }
}

fn newell(points: &[[f64; 3]]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

/// A box of the given size at the origin, its twelve edges shared, with a
/// hook to bound the top and bottom faces further and to write the top
/// face's surface differently.
struct BoxShell {
    corners: [[f64; 3]; 8],
    vertices: [u64; 8],
    /// Edge ids by corner pair, lowest corner first.
    edges: BTreeMap<(usize, usize), u64>,
}

impl BoxShell {
    fn new(part: &mut Part, size: [f64; 3]) -> Self {
        let [sx, sy, sz] = size;
        let corners = [
            [0.0, 0.0, 0.0],
            [sx, 0.0, 0.0],
            [sx, sy, 0.0],
            [0.0, sy, 0.0],
            [0.0, 0.0, sz],
            [sx, 0.0, sz],
            [sx, sy, sz],
            [0.0, sy, sz],
        ];
        let vertices = corners.map(|corner| part.vertex(corner));
        let mut edges = BTreeMap::new();
        for (a, b) in [
            (0, 1),
            (1, 2),
            (2, 3),
            (0, 3),
            (4, 5),
            (5, 6),
            (6, 7),
            (4, 7),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ] {
            let edge = part.line_edge(vertices[a], vertices[b], corners[a], corners[b]);
            edges.insert((a, b), edge);
        }
        Self {
            corners,
            vertices,
            edges,
        }
    }

    /// The loop of a face through the given corners, wound
    /// counter-clockwise about `outward`.
    fn face_loop(&self, part: &mut Part, corners: [usize; 4], outward: [f64; 3]) -> u64 {
        let mut order = corners.to_vec();
        let points: Vec<[f64; 3]> = order.iter().map(|corner| self.corners[*corner]).collect();
        let n = newell(&points);
        if n[0] * outward[0] + n[1] * outward[1] + n[2] * outward[2] < 0.0 {
            order.reverse();
        }
        let mut uses = Vec::new();
        for index in 0..4 {
            let (a, b) = (order[index], order[(index + 1) % 4]);
            let (key, forward) = if a < b {
                ((a, b), true)
            } else {
                ((b, a), false)
            };
            uses.push((self.edges[&key], forward));
        }
        part.edge_loop(&uses)
    }
}

/// The FreeCAD/OCCT way to write a 40×30×20 block with an 8 mm bore
/// through it: the bore is one 360° cylindrical face with one seam edge,
/// and the top and bottom faces each carry the bore as one full-circle
/// inner bound.
fn occt_style_holed_box() -> String {
    let mut part = Part::new(false);
    let shell = BoxShell::new(&mut part, [40.0, 30.0, 20.0]);
    let (cx, cy, r) = (20.0, 15.0, 4.0);
    let seam_bottom = part.vertex([cx + r, cy, 0.0]);
    let seam_top = part.vertex([cx + r, cy, 20.0]);
    let circle_bottom = part.circle_edge(
        seam_bottom,
        seam_bottom,
        [cx, cy, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        r,
    );
    let circle_top = part.circle_edge(
        seam_top,
        seam_top,
        [cx, cy, 20.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        r,
    );
    let seam = part.line_edge(seam_bottom, seam_top, [cx + r, cy, 0.0], [cx + r, cy, 20.0]);
    let mut faces = Vec::new();
    // Bottom: outward −Z; the bore runs clockwise about −Z, which is the
    // circle's own sense.
    let outer = shell.face_loop(&mut part, [0, 1, 2, 3], [0.0, 0.0, -1.0]);
    let hole = part.edge_loop(&[(circle_bottom, true)]);
    let bounds = [part.bound(outer, true), part.bound(hole, false)];
    let plane = part.plane([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    faces.push(part.face(&bounds, plane, false));
    // Top: outward +Z; the bore runs clockwise about +Z: the circle reversed.
    let outer = shell.face_loop(&mut part, [4, 5, 6, 7], [0.0, 0.0, 1.0]);
    let hole = part.edge_loop(&[(circle_top, false)]);
    let bounds = [part.bound(outer, true), part.bound(hole, false)];
    let plane = part.plane([0.0, 0.0, 20.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    faces.push(part.face(&bounds, plane, true));
    // The four sides.
    for (corners, outward, origin) in [
        ([0, 1, 5, 4], [0.0, -1.0, 0.0], [0.0, 0.0, 0.0]),
        ([1, 2, 6, 5], [1.0, 0.0, 0.0], [40.0, 0.0, 0.0]),
        ([2, 3, 7, 6], [0.0, 1.0, 0.0], [0.0, 30.0, 0.0]),
        ([3, 0, 4, 7], [-1.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
    ] {
        let outer = shell.face_loop(&mut part, corners, outward);
        let bounds = [part.bound(outer, true)];
        let reference = if outward[0] == 0.0 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let plane = part.plane(origin, outward, reference);
        faces.push(part.face(&bounds, plane, true));
    }
    // The bore: one cylindrical face, its normal turned into the bore, with
    // the seam used once each way.
    let wall = part.edge_loop(&[
        (circle_bottom, false),
        (seam, true),
        (circle_top, true),
        (seam, false),
    ]);
    let bounds = [part.bound(wall, true)];
    let placement = part.placement([cx, cy, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    let cylinder = part
        .writer
        .entity(&format!("CYLINDRICAL_SURFACE('',#{placement},{})", real(r)));
    faces.push(part.face(&bounds, cylinder, false));
    let _ = shell.vertices;
    let solid = part.solid(&faces, "holed block");
    part.finish(solid, false)
}

#[test]
fn an_occt_style_block_with_a_seamed_through_bore_imports_exactly() {
    let text = occt_style_holed_box();
    let session = import(&text);
    let snapshot = &session.snapshot;
    assert!(NativeKernel::validate(snapshot, ValidationProfile::Solid).valid);
    assert_eq!(session.tier(), Tier::Exact);
    assert_relative(
        snapshot.measures().volume,
        40.0 * 30.0 * 20.0 - std::f64::consts::PI * 16.0 * 20.0,
        "volume",
    );
    // The one 360° face became the kernel's two halves, seamed at azimuth
    // 0 and π, and the bore's rims are semicircles.
    let counts = NativeKernel::surface_counts(snapshot);
    assert_eq!(counts.cylinders, 2, "{counts:?}");
    assert_eq!(counts.planes, 6, "{counts:?}");
    assert_eq!(snapshot.counts().faces, 8);
    assert_eq!(
        snapshot.counts().edges,
        12 + 4 + 2,
        "twelve box edges, four semicircles, two seams"
    );
    let described = NativeKernel::describe_faces(snapshot);
    let bore: Vec<_> = described
        .values()
        .filter(|face| face.geometry.surface_kind() == "cylinder")
        .collect();
    for face in &bore {
        assert_relative(
            face.area,
            std::f64::consts::PI * 4.0 * 20.0,
            "half the bore's area",
        );
        // The normal points into the bore, towards its axis.
        let arm = [face.centre.x - 20.0, face.centre.y - 15.0];
        assert!(
            arm[0] * face.normal.x + arm[1] * face.normal.y < 0.0,
            "{}",
            face.summary
        );
    }
    // The imported body exports as the kernel's own and reads back the same
    // body: the seams the import made are ordinary topology to the exporter.
    // (The digest is not compared: it hashes entity order, and the seam
    // vertices the first import cut are STEP vertices to the second.)
    let again = export_step(snapshot, "block").unwrap();
    let second = import(&again);
    assert_eq!(second.snapshot.counts(), snapshot.counts());
    assert_relative(
        second.snapshot.measures().volume,
        snapshot.measures().volume,
        "volume again",
    );
    assert_faces_agree(snapshot, &second.snapshot, "block re-exported");
}

/// The SolidWorks style: an AP242 header, inch units, and every circle a
/// rational B-spline.
fn solidworks_style_inch_cylinder() -> String {
    let mut part = Part::new(true);
    let (r, h) = (12.7, 25.4);
    let seam_bottom = part.vertex([r, 0.0, 0.0]);
    let seam_top = part.vertex([r, 0.0, h]);
    let circle_bottom = part.rational_circle_edge(seam_bottom, [0.0, 0.0, 0.0], r);
    let circle_top = part.rational_circle_edge(seam_top, [0.0, 0.0, h], r);
    let seam = part.line_edge(seam_bottom, seam_top, [r, 0.0, 0.0], [r, 0.0, h]);
    let mut faces = Vec::new();
    // Bottom, outward −Z: the circle runs counter-clockwise about −Z when
    // reversed.
    let bottom = part.edge_loop(&[(circle_bottom, false)]);
    let bounds = [part.bound(bottom, true)];
    let plane = part.plane([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]);
    faces.push(part.face(&bounds, plane, true));
    let top = part.edge_loop(&[(circle_top, true)]);
    let bounds = [part.bound(top, true)];
    let plane = part.plane([0.0, 0.0, h], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    faces.push(part.face(&bounds, plane, true));
    let wall = part.edge_loop(&[
        (circle_bottom, true),
        (seam, true),
        (circle_top, false),
        (seam, false),
    ]);
    let bounds = [part.bound(wall, true)];
    let placement = part.placement([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    let cylinder = part.writer.entity(&format!(
        "CYLINDRICAL_SURFACE('',#{placement},{})",
        real(r / 25.4)
    ));
    faces.push(part.face(&bounds, cylinder, true));
    let solid = part.solid(&faces, "post");
    part.finish(solid, true)
}

#[test]
fn a_solidworks_style_inch_file_with_bspline_circles_imports_exactly() {
    let text = solidworks_style_inch_cylinder();
    assert!(text.contains("AP242_MANAGED_MODEL_BASED_3D_ENGINEERING"));
    let file = artificer_step::parse(&text).unwrap();
    assert_eq!(file.header.application_protocol(), Some(242));
    assert!((file.units.length_to_mm - 25.4).abs() < 1.0e-12);
    let session = import(&text);
    let snapshot = &session.snapshot;
    assert!(NativeKernel::validate(snapshot, ValidationProfile::Solid).valid);
    assert_eq!(
        session.tier(),
        Tier::Exact,
        "{:?}",
        session.step_reports["part"].warnings
    );
    assert_relative(
        snapshot.measures().volume,
        std::f64::consts::PI * 12.7 * 12.7 * 25.4,
        "a half-inch post's volume in cubic millimetres",
    );
    // The rational circles were recognised and snapped to exact circles.
    for edge in NativeKernel::edges(snapshot) {
        let description = NativeKernel::describe_edge(snapshot, edge).unwrap();
        assert!(
            matches!(description.geometry.curve_kind(), "circle" | "line"),
            "{}",
            description.summary
        );
    }
    assert_eq!(NativeKernel::surface_counts(snapshot).cylinders, 2);
}

/// A box whose top is written as a rational B-spline surface with unequal
/// weights: the one thing ADR 0050's carrier cannot hold.
fn box_with_a_rational_top(surface: &str) -> String {
    let mut part = Part::new(false);
    let shell = BoxShell::new(&mut part, [40.0, 30.0, 20.0]);
    let mut faces = Vec::new();
    let outer = shell.face_loop(&mut part, [0, 1, 2, 3], [0.0, 0.0, -1.0]);
    let bounds = [part.bound(outer, true)];
    let plane = part.plane([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]);
    faces.push(part.face(&bounds, plane, true));
    for (corners, outward, origin) in [
        ([0, 1, 5, 4], [0.0, -1.0, 0.0], [0.0, 0.0, 0.0]),
        ([1, 2, 6, 5], [1.0, 0.0, 0.0], [40.0, 0.0, 0.0]),
        ([2, 3, 7, 6], [0.0, 1.0, 0.0], [0.0, 30.0, 0.0]),
        ([3, 0, 4, 7], [-1.0, 0.0, 0.0], [0.0, 0.0, 0.0]),
    ] {
        let outer = shell.face_loop(&mut part, corners, outward);
        let bounds = [part.bound(outer, true)];
        let reference = if outward[0] == 0.0 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let plane = part.plane(origin, outward, reference);
        faces.push(part.face(&bounds, plane, true));
    }
    let outer = shell.face_loop(&mut part, [4, 5, 6, 7], [0.0, 0.0, 1.0]);
    let bounds = [part.bound(outer, true)];
    let top_surface = match surface {
        "rational" => {
            let net: Vec<u64> = [[0.0, 0.0], [0.0, 30.0], [40.0, 0.0], [40.0, 30.0]]
                .iter()
                .map(|[x, y]| part.point([*x, *y, 20.0]))
                .collect();
            part.writer.entity(&format!(
                "(BOUNDED_SURFACE()B_SPLINE_SURFACE(1,1,((#{},#{}),(#{},#{})),.UNSPECIFIED.,.F.,.F.,.F.)B_SPLINE_SURFACE_WITH_KNOTS((2,2),(2,2),(0.,1.),(0.,1.),.UNSPECIFIED.)GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_SURFACE(((1.,1.),(1.,2.)))REPRESENTATION_ITEM('')SURFACE())",
                net[0], net[1], net[2], net[3]
            ))
        }
        _ => {
            let plane = part.plane([0.0, 0.0, 19.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
            part.writer
                .entity(&format!("OFFSET_SURFACE('',#{plane},1.,.T.)"))
        }
    };
    faces.push(part.face(&bounds, top_surface, true));
    let solid = part.solid(&faces, "block");
    part.finish(solid, false)
}

#[test]
fn a_rational_spline_surface_is_refused_by_name_and_the_part_opens_as_a_reference_mesh() {
    let session = import(&box_with_a_rational_top("rational"));
    let report = &session.step_reports["part"];
    assert_eq!(session.tier(), Tier::Approximate);
    assert_eq!(report.rung.as_deref(), Some("step-import/faceted"));
    let codes: Vec<&str> = report
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    assert!(codes.contains(&"STEP_RATIONAL_UNSUPPORTED"), "{codes:?}");
    assert!(codes.contains(&"STEP_FACETED_APPROXIMATION"), "{codes:?}");
    let refusal = report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "STEP_RATIONAL_UNSUPPORTED")
        .unwrap();
    assert!(
        refusal.message.starts_with('#'),
        "the refusal names the entity: {}",
        refusal.message
    );
    assert!(
        refusal.measurement.is_some(),
        "the weight spread is measured"
    );
    // The reference mesh is the block: its unreadable top is capped by the
    // boundary the file gave it.
    assert!(NativeKernel::validate(&session.snapshot, ValidationProfile::Solid).valid);
    assert_relative(session.snapshot.measures().volume, 24_000.0, "volume");
}

#[test]
fn an_unreadable_surface_opens_as_a_labelled_reference_mesh_with_the_face_named() {
    let session = import(&box_with_a_rational_top("offset"));
    let report = &session.step_reports["part"];
    assert_eq!(session.tier(), Tier::Approximate);
    assert_eq!(report.rung.as_deref(), Some("step-import/faceted"));
    let unsupported = report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "STEP_FACE_UNSUPPORTED")
        .unwrap_or_else(|| panic!("{:?}", report.warnings));
    assert!(
        unsupported.message.contains("OFFSET_SURFACE"),
        "{}",
        unsupported.message
    );
    assert!(unsupported.details.contains_key("entity"));
    let label = report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "STEP_FACETED_APPROXIMATION")
        .unwrap();
    assert!(
        label.message.contains("STEP_FACE_UNSUPPORTED"),
        "the label lists the refusals: {}",
        label.message
    );
    assert_relative(session.snapshot.measures().volume, 24_000.0, "volume");
}

#[test]
fn text_that_is_not_part_21_is_refused_by_name() {
    let mut session = Session::new();
    let error = session
        .import_step_text(
            "part",
            "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=CARTESIAN_POINT('',(1.,2.);\n",
            &CancellationToken::default(),
        )
        .unwrap_err();
    assert!(
        error.message.contains("STEP_SYNTAX_INVALID") || error.message.contains("Part 21"),
        "{error:?}"
    );
    let empty = session
        .import_step_text("part", "ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=CARTESIAN_POINT('',(1.,2.,3.));\nENDSEC;\nEND-ISO-10303-21;\n", &CancellationToken::default())
        .unwrap_err();
    assert!(empty.message.contains("no solid"), "{empty:?}");
}

// ---------------------------------------------------------------------------
// I4: reachability
// ---------------------------------------------------------------------------

#[test]
fn the_script_builtin_imports_a_file_decompiles_and_names_faces_by_their_entity_ids() {
    let directory =
        std::env::temp_dir().join(format!("artificer-step-import-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("block.step");
    let text = occt_style_holed_box();
    std::fs::write(&path, &text).unwrap();
    // The bore's STEP face is named by its entity number, and became two
    // kernel faces; the top face is named by its and drilled again.
    let file = artificer_step::parse(&text).unwrap();
    let bore = file
        .graph
        .of_kind("ADVANCED_FACE")
        .find(|face| {
            face.arg(2)
                .as_ref()
                .and_then(|id| file.graph.get(id))
                .is_some_and(|surface| surface.is("CYLINDRICAL_SURFACE"))
        })
        .unwrap()
        .id;
    let top = file
        .graph
        .of_kind("ADVANCED_FACE")
        .find(|face| {
            face.arg(1)
                .as_refs()
                .is_some_and(|bounds| bounds.len() == 2)
                && face.arg(3).as_bool() == Some(true)
        })
        .unwrap()
        .id;
    let script = format!(
        "let part = import_step(path: {}, label: \"part\");\ndrill(face: part.face(\"#{top}\"), center: [-12, 0], diameter: 6, depth: 20, label: \"second\");\n",
        quoted_path(&path)
    );
    let mut session = Session::new();
    let outcome = session.run_script(&script, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    assert_eq!(session.step_kinds["part"], "import_step");
    assert_relative(
        session
            .step_snapshots
            .get("part")
            .and_then(|id| session.snapshot_cache.get(id))
            .map(|snapshot| snapshot.measures().volume)
            .unwrap(),
        40.0 * 30.0 * 20.0 - std::f64::consts::PI * 16.0 * 20.0,
        "volume",
    );
    assert!(
        session.snapshot.measures().volume
            < 40.0 * 30.0 * 20.0 - std::f64::consts::PI * 16.0 * 20.0,
        "the second bore took material away"
    );
    let query = session.query();
    let top_face = query
        .entity_info(&EntitySelector::ByHistory {
            from_step: "part".into(),
            kind: EntityKind::Face,
            role: format!("#{top}"),
            ordinal: None,
        })
        .unwrap();
    assert!(
        top_face.geometry_description.contains("Face"),
        "{top_face:?}"
    );
    let half = query
        .entity_info(&EntitySelector::ByHistory {
            from_step: "part".into(),
            kind: EntityKind::Face,
            role: format!("#{bore}"),
            ordinal: Some(1),
        })
        .unwrap();
    assert_eq!(half.kind, EntityKind::Face);
    let ambiguous = query.entity_info(&EntitySelector::ByHistory {
        from_step: "part".into(),
        kind: EntityKind::Face,
        role: format!("#{bore}"),
        ordinal: None,
    });
    assert!(
        ambiguous.is_err(),
        "the bore became two faces, so its name alone is ambiguous"
    );
    // The journal decompiles to a script that rebuilds the same body.
    let decompiled = session.to_art(&DecompileOptions::default()).unwrap();
    assert!(decompiled.contains("import_step(path:"), "{decompiled}");
    let mut rebuilt = Session::new();
    let outcome = rebuilt.run_script(&decompiled, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}\n{decompiled}", outcome.failure);
    assert_eq!(
        rebuilt.snapshot.semantic_digest(),
        session.snapshot.semantic_digest()
    );
    std::fs::remove_dir_all(&directory).ok();
}

fn quoted_path(path: &std::path::Path) -> String {
    format!("\"{}\"", path.to_string_lossy().replace('\\', "\\\\"))
}

#[test]
fn the_json_rpc_import_method_reads_step_text() {
    let server = SharedSession::new();
    let text = occt_style_holed_box();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "import.step",
        "params": { "label": "block", "text": text }
    });
    let response = server.handle_request(&request.to_string());
    assert_eq!(response.error, None);
    let result = response.result.expect("a result");
    assert_eq!(result["success"], true);
    assert_eq!(result["step_label"], "block");
    assert_eq!(result["rung"], "step-import/exact");
    assert_eq!(result["tier"], "exact");
    let report = server.handle_request(r#"{"jsonrpc":"2.0","id":2,"method":"report"}"#);
    let body = report.result.expect("a report")["body"].clone();
    let volume = body["volume"].as_f64().expect("a volume");
    assert_relative(
        volume,
        40.0 * 30.0 * 20.0 - std::f64::consts::PI * 16.0 * 20.0,
        "volume",
    );
    let bad =
        server.handle_request(r#"{"jsonrpc":"2.0","id":3,"method":"import.step","params":{}}"#);
    assert!(bad.error.is_some());
}

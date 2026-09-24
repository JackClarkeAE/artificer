//! Sheet bodies (ADR 0056, Track S), pinned to closed forms computed here
//! without the kernel: the areas of the sheets the constructions sweep, the
//! volumes the thickened and stitched solids enclose, and the areas a trim
//! leaves, each an arithmetic expression of the dimensions asked for.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, ExecutionOutcome, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EntityKind, ExecuteRequest, KernelCommand, KernelError,
    PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, QuantityKind, RequestId, RevolveAngle, StitchRequest, Tier,
    ValidationProfile, Vector3,
};

// ---------------------------------------------------------------------------
// Running the kernel
// ---------------------------------------------------------------------------

fn precision() -> PrecisionPolicy {
    PrecisionPolicy::default()
}

fn execute(input: &Snapshot, command: KernelCommand) -> Result<ExecutionOutcome, KernelError> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("surface_frontier"),
        expected_snapshot: input.id(),
        precision: precision(),
        command,
    };
    NativeKernel::execute(input, &request, &CancellationToken::default())
}

fn build(command: KernelCommand) -> ExecutionOutcome {
    execute(&NativeKernel::empty(), command).unwrap_or_else(|error| panic!("{error:?}"))
}

fn stitch(sheets: &[&Snapshot]) -> Result<ExecutionOutcome, KernelError> {
    let request = StitchRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("surface_frontier::stitch"),
        expected_snapshots: sheets.iter().map(|sheet| sheet.id()).collect(),
        precision: precision(),
    };
    NativeKernel::stitch_sheets(sheets, &request, &CancellationToken::default())
}

fn frame(origin: [f64; 3], u: [f64; 3], v: [f64; 3]) -> PlanarFrame3 {
    PlanarFrame3::new(
        Point3::new(origin[0], origin[1], origin[2]),
        Vector3::new(u[0], u[1], u[2]),
        Vector3::new(v[0], v[1], v[2]),
    )
}

fn xy() -> PlanarFrame3 {
    frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0])
}

/// The XZ plane as a revolve section: `x` is the radius, `y` the height.
fn xz() -> PlanarFrame3 {
    frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0])
}

fn line(start: (f64, f64), end: (f64, f64)) -> PlanarCurve2 {
    PlanarCurve2::Line {
        start: Point2::new(start.0, start.1),
        end: Point2::new(end.0, end.1),
    }
}

fn arc(center: (f64, f64), start: (f64, f64), end: (f64, f64)) -> PlanarCurve2 {
    PlanarCurve2::CircularArc {
        center: Point2::new(center.0, center.1),
        start: Point2::new(start.0, start.1),
        end: Point2::new(end.0, end.1),
        direction: ArcDirection::CounterClockwise,
    }
}

fn circle(radius: f64) -> Vec<PlanarCurve2> {
    vec![PlanarCurve2::Circle {
        center: Point2::new(0.0, 0.0),
        radius,
        direction: ArcDirection::CounterClockwise,
    }]
}

/// The quadratic Bézier arch from `(0, 0)` over `(5, 10)` to `(10, 0)`: a
/// parabola with its apex at `(5, 5)`, whose length and total turning have
/// closed forms (see [`arch_length`] and [`arch_turning`]).
fn arch() -> PlanarCurve2 {
    PlanarCurve2::Bspline {
        degree: 2,
        control_points: vec![
            Point2::new(0.0, 0.0),
            Point2::new(5.0, 10.0),
            Point2::new(10.0, 0.0),
        ],
        knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        weights: None,
    }
}

/// The arch's speed is `10·√(1 + 4(1 − 2t)²)`, so its length is
/// `5·∫₀² √(1 + s²) ds = 5√5 + (5/2)·asinh 2`.
fn arch_length() -> f64 {
    5.0 * 5.0_f64.sqrt() + 2.5 * 2.0_f64.asinh()
}

/// The arch's tangent turns from `(1, 2)` to `(1, −2)`: through `2·atan 2`.
fn arch_turning() -> f64 {
    2.0 * 2.0_f64.atan()
}

fn polygon(points: &[(f64, f64)]) -> PlanarProfile2 {
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: (0..points.len())
                    .map(|index| line(points[index], points[(index + 1) % points.len()]))
                    .collect(),
            },
            holes: Vec::new(),
        }],
    }
}

fn z_axis() -> PlanarAxis2 {
    PlanarAxis2::new(Point2::new(0.0, 0.0), Point2::new(0.0, 1.0))
}

fn cylinder_sheet(radius: f64, height: f64) -> ExecutionOutcome {
    build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain: circle(radius),
        distance: height,
    })
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    let scale = expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= 1.0e-9 * scale,
        "{what}: expected {expected}, got {actual}"
    );
}

fn assert_sheet(outcome: &ExecutionOutcome) {
    assert!(
        outcome.report.validation.valid,
        "{:?}",
        outcome.report.validation.diagnostics
    );
    assert_eq!(outcome.report.validation.profile, ValidationProfile::Sheet);
    assert_eq!(outcome.snapshot.counts().solids, 0);
    assert!(outcome.snapshot.counts().shells >= 1);
    assert_close(outcome.snapshot.measures().volume, 0.0, "a sheet's volume");
    let report = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Sheet);
    assert!(report.valid, "{:?}", report.diagnostics);
    // Held to the solid profile the same body fails: its boundary edges
    // are used once.
    let as_solid = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(!as_solid.valid);
}

fn assert_solid(outcome: &ExecutionOutcome) {
    assert!(
        outcome.report.validation.valid,
        "{:?}",
        outcome.report.validation.diagnostics
    );
    assert_eq!(outcome.report.validation.profile, ValidationProfile::Solid);
    assert!(outcome.snapshot.counts().solids >= 1);
    let report = NativeKernel::validate(&outcome.snapshot, ValidationProfile::Solid);
    assert!(report.valid, "{:?}", report.diagnostics);
}

fn boundary_edges(outcome: &ExecutionOutcome) -> usize {
    outcome
        .report
        .history
        .iter()
        .filter(|record| {
            record
                .role
                .as_ref()
                .is_some_and(|role| role.name == "boundary_edge")
        })
        .count()
}

fn code_of(error: &KernelError) -> Vec<String> {
    error
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Constructions
// ---------------------------------------------------------------------------

#[test]
fn a_surface_extrusion_of_a_circle_is_a_cylinder_sheet_with_area_two_pi_r_h() {
    let (radius, height) = (10.0, 20.0);
    let sheet = cylinder_sheet(radius, height);
    assert_sheet(&sheet);
    assert_close(
        sheet.snapshot.measures().surface_area,
        2.0 * PI * radius * height,
        "cylinder sheet area",
    );
    let counts = sheet.snapshot.counts();
    assert_eq!((counts.faces, counts.shells), (2, 1));
    // Two rims of two arcs each are the boundary; the seams are shared.
    assert_eq!(boundary_edges(&sheet), 4);
    assert_eq!(sheet.report.rung.as_deref(), Some("surface/extrude"));
}

#[test]
fn a_surface_extrusion_of_an_open_chain_has_walls_and_no_caps() {
    // Two lines and an arc: a plane, a quarter cylinder and a plane, all
    // 3 high, with no cap and every end edge on the boundary.
    let chain = vec![
        line((0.0, 0.0), (10.0, 0.0)),
        arc((10.0, 5.0), (10.0, 0.0), (15.0, 5.0)),
        line((15.0, 5.0), (15.0, 12.0)),
    ];
    let sheet = build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain,
        distance: 3.0,
    });
    assert_sheet(&sheet);
    let length = 10.0 + 2.0 * PI * 5.0 / 4.0 + 7.0;
    assert_close(
        sheet.snapshot.measures().surface_area,
        length * 3.0,
        "open chain sheet area",
    );
    assert_eq!(sheet.snapshot.counts().faces, 3);
    // Three bottom edges, three top edges and the two end generators.
    assert_eq!(boundary_edges(&sheet), 8);
}

#[test]
fn a_surface_extrusion_by_a_negative_distance_sweeps_the_other_way() {
    let up = build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain: vec![line((0.0, 0.0), (4.0, 0.0))],
        distance: 5.0,
    });
    let down = build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain: vec![line((0.0, 0.0), (4.0, 0.0))],
        distance: -5.0,
    });
    assert_sheet(&up);
    assert_sheet(&down);
    assert_close(up.snapshot.measures().surface_area, 20.0, "up");
    assert_close(down.snapshot.measures().surface_area, 20.0, "down");
    let bounds = down.snapshot.measures().bounds.unwrap();
    assert_close(bounds.min.z, -5.0, "swept below the frame");
    assert_close(bounds.max.z, 0.0, "from the frame");
}

#[test]
fn a_surface_extrusion_of_a_spline_is_a_bspline_sheet_with_area_length_times_height() {
    let height = 20.0;
    let sheet = build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain: vec![arch()],
        distance: height,
    });
    assert_sheet(&sheet);
    assert_eq!(sheet.snapshot.counts().faces, 1);
    assert_eq!(boundary_edges(&sheet), 4);
    assert_close(
        sheet.snapshot.measures().surface_area,
        arch_length() * height,
        "spline sheet area",
    );
    assert_eq!(sheet.report.rung.as_deref(), Some("surface/extrude"));
    assert_eq!(sheet.report.tier(), Tier::Exact);
    // A spline piece chains with lines like any other.
    let closed = build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain: vec![arch(), line((10.0, 0.0), (0.0, 0.0))],
        distance: height,
    });
    assert_sheet(&closed);
    assert_eq!(closed.snapshot.counts().faces, 2);
    // Two bottom edges and two top edges; the two generators are shared.
    assert_eq!(boundary_edges(&closed), 4);
    assert_close(
        closed.snapshot.measures().surface_area,
        (arch_length() + 10.0) * height,
        "closed spline chain area",
    );
}

#[test]
fn a_surface_revolve_of_a_slanted_line_is_a_cone_sheet_with_the_slant_area() {
    let (r0, z0, r1, z1) = (5.0, 0.0, 10.0, 8.0);
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![line((r0, z0), (r1, z1))],
        axis: z_axis(),
        angle: RevolveAngle::FullTurn,
    });
    assert_sheet(&sheet);
    let slant = ((r1 - r0) * (r1 - r0) + (z1 - z0) * (z1 - z0)).sqrt();
    assert_close(
        sheet.snapshot.measures().surface_area,
        PI * (r0 + r1) * slant,
        "cone sheet area by its slant",
    );
    assert_eq!(sheet.snapshot.counts().faces, 2);
    assert_eq!(boundary_edges(&sheet), 4);
    assert_eq!(sheet.report.rung.as_deref(), Some("surface/revolve"));
}

#[test]
fn a_partial_surface_revolve_has_no_wedge_faces() {
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![line((10.0, 0.0), (10.0, 4.0))],
        axis: z_axis(),
        angle: RevolveAngle::partial(0.0, PI / 2.0),
    });
    assert_sheet(&sheet);
    assert_close(
        sheet.snapshot.measures().surface_area,
        2.0 * PI * 10.0 * 4.0 / 4.0,
        "quarter cylinder sheet area",
    );
    // Two rim arcs, two rim arcs and the two end generators.
    assert_eq!(boundary_edges(&sheet), 6);
}

#[test]
fn a_surface_revolve_of_a_semicircle_is_a_closed_sphere_sheet() {
    let radius = 10.0;
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![arc((0.0, 0.0), (0.0, -radius), (0.0, radius))],
        axis: z_axis(),
        angle: RevolveAngle::FullTurn,
    });
    assert!(
        sheet.report.validation.valid,
        "{:?}",
        sheet.report.validation.diagnostics
    );
    assert_eq!(sheet.snapshot.counts().solids, 0);
    assert_close(
        sheet.snapshot.measures().surface_area,
        4.0 * PI * radius * radius,
        "sphere sheet area",
    );
    // A closed sheet has no boundary, and is still not a solid.
    assert_eq!(boundary_edges(&sheet), 0);
}

#[test]
fn a_planar_patch_is_one_face_with_the_profile_area() {
    let patch = build(KernelCommand::PlanarPatch {
        frame: xy(),
        profile: polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)]),
    });
    assert_sheet(&patch);
    assert_close(patch.snapshot.measures().surface_area, 60.0, "patch area");
    assert_eq!(patch.snapshot.counts().faces, 1);
    assert_eq!(boundary_edges(&patch), 4);
    assert_eq!(patch.report.rung.as_deref(), Some("surface/patch"));
}

#[test]
fn a_planar_patch_keeps_the_profile_holes() {
    let mut profile = polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
    profile.regions[0].holes.push(PlanarLoop2 {
        curves: vec![PlanarCurve2::Circle {
            center: Point2::new(5.0, 5.0),
            radius: 2.0,
            direction: ArcDirection::Clockwise,
        }],
    });
    let patch = build(KernelCommand::PlanarPatch {
        frame: xy(),
        profile,
    });
    assert_sheet(&patch);
    assert_close(
        patch.snapshot.measures().surface_area,
        100.0 - PI * 4.0,
        "holed patch area",
    );
    assert_eq!(boundary_edges(&patch), 6);
}

#[test]
fn a_disconnected_chain_is_refused_by_name() {
    let error = execute(
        &NativeKernel::empty(),
        KernelCommand::SurfaceExtrude {
            frame: xy(),
            chain: vec![
                line((0.0, 0.0), (10.0, 0.0)),
                line((11.0, 0.0), (11.0, 5.0)),
            ],
            distance: 3.0,
        },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["SURFACE_CHAIN_DISCONNECTED"]);
}

#[test]
fn a_chain_pinched_on_the_axis_is_refused_by_name() {
    let error = execute(
        &NativeKernel::empty(),
        KernelCommand::SurfaceRevolve {
            frame: xz(),
            chain: vec![line((5.0, 0.0), (0.0, 5.0)), line((0.0, 5.0), (5.0, 10.0))],
            axis: z_axis(),
            angle: RevolveAngle::FullTurn,
        },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["SURFACE_REVOLVE_PINCHED_ON_AXIS"]);
}

// ---------------------------------------------------------------------------
// What a sheet cannot do, by name
// ---------------------------------------------------------------------------

#[test]
fn a_solid_operation_on_a_sheet_is_refused_by_name_not_by_panic() {
    let sheet = cylinder_sheet(10.0, 20.0);
    let face = sheet
        .report
        .history
        .iter()
        .flat_map(|record| record.outputs.iter().copied())
        .find(|entity| entity.kind == EntityKind::Face)
        .unwrap();
    let error = execute(
        &sheet.snapshot,
        KernelCommand::PushPullFace {
            target_face: face,
            distance: 1.0,
        },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["SHEET_UNSUPPORTED_HERE"]);
    let error = execute(
        &sheet.snapshot,
        KernelCommand::ShellSnapshot {
            open_faces: Vec::new(),
            wall: 1.0,
        },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["SHEET_UNSUPPORTED_HERE"]);
}

#[test]
fn a_thicken_or_trim_of_a_solid_is_refused_by_name() {
    let solid = build(KernelCommand::MakeCuboid {
        origin: Point3::new(0.0, 0.0, 0.0),
        size_x: 10.0,
        size_y: 10.0,
        size_z: 10.0,
    });
    let error = execute(
        &solid.snapshot,
        KernelCommand::ThickenSheet { thickness: 1.0 },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["SHEET_INPUT_REQUIRED"]);
}

#[test]
fn a_moved_sheet_is_still_a_sheet() {
    let sheet = cylinder_sheet(10.0, 20.0);
    let moved = execute(
        &sheet.snapshot,
        KernelCommand::TransformSnapshot {
            transform: artificer_protocol::SimilarityTransform3 {
                translation: Vector3::new(100.0, 0.0, 0.0),
                rotation: artificer_protocol::RotationQuaternion::IDENTITY,
                uniform_scale: 1.0,
            },
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&moved);
    assert_close(
        moved.snapshot.measures().surface_area,
        2.0 * PI * 10.0 * 20.0,
        "moved cylinder sheet area",
    );
    let mirrored = execute(
        &sheet.snapshot,
        KernelCommand::MirrorSnapshot {
            plane_origin: Point3::new(0.0, 0.0, 0.0),
            plane_normal: Vector3::new(0.0, 0.0, 1.0),
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&mirrored);
}

#[test]
fn a_sheets_boundary_edges_are_drawn_as_hard_edges() {
    let sheet = cylinder_sheet(10.0, 20.0);
    let scene = NativeKernel::debug_scene(&sheet.snapshot);
    let boundary: Vec<_> = scene
        .edges
        .iter()
        .filter(|edge| edge.incident_faces[1].is_none())
        .collect();
    // Four rim arcs on the boundary, drawn as creases; the two seams
    // between the halves are smooth.
    let mut boundary_edges: Vec<_> = boundary.iter().map(|edge| edge.source_edge).collect();
    boundary_edges.sort_unstable();
    boundary_edges.dedup();
    assert_eq!(boundary_edges.len(), 4);
    assert!(
        boundary
            .iter()
            .all(|edge| !edge.is_smooth && !edge.is_tangent)
    );
    let seams: Vec<_> = scene
        .edges
        .iter()
        .filter(|edge| edge.incident_faces[1].is_some())
        .collect();
    assert!(!seams.is_empty());
    assert!(seams.iter().all(|edge| edge.is_smooth));
}

#[test]
fn a_patterned_sheet_is_a_sheet_of_several_shells() {
    let sheet = cylinder_sheet(10.0, 20.0);
    let row = execute(
        &sheet.snapshot,
        KernelCommand::LinearPatternSnapshot {
            direction: Vector3::new(1.0, 0.0, 0.0),
            spacing: 30.0,
            count: 3,
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&row);
    assert_eq!(row.snapshot.counts().shells, 3);
    assert_close(
        row.snapshot.measures().surface_area,
        3.0 * 2.0 * PI * 10.0 * 20.0,
        "three cylinder sheets",
    );
}

// ---------------------------------------------------------------------------
// Stitch
// ---------------------------------------------------------------------------

/// The six faces of the cube `[0, s]³`, each a patch drawn on its own
/// frame. The frames face whichever way is convenient: the stitch is what
/// turns them consistently outward.
fn cube_patches(size: f64, top_lift: f64) -> Vec<Snapshot> {
    let s = size;
    let square = polygon(&[(0.0, 0.0), (s, 0.0), (s, s), (0.0, s)]);
    let frames = [
        frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        frame([0.0, 0.0, s + top_lift], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        frame([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        frame([0.0, s, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        frame([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        frame([s, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ];
    frames
        .into_iter()
        .map(|frame| {
            build(KernelCommand::PlanarPatch {
                frame,
                profile: square.clone(),
            })
            .snapshot
        })
        .collect()
}

#[test]
fn six_planar_patches_stitch_into_a_closed_cube() {
    let patches = cube_patches(10.0, 0.0);
    let sheets: Vec<&Snapshot> = patches.iter().collect();
    let cube = stitch(&sheets).unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&cube);
    assert_close(cube.snapshot.measures().volume, 1000.0, "cube volume");
    assert_close(cube.snapshot.measures().surface_area, 600.0, "cube area");
    let counts = cube.snapshot.counts();
    assert_eq!(
        (
            counts.vertices,
            counts.edges,
            counts.faces,
            counts.shells,
            counts.solids
        ),
        (8, 12, 6, 1, 1)
    );
    assert_eq!(cube.report.rung.as_deref(), Some("stitch/solid"));
}

#[test]
fn three_patches_stitch_into_an_open_sheet() {
    let patches = cube_patches(10.0, 0.0);
    let sheets: Vec<&Snapshot> = patches.iter().take(3).collect();
    let corner = stitch(&sheets).unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&corner);
    assert_close(
        corner.snapshot.measures().surface_area,
        300.0,
        "three faces",
    );
    assert_eq!(corner.snapshot.counts().shells, 1);
    assert_eq!(corner.report.rung.as_deref(), Some("stitch/sheet"));
    // The side shares one edge with the bottom and one with the top; the
    // other eight of the ten edges stay open.
    assert_eq!(corner.snapshot.counts().edges, 10);
    assert_eq!(boundary_edges(&corner), 8);
}

#[test]
fn a_stitch_with_one_edge_out_of_tolerance_is_refused_with_the_gap() {
    let lift = 0.01;
    let patches = cube_patches(10.0, lift);
    let sheets: Vec<&Snapshot> = patches.iter().collect();
    let error = stitch(&sheets).unwrap_err();
    assert_eq!(code_of(&error), vec!["STITCH_GAP_EXCEEDS_TOLERANCE"]);
    let measurement = error.diagnostics[0].measurement.unwrap();
    assert_close(measurement.measured, lift, "the measured gap");
    assert!(measurement.allowed.max.unwrap() < lift);
    // Without the lifted top the other five stitch into an open box.
    let sheets: Vec<&Snapshot> = patches
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, sheet)| sheet)
        .collect();
    let open_box = stitch(&sheets).unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&open_box);
    assert_eq!(boundary_edges(&open_box), 4);
}

#[test]
fn a_cylinder_sheet_and_two_disks_stitch_into_a_cylinder() {
    let (radius, height) = (10.0, 20.0);
    let wall = cylinder_sheet(radius, height);
    let disk = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: circle(radius),
            },
            holes: Vec::new(),
        }],
    };
    let bottom = build(KernelCommand::PlanarPatch {
        frame: xy(),
        profile: disk.clone(),
    });
    let top = build(KernelCommand::PlanarPatch {
        frame: frame([0.0, 0.0, height], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        profile: disk,
    });
    let can = stitch(&[&wall.snapshot, &bottom.snapshot, &top.snapshot])
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&can);
    assert_close(
        can.snapshot.measures().volume,
        PI * radius * radius * height,
        "stitched can volume",
    );
    assert_close(
        can.snapshot.measures().surface_area,
        2.0 * PI * radius * height + 2.0 * PI * radius * radius,
        "stitched can area",
    );
}

// ---------------------------------------------------------------------------
// Thicken
// ---------------------------------------------------------------------------

#[test]
fn a_cylinder_sheet_thickened_outward_is_a_tube() {
    let (radius, height, wall) = (10.0, 20.0, 2.0);
    let sheet = cylinder_sheet(radius, height);
    let tube = execute(
        &sheet.snapshot,
        KernelCommand::ThickenSheet { thickness: wall },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&tube);
    let outer = radius + wall;
    assert_close(
        tube.snapshot.measures().volume,
        PI * (outer * outer - radius * radius) * height,
        "tube volume",
    );
    assert_close(
        tube.snapshot.measures().surface_area,
        2.0 * PI * (outer + radius) * height + 2.0 * PI * (outer * outer - radius * radius),
        "tube area",
    );
    // Two inner halves, two outer halves and four annular walls.
    assert_eq!(tube.snapshot.counts().faces, 8);
    assert_eq!(tube.report.rung.as_deref(), Some("thicken/exact"));
}

#[test]
fn a_cylinder_sheet_thickened_inward_is_a_tube_inside_it() {
    let (radius, height, wall) = (10.0, 20.0, 2.0);
    let sheet = cylinder_sheet(radius, height);
    let tube = execute(
        &sheet.snapshot,
        KernelCommand::ThickenSheet { thickness: -wall },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&tube);
    let inner = radius - wall;
    assert_close(
        tube.snapshot.measures().volume,
        PI * (radius * radius - inner * inner) * height,
        "inward tube volume",
    );
    let bounds = tube.snapshot.measures().bounds.unwrap();
    assert_close(bounds.max.x, radius, "the sheet is the outside");
}

#[test]
fn a_sphere_sheet_thickened_is_a_hollow_ball() {
    let (radius, wall) = (10.0, 2.0);
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![arc((0.0, 0.0), (0.0, -radius), (0.0, radius))],
        axis: z_axis(),
        angle: RevolveAngle::FullTurn,
    });
    let ball = execute(
        &sheet.snapshot,
        KernelCommand::ThickenSheet { thickness: wall },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&ball);
    let outer = radius + wall;
    assert_close(
        ball.snapshot.measures().volume,
        4.0 / 3.0 * PI * (outer.powi(3) - radius.powi(3)),
        "hollow ball volume",
    );
    assert_close(
        ball.snapshot.measures().surface_area,
        4.0 * PI * (outer * outer + radius * radius),
        "hollow ball area",
    );
    assert_eq!(ball.snapshot.counts().shells, 2);
}

#[test]
fn a_spline_sheet_thickened_is_labelled_approximate_with_its_deviation() {
    let (height, wall) = (20.0, 1.0);
    let sheet = build(KernelCommand::SurfaceExtrude {
        frame: xy(),
        chain: vec![arch()],
        distance: height,
    });
    let thick = execute(
        &sheet.snapshot,
        KernelCommand::ThickenSheet { thickness: wall },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&thick);
    // Never presented as exact: the approximate rung, the tier that follows
    // from it, and a warning that measures how far the offset strays.
    assert_eq!(thick.report.rung.as_deref(), Some("thicken/approximate"));
    assert_eq!(thick.report.tier(), Tier::Approximate);
    let warning = thick
        .report
        .warnings
        .iter()
        .find(|warning| warning.code.as_str() == "SURFACE_OFFSET_APPROXIMATION")
        .expect("the offset approximation is declared");
    let measurement = warning
        .measurement
        .as_ref()
        .expect("the deviation is measured");
    assert_eq!(measurement.quantity, QuantityKind::Length);
    let deviation = measurement.measured;
    // Refined until the offset is within the approximation budget, the
    // same budget the faceted tier tessellates to, and said against it.
    let budget = precision().approximation_budget;
    assert_eq!(measurement.allowed.max, Some(budget));
    assert!(
        deviation > 0.0 && deviation <= budget,
        "deviation {deviation} against {budget}"
    );
    // The band between a plane curve and its offset by `d` has area
    // `d·L ± d²·θ/2`, `θ` the curve's total turning: plus away from the
    // centre of curvature, minus toward it. The sheet faces toward it
    // (below the arch), which the offset's extent shows.
    let bounds = thick.snapshot.measures().bounds.unwrap();
    let toward = bounds.max.y < 5.0 + wall / 2.0;
    let sign = if toward { -1.0 } else { 1.0 };
    let expected = height * (wall * arch_length() + sign * wall * wall * arch_turning() / 2.0);
    // The offset strays from the true offset by `deviation` at worst over
    // the sheet's area, which bounds the volume the approximation moves.
    let slack = 2.0 * deviation * sheet.snapshot.measures().surface_area;
    let volume = thick.snapshot.measures().volume;
    assert!(
        (volume - expected).abs() <= slack,
        "thickened spline volume: expected {expected} within {slack}, got {volume}"
    );
    // The same band the other way is thicker by the turning, and the two
    // together are `2·d·L·h` whichever way the sheet faces.
    let other = execute(
        &sheet.snapshot,
        KernelCommand::ThickenSheet { thickness: -wall },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&other);
    assert!(
        (other.snapshot.measures().volume + volume - 2.0 * height * wall * arch_length()).abs()
            <= 2.0 * slack
    );
}

#[test]
fn a_planar_patch_thickened_is_a_slab() {
    let patch = build(KernelCommand::PlanarPatch {
        frame: xy(),
        profile: polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)]),
    });
    let slab = execute(
        &patch.snapshot,
        KernelCommand::ThickenSheet { thickness: 3.0 },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&slab);
    assert_close(slab.snapshot.measures().volume, 180.0, "slab volume");
    assert_close(
        slab.snapshot.measures().surface_area,
        2.0 * 60.0 + 2.0 * 30.0 + 2.0 * 18.0,
        "slab area",
    );
    assert_eq!(slab.snapshot.counts().faces, 6);
}

#[test]
fn a_cone_sheet_thickened_is_a_conical_shell() {
    // A cone from radius 4 at z = 0 to radius 10 at z = 8, thickened by 1
    // along its outward normal: the volume is the difference of the two
    // frustums, computed here from the offset section.
    let (r0, r1, h, wall) = (4.0_f64, 10.0_f64, 8.0_f64, 1.0_f64);
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![line((r0, 0.0), (r1, h))],
        axis: z_axis(),
        angle: RevolveAngle::FullTurn,
    });
    let shell = execute(
        &sheet.snapshot,
        KernelCommand::ThickenSheet { thickness: wall },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_solid(&shell);
    // The section is the quadrilateral between the line and its offset;
    // by Pappus the volume is its area times the path of its centroid.
    let slope = (r1 - r0) / h;
    let scale = (1.0 + slope * slope).sqrt();
    let (nr, nz) = (1.0 / scale, -slope / scale);
    let section = [
        (r0, 0.0),
        (r1, h),
        (r1 + wall * nr, h + wall * nz),
        (r0 + wall * nr, wall * nz),
    ];
    let mut area = 0.0;
    let mut moment = 0.0;
    for index in 0..4 {
        let (x0, y0) = section[index];
        let (x1, y1) = section[(index + 1) % 4];
        let cross = x0 * y1 - x1 * y0;
        area += cross / 2.0;
        moment += (x0 + x1) * cross / 6.0;
    }
    // The section is walked clockwise; the volume is what its area sweeps
    // either way round.
    let volume = 2.0 * PI * moment.abs();
    assert!(area.abs() > 0.0);
    assert_close(
        shell.snapshot.measures().volume,
        volume,
        "conical shell volume",
    );
}

// ---------------------------------------------------------------------------
// Trim
// ---------------------------------------------------------------------------

#[test]
fn a_cylinder_sheet_trimmed_by_a_plane_square_to_its_axis_keeps_the_area_above() {
    let (radius, height, cut) = (10.0, 20.0, 5.0);
    let sheet = cylinder_sheet(radius, height);
    let trimmed = execute(
        &sheet.snapshot,
        KernelCommand::TrimSheetByPlane {
            plane_origin: Point3::new(0.0, 0.0, cut),
            plane_normal: Vector3::new(0.0, 0.0, 1.0),
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&trimmed);
    assert_close(
        trimmed.snapshot.measures().surface_area,
        2.0 * PI * radius * (height - cut),
        "trimmed cylinder sheet area",
    );
    let bounds = trimmed.snapshot.measures().bounds.unwrap();
    assert_close(bounds.min.z, cut, "cut at the plane");
    assert_eq!(trimmed.report.rung.as_deref(), Some("trim/plane"));
}

#[test]
fn a_cylinder_sheet_trimmed_by_an_oblique_plane_keeps_the_area_by_closed_form() {
    // The plane `x + z = 10` through the axis point at half height, tilted
    // at 45 degrees: the cut sits at `z = 10 − 10·cos θ`, so the kept
    // height is `10 + 10·cos θ`, whose mean over a turn is the half height.
    let (radius, height) = (10.0, 20.0);
    let sheet = cylinder_sheet(radius, height);
    let trimmed = execute(
        &sheet.snapshot,
        KernelCommand::TrimSheetByPlane {
            plane_origin: Point3::new(0.0, 0.0, 10.0),
            plane_normal: Vector3::new(1.0, 0.0, 1.0),
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&trimmed);
    assert_close(
        trimmed.snapshot.measures().surface_area,
        2.0 * PI * radius * height / 2.0,
        "obliquely trimmed cylinder sheet area",
    );
}

#[test]
fn a_planar_patch_trimmed_by_a_plane_keeps_the_polygon_on_the_kept_side() {
    let patch = build(KernelCommand::PlanarPatch {
        frame: xy(),
        profile: polygon(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]),
    });
    // `x + y ≥ 10`: the triangle above the diagonal.
    let trimmed = execute(
        &patch.snapshot,
        KernelCommand::TrimSheetByPlane {
            plane_origin: Point3::new(10.0, 0.0, 0.0),
            plane_normal: Vector3::new(1.0, 1.0, 0.0),
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&trimmed);
    assert_close(
        trimmed.snapshot.measures().surface_area,
        50.0,
        "half the square",
    );
    assert_eq!(trimmed.snapshot.counts().faces, 1);
    assert_eq!(boundary_edges(&trimmed), 3);
}

#[test]
fn a_sphere_sheet_trimmed_by_a_plane_square_to_its_axis_is_a_cap() {
    let (radius, cut) = (10.0, 4.0);
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![arc((0.0, 0.0), (0.0, -radius), (0.0, radius))],
        axis: z_axis(),
        angle: RevolveAngle::FullTurn,
    });
    let cap = execute(
        &sheet.snapshot,
        KernelCommand::TrimSheetByPlane {
            plane_origin: Point3::new(0.0, 0.0, cut),
            plane_normal: Vector3::new(0.0, 0.0, 1.0),
        },
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_sheet(&cap);
    // Archimedes: a zone's area is `2πR·h`.
    assert_close(
        cap.snapshot.measures().surface_area,
        2.0 * PI * radius * (radius - cut),
        "spherical cap area",
    );
}

#[test]
fn a_trim_that_leaves_nothing_is_refused_by_name() {
    let sheet = cylinder_sheet(10.0, 20.0);
    let error = execute(
        &sheet.snapshot,
        KernelCommand::TrimSheetByPlane {
            plane_origin: Point3::new(0.0, 0.0, 30.0),
            plane_normal: Vector3::new(0.0, 0.0, 1.0),
        },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["TRIM_RESULT_EMPTY"]);
}

#[test]
fn an_oblique_section_of_a_sphere_is_refused_by_name() {
    let sheet = build(KernelCommand::SurfaceRevolve {
        frame: xz(),
        chain: vec![arc((0.0, 0.0), (0.0, -10.0), (0.0, 10.0))],
        axis: z_axis(),
        angle: RevolveAngle::FullTurn,
    });
    let error = execute(
        &sheet.snapshot,
        KernelCommand::TrimSheetByPlane {
            plane_origin: Point3::new(0.0, 0.0, 2.0),
            plane_normal: Vector3::new(1.0, 0.0, 1.0),
        },
    )
    .unwrap_err();
    assert_eq!(code_of(&error), vec!["TRIM_SECTION_UNSUPPORTED"]);
}

// ---------------------------------------------------------------------------
// STEP
// ---------------------------------------------------------------------------

#[test]
fn a_sheet_exports_to_step_as_an_open_shell_surface_model() {
    let sheet = cylinder_sheet(10.0, 20.0);
    let step = NativeKernel::export_step(&sheet.snapshot, "sheet").unwrap();
    assert!(step.contains("OPEN_SHELL('sheet'"));
    assert!(step.contains("SHELL_BASED_SURFACE_MODEL('sheet'"));
    assert!(step.contains("MANIFOLD_SURFACE_SHAPE_REPRESENTATION("));
    assert!(!step.contains("CLOSED_SHELL"));
    assert!(!step.contains("MANIFOLD_SOLID_BREP"));
    // Every reference resolves to an entity the file declares.
    let data = step.split("DATA;").nth(1).unwrap();
    let declared: std::collections::BTreeSet<u64> = data
        .lines()
        .filter(|line| line.starts_with('#'))
        .map(|line| line[1..line.find('=').unwrap()].parse().unwrap())
        .collect();
    for line in data.lines().filter(|line| line.starts_with('#')) {
        let body = &line[line.find('=').unwrap()..];
        let mut rest = body;
        while let Some(hash) = rest.find('#') {
            let digits: String = rest[hash + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            let id: u64 = digits.parse().unwrap();
            assert!(
                declared.contains(&id),
                "#{id} is referenced but not declared"
            );
            rest = &rest[hash + 1..];
        }
    }
    // The cylinder's two faces and their eight edges are all there.
    assert_eq!(step.matches("ADVANCED_FACE(").count(), 2);
    assert_eq!(step.matches("CYLINDRICAL_SURFACE(").count(), 2);
    assert_eq!(step.matches("EDGE_CURVE(").count(), 6);
}

// ---------------------------------------------------------------------------
// Scripts
// ---------------------------------------------------------------------------

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session
}

#[test]
fn scripts_reach_every_sheet_operation() {
    let session = run(
        "let s = sketch(on: \"XY\", entities: [circle(radius: 10)], label: \"s\");\n\
         let wall = surface_extrude(sketch: s, distance: 20, label: \"wall\");\n\
         let cut = trim(plane: plane(from: \"XY\", offset: 5), label: \"cut\");\n\
         let tube = thicken(thickness: 2, label: \"tube\");\n",
    );
    let report = session.report();
    let rungs: Vec<_> = report
        .steps
        .iter()
        .map(|step| step.rung.clone().unwrap_or_default())
        .collect();
    assert_eq!(
        rungs,
        vec!["", "surface/extrude", "trim/plane", "thicken/exact"]
    );
    assert_close(
        session.snapshot.measures().volume,
        PI * (12.0 * 12.0 - 10.0 * 10.0) * 15.0,
        "trimmed then thickened tube",
    );
    assert_eq!(session.snapshot.counts().solids, 1);
}

#[test]
fn scripts_stitch_patches_and_revolve_surfaces() {
    let session = run(
        "let b = sketch(on: \"XY\", entities: [rect(origin: [0, 0], width: 10, height: 10)], label: \"b\");\n\
         let bottom = patch(sketch: b, label: \"bottom\");\n\
         let t = sketch(on: plane(from: \"XY\", offset: 10), entities: [rect(origin: [0, 0], width: 10, height: 10)], label: \"t\");\n\
         let top = patch(sketch: t, label: \"top\");\n\
         let ring = sketch(on: \"XY\", entities: [rect(origin: [0, 0], width: 10, height: 10)], label: \"ring\");\n\
         let sides = surface_extrude(sketch: ring, distance: 10, label: \"sides\");\n\
         let cube = stitch(sheets: [bottom, sides, top], label: \"cube\");\n",
    );
    assert_close(session.snapshot.measures().volume, 1000.0, "stitched cube");
    let session = run(
        "let s = sketch(on: \"XZ\", entities: [line(start: [5, 0], end: [10, 8])], label: \"s\");\n\
         let cone = surface_revolve(sketch: s, axis: [0, 0, 1], label: \"cone\");\n",
    );
    assert_close(
        session.snapshot.measures().surface_area,
        PI * 15.0 * 89.0_f64.sqrt(),
        "revolved cone sheet",
    );
    assert_eq!(session.snapshot.counts().solids, 0);
}

#[test]
fn scripts_thicken_a_spline_sheet_to_the_approximate_tier() {
    let session = run(
        "let s = sketch(on: \"XY\", entities: [spline(control_points: [[0, 0], [5, 10], [10, 0]], degree: 2)], label: \"s\");\n\
         let sheet = surface_extrude(sketch: s, distance: 20, label: \"sheet\");\n\
         let thick = thicken(thickness: 1, label: \"thick\");\n",
    );
    let report = session.report();
    assert_eq!(report.tier, Tier::Approximate);
    let steps: Vec<_> = report
        .steps
        .iter()
        .map(|step| (step.rung.clone().unwrap_or_default(), step.tier))
        .collect();
    assert_eq!(
        steps,
        vec![
            (String::new(), Tier::Exact),
            ("surface/extrude".to_owned(), Tier::Exact),
            ("thicken/approximate".to_owned(), Tier::Approximate),
        ]
    );
    assert!(
        report.steps[2]
            .warnings
            .iter()
            .any(|warning| warning.code == "SURFACE_OFFSET_APPROXIMATION"),
        "{:?}",
        report.steps[2].warnings
    );
    assert_eq!(session.snapshot.counts().solids, 1);
    let body = report.body.as_ref().expect("a body");
    assert_eq!(body.tier, Tier::Approximate);
    assert_eq!(body.approximate_feature_count, 1);
}

#[test]
fn a_journal_with_sheet_steps_decompiles_to_a_script_that_rebuilds_them() {
    let session = run(
        "let s = sketch(on: \"XY\", entities: [circle(radius: 10)], label: \"s\");\n\
         let wall = surface_extrude(sketch: s, distance: 20, label: \"wall\");\n\
         let cut = trim(plane: plane(origin: [0, 0, 5], normal: [0, 0, 1], x_axis: [1, 0, 0]), label: \"cut\");\n\
         let tube = thicken(thickness: 2, label: \"tube\");\n\
         let b = sketch(on: \"XY\", entities: [rect(origin: [0, 0], width: 4, height: 4)], label: \"b\");\n\
         let p = patch(sketch: b, label: \"p\");\n\
         let c = sketch(on: \"XZ\", entities: [rect(origin: [0, 0], width: 4, height: 4)], label: \"c\");\n\
         let q = patch(sketch: c, label: \"q\");\n\
         let st = stitch(sheets: [p, q], label: \"st\");\n\
         let r = sketch(on: \"XZ\", entities: [line(start: [5, 0], end: [10, 8])], label: \"r\");\n\
         let cone = surface_revolve(sketch: r, axis: [0, 0, 1], angle: 90, label: \"cone\");\n",
    );
    let script = session
        .to_art(&artificer_kernel::api::decompile::DecompileOptions::default())
        .unwrap();
    for word in [
        "surface_extrude(",
        "trim(",
        "thicken(",
        "patch(",
        "stitch(sheets: [p, q]",
        "surface_revolve(",
    ] {
        assert!(script.contains(word), "{word} missing from:\n{script}");
    }
    let again = run(&script);
    assert_eq!(
        again.snapshot.semantic_digest(),
        session.snapshot.semantic_digest()
    );
}

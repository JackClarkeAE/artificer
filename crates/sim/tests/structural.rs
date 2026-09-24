//! The static structural gates of ADR 0058: a cantilever against
//! Euler–Bernoulli, a plate with a hole against the textbook stress
//! concentration, mirror symmetry, and the refusals.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::time::Instant;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{EntityId, EntityKind, EntityRef, Point3, Tier};
use artificer_sim::{
    Load, StructuralError, StructuralResult, StructuralStudy, Support, VoxelMesh, material_by_key,
    solve_static,
};

fn build(source: &str) -> Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

fn cuboid(size: [f64; 3]) -> Snapshot {
    build(&format!(
        "let b = box(origin: [0, 0, 0], size: [{}, {}, {}], label: \"b\");\n",
        size[0], size[1], size[2]
    ))
}

/// The planar face whose outward normal points most nearly along
/// `direction`.
fn face_facing(snapshot: &Snapshot, direction: [f64; 3]) -> EntityRef {
    NativeKernel::describe_faces(snapshot)
        .values()
        .max_by(|left, right| {
            let dot = |face: &artificer_kernel::FaceDescription| {
                face.normal.x * direction[0]
                    + face.normal.y * direction[1]
                    + face.normal.z * direction[2]
            };
            dot(left).total_cmp(&dot(right))
        })
        .map(|face| face.face)
        .expect("a body has faces")
}

fn mesh(snapshot: &Snapshot, cell: f64) -> VoxelMesh {
    let grid = NativeKernel::voxelise(snapshot, cell);
    assert!(!grid.is_empty(), "the grid is empty at cell {cell}");
    VoxelMesh::from_grid(grid)
}

fn solve(mesh: &VoxelMesh, study: &StructuralStudy) -> StructuralResult {
    let started = Instant::now();
    let result = solve_static(mesh, study, &CancellationToken::default(), &mut |_| {})
        .expect("the study is well posed");
    eprintln!(
        "{} voxels, {} nodes: {} iterations to {:.1e} in {:.2?}",
        result.voxels,
        result.nodes,
        result.solve.iterations,
        result.solve.residual,
        started.elapsed()
    );
    assert!(result.solve.converged, "{:?}", result.solve);
    assert_eq!(result.tier, Tier::Approximate);
    result
}

/// A 10 × 10 × 100 mm aluminium cantilever along x, clamped at x = 0 and
/// pushed down with 100 N at the tip.
fn cantilever(cell: f64) -> (VoxelMesh, StructuralStudy) {
    let snapshot = cuboid([100.0, 10.0, 10.0]);
    let root = face_facing(&snapshot, [-1.0, 0.0, 0.0]);
    let tip = face_facing(&snapshot, [1.0, 0.0, 0.0]);
    let mut study = StructuralStudy::new(material_by_key("aluminium-6061").unwrap());
    study.supports.push(Support::Fixed { face: root });
    study.loads.push(Load::Force {
        face: tip,
        newtons: [0.0, 0.0, -100.0],
    });
    study.tolerance = 1.0e-7;
    (mesh(&snapshot, cell), study)
}

/// Euler–Bernoulli tip deflection `F L³ / 3 E I` of that cantilever.
fn euler_bernoulli_tip_deflection() -> f64 {
    let material = material_by_key("aluminium-6061").unwrap();
    let (force, length, depth, width): (f64, f64, f64, f64) = (100.0, 100.0, 10.0, 10.0);
    let inertia = width * depth.powi(3) / 12.0;
    force * length.powi(3) / (3.0 * material.youngs_modulus_mpa * inertia)
}

/// The tip deflection the study reports: the largest downward displacement
/// among the nodes on the loaded face.
fn tip_deflection(mesh: &VoxelMesh, result: &StructuralResult) -> f64 {
    let tip_x = mesh.grid().origin().x + mesh.grid().dims()[0] as f64 * mesh.grid().cell();
    (0..mesh.node_count())
        .filter(|node| (mesh.node_position(*node).x - tip_x).abs() < 1.0e-9)
        .map(|node| -result.displacement[node][2])
        .fold(0.0, f64::max)
}

#[test]
fn cantilever_deflection_approaches_euler_bernoulli_and_converges_monotonically() {
    let exact = euler_bernoulli_tip_deflection();
    let mut errors = Vec::new();
    let mut deflections = Vec::new();
    for cell in [2.5, 5.0 / 3.0, 1.25] {
        let (mesh, study) = cantilever(cell);
        let result = solve(&mesh, &study);
        let deflection = tip_deflection(&mesh, &result);
        eprintln!(
            "cell {cell}: tip {deflection:.4} mm against {exact:.4} mm ({:.1}%), max stress {:.1} MPa, peak node {:.1} MPa, factor {:.2}",
            100.0 * (deflection - exact) / exact,
            result.max_von_mises,
            result.peak_node_von_mises,
            result.safety_factor
        );
        assert!(
            deflection < exact,
            "a voxel mesh is stiffer than the part: {deflection} against {exact}"
        );
        deflections.push(deflection);
        errors.push((exact - deflection) / exact);
        // The root carries the bending stress `M c / I` = 60 MPa; the
        // element centres sit half a cell inside the surface and read
        // under it, and the clamp adds a local concentration over it.
        assert!(
            (30.0..=90.0).contains(&result.max_von_mises),
            "root stress {} MPa",
            result.max_von_mises
        );
        assert!(result.safety_factor > 1.0 && result.safety_factor.is_finite());
        assert_eq!(result.voxels, mesh.element_count());
        assert!((result.total_force[2] + 100.0).abs() < 1.0e-9);
    }
    // Within the discretisation error the coarsest grid is honest about:
    // four elements through the depth of a bending beam.
    assert!(errors[0] < 0.35, "coarse error {:.3}", errors[0]);
    assert!(errors[2] < 0.15, "fine error {:.3}", errors[2]);
    for pair in errors.windows(2) {
        assert!(
            pair[1] < pair[0],
            "the error must fall as the grid is refined: {errors:?}"
        );
    }
    for pair in deflections.windows(2) {
        assert!(pair[1] > pair[0], "{deflections:?}");
    }
}

#[test]
fn a_plate_with_a_hole_concentrates_stress_near_three() {
    // A 120 × 60 × 1 mm plate in tension along x with a 12 mm hole through
    // its middle: d / W = 0.2, for which the gross-section concentration
    // factor is 3.14 (Howland; Peterson's charts).
    let snapshot = build(
        "let plate = box(origin: [0, 0, 0], size: [120, 60, 1], label: \"plate\");\n\
         let bore = cylinder(center: [60, 30, -1], axis: [0, 0, 1], radius: 6, height: 3, label: \"bore\");\n\
         let holed = difference(target: plate, tool: bore, label: \"holed\");\n",
    );
    let left = face_facing(&snapshot, [-1.0, 0.0, 0.0]);
    let right = face_facing(&snapshot, [1.0, 0.0, 0.0]);
    let mut study = StructuralStudy::new(material_by_key("mild-steel").unwrap());
    study.supports.push(Support::Fixed { face: left });
    let tension = 100.0;
    study.loads.push(Load::Force {
        face: right,
        newtons: [tension, 0.0, 0.0],
    });
    let mesh = mesh(&snapshot, 1.0);
    let result = solve(&mesh, &study);
    let nominal = tension / (60.0 * 1.0);
    // The peak sits on the hole's wall at the transverse axis, and the
    // nodal stress there is the extrapolated reading.
    let peak = result.peak_node_von_mises;
    let at = mesh.node_position(result.peak_node);
    let factor = peak / nominal;
    eprintln!(
        "plate: nominal {nominal:.3} MPa, peak node {peak:.3} MPa at {at:?}, factor {factor:.2}; element max {:.3} MPa ({:.2})",
        result.max_von_mises,
        result.max_von_mises / nominal
    );
    assert!(
        (at.x - 60.0).abs() <= 2.0 && ((at.y - 30.0).abs() - 6.0).abs() <= 2.0,
        "the peak is at the hole's transverse wall, not {at:?}"
    );
    assert!(
        (2.4..=3.6).contains(&factor),
        "a stress concentration near 3, read {factor:.2} on a 1 mm grid"
    );
    // Far from the hole and from the clamp the plate is simply in tension.
    let far = mesh.element_in_cell([30, 5, 0]).unwrap();
    assert!(
        (result.von_mises[far] / nominal - 1.0).abs() < 0.08,
        "far field {}",
        result.von_mises[far] / nominal
    );
}

#[test]
fn a_mirrored_load_gives_a_mirrored_field() {
    let cell = 2.5;
    let (mesh, mut study) = cantilever(cell);
    let down = solve(&mesh, &study);
    // The field of a load in -z is symmetric about the beam's mid-plane
    // y = 5: the same z-deflection at mirrored nodes, and opposite
    // y-deflection.
    let mut checked = 0;
    let scale = down.max_deflection;
    for node in 0..mesh.node_count() {
        let position = mesh.node_position(node);
        let mirrored = Point3::new(position.x, 10.0 - position.y, position.z);
        let twin = (0..mesh.node_count())
            .find(|other| {
                let other = mesh.node_position(*other);
                (other.x - mirrored.x).abs() < 1.0e-9
                    && (other.y - mirrored.y).abs() < 1.0e-9
                    && (other.z - mirrored.z).abs() < 1.0e-9
            })
            .expect("a voxel grid is symmetric");
        let [ux, uy, uz] = down.displacement[node];
        let [tx, ty, tz] = down.displacement[twin];
        assert!((ux - tx).abs() <= 1.0e-6 * scale, "{node}: {ux} vs {tx}");
        assert!((uy + ty).abs() <= 1.0e-6 * scale, "{node}: {uy} vs {ty}");
        assert!((uz - tz).abs() <= 1.0e-6 * scale, "{node}: {uz} vs {tz}");
        checked += 1;
    }
    assert_eq!(checked, mesh.node_count());

    // Turning the load from -z to -y on a square section turns the whole
    // field with it: every node's (y, z) displacement swaps.
    study.loads = vec![Load::Force {
        face: study.loads[0].face().unwrap(),
        newtons: [0.0, -100.0, 0.0],
    }];
    let sideways = solve(&mesh, &study);
    for node in 0..mesh.node_count() {
        let position = mesh.node_position(node);
        let swapped = Point3::new(position.x, position.z, position.y);
        let twin = (0..mesh.node_count())
            .find(|other| {
                let other = mesh.node_position(*other);
                (other.x - swapped.x).abs() < 1.0e-9
                    && (other.y - swapped.y).abs() < 1.0e-9
                    && (other.z - swapped.z).abs() < 1.0e-9
            })
            .expect("a square section is symmetric under the swap");
        let [ux, uy, uz] = down.displacement[node];
        let [sx, sy, sz] = sideways.displacement[twin];
        assert!((ux - sx).abs() <= 1.0e-6 * scale);
        assert!((uy - sz).abs() <= 1.0e-6 * scale);
        assert!((uz - sy).abs() <= 1.0e-6 * scale);
    }
    assert!((down.max_deflection - sideways.max_deflection).abs() <= 1.0e-6 * scale);
}

#[test]
fn a_study_with_no_fixed_face_is_refused_by_name() {
    let snapshot = cuboid([20.0, 10.0, 10.0]);
    let mesh = mesh(&snapshot, 2.5);
    let mut study = StructuralStudy::new(material_by_key("pla").unwrap());
    study.loads.push(Load::Gravity {
        direction: [0.0, 0.0, -1.0],
    });
    let refused = solve_static(&mesh, &study, &CancellationToken::default(), &mut |_| {});
    assert_eq!(refused, Err(StructuralError::NoSupports));
    assert_eq!(
        StructuralError::NoSupports.to_string(),
        "no fixed faces: the part would move as a rigid body under any load"
    );

    // A support on a face the grid has no cells for is refused by the face.
    let nowhere = EntityRef {
        snapshot: snapshot.id(),
        entity: EntityId(9_999),
        kind: EntityKind::Face,
    };
    study.supports.push(Support::Fixed { face: nowhere });
    let refused = solve_static(&mesh, &study, &CancellationToken::default(), &mut |_| {});
    assert_eq!(
        refused,
        Err(StructuralError::SupportOnNoCells { face: nowhere })
    );

    // And a well-posed study with nothing pushing on it is refused too.
    let held = StructuralStudy {
        supports: vec![Support::Fixed {
            face: face_facing(&snapshot, [-1.0, 0.0, 0.0]),
        }],
        loads: Vec::new(),
        ..study
    };
    assert_eq!(
        solve_static(&mesh, &held, &CancellationToken::default(), &mut |_| {}),
        Err(StructuralError::NoLoad)
    );
}

#[test]
fn an_empty_grid_is_refused_and_a_cancelled_solve_says_so() {
    let snapshot = cuboid([20.0, 10.0, 10.0]);
    let empty = VoxelMesh::from_grid(NativeKernel::voxelise(&snapshot, f64::NAN));
    let mut study = StructuralStudy::new(material_by_key("abs").unwrap());
    study.supports.push(Support::Fixed {
        face: face_facing(&snapshot, [-1.0, 0.0, 0.0]),
    });
    study.loads.push(Load::Pressure {
        face: face_facing(&snapshot, [0.0, 0.0, 1.0]),
        megapascals: 0.5,
    });
    assert_eq!(
        solve_static(&empty, &study, &CancellationToken::default(), &mut |_| {}),
        Err(StructuralError::EmptyGrid)
    );

    let mesh = mesh(&snapshot, 2.5);
    let token = CancellationToken::default();
    token.cancel();
    let result = solve_static(&mesh, &study, &token, &mut |_| {}).unwrap();
    assert!(result.solve.cancelled);
    assert!(!result.solve.converged);
    assert_eq!(result.solve.iterations, 0);
    // A pressure of 0.5 MPa on a 20 × 10 face pushes with 100 N into
    // the part, which is downward.
    assert!(
        (result.total_force[2] + 100.0).abs() < 1.0e-9,
        "{:?}",
        result.total_force
    );
}

#[test]
fn the_same_study_solves_to_the_same_bits_every_run() {
    let hash = |result: &StructuralResult| {
        let mut hasher = DefaultHasher::new();
        for [x, y, z] in &result.displacement {
            x.to_bits().hash(&mut hasher);
            y.to_bits().hash(&mut hasher);
            z.to_bits().hash(&mut hasher);
        }
        for stress in &result.von_mises {
            stress.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    };
    let (mesh, study) = cantilever(2.5);
    let first = solve(&mesh, &study);
    let second = solve(&mesh, &study);
    assert_eq!(hash(&first), hash(&second));
    assert_eq!(first.solve, second.solve);
}

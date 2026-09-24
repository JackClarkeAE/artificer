//! The thermal gate of ADR 0058: a bar held at two temperatures is linear
//! along its length, and the refusals are named.

use std::collections::BTreeMap;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{EntityRef, Tier};
use artificer_sim::{
    Convection, ThermalError, ThermalStudy, VoxelMesh, material_by_key, solve_steady_state,
};

fn cuboid(size: [f64; 3]) -> Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(
        &format!(
            "let b = box(origin: [0, 0, 0], size: [{}, {}, {}], label: \"b\");\n",
            size[0], size[1], size[2]
        ),
        &BTreeMap::new(),
        &CancellationToken::default(),
    );
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

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

#[test]
fn a_bar_between_two_temperatures_is_linear_along_its_length() {
    let snapshot = cuboid([100.0, 10.0, 10.0]);
    let mesh = VoxelMesh::from_grid(NativeKernel::voxelise(&snapshot, 2.5));
    let mut study = ThermalStudy::new(material_by_key("aluminium-6061").unwrap());
    study
        .held
        .push((face_facing(&snapshot, [-1.0, 0.0, 0.0]), 100.0));
    study
        .held
        .push((face_facing(&snapshot, [1.0, 0.0, 0.0]), 0.0));
    let result = solve_steady_state(&mesh, &study, &CancellationToken::default(), &mut |_| {})
        .expect("a well-posed bar");
    assert!(result.solve.converged, "{:?}", result.solve);
    assert_eq!(result.tier, Tier::Approximate);
    assert_eq!(result.voxels, 4 * 4 * 40);
    assert_eq!(result.convecting_sides, 0);
    for node in 0..mesh.node_count() {
        let x = mesh.node_position(node).x;
        let expected = 100.0 * (1.0 - x / 100.0);
        assert!(
            (result.temperature[node] - expected).abs() < 1.0e-6,
            "node {node} at x {x}: {} against {expected}",
            result.temperature[node]
        );
    }
    assert!((result.max - 100.0).abs() < 1.0e-9);
    assert!(result.min.abs() < 1.0e-9);
    // Element temperatures are the means of their corners, so the first
    // column reads the mean of 100 and 97.5.
    let first = mesh.element_in_cell([0, 0, 0]).unwrap();
    assert!((result.element_temperature[first] - 98.75).abs() < 1.0e-6);
}

/// An aluminium pin fin, 10 × 10 × 100 mm, held at 100 °C at its root in
/// 20 °C air with a film coefficient of 25 W/m²K: the textbook fin with
/// `m = √(hP / kA)` cools as `cosh(m(L − x)) / cosh(mL)` along its length.
#[test]
fn a_fin_losing_heat_to_the_air_follows_the_textbook_profile() {
    let snapshot = cuboid([100.0, 10.0, 10.0]);
    let mesh = VoxelMesh::from_grid(NativeKernel::voxelise(&snapshot, 2.5));
    let material = material_by_key("aluminium-6061").unwrap();
    let mut study = ThermalStudy::new(material);
    study
        .held
        .push((face_facing(&snapshot, [-1.0, 0.0, 0.0]), 100.0));
    let (film, ambient) = (25.0, 20.0);
    study.convection = Some(Convection {
        coefficient_w_m2k: film,
        ambient_c: ambient,
    });
    let result = solve_steady_state(&mesh, &study, &CancellationToken::default(), &mut |_| {})
        .expect("a fin");
    assert!(result.solve.converged, "{:?}", result.solve);
    assert!(result.convecting_sides > 0);
    // m in 1/mm: h in W/mm²K, P in mm, k in W/mmK, A in mm².
    let m = (film * 1.0e-6 * 40.0 / (material.conductivity_w_mmk() * 100.0)).sqrt();
    let length = 100.0;
    let profile =
        |x: f64| ambient + (100.0 - ambient) * (m * (length - x)).cosh() / (m * length).cosh();
    for (cell, x) in [(10, 26.25), (20, 51.25), (39, 98.75)] {
        let element = mesh.element_in_cell([cell, 1, 1]).unwrap();
        let read = result.element_temperature[element];
        let expected = profile(x);
        assert!(
            (read - expected).abs() / (expected - ambient) < 0.1,
            "at x {x}: {read} °C against the fin's {expected:.2} °C"
        );
    }
    assert!(result.min >= ambient - 1.0e-6, "{}", result.min);
    assert!(result.max <= 100.0 + 1.0e-6, "{}", result.max);
}

#[test]
fn a_study_with_nothing_holding_a_temperature_is_refused_by_name() {
    let snapshot = cuboid([20.0, 10.0, 10.0]);
    let mesh = VoxelMesh::from_grid(NativeKernel::voxelise(&snapshot, 2.5));
    let study = ThermalStudy::new(material_by_key("pla").unwrap());
    assert_eq!(
        solve_steady_state(&mesh, &study, &CancellationToken::default(), &mut |_| {}),
        Err(ThermalError::NoTemperatures)
    );
    let empty = VoxelMesh::from_grid(NativeKernel::voxelise(&snapshot, f64::NAN));
    assert_eq!(
        solve_steady_state(&empty, &study, &CancellationToken::default(), &mut |_| {}),
        Err(ThermalError::EmptyGrid)
    );
    let mut nowhere = ThermalStudy::new(material_by_key("pla").unwrap());
    let face = EntityRef {
        snapshot: snapshot.id(),
        entity: artificer_protocol::EntityId(4_242),
        kind: artificer_protocol::EntityKind::Face,
    };
    nowhere.held.push((face, 50.0));
    assert_eq!(
        solve_steady_state(&mesh, &nowhere, &CancellationToken::default(), &mut |_| {}),
        Err(ThermalError::FaceOnNoCells { face })
    );
}

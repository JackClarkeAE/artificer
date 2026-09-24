//! The topology-optimisation gate of ADR 0058: the MBB beam at the classic
//! parameters reproduces the well-known layout — a truss of chords and
//! diagonals with holes between them, half the material gone.

use std::collections::BTreeSet;
use std::time::Instant;

use artificer_kernel::{CancellationToken, VoxelGrid};
use artificer_protocol::{Point3, Tier};
use artificer_sim::{
    Conditions, TopologyError, TopologyResult, TopologyStudy, VoxelMesh, material_by_key,
    optimise_topology,
};

const WIDTH: usize = 60;
const HEIGHT: usize = 20;

/// The half MBB beam of top88: 60 × 20 cells one cell thick, symmetric at
/// the left edge, on a roller at the bottom-right corner, pushed down at
/// the top-left corner, held in plane.
fn mbb() -> (VoxelMesh, Conditions) {
    let grid = VoxelGrid::new(
        Point3::new(0.0, 0.0, 0.0),
        1.0,
        [WIDTH, HEIGHT, 1],
        vec![true; WIDTH * HEIGHT],
        Vec::new(),
    );
    let mesh = VoxelMesh::from_grid(grid);
    let mut conditions = Conditions::unconstrained(&mesh);
    for node in 0..mesh.node_count() {
        let [x, y, _] = mesh.node_corner(node);
        // One cell thick: every node is held out of plane, which is plane
        // strain through the thickness.
        conditions.hold(node as u32, [x == 0, x == WIDTH && y == 0, true]);
        if x == 0 && y == HEIGHT {
            conditions.push(node as u32, [0.0, -0.5, 0.0]);
        }
    }
    (mesh, conditions)
}

fn layout(mesh: &VoxelMesh, result: &TopologyResult) -> Vec<Vec<f64>> {
    let mut rows = vec![vec![0.0; WIDTH]; HEIGHT];
    for element in 0..mesh.element_count() {
        let [x, y, _] = mesh.cell_of_element(element);
        rows[y][x] = result.densities[element];
    }
    rows
}

fn print_layout(rows: &[Vec<f64>]) {
    for row in rows.iter().rev() {
        let line = row
            .iter()
            .map(|density| {
                if *density >= 0.7 {
                    '#'
                } else if *density >= 0.4 {
                    '+'
                } else if *density >= 0.15 {
                    '.'
                } else {
                    ' '
                }
            })
            .collect::<String>();
        eprintln!("|{line}|");
    }
}

/// The four-connected regions of cells below a density, as sets of cells.
fn void_regions(rows: &[Vec<f64>], threshold: f64) -> Vec<BTreeSet<(usize, usize)>> {
    let mut seen = BTreeSet::new();
    let mut regions = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if rows[y][x] >= threshold || seen.contains(&(x, y)) {
                continue;
            }
            let mut region = BTreeSet::new();
            let mut stack = vec![(x, y)];
            while let Some((cx, cy)) = stack.pop() {
                if !seen.insert((cx, cy)) {
                    continue;
                }
                region.insert((cx, cy));
                let mut neighbours = Vec::new();
                if cx > 0 {
                    neighbours.push((cx - 1, cy));
                }
                if cx + 1 < WIDTH {
                    neighbours.push((cx + 1, cy));
                }
                if cy > 0 {
                    neighbours.push((cx, cy - 1));
                }
                if cy + 1 < HEIGHT {
                    neighbours.push((cx, cy + 1));
                }
                for (nx, ny) in neighbours {
                    if rows[ny][nx] < threshold && !seen.contains(&(nx, ny)) {
                        stack.push((nx, ny));
                    }
                }
            }
            regions.push(region);
        }
    }
    regions
}

#[test]
fn the_mbb_beam_reproduces_the_classic_truss_layout() {
    let (mesh, conditions) = mbb();
    let mut study = TopologyStudy::new(material_by_key("mild-steel").unwrap(), conditions, 0.5);
    study.max_iterations = 60;
    let started = Instant::now();
    let mut iterations_seen = Vec::new();
    let result = optimise_topology(
        &mesh,
        &study,
        &CancellationToken::default(),
        &mut |progress| {
            iterations_seen.push((progress.iteration, progress.compliance, progress.change));
        },
    )
    .expect("a well-posed beam");
    eprintln!(
        "{} iterations in {:.2?}, compliance {:.4} → {:.4}, volume {:.3}",
        result.iterations,
        started.elapsed(),
        result.compliance_history[0],
        result.compliance_history[result.compliance_history.len() - 1],
        result.volume_fraction
    );
    let rows = layout(&mesh, &result);
    print_layout(&rows);

    assert_eq!(result.tier, Tier::Approximate);
    assert_eq!(result.voxels, WIDTH * HEIGHT);
    assert_eq!(iterations_seen.len(), result.iterations);
    assert!(result.iterations >= 10, "{}", result.iterations);
    // Half the material, kept exactly by the bisection.
    assert!(
        (result.volume_fraction - 0.5).abs() < 0.01,
        "{}",
        result.volume_fraction
    );
    // The structure got stiffer as it was carved.
    let first = result.compliance_history[0];
    let last = *result.compliance_history.last().unwrap();
    assert!(last < first, "compliance {first} → {last}");
    // Material under the load and at the support.
    assert!(
        (0..=2).all(|x| rows[HEIGHT - 1][x] > 0.5),
        "a chord under the load"
    );
    assert!(
        (57..=59).all(|x| rows[0][x] > 0.5),
        "material at the support"
    );
    // The classic layout is a truss: a top chord, a bottom chord towards
    // the support, diagonals between, and holes between the members.
    let voids = void_regions(&rows, 0.3);
    let large = voids.iter().filter(|region| region.len() >= 6).count();
    assert!(
        large >= 3,
        "{large} holes of six cells or more in {} voids",
        voids.len()
    );
    // Most of the field is decided: material or void, not grey.
    let decided = result
        .densities
        .iter()
        .filter(|density| **density < 0.2 || **density > 0.8)
        .count();
    assert!(
        decided as f64 / result.densities.len() as f64 > 0.7,
        "{decided} of {} decided",
        result.densities.len()
    );
}

#[test]
fn the_optimisation_is_deterministic_and_refuses_by_name() {
    let (mesh, conditions) = mbb();
    let mut study = TopologyStudy::new(
        material_by_key("mild-steel").unwrap(),
        conditions.clone(),
        0.5,
    );
    study.max_iterations = 5;
    let first =
        optimise_topology(&mesh, &study, &CancellationToken::default(), &mut |_| {}).unwrap();
    let second =
        optimise_topology(&mesh, &study, &CancellationToken::default(), &mut |_| {}).unwrap();
    assert_eq!(first.densities, second.densities);
    assert_eq!(first.compliance_history, second.compliance_history);
    assert_eq!(first.iterations, 5);
    assert!(!first.converged);

    let mut unheld = study.clone();
    unheld.conditions = Conditions::unconstrained(&mesh);
    assert_eq!(
        optimise_topology(&mesh, &unheld, &CancellationToken::default(), &mut |_| {}),
        Err(TopologyError::NoSupports)
    );
    let mut unloaded = study.clone();
    unloaded.conditions.force.fill(0.0);
    assert_eq!(
        optimise_topology(&mesh, &unloaded, &CancellationToken::default(), &mut |_| {}),
        Err(TopologyError::NoLoad)
    );
    let mut nothing_left = study.clone();
    nothing_left.volume_fraction = 1.5;
    assert_eq!(
        optimise_topology(
            &mesh,
            &nothing_left,
            &CancellationToken::default(),
            &mut |_| {}
        ),
        Err(TopologyError::VolumeFraction(1.5))
    );

    let token = CancellationToken::default();
    token.cancel();
    let cancelled = optimise_topology(&mesh, &study, &token, &mut |_| {}).unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.iterations, 0);
}

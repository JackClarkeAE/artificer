//! Turns an ideal mesh into what a scanner would return for it.
//!
//! The pipeline's real inputs are scans, and the two reference scans it
//! was tuned on are not in the repository — so the fixtures that pin
//! its behaviour have to be *made*, from geometry we control, with the
//! defects a scanner actually introduces. Each defect here is the
//! physical one:
//!
//! - **Refinement** first, because a CAD export spends one triangle on
//!   a whole wall and a scanner spends one per sample spot. Every
//!   later stage assumes scan density; so does the pipeline (its
//!   occupancy grids read coarse meshes as mostly empty space).
//! - **Spot smoothing** is why scanners round every crease: a
//!   measurement integrates the surface over the spot, so geometry
//!   smaller than the spot radius averages away. Moving-least-squares
//!   projection with a Gaussian window is that integral.
//! - **Noise** displaces each sample along the direction it was
//!   measured — the surface normal.
//! - **Dropout** removes patches outright: occlusion, gloss, steep
//!   incidence. A hole is different evidence than noise, and the
//!   hygiene stage exists because of exactly this difference.
//!
//! Everything is deterministic under a seed, matching the project's
//! invariant: the same input and options give byte-identical output.

use crate::mesh::TriangleMesh;
use artificer_geometry::{Point3, Vector3};

/// The scanner being simulated.
#[derive(Clone, Copy, Debug)]
pub struct SimulateOptions {
    /// Target sample spacing (mm): edges longer than 1.5x this split
    /// until the mesh reaches scan density. Zero skips refinement.
    pub density: f64,
    /// Spot radius (mm): geometry smaller than this rounds away, which
    /// is what makes simulated scans exercise the RANSAC peeling the
    /// way real ones do. Zero skips smoothing.
    pub smooth: f64,
    /// Gaussian noise sigma (mm) along the surface normal.
    pub noise: f64,
    /// How many dropout holes to punch.
    pub dropout: usize,
    /// Diameter (mm) of each dropout hole.
    pub dropout_size: f64,
    /// RNG seed; the same seed reproduces the same scan exactly.
    pub seed: u64,
}

impl Default for SimulateOptions {
    fn default() -> Self {
        SimulateOptions {
            density: 0.4,
            smooth: 0.35,
            noise: 0.02,
            dropout: 0,
            dropout_size: 6.0,
            seed: 0x005c_a9ed,
        }
    }
}

/// A simulated scan and the account of what was done to make it.
pub struct SimulatedScan {
    pub mesh: TriangleMesh,
    pub notes: Vec<String>,
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in (0, 1] — the open end matters for Box-Muller's log.
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    /// Standard Gaussian by Box-Muller.
    fn gaussian(&mut self) -> f64 {
        let (u1, u2) = (self.unit(), self.unit());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Splits every edge longer than 1.5x the target until none remains.
///
/// Midpoints are shared through an edge map so neighbouring triangles
/// stay welded, and each pass rewrites a triangle by how many of its
/// edges split: one gives two triangles, two give three, three give
/// the standard four-way subdivision.
fn refine_to_density(
    positions: &mut Vec<Point3>,
    triangles: &mut Vec<[u32; 3]>,
    target: f64,
) -> usize {
    const MAX_TRIANGLES: usize = 12_000_000;
    let limit = target * 1.5;
    let limit_squared = limit * limit;
    let mut passes = 0usize;
    for _ in 0..24 {
        let mut midpoint: std::collections::HashMap<(u32, u32), u32> =
            std::collections::HashMap::new();
        let key = |a: u32, b: u32| (a.min(b), a.max(b));
        let mut next: Vec<[u32; 3]> = Vec::with_capacity(triangles.len());
        let mut any = false;
        for &[a, b, c] in triangles.iter() {
            // A triangle already below cell area is a sliver whose
            // splitting adds count without density — and on a knife
            // sliver the split cascades through its neighbours until
            // the cap. Carry it as it is.
            let (pa, pb, pc) = (
                positions[a as usize],
                positions[b as usize],
                positions[c as usize],
            );
            let edge_lengths = [(pb - pa).length(), (pc - pb).length(), (pa - pc).length()];
            let shortest = edge_lengths.iter().copied().fold(f64::INFINITY, f64::min);
            let longest = edge_lengths.iter().copied().fold(0.0f64, f64::max);
            if (pb - pa).cross(pc - pa).length() / 2.0 < target * target / 32.0
                || (shortest < target / 4.0 && longest > 8.0 * target)
            {
                // Below cell area, or a knife whose short edge is
                // already deep sub-cell: splitting such a triangle
                // adds count without density, and on a knife the
                // split's new diagonals cascade through neighbours
                // until the cap. An honest sliver wider than a
                // quarter-cell still refines normally.
                next.push([a, b, c]);
                continue;
            }
            let mut split = |from: u32, to: u32, positions: &mut Vec<Point3>| -> Option<u32> {
                let (p, q) = (positions[from as usize], positions[to as usize]);
                let d = q - p;
                if d.dot(d) <= limit_squared {
                    return None;
                }
                Some(*midpoint.entry(key(from, to)).or_insert_with(|| {
                    positions.push(Point3::new(
                        (p.x + q.x) / 2.0,
                        (p.y + q.y) / 2.0,
                        (p.z + q.z) / 2.0,
                    ));
                    (positions.len() - 1) as u32
                }))
            };
            let mab = split(a, b, positions);
            let mbc = split(b, c, positions);
            let mca = split(c, a, positions);
            any |= mab.is_some() || mbc.is_some() || mca.is_some();
            match (mab, mbc, mca) {
                (None, None, None) => next.push([a, b, c]),
                (Some(m), None, None) => {
                    next.push([a, m, c]);
                    next.push([m, b, c]);
                }
                (None, Some(m), None) => {
                    next.push([b, m, a]);
                    next.push([m, c, a]);
                }
                (None, None, Some(m)) => {
                    next.push([c, m, b]);
                    next.push([m, a, b]);
                }
                (Some(x), Some(y), None) => {
                    next.push([x, b, y]);
                    next.push([a, x, y]);
                    next.push([a, y, c]);
                }
                (None, Some(x), Some(y)) => {
                    next.push([x, c, y]);
                    next.push([b, x, y]);
                    next.push([b, y, a]);
                }
                (Some(x), None, Some(y)) => {
                    next.push([a, x, y]);
                    next.push([x, b, c]);
                    next.push([y, x, c]);
                }
                (Some(x), Some(y), Some(z)) => {
                    next.push([a, x, z]);
                    next.push([x, b, y]);
                    next.push([z, y, c]);
                    next.push([x, y, z]);
                }
            }
        }
        *triangles = next;
        passes += 1;
        if !any || triangles.len() > MAX_TRIANGLES {
            break;
        }
    }
    passes
}

/// Area-weighted face samples hashed into cells one gather-radius wide,
/// so a neighbourhood is always the 27 cells around a point.
struct SampleField {
    cells: std::collections::HashMap<(i64, i64, i64), Vec<u32>>,
    cell: f64,
    centroids: Vec<Point3>,
    normals: Vec<Vector3>,
    areas: Vec<f64>,
}

impl SampleField {
    fn build(mesh_positions: &[Point3], triangles: &[[u32; 3]], cell: f64) -> Self {
        let mut field = SampleField {
            cells: std::collections::HashMap::new(),
            cell,
            centroids: Vec::with_capacity(triangles.len()),
            normals: Vec::with_capacity(triangles.len()),
            areas: Vec::with_capacity(triangles.len()),
        };
        for (index, &[a, b, c]) in triangles.iter().enumerate() {
            let (pa, pb, pc) = (
                mesh_positions[a as usize],
                mesh_positions[b as usize],
                mesh_positions[c as usize],
            );
            let cross = (pb - pa).cross(pc - pa);
            let doubled = cross.length();
            let centroid = Point3::new(
                (pa.x + pb.x + pc.x) / 3.0,
                (pa.y + pb.y + pc.y) / 3.0,
                (pa.z + pb.z + pc.z) / 3.0,
            );
            field.centroids.push(centroid);
            field.normals.push(if doubled > 1e-12 {
                cross / doubled
            } else {
                Vector3::new(0.0, 0.0, 0.0)
            });
            field.areas.push(doubled / 2.0);
            field
                .cells
                .entry(field.key(centroid))
                .or_default()
                .push(index as u32);
        }
        field
    }

    fn key(&self, point: Point3) -> (i64, i64, i64) {
        (
            (point.x / self.cell).floor() as i64,
            (point.y / self.cell).floor() as i64,
            (point.z / self.cell).floor() as i64,
        )
    }

    fn gather(&self, around: Point3, mut visit: impl FnMut(u32)) {
        let base = self.key(around);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(bucket) = self.cells.get(&(base.0 + dx, base.1 + dy, base.2 + dz)) {
                        for &index in bucket {
                            visit(index);
                        }
                    }
                }
            }
        }
    }
}

/// Per-vertex normals from adjacent faces, for noise direction and the
/// thin-wall guard.
fn vertex_normals(positions: &[Point3], triangles: &[[u32; 3]]) -> Vec<Vector3> {
    let mut normals = vec![Vector3::new(0.0, 0.0, 0.0); positions.len()];
    for &[a, b, c] in triangles {
        let (pa, pb, pc) = (
            positions[a as usize],
            positions[b as usize],
            positions[c as usize],
        );
        let cross = (pb - pa).cross(pc - pa);
        for vertex in [a, b, c] {
            normals[vertex as usize] = normals[vertex as usize] + cross;
        }
    }
    for normal in &mut normals {
        let length = normal.length();
        if length > 1e-12 {
            *normal = *normal / length;
        }
    }
    normals
}

/// One moving-least-squares pass: each vertex lands on the Gaussian-
/// weighted average plane of the surface within the spot radius.
///
/// No normal filter screens the gather — the *distance* cutoff is the
/// wall guard, because material on the far side of a wall thicker than
/// the spot is simply out of reach. A filter by normal agreement was
/// tried and dug trenches instead of rounding: the crease vertex
/// gathered both sides and sank while its one-sided neighbours stood
/// still. A wall thinner than the spot leaves the window's normals
/// disagreeing, and the vertex honestly stays put — which is also
/// roughly where a real scanner starts failing.
fn smooth_pass(positions: &mut [Point3], field: &SampleField, radius: f64) {
    let sigma = radius / 2.0;
    let radius_squared = radius * radius;
    for position in positions.iter_mut() {
        let mut weight_sum = 0.0f64;
        let mut centroid = Vector3::new(0.0, 0.0, 0.0);
        let mut normal = Vector3::new(0.0, 0.0, 0.0);
        field.gather(*position, |face| {
            let towards = field.centroids[face as usize] - *position;
            let distance_squared = towards.dot(towards);
            if distance_squared > radius_squared {
                return;
            }
            let weight =
                field.areas[face as usize] * (-distance_squared / (2.0 * sigma * sigma)).exp();
            weight_sum += weight;
            centroid = centroid + towards * weight;
            normal = normal + field.normals[face as usize] * weight;
        });
        if weight_sum <= 1e-12 {
            continue;
        }
        let length = normal.length();
        if length / weight_sum < 0.3 {
            // The window saw disagreeing material — a sub-spot wall's
            // two sides; moving on its average would invent geometry.
            continue;
        }
        let normal = normal / length;
        let centroid = centroid / weight_sum;
        // Project onto the local plane: only the offset along the
        // normal moves, and never further than the spot itself.
        let offset = normal.dot(centroid).clamp(-radius, radius);
        *position = Point3::new(
            position.x + normal.x * offset,
            position.y + normal.y * offset,
            position.z + normal.z * offset,
        );
    }
}

/// Simulates a scan of `mesh`. The input is read as an ideal surface;
/// the output is what a scanner with these options would hand back.
pub fn simulate_scan(mesh: &TriangleMesh, options: &SimulateOptions) -> SimulatedScan {
    let mut notes = Vec::new();
    let mut positions: Vec<Point3> = mesh.positions().to_vec();
    let mut triangles: Vec<[u32; 3]> = mesh.triangles().to_vec();
    let mut rng = SplitMix64(options.seed);
    if options.density > 0.0 {
        let before = triangles.len();
        let passes = refine_to_density(&mut positions, &mut triangles, options.density);
        notes.push(format!(
            "refined {} -> {} triangles in {} pass(es) toward {:.2} mm spacing",
            before,
            triangles.len(),
            passes,
            options.density
        ));
    }
    if options.smooth > 0.0 {
        // The field holds the ORIGINAL surface; iterating the
        // projection against it converges each vertex onto the
        // moving-least-squares surface rather than drifting — three
        // passes settle it in practice.
        let field = SampleField::build(&positions, &triangles, options.smooth);
        for _ in 0..3 {
            smooth_pass(&mut positions, &field, options.smooth);
        }
        notes.push(format!(
            "spot smoothing at radius {:.2} mm rounded the creases",
            options.smooth
        ));
    }
    if options.noise > 0.0 {
        let normals = vertex_normals(&positions, &triangles);
        for (position, normal) in positions.iter_mut().zip(&normals) {
            let along = options.noise * rng.gaussian();
            *position = Point3::new(
                position.x + normal.x * along,
                position.y + normal.y * along,
                position.z + normal.z * along,
            );
        }
        notes.push(format!(
            "measurement noise sigma {:.3} mm along the normals",
            options.noise
        ));
    }
    if options.dropout > 0 && options.dropout_size > 0.0 {
        // Seeds land where there is area to lose, picked by cumulative
        // area so a big wall attracts more holes than a sliver.
        let mut cumulative: Vec<f64> = Vec::with_capacity(triangles.len());
        let mut total = 0.0;
        for &[a, b, c] in &triangles {
            let (pa, pb, pc) = (
                positions[a as usize],
                positions[b as usize],
                positions[c as usize],
            );
            total += (pb - pa).cross(pc - pa).length() / 2.0;
            cumulative.push(total);
        }
        let mut seeds: Vec<Point3> = Vec::new();
        for _ in 0..options.dropout {
            let target = rng.unit() * total;
            let face = cumulative.partition_point(|&sum| sum < target);
            let [a, b, c] = triangles[face.min(triangles.len() - 1)];
            let (pa, pb, pc) = (
                positions[a as usize],
                positions[b as usize],
                positions[c as usize],
            );
            seeds.push(Point3::new(
                (pa.x + pb.x + pc.x) / 3.0,
                (pa.y + pb.y + pc.y) / 3.0,
                (pa.z + pb.z + pc.z) / 3.0,
            ));
        }
        let radius = options.dropout_size / 2.0;
        let before = triangles.len();
        let mut kept: Vec<[u32; 3]> = Vec::with_capacity(triangles.len());
        for &[a, b, c] in &triangles {
            let centroid = Point3::new(
                (positions[a as usize].x + positions[b as usize].x + positions[c as usize].x) / 3.0,
                (positions[a as usize].y + positions[b as usize].y + positions[c as usize].y) / 3.0,
                (positions[a as usize].z + positions[b as usize].z + positions[c as usize].z) / 3.0,
            );
            let nearest = seeds
                .iter()
                .map(|seed| (*seed - centroid).length())
                .fold(f64::INFINITY, f64::min);
            // Solid loss inside, ragged in the outer band — a scanner's
            // holes do not have clean rims.
            let drop = nearest < radius * 0.7 || (nearest < radius && rng.next().is_multiple_of(2));
            if !drop {
                kept.push([a, b, c]);
            }
        }
        notes.push(format!(
            "{} dropout hole(s) of ~{:.1} mm removed {} triangle(s)",
            options.dropout,
            options.dropout_size,
            before - kept.len()
        ));
        triangles = kept;
    }
    // Compact away vertices nothing references, then hand back a mesh.
    let mut remap: Vec<u32> = vec![u32::MAX; positions.len()];
    let mut packed: Vec<Point3> = Vec::with_capacity(positions.len());
    for triangle in &mut triangles {
        for slot in triangle.iter_mut() {
            let old = *slot as usize;
            if remap[old] == u32::MAX {
                remap[old] = packed.len() as u32;
                packed.push(positions[old]);
            }
            *slot = remap[old];
        }
    }
    let mesh =
        TriangleMesh::new(packed, triangles).expect("a simulated scan keeps at least one triangle");
    SimulatedScan { mesh, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    fn max_edge(mesh: &TriangleMesh) -> f64 {
        mesh.triangles()
            .iter()
            .flat_map(|&[a, b, c]| {
                let (pa, pb, pc) = (
                    mesh.positions()[a as usize],
                    mesh.positions()[b as usize],
                    mesh.positions()[c as usize],
                );
                [(pb - pa).length(), (pc - pb).length(), (pa - pc).length()]
            })
            .fold(0.0, f64::max)
    }

    fn plate() -> TriangleMesh {
        synth::plate_with_boss()
    }

    #[test]
    fn refinement_reaches_scan_density() {
        let scan = simulate_scan(
            &plate(),
            &SimulateOptions {
                density: 1.0,
                smooth: 0.0,
                noise: 0.0,
                dropout: 0,
                ..SimulateOptions::default()
            },
        );
        assert!(
            max_edge(&scan.mesh) <= 1.5 + 1e-9,
            "longest edge {:.2}",
            max_edge(&scan.mesh)
        );
        assert!(scan.mesh.triangles().len() > plate().triangles().len() * 10);
    }

    /// The spot integral rounds a crease: across the plate's sharp
    /// edges the worst neighbouring-face dihedral drops well below the
    /// 90 degrees the ideal solid has.
    #[test]
    fn smoothing_rounds_the_sharp_edge() {
        let sharp = simulate_scan(
            &plate(),
            &SimulateOptions {
                density: 0.8,
                smooth: 0.0,
                noise: 0.0,
                dropout: 0,
                ..SimulateOptions::default()
            },
        );
        let smoothed = simulate_scan(
            &plate(),
            &SimulateOptions {
                density: 0.8,
                smooth: 1.6,
                noise: 0.0,
                dropout: 0,
                ..SimulateOptions::default()
            },
        );
        let worst_dihedral = |mesh: &TriangleMesh| -> f64 {
            let mut edge_normal: std::collections::HashMap<(u32, u32), Vector3> =
                std::collections::HashMap::new();
            let mut worst = 0.0f64;
            for (face, &[a, b, c]) in mesh.triangles().iter().enumerate() {
                let Some(normal) = mesh.face_normal(face) else {
                    continue;
                };
                for (from, to) in [(a, b), (b, c), (c, a)] {
                    let key = (from.min(to), from.max(to));
                    match edge_normal.get(&key) {
                        Some(other) => {
                            let dihedral = normal.dot(*other).clamp(-1.0, 1.0).acos().to_degrees();
                            worst = worst.max(dihedral);
                        }
                        None => {
                            edge_normal.insert(key, normal);
                        }
                    }
                }
            }
            worst
        };
        let before = worst_dihedral(&sharp.mesh);
        let after = worst_dihedral(&smoothed.mesh);
        assert!(
            before > 85.0,
            "the ideal plate has right angles: {before:.1}"
        );
        assert!(
            after < 55.0,
            "smoothing must round the crease: {after:.1} degrees"
        );
    }

    #[test]
    fn noise_magnitude_matches_sigma() {
        let quiet = simulate_scan(
            &plate(),
            &SimulateOptions {
                density: 1.0,
                smooth: 0.0,
                noise: 0.0,
                dropout: 0,
                ..SimulateOptions::default()
            },
        );
        let noisy = simulate_scan(
            &plate(),
            &SimulateOptions {
                density: 1.0,
                smooth: 0.0,
                noise: 0.05,
                dropout: 0,
                ..SimulateOptions::default()
            },
        );
        // Same refinement, same vertex order: displacement per vertex
        // is the noise alone.
        let mut sum_squared = 0.0;
        let count = quiet
            .mesh
            .positions()
            .len()
            .min(noisy.mesh.positions().len());
        for index in 0..count {
            let d = noisy.mesh.positions()[index] - quiet.mesh.positions()[index];
            sum_squared += d.dot(d);
        }
        let rms = (sum_squared / count as f64).sqrt();
        assert!(
            (0.03..0.07).contains(&rms),
            "rms {rms:.4} against sigma 0.05"
        );
    }

    #[test]
    fn dropout_punches_holes() {
        let options = SimulateOptions {
            density: 1.0,
            smooth: 0.0,
            noise: 0.0,
            dropout: 3,
            dropout_size: 8.0,
            ..SimulateOptions::default()
        };
        let full = simulate_scan(
            &plate(),
            &SimulateOptions {
                dropout: 0,
                ..options
            },
        );
        let holed = simulate_scan(&plate(), &options);
        assert!(holed.mesh.triangles().len() < full.mesh.triangles().len());
        assert!(holed.mesh.surface_area() < full.mesh.surface_area());
    }

    #[test]
    fn the_same_seed_reproduces_the_same_scan() {
        let options = SimulateOptions {
            density: 0.9,
            smooth: 0.5,
            noise: 0.04,
            dropout: 2,
            dropout_size: 5.0,
            seed: 42,
        };
        let first = simulate_scan(&plate(), &options);
        let second = simulate_scan(&plate(), &options);
        assert_eq!(first.mesh.triangles(), second.mesh.triangles());
        let identical = first
            .mesh
            .positions()
            .iter()
            .zip(second.mesh.positions())
            .all(|(a, b)| a.x == b.x && a.y == b.y && a.z == b.z);
        assert!(identical, "same seed, same scan, to the bit");
    }
}

//! Estimates a scan's noise floor from the mesh itself.
//!
//! Every adaptive decision downstream — discriminator window sizes,
//! working tolerance, thin-sheet guards — wants one number: how far a
//! measured point scatters about the true surface. The scan carries
//! that number in its flattest neighbourhoods: fit small local planes
//! everywhere, and on a flat patch the residual *is* the noise, while
//! curvature only ever adds residual. A low percentile over many
//! patches therefore reads the noise floor without knowing where the
//! flats are.
//!
//! Deterministic under a fixed seed, like every other sampler here.

use crate::mesh::TriangleMesh;
use artificer_geometry::{Point3, Vector3};

/// How many local patches to sample.
const PATCHES: usize = 400;
/// Faces gathered per patch by breadth-first adjacency growth.
const PATCH_FACES: usize = 40;
/// The percentile of patch residuals read as the noise floor.
const FLOOR_PERCENTILE: f64 = 0.25;

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// The estimated scan noise sigma (mm), from the residual floor of
/// small local plane fits.
pub fn estimate_noise(mesh: &TriangleMesh) -> f64 {
    let face_count = mesh.triangles().len();
    if face_count < 200 {
        return 0.0;
    }
    // Patches must be small against the mesh, or on a coarse part
    // every patch spans a crease and the percentile reads geometry
    // instead of noise.
    let patch_faces = PATCH_FACES.min(face_count / 50).max(12);
    let adjacency = mesh.face_adjacency();
    let mut rng = SplitMix64(0x5e5a_11ed);
    // Seeds land by AREA, not by face index: a finely-tessellated
    // fillet band holds most of a part's faces while its flats hold
    // most of its area, and face-uniform seeding then reads the whole
    // part as curved.
    let mut cumulative: Vec<f64> = Vec::with_capacity(face_count);
    let mut total = 0.0f64;
    for face in 0..face_count {
        total += mesh.face_area(face);
        cumulative.push(total);
    }
    if total <= 0.0 {
        return 0.0;
    }
    let mut residuals: Vec<f64> = Vec::with_capacity(PATCHES);
    for _ in 0..PATCHES {
        let target = (rng.next() >> 11) as f64 / (1u64 << 53) as f64 * total;
        let seed = cumulative
            .partition_point(|&sum| sum < target)
            .min(face_count - 1);
        // Breadth-first growth over adjacency: a compact patch that
        // cannot leak across gaps or to the far side of a thin wall.
        let mut member = vec![seed];
        let mut queue = std::collections::VecDeque::from([seed]);
        let mut seen = std::collections::HashSet::from([seed]);
        while member.len() < patch_faces {
            let Some(face) = queue.pop_front() else { break };
            for &neighbour in &adjacency[face] {
                if seen.insert(neighbour as usize) {
                    member.push(neighbour as usize);
                    queue.push_back(neighbour as usize);
                    if member.len() >= patch_faces {
                        break;
                    }
                }
            }
        }
        if member.len() < patch_faces / 2 {
            continue;
        }
        // Plane through the patch's vertices by PCA; the residual rms
        // about it is what this patch says the noise is.
        let mut vertices: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for &face in &member {
            for index in mesh.triangles()[face] {
                vertices.insert(index);
            }
        }
        let points: Vec<Point3> = vertices
            .iter()
            .map(|&index| mesh.positions()[index as usize])
            .collect();
        if points.len() < 12 {
            continue;
        }
        let count = points.len() as f64;
        let centroid = points.iter().fold(Vector3::new(0.0, 0.0, 0.0), |acc, p| {
            acc + Vector3::new(p.x, p.y, p.z)
        }) / count;
        let mut covariance = [[0.0f64; 3]; 3];
        for p in &points {
            let d = [p.x - centroid.x, p.y - centroid.y, p.z - centroid.z];
            for i in 0..3 {
                for j in 0..3 {
                    covariance[i][j] += d[i] * d[j];
                }
            }
        }
        let (values, _vectors) = crate::numeric::sym_eigen_3x3(covariance);
        // Smallest eigenvalue is the residual power about the PCA
        // plane; its rms is what this patch says the noise is. (An
        // adjacent-vertex difference statistic was tried and read
        // sliver-refined scans several times low; the plane residual
        // is stable and the tolerance law is calibrated against it.)
        let smallest = (0..3)
            .min_by(|&i, &j| values[i].total_cmp(&values[j]))
            .unwrap_or(0);
        residuals.push((values[smallest].max(0.0) / count).sqrt());
    }
    if residuals.is_empty() {
        return 0.0;
    }
    residuals.sort_by(f64::total_cmp);
    residuals[((residuals.len() as f64 * FLOOR_PERCENTILE) as usize).min(residuals.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulate::{SimulateOptions, simulate_scan};
    use crate::synth;

    fn noisy_plate(noise: f64) -> TriangleMesh {
        simulate_scan(
            &synth::plate_with_boss(),
            &SimulateOptions {
                density: 0.5,
                smooth: 0.0,
                noise,
                dropout: 0,
                ..SimulateOptions::default()
            },
        )
        .mesh
    }

    #[test]
    fn reads_the_injected_sigma_back() {
        for sigma in [0.02, 0.05, 0.15] {
            let estimate = estimate_noise(&noisy_plate(sigma));
            assert!(
                (0.6 * sigma..1.5 * sigma).contains(&estimate),
                "sigma {sigma} estimated {estimate:.4}"
            );
        }
    }

    #[test]
    fn a_quiet_scan_reads_near_zero() {
        let estimate = estimate_noise(&noisy_plate(0.0));
        assert!(estimate < 0.005, "quiet estimate {estimate:.5}");
    }
}

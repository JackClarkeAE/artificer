//! Schnabel-style multi-primitive RANSAC extraction ("detect and peel").
//!
//! Real scans round every physical edge, so dihedral region growing cannot
//! isolate analytic patches on noisy data. This stage works the way the
//! commercial reverse-engineering kernels do instead: propose primitive
//! candidates from minimal point+normal samples drawn from local
//! neighbourhoods, score them by inlier support (distance band plus normal
//! agreement), keep the best, extract its largest connected component of
//! supporting faces, refine with the least-squares fitters, subtract those
//! faces, and repeat until nothing meets the support threshold.
//!
//! Everything is deterministic: candidates come from a seeded SplitMix64
//! generator, so a given mesh and parameter set always yields the same
//! report.

use artificer_geometry::{Point3, Vector3};

use crate::fit::{fit_cone, fit_cylinder, fit_plane, fit_sphere};
use crate::mesh::TriangleMesh;
use crate::numeric::solve_linear;
use crate::segment::{FitInputs, SurfaceClass, fit_inputs};
use crate::transform::{normalize, orthonormal_basis};

#[derive(Clone, Copy, Debug)]
pub struct RansacParams {
    /// Inlier distance band (mm). `<= 0` derives it from the reverse
    /// tolerance at pipeline level.
    pub epsilon: f64,
    /// Maximum angle (degrees) between a face normal and the primitive
    /// surface normal for the face to count as support.
    pub normal_tolerance_deg: f64,
    /// Candidate primitives proposed per extraction round.
    pub candidates_per_round: usize,
    /// Minimum connected faces for an extracted primitive to be kept.
    pub min_support_faces: usize,
    /// Hard cap on extracted primitives.
    pub max_primitives: usize,
    pub seed: u64,
}

impl Default for RansacParams {
    fn default() -> Self {
        Self {
            epsilon: 0.0,
            normal_tolerance_deg: 25.0,
            candidates_per_round: 350,
            min_support_faces: 300,
            max_primitives: 150,
            seed: 0x5eed_cad5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExtractedPrimitive {
    pub surface: SurfaceClass,
    pub faces: Vec<u32>,
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

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// Candidate primitive before refinement: geometry only, no statistics.
#[derive(Clone, Copy, Debug)]
enum Primitive {
    Plane { origin: Point3, normal: Vector3 },
    Sphere { center: Point3, radius: f64 },
    Cylinder { point: Point3, axis: Vector3, radius: f64 },
    Cone { apex: Point3, axis: Vector3, half_angle: f64 },
}

impl Primitive {
    /// Signed distance and the unit surface normal direction at the
    /// closest point; `None` where the primitive normal is undefined.
    fn probe(&self, p: Point3) -> Option<(f64, Vector3)> {
        match self {
            Self::Plane { origin, normal } => Some(((p - *origin).dot(*normal), *normal)),
            Self::Sphere { center, radius } => {
                let radial = p - *center;
                let len = radial.length();
                (len > 1e-12).then(|| (len - radius, radial / len))
            }
            Self::Cylinder { point, axis, radius } => {
                let v = p - *point;
                let radial = v - *axis * v.dot(*axis);
                let len = radial.length();
                (len > 1e-12).then(|| (len - radius, radial / len))
            }
            Self::Cone { apex, axis, half_angle } => {
                let v = p - *apex;
                let h = v.dot(*axis);
                let radial = v - *axis * h;
                let len = radial.length();
                let (sin_a, cos_a) = half_angle.sin_cos();
                (len > 1e-12)
                    .then(|| (len * cos_a - h * sin_a, radial / len * cos_a - *axis * sin_a))
            }
        }
    }

    /// Simpler shapes win ties: a plane and a huge-radius cylinder explain
    /// the same support, and the plane is the better model.
    const fn parsimony(&self) -> f64 {
        match self {
            Self::Plane { .. } => 1.0,
            Self::Cylinder { .. } => 0.98,
            Self::Sphere { .. } => 0.97,
            Self::Cone { .. } => 0.95,
        }
    }

    fn from_surface(surface: &SurfaceClass) -> Option<Self> {
        Some(match surface {
            SurfaceClass::Plane(f) => Self::Plane {
                origin: f.origin,
                normal: f.normal,
            },
            SurfaceClass::Sphere(f) => Self::Sphere {
                center: f.center,
                radius: f.radius,
            },
            SurfaceClass::Cylinder(f) => Self::Cylinder {
                point: f.axis_point,
                axis: f.axis,
                radius: f.radius,
            },
            SurfaceClass::Cone(f) => Self::Cone {
                apex: f.apex,
                axis: f.axis,
                half_angle: f.half_angle,
            },
            SurfaceClass::Blend(_) | SurfaceClass::Freeform => return None,
        })
    }
}

/// Sphere through two oriented points: centres of both normal lines'
/// closest approach.
fn sphere_from_two(
    p1: Point3,
    n1: Vector3,
    p2: Point3,
    n2: Vector3,
    scale: f64,
) -> Option<Primitive> {
    let b = n1.dot(n2);
    let denom = 1.0 - b * b;
    if denom < 1e-6 {
        return None;
    }
    let w = p1 - p2;
    // Minimize |p1 + t1 n1 - (p2 + t2 n2)|.
    let t1 = (-n1.dot(w) + b * n2.dot(w)) / denom;
    let t2 = (n2.dot(w) - b * n1.dot(w)) / denom;
    let c1 = p1 + n1 * t1;
    let c2 = p2 + n2 * t2;
    if (c1 - c2).length() > scale * 0.05 {
        return None;
    }
    let center = Point3::new((c1.x + c2.x) / 2.0, (c1.y + c2.y) / 2.0, (c1.z + c2.z) / 2.0);
    let radius = ((p1 - center).length() + (p2 - center).length()) / 2.0;
    (radius.is_finite() && radius > 1e-6 && radius < scale * 10.0).then_some(Primitive::Sphere {
        center,
        radius,
    })
}

/// Cylinder through two oriented points: the axis is orthogonal to both
/// normals; the axis line is where the projected normal lines cross.
fn cylinder_from_two(
    p1: Point3,
    n1: Vector3,
    p2: Point3,
    n2: Vector3,
    scale: f64,
) -> Option<Primitive> {
    let axis = normalize(n1.cross(n2))?;
    let (e1, e2) = orthonormal_basis(axis);
    let flat = |p: Point3| {
        let v = p - Point3::default();
        (v.dot(e1), v.dot(e2))
    };
    let dir = |n: Vector3| (n.dot(e1), n.dot(e2));
    let (x1, y1) = flat(p1);
    let (x2, y2) = flat(p2);
    let (u1, v1) = dir(n1);
    let (u2, v2) = dir(n2);
    // Intersect (x1,y1)+t(u1,v1) with (x2,y2)+s(u2,v2).
    let det = u1 * (-v2) - (-u2) * v1;
    if det.abs() < 1e-9 {
        return None;
    }
    let t = ((x2 - x1) * (-v2) - (-u2) * (y2 - y1)) / det;
    let cx = x1 + t * u1;
    let cy = y1 + t * v1;
    let point = Point3::default() + e1 * cx + e2 * cy;
    let r1 = (p1 - point).cross(axis).length();
    let r2 = (p2 - point).cross(axis).length();
    let radius = (r1 + r2) / 2.0;
    ((r1 - r2).abs() < scale * 0.05 && radius > 1e-6 && radius < scale * 10.0).then_some(
        Primitive::Cylinder {
            point,
            axis,
            radius,
        },
    )
}

/// Cone through three oriented points: apex from the tangent planes, axis
/// normal to the circle the apex-to-point directions trace on the sphere.
fn cone_from_three(samples: [(Point3, Vector3); 3], scale: f64) -> Option<Primitive> {
    let a: Vec<Vec<f64>> = samples
        .iter()
        .map(|(_, n)| vec![n.x, n.y, n.z])
        .collect();
    let b: Vec<f64> = samples
        .iter()
        .map(|(p, n)| n.dot(*p - Point3::default()))
        .collect();
    let apex = solve_linear(a, b)?;
    let apex = Point3::new(apex[0], apex[1], apex[2]);
    // An apex far outside the part is the cylinder-degenerate case; let the
    // cylinder candidate type claim that support instead.
    if (apex - samples[0].0).length() > scale * 3.0 {
        return None;
    }
    let d: Vec<Vector3> = samples
        .iter()
        .map(|(p, _)| normalize(*p - apex))
        .collect::<Option<_>>()?;
    let mut axis = normalize((d[1] - d[0]).cross(d[2] - d[0]))?;
    let mut cos_a = (d[0].dot(axis) + d[1].dot(axis) + d[2].dot(axis)) / 3.0;
    if cos_a < 0.0 {
        axis = axis * -1.0;
        cos_a = -cos_a;
    }
    let half_angle = cos_a.clamp(-1.0, 1.0).acos();
    (0.05..=1.45)
        .contains(&half_angle)
        .then_some(Primitive::Cone {
            apex,
            axis,
            half_angle,
        })
}

/// Per-face geometry cached once per extraction call.
struct FaceData {
    centroid: Vec<Point3>,
    normal: Vec<Option<Vector3>>,
    area: Vec<f64>,
}

impl FaceData {
    fn build(mesh: &TriangleMesh) -> Self {
        let count = mesh.triangles().len();
        Self {
            centroid: (0..count).map(|f| mesh.face_centroid(f)).collect(),
            normal: (0..count).map(|f| mesh.face_normal(f)).collect(),
            area: (0..count).map(|f| mesh.face_area(f)).collect(),
        }
    }
}

fn supports(
    primitive: &Primitive,
    centroid: Point3,
    face_normal: Vector3,
    epsilon: f64,
    min_alignment: f64,
) -> bool {
    primitive.probe(centroid).is_some_and(|(distance, surface_normal)| {
        distance.abs() <= epsilon && face_normal.dot(surface_normal).abs() >= min_alignment
    })
}

/// Breadth-first neighbourhood of unassigned faces around a seed, for
/// localized candidate sampling.
fn neighbourhood(
    seed: u32,
    adjacency: &[Vec<u32>],
    assigned: &[bool],
    cap: usize,
) -> Vec<u32> {
    let mut out = vec![seed];
    let mut cursor = 0;
    while cursor < out.len() && out.len() < cap {
        let face = out[cursor];
        cursor += 1;
        for &next in &adjacency[face as usize] {
            if !assigned[next as usize] && !out.contains(&next) {
                out.push(next);
                if out.len() >= cap {
                    break;
                }
            }
        }
    }
    out
}

/// Largest connected component of `faces` under mesh adjacency.
fn largest_component(faces: &[u32], adjacency: &[Vec<u32>], face_count: usize) -> Vec<u32> {
    let mut member = vec![false; face_count];
    for &f in faces {
        member[f as usize] = true;
    }
    let mut visited = vec![false; face_count];
    let mut best: Vec<u32> = Vec::new();
    for &start in faces {
        if visited[start as usize] {
            continue;
        }
        visited[start as usize] = true;
        let mut component = vec![start];
        let mut cursor = 0;
        while cursor < component.len() {
            let face = component[cursor];
            cursor += 1;
            for &next in &adjacency[face as usize] {
                if member[next as usize] && !visited[next as usize] {
                    visited[next as usize] = true;
                    component.push(next);
                }
            }
        }
        if component.len() > best.len() {
            best = component;
        }
    }
    best
}

/// Refits the winning candidate's component with the exact least-squares
/// fitters. The candidate's own kind is tried first; if its refined RMS
/// misses `epsilon`, the other kinds compete for the same component in
/// parsimony order — a near-degenerate cone candidate over a cylindrical
/// patch re-selects as the cylinder instead of wasting the extraction.
fn refine(
    mesh: &TriangleMesh,
    faces: &[u32],
    primitive: &Primitive,
    epsilon: f64,
) -> Option<SurfaceClass> {
    let FitInputs {
        points,
        normals,
        cone_samples,
        mean_normal,
    } = fit_inputs(mesh, faces);
    let fit_kind = |kind: u8| -> Option<SurfaceClass> {
        match kind {
            0 => fit_plane(&points, Some(mean_normal)).map(SurfaceClass::Plane),
            1 => fit_cylinder(&points, &normals).map(SurfaceClass::Cylinder),
            2 => fit_sphere(&points).map(SurfaceClass::Sphere),
            _ => fit_cone(&cone_samples).map(SurfaceClass::Cone),
        }
    };
    let first = match primitive {
        Primitive::Plane { .. } => 0,
        Primitive::Cylinder { .. } => 1,
        Primitive::Sphere { .. } => 2,
        Primitive::Cone { .. } => 3,
    };
    let mut order = vec![first];
    order.extend([0u8, 1, 2, 3].into_iter().filter(|k| *k != first));
    for kind in order {
        if let Some(surface) = fit_kind(kind)
            && surface.rms().is_some_and(|rms| rms <= epsilon)
        {
            return Some(surface);
        }
    }
    None
}

/// Detects and peels analytic primitives from `candidate_faces`.
pub fn extract_primitives(
    mesh: &TriangleMesh,
    candidate_faces: &[u32],
    adjacency: &[Vec<u32>],
    params: &RansacParams,
) -> Vec<ExtractedPrimitive> {
    let face_count = mesh.triangles().len();
    let data = FaceData::build(mesh);
    let scale = mesh.bounds_diagonal().max(1.0);
    let epsilon = if params.epsilon > 0.0 { params.epsilon } else { 0.05 };
    let min_alignment = params.normal_tolerance_deg.to_radians().cos();
    let mut rng = SplitMix64(params.seed);
    let mut assigned = vec![true; face_count];
    let mut unassigned: Vec<u32> = candidate_faces
        .iter()
        .copied()
        .filter(|&f| data.normal[f as usize].is_some())
        .collect();
    for &f in &unassigned {
        assigned[f as usize] = false;
    }
    let mut results = Vec::new();
    let mut dry_rounds = 0;
    while results.len() < params.max_primitives
        && dry_rounds < 4
        && unassigned.len() >= params.min_support_faces
    {
        // Score candidates against a subsample; full support only for the winner.
        let stride = unassigned.len().div_ceil(6000).max(1);
        let subset: Vec<u32> = unassigned.iter().copied().step_by(stride).collect();
        let mut best: Option<(f64, Primitive)> = None;
        for round in 0..params.candidates_per_round {
            let seed = unassigned[rng.below(unassigned.len())];
            let hood = neighbourhood(seed, adjacency, &assigned, 48);
            let draw = |rng: &mut SplitMix64| {
                let face = hood[rng.below(hood.len())];
                let normal = data.normal[face as usize].unwrap_or(Vector3::new(0.0, 0.0, 1.0));
                (data.centroid[face as usize], normal)
            };
            let candidate = match round % 5 {
                // Planes are cheap and common: propose them most often.
                0 | 1 => {
                    let (origin, normal) = draw(&mut rng);
                    Some(Primitive::Plane { origin, normal })
                }
                2 | 3 => {
                    let (p1, n1) = draw(&mut rng);
                    let (p2, n2) = draw(&mut rng);
                    cylinder_from_two(p1, n1, p2, n2, scale)
                        .or_else(|| sphere_from_two(p1, n1, p2, n2, scale))
                }
                _ => {
                    let s = [draw(&mut rng), draw(&mut rng), draw(&mut rng)];
                    cone_from_three(s, scale)
                }
            };
            let Some(candidate) = candidate else { continue };
            let mut score = 0.0;
            for &face in &subset {
                let Some(normal) = data.normal[face as usize] else {
                    continue;
                };
                if supports(&candidate, data.centroid[face as usize], normal, epsilon, min_alignment)
                {
                    score += data.area[face as usize];
                }
            }
            score *= candidate.parsimony();
            if best.is_none_or(|(best_score, _)| score > best_score) {
                best = Some((score, candidate));
            }
        }
        let Some((_, winner)) = best else {
            dry_rounds += 1;
            continue;
        };
        // Full support, connectivity, refinement, then support again with
        // the refined shape so the boundary is exact.
        let accept = |primitive: &Primitive, unassigned: &[u32]| -> Vec<u32> {
            let inliers: Vec<u32> = unassigned
                .iter()
                .copied()
                .filter(|&f| {
                    data.normal[f as usize].is_some_and(|n| {
                        supports(primitive, data.centroid[f as usize], n, epsilon, min_alignment)
                    })
                })
                .collect();
            largest_component(&inliers, adjacency, face_count)
        };
        let component = accept(&winner, &unassigned);
        if component.len() < params.min_support_faces {
            dry_rounds += 1;
            continue;
        }
        let Some(refined) = refine(mesh, &component, &winner, epsilon) else {
            dry_rounds += 1;
            continue;
        };
        let refined_primitive = Primitive::from_surface(&refined).unwrap_or(winner);
        let final_component = accept(&refined_primitive, &unassigned);
        let faces = if final_component.len() >= params.min_support_faces {
            final_component
        } else {
            component
        };
        for &f in &faces {
            assigned[f as usize] = true;
        }
        unassigned.retain(|&f| !assigned[f as usize]);
        results.push(ExtractedPrimitive {
            surface: refined,
            faces,
        });
        dry_rounds = 0;
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    #[test]
    fn peels_the_plate_and_boss_without_pre_segmentation() {
        let mesh = synth::plate_with_boss();
        let adjacency = mesh.face_adjacency();
        let all_faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
        let params = RansacParams {
            epsilon: 0.05,
            min_support_faces: 60,
            ..RansacParams::default()
        };
        let extracted = extract_primitives(&mesh, &all_faces, &adjacency, &params);
        let planes = extracted
            .iter()
            .filter(|e| matches!(e.surface, SurfaceClass::Plane(_)))
            .count();
        let cylinders: Vec<_> = extracted
            .iter()
            .filter_map(|e| match &e.surface {
                SurfaceClass::Cylinder(fit) => Some(fit),
                _ => None,
            })
            .collect();
        assert!(planes >= 7, "found {planes} planes");
        assert_eq!(cylinders.len(), 1, "found {} cylinders", cylinders.len());
        assert!((cylinders[0].radius - 12.0).abs() < 0.05);
        let covered: usize = extracted.iter().map(|e| e.faces.len()).sum();
        assert!(
            covered as f64 > mesh.triangles().len() as f64 * 0.95,
            "covered {covered} of {}",
            mesh.triangles().len()
        );
    }
}

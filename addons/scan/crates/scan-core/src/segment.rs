//! Sharp-edge segmentation and per-region surface classification.
//!
//! Mechanical parts scan into smooth patches separated by sharp dihedral
//! edges. Region growing across smooth edges recovers those patches, and
//! each patch is then classified by competitive primitive fitting: fit
//! plane, cylinder, sphere, and cone, and keep the simplest model whose
//! deviation meets tolerance.

use std::collections::HashSet;

use artificer_geometry::{Point3, Vector3};

use crate::fit::{
    ConeFit, CylinderFit, PlaneFit, RevolvedBlendFit, SphereFit, fit_cone, fit_cylinder,
    fit_plane, fit_sphere,
};
use crate::mesh::TriangleMesh;

#[derive(Clone, Copy, Debug)]
pub struct SegmentationParams {
    /// Edges with a larger dihedral angle (degrees) become region borders.
    pub max_dihedral_deg: f64,
    /// Regions with fewer faces are reported but never classified.
    pub min_region_faces: usize,
}

impl Default for SegmentationParams {
    fn default() -> Self {
        Self {
            max_dihedral_deg: 30.0,
            min_region_faces: 8,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Region {
    pub faces: Vec<u32>,
    pub area: f64,
}

impl Region {
    pub fn vertex_points(&self, mesh: &TriangleMesh) -> Vec<Point3> {
        let mut seen = HashSet::new();
        let mut points = Vec::new();
        for &face in &self.faces {
            for index in mesh.triangles()[face as usize] {
                if seen.insert(index) {
                    points.push(mesh.positions()[index as usize]);
                }
            }
        }
        points
    }

    pub fn face_normals(&self, mesh: &TriangleMesh) -> Vec<(Vector3, f64)> {
        self.faces
            .iter()
            .filter_map(|&face| {
                let normal = mesh.face_normal(face as usize)?;
                Some((normal, mesh.face_area(face as usize)))
            })
            .collect()
    }

    pub fn mean_normal(&self, mesh: &TriangleMesh) -> Vector3 {
        let mut sum = Vector3::default();
        for &face in &self.faces {
            sum = sum + mesh.face_area_vector(face as usize);
        }
        sum
    }
}

/// Grows regions across edges whose dihedral angle stays under the
/// threshold. Returns regions sorted by area, largest first.
pub fn segment(mesh: &TriangleMesh, params: &SegmentationParams) -> Vec<Region> {
    let face_count = mesh.triangles().len();
    let adjacency = mesh.face_adjacency();
    let normals: Vec<Option<Vector3>> = (0..face_count).map(|f| mesh.face_normal(f)).collect();
    let cosine_threshold = params.max_dihedral_deg.to_radians().cos();
    let mut visited = vec![false; face_count];
    let mut regions = Vec::new();
    for seed in 0..face_count {
        if visited[seed] || normals[seed].is_none() {
            continue;
        }
        visited[seed] = true;
        let mut faces = vec![seed as u32];
        let mut queue = vec![seed as u32];
        while let Some(face) = queue.pop() {
            let Some(face_normal) = normals[face as usize] else {
                continue;
            };
            for &neighbor in &adjacency[face as usize] {
                if visited[neighbor as usize] {
                    continue;
                }
                let Some(neighbor_normal) = normals[neighbor as usize] else {
                    continue;
                };
                if face_normal.dot(neighbor_normal) >= cosine_threshold {
                    visited[neighbor as usize] = true;
                    faces.push(neighbor);
                    queue.push(neighbor);
                }
            }
        }
        let area = faces.iter().map(|&f| mesh.face_area(f as usize)).sum();
        regions.push(Region { faces, area });
    }
    regions.sort_by(|a, b| b.area.total_cmp(&a.area));
    regions
}

#[derive(Clone, Debug)]
pub enum SurfaceClass {
    Plane(PlaneFit),
    Cylinder(CylinderFit),
    Sphere(SphereFit),
    Cone(ConeFit),
    /// A fillet ring: torus patch revolved about a datum axis. Produced by
    /// blend recognition after datum alignment, never by region fitting.
    Blend(RevolvedBlendFit),
    Freeform,
}

impl SurfaceClass {
    /// The same surface expressed in another frame. Deviation statistics
    /// are invariant under rigid motion and carry over unchanged.
    pub fn transformed(&self, transform: &crate::transform::RigidTransform) -> Self {
        match self {
            Self::Plane(fit) => Self::Plane(PlaneFit {
                origin: transform.apply_point(fit.origin),
                normal: transform.apply_vector(fit.normal),
                deviation: fit.deviation,
            }),
            Self::Cylinder(fit) => Self::Cylinder(CylinderFit {
                axis_point: transform.apply_point(fit.axis_point),
                axis: transform.apply_vector(fit.axis),
                radius: fit.radius,
                deviation: fit.deviation,
            }),
            Self::Sphere(fit) => Self::Sphere(SphereFit {
                center: transform.apply_point(fit.center),
                radius: fit.radius,
                deviation: fit.deviation,
            }),
            Self::Cone(fit) => Self::Cone(ConeFit {
                apex: transform.apply_point(fit.apex),
                axis: transform.apply_vector(fit.axis),
                half_angle: fit.half_angle,
                deviation: fit.deviation,
            }),
            Self::Blend(fit) => Self::Blend(RevolvedBlendFit {
                axis_point: transform.apply_point(fit.axis_point),
                axis: transform.apply_vector(fit.axis),
                major_radius: fit.major_radius,
                minor_radius: fit.minor_radius,
                deviation: fit.deviation,
            }),
            Self::Freeform => Self::Freeform,
        }
    }

    /// Signed distance to the surface and the unit surface normal at the
    /// closest point; `None` for freeform or degenerate probe locations.
    pub fn probe(&self, point: Point3) -> Option<(f64, Vector3)> {
        match self {
            Self::Plane(fit) => Some((fit.signed_distance(point), fit.normal)),
            Self::Sphere(fit) => {
                let radial = point - fit.center;
                let length = radial.length();
                (length > 1e-12).then(|| (length - fit.radius, radial / length))
            }
            Self::Cylinder(fit) => {
                let v = point - fit.axis_point;
                let radial = v - fit.axis * v.dot(fit.axis);
                let length = radial.length();
                (length > 1e-12).then(|| (length - fit.radius, radial / length))
            }
            Self::Cone(fit) => {
                let v = point - fit.apex;
                let h = v.dot(fit.axis);
                let radial = v - fit.axis * h;
                let length = radial.length();
                let (sin_a, cos_a) = fit.half_angle.sin_cos();
                (length > 1e-12).then(|| {
                    (
                        length * cos_a - h * sin_a,
                        radial / length * cos_a - fit.axis * sin_a,
                    )
                })
            }
            Self::Blend(fit) => {
                let v = point - fit.axis_point;
                let h = v.dot(fit.axis);
                let radial = v - fit.axis * h;
                let length = radial.length();
                if length < 1e-12 {
                    return None;
                }
                let profile = (length - fit.major_radius, h);
                let profile_length = profile.0.hypot(profile.1);
                (profile_length > 1e-12).then(|| {
                    (
                        profile_length - fit.minor_radius,
                        radial / length * (profile.0 / profile_length)
                            + fit.axis * (profile.1 / profile_length),
                    )
                })
            }
            Self::Freeform => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Plane(_) => "plane",
            Self::Cylinder(_) => "cylinder",
            Self::Sphere(_) => "sphere",
            Self::Cone(_) => "cone",
            Self::Blend(_) => "blend",
            Self::Freeform => "freeform",
        }
    }

    pub fn rms(&self) -> Option<f64> {
        match self {
            Self::Plane(f) => Some(f.deviation.rms),
            Self::Cylinder(f) => Some(f.deviation.rms),
            Self::Sphere(f) => Some(f.deviation.rms),
            Self::Cone(f) => Some(f.deviation.rms),
            Self::Blend(f) => Some(f.deviation.rms),
            Self::Freeform => None,
        }
    }
}

/// Point, normal, and paired-sample inputs for the primitive fitters,
/// assembled from a face set and capped at a fixed sample budget: fitting
/// cost is linear in samples and the refinement Jacobian is evaluated
/// repeatedly, so beyond the budget extra samples no longer move the fit.
pub(crate) struct FitInputs {
    pub points: Vec<Point3>,
    pub normals: Vec<(Vector3, f64)>,
    /// Paired `(centroid, normal, area)` per face, for the cone fit.
    pub cone_samples: Vec<(Point3, Vector3, f64)>,
    pub mean_normal: Vector3,
}

pub(crate) fn fit_inputs(mesh: &TriangleMesh, faces: &[u32]) -> FitInputs {
    const FIT_SAMPLE_BUDGET: usize = 20_000;
    let mut seen = HashSet::new();
    let mut points = Vec::new();
    for &face in faces {
        for index in mesh.triangles()[face as usize] {
            if seen.insert(index) {
                points.push(mesh.positions()[index as usize]);
            }
        }
    }
    let mut normals = Vec::new();
    let mut cone_samples = Vec::new();
    let mut mean_normal = Vector3::default();
    for &face in faces {
        mean_normal = mean_normal + mesh.face_area_vector(face as usize);
        if let Some(normal) = mesh.face_normal(face as usize) {
            let area = mesh.face_area(face as usize);
            normals.push((normal, area));
            cone_samples.push((mesh.face_centroid(face as usize), normal, area));
        }
    }
    if points.len() > FIT_SAMPLE_BUDGET {
        let stride = points.len().div_ceil(FIT_SAMPLE_BUDGET);
        points = points.into_iter().step_by(stride).collect();
    }
    if normals.len() > FIT_SAMPLE_BUDGET {
        let stride = normals.len().div_ceil(FIT_SAMPLE_BUDGET);
        normals = normals.into_iter().step_by(stride).collect();
    }
    if cone_samples.len() > FIT_SAMPLE_BUDGET {
        let stride = cone_samples.len().div_ceil(FIT_SAMPLE_BUDGET);
        cone_samples = cone_samples.into_iter().step_by(stride).collect();
    }
    FitInputs {
        points,
        normals,
        cone_samples,
        mean_normal,
    }
}

/// Fits every primitive to the region and keeps the simplest model whose
/// RMS deviation meets `tolerance`. A more complex model only displaces a
/// simpler passing one when it fits at least 30 percent better.
pub fn classify_region(
    mesh: &TriangleMesh,
    region: &Region,
    tolerance: f64,
    params: &SegmentationParams,
) -> SurfaceClass {
    if region.faces.len() < params.min_region_faces {
        return SurfaceClass::Freeform;
    }
    let inputs = fit_inputs(mesh, &region.faces);
    let FitInputs {
        points,
        normals,
        cone_samples,
        mean_normal,
    } = inputs;
    let plane = fit_plane(&points, Some(mean_normal));
    let cylinder = fit_cylinder(&points, &normals);
    let sphere = fit_sphere(&points);
    let cone = fit_cone(&cone_samples);
    let best_curved_rms = [
        cylinder.as_ref().map(|f| f.deviation.rms),
        sphere.as_ref().map(|f| f.deviation.rms),
        cone.as_ref().map(|f| f.deviation.rms),
    ]
    .into_iter()
    .flatten()
    .fold(f64::INFINITY, f64::min);
    if let Some(plane) = plane
        && plane.deviation.rms <= tolerance
        && plane.deviation.rms <= 2.0 * best_curved_rms.max(tolerance * 0.05)
    {
        return SurfaceClass::Plane(plane);
    }
    let mut chosen: Option<SurfaceClass> = None;
    let mut chosen_rms = f64::INFINITY;
    let candidates: [Option<SurfaceClass>; 3] = [
        cylinder.map(SurfaceClass::Cylinder),
        sphere.map(SurfaceClass::Sphere),
        cone.map(SurfaceClass::Cone),
    ];
    for candidate in candidates.into_iter().flatten() {
        let Some(rms) = candidate.rms() else { continue };
        if rms > tolerance {
            continue;
        }
        // First passing model wins; later (more complex or equal) models
        // must be decisively better to displace it.
        if chosen.is_none() || rms < 0.7 * chosen_rms {
            chosen_rms = rms;
            chosen = Some(candidate);
        }
    }
    chosen.unwrap_or(SurfaceClass::Freeform)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    #[test]
    fn plate_with_boss_segments_into_expected_patches() {
        let mesh = synth::plate_with_boss();
        let params = SegmentationParams::default();
        let regions = segment(&mesh, &params);
        // 6 box faces, boss shell, boss cap; welding of coincident rim
        // vertices may create tiny extra slivers but the big eight dominate.
        assert!(regions.len() >= 8, "found {} regions", regions.len());
        let mut planes = 0;
        let mut cylinders = 0;
        for region in regions.iter().take(8) {
            match classify_region(&mesh, region, 0.05, &params) {
                SurfaceClass::Plane(_) => planes += 1,
                SurfaceClass::Cylinder(fit) => {
                    cylinders += 1;
                    assert!((fit.radius - 12.0).abs() < 0.01, "radius {}", fit.radius);
                    assert!(fit.axis.z.abs() > 1.0 - 1e-6);
                }
                other => panic!("unexpected class {:?}", other.kind()),
            }
        }
        assert_eq!(planes, 7);
        assert_eq!(cylinders, 1);
    }
}

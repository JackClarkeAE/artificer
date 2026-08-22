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
    fit_torus,
    ConeFit, CylinderFit, EdgeRoundFit, PatternFit, PlaneFit, RevolvedBlendFit, SphereFit,
    fit_cone, fit_cylinder, fit_plane, fit_sphere,
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
    /// A doubly-curved patch: a torus found by region fitting, on its own
    /// axis rather than the datum's. This is the moulded case — a cast
    /// panel crowned unequally along its two principal directions, which
    /// a sphere can only approximate. It shares [`RevolvedBlendFit`] with
    /// `Blend` because the geometry is identical, but it is deliberately
    /// a separate class: a `Blend`'s axis is the part's, and belongs in
    /// the datum vote, while this one's axis is wherever the moulding
    /// happens to curve and must stay out of it.
    Torus(RevolvedBlendFit),
    /// An n-fold circular pattern (a gear's toothing): one master surface
    /// repeated about the datum axis. Produced by pattern recognition.
    Pattern(PatternFit),
    /// The round/chamfer band along the shared edge of two features.
    EdgeRound(EdgeRoundFit),
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
            Self::Torus(fit) => Self::Torus(RevolvedBlendFit {
                axis_point: transform.apply_point(fit.axis_point),
                axis: transform.apply_vector(fit.axis),
                major_radius: fit.major_radius,
                minor_radius: fit.minor_radius,
                deviation: fit.deviation,
            }),
            Self::Pattern(fit) => Self::Pattern(PatternFit {
                axis_point: transform.apply_point(fit.axis_point),
                axis: transform.apply_vector(fit.axis),
                ..*fit
            }),
            Self::EdgeRound(fit) => Self::EdgeRound(*fit),
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
            Self::Blend(fit) | Self::Torus(fit) => {
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
            Self::Pattern(_) | Self::EdgeRound(_) | Self::Freeform => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Plane(_) => "plane",
            Self::Cylinder(_) => "cylinder",
            Self::Sphere(_) => "sphere",
            Self::Cone(_) => "cone",
            Self::Blend(_) => "blend",
            Self::Torus(_) => "torus",
            Self::Pattern(_) => "pattern",
            Self::EdgeRound(_) => "edge_round",
            Self::Freeform => "freeform",
        }
    }

    /// Re-measures the surface against `points` after its parameters have
    /// been changed.
    ///
    /// Snapping and harmonizing move a fitted surface off the points it was
    /// fitted to. The stored [`DeviationStats`] then describe geometry that
    /// is no longer there, and every tolerance decision downstream reads
    /// them — so a surface that was moved has to be re-measured, not just
    /// re-parameterized.
    ///
    /// The variants absent below (`Pattern`, `EdgeRound`, `Freeform`) carry
    /// no closed-form signed distance and are never mutated by snapping, so
    /// their statistics stay valid.
    pub(crate) fn recompute_deviation(&mut self, points: &[Point3]) {
        if points.is_empty() {
            return;
        }
        match self {
            Self::Plane(f) => {
                f.deviation = crate::fit::stats(points.iter().map(|p| f.signed_distance(*p)));
            }
            Self::Cylinder(f) => {
                f.deviation = crate::fit::stats(points.iter().map(|p| f.signed_distance(*p)));
            }
            Self::Sphere(f) => {
                f.deviation = crate::fit::stats(points.iter().map(|p| f.signed_distance(*p)));
            }
            Self::Cone(f) => {
                f.deviation = crate::fit::stats(points.iter().map(|p| f.signed_distance(*p)));
            }
            Self::Blend(f) => {
                f.deviation = crate::fit::stats(points.iter().map(|p| f.signed_distance(*p)));
            }
            // A fitted torus is measured again against whatever it ended
            // up owning, and asked again whether those points still
            // evidence it. They often will not: the patch it was born on
            // curved, and the merged feature it grew into may be mostly
            // flat. A `Blend` is exempt — a fillet's standing comes from
            // blend recognition about the datum axis, not from bowing
            // measurably across its own width.
            Self::Torus(f) => {
                f.deviation = crate::fit::stats(points.iter().map(|p| f.signed_distance(*p)));
                if !crate::fit::torus_is_evidenced(f, points) {
                    *self = Self::Freeform;
                }
            }
            Self::Pattern(_) | Self::EdgeRound(_) | Self::Freeform => {}
        }
    }

    pub fn rms(&self) -> Option<f64> {
        match self {
            Self::Plane(f) => Some(f.deviation.rms),
            Self::Cylinder(f) => Some(f.deviation.rms),
            Self::Sphere(f) => Some(f.deviation.rms),
            Self::Cone(f) => Some(f.deviation.rms),
            Self::Blend(f) | Self::Torus(f) => Some(f.deviation.rms),
            Self::Pattern(f) => Some(f.deviation.rms),
            Self::EdgeRound(f) => Some(f.deviation.rms),
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
    let torus = fit_torus(&points);
    // Deliberately not part of `best_curved_rms` below. That gate lets a
    // decisively better curve unseat a passing plane, and a torus can
    // always find some enormous-radius fit that beats a plane by a hair
    // on noise alone — which would turn flat faces into mouldings across
    // every prismatic part in the corpus. A patch only reaches the torus
    // once it has already failed to be flat.
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
    // Torus last: it is the most expensive description here, so the
    // ordering below makes it earn its place by a clear margin over
    // whatever simpler surface already passed.
    let candidates: [Option<SurfaceClass>; 4] = [
        cylinder.map(SurfaceClass::Cylinder),
        sphere.map(SurfaceClass::Sphere),
        cone.map(SurfaceClass::Cone),
        torus.map(SurfaceClass::Torus),
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

#[cfg(test)]
mod moulded_exploration {
    use super::*;
    use crate::mesh::TriangleMesh;
    use artificer_geometry::Point3;

    /// A square panel lying on a sphere of `radius`, with gaussian noise
    /// along the normal — a moulded panel that is flat to the eye and
    /// crowned to a gauge.
    fn crowned_panel(radius: f64, half: f64, steps: usize, sigma: f64) -> TriangleMesh {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut noise = || {
            // SplitMix64, then a crude normal from two uniforms.
            let mut next = || {
                state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
            };
            let (u, v) = (next().max(1e-12), next());
            sigma * (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
        };
        let at = |x: f64, y: f64, lift: f64| {
            let z = (radius * radius - x * x - y * y).max(0.0).sqrt() - radius;
            Point3::new(x, y, z + lift)
        };
        let mut soup = Vec::new();
        let step = 2.0 * half / steps as f64;
        for i in 0..steps {
            for j in 0..steps {
                let (x0, y0) = (-half + i as f64 * step, -half + j as f64 * step);
                let (x1, y1) = (x0 + step, y0 + step);
                let a = at(x0, y0, noise());
                let b = at(x1, y0, noise());
                let c = at(x1, y1, noise());
                let d = at(x0, y1, noise());
                soup.push([a, b, c]);
                soup.push([a, c, d]);
            }
        }
        TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("panel")
    }

    /// A panel crowned differently along x and y — the real moulded
    /// case, which no sphere can fit. `rx` and `ry` are the principal
    /// radii; equal values give back a sphere.
    fn elliptic_panel(rx: f64, ry: f64, half: f64, steps: usize, sigma: f64) -> TriangleMesh {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut noise = || {
            let mut next = || {
                state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
            };
            let (u, v) = (next().max(1e-12), next());
            sigma * (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
        };
        let at = |x: f64, y: f64, lift: f64| {
            // Paraboloid, the leading term of any smooth crown.
            let z = -(x * x) / (2.0 * rx) - (y * y) / (2.0 * ry);
            Point3::new(x, y, z + lift)
        };
        let mut soup = Vec::new();
        let step = 2.0 * half / steps as f64;
        for i in 0..steps {
            for j in 0..steps {
                let (x0, y0) = (-half + i as f64 * step, -half + j as f64 * step);
                let (x1, y1) = (x0 + step, y0 + step);
                let a = at(x0, y0, noise());
                let b = at(x1, y0, noise());
                let c = at(x1, y1, noise());
                let d = at(x0, y1, noise());
                soup.push([a, b, c]);
                soup.push([a, c, d]);
            }
        }
        TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("panel")
    }

    #[test]
    fn what_happens_to_a_doubly_curved_crown() {
        let params = SegmentationParams::default();
        let half = 50.0;
        // Same mean curvature, increasingly unequal principal radii.
        for (rx, ry) in [
            (2000.0f64, 2000.0f64),
            (2000.0, 3000.0),
            (2000.0, 6000.0),
            (2000.0, 20000.0),
            (2000.0, 1.0e9),
        ] {
            let mesh = elliptic_panel(rx, ry, half, 60, 0.03);
            let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
            let area = faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum::<f64>();
            let region = Region { faces, area };
            let class = classify_region(&mesh, &region, 0.18, &params);
            let (name, fitted, rms) = match &class {
                SurfaceClass::Plane(f) => ("plane", 0.0, f.deviation.rms),
                SurfaceClass::Sphere(f) => ("sphere", f.radius, f.deviation.rms),
                SurfaceClass::Cylinder(f) => ("cylinder", f.radius, f.deviation.rms),
                SurfaceClass::Cone(f) => ("cone", 0.0, f.deviation.rms),
                SurfaceClass::Torus(f) => ("torus", f.minor_radius, f.deviation.rms),
                _ => ("FREEFORM", 0.0, f64::NAN),
            };
            println!(
                "rx={rx:>7.0} ry={ry:>10.0} -> {name:<9} (R={fitted:>7.0}, rms={rms:.4})"
            );
            // Nothing here is allowed to fall out of the vocabulary: a
            // moulded crown must land on *some* primitive within
            // tolerance, or the analytic-exact kernel cannot carry
            // cast parts at all.
            assert_ne!(name, "FREEFORM", "rx={rx} ry={ry} left the vocabulary");
            assert!(rms <= 0.18, "rx={rx} ry={ry} fitted at rms {rms:.4}");
            // Passing is not the same as describing, and this is where
            // that used to bite: an unequally crowned panel was fitted
            // by a sphere stretched over a shape it did not match, at
            // three times the measurement noise, while still reporting
            // itself inside tolerance. A torus carries two independent
            // principal radii, so the crown is now described rather
            // than absorbed, and the residual falls back to what the
            // scanner actually contributed.
            if (rx - ry).abs() > 0.4 * rx && ry < 1.0e4 {
                assert_eq!(name, "torus", "an unequal crown is a torus patch");
                // How close to the noise floor depends on how unequal
                // the crown is. At ry/rx = 1.5 the torus is exact and
                // the residual is purely the scanner's. At 3 it is
                // roughly 0.049 against a 0.030 floor — better than
                // twice as good as the sphere it replaced, but not
                // exact, because a torus's curvature varies across the
                // patch in its own way and a general crown's does not.
                // A torus is the best two-radius primitive available,
                // not a universal description of a moulded surface.
                assert!(
                    rms < 0.06,
                    "rx={rx} ry={ry} fitted at rms {rms:.4}: the crown is being absorbed \
                     as residual again rather than described"
                );
                assert!(
                    (fitted - rx.min(ry)).abs() < 0.15 * rx.min(ry),
                    "tube radius {fitted:.0} should be the sharper crown {:.0}",
                    rx.min(ry)
                );
            }
        }
    }

    #[test]
    fn where_does_a_moulded_crown_stop_reading_as_flat() {
        let params = SegmentationParams::default();
        let half = 50.0;
        for radius in [500.0f64, 2000.0, 8000.0, 40000.0, 200_000.0] {
            let sagitta = radius - (radius * radius - half * half).max(0.0).sqrt();
            let mesh = crowned_panel(radius, half, 60, 0.03);
            let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
            let area = faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum::<f64>();
            let region = Region { faces, area };
            let class = classify_region(&mesh, &region, 0.18, &params);
            let (name, fitted) = match &class {
                SurfaceClass::Plane(_) => ("plane", 0.0),
                SurfaceClass::Sphere(f) => ("sphere", f.radius),
                SurfaceClass::Cylinder(f) => ("cylinder", f.radius),
                SurfaceClass::Cone(_) => ("cone", 0.0),
                _ => ("freeform", 0.0),
            };
            println!(
                "R={radius:>8.0} crown={sagitta:>6.3}mm -> {name} (fitted R={fitted:.0})"
            );
            // The crossover is the noise floor, not a chosen constant: a
            // crown you can measure is kept exactly, one you cannot is
            // reported flat. This is the whole answer to how a moulded
            // panel survives an analytic-exact kernel — no spline is
            // needed, because a large-radius sphere is already exact.
            if sagitta > 0.1 {
                assert_eq!(name, "sphere", "a measurable {sagitta:.3}mm crown is real shape");
                assert!(
                    (fitted - radius).abs() < 0.1 * radius,
                    "fitted R={fitted:.0} should be near {radius:.0}"
                );
            } else if sagitta < 0.031 {
                assert_eq!(name, "plane", "a {sagitta:.3}mm crown is under the noise");
            }
        }
    }
}

//! Fragment merging: one physical surface, one feature.
//!
//! RANSAC peeling extracts connected components, so a hub interrupted by
//! keyways or a face crossed by slots comes back as several fragments of
//! the same analytic surface. This stage greedily accretes compatible
//! fragments — coaxial cylinders, coplanar planes, concentric spheres —
//! and accepts each merge only when a least-squares refit over the union
//! of faces still meets tolerance. The refit is the guard that keeps a
//! genuine 46.8 mm step from swallowing a neighbouring 47.3 mm relief:
//! merging those would push the union RMS past tolerance, so they stay
//! separate features.

use crate::fit::{fit_cylinder, fit_plane, fit_sphere};
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::{FitInputs, SurfaceClass, fit_inputs};

/// Axes within this angle (degrees) may belong to one cylinder.
const AXIS_ANGLE_TOL_DEG: f64 = 5.0;
/// Axis lines radially closer than this (mm) may coincide.
const AXIS_SEPARATION_TOL: f64 = 1.5;
/// Radii within `max(this, 4% of radius)` (mm) may coincide.
const RADIUS_TOL_MIN: f64 = 1.0;
const RADIUS_TOL_FRACTION: f64 = 0.04;
const PLANE_ANGLE_TOL_DEG: f64 = 3.0;
/// Plane offsets along the shared normal within this (mm) may coincide.
const PLANE_OFFSET_TOL: f64 = 0.75;
const SPHERE_CENTER_TOL: f64 = 1.5;

/// Cheap geometric screen; the union refit makes the actual decision.
fn compatible(a: &SurfaceClass, b: &SurfaceClass) -> bool {
    match (a, b) {
        (SurfaceClass::Cylinder(x), SurfaceClass::Cylinder(y)) => {
            let parallel =
                x.axis.dot(y.axis).abs() >= AXIS_ANGLE_TOL_DEG.to_radians().cos();
            let separation = (y.axis_point - x.axis_point).cross(x.axis).length();
            let radius_tol = RADIUS_TOL_MIN.max(RADIUS_TOL_FRACTION * x.radius);
            parallel
                && separation <= AXIS_SEPARATION_TOL
                && (x.radius - y.radius).abs() <= radius_tol
        }
        (SurfaceClass::Plane(x), SurfaceClass::Plane(y)) => {
            if x.normal.dot(y.normal).abs() < PLANE_ANGLE_TOL_DEG.to_radians().cos() {
                return false;
            }
            let offset_gap = (y.origin - x.origin).dot(x.normal);
            offset_gap.abs() <= PLANE_OFFSET_TOL
        }
        (SurfaceClass::Sphere(x), SurfaceClass::Sphere(y)) => {
            (y.center - x.center).length() <= SPHERE_CENTER_TOL
                && (x.radius - y.radius).abs()
                    <= RADIUS_TOL_MIN.max(RADIUS_TOL_FRACTION * x.radius)
        }
        _ => false,
    }
}

fn refit_like(mesh: &TriangleMesh, faces: &[u32], like: &SurfaceClass) -> Option<SurfaceClass> {
    let FitInputs {
        points,
        normals,
        mean_normal,
        ..
    } = fit_inputs(mesh, faces);
    match like {
        SurfaceClass::Cylinder(_) => fit_cylinder(&points, &normals).map(SurfaceClass::Cylinder),
        SurfaceClass::Plane(_) => fit_plane(&points, Some(mean_normal)).map(SurfaceClass::Plane),
        SurfaceClass::Sphere(_) => fit_sphere(&points).map(SurfaceClass::Sphere),
        _ => None,
    }
}

/// Merges fragments of the same physical surface. `epsilon` is the RMS
/// bound a union refit must meet for a merge to stick.
pub fn merge_fragments(
    mesh: &TriangleMesh,
    features: Vec<FeatureRecord>,
    epsilon: f64,
) -> Vec<FeatureRecord> {
    let mergeable = |surface: &SurfaceClass| {
        matches!(
            surface,
            SurfaceClass::Cylinder(_) | SurfaceClass::Plane(_) | SurfaceClass::Sphere(_)
        )
    };
    let mut order: Vec<usize> = (0..features.len())
        .filter(|&i| mergeable(&features[i].surface))
        .collect();
    // Largest fragment anchors its group so the strongest evidence leads.
    order.sort_by(|&a, &b| features[b].area.total_cmp(&features[a].area));
    let mut consumed = vec![false; features.len()];
    let mut merged: Vec<(usize, SurfaceClass, Vec<u32>, usize)> = Vec::new();
    for (position, &anchor) in order.iter().enumerate() {
        if consumed[anchor] {
            continue;
        }
        let mut surface = features[anchor].surface.clone();
        let mut faces = features[anchor].faces.clone();
        let mut absorbed = 0usize;
        let mut changed = true;
        while changed {
            changed = false;
            for &candidate in &order[position + 1..] {
                if consumed[candidate] || !compatible(&surface, &features[candidate].surface) {
                    continue;
                }
                let mut trial: Vec<u32> = faces.clone();
                trial.extend(&features[candidate].faces);
                let Some(refit) = refit_like(mesh, &trial, &surface) else {
                    continue;
                };
                if refit.rms().is_some_and(|rms| rms <= epsilon) {
                    surface = refit;
                    faces = trial;
                    consumed[candidate] = true;
                    absorbed += 1;
                    changed = true;
                }
            }
        }
        if absorbed > 0 {
            consumed[anchor] = true;
            merged.push((anchor, surface, faces, absorbed));
        }
    }
    // Anchors were processed in area order, not index order; rebuild by
    // index with a keyed lookup so no merged feature is dropped.
    let mut merged_by_anchor: std::collections::HashMap<usize, (SurfaceClass, Vec<u32>, usize)> =
        merged
            .into_iter()
            .map(|(anchor, surface, faces, absorbed)| (anchor, (surface, faces, absorbed)))
            .collect();
    let mut out: Vec<FeatureRecord> = Vec::with_capacity(features.len());
    for (index, feature) in features.into_iter().enumerate() {
        if let Some((surface, faces, absorbed)) = merged_by_anchor.remove(&index) {
            let area = faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
            let mut notes = feature.notes;
            notes.push(format!("merged {absorbed} fragment(s) of the same surface"));
            out.push(FeatureRecord {
                id: feature.id,
                surface,
                face_count: faces.len(),
                area,
                faces,
                notes,
            });
        } else if !consumed[index] {
            out.push(feature);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{SegmentationParams, classify_region, segment};
    use crate::synth;

    #[test]
    fn split_cylinder_halves_merge_into_one() {
        let mesh = synth::open_cylinder(9.0, 30.0, 96, 12);
        // Split the shell into two halves by azimuth and classify each on
        // its own, as RANSAC would after peeling connected components.
        let mut halves: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        for face in 0..mesh.triangles().len() {
            let c = mesh.face_centroid(face);
            halves[usize::from(c.y < 0.0)].push(face as u32);
        }
        let params = SegmentationParams::default();
        let features: Vec<FeatureRecord> = halves
            .iter()
            .enumerate()
            .map(|(id, faces)| {
                let region = crate::segment::Region {
                    faces: faces.clone(),
                    area: faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
                };
                FeatureRecord {
                    id,
                    surface: classify_region(&mesh, &region, 0.05, &params),
                    face_count: faces.len(),
                    area: region.area,
                    faces: faces.clone(),
                    notes: Vec::new(),
                }
            })
            .collect();
        assert!(features
            .iter()
            .all(|f| matches!(f.surface, SurfaceClass::Cylinder(_))));
        let area_before: f64 = features.iter().map(|f| f.area).sum();
        let merged = merge_fragments(&mesh, features, 0.05);
        let area_after: f64 = merged.iter().map(|f| f.area).sum();
        assert!((area_before - area_after).abs() < 1e-6, "area not conserved");
        assert_eq!(merged.len(), 1, "halves did not merge");
        let SurfaceClass::Cylinder(fit) = &merged[0].surface else {
            panic!("merged feature is not a cylinder");
        };
        assert!((fit.radius - 9.0).abs() < 0.01);
        assert!(merged[0].notes.iter().any(|n| n.contains("merged 1 fragment")));
    }

    #[test]
    fn distinct_radii_refuse_to_merge() {
        // Two coaxial shells 0.8 mm apart in radius: compatible by the
        // cheap screen, but the union refit must reject the merge.
        let mut soup = synth::open_cylinder_soup(9.0, 30.0, 96, 12);
        soup.extend(
            synth::open_cylinder_soup(9.8, 30.0, 96, 12)
                .into_iter()
                .map(|t| t.map(|p| artificer_geometry::Point3::new(p.x, p.y, p.z + 40.0))),
        );
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let params = SegmentationParams::default();
        let features: Vec<FeatureRecord> = segment(&mesh, &params)
            .into_iter()
            .enumerate()
            .map(|(id, region)| FeatureRecord {
                id,
                surface: classify_region(&mesh, &region, 0.05, &params),
                face_count: region.faces.len(),
                area: region.area,
                faces: region.faces,
                notes: Vec::new(),
            })
            .collect();
        let cylinders_before = features
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::Cylinder(_)))
            .count();
        assert_eq!(cylinders_before, 2);
        let area_before: f64 = features.iter().map(|f| f.area).sum();
        let merged = merge_fragments(&mesh, features, 0.05);
        let area_after: f64 = merged.iter().map(|f| f.area).sum();
        assert!((area_before - area_after).abs() < 1e-6, "area not conserved");
        let cylinders_after = merged
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::Cylinder(_)))
            .count();
        assert_eq!(cylinders_after, 2, "distinct radii were wrongly merged");
    }
}

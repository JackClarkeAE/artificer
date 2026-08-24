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

use artificer_geometry::Vector3;

use crate::fit::{fit_cone, fit_cylinder, fit_plane, fit_sphere};
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
            let parallel = x.axis.dot(y.axis).abs() >= AXIS_ANGLE_TOL_DEG.to_radians().cos();
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
                && (x.radius - y.radius).abs() <= RADIUS_TOL_MIN.max(RADIUS_TOL_FRACTION * x.radius)
        }
        _ => false,
    }
}

pub(crate) fn refit_like(
    mesh: &TriangleMesh,
    faces: &[u32],
    like: &SurfaceClass,
) -> Option<SurfaceClass> {
    let FitInputs {
        points,
        normals,
        cone_samples,
        mean_normal,
    } = fit_inputs(mesh, faces);
    match like {
        SurfaceClass::Cylinder(_) => fit_cylinder(&points, &normals).map(SurfaceClass::Cylinder),
        SurfaceClass::Plane(_) => fit_plane(&points, Some(mean_normal)).map(SurfaceClass::Plane),
        SurfaceClass::Sphere(_) => fit_sphere(&points).map(SurfaceClass::Sphere),
        SurfaceClass::Cone(_) => fit_cone(&cone_samples).map(SurfaceClass::Cone),
        _ => None,
    }
}

/// A small feature's points either lie on a large anchor surface or they
/// do not — regardless of what shape the small patch's own noisy fit
/// chose. Absorption tests actual point membership (distance band plus
/// normal agreement) against the biggest surfaces and folds compatible
/// patches in, then refits each grown anchor.
///
/// This is the coplanar/coaxial consolidation the pairwise merge cannot
/// do: a 12-face patch on a flat face may have fit as a tilted plane, a
/// huge sphere, or stayed freeform — its parameters are noise, but its
/// membership is decisive.
pub fn absorb_into_anchors(
    mesh: &TriangleMesh,
    features: Vec<FeatureRecord>,
    epsilon: f64,
) -> Vec<FeatureRecord> {
    const ANCHOR_MIN_AREA: f64 = 50.0;
    const SAMPLE_BUDGET: usize = 36;
    const MIN_PASS_FRACTION: f64 = 0.8;
    let min_alignment = 25.0f64.to_radians().cos();
    let mut order: Vec<usize> = (0..features.len()).collect();
    order.sort_by(|&a, &b| features[b].area.total_cmp(&features[a].area));
    let anchors: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| {
            features[i].area >= ANCHOR_MIN_AREA
                && !matches!(features[i].surface, SurfaceClass::Freeform)
        })
        .collect();
    if anchors.is_empty() {
        return features;
    }
    let mut consumed = vec![false; features.len()];
    let mut additions: Vec<Vec<u32>> = vec![Vec::new(); features.len()];
    let mut absorbed_counts = vec![0usize; features.len()];
    // Smallest candidates first; each may join exactly one anchor.
    for &candidate in order.iter().rev() {
        let candidate_area = features[candidate].area;
        let samples: Vec<(artificer_geometry::Point3, Option<Vector3>, f64)> = {
            let faces = &features[candidate].faces;
            let stride = faces.len().div_ceil(SAMPLE_BUDGET).max(1);
            faces
                .iter()
                .step_by(stride)
                .map(|&face| {
                    (
                        mesh.face_centroid(face as usize),
                        mesh.face_normal(face as usize),
                        mesh.face_area(face as usize),
                    )
                })
                .collect()
        };
        if samples.is_empty() {
            continue;
        }
        let mut best: Option<(usize, f64)> = None;
        for &anchor in &anchors {
            if anchor == candidate
                || consumed[anchor]
                || features[anchor].area < 2.0 * candidate_area
            {
                continue;
            }
            let mut passing = 0usize;
            let mut squared = 0.0;
            for (point, normal, _) in &samples {
                let Some((distance, surface_normal)) = features[anchor].surface.probe(*point)
                else {
                    continue;
                };
                let aligned = normal.is_none_or(|n| n.dot(surface_normal).abs() >= min_alignment);
                if distance.abs() <= epsilon && aligned {
                    passing += 1;
                }
                squared += distance * distance;
            }
            let rms = (squared / samples.len() as f64).sqrt();
            if passing as f64 >= MIN_PASS_FRACTION * samples.len() as f64
                && rms <= epsilon
                && best.is_none_or(|(_, best_rms)| rms < best_rms)
            {
                best = Some((anchor, rms));
            }
        }
        if let Some((anchor, _)) = best {
            consumed[candidate] = true;
            let faces = features[candidate].faces.clone();
            additions[anchor].extend(faces);
            absorbed_counts[anchor] += 1;
        }
    }
    let mut out = Vec::with_capacity(features.len());
    for (index, mut feature) in features.into_iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if absorbed_counts[index] > 0 {
            feature.faces.append(&mut additions[index]);
            feature.face_count = feature.faces.len();
            feature.area = feature
                .faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
            feature.notes.push(format!(
                "absorbed {} on-surface patch(es)",
                absorbed_counts[index]
            ));
            if let Some(refit) = refit_like(mesh, &feature.faces, &feature.surface)
                && refit.rms().is_some_and(|rms| rms <= epsilon)
            {
                feature.surface = refit;
            }
        }
        out.push(feature);
    }
    out
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
        // The anchor's ORIGINAL surface, never updated: every accepted
        // refit is measured against it as well as against the running
        // one. Compatibility is checked pairwise and the refit then
        // moves the running surface, so without this the chain
        // ratchets — each step legal, the accumulation arbitrary. On a
        // noisy wheel spacer that walked two lug-hole walls 16 mm out
        // to a radius outside the part. Same trap the constraint
        // frames hit (A parallel B, B square to C says nothing about A
        // and C), and the same answer: cap the accumulated correction
        // by what one step was ever allowed.
        let anchor_surface = features[anchor].surface.clone();
        let mut surface = anchor_surface.clone();
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
                // Residual tolerance scales with the scan's noise, and
                // must: a legitimate union of noisy fragments cannot
                // fit tighter than the noise. Geometric identity does
                // not scale — whether two surfaces are the same
                // physical surface is a question about the part, not
                // about the scanner — so the drift test keeps its own
                // absolute constants.
                if refit.rms().is_some_and(|rms| rms <= epsilon)
                    && compatible(&anchor_surface, &refit)
                {
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

    /// A fragment whose stated fit is a lie must not drag an honest
    /// anchor off its own material.
    ///
    /// The compatibility screen reads the candidate's *claimed*
    /// geometry, and upstream stages do produce claims their faces do
    /// not support — displaced fits are exactly the failure this
    /// guard exists for. The union refit follows the faces, so the
    /// merged surface leaves the anchor behind unless something
    /// checks where it landed.
    #[test]
    fn a_fragment_whose_fit_lies_cannot_drag_the_anchor() {
        use crate::fit::{CylinderFit, DeviationStats};
        use artificer_geometry::Point3;
        let arc_at = |offset: f64, sweep_deg: f64, segments: usize| -> Vec<[Point3; 3]> {
            synth::cylinder_arc_soup(5.0, 8.0, 0.0, sweep_deg.to_radians(), segments, 8)
                .into_iter()
                .map(|piece| piece.map(|p| Point3::new(p.x + offset, p.y, p.z)))
                .collect()
        };
        let mut soup = arc_at(0.0, 300.0, 60);
        let split = soup.len();
        // Material ten millimetres away — nowhere near the anchor.
        soup.extend(arc_at(10.0, 200.0, 40));
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("mesh");
        let cylinder = |x: f64| {
            SurfaceClass::Cylinder(CylinderFit {
                axis_point: Point3::new(x, 0.0, 4.0),
                axis: Vector3::new(0.0, 0.0, 1.0),
                radius: 5.0,
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            })
        };
        let make = |id: usize, surface: SurfaceClass, faces: Vec<u32>| FeatureRecord {
            id,
            surface,
            face_count: faces.len(),
            area: faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
            faces,
            notes: Vec::new(),
        };
        let features = vec![
            make(0, cylinder(0.0), (0..split as u32).collect()),
            // Claims to sit a legal step away; its faces do not.
            make(
                1,
                cylinder(1.4),
                (split as u32..mesh.triangles().len() as u32).collect(),
            ),
        ];
        // Loose enough to rubber-stamp the union, as a noisy scan's
        // adaptive tolerance is.
        let merged = merge_fragments(&mesh, features, 5.0);
        let anchor = merged.iter().find(|f| f.id == 0).expect("anchor survives");
        let SurfaceClass::Cylinder(fit) = &anchor.surface else {
            panic!("anchor is a cylinder");
        };
        assert!(
            fit.axis_point.x.abs() <= AXIS_SEPARATION_TOL + 1e-6,
            "anchor was dragged to x {:.2}",
            fit.axis_point.x
        );
    }

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
        assert!(
            features
                .iter()
                .all(|f| matches!(f.surface, SurfaceClass::Cylinder(_)))
        );
        let area_before: f64 = features.iter().map(|f| f.area).sum();
        let merged = merge_fragments(&mesh, features, 0.05);
        let area_after: f64 = merged.iter().map(|f| f.area).sum();
        assert!(
            (area_before - area_after).abs() < 1e-6,
            "area not conserved"
        );
        assert_eq!(merged.len(), 1, "halves did not merge");
        let SurfaceClass::Cylinder(fit) = &merged[0].surface else {
            panic!("merged feature is not a cylinder");
        };
        assert!((fit.radius - 9.0).abs() < 0.01);
        assert!(
            merged[0]
                .notes
                .iter()
                .any(|n| n.contains("merged 1 fragment"))
        );
    }

    #[test]
    fn on_plane_patch_absorbs_regardless_of_its_own_fit() {
        use crate::fit::{DeviationStats, SphereFit, fit_plane};
        use artificer_geometry::{Point3, Vector3};
        // A big flat plate plus a small disconnected coplanar patch whose
        // own "fit" is a nonsense sphere: membership must win anyway.
        let x = Vector3::new(1.0, 0.0, 0.0);
        let y = Vector3::new(0.0, 1.0, 0.0);
        let mut soup = synth::plane_patch_soup(Point3::new(0.0, 0.0, 0.0), x, y, 20.0, 20.0, 8, 8);
        let base_count = soup.len();
        soup.extend(synth::plane_patch_soup(
            Point3::new(25.0, 0.0, 0.0),
            x,
            y,
            2.0,
            2.0,
            2,
            2,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let base_faces: Vec<u32> = (0..base_count as u32).collect();
        let patch_faces: Vec<u32> = (base_count as u32..mesh.triangles().len() as u32).collect();
        let base_points: Vec<Point3> = base_faces
            .iter()
            .map(|&f| mesh.face_centroid(f as usize))
            .collect();
        let plane = fit_plane(&base_points, Some(Vector3::new(0.0, 0.0, 1.0))).unwrap();
        let features = vec![
            FeatureRecord {
                id: 0,
                surface: SurfaceClass::Plane(plane),
                face_count: base_faces.len(),
                area: 400.0,
                faces: base_faces,
                notes: Vec::new(),
            },
            FeatureRecord {
                id: 1,
                surface: SurfaceClass::Sphere(SphereFit {
                    center: Point3::new(26.0, 1.0, -500.0),
                    radius: 500.0,
                    deviation: DeviationStats {
                        rms: 0.01,
                        max_abs: 0.02,
                    },
                }),
                face_count: patch_faces.len(),
                area: 4.0,
                faces: patch_faces,
                notes: Vec::new(),
            },
        ];
        let absorbed = absorb_into_anchors(&mesh, features, 0.05);
        assert_eq!(absorbed.len(), 1, "patch was not absorbed");
        assert!(matches!(absorbed[0].surface, SurfaceClass::Plane(_)));
        assert_eq!(absorbed[0].face_count, mesh.triangles().len());
        assert!(absorbed[0].notes.iter().any(|n| n.contains("absorbed 1")));
    }

    #[test]
    fn off_plane_patch_is_not_absorbed() {
        use crate::fit::fit_plane;
        use artificer_geometry::{Point3, Vector3};
        let x = Vector3::new(1.0, 0.0, 0.0);
        let y = Vector3::new(0.0, 1.0, 0.0);
        let mut soup = synth::plane_patch_soup(Point3::new(0.0, 0.0, 0.0), x, y, 20.0, 20.0, 8, 8);
        let base_count = soup.len();
        // The patch floats 1 mm above the plane: far outside a 0.05 band.
        soup.extend(synth::plane_patch_soup(
            Point3::new(25.0, 0.0, 1.0),
            x,
            y,
            2.0,
            2.0,
            2,
            2,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let base_faces: Vec<u32> = (0..base_count as u32).collect();
        let patch_faces: Vec<u32> = (base_count as u32..mesh.triangles().len() as u32).collect();
        let base_points: Vec<Point3> = base_faces
            .iter()
            .map(|&f| mesh.face_centroid(f as usize))
            .collect();
        let plane = fit_plane(&base_points, Some(Vector3::new(0.0, 0.0, 1.0))).unwrap();
        let patch_points: Vec<Point3> = patch_faces
            .iter()
            .map(|&f| mesh.face_centroid(f as usize))
            .collect();
        let patch_plane = fit_plane(&patch_points, Some(Vector3::new(0.0, 0.0, 1.0))).unwrap();
        let features = vec![
            FeatureRecord {
                id: 0,
                surface: SurfaceClass::Plane(plane),
                face_count: base_faces.len(),
                area: 400.0,
                faces: base_faces,
                notes: Vec::new(),
            },
            FeatureRecord {
                id: 1,
                surface: SurfaceClass::Plane(patch_plane),
                face_count: patch_faces.len(),
                area: 4.0,
                faces: patch_faces,
                notes: Vec::new(),
            },
        ];
        let absorbed = absorb_into_anchors(&mesh, features, 0.05);
        assert_eq!(absorbed.len(), 2, "offset patch was wrongly absorbed");
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
        assert!(
            (area_before - area_after).abs() < 1e-6,
            "area not conserved"
        );
        let cylinders_after = merged
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::Cylinder(_)))
            .count();
        assert_eq!(cylinders_after, 2, "distinct radii were wrongly merged");
    }
}

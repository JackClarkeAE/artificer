//! Checks that a feature's fitted surface actually lies under its own
//! material, and demotes it when it does not.
//!
//! Every stage here hands its output to the next one, and none of them
//! re-reads the scan to ask whether the claim still holds. That is fine
//! while each stage is right, and silently catastrophic when one is
//! not: a merge that walked a plane through the middle of a 15 mm slab
//! produced a "plane" with a 15 mm residual holding nine hundred
//! thousand faces, and every downstream stage — claiming, instancing,
//! rebuilding — dutifully built on it. Nothing was wrong with those
//! stages. They were told a lie in the one language they cannot check.
//!
//! So this is the check: probe the fitted surface at the feature's own
//! face centroids and measure **what share of them it fails to
//! describe**. A share and not a median, because the corrupt case is a
//! *mixture* — the cylinder above held 57,367 faces of which 37,912
//! had been claimed onto it later, sitting exactly on the surface, so
//! its median deviation was near zero while a third of its material
//! stood tens of millimetres away. A median calls that healthy. A
//! share does not. And a share and not a mean or a max, because
//! claiming legitimately hands a feature a few strays and one outlier
//! must not condemn an honest fit. A feature that fails is demoted to
//! freeform, where the blend discriminator and the residue machinery
//! can make an honest second attempt on it.

use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::SurfaceClass;
use crate::transform::RigidTransform;

/// A face further than this many tolerances from its feature's surface
/// is not described by it.
const SUPPORT_FACTOR: f64 = 3.0;
/// A feature may carry this share of undescribed faces — claiming and
/// segmentation both hand over a few strays — before its surface stops
/// being a claim about that material.
const MAX_UNDESCRIBED: f64 = 0.2;
/// ...but never tighter than this (mm): on a quiet synthetic the
/// tolerance can be small enough that ordinary tessellation chord error
/// would condemn a perfect fit.
const SUPPORT_FLOOR: f64 = 0.05;
/// Faces probed per feature. The share is a population statistic; a
/// few hundred samples pin it as well as a million.
const SAMPLES: usize = 600;

/// What the validation pass found.
pub struct Support {
    pub demoted: usize,
    pub demoted_area: f64,
    /// The worst offenders, largest first, for the report.
    pub notes: Vec<String>,
}

/// How much of a feature its own surface fails to describe: the share
/// of sampled faces past `limit`, and the deviation at the ninetieth
/// percentile for the report. `None` when the surface cannot be probed
/// (the pattern) or there are no faces to ask about.
pub fn undescribed_share(
    mesh: &TriangleMesh,
    feature: &FeatureRecord,
    to_frame: &RigidTransform,
    limit: f64,
) -> Option<(f64, f64)> {
    if feature.faces.is_empty() {
        return None;
    }
    let stride = (feature.faces.len() / SAMPLES).max(1);
    let mut deviations: Vec<f64> = Vec::with_capacity(SAMPLES + 1);
    for &face in feature.faces.iter().step_by(stride) {
        let centroid = to_frame.apply_point(mesh.face_centroid(face as usize));
        let (distance, _) = feature.surface.probe(centroid)?;
        deviations.push(distance.abs());
    }
    if deviations.is_empty() {
        return None;
    }
    let off = deviations.iter().filter(|&&d| d > limit).count();
    deviations.sort_by(f64::total_cmp);
    let ninetieth = deviations[(deviations.len() * 9 / 10).min(deviations.len() - 1)];
    Some((off as f64 / deviations.len() as f64, ninetieth))
}

/// Demotes every analytic feature whose own material does not lie on
/// its surface. Frame-agnostic: pass the transform the features are
/// expressed in.
pub fn demote_unsupported(
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    to_frame: &RigidTransform,
    tolerance: f64,
) -> Support {
    let limit = (SUPPORT_FACTOR * tolerance).max(SUPPORT_FLOOR);
    let mut found: Vec<(f64, String)> = Vec::new();
    let mut demoted_area = 0.0;
    for feature in features.iter_mut() {
        if matches!(feature.surface, SurfaceClass::Freeform) {
            continue;
        }
        let Some((share, ninetieth)) =
            undescribed_share(mesh, feature, to_frame, limit)
        else {
            continue;
        };
        if share <= MAX_UNDESCRIBED {
            continue;
        }
        found.push((
            feature.area,
            format!(
                "#{} {} carried {:.0} mm^2 of which {:.0}% sits off it, reaching {:.2} mm",
                feature.id,
                crate::finalize::feature_label(&feature.surface),
                feature.area,
                100.0 * share,
                ninetieth
            ),
        ));
        demoted_area += feature.area;
        feature.surface = SurfaceClass::Freeform;
        feature.notes.push(format!(
            "demoted: {:.0}% of its own faces sit further than {limit:.2} mm from this \
             surface, reaching {ninetieth:.2} mm — it does not describe them",
            100.0 * share
        ));
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    let demoted = found.len();
    let mut notes: Vec<String> = found.into_iter().take(4).map(|(_, note)| note).collect();
    if demoted > notes.len() {
        notes.push(format!("... and {} more", demoted - notes.len()));
    }
    Support {
        demoted,
        demoted_area,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{DeviationStats, PlaneFit};
    use crate::synth;
    use artificer_geometry::{Point3, Vector3};

    fn plane_feature(id: usize, offset: f64, faces: Vec<u32>, area: f64) -> FeatureRecord {
        FeatureRecord {
            id,
            surface: SurfaceClass::Plane(PlaneFit {
                origin: Point3::new(0.0, 0.0, offset),
                normal: Vector3::new(0.0, 0.0, 1.0),
                deviation: DeviationStats { rms: 0.0, max_abs: 0.0 },
            }),
            face_count: faces.len(),
            area,
            faces,
            notes: Vec::new(),
        }
    }

    /// A plane driven through the middle of a slab holds both of its
    /// faces and describes neither — exactly the merge failure this
    /// exists to catch.
    #[test]
    fn a_plane_through_the_middle_of_a_slab_is_demoted() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(
                Point3::new(-20.0, -20.0, 0.0),
                Vector3::new(40.0, 40.0, 15.0),
                6,
            ),
            1e-6,
        )
        .expect("mesh");
        // Every level face of the slab: the top at z 15, the floor at z 0.
        let level: Vec<u32> = (0..mesh.triangles().len() as u32)
            .filter(|&face| {
                mesh.face_normal(face as usize)
                    .is_some_and(|n| n.z.abs() > 0.9)
            })
            .collect();
        assert!(!level.is_empty());
        let mut features = vec![plane_feature(0, 7.5, level, 3200.0)];
        let support = demote_unsupported(
            &mesh,
            &mut features,
            &RigidTransform::IDENTITY,
            0.05,
        );
        assert_eq!(support.demoted, 1, "the mid-slab plane must not survive");
        assert!(matches!(features[0].surface, SurfaceClass::Freeform));
    }

    /// The mixture: most of a feature's faces sit exactly on its
    /// surface — later stages claim them there — while a large
    /// minority stands far away. The median calls this healthy, which
    /// is why the statistic is a share.
    #[test]
    fn a_fit_most_of_whose_faces_were_claimed_onto_it_is_still_judged() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(
                Point3::new(-20.0, -20.0, 0.0),
                Vector3::new(40.0, 40.0, 15.0),
                6,
            ),
            1e-6,
        )
        .expect("mesh");
        let level = |high: bool| -> Vec<u32> {
            (0..mesh.triangles().len() as u32)
                .filter(|&face| {
                    mesh.face_normal(face as usize)
                        .is_some_and(|n| if high { n.z > 0.9 } else { n.z < -0.9 })
                })
                .collect()
        };
        let (top, floor) = (level(true), level(false));
        assert!(top.len() > 8 && floor.len() > 8);
        // Two thirds honestly on the top plane, one third fifteen
        // millimetres below it.
        let mut faces = top.clone();
        faces.extend(top.iter().copied());
        faces.extend(floor.iter().copied());
        let mut features = vec![plane_feature(0, 15.0, faces, 4800.0)];
        let median_would_pass = {
            let deviations: Vec<f64> = features[0]
                .faces
                .iter()
                .map(|&f| {
                    features[0]
                        .surface
                        .probe(mesh.face_centroid(f as usize))
                        .expect("probe")
                        .0
                        .abs()
                })
                .collect();
            let mut sorted = deviations.clone();
            sorted.sort_by(f64::total_cmp);
            sorted[sorted.len() / 2]
        };
        assert!(
            median_would_pass < 0.05,
            "the median must look healthy for this test to mean anything: {median_would_pass}"
        );
        let support = demote_unsupported(
            &mesh,
            &mut features,
            &RigidTransform::IDENTITY,
            0.05,
        );
        assert_eq!(support.demoted, 1, "a third of the material is elsewhere");
    }

    /// An honest plane keeps its fit.
    #[test]
    fn a_plane_on_its_own_material_survives() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(
                Point3::new(-20.0, -20.0, 0.0),
                Vector3::new(40.0, 40.0, 15.0),
                6,
            ),
            1e-6,
        )
        .expect("mesh");
        let top: Vec<u32> = (0..mesh.triangles().len() as u32)
            .filter(|&face| {
                mesh.face_normal(face as usize).is_some_and(|n| n.z > 0.9)
            })
            .collect();
        let mut features = vec![plane_feature(0, 15.0, top, 1600.0)];
        let support = demote_unsupported(
            &mesh,
            &mut features,
            &RigidTransform::IDENTITY,
            0.05,
        );
        assert_eq!(support.demoted, 0, "an honest fit is left alone");
        assert!(matches!(features[0].surface, SurfaceClass::Plane(_)));
    }
}

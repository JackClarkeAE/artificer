//! Automatic datum alignment from classified features.
//!
//! A scan arrives in an arbitrary pose. Before dimensions and directions
//! can snap to design intent, the part must sit in its own datum frame:
//! the dominant feature direction becomes +Z, the strongest perpendicular
//! direction becomes +X, and the origin lands where the dominant axis
//! meets the largest perpendicular plane. This mirrors how an inspector
//! sets up a part: primary datum from the biggest functional surface
//! family, secondary from a perpendicular face, origin at their meeting.

use artificer_geometry::{Point3, Vector3};

use crate::report::FeatureRecord;
use crate::segment::SurfaceClass;
use crate::transform::{RigidTransform, normalize, orthonormal_basis};

/// Directions closer than this (degrees) merge into one direction cluster.
const CLUSTER_ANGLE_DEG: f64 = 5.0;
/// A cluster is "perpendicular" to Z when within this many degrees of 90.
const PERPENDICULAR_SLACK_DEG: f64 = 10.0;

#[derive(Clone, Debug)]
pub struct DatumAlignment {
    /// Maps scan coordinates into the datum frame.
    pub transform: RigidTransform,
    /// Human-readable account of which features supplied each datum.
    pub notes: Vec<String>,
}

struct Cluster {
    representative: Vector3,
    weighted_sum: Vector3,
    weight: f64,
}

fn cluster_directions(entries: impl Iterator<Item = (Vector3, f64)>) -> Vec<Cluster> {
    let cos_merge = CLUSTER_ANGLE_DEG.to_radians().cos();
    let mut clusters: Vec<Cluster> = Vec::new();
    for (direction, weight) in entries {
        let Some(unit) = normalize(direction) else {
            continue;
        };
        match clusters
            .iter_mut()
            .find(|c| c.representative.dot(unit).abs() >= cos_merge)
        {
            Some(cluster) => {
                // Sign-insensitive: flip onto the representative first.
                let oriented = if cluster.representative.dot(unit) < 0.0 {
                    unit * -1.0
                } else {
                    unit
                };
                cluster.weighted_sum = cluster.weighted_sum + oriented * weight;
                cluster.weight += weight;
                if let Some(mean) = normalize(cluster.weighted_sum) {
                    cluster.representative = mean;
                }
            }
            None => clusters.push(Cluster {
                representative: unit,
                weighted_sum: unit * weight,
                weight,
            }),
        }
    }
    clusters.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    clusters
}

fn feature_direction(surface: &SurfaceClass) -> Option<Vector3> {
    match surface {
        SurfaceClass::Plane(fit) => Some(fit.normal),
        SurfaceClass::Cylinder(fit) => Some(fit.axis),
        SurfaceClass::Cone(fit) => Some(fit.axis),
        SurfaceClass::Blend(fit) => Some(fit.axis),
        SurfaceClass::Sphere(_) | SurfaceClass::Freeform => None,
    }
}

/// Derives a datum frame from classified features. Returns `None` when no
/// analytic feature offers a usable direction.
pub fn auto_datum_alignment(features: &[FeatureRecord]) -> Option<DatumAlignment> {
    let clusters = cluster_directions(features.iter().filter_map(|f| {
        feature_direction(&f.surface).map(|direction| (direction, f.area))
    }));
    let primary = clusters.first()?;
    let z = primary.representative;
    let mut notes = vec![format!(
        "primary datum Z from {:.0} mm^2 of aligned features, direction ({:+.3} {:+.3} {:+.3})",
        primary.weight, z.x, z.y, z.z
    )];
    // Secondary: heaviest cluster roughly perpendicular to Z.
    let max_dot = PERPENDICULAR_SLACK_DEG.to_radians().sin();
    let x_hint = match clusters[1..]
        .iter()
        .find(|c| c.representative.dot(z).abs() <= max_dot)
    {
        Some(secondary) => {
            notes.push(format!(
                "secondary datum X from {:.0} mm^2 of perpendicular features",
                secondary.weight
            ));
            secondary.representative
        }
        None => {
            notes.push("no perpendicular feature family; X is arbitrary".to_owned());
            orthonormal_basis(z).0
        }
    };
    let cos_near = CLUSTER_ANGLE_DEG.to_radians().cos();
    // Lateral origin: the axis line of the largest cylinder or cone whose
    // axis follows Z; spheres and plane centroids are weaker fallbacks.
    let mut lateral: Option<(f64, Point3, &'static str)> = None;
    for feature in features {
        let candidate = match &feature.surface {
            SurfaceClass::Cylinder(fit) if fit.axis.dot(z).abs() >= cos_near => {
                Some((fit.axis_point, "cylinder axis"))
            }
            SurfaceClass::Cone(fit) if fit.axis.dot(z).abs() >= cos_near => {
                Some((fit.apex, "cone apex"))
            }
            SurfaceClass::Sphere(fit) => Some((fit.center, "sphere center")),
            _ => None,
        };
        if let Some((point, label)) = candidate
            && lateral.is_none_or(|(best_area, _, _)| feature.area > best_area)
        {
            lateral = Some((feature.area, point, label));
        }
    }
    let (lateral_point, lateral_label) = match lateral {
        Some((_, point, label)) => (point, label),
        None => {
            let fallback = features
                .iter()
                .find_map(|f| match &f.surface {
                    SurfaceClass::Plane(fit) => Some(fit.origin),
                    _ => None,
                })
                .unwrap_or_default();
            (fallback, "largest plane centroid")
        }
    };
    // Height origin: the largest plane perpendicular to Z sets Z = 0.
    let level_plane = features
        .iter()
        .filter_map(|f| match &f.surface {
            SurfaceClass::Plane(fit) if fit.normal.dot(z).abs() >= cos_near => {
                Some((f.area, fit))
            }
            _ => None,
        })
        .max_by(|a, b| a.0.total_cmp(&b.0));
    let origin = match level_plane {
        Some((area, plane)) => {
            notes.push(format!(
                "origin: {lateral_label} at the level of a {area:.0} mm^2 perpendicular plane"
            ));
            let height = (plane.origin - lateral_point).dot(z);
            lateral_point + z * height
        }
        None => {
            notes.push(format!("origin: {lateral_label} (no perpendicular plane found)"));
            lateral_point
        }
    };
    let transform = RigidTransform::to_frame(origin, x_hint, z)?;
    Some(DatumAlignment { transform, notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{CylinderFit, DeviationStats, PlaneFit};

    fn record(id: usize, surface: SurfaceClass, area: f64) -> FeatureRecord {
        FeatureRecord {
            id,
            surface,
            face_count: 100,
            area,
            faces: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn tilted_cylinder_and_plane_define_the_frame() {
        let axis = normalize(Vector3::new(0.3, -0.2, 0.9)).unwrap();
        let axis_point = Point3::new(5.0, 3.0, 1.0);
        let plane_origin = axis_point + axis * 12.0 + Vector3::new(0.4, 0.2, 0.0);
        let no_dev = DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        };
        let features = vec![
            record(
                0,
                SurfaceClass::Cylinder(CylinderFit {
                    axis_point,
                    axis,
                    radius: 10.0,
                    deviation: no_dev,
                }),
                2000.0,
            ),
            record(
                1,
                SurfaceClass::Plane(PlaneFit {
                    origin: plane_origin,
                    normal: axis,
                    deviation: no_dev,
                }),
                1500.0,
            ),
        ];
        let alignment = auto_datum_alignment(&features).unwrap();
        let t = &alignment.transform;
        // The cylinder axis must map onto +/-Z.
        let mapped_axis = t.apply_vector(axis);
        assert!(mapped_axis.z.abs() > 1.0 - 1e-9, "axis {mapped_axis:?}");
        // A point on the cylinder axis must land on the datum Z axis.
        let on_axis = t.apply_point(axis_point + axis * 4.0);
        assert!(on_axis.x.abs() < 1e-9 && on_axis.y.abs() < 1e-9);
        // The perpendicular plane must sit at Z = 0.
        let level = t.apply_point(plane_origin);
        assert!(level.z.abs() < 1e-9, "plane level {}", level.z);
    }

    #[test]
    fn no_directional_features_yields_none() {
        assert!(auto_datum_alignment(&[]).is_none());
    }
}

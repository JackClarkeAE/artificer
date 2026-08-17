//! Canonicalization of fitted primitives into design-intent geometry.
//!
//! Scans never come back exactly square: a milled face reads 89.94 degrees,
//! a drilled hole reads 7.98 mm. This stage snaps directions to datum axes,
//! dimensions to a round grid, and harmonizes families (coplanar planes,
//! coaxial cylinders) — recording every adjustment so the metrology story
//! stays honest.

use artificer_geometry::{Point3, Vector3};

use crate::segment::SurfaceClass;
use crate::transform::normalize;

#[derive(Clone, Debug)]
pub struct SnapPolicy {
    /// Directions within this many degrees of a datum axis snap onto it.
    pub angle_tolerance_deg: f64,
    /// Lengths snap to multiples of this grid (mm)...
    pub length_grid: f64,
    /// ...when they are within this distance of the grid line (mm).
    pub length_tolerance: f64,
    /// Candidate datum directions (sign-insensitive).
    pub datum_directions: Vec<Vector3>,
}

impl Default for SnapPolicy {
    fn default() -> Self {
        Self {
            angle_tolerance_deg: 2.0,
            length_grid: 0.5,
            length_tolerance: 0.1,
            datum_directions: vec![
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ],
        }
    }
}

impl SnapPolicy {
    /// Snaps a unit direction to the nearest datum axis when within the
    /// angular tolerance. Returns the snapped direction and the original
    /// angular deviation in degrees.
    pub fn snap_direction(&self, direction: Vector3) -> Option<(Vector3, f64)> {
        let unit = normalize(direction)?;
        let mut best: Option<(Vector3, f64)> = None;
        for datum in &self.datum_directions {
            let Some(datum_unit) = normalize(*datum) else {
                continue;
            };
            let dot = unit.dot(datum_unit);
            let angle = dot.abs().clamp(0.0, 1.0).acos().to_degrees();
            if angle <= self.angle_tolerance_deg
                && best.is_none_or(|(_, best_angle)| angle < best_angle)
            {
                let oriented = if dot < 0.0 {
                    datum_unit * -1.0
                } else {
                    datum_unit
                };
                best = Some((oriented, angle));
            }
        }
        best
    }

    /// Snaps a length to the grid when within tolerance. Returns the snapped
    /// value and the signed adjustment applied.
    pub fn snap_length(&self, value: f64) -> Option<(f64, f64)> {
        if self.length_grid.is_nan() || self.length_grid <= 0.0 {
            return None;
        }
        let snapped = (value / self.length_grid).round() * self.length_grid;
        let delta = snapped - value;
        // Below a nanometre the value is already on-grid; snapping it would
        // only generate noise notes.
        (delta.abs() <= self.length_tolerance && delta.abs() > 1e-6).then_some((snapped, delta))
    }
}

/// Applies direction and dimension snapping to a single fitted surface,
/// returning human-readable notes describing each adjustment.
pub fn snap_surface(surface: &mut SurfaceClass, policy: &SnapPolicy) -> Vec<String> {
    let mut notes = Vec::new();
    match surface {
        SurfaceClass::Plane(fit) => {
            if let Some((snapped, angle)) = policy.snap_direction(fit.normal) {
                if angle > 1e-9 {
                    notes.push(format!(
                        "normal snapped to ({:+.0} {:+.0} {:+.0}), was {:.3} deg off",
                        snapped.x, snapped.y, snapped.z, angle
                    ));
                }
                fit.normal = snapped;
                let offset = (fit.origin - Point3::default()).dot(snapped);
                if let Some((new_offset, delta)) = policy.snap_length(offset) {
                    fit.origin = fit.origin + snapped * (new_offset - offset);
                    notes.push(format!(
                        "plane offset {offset:.3} snapped to {new_offset:.3} ({delta:+.3})"
                    ));
                }
            }
        }
        SurfaceClass::Cylinder(fit) => {
            if let Some((snapped, angle)) = policy.snap_direction(fit.axis) {
                if angle > 1e-9 {
                    notes.push(format!(
                        "axis snapped to ({:+.0} {:+.0} {:+.0}), was {:.3} deg off",
                        snapped.x, snapped.y, snapped.z, angle
                    ));
                }
                fit.axis = snapped;
            }
            let diameter = fit.radius * 2.0;
            if let Some((new_diameter, delta)) = policy.snap_length(diameter) {
                fit.radius = new_diameter / 2.0;
                notes.push(format!(
                    "diameter {diameter:.3} snapped to {new_diameter:.3} ({delta:+.3})"
                ));
            }
        }
        SurfaceClass::Sphere(fit) => {
            let diameter = fit.radius * 2.0;
            if let Some((new_diameter, delta)) = policy.snap_length(diameter) {
                fit.radius = new_diameter / 2.0;
                notes.push(format!(
                    "diameter {diameter:.3} snapped to {new_diameter:.3} ({delta:+.3})"
                ));
            }
        }
        SurfaceClass::Cone(fit) => {
            if let Some((snapped, angle)) = policy.snap_direction(fit.axis) {
                if angle > 1e-9 {
                    notes.push(format!(
                        "axis snapped to ({:+.0} {:+.0} {:+.0}), was {:.3} deg off",
                        snapped.x, snapped.y, snapped.z, angle
                    ));
                }
                fit.axis = snapped;
            }
            let degrees = fit.half_angle.to_degrees();
            let snapped_degrees = (degrees / 0.5).round() * 0.5;
            if (snapped_degrees - degrees).abs() <= policy.angle_tolerance_deg
                && snapped_degrees != degrees
                && snapped_degrees > 0.0
            {
                notes.push(format!(
                    "half angle {degrees:.3} deg snapped to {snapped_degrees:.1} deg"
                ));
                fit.half_angle = snapped_degrees.to_radians();
            }
        }
        SurfaceClass::Blend(fit) => {
            if let Some((snapped, delta)) = policy.snap_length(fit.minor_radius) {
                notes.push(format!(
                    "fillet radius {:.3} snapped to {snapped:.3} ({delta:+.3})",
                    fit.minor_radius
                ));
                fit.minor_radius = snapped;
            }
        }
        SurfaceClass::Pattern(_) | SurfaceClass::EdgeRound(_) | SurfaceClass::Freeform => {}
    }
    notes
}

/// Harmonizes families across surfaces: planes with matching normals and
/// nearly equal offsets become exactly coplanar, and cylinders with nearly
/// identical axes become exactly coaxial. Returns one note per surface.
pub fn harmonize_surfaces(surfaces: &mut [SurfaceClass], policy: &SnapPolicy) -> Vec<Vec<String>> {
    let mut notes = vec![Vec::new(); surfaces.len()];
    let cos_tolerance = policy.angle_tolerance_deg.to_radians().cos();
    // Coplanar groups: same (signed) normal, offsets within tolerance.
    let mut plane_groups: Vec<(Vector3, Vec<usize>, Vec<f64>)> = Vec::new();
    for (index, surface) in surfaces.iter().enumerate() {
        let SurfaceClass::Plane(fit) = surface else {
            continue;
        };
        let offset = (fit.origin - Point3::default()).dot(fit.normal);
        let group = plane_groups.iter_mut().find(|(normal, _, offsets)| {
            normal.dot(fit.normal) >= cos_tolerance
                && offsets
                    .iter()
                    .all(|o| (o - offset).abs() <= policy.length_tolerance)
        });
        match group {
            Some((_, members, offsets)) => {
                members.push(index);
                offsets.push(offset);
            }
            None => plane_groups.push((fit.normal, vec![index], vec![offset])),
        }
    }
    for (_, members, offsets) in &plane_groups {
        if members.len() < 2 {
            continue;
        }
        let mean = offsets.iter().sum::<f64>() / offsets.len() as f64;
        for (&index, &offset) in members.iter().zip(offsets) {
            if let SurfaceClass::Plane(fit) = &mut surfaces[index] {
                fit.origin = fit.origin + fit.normal * (mean - offset);
                notes[index].push(format!(
                    "made coplanar with {} other plane(s) at offset {mean:.3}",
                    members.len() - 1
                ));
            }
        }
    }
    // Coaxial groups: parallel axes within tolerance, radially close.
    let cylinder_indices: Vec<usize> = surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, SurfaceClass::Cylinder(_)))
        .map(|(i, _)| i)
        .collect();
    let mut grouped: Vec<Vec<usize>> = Vec::new();
    for &index in &cylinder_indices {
        let SurfaceClass::Cylinder(fit) = &surfaces[index] else {
            continue;
        };
        let group = grouped.iter_mut().find(|members| {
            members.iter().all(|&other| {
                let SurfaceClass::Cylinder(existing) = &surfaces[other] else {
                    return false;
                };
                let parallel = existing.axis.dot(fit.axis).abs() >= cos_tolerance;
                let separation = (fit.axis_point - existing.axis_point)
                    .cross(existing.axis)
                    .length();
                parallel && separation <= policy.length_tolerance
            })
        });
        match group {
            Some(members) => members.push(index),
            None => grouped.push(vec![index]),
        }
    }
    for members in &grouped {
        if members.len() < 2 {
            continue;
        }
        let mut axis_sum = Vector3::default();
        let mut point_sum = Vector3::default();
        let mut reference: Option<Vector3> = None;
        for &index in members {
            if let SurfaceClass::Cylinder(fit) = &surfaces[index] {
                let axis = match reference {
                    Some(r) if r.dot(fit.axis) < 0.0 => fit.axis * -1.0,
                    _ => fit.axis,
                };
                reference.get_or_insert(axis);
                axis_sum = axis_sum + axis;
                point_sum = point_sum + (fit.axis_point - Point3::default());
            }
        }
        let Some(axis) = normalize(axis_sum) else {
            continue;
        };
        let center = Point3::default() + point_sum / members.len() as f64;
        for &index in members {
            if let SurfaceClass::Cylinder(fit) = &mut surfaces[index] {
                fit.axis = axis;
                // Keep each cylinder's own height station; share the axis line.
                let along = (fit.axis_point - center).dot(axis);
                fit.axis_point = center + axis * along;
                notes[index].push(format!(
                    "made coaxial with {} other cylinder(s)",
                    members.len() - 1
                ));
            }
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{CylinderFit, DeviationStats, PlaneFit};

    fn no_deviation() -> DeviationStats {
        DeviationStats {
            rms: 0.0,
            max_abs: 0.0,
        }
    }

    #[test]
    fn nearly_vertical_axis_snaps_to_z_and_diameter_rounds() {
        let mut surface = SurfaceClass::Cylinder(CylinderFit {
            axis_point: Point3::new(1.0, 2.0, 0.0),
            axis: normalize(Vector3::new(0.01, -0.005, 1.0)).unwrap(),
            radius: 9.991,
            deviation: no_deviation(),
        });
        let notes = snap_surface(&mut surface, &SnapPolicy::default());
        let SurfaceClass::Cylinder(fit) = surface else {
            unreachable!()
        };
        assert!((fit.axis.z - 1.0).abs() < 1e-12);
        assert!((fit.radius - 10.0).abs() < 1e-12);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn distant_direction_does_not_snap() {
        let policy = SnapPolicy::default();
        let off_axis = normalize(Vector3::new(1.0, 1.0, 0.2)).unwrap();
        assert!(policy.snap_direction(off_axis).is_none());
    }

    #[test]
    fn near_coplanar_planes_become_exactly_coplanar() {
        let normal = Vector3::new(0.0, 0.0, 1.0);
        let mut surfaces = vec![
            SurfaceClass::Plane(PlaneFit {
                origin: Point3::new(0.0, 0.0, 10.02),
                normal,
                deviation: no_deviation(),
            }),
            SurfaceClass::Plane(PlaneFit {
                origin: Point3::new(5.0, 5.0, 9.98),
                normal,
                deviation: no_deviation(),
            }),
        ];
        harmonize_surfaces(&mut surfaces, &SnapPolicy::default());
        let offsets: Vec<f64> = surfaces
            .iter()
            .map(|s| {
                let SurfaceClass::Plane(fit) = s else {
                    unreachable!()
                };
                (fit.origin - Point3::default()).dot(fit.normal)
            })
            .collect();
        assert!((offsets[0] - offsets[1]).abs() < 1e-12);
        assert!((offsets[0] - 10.0).abs() < 1e-9);
    }
}

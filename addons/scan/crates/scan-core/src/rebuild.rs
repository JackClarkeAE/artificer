//! Sharp-edge reconstruction: the idealized model.
//!
//! The scan carries rounds on every physical edge; the design does not.
//! This stage rebuilds the part from its recognized features with every
//! revolved surface extended to its exact intersection with its
//! neighbours — fillet and chamfer rings drop out of the geometry and
//! survive only as parametric callouts, the toothing regenerates as the
//! master profile swept helically, and the result is a model whose edges
//! are sharp.

use artificer_geometry::Point3;

use crate::mesh::TriangleMesh;
use crate::reconstruct::extents;
use crate::report::ReverseReport;
use crate::segment::SurfaceClass;

const SEGMENTS: usize = 256;
/// Endpoints extend to an intersection at most this far away (mm).
const SNAP_REACH: f64 = 3.0;

/// One revolved element in profile space: `rho(z) = intercept + slope * z`
/// for walls, a horizontal span for faces.
#[derive(Clone, Copy, Debug)]
enum Element {
    /// rho constant over [z0, z1].
    Wall { feature: usize, rho: f64, z0: f64, z1: f64 },
    /// z constant over [rho0, rho1].
    Face { feature: usize, z: f64, rho0: f64, rho1: f64 },
    /// rho = intercept + slope * z over [z0, z1].
    Taper {
        feature: usize,
        slope: f64,
        intercept: f64,
        z0: f64,
        z1: f64,
    },
}

pub struct RebuiltModel {
    pub mesh: TriangleMesh,
    /// Feature id (in the report) per rebuilt triangle.
    pub feature_of_face: Vec<usize>,
}

/// Rebuilds the sharp idealized model from a finished report. Requires a
/// datum frame; returns `None` without one.
pub fn rebuild_sharp(mesh: &TriangleMesh, report: &ReverseReport) -> Option<RebuiltModel> {
    let alignment = report.datum.as_ref()?;
    // The toothing band: regenerated from the master profile, so revolved
    // elements inside it are already covered.
    let master = report.plan.as_ref().and_then(|p| p.master_profile.as_ref());
    let pattern_band = report.features.iter().find_map(|f| match &f.surface {
        SurfaceClass::Pattern(fit) => Some((fit.z_range, fit.radius_range)),
        _ => None,
    });
    let inside_pattern = |rho: f64, z0: f64, z1: f64| -> bool {
        pattern_band.is_some_and(|((pz0, pz1), (pr0, pr1))| {
            rho >= pr0 - 1.0 && rho <= pr1 + 1.0 && z0 >= pz0 - 1.0 && z1 <= pz1 + 1.0
        })
    };
    let mut elements: Vec<Element> = Vec::new();
    for feature in &report.features {
        match &feature.surface {
            SurfaceClass::Cylinder(fit)
                if fit.axis.z.abs() > 0.999
                    && fit.axis_point.x.hypot(fit.axis_point.y) < 3.0 =>
            {
                let (z0, z1, _, _) = extents(mesh, &feature.faces, alignment);
                if z1 - z0 < 0.3 || inside_pattern(fit.radius, z0, z1) {
                    continue;
                }
                elements.push(Element::Wall {
                    feature: feature.id,
                    rho: fit.radius,
                    z0,
                    z1,
                });
            }
            SurfaceClass::Plane(fit) if fit.normal.z.abs() > 0.999 => {
                let (_, _, r0, r1) = extents(mesh, &feature.faces, alignment);
                if r1 - r0 < 0.3 {
                    continue;
                }
                elements.push(Element::Face {
                    feature: feature.id,
                    z: fit.origin.z,
                    rho0: r0.max(0.0),
                    rho1: r1,
                });
            }
            SurfaceClass::Cone(fit)
                if fit.axis.z.abs() > 0.999 && fit.apex.x.hypot(fit.apex.y) < 3.0 =>
            {
                let (z0, z1, _, _) = extents(mesh, &feature.faces, alignment);
                if z1 - z0 < 0.3 {
                    continue;
                }
                let slope = fit.half_angle.tan() * fit.axis.z.signum();
                let intercept = -slope * fit.apex.z;
                elements.push(Element::Taper {
                    feature: feature.id,
                    slope,
                    intercept,
                    z0,
                    z1,
                });
            }
            // Blends and edge rounds are deliberately not meshed: sharp
            // output carries them as callouts only.
            _ => {}
        }
    }
    // Sharpen: extend every endpoint to the nearest intersection with
    // another element within reach.
    let snapshot = elements.clone();
    let rho_at = |element: &Element, z: f64| -> Option<f64> {
        match element {
            Element::Wall { rho, .. } => Some(*rho),
            Element::Taper {
                slope, intercept, ..
            } => Some(intercept + slope * z),
            Element::Face { .. } => None,
        }
    };
    for element in &mut elements {
        match element {
            Element::Wall { rho, z0, z1, .. } => {
                for (end, own) in [(0usize, *z0), (1usize, *z1)] {
                    let mut best: Option<f64> = None;
                    for other in &snapshot {
                        let candidate = match other {
                            Element::Face { z, rho0, rho1, .. }
                                if *rho >= rho0 - SNAP_REACH && *rho <= rho1 + SNAP_REACH =>
                            {
                                Some(*z)
                            }
                            Element::Taper {
                                slope, intercept, ..
                            } if slope.abs() > 1e-6 => Some((*rho - intercept) / slope),
                            _ => None,
                        };
                        if let Some(z_star) = candidate
                            && (z_star - own).abs() <= SNAP_REACH
                            && best.is_none_or(|b| (z_star - own).abs() < (b - own).abs())
                        {
                            best = Some(z_star);
                        }
                    }
                    if let Some(z_star) = best {
                        if end == 0 {
                            *z0 = z_star;
                        } else {
                            *z1 = z_star;
                        }
                    }
                }
            }
            Element::Face { z, rho0, rho1, .. } => {
                for (end, own) in [(0usize, *rho0), (1usize, *rho1)] {
                    let mut best: Option<f64> = None;
                    for other in &snapshot {
                        if let Some(rho_star) = rho_at(other, *z)
                            && (rho_star - own).abs() <= SNAP_REACH
                            && best.is_none_or(|b| (rho_star - own).abs() < (b - own).abs())
                        {
                            best = Some(rho_star);
                        }
                    }
                    if let Some(rho_star) = best {
                        if end == 0 {
                            *rho0 = rho_star;
                        } else {
                            *rho1 = rho_star;
                        }
                    }
                }
            }
            Element::Taper {
                slope,
                intercept,
                z0,
                z1,
                ..
            } => {
                for (end, own) in [(0usize, *z0), (1usize, *z1)] {
                    let mut best: Option<f64> = None;
                    for other in &snapshot {
                        let candidate = match other {
                            Element::Face { z, .. } => Some(*z),
                            Element::Wall { rho, .. } if slope.abs() > 1e-6 => {
                                Some((*rho - *intercept) / *slope)
                            }
                            _ => None,
                        };
                        if let Some(z_star) = candidate
                            && (z_star - own).abs() <= SNAP_REACH
                            && best.is_none_or(|b| (z_star - own).abs() < (b - own).abs())
                        {
                            best = Some(z_star);
                        }
                    }
                    if let Some(z_star) = best {
                        if end == 0 {
                            *z0 = z_star;
                        } else {
                            *z1 = z_star;
                        }
                    }
                }
            }
        }
    }
    // Emit geometry: revolved elements plus the helically swept toothing.
    let mut positions: Vec<Point3> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    let mut feature_of_face: Vec<usize> = Vec::new();
    let push_soup = |soup: Vec<[Point3; 3]>,
                         feature: usize,
                         positions: &mut Vec<Point3>,
                         triangles: &mut Vec<[u32; 3]>,
                         feature_of_face: &mut Vec<usize>| {
        for triangle in soup {
            let base = positions.len() as u32;
            positions.extend_from_slice(&triangle);
            triangles.push([base, base + 1, base + 2]);
            feature_of_face.push(feature);
        }
    };
    for element in &elements {
        let (profile, feature) = match element {
            Element::Wall {
                feature, rho, z0, z1, ..
            } => (vec![(*rho, *z0), (*rho, *z1)], *feature),
            Element::Face {
                feature,
                z,
                rho0,
                rho1,
                ..
            } => (vec![(*rho0, *z), (*rho1, *z)], *feature),
            Element::Taper {
                feature,
                slope,
                intercept,
                z0,
                z1,
            } => (
                vec![(intercept + slope * z0, *z0), (intercept + slope * z1, *z1)],
                *feature,
            ),
        };
        push_soup(
            crate::synth::revolved_profile_soup(&profile, SEGMENTS),
            feature,
            &mut positions,
            &mut triangles,
            &mut feature_of_face,
        );
    }
    if let (Some(master), Some(((pz0, pz1), _))) = (master, pattern_band) {
        let pattern_id = report
            .features
            .iter()
            .find(|f| matches!(f.surface, SurfaceClass::Pattern(_)))
            .map_or(0, |f| f.id);
        // Extend the tooth band to the nearest level faces for sharp ends.
        let mut z0 = pz0;
        let mut z1 = pz1;
        for element in &snapshot {
            if let Element::Face { z, .. } = element {
                if (z - pz0).abs() <= SNAP_REACH {
                    z0 = z0.min(*z);
                }
                if (z - pz1).abs() <= SNAP_REACH {
                    z1 = z1.max(*z);
                }
            }
        }
        push_soup(
            helical_pattern_soup(&master.points, master.count, master.helix_rate, z0, z1, 48),
            pattern_id,
            &mut positions,
            &mut triangles,
            &mut feature_of_face,
        );
    }
    let mesh = TriangleMesh::new(positions, triangles)?;
    Some(RebuiltModel {
        mesh,
        feature_of_face,
    })
}

/// Sweeps the master sector profile helically about +Z, repeated `count`
/// times per revolution, from `z0` to `z1`.
fn helical_pattern_soup(
    profile: &[(f64, f64)],
    count: usize,
    helix_rate: f64,
    z0: f64,
    z1: f64,
    z_steps: usize,
) -> Vec<[Point3; 3]> {
    let mut soup = Vec::new();
    if profile.len() < 2 {
        return soup;
    }
    let sector = std::f64::consts::TAU / count as f64;
    let ring: Vec<(f64, f64)> = (0..count)
        .flat_map(|k| {
            profile
                .iter()
                .map(move |&(theta, rho)| (theta + k as f64 * sector, rho))
        })
        .collect();
    let ring_len = ring.len();
    let point = |slot: usize, step: usize| -> Point3 {
        let z = z0 + (z1 - z0) * step as f64 / z_steps as f64;
        let (theta, rho) = ring[slot % ring_len];
        let angle = theta + helix_rate * (z - z0);
        Point3::new(rho * angle.cos(), rho * angle.sin(), z)
    };
    for slot in 0..ring_len {
        for step in 0..z_steps {
            let a = point(slot, step);
            let b = point(slot + 1, step);
            let c = point(slot + 1, step + 1);
            let d = point(slot, step + 1);
            soup.push([a, b, c]);
            soup.push([a, c, d]);
        }
    }
    soup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReverseOptions, reverse_engineer};
    use crate::synth;
    use artificer_geometry::Vector3;

    #[test]
    fn turned_part_rebuilds_with_sharp_corner() {
        // Wall to z 8.5 with a fillet rolling to a top face at z 10: the
        // rebuild must extend the wall to exactly z = 10 (sharp corner)
        // and drop the fillet from the geometry.
        let mut soup = synth::open_cylinder_soup(20.0, 8.5, 128, 6);
        soup.extend(synth::revolved_blend_soup(
            18.5,
            1.5,
            8.5,
            0.0,
            std::f64::consts::FRAC_PI_2,
            128,
            8,
        ));
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 10.0),
            Vector3::new(0.0, 0.0, 1.0),
            18.5,
            128,
        ));
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, -1.0),
            20.0,
            128,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let mut options = ReverseOptions::default();
        if let Some(ransac) = &mut options.ransac {
            ransac.min_support_faces = 60;
        }
        let report = reverse_engineer(&mesh, &options);
        let rebuilt = rebuild_sharp(&mesh, &report).expect("rebuild");
        assert!(!rebuilt.mesh.triangles().is_empty());
        assert_eq!(
            rebuilt.feature_of_face.len(),
            rebuilt.mesh.triangles().len()
        );
        let bounds = rebuilt.mesh.bounds().unwrap();
        // The datum frame's sign is arbitrary, so assert on spans: sharp
        // corner means the wall reaches the far face exactly (height 10,
        // not the scanned 8.5) and the radial extent is the wall radius.
        let height = bounds.max.z - bounds.min.z;
        assert!((height - 10.0).abs() < 0.2, "height {height}");
        assert!((bounds.max.x - 20.0).abs() < 0.3, "radius {}", bounds.max.x);
    }
}

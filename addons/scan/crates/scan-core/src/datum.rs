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
/// Axis lines laterally closer than this (mm) are one line, and so
/// vote together for the datum's lateral origin.
const AXIS_LINE_TOL: f64 = 2.0;
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
        SurfaceClass::Pattern(fit) => Some(fit.axis),
        // A moulded torus curves about wherever the tool left it, not
        // about the part's axis, so it must not vote on the datum.
        SurfaceClass::Sphere(_)
        | SurfaceClass::Torus(_)
        | SurfaceClass::EdgeRound(_)
        | SurfaceClass::Freeform => None,
    }
}

/// Derives a datum frame from classified features. Returns `None` when no
/// analytic feature offers a usable direction.
/// A direction the part could be datumed on, with the area backing it.
///
/// The automatic choice takes the heaviest, which is right on most parts
/// and wrong on some: a part with a large face and a functionally
/// important small bore is datumed by a machinist on the bore. The
/// commercial packages settle this by asking, so the candidates are
/// published rather than silently collapsed to one.
#[derive(Clone, Copy, Debug)]
pub struct DatumCandidate {
    pub direction: Vector3,
    /// Feature area supporting this direction (mm²).
    pub weight: f64,
}

/// The datum directions a part offers, heaviest first.
pub fn datum_candidates(features: &[FeatureRecord]) -> Vec<DatumCandidate> {
    cluster_directions(
        features
            .iter()
            .filter_map(|f| feature_direction(&f.surface).map(|direction| (direction, f.area))),
    )
    .into_iter()
    .map(|cluster| DatumCandidate {
        direction: cluster.representative,
        weight: cluster.weight,
    })
    .collect()
}

pub fn auto_datum_alignment(features: &[FeatureRecord]) -> Option<DatumAlignment> {
    datum_alignment_on(features, 0)
}

/// Builds the datum frame on the `choice`-th ranked candidate direction.
///
/// Choosing a different primary is not a cosmetic re-labelling: every
/// stage downstream — the revolved profile, band extraction, patterns —
/// asks whether a surface is "about the datum axis", so this is the one
/// decision that changes what the pipeline is able to recognize at all.
/// The axis line the most material agrees on, in whatever frame the
/// features are expressed in: its point (axial component dropped), the
/// area backing it, and how many features voted.
///
/// "Axis parallel to `z`" is not "is the part's own axis" — every
/// hole, boss and chamfer on a plate is parallel to it too. Taking the
/// single largest such feature therefore puts the frame wherever the
/// biggest patch happens to lie. What distinguishes the part's own
/// axis is that the most material is coaxial with it, so fragments
/// vote together as a line and the heaviest line wins. Same idiom as
/// the datum *direction*, which has always been an area-weighted vote
/// rather than a single winner.
pub fn dominant_axis_line(features: &[FeatureRecord], z: Vector3) -> Option<(Point3, f64, usize)> {
    let cos_near = CLUSTER_ANGLE_DEG.to_radians().cos();
    struct AxisLine {
        /// Where the line is: taken from the single best-conditioned
        /// member, never averaged. A large cylinder pins an axis to a
        /// fraction of the noise; a small cone's apex wanders, and a
        /// shallow one's sits hundreds of millimetres from its own
        /// material. Averaging them lets the weak locators drag the
        /// strong one — on a turned synthetic that moved the origin
        /// 0.02 mm and read a 1.5 mm fillet as 1.44.
        point: Point3,
        /// The area of the member that supplied `point`.
        best: f64,
        /// Total area agreeing with the line: this is what decides
        /// which line wins.
        area: f64,
        members: usize,
    }
    let lateral_gap = |a: Point3, b: Point3| {
        let offset = a - b;
        (offset - z * offset.dot(z)).length()
    };
    let mut lines: Vec<AxisLine> = Vec::new();
    for feature in features {
        let candidate = match &feature.surface {
            SurfaceClass::Cylinder(fit) if fit.axis.dot(z).abs() >= cos_near => {
                Some(fit.axis_point)
            }
            SurfaceClass::Cone(fit) if fit.axis.dot(z).abs() >= cos_near => Some(fit.apex),
            SurfaceClass::Sphere(fit) => Some(fit.center),
            _ => None,
        };
        let Some(point) = candidate else { continue };
        match lines
            .iter_mut()
            .find(|line| lateral_gap(line.point, point) <= AXIS_LINE_TOL)
        {
            Some(line) => {
                if feature.area > line.best {
                    line.point = point;
                    line.best = feature.area;
                }
                line.area += feature.area;
                line.members += 1;
            }
            None => lines.push(AxisLine {
                point,
                best: feature.area,
                area: feature.area,
                members: 1,
            }),
        }
    }
    // Heaviest line wins; ties break on position so the frame never
    // depends on iteration order.
    lines.sort_by(|a, b| {
        b.area
            .total_cmp(&a.area)
            .then(a.point.x.total_cmp(&b.point.x))
            .then(a.point.y.total_cmp(&b.point.y))
    });
    lines.first().map(|line| {
        // Only the lateral part means anything — the height comes from
        // the level plane — so drop the axial part rather than letting
        // whichever feature voted decide the last bits of it.
        (
            line.point - z * (line.point - Point3::default()).dot(z),
            line.area,
            line.members,
        )
    })
}

pub fn datum_alignment_on(features: &[FeatureRecord], choice: usize) -> Option<DatumAlignment> {
    let clusters = cluster_directions(
        features
            .iter()
            .filter_map(|f| feature_direction(&f.surface).map(|direction| (direction, f.area))),
    );
    if choice >= clusters.len() {
        return None;
    }
    let primary = &clusters[choice];
    let z = primary.representative;
    let mut notes = vec![format!(
        "primary datum Z from {:.0} mm^2 of aligned features, direction ({:+.3} {:+.3} {:+.3}){}",
        primary.weight,
        z.x,
        z.y,
        z.z,
        if choice == 0 {
            String::new()
        } else {
            format!(" (candidate {choice}, chosen over the heaviest)")
        }
    )];
    // Secondary: heaviest cluster roughly perpendicular to Z.
    let max_dot = PERPENDICULAR_SLACK_DEG.to_radians().sin();
    let x_hint = match clusters
        .iter()
        .enumerate()
        .filter(|&(index, _)| index != choice)
        .map(|(_, cluster)| cluster)
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
    // Lateral origin: the axis *line* the most material agrees on.
    //
    // "Axis parallel to Z" is not "is the part's own axis" — every
    // hole, boss and chamfer on a plate is Z-parallel too. Taking the
    // single largest such feature therefore puts the frame wherever
    // the biggest patch happens to lie, and the datum is chosen when
    // the feature set is at its most fragmented: the outer wall is
    // still a dozen unstitched pieces while one chamfer cone is whole.
    // On a noisy wheel spacer that landed the origin on a lug hole,
    // 40 mm off the part's axis, and every revolved stage afterwards —
    // band stitching, axis locking, pattern detection — reasoned about
    // the wrong centre while reporting healthy residuals.
    //
    // What distinguishes the part's own axis is not any one feature's
    // size but that the most material is coaxial with it, so fragments
    // vote together as a line and the heaviest line wins. Same idiom
    // as the datum *direction*, which has always been an area-weighted
    // vote rather than a single winner.
    let dominant = dominant_axis_line(features, z);
    let (lateral_point, lateral_label) = match dominant {
        Some((point, area, members)) => (
            point,
            format!("axis line backed by {area:.0} mm^2 of {members} coaxial feature(s)"),
        ),
        None => {
            let fallback = features
                .iter()
                .find_map(|f| match &f.surface {
                    SurfaceClass::Plane(fit) => Some(fit.origin),
                    _ => None,
                })
                .unwrap_or_default();
            (fallback, "largest plane centroid".to_owned())
        }
    };
    // Height origin: the largest plane perpendicular to Z sets Z = 0.
    let level_plane = features
        .iter()
        .filter_map(|f| match &f.surface {
            SurfaceClass::Plane(fit) if fit.normal.dot(z).abs() >= cos_near => Some((f.area, fit)),
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
            notes.push(format!(
                "origin: {lateral_label} (no perpendicular plane found)"
            ));
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

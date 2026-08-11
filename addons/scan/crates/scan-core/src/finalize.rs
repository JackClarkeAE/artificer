//! Final decomposition: every face owned by exactly one feature.
//!
//! After recognition, three kinds of geometry remain unowned: faces that
//! lie on an already-recognized surface but arrived in a freeform region
//! (claimed face-by-face), transition bands along the shared edges of two
//! features — the physical rounds and chamfer breaks, which *are*
//! features (claimed per feature pair as [`SurfaceClass::EdgeRound`]) —
//! and genuine leftovers, which collapse into one honest residue record
//! instead of tens of thousands of sliver entries. The result reads like
//! a part specification: a bounded list of features that tile the mesh.

use artificer_geometry::{Point3, Vector3};

use crate::datum::DatumAlignment;
use crate::fit::{DeviationStats, EdgeRoundFit};
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::SurfaceClass;
use crate::transform::RigidTransform;

/// Faces this close to a recognized surface (times tolerance) join it.
const CLAIM_BAND_FACTOR: f64 = 1.25;
/// Face normals within this angle of the surface normal may join.
const CLAIM_NORMAL_DEG: f64 = 30.0;
/// A face within this distance (mm) of two features belongs to the round
/// along their shared edge.
const ROUND_REACH: f64 = 2.5;
/// An edge-round group must reach this area (mm^2) to become a feature.
const ROUND_MIN_AREA: f64 = 8.0;
const ROUND_MIN_FACES: usize = 30;

/// A short human label for the note on an edge round.
fn short_label(surface: &SurfaceClass) -> String {
    match surface {
        SurfaceClass::Plane(fit) => format!("plane z {:+.1}", fit.origin.z),
        SurfaceClass::Cylinder(fit) => format!("cylinder d {:.1}", fit.radius * 2.0),
        SurfaceClass::Sphere(fit) => format!("sphere d {:.1}", fit.radius * 2.0),
        SurfaceClass::Cone(fit) => {
            format!("cone {:.1} deg", fit.half_angle.to_degrees())
        }
        SurfaceClass::Blend(fit) => format!("fillet r {:.1}", fit.minor_radius),
        SurfaceClass::Pattern(fit) => format!("pattern x {}", fit.count),
        SurfaceClass::EdgeRound(_) => "edge round".to_owned(),
        SurfaceClass::Freeform => "freeform".to_owned(),
    }
}

/// Accumulator for one edge-round group: faces, area, span sum,
/// squared-deviation sum, and max deviation.
type RoundGroup = (Vec<u32>, f64, f64, f64, f64);

pub struct FinalizeStats {
    pub claimed_faces: usize,
    pub edge_rounds: usize,
    pub residue_area: f64,
}

/// Completes the face-to-feature decomposition. Runs in the datum frame
/// when one exists, identity otherwise.
pub fn finalize_features(
    mesh: &TriangleMesh,
    features: &mut Vec<FeatureRecord>,
    alignment: Option<&DatumAlignment>,
    tolerance: f64,
) -> FinalizeStats {
    let identity = RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let mut stats = FinalizeStats {
        claimed_faces: 0,
        edge_rounds: 0,
        residue_area: 0.0,
    };
    // Implausible fits become residue input: a sphere whose centre sits far
    // outside the part explains nothing.
    let bounds_diagonal = mesh.bounds_diagonal().max(1.0);
    for feature in features.iter_mut() {
        if let SurfaceClass::Sphere(fit) = &feature.surface {
            let centre_distance = (fit.center - Point3::default()).length();
            if centre_distance > bounds_diagonal {
                feature.surface = SurfaceClass::Freeform;
            }
        }
    }
    let solids: Vec<usize> = (0..features.len())
        .filter(|&i| !matches!(features[i].surface, SurfaceClass::Freeform))
        .collect();
    // Unowned faces, with datum-frame geometry.
    struct Loose {
        face: u32,
        centroid: Point3,
        normal: Option<Vector3>,
        area: f64,
    }
    let mut loose: Vec<Loose> = Vec::new();
    for feature in features.iter() {
        if !matches!(feature.surface, SurfaceClass::Freeform) {
            continue;
        }
        for &face in &feature.faces {
            loose.push(Loose {
                face,
                centroid: to_frame.apply_point(mesh.face_centroid(face as usize)),
                normal: mesh
                    .face_normal(face as usize)
                    .map(|n| to_frame.apply_vector(n)),
                area: mesh.face_area(face as usize),
            });
        }
    }
    let min_alignment = CLAIM_NORMAL_DEG.to_radians().cos();
    let claim_band = CLAIM_BAND_FACTOR * tolerance;
    let mut additions: Vec<Vec<u32>> = vec![Vec::new(); features.len()];
    let mut claimed_counts: Vec<usize> = vec![0; features.len()];
    // Per-pair edge-round groups, and the residue.
    let mut rounds: std::collections::HashMap<(usize, usize), RoundGroup> =
        std::collections::HashMap::new();
    let mut residue_faces: Vec<u32> = Vec::new();
    for item in &loose {
        // Distances to every recognized surface.
        let mut nearest: Option<(usize, f64, f64)> = None; // (feature, |d|, align)
        let mut second: Option<(usize, f64)> = None;
        for &index in &solids {
            let Some((distance, surface_normal)) = features[index].surface.probe(item.centroid)
            else {
                continue;
            };
            let magnitude = distance.abs();
            let align = item
                .normal
                .map_or(1.0, |n| n.dot(surface_normal).abs());
            if nearest.is_none_or(|(_, best, _)| magnitude < best) {
                second = nearest.map(|(i, d, _)| (i, d));
                nearest = Some((index, magnitude, align));
            } else if second.is_none_or(|(_, best)| magnitude < best) {
                second = Some((index, magnitude));
            }
        }
        match nearest {
            // On-surface: join the feature it lies on.
            Some((index, distance, align)) if distance <= claim_band && align >= min_alignment => {
                additions[index].push(item.face);
                claimed_counts[index] += 1;
                stats.claimed_faces += 1;
            }
            // Between two surfaces: the round along their shared edge.
            Some((first, d1, _)) if d1 <= ROUND_REACH => match second {
                Some((other, d2)) if d2 <= ROUND_REACH => {
                    let key = (first.min(other), first.max(other));
                    let entry = rounds.entry(key).or_insert((Vec::new(), 0.0, 0.0, 0.0, 0.0));
                    entry.0.push(item.face);
                    entry.1 += item.area;
                    entry.2 += item.area * (d1 + d2);
                    entry.3 += item.area * d1 * d1;
                    entry.4 = entry.4.max(d1);
                    let _ = &entry;
                }
                _ => residue_faces.push(item.face),
            },
            _ => residue_faces.push(item.face),
        }
    }
    // Apply claims.
    for (index, faces) in additions.into_iter().enumerate() {
        if faces.is_empty() {
            continue;
        }
        let feature = &mut features[index];
        feature.faces.extend(faces);
        feature.face_count = feature.faces.len();
        feature.area = feature
            .faces
            .iter()
            .map(|&face| mesh.face_area(face as usize))
            .sum();
        feature.notes.push(format!(
            "claimed {} on-surface face(s) in the final pass",
            claimed_counts[index]
        ));
    }
    // Emit edge rounds; too-small groups fall into the residue.
    let mut round_records: Vec<FeatureRecord> = Vec::new();
    let mut ordered_rounds: Vec<((usize, usize), RoundGroup)> = rounds.into_iter().collect();
    ordered_rounds.sort_by_key(|(key, _)| *key);
    for ((a, b), (faces, area, span_sum, dev_sum, max_d)) in ordered_rounds {
        if area < ROUND_MIN_AREA || faces.len() < ROUND_MIN_FACES {
            residue_faces.extend(faces);
            continue;
        }
        let span = span_sum / area;
        let rms = (dev_sum / area).sqrt();
        round_records.push(FeatureRecord {
            id: 0,
            surface: SurfaceClass::EdgeRound(EdgeRoundFit {
                span,
                deviation: DeviationStats {
                    rms,
                    max_abs: max_d,
                },
            }),
            face_count: faces.len(),
            area,
            faces,
            notes: vec![format!(
                "round along the edge between {} and {}",
                short_label(&features[a].surface),
                short_label(&features[b].surface)
            )],
        });
        stats.edge_rounds += 1;
    }
    // Drop the emptied freeform features and collapse the residue.
    features.retain(|feature| !matches!(feature.surface, SurfaceClass::Freeform));
    features.extend(round_records);
    if !residue_faces.is_empty() {
        stats.residue_area = residue_faces
            .iter()
            .map(|&face| mesh.face_area(face as usize))
            .sum();
        features.push(FeatureRecord {
            id: 0,
            surface: SurfaceClass::Freeform,
            face_count: residue_faces.len(),
            area: stats.residue_area,
            faces: residue_faces,
            notes: vec!["unexplained residue".to_owned()],
        });
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReverseOptions, reverse_engineer};
    use crate::synth;

    #[test]
    fn wavy_round_between_two_planes_becomes_an_edge_round() {
        // Two perpendicular plates joined by a wavy (non-analytic) round
        // strip: the strip must come out as one edge-round feature, and
        // every face of the mesh must end up owned by exactly one feature.
        let x = Vector3::new(1.0, 0.0, 0.0);
        let y = Vector3::new(0.0, 1.0, 0.0);
        let z = Vector3::new(0.0, 0.0, 1.0);
        let mut soup = synth::plane_patch_soup(Point3::new(-24.0, 0.0, 0.0), x, y, 22.0, 30.0, 8, 10);
        soup.extend(synth::plane_patch_soup(
            Point3::new(2.0, 0.0, 2.0),
            z,
            y,
            22.0,
            30.0,
            8,
            10,
        ));
        // Round strip r = 2 along y between the plates, with a wave that
        // keeps every fit above tolerance.
        for j in 0..30usize {
            for k in 0..8usize {
                let corner = |dj: usize, dk: usize| {
                    let angle =
                        std::f64::consts::FRAC_PI_2 * (k + dk) as f64 / 8.0;
                    let wave = 0.25 * (((j + dj) as f64) * 1.3).sin();
                    let radius = 2.0 + wave;
                    Point3::new(
                        -(radius * angle.cos()) + 2.0,
                        (j + dj) as f64,
                        -(radius * angle.sin()) + 2.0,
                    )
                };
                let (a, b, c, d) = (corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1));
                soup.push([a, b, c]);
                soup.push([a, c, d]);
            }
        }
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let mut options = ReverseOptions::default();
        options.auto_datum = false;
        if let Some(ransac) = &mut options.ransac {
            ransac.min_support_faces = 60;
        }
        let report = reverse_engineer(&mesh, &options);
        let rounds: Vec<_> = report
            .features
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::EdgeRound(_)))
            .collect();
        assert_eq!(rounds.len(), 1, "edge round missing");
        assert!(rounds[0].notes.iter().any(|n| n.contains("between")));
        // Total ownership: every face accounted for exactly once.
        let owned: usize = report.features.iter().map(|f| f.face_count).sum();
        assert_eq!(owned, mesh.triangles().len());
        let total: f64 = report.features.iter().map(|f| f.area).sum();
        assert!((total - mesh.surface_area()).abs() < 1e-6);
    }
}

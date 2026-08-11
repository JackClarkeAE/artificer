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

/// A short human label for notes that reference another feature.
pub(crate) fn feature_label(surface: &SurfaceClass) -> String {
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

/// An unowned face with datum-frame geometry.
struct LooseFace {
    face: u32,
    centroid: Point3,
    normal: Option<Vector3>,
    area: f64,
}

/// Whether a component actually sits near a surface (mean probe distance
/// within round reach). Unprobeable surfaces (the pattern) pass — the
/// topology is the only evidence available there, and it suffices.
fn component_near(
    surface: &SurfaceClass,
    member_loose: &[usize],
    loose: &[LooseFace],
) -> bool {
    let mut total = 0.0;
    let mut count = 0usize;
    for &loose_index in member_loose {
        if let Some((distance, _)) = surface.probe(loose[loose_index].centroid) {
            total += distance.abs();
            count += 1;
        }
    }
    count == 0 || total / count as f64 <= ROUND_REACH
}

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
    let mut loose: Vec<LooseFace> = Vec::new();
    for feature in features.iter() {
        if !matches!(feature.surface, SurfaceClass::Freeform) {
            continue;
        }
        for &face in &feature.faces {
            loose.push(LooseFace {
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
    let mut rounds: std::collections::HashMap<(usize, usize), RoundGroup> =
        std::collections::HashMap::new();
    let mut residue_faces: Vec<u32> = Vec::new();
    // Pass 1: a face lying on a recognized surface joins it.
    let mut unresolved: Vec<usize> = Vec::new();
    for (loose_index, item) in loose.iter().enumerate() {
        let mut nearest: Option<(usize, f64, f64)> = None;
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
                nearest = Some((index, magnitude, align));
            }
        }
        match nearest {
            Some((index, distance, align)) if distance <= claim_band && align >= min_alignment => {
                additions[index].push(item.face);
                claimed_counts[index] += 1;
                stats.claimed_faces += 1;
            }
            _ => unresolved.push(loose_index),
        }
    }
    // Pass 2: what remains resolves topologically. Connected components of
    // the unresolved faces read their identity off the mesh adjacency:
    // bordered mostly by one feature they are its pocket (tooth-end
    // chamfers join the pattern this way — no probe needed); bordered by
    // two, they are the round along that edge; floating, residue.
    {
        let adjacency = mesh.face_adjacency();
        let mut owner = vec![u32::MAX; mesh.triangles().len()];
        for &index in &solids {
            for &face in &features[index].faces {
                owner[face as usize] = index as u32;
            }
        }
        for (index, faces) in additions.iter().enumerate() {
            for &face in faces {
                owner[face as usize] = index as u32;
            }
        }
        let mut component_of = vec![usize::MAX; mesh.triangles().len()];
        let unresolved_set: std::collections::HashMap<u32, usize> = unresolved
            .iter()
            .map(|&loose_index| (loose[loose_index].face, loose_index))
            .collect();
        let mut components: Vec<Vec<usize>> = Vec::new();
        for &loose_index in &unresolved {
            let seed = loose[loose_index].face;
            if component_of[seed as usize] != usize::MAX {
                continue;
            }
            let component_index = components.len();
            let mut member_faces = vec![loose_index];
            component_of[seed as usize] = component_index;
            let mut queue = vec![seed];
            while let Some(face) = queue.pop() {
                for &neighbor in &adjacency[face as usize] {
                    if component_of[neighbor as usize] != usize::MAX {
                        continue;
                    }
                    if let Some(&other_loose) = unresolved_set.get(&neighbor) {
                        component_of[neighbor as usize] = component_index;
                        member_faces.push(other_loose);
                        queue.push(neighbor);
                    }
                }
            }
            components.push(member_faces);
        }
        for member_loose in components {
            // Tally which features border this component.
            let mut border: std::collections::HashMap<u32, usize> =
                std::collections::HashMap::new();
            for &loose_index in &member_loose {
                let face = loose[loose_index].face;
                for &neighbor in &adjacency[face as usize] {
                    let neighbor_owner = owner[neighbor as usize];
                    if neighbor_owner != u32::MAX {
                        *border.entry(neighbor_owner).or_default() += 1;
                    }
                }
            }
            let mut ranked: Vec<(u32, usize)> = border.into_iter().collect();
            ranked.sort_by_key(|&(index, count)| (std::cmp::Reverse(count), index));
            let total_border: usize = ranked.iter().map(|(_, c)| c).sum();
            if ranked.is_empty() {
                // A floating island (disconnected strip): fall back to the
                // probe-based two-nearest test per face.
                for &loose_index in &member_loose {
                    let item = &loose[loose_index];
                    let mut nearest: Option<(usize, f64)> = None;
                    let mut second: Option<(usize, f64)> = None;
                    for &index in &solids {
                        let Some((distance, _)) = features[index].surface.probe(item.centroid)
                        else {
                            continue;
                        };
                        let magnitude = distance.abs();
                        if nearest.is_none_or(|(_, best)| magnitude < best) {
                            second = nearest;
                            nearest = Some((index, magnitude));
                        } else if second.is_none_or(|(_, best)| magnitude < best) {
                            second = Some((index, magnitude));
                        }
                    }
                    match (nearest, second) {
                        (Some((a, d1)), Some((b, d2)))
                            if d1 <= ROUND_REACH && d2 <= ROUND_REACH =>
                        {
                            let key = (a.min(b), a.max(b));
                            let entry =
                                rounds.entry(key).or_insert((Vec::new(), 0.0, 0.0, 0.0, 0.0));
                            entry.0.push(item.face);
                            entry.1 += item.area;
                            entry.2 += item.area * (d1 + d2);
                            entry.3 += item.area * d1 * d1;
                            entry.4 = entry.4.max(d1);
                        }
                        _ => residue_faces.push(item.face),
                    }
                }
            } else if (ranked.len() == 1 || ranked[0].1 as f64 >= 0.9 * total_border as f64)
                && component_near(
                    &features[ranked[0].0 as usize].surface,
                    &member_loose,
                    &loose,
                )
            {
                // A pocket inside one feature: it belongs to that feature
                // by topology, in or out of tolerance.
                let index = ranked[0].0 as usize;
                for &loose_index in &member_loose {
                    additions[index].push(loose[loose_index].face);
                }
                claimed_counts[index] += member_loose.len();
                stats.claimed_faces += member_loose.len();
            } else {
                let (first, second) = (ranked[0].0 as usize, ranked[1].0 as usize);
                let key = (first.min(second), first.max(second));
                let entry = rounds.entry(key).or_insert((Vec::new(), 0.0, 0.0, 0.0, 0.0));
                for &loose_index in &member_loose {
                    let item = &loose[loose_index];
                    let d1 = features[key.0]
                        .surface
                        .probe(item.centroid)
                        .map_or(0.0, |(d, _)| d.abs());
                    let d2 = features[key.1]
                        .surface
                        .probe(item.centroid)
                        .map_or(0.0, |(d, _)| d.abs());
                    entry.0.push(item.face);
                    entry.1 += item.area;
                    entry.2 += item.area * (d1 + d2);
                    entry.3 += item.area * d1 * d1;
                    entry.4 = entry.4.max(d1);
                }
            }
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
                feature_label(&features[a].surface),
                feature_label(&features[b].surface)
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

/// Reclassifies circumferential edge rounds into parametric blend
/// features: in profile space `(radius, z)` about the datum axis a
/// revolved fillet is an arc and a revolved chamfer is a line, chosen by
/// description length. Non-revolved rounds (a tooth edge) stay rounds.
pub fn refine_rounds(
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    alignment: Option<&DatumAlignment>,
    tolerance: f64,
) -> (usize, usize) {
    let identity = RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let mut fillets = 0usize;
    let mut chamfers = 0usize;
    for feature in features.iter_mut() {
        if !matches!(feature.surface, SurfaceClass::EdgeRound(_)) || feature.face_count < 24 {
            continue;
        }
        let mut samples: Vec<(f64, f64, f64)> = Vec::new();
        let mut bins = [false; 24];
        for &face in &feature.faces {
            let c = to_frame.apply_point(mesh.face_centroid(face as usize));
            let radial = c.x.hypot(c.y);
            if radial < 1.0 {
                continue;
            }
            let angle = c.y.atan2(c.x);
            let bin = ((angle + std::f64::consts::PI) / std::f64::consts::TAU * 24.0) as usize;
            bins[bin.min(23)] = true;
            samples.push((radial, c.z, mesh.face_area(face as usize)));
        }
        if bins.iter().filter(|b| **b).count() < 8 || samples.len() < 24 {
            continue;
        }
        // Weighted line fit rho = a + s z.
        let (mut sw, mut sz, mut sr, mut szz, mut szr) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(radial, z, area) in &samples {
            sw += area;
            sz += area * z;
            sr += area * radial;
            szz += area * z * z;
            szr += area * z * radial;
        }
        let denom = sw * szz - sz * sz;
        let line = if denom.abs() > 1e-9 {
            let slope = (sw * szr - sz * sr) / denom;
            let intercept = (sr - slope * sz) / sw;
            let rms = (samples
                .iter()
                .map(|&(radial, z, area)| {
                    let e = radial - (intercept + slope * z);
                    area * e * e
                })
                .sum::<f64>()
                / sw)
                .sqrt();
            Some((slope, intercept, rms))
        } else {
            None
        };
        // Arc fit in profile space.
        let profile: Vec<(f64, f64)> = samples.iter().map(|&(radial, z, _)| (radial, z)).collect();
        let arc = crate::fit::fit_circle_2d(&profile).map(|(rho_c, z_c, radius)| {
            let rms = (samples
                .iter()
                .map(|&(radial, z, area)| {
                    let e = (radial - rho_c).hypot(z - z_c) - radius;
                    area * e * e
                })
                .sum::<f64>()
                / sw)
                .sqrt();
            (rho_c, z_c, radius, rms)
        });
        // Description-length choice, tolerance-capped.
        let n = samples.len() as f64;
        let cost = |rms: f64, params: f64| 2.0 * n * rms.max(1e-3).ln() + params * n.ln();
        let line_ok = line
            .filter(|(slope, _, rms)| *rms <= 1.5 * tolerance && (0.08..=8.0).contains(&slope.abs()));
        let arc_ok = arc.filter(|(_, _, radius, rms)| {
            *rms <= 1.5 * tolerance && *radius >= tolerance && *radius <= 8.0
        });
        let line_cost = line_ok.map(|(_, _, rms)| cost(rms, 2.0));
        let arc_cost = arc_ok.map(|(_, _, _, rms)| cost(rms, 3.0));
        match (line_ok, arc_ok) {
            (Some((slope, intercept, rms)), arc_choice)
                if arc_choice.is_none() || line_cost <= arc_cost =>
            {
                let apex_z = -intercept / slope;
                let axis = if slope > 0.0 {
                    Vector3::new(0.0, 0.0, 1.0)
                } else {
                    Vector3::new(0.0, 0.0, -1.0)
                };
                let half_angle = slope.abs().atan();
                feature.surface = SurfaceClass::Cone(crate::fit::ConeFit {
                    apex: Point3::new(0.0, 0.0, apex_z),
                    axis,
                    half_angle,
                    deviation: DeviationStats { rms, max_abs: rms },
                });
                feature.notes.push(format!(
                    "circular chamfer ring, {:.1} deg",
                    half_angle.to_degrees()
                ));
                chamfers += 1;
            }
            (_, Some((rho_c, z_c, radius, rms))) => {
                feature.surface = SurfaceClass::Blend(crate::fit::RevolvedBlendFit {
                    axis_point: Point3::new(0.0, 0.0, z_c),
                    axis: Vector3::new(0.0, 0.0, 1.0),
                    major_radius: rho_c,
                    minor_radius: radius,
                    deviation: DeviationStats { rms, max_abs: rms },
                });
                feature.notes.push(format!("circular fillet, r {radius:.2}"));
                fillets += 1;
            }
            _ => {}
        }
    }
    (fillets, chamfers)
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
        // Floor to x = -2, wall from z = 2, joined by a wavy round of
        // nominal radius 2 centred at (-2, 2), tangent at both seams so
        // the strip welds to the plates. The wave keeps every fit above
        // tolerance but vanishes at the seams.
        let mut soup =
            synth::plane_patch_soup(Point3::new(-26.0, 0.0, 0.0), x, y, 24.0, 30.0, 8, 30);
        soup.extend(synth::plane_patch_soup(
            Point3::new(0.0, 0.0, 2.0),
            z,
            y,
            24.0,
            30.0,
            8,
            30,
        ));
        for j in 0..30usize {
            for k in 0..8usize {
                let corner = |dj: usize, dk: usize| {
                    let t = std::f64::consts::FRAC_PI_2 * (k + dk) as f64 / 8.0;
                    let wave = 0.25 * (((j + dj) as f64) * 1.3).sin() * (2.0 * t).sin();
                    let radius = 2.0 + wave;
                    Point3::new(
                        -2.0 + radius * t.sin(),
                        (j + dj) as f64,
                        2.0 - radius * t.cos(),
                    )
                };
                let (a, b, c, d) = (corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1));
                soup.push([a, b, c]);
                soup.push([a, c, d]);
            }
        }
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let mut options = ReverseOptions {
            auto_datum: false,
            ..ReverseOptions::default()
        };
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

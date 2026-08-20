//! Advanced feature consolidation: statistics, topology, shared intent.
//!
//! Three rungs above geometric stitching:
//!
//! 1. **Model selection** — merges are decided by description length
//!    (BIC), not thresholds: two features collapse into one exactly when
//!    the union's residual growth costs less than carrying a second
//!    parameter set. The tolerance stays as a hard safety cap.
//! 2. **Adjacency-graph consolidation** — candidates come from the
//!    feature adjacency graph built off mesh edges, including pairs that
//!    touch only through an edge-round band. When such a pair merges,
//!    the band was never a feature — just the seam of an over-split
//!    surface — so its faces fold into the merged surface and the round
//!    dissolves.
//! 3. **Shared parameters** — a joint least-squares solve turns repeated
//!    values into shared entities: one axis referenced by every coaxial
//!    feature, one direction for every level plane, one radius per
//!    equal-radius group. The parameter list, not the feature list, is
//!    what a parametric model consists of.

use std::collections::HashMap;

use artificer_geometry::Point3;

use crate::datum::DatumAlignment;
use crate::fit::DeviationStats;
use crate::merge::refit_like;
use crate::mesh::TriangleMesh;
use crate::numeric::refine_least_squares;
use crate::report::FeatureRecord;
use crate::segment::SurfaceClass;
use crate::transform::RigidTransform;

/// Two features must share at least this many mesh edges (directly or
/// through a round) to be merge candidates.
const MIN_SHARED_EDGES: usize = 8;
/// Residual floor for the BIC cost so exact synthetic fits stay finite.
const NOISE_FLOOR: f64 = 1e-3;
/// Safety cap: a merged surface must still meet this multiple of tolerance.
const MERGE_RMS_CAP: f64 = 1.25;
/// Round faces within this multiple of tolerance of the merged surface
/// fold into it when their seam dissolves.
const DISSOLVE_BAND: f64 = 2.0;

fn parameter_count(surface: &SurfaceClass) -> usize {
    match surface {
        SurfaceClass::Plane(_) => 3,
        SurfaceClass::Sphere(_) => 4,
        SurfaceClass::Cylinder(_) => 5,
        SurfaceClass::Blend(_) => 5,
        SurfaceClass::Cone(_) => 6,
        SurfaceClass::Pattern(_) | SurfaceClass::EdgeRound(_) | SurfaceClass::Freeform => 8,
    }
}

/// BIC-style description cost of one feature: residual code length plus
/// parameter overhead.
fn description_cost(face_count: usize, rms: f64, params: usize) -> f64 {
    let n = face_count.max(2) as f64;
    2.0 * n * rms.max(NOISE_FLOOR).ln() + params as f64 * n.ln()
}

fn same_kind(a: &SurfaceClass, b: &SurfaceClass) -> bool {
    matches!(
        (a, b),
        (SurfaceClass::Plane(_), SurfaceClass::Plane(_))
            | (SurfaceClass::Cylinder(_), SurfaceClass::Cylinder(_))
            | (SurfaceClass::Sphere(_), SurfaceClass::Sphere(_))
            | (SurfaceClass::Cone(_), SurfaceClass::Cone(_))
            | (SurfaceClass::Blend(_), SurfaceClass::Blend(_))
    )
}

/// One merge candidate: the two feature indices, the round they join
/// through if any, and how many mesh edges they share.
type MergeCandidate = (usize, usize, Option<usize>, usize);

/// The order merge candidates are evaluated in.
///
/// Shared-edge count decides, but it ties constantly — symmetric parts are
/// the normal case. The tie has to be broken on something intrinsic to the
/// candidate, because the list is built by iterating a `HashMap` and that
/// order is randomised per map instance. A stable sort would then hand the
/// choice between two tied, mutually exclusive merges to the hash seed, and
/// the pipeline promises a deterministic result. Appending the indices makes
/// this a total order over distinct candidates.
fn candidate_order(
    candidate: &MergeCandidate,
) -> (std::cmp::Reverse<usize>, usize, usize, Option<usize>) {
    let &(a, b, via_round, count) = candidate;
    (std::cmp::Reverse(count), a, b, via_round)
}

pub struct ConsolidateOutcome {
    pub merges: usize,
    pub rounds_dissolved: usize,
}

/// Rungs 1 and 2: MDL-gated merging over the feature adjacency graph,
/// dissolving seam rounds between merged surfaces.
pub fn consolidate_features(
    mesh: &TriangleMesh,
    features: &mut Vec<FeatureRecord>,
    tolerance: f64,
    alignment: Option<&DatumAlignment>,
) -> ConsolidateOutcome {
    let adjacency = mesh.face_adjacency();
    let mut outcome = ConsolidateOutcome {
        merges: 0,
        rounds_dissolved: 0,
    };
    for _pass in 0..24 {
        // Feature label per face, then boundary-edge counts per pair.
        let mut label = vec![u32::MAX; mesh.triangles().len()];
        for (index, feature) in features.iter().enumerate() {
            for &face in &feature.faces {
                label[face as usize] = index as u32;
            }
        }
        let mut boundary: HashMap<(u32, u32), usize> = HashMap::new();
        for face in 0..mesh.triangles().len() {
            let a = label[face];
            if a == u32::MAX {
                continue;
            }
            for &neighbor in &adjacency[face] {
                let b = label[neighbor as usize];
                if b != u32::MAX && a < b {
                    *boundary.entry((a, b)).or_default() += 1;
                }
            }
        }
        // Candidates: same-kind pairs touching directly, or joined only
        // through an edge round (the round's two biggest neighbours).
        let mut candidates: Vec<MergeCandidate> = Vec::new();
        for (&(a, b), &count) in &boundary {
            if count < MIN_SHARED_EDGES {
                continue;
            }
            let (a, b) = (a as usize, b as usize);
            if same_kind(&features[a].surface, &features[b].surface) {
                candidates.push((a, b, None, count));
            }
        }
        for (round_index, feature) in features.iter().enumerate() {
            if !matches!(feature.surface, SurfaceClass::EdgeRound(_)) {
                continue;
            }
            let mut neighbors: Vec<(usize, usize)> = boundary
                .iter()
                .filter_map(|(&(a, b), &count)| {
                    if a as usize == round_index {
                        Some((b as usize, count))
                    } else if b as usize == round_index {
                        Some((a as usize, count))
                    } else {
                        None
                    }
                })
                .filter(|&(other, count)| {
                    count >= MIN_SHARED_EDGES
                        && !matches!(
                            features[other].surface,
                            SurfaceClass::EdgeRound(_) | SurfaceClass::Freeform
                        )
                })
                .collect();
            neighbors.sort_by_key(|&(_, count)| std::cmp::Reverse(count));
            if neighbors.len() >= 2 {
                let (a, b) = (neighbors[0].0, neighbors[1].0);
                if same_kind(&features[a].surface, &features[b].surface) {
                    let key = (a.min(b), a.max(b));
                    candidates.push((key.0, key.1, Some(round_index), neighbors[0].1));
                }
            }
        }
        candidates.sort_by_key(candidate_order);
        // Evaluate: union refit, safety cap, then the description-length
        // decision. First accepted merge restarts the pass.
        let mut applied = false;
        for (a, b, via_round, _) in candidates {
            let mut union_faces = features[a].faces.clone();
            union_faces.extend(&features[b].faces);
            let Some(refit) = refit_like(mesh, &union_faces, &features[a].surface) else {
                continue;
            };
            // refit_like fits raw mesh points; after the datum stage every
            // stored surface is datum-frame, so the union must follow.
            let refit = match alignment {
                Some(alignment) => refit.transformed(&alignment.transform),
                None => refit,
            };
            let Some(union_rms) = refit.rms() else {
                continue;
            };
            if union_rms > MERGE_RMS_CAP * tolerance {
                continue;
            }
            let cost_two = description_cost(
                features[a].face_count,
                features[a].surface.rms().unwrap_or(tolerance),
                parameter_count(&features[a].surface),
            ) + description_cost(
                features[b].face_count,
                features[b].surface.rms().unwrap_or(tolerance),
                parameter_count(&features[b].surface),
            );
            let cost_one = description_cost(
                features[a].face_count + features[b].face_count,
                union_rms,
                parameter_count(&refit),
            );
            if cost_one > cost_two {
                continue;
            }
            // Merge b into a.
            let absorbed_label = crate::finalize::feature_label(&features[b].surface);
            let b_faces = std::mem::take(&mut features[b].faces);
            features[a].faces.extend(b_faces);
            features[a].surface = refit;
            features[a].notes.push(format!(
                "MDL merge absorbed {absorbed_label} (union rms {union_rms:.3})"
            ));
            outcome.merges += 1;
            // A seam round between two pieces of one surface dissolves:
            // its faces fold into the merged surface where they fit, and
            // drop to residue where they do not.
            if let Some(round_index) = via_round {
                let round_faces = std::mem::take(&mut features[round_index].faces);
                let mut leftovers = Vec::new();
                for face in round_faces {
                    let centroid = mesh.face_centroid(face as usize);
                    let centroid = match alignment {
                        Some(alignment) => alignment.transform.apply_point(centroid),
                        None => centroid,
                    };
                    let fits = features[a]
                        .surface
                        .probe(centroid)
                        .is_some_and(|(d, _)| d.abs() <= DISSOLVE_BAND * tolerance);
                    if fits {
                        features[a].faces.push(face);
                    } else {
                        leftovers.push(face);
                    }
                }
                if !leftovers.is_empty() {
                    if let Some(residue) = features
                        .iter_mut()
                        .find(|f| matches!(f.surface, SurfaceClass::Freeform))
                    {
                        residue.faces.extend(leftovers);
                        residue.face_count = residue.faces.len();
                        residue.area = residue
                            .faces
                            .iter()
                            .map(|&face| mesh.face_area(face as usize))
                            .sum();
                    } else {
                        let area = leftovers
                            .iter()
                            .map(|&face| mesh.face_area(face as usize))
                            .sum();
                        features.push(FeatureRecord {
                            id: 0,
                            surface: SurfaceClass::Freeform,
                            face_count: leftovers.len(),
                            area,
                            faces: leftovers,
                            notes: vec!["unexplained residue".to_owned()],
                        });
                    }
                }
                outcome.rounds_dissolved += 1;
            }
            let merged = &mut features[a];
            merged.face_count = merged.faces.len();
            merged.area = merged
                .faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
            features.retain(|feature| !feature.faces.is_empty());
            applied = true;
            break;
        }
        if !applied {
            break;
        }
    }
    outcome
}

/// Rung 2.5: non-adjacent same-surface unification. RANSAC peeling and
/// band stitching can leave one interrupted surface of revolution as
/// several azimuthal arcs whose per-arc fits drift apart (an 8.5, a 9.5
/// and a 10.5 degree cone tiling one taper); the arcs never share a mesh
/// edge, so adjacency-driven consolidation cannot see the pair. Here the
/// candidate screen is geometric — axis-true revolved features (or level
/// planes) whose (z, radius) bands overlap — and the decision is the
/// usual one: the union refit meets tolerance, stays axis-true, and
/// costs less to describe than two parameter sets.
pub fn unify_coaxial_families(
    mesh: &TriangleMesh,
    features: &mut Vec<FeatureRecord>,
    alignment: &DatumAlignment,
    tolerance: f64,
) -> usize {
    const SLACK: f64 = 0.5;
    let mut merges = 0;
    'passes: loop {
        struct Band {
            index: usize,
            kind: u8,
            z0: f64,
            z1: f64,
            r0: f64,
            r1: f64,
        }
        let mut bands: Vec<Band> = Vec::new();
        for (index, feature) in features.iter().enumerate() {
            let (z0, z1, r0, r1) = crate::reconstruct::extents(mesh, &feature.faces, alignment);
            let kind = match &feature.surface {
                SurfaceClass::Cylinder(fit)
                    if fit.axis.z.abs() > 0.999
                        && fit.axis_point.x.hypot(fit.axis_point.y) < 3.0 =>
                {
                    0u8
                }
                SurfaceClass::Cone(fit)
                    if fit.axis.z.abs() > 0.999
                        && crate::reconstruct::cone_axis_offset(fit, (z0 + z1) / 2.0) < 3.0 =>
                {
                    1
                }
                SurfaceClass::Plane(fit) if fit.normal.z.abs() > 0.999 => 2,
                _ => continue,
            };
            bands.push(Band {
                index,
                kind,
                z0,
                z1,
                r0,
                r1,
            });
        }
        let mut pairs: Vec<(usize, usize, f64)> = Vec::new();
        for i in 0..bands.len() {
            for j in i + 1..bands.len() {
                let (a, b) = (&bands[i], &bands[j]);
                if a.kind != b.kind {
                    continue;
                }
                let z_overlap = a.z0.max(b.z0) <= a.z1.min(b.z1) + SLACK;
                let r_overlap = a.r0.max(b.r0) <= a.r1.min(b.r1) + SLACK;
                if z_overlap && r_overlap {
                    pairs.push((
                        a.index,
                        b.index,
                        features[a.index].area + features[b.index].area,
                    ));
                }
            }
        }
        pairs.sort_by(|x, y| y.2.total_cmp(&x.2));
        for (a, b, _) in pairs {
            let mut union_faces = features[a].faces.clone();
            union_faces.extend(&features[b].faces);
            // Arcs of one interrupted surface under-constrain a free fit
            // (a cone fit over two same-side arcs happily tilts), so the
            // union is judged axis-locked, in profile space: a cylinder
            // is a constant radius, a cone a line rho(z), a level plane a
            // constant z — the same reading the band extractor uses.
            let mut sw = 0.0;
            let (mut sz, mut sr, mut szz, mut szr) = (0.0, 0.0, 0.0, 0.0);
            for &face in &union_faces {
                let c = alignment
                    .transform
                    .apply_point(mesh.face_centroid(face as usize));
                let w = mesh.face_area(face as usize);
                let radial = c.x.hypot(c.y);
                sw += w;
                sz += w * c.z;
                sr += w * radial;
                szz += w * c.z * c.z;
                szr += w * c.z * radial;
            }
            if sw <= 0.0 {
                continue;
            }
            let template = if features[a].area >= features[b].area {
                features[a].surface.clone()
            } else {
                features[b].surface.clone()
            };
            let mut squared = 0.0;
            let mut max_abs = 0.0f64;
            let mut residual = |value: f64, reference: f64, w: f64| {
                let r = value - reference;
                squared += w * r * r;
                max_abs = max_abs.max(r.abs());
            };
            let refit = match &template {
                SurfaceClass::Cylinder(fit) => {
                    let radius = sr / sw;
                    for &face in &union_faces {
                        let c = alignment
                            .transform
                            .apply_point(mesh.face_centroid(face as usize));
                        residual(c.x.hypot(c.y), radius, mesh.face_area(face as usize));
                    }
                    let mut fit = *fit;
                    fit.axis_point = Point3::new(0.0, 0.0, sz / sw);
                    fit.axis = artificer_geometry::Vector3::new(0.0, 0.0, 1.0);
                    fit.radius = radius;
                    fit.deviation = DeviationStats {
                        rms: (squared / sw).sqrt(),
                        max_abs,
                    };
                    SurfaceClass::Cylinder(fit)
                }
                SurfaceClass::Cone(fit) => {
                    let denom = sw * szz - sz * sz;
                    if denom.abs() < 1e-9 {
                        continue;
                    }
                    let slope = (sw * szr - sz * sr) / denom;
                    let intercept = (sr - slope * sz) / sw;
                    if !(0.02..=12.0).contains(&slope.abs()) {
                        continue;
                    }
                    for &face in &union_faces {
                        let c = alignment
                            .transform
                            .apply_point(mesh.face_centroid(face as usize));
                        residual(
                            c.x.hypot(c.y),
                            intercept + slope * c.z,
                            mesh.face_area(face as usize),
                        );
                    }
                    let mut fit = *fit;
                    fit.apex = Point3::new(0.0, 0.0, -intercept / slope);
                    fit.axis = artificer_geometry::Vector3::new(0.0, 0.0, slope.signum());
                    fit.half_angle = slope.abs().atan();
                    fit.deviation = DeviationStats {
                        rms: (squared / sw).sqrt(),
                        max_abs,
                    };
                    SurfaceClass::Cone(fit)
                }
                SurfaceClass::Plane(fit) => {
                    let level = sz / sw;
                    for &face in &union_faces {
                        let c = alignment
                            .transform
                            .apply_point(mesh.face_centroid(face as usize));
                        residual(c.z, level, mesh.face_area(face as usize));
                    }
                    let mut fit = *fit;
                    fit.origin = Point3::new(0.0, 0.0, level);
                    fit.normal = artificer_geometry::Vector3::new(0.0, 0.0, fit.normal.z.signum());
                    fit.deviation = DeviationStats {
                        rms: (squared / sw).sqrt(),
                        max_abs,
                    };
                    SurfaceClass::Plane(fit)
                }
                _ => continue,
            };
            let Some(union_rms) = refit.rms() else {
                continue;
            };
            if union_rms > MERGE_RMS_CAP * tolerance {
                continue;
            }
            let cost_two = description_cost(
                features[a].face_count,
                features[a].surface.rms().unwrap_or(tolerance),
                parameter_count(&features[a].surface),
            ) + description_cost(
                features[b].face_count,
                features[b].surface.rms().unwrap_or(tolerance),
                parameter_count(&features[b].surface),
            );
            let cost_one = description_cost(
                features[a].face_count + features[b].face_count,
                union_rms,
                parameter_count(&refit),
            );
            if cost_one > cost_two {
                continue;
            }
            let absorbed_label = crate::finalize::feature_label(&features[b].surface);
            let b_faces = std::mem::take(&mut features[b].faces);
            features[a].faces.extend(b_faces);
            features[a].surface = refit;
            features[a].notes.push(format!(
                "coaxial family unified with {absorbed_label} (union rms {union_rms:.3})"
            ));
            let merged = &mut features[a];
            merged.face_count = merged.faces.len();
            merged.area = merged
                .faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
            features.retain(|feature| !feature.faces.is_empty());
            merges += 1;
            continue 'passes;
        }
        break;
    }
    merges
}

/// Rung 3: the joint solve. Coaxial features share one axis solved over
/// all their sample points simultaneously; level planes share the datum
/// direction; equal radii unify. Returns the shared-parameter entities
/// as human-readable notes.
pub fn solve_shared_parameters(
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    alignment: Option<&DatumAlignment>,
    tolerance: f64,
) -> Vec<String> {
    let identity = RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let mut parameters = Vec::new();
    // Coaxial cluster: everything with an axis locked to +/-Z whose axis
    // line passes near the datum origin.
    let coaxial: Vec<usize> = (0..features.len())
        .filter(|&index| match &features[index].surface {
            SurfaceClass::Cylinder(fit) => {
                fit.axis.z.abs() > 0.999 && fit.axis_point.x.hypot(fit.axis_point.y) < 2.0
            }
            SurfaceClass::Cone(fit) => {
                let faces = &features[index].faces;
                let z_mid = if faces.is_empty() {
                    fit.apex.z
                } else {
                    faces
                        .iter()
                        .map(|&f| to_frame.apply_point(mesh.face_centroid(f as usize)).z)
                        .sum::<f64>()
                        / faces.len() as f64
                };
                fit.axis.z.abs() > 0.999 && crate::reconstruct::cone_axis_offset(fit, z_mid) < 2.0
            }
            SurfaceClass::Blend(fit) => {
                fit.axis.z.abs() > 0.999 && fit.axis_point.x.hypot(fit.axis_point.y) < 2.0
            }
            SurfaceClass::Pattern(_) => true,
            _ => false,
        })
        .collect();
    let cylinder_members: Vec<usize> = coaxial
        .iter()
        .copied()
        .filter(|&index| matches!(features[index].surface, SurfaceClass::Cylinder(_)))
        .collect();
    if cylinder_members.len() >= 2 {
        // Joint axis: minimize radial variance across every member
        // cylinder at once, each keeping its own radius.
        let mut groups: Vec<Vec<(f64, f64)>> = Vec::new();
        for &index in &cylinder_members {
            let faces = &features[index].faces;
            let stride = faces.len().div_ceil(4000).max(1);
            groups.push(
                faces
                    .iter()
                    .step_by(stride)
                    .map(|&face| {
                        let c = to_frame.apply_point(mesh.face_centroid(face as usize));
                        (c.x, c.y)
                    })
                    .collect(),
            );
        }
        let solved = refine_least_squares(
            vec![0.0, 0.0],
            |p| {
                let mut residuals = Vec::new();
                for group in &groups {
                    let mean: f64 = group
                        .iter()
                        .map(|(x, y)| (x - p[0]).hypot(y - p[1]))
                        .sum::<f64>()
                        / group.len().max(1) as f64;
                    for (x, y) in group {
                        residuals.push((x - p[0]).hypot(y - p[1]) - mean);
                    }
                }
                residuals
            },
            20,
        );
        let (x0, y0) = (solved[0], solved[1]);
        // Apply: every coaxial feature adopts the shared axis line.
        for &index in &coaxial {
            match &mut features[index].surface {
                SurfaceClass::Cylinder(fit) => {
                    fit.axis_point = Point3::new(x0, y0, fit.axis_point.z);
                }
                SurfaceClass::Cone(fit) => {
                    fit.apex = Point3::new(x0, y0, fit.apex.z);
                }
                SurfaceClass::Blend(fit) => {
                    fit.axis_point = Point3::new(x0, y0, fit.axis_point.z);
                }
                SurfaceClass::Pattern(fit) => {
                    fit.axis_point = Point3::new(x0, y0, fit.axis_point.z);
                }
                _ => {}
            }
        }
        // Refresh cylinder radii and deviations against the shared axis.
        for (&index, group) in cylinder_members.iter().zip(&groups) {
            if let SurfaceClass::Cylinder(fit) = &mut features[index].surface {
                let radii: Vec<f64> = group.iter().map(|(x, y)| (x - x0).hypot(y - y0)).collect();
                let mean = radii.iter().sum::<f64>() / radii.len().max(1) as f64;
                let rms = (radii.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>()
                    / radii.len().max(1) as f64)
                    .sqrt();
                let max_abs = radii
                    .iter()
                    .map(|r| (r - mean).abs())
                    .fold(0.0f64, f64::max);
                fit.radius = mean;
                fit.deviation = DeviationStats { rms, max_abs };
            }
        }
        parameters.push(format!(
            "shared axis at (x {x0:+.3}, y {y0:+.3}) referenced by {} coaxial feature(s)",
            coaxial.len()
        ));
    }
    // Shared direction: every exactly-level plane references one entity.
    let level_planes = features
        .iter()
        .filter(|f| matches!(&f.surface, SurfaceClass::Plane(fit) if fit.normal.z.abs() > 0.999))
        .count();
    if level_planes >= 2 {
        parameters.push(format!(
            "shared direction +/-Z referenced by {level_planes} plane(s) and the axis family"
        ));
    }
    // Equal-radius groups among coaxial cylinders.
    let mut radius_groups: Vec<(f64, Vec<usize>)> = Vec::new();
    for &index in &cylinder_members {
        let SurfaceClass::Cylinder(fit) = &features[index].surface else {
            continue;
        };
        match radius_groups
            .iter_mut()
            .find(|(radius, _)| (*radius - fit.radius).abs() <= 2.0 * tolerance)
        {
            Some((_, members)) => members.push(index),
            None => radius_groups.push((fit.radius, vec![index])),
        }
    }
    for (_, members) in radius_groups.iter().filter(|(_, m)| m.len() >= 2) {
        let mut weighted = 0.0;
        let mut weight = 0.0;
        for &index in members {
            if let SurfaceClass::Cylinder(fit) = &features[index].surface {
                weighted += fit.radius * features[index].area;
                weight += features[index].area;
            }
        }
        let shared = weighted / weight.max(1e-12);
        for &index in members {
            if let SurfaceClass::Cylinder(fit) = &mut features[index].surface {
                fit.radius = shared;
            }
        }
        parameters.push(format!(
            "shared diameter {:.3} referenced by {} cylinder(s)",
            shared * 2.0,
            members.len()
        ));
    }
    parameters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{CylinderFit, EdgeRoundFit};
    use crate::synth;
    use artificer_geometry::Vector3;

    fn cylinder_feature(
        mesh: &TriangleMesh,
        faces: Vec<u32>,
        axis_point: Point3,
        radius: f64,
        rms: f64,
    ) -> FeatureRecord {
        let area = faces.iter().map(|&f| mesh.face_area(f as usize)).sum();
        FeatureRecord {
            id: 0,
            surface: SurfaceClass::Cylinder(CylinderFit {
                axis_point,
                axis: Vector3::new(0.0, 0.0, 1.0),
                radius,
                deviation: DeviationStats { rms, max_abs: rms },
            }),
            face_count: faces.len(),
            area,
            faces,
            notes: Vec::new(),
        }
    }

    #[test]
    fn split_cylinder_with_seam_round_merges_and_dissolves() {
        // One cylinder split into upper and lower halves with a fake seam
        // round between them: MDL merging over the adjacency graph must
        // unify the halves and dissolve the seam.
        let mesh = synth::open_cylinder(10.0, 30.0, 96, 30);
        let (mut lower, mut seam, mut upper) = (Vec::new(), Vec::new(), Vec::new());
        for face in 0..mesh.triangles().len() {
            let z = mesh.face_centroid(face).z;
            if z < 14.0 {
                lower.push(face as u32);
            } else if z < 16.0 {
                seam.push(face as u32);
            } else {
                upper.push(face as u32);
            }
        }
        let seam_area: f64 = seam.iter().map(|&f| mesh.face_area(f as usize)).sum();
        let mut features = vec![
            cylinder_feature(&mesh, lower, Point3::new(0.0, 0.0, 0.0), 10.0, 0.01),
            cylinder_feature(&mesh, upper, Point3::new(0.0, 0.0, 0.0), 10.0, 0.01),
            FeatureRecord {
                id: 0,
                surface: SurfaceClass::EdgeRound(EdgeRoundFit {
                    span: 0.5,
                    deviation: DeviationStats {
                        rms: 0.05,
                        max_abs: 0.1,
                    },
                }),
                face_count: seam.len(),
                area: seam_area,
                faces: seam,
                notes: Vec::new(),
            },
        ];
        let outcome = consolidate_features(&mesh, &mut features, 0.05, None);
        assert!(outcome.merges >= 1, "no merge happened");
        assert_eq!(outcome.rounds_dissolved, 1);
        let cylinders = features
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::Cylinder(_)))
            .count();
        assert_eq!(cylinders, 1, "halves did not unify");
        let owned: usize = features.iter().map(|f| f.face_count).sum();
        assert_eq!(owned, mesh.triangles().len(), "faces lost");
    }

    #[test]
    fn candidate_order_is_total_so_ties_cannot_follow_the_hash_seed() {
        // Every candidate here shares the same edge count, which is the
        // case that used to fall through a stable sort to HashMap order.
        let tied: Vec<MergeCandidate> = vec![
            (0, 1, None, 12),
            (2, 3, None, 12),
            (0, 3, Some(7), 12),
            (0, 3, None, 12),
            (1, 2, None, 12),
        ];
        let mut expected = tied.clone();
        expected.sort_by_key(candidate_order);

        // Whatever order the map handed them over in, the evaluation order
        // must come out the same.
        for rotation in 0..tied.len() {
            let mut shuffled = tied.clone();
            shuffled.rotate_left(rotation);
            shuffled.sort_by_key(candidate_order);
            assert_eq!(
                shuffled, expected,
                "rotation {rotation} sorted to a different evaluation order"
            );
        }
        let mut reversed = tied.clone();
        reversed.reverse();
        reversed.sort_by_key(candidate_order);
        assert_eq!(reversed, expected, "reversed input sorted differently");

        // And the key must still put the strongest candidate first.
        let mut mixed: Vec<MergeCandidate> =
            vec![(9, 9, None, 3), (0, 1, None, 40), (4, 5, None, 12)];
        mixed.sort_by_key(candidate_order);
        assert_eq!(mixed[0].3, 40, "edge count must outrank the tie-break");
    }

    #[test]
    fn genuinely_different_radii_survive_mdl() {
        // Stacked cylinders 0.8 mm apart in radius sharing a rim: the
        // union refit breaks the safety cap, so MDL never sees them.
        let mut soup = synth::open_cylinder_soup(10.0, 15.0, 96, 15);
        soup.extend(
            synth::open_cylinder_soup(10.8, 15.0, 96, 15)
                .into_iter()
                .map(|t| t.map(|p| Point3::new(p.x, p.y, p.z + 15.0))),
        );
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let mut lower = Vec::new();
        let mut upper = Vec::new();
        for face in 0..mesh.triangles().len() {
            if mesh.face_centroid(face).z < 15.0 {
                lower.push(face as u32);
            } else {
                upper.push(face as u32);
            }
        }
        let mut features = vec![
            cylinder_feature(&mesh, lower, Point3::new(0.0, 0.0, 0.0), 10.0, 0.01),
            cylinder_feature(&mesh, upper, Point3::new(0.0, 0.0, 15.0), 10.8, 0.01),
        ];
        let outcome = consolidate_features(&mesh, &mut features, 0.05, None);
        assert_eq!(outcome.merges, 0, "distinct radii were wrongly merged");
        assert_eq!(features.len(), 2);
    }

    #[test]
    fn joint_axis_solve_shares_one_axis_line() {
        let mesh = synth::open_cylinder(10.0, 30.0, 96, 30);
        let (mut lower, mut upper) = (Vec::new(), Vec::new());
        for face in 0..mesh.triangles().len() {
            if mesh.face_centroid(face).z < 15.0 {
                lower.push(face as u32);
            } else {
                upper.push(face as u32);
            }
        }
        // Deliberately disagreeing axis points.
        let mut features = vec![
            cylinder_feature(&mesh, lower, Point3::new(0.3, -0.2, 0.0), 10.05, 0.02),
            cylinder_feature(&mesh, upper, Point3::new(-0.25, 0.15, 15.0), 9.95, 0.02),
        ];
        let parameters = solve_shared_parameters(&mesh, &mut features, None, 0.05);
        assert!(parameters.iter().any(|p| p.contains("shared axis")));
        let points: Vec<Point3> = features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cylinder(fit) => Some(fit.axis_point),
                _ => None,
            })
            .collect();
        assert!((points[0].x - points[1].x).abs() < 1e-9);
        assert!((points[0].y - points[1].y).abs() < 1e-9);
        // The true axis is the origin; the joint solve must find it.
        assert!(points[0].x.hypot(points[0].y) < 0.02, "axis off origin");
        let radii: Vec<f64> = features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cylinder(fit) => Some(fit.radius),
                _ => None,
            })
            .collect();
        // Equal-radius unification: both halves of one true cylinder.
        assert!((radii[0] - radii[1]).abs() < 1e-9);
    }
}

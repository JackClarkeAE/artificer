//! The end-to-end reverse-engineering pipeline and its report formats.

use artificer_geometry::Point3;

use artificer_geometry::Vector3;

use crate::consolidate::{consolidate_features, solve_shared_parameters, unify_coaxial_families};
use crate::datum::DatumAlignment;
use crate::finalize::{finalize_features, refine_rounds};
use crate::merge::{absorb_into_anchors, merge_fragments};
use crate::mesh::TriangleMesh;
use crate::ransac::{RansacParams, extract_primitives};
use crate::reconstruct::{
    MasterProfile, PatternProposal, ReconstructionPlan, axis_lock_refit, detect_circular_pattern,
    extract_revolved_bands, lock_revolved_surfaces, plan_summary, plan_to_history_json,
    recognize_blends, recognize_pattern_feature, recognize_ring_patterns, reconstruct,
    split_disjoint_bands,
};
use crate::segment::{SegmentationParams, SurfaceClass, classify_region, segment};
use crate::snap::{SnapPolicy, harmonize_surfaces, snap_surface};

#[derive(Clone, Debug)]
pub struct ReverseOptions {
    /// Maximum RMS deviation (mm) for a region to accept an analytic fit.
    pub tolerance: f64,
    pub segmentation: SegmentationParams,
    /// RANSAC peeling over faces the segmentation left freeform; `None`
    /// disables the stage. An `epsilon <= 0` inherits `tolerance`.
    pub ransac: Option<RansacParams>,
    /// Merge fragments of one physical surface (coaxial cylinders,
    /// coplanar planes, concentric spheres) into single features, gated
    /// by a union refit meeting tolerance.
    pub merge_fragments: bool,
    /// Analytic features below this area (mm^2) are demoted to freeform:
    /// a few square millimetres of "cone" on a large part is transition
    /// geometry, not a credible design feature.
    pub min_feature_area: f64,
    /// Re-express all features in an automatically detected datum frame
    /// (dominant feature direction becomes +Z) before canonicalization.
    pub auto_datum: bool,
    /// Which ranked datum candidate to build the frame on. `None` takes
    /// the heaviest, which is the automatic choice.
    pub datum_choice: Option<usize>,
    /// Canonicalization policy; `None` reports raw fitted values.
    pub snap: Option<SnapPolicy>,
    /// Raise the working tolerance to five estimated noise sigmas when
    /// the scan is noisier than the tolerance assumes. A fixed 0.15 on
    /// a sigma 0.07 scan starves every fit and the whole part ships as
    /// one organic photocopy; scaling it recovered 75% analytic on the
    /// same scan. Off restores fixed-tolerance behaviour.
    pub adaptive_tolerance: bool,
    /// Complete the decomposition: claim on-surface faces, recognize edge
    /// rounds between features, collapse the rest into one residue record.
    pub finalize: bool,
    /// MDL-gated merging over the feature adjacency graph, dissolving seam
    /// rounds between merged surfaces (consolidation rungs 1 and 2).
    pub consolidate: bool,
    /// Joint solve for shared parameter entities: one axis for coaxial
    /// features, shared directions and radii (consolidation rung 3).
    pub shared_parameters: bool,
}

impl Default for ReverseOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.05,
            segmentation: SegmentationParams::default(),
            adaptive_tolerance: true,
            ransac: Some(RansacParams::default()),
            merge_fragments: true,
            min_feature_area: 25.0,
            auto_datum: true,
            datum_choice: None,
            snap: Some(SnapPolicy::default()),
            finalize: true,
            consolidate: true,
            shared_parameters: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FeatureRecord {
    pub id: usize,
    pub surface: SurfaceClass,
    pub face_count: usize,
    pub area: f64,
    /// Triangle indices of the region (not serialized; used by viewers).
    pub faces: Vec<u32>,
    /// Canonicalization notes: what was snapped and by how much.
    pub notes: Vec<String>,
}

/// Opening words of the note that marks a feature the discriminator
/// pulled back out of the unnamed residue.
pub const RECOVERED_NOTE: &str = "recovered from the unnamed residue";

/// Recovered fragments smaller than this (mm²) are summarised in the
/// printed listing rather than named one by one.
pub const FRAGMENT_AREA: f64 = 25.0;

impl FeatureRecord {
    /// Whether this feature came back from the residue rather than from
    /// the region pass.
    ///
    /// The distinction is not cosmetic. A recovered surface is a
    /// fragment: it earned the right to carry its own measured area and
    /// nothing more. Treating fragments as evidence let the gear fold
    /// thousands of them into a 120-fold ring pattern and invent
    /// 19,903 mm² of material that was never scanned — three times the
    /// invention the whole model had before.
    pub fn is_recovered(&self) -> bool {
        self.notes
            .iter()
            .any(|note| note.starts_with(RECOVERED_NOTE))
    }
}

/// Feature count and classified coverage captured after one pipeline stage.
#[derive(Clone, Debug)]
pub struct StageMetric {
    pub stage: String,
    pub features: usize,
    pub classified_fraction: f64,
    /// Wall-clock seconds this stage took. A pipeline that answers in
    /// fifteen seconds on one part and an hour on another is asking to
    /// be measured, and the stage table is where a reader already
    /// looks to see what each stage did.
    pub seconds: f64,
}

#[derive(Clone, Debug)]
pub struct ReverseReport {
    pub features: Vec<FeatureRecord>,
    pub total_area: f64,
    pub classified_area: f64,
    /// Per-stage progress: how consolidation earned its keep.
    pub stages: Vec<StageMetric>,
    /// Shared-parameter entities from the joint solve (rung 3).
    pub parameters: Vec<String>,
    /// When auto-datum ran: the transform from scan coordinates into the
    /// datum frame that all reported features are expressed in.
    pub datum: Option<DatumAlignment>,
    /// When auto-datum ran: the revolved-profile reconstruction plan.
    pub plan: Option<ReconstructionPlan>,
    /// The scan's estimated noise sigma (mm), from the residual floor
    /// of local plane fits — what the adaptive stages scaled by.
    pub noise_sigma: f64,
    /// Fits demoted for not describing their own material. A demotion
    /// is a finding — some stage produced a surface its own faces
    /// disown — so it travels with the report rather than being
    /// folded into the parameter notes, and every consumer can say so.
    pub demotions: Vec<String>,
    /// The tolerance the run was performed at, so later stages (rebuild)
    /// can reason in the same noise units rather than re-guessing.
    pub tolerance: f64,
}

/// Segments the mesh, fits analytic surfaces, and canonicalizes the result.
pub fn reverse_engineer(mesh: &TriangleMesh, options: &ReverseOptions) -> ReverseReport {
    // The scan's own noise floor, measured before anything else needs
    // it: the discriminator widens its windows by it, and every later
    // consumer reasons in the same units.
    let noise_sigma = crate::noise::estimate_noise(mesh);
    // The scan sets the floor: fits judged tighter than the noise can
    // possibly satisfy refuse everything, and the part ships as a
    // photocopy. RANSAC's default epsilon inherits the tolerance, so
    // the floor reaches it too.
    let mut effective = options.clone();
    if std::env::var_os("ARTIFICER_NOISE_DEBUG").is_some() {
        eprintln!(
            "noise-debug: sigma {noise_sigma:.4} tolerance {:.3}",
            options.tolerance
        );
    }
    // Scan-sized meshes only: on a small synthetic the estimator's
    // patches inevitably straddle features and read geometry as
    // noise, and a clean synthetic needs no floor. Every real scan
    // fixture is hundreds of thousands of faces.
    if effective.adaptive_tolerance && mesh.triangles().len() >= 100_000 {
        // Seven, not five: the estimator's edge-difference statistic
        // under-reads sliver-refined simulations by about half (their
        // garbage vertex normals scatter injected noise off-normal),
        // and 7 sigma-hat empirically matches the hand-tuned optimum
        // on the sigma 0.07 fixture (75% analytic vs 41% at five).
        let floor = 7.0 * noise_sigma;
        if floor > effective.tolerance {
            effective.tolerance = floor;
        }
    }
    let options = &effective;
    let regions = segment(mesh, &options.segmentation);
    let mut features: Vec<FeatureRecord> = regions
        .into_iter()
        .enumerate()
        .map(|(id, region)| {
            let surface = classify_region(mesh, &region, options.tolerance, &options.segmentation);
            FeatureRecord {
                id,
                surface,
                face_count: region.faces.len(),
                area: region.area,
                faces: region.faces,
                notes: Vec::new(),
            }
        })
        .collect();
    let mut stages: Vec<StageMetric> = Vec::new();
    let mut stage_mark = std::time::Instant::now();
    let mut record_stage =
        |stages: &mut Vec<StageMetric>, name: &str, features: &[FeatureRecord]| {
            let total: f64 = features.iter().map(|f| f.area).sum();
            let classified: f64 = features
                .iter()
                .filter(|f| !matches!(f.surface, SurfaceClass::Freeform))
                .map(|f| f.area)
                .sum();
            let now = std::time::Instant::now();
            let seconds = now.duration_since(stage_mark).as_secs_f64();
            stage_mark = now;
            stages.push(StageMetric {
                stage: name.to_owned(),
                features: features.len(),
                classified_fraction: if total > 0.0 { classified / total } else { 0.0 },
                seconds,
            });
        };
    record_stage(&mut stages, "segment+classify", &features);
    if let Some(ransac) = &options.ransac {
        let mut params = *ransac;
        if params.epsilon <= 0.0 {
            params.epsilon = options.tolerance;
        }
        let residual: Vec<u32> = features
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::Freeform))
            .flat_map(|f| f.faces.iter().copied())
            .collect();
        if residual.len() >= params.min_support_faces {
            let adjacency = mesh.face_adjacency();
            let extracted = extract_primitives(mesh, &residual, &adjacency, &params);
            if !extracted.is_empty() {
                let mut taken = vec![false; mesh.triangles().len()];
                for primitive in &extracted {
                    for &face in &primitive.faces {
                        taken[face as usize] = true;
                    }
                }
                for feature in &mut features {
                    if matches!(feature.surface, SurfaceClass::Freeform) {
                        feature.faces.retain(|&face| !taken[face as usize]);
                        feature.face_count = feature.faces.len();
                        feature.area = feature
                            .faces
                            .iter()
                            .map(|&face| mesh.face_area(face as usize))
                            .sum();
                    }
                }
                features.retain(|f| !f.faces.is_empty());
                for primitive in extracted {
                    let area = primitive
                        .faces
                        .iter()
                        .map(|&face| mesh.face_area(face as usize))
                        .sum();
                    features.push(FeatureRecord {
                        id: 0,
                        surface: primitive.surface,
                        face_count: primitive.faces.len(),
                        area,
                        faces: primitive.faces,
                        notes: vec!["extracted by RANSAC peeling".to_owned()],
                    });
                }
            }
        }
    }
    let merge_epsilon = options.ransac.as_ref().map_or(options.tolerance, |r| {
        if r.epsilon > 0.0 {
            r.epsilon
        } else {
            options.tolerance
        }
    });
    record_stage(&mut stages, "ransac-peel", &features);
    if options.merge_fragments {
        features = merge_fragments(mesh, features, merge_epsilon);
        features = absorb_into_anchors(mesh, features, merge_epsilon);
    }
    // Trust, then verify: every stage above hands its claims on without
    // ever re-reading the scan, so a fit that stopped describing its own
    // material travels the whole pipeline unchallenged. Ask each one
    // whether its faces are still on it, and demote the ones that are
    // not — freeform is where an honest second attempt can be made.
    let mut support_notes: Vec<String> = Vec::new();
    {
        let support = crate::validate::demote_unsupported(
            mesh,
            &mut features,
            &crate::transform::RigidTransform::IDENTITY,
            options.tolerance,
        );
        if support.demoted > 0 {
            support_notes.push(format!(
                "{} fit(s) totalling {:.0} mm^2 did not describe their own material and were demoted",
                support.demoted, support.demoted_area
            ));
            support_notes.extend(support.notes);
        }
    }
    record_stage(&mut stages, "merge+absorb", &features);
    // Publish what the part offers before saying which was taken: the
    // datum is the decision every later stage is expressed in, and a
    // reader cannot judge it without seeing what else was available.
    let candidates = crate::datum::datum_candidates(&features);
    let mut datum_notes: Vec<String> = Vec::new();
    if options.auto_datum && candidates.len() > 1 {
        for (rank, candidate) in candidates.iter().take(4).enumerate() {
            datum_notes.push(format!(
                "datum candidate {rank}: ({:+.4} {:+.4} {:+.4}) backed by {:.0} mm^2{}",
                candidate.direction.x,
                candidate.direction.y,
                candidate.direction.z,
                candidate.weight,
                if rank == options.datum_choice.unwrap_or(0) {
                    "  <- chosen"
                } else {
                    ""
                }
            ));
        }
    }
    let mut datum = if options.auto_datum {
        crate::datum::datum_alignment_on(&features, options.datum_choice.unwrap_or(0))
    } else {
        None
    };
    let mut detected_pattern: Option<PatternProposal> = None;
    let mut master_profiles: Vec<MasterProfile> = Vec::new();
    if let Some(alignment) = datum.as_mut() {
        // With a datum axis known, near-axis cylinders refit with the axis
        // locked: noisy patch axes stop wobbling, radii cluster, and a
        // second merge pass stitches interrupted bands into one surface.
        let r = alignment.transform.rotation;
        let scan_axis = Vector3::new(r[2][0], r[2][1], r[2][2]);
        let locked = axis_lock_refit(mesh, &mut features, scan_axis, options.tolerance);
        if locked > 0 && options.merge_fragments {
            features = merge_fragments(mesh, features, merge_epsilon);
            features = absorb_into_anchors(mesh, features, merge_epsilon);
        }
        for feature in &mut features {
            feature.surface = feature.surface.transformed(&alignment.transform);
        }
        recognize_blends(mesh, &mut features, alignment, options.tolerance);
        extract_revolved_bands(mesh, &mut features, alignment, options.tolerance);
        // Only after the band extractor has had its say: it reads a
        // tilted axis as the signature of a misfit worth dismantling and
        // re-stitching, so locking axes any earlier hides its donors from
        // it and leaves the fragments it would have stitched.
        lock_revolved_surfaces(mesh, &mut features, alignment, options.tolerance);
        // Ask the datum again, now that the answer can be known.
        //
        // The frame has to be chosen before any of the stages above
        // can run, which is exactly when the feature set is at its
        // most fragmented: on a noisy scan it is hundreds of thousands
        // of shards, and the part's own axis — carried by a wall that
        // has not been stitched together yet — has no coherent
        // representative among them, while one small chamfer cone is a
        // whole, well-fitted feature that wins on its own. That put
        // the origin on a lug hole, 40 mm out, and every revolved
        // stage then reasoned about the wrong centre while reporting
        // healthy residuals. Stitching has since made the part's own
        // surfaces whole, so the question is worth re-asking here.
        //
        // Only the origin is revisited. The direction was settled by
        // an area-weighted vote over every feature, which fragments do
        // not fool, and the features are already expressed about it.
        {
            /// A correction smaller than this (mm) is not worth the
            /// churn — and on a healthy part there is nothing to fix.
            const ORIGIN_CORRECTION_MIN: f64 = 0.5;
            let axis = Vector3::new(0.0, 0.0, 1.0);
            if let Some((point, area, members)) = crate::datum::dominant_axis_line(&features, axis)
            {
                let offset = Vector3::new(point.x, point.y, 0.0);
                if offset.length() > ORIGIN_CORRECTION_MIN {
                    let correction = crate::transform::RigidTransform {
                        rotation: crate::transform::RigidTransform::IDENTITY.rotation,
                        translation: offset * -1.0,
                    };
                    for feature in &mut features {
                        feature.surface = feature.surface.transformed(&correction);
                    }
                    alignment.transform = alignment.transform.then(&correction);
                    datum_notes.push(format!(
                        "origin corrected by {:.2} mm once stitching matured the features: \
                         the axis is now backed by {area:.0} mm^2 of {members} coaxial \
                         feature(s)",
                        offset.length()
                    ));
                }
            }
        }
        // The second checkpoint. Everything since the first ran in the
        // datum frame — a second merge, band stitching, axis locking —
        // and each can move a surface off the material it stands for.
        {
            let support = crate::validate::demote_unsupported(
                mesh,
                &mut features,
                &alignment.transform,
                options.tolerance,
            );
            if support.demoted > 0 {
                support_notes.push(format!(
                    "after datum: {} fit(s) totalling {:.0} mm^2 no longer described their material",
                    support.demoted, support.demoted_area
                ));
                support_notes.extend(support.notes);
            }
        }
        record_stage(&mut stages, "datum+lock+bands", &features);
        detected_pattern = detect_circular_pattern(mesh, &features, alignment);
        if let Some(pattern) = &detected_pattern
            && let Some(mut profile) = recognize_pattern_feature(
                mesh,
                &mut features,
                alignment,
                pattern,
                options.tolerance,
            )
        {
            profile.feature_id = features.len() - 1;
            master_profiles.push(profile);
        }
    }
    record_stage(&mut stages, "pattern", &features);
    // Significance filter: what survives to here as a tiny analytic patch
    // is transition geometry (rounded edges, chamfer rows), not a design
    // feature — report it as unexplained instead of as confetti.
    for feature in &mut features {
        if feature.area < options.min_feature_area
            && !matches!(
                feature.surface,
                SurfaceClass::Freeform | SurfaceClass::Blend(_) | SurfaceClass::Pattern(_)
            )
        {
            feature.surface = SurfaceClass::Freeform;
        }
    }
    features.sort_by(|a, b| b.area.total_cmp(&a.area));
    for (id, feature) in features.iter_mut().enumerate() {
        feature.id = id;
    }
    if let Some(policy) = &options.snap {
        let mut surfaces: Vec<SurfaceClass> = features.iter().map(|f| f.surface.clone()).collect();
        for (feature, surface) in features.iter_mut().zip(surfaces.iter_mut()) {
            feature.notes.extend(snap_surface(surface, policy));
        }
        let harmonize_notes = harmonize_surfaces(&mut surfaces, policy);
        for ((feature, surface), notes) in features.iter_mut().zip(surfaces).zip(harmonize_notes) {
            feature.surface = surface;
            feature.notes.extend(notes);
        }
    }
    record_stage(&mut stages, "snap+harmonize", &features);
    // Frames are discovered mid-pipeline but reported with the other
    // shared parameters, which are collected further down.
    let mut frame_notes: Vec<String> = Vec::new();
    if let Some(alignment) = datum.as_mut() {
        let mut published = datum_notes;
        published.append(&mut alignment.notes);
        alignment.notes = published;
    }
    let mut parameters_late: Vec<String> = Vec::new();
    if options.finalize {
        finalize_features(
            mesh,
            &mut features,
            datum.as_ref(),
            options.tolerance,
            noise_sigma,
        );
        features.sort_by(|a, b| b.area.total_cmp(&a.area));
        for (id, feature) in features.iter_mut().enumerate() {
            feature.id = id;
        }
        record_stage(&mut stages, "finalize", &features);
    }
    // Profiles were stamped with pre-sort indices; re-stamp them against
    // the final ids by matching each pattern feature's count and band.
    restamp_profiles(&mut master_profiles, &features);
    if options.consolidate {
        consolidate_features(mesh, &mut features, options.tolerance, datum.as_ref());
        features.sort_by(|a, b| b.area.total_cmp(&a.area));
        for (id, feature) in features.iter_mut().enumerate() {
            feature.id = id;
        }
        record_stage(&mut stages, "mdl-consolidate", &features);
        refine_rounds(mesh, &mut features, datum.as_ref(), options.tolerance);
        record_stage(&mut stages, "round-refine", &features);
        // What a designer specified is a small set of directions, not one
        // per face. Recover them and re-solve to them — but offer, never
        // impose: a surface joins its frame only if it still explains its
        // own samples afterwards.
        let constrained = crate::constrain::constrain_features(
            mesh,
            &mut features,
            datum.as_ref(),
            options.tolerance,
        );
        for frame in &constrained.frames {
            frame_notes.push(format!(
                "frame of {} surface(s), {:.0} mm^2: ({:+.4} {:+.4} {:+.4}) / ({:+.4} {:+.4} {:+.4}) /                  ({:+.4} {:+.4} {:+.4}); worst correction {:.3} deg",
                frame.members.len(),
                frame.area,
                frame.axes[0].x, frame.axes[0].y, frame.axes[0].z,
                frame.axes[1].x, frame.axes[1].y, frame.axes[1].z,
                frame.axes[2].x, frame.axes[2].y, frame.axes[2].z,
                frame.worst_correction
            ));
        }
        if constrained.refused > 0 {
            frame_notes.push(format!(
                "{} surface(s) totalling {:.0} mm^2 were offered a frame and their own samples                  refused it: the part is genuinely skew there",
                constrained.refused, constrained.refused_area
            ));
        }
        record_stage(&mut stages, "constrain", &features);
        if let Some(alignment) = datum.as_ref() {
            // Again, now that merging has made features whole. A surface
            // that only fits as an absurd sphere while it is scattered
            // across fragments reads as the plain cone it is once its
            // pieces are one feature, and the fits that matter for the
            // rebuild are the ones that survive to this point.
            lock_revolved_surfaces(mesh, &mut features, alignment, options.tolerance);
            // Before unifying: a band that spans a gap in height is two
            // surfaces, and every judgement made about it — solidity,
            // family membership, what to emit — is wrong while it is one.
            split_disjoint_bands(mesh, &mut features, alignment, 2.0);
            unify_coaxial_families(mesh, &mut features, alignment, options.tolerance);
            // A fillet sliced into concentric strips is not a failure any
            // single fit can see — every strip is a genuinely good cone.
            // The judgement has to be made over the chain.
            parameters_late.extend(crate::reconstruct::unify_blend_chains(
                mesh,
                &mut features,
                alignment,
                options.tolerance,
            ));
            for (id, feature) in features.iter_mut().enumerate() {
                feature.id = id;
            }
            record_stage(&mut stages, "coaxial-unify", &features);
            let mut ring_profiles =
                recognize_ring_patterns(mesh, &mut features, alignment, options.tolerance);
            master_profiles.append(&mut ring_profiles);
            // Folds drop consumed features and append patterns, so ids —
            // which labels, skip notes, and the viewer all key on — must
            // be renumbered and every profile re-stamped.
            for (id, feature) in features.iter_mut().enumerate() {
                feature.id = id;
            }
            restamp_profiles(&mut master_profiles, &features);
            record_stage(&mut stages, "ring-patterns", &features);
        }
    }
    let mut parameters: Vec<String> = std::mem::take(&mut frame_notes);
    parameters.append(&mut parameters_late);
    if options.shared_parameters {
        parameters.extend(solve_shared_parameters(
            mesh,
            &mut features,
            datum.as_ref(),
            options.tolerance,
        ));
        record_stage(&mut stages, "shared-parameters", &features);
    }
    let total_area: f64 = features.iter().map(|f| f.area).sum();
    let classified_area: f64 = features
        .iter()
        .filter(|f| !matches!(f.surface, SurfaceClass::Freeform))
        .map(|f| f.area)
        .sum();
    let plan = datum.as_ref().map(|alignment| {
        let mut plan = reconstruct(
            mesh,
            &features,
            alignment,
            options.tolerance,
            detected_pattern,
            master_profiles.clone(),
        );
        // The wizard layer: what several surfaces are *together*.
        plan.instances = crate::instance::recognize_instances(
            mesh,
            &features,
            Some(alignment),
            options.tolerance,
        );
        // A bag of operations is not a model. Order them.
        let organic: f64 = features
            .iter()
            .filter(|feature| matches!(feature.surface, SurfaceClass::Freeform))
            .map(|feature| feature.area)
            .sum();
        plan.tree = crate::tree::order_tree(mesh, &features, &plan, Some(alignment), organic);
        plan
    });
    // Every stage above that moved a surface — snapping, harmonizing, the
    // shared-parameter solve — left its `DeviationStats` describing where the
    // surface used to be, and those numbers are what a tolerance decision
    // downstream of this report reads. Re-measure once, here, for two
    // reasons: this is past every decision the pipeline makes, so a corrected
    // residual cannot feed back and change which merges were accepted; and
    // the datum alignment is in scope, which it has to be. After the datum
    // stage the stored surfaces are datum-frame while `mesh` is still in scan
    // coordinates, so the points must be carried across before they can be
    // measured against anything.
    for feature in &mut features {
        let mut points = crate::segment::fit_inputs(mesh, &feature.faces).points;
        if let Some(alignment) = datum.as_ref() {
            for point in &mut points {
                *point = alignment.transform.apply_point(*point);
            }
        }
        feature.surface.recompute_deviation(&points);
    }
    ReverseReport {
        noise_sigma,
        demotions: support_notes,
        features,
        total_area,
        classified_area,
        stages,
        parameters,
        datum,
        plan,
        tolerance: options.tolerance,
    }
}

/// Points every master profile at the pattern feature carrying its fold:
/// same repeat count, closest band midpoint. Runs again after any pass
/// that renumbers or rewrites the feature list.
fn restamp_profiles(
    profiles: &mut [crate::reconstruct::MasterProfile],
    features: &[FeatureRecord],
) {
    for profile in profiles {
        let profile_mid = (profile.z_range.0 + profile.z_range.1) / 2.0;
        let matched = features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Pattern(fit) if fit.count == profile.count => Some((
                    f.id,
                    ((fit.z_range.0 + fit.z_range.1) / 2.0 - profile_mid).abs(),
                )),
                _ => None,
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = matched {
            profile.feature_id = id;
        }
    }
}

fn push_escaped(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_point(out: &mut String, name: &str, point: Point3) {
    out.push_str(&format!(
        "\"{name}\":[{:.6},{:.6},{:.6}]",
        point.x, point.y, point.z
    ));
}

pub fn report_to_json(report: &ReverseReport) -> String {
    let mut out = String::from("{");
    if let Some(alignment) = &report.datum {
        let r = alignment.transform.rotation;
        let t = alignment.transform.translation;
        out.push_str(&format!(
            "\"datum\":{{\"rotation\":[[{:.9},{:.9},{:.9}],[{:.9},{:.9},{:.9}],[{:.9},{:.9},{:.9}]],\"translation\":[{:.6},{:.6},{:.6}],\"notes\":[",
            r[0][0], r[0][1], r[0][2], r[1][0], r[1][1], r[1][2], r[2][0], r[2][1], r[2][2],
            t.x, t.y, t.z
        ));
        for (index, note) in alignment.notes.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            push_escaped(&mut out, note);
        }
        out.push_str("]},");
    }
    out.push_str("\"features\":[");
    for (index, feature) in report.features.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":{},\"kind\":\"{}\",\"faces\":{},\"area\":{:.6}",
            feature.id,
            feature.surface.kind(),
            feature.face_count,
            feature.area
        ));
        match &feature.surface {
            SurfaceClass::Plane(fit) => {
                out.push(',');
                push_point(&mut out, "origin", fit.origin);
                out.push_str(&format!(
                    ",\"normal\":[{:.6},{:.6},{:.6}]",
                    fit.normal.x, fit.normal.y, fit.normal.z
                ));
            }
            SurfaceClass::Cylinder(fit) => {
                out.push(',');
                push_point(&mut out, "axis_point", fit.axis_point);
                out.push_str(&format!(
                    ",\"axis\":[{:.6},{:.6},{:.6}],\"radius\":{:.6},\"diameter\":{:.6}",
                    fit.axis.x,
                    fit.axis.y,
                    fit.axis.z,
                    fit.radius,
                    fit.radius * 2.0
                ));
            }
            SurfaceClass::Sphere(fit) => {
                out.push(',');
                push_point(&mut out, "center", fit.center);
                out.push_str(&format!(",\"radius\":{:.6}", fit.radius));
            }
            SurfaceClass::Cone(fit) => {
                out.push(',');
                push_point(&mut out, "apex", fit.apex);
                out.push_str(&format!(
                    ",\"axis\":[{:.6},{:.6},{:.6}],\"half_angle_deg\":{:.6}",
                    fit.axis.x,
                    fit.axis.y,
                    fit.axis.z,
                    fit.half_angle.to_degrees()
                ));
            }
            SurfaceClass::Blend(fit) => {
                out.push(',');
                push_point(&mut out, "axis_point", fit.axis_point);
                out.push_str(&format!(
                    ",\"axis\":[{:.6},{:.6},{:.6}],\"major_radius\":{:.6},\"fillet_radius\":{:.6}",
                    fit.axis.x, fit.axis.y, fit.axis.z, fit.major_radius, fit.minor_radius
                ));
            }
            SurfaceClass::Torus(fit) => {
                out.push(',');
                push_point(&mut out, "axis_point", fit.axis_point);
                out.push_str(&format!(
                    ",\"axis\":[{:.6},{:.6},{:.6}],\"major_radius\":{:.6},\"minor_radius\":{:.6}",
                    fit.axis.x, fit.axis.y, fit.axis.z, fit.major_radius, fit.minor_radius
                ));
            }
            SurfaceClass::Pattern(fit) => {
                out.push(',');
                push_point(&mut out, "axis_point", fit.axis_point);
                out.push_str(&format!(
                    ",\"axis\":[{:.6},{:.6},{:.6}],\"count\":{},\"z_range\":[{:.6},{:.6}],\"radius_range\":[{:.6},{:.6}],\"worst_instance_rms\":{:.6}",
                    fit.axis.x,
                    fit.axis.y,
                    fit.axis.z,
                    fit.count,
                    fit.z_range.0,
                    fit.z_range.1,
                    fit.radius_range.0,
                    fit.radius_range.1,
                    fit.worst_instance_rms
                ));
            }
            SurfaceClass::EdgeRound(fit) => {
                out.push_str(&format!(",\"span\":{:.6}", fit.span));
            }
            SurfaceClass::Freeform => {}
        }
        if let Some(rms) = feature.surface.rms() {
            out.push_str(&format!(",\"rms\":{rms:.6}"));
        }
        out.push_str(",\"notes\":[");
        for (note_index, note) in feature.notes.iter().enumerate() {
            if note_index > 0 {
                out.push(',');
            }
            push_escaped(&mut out, note);
        }
        out.push_str("]}");
    }
    out.push_str(&format!(
        "],\"total_area\":{:.6},\"classified_area\":{:.6},\"noise_sigma\":{:.6}",
        report.total_area, report.classified_area, report.noise_sigma
    ));
    out.push_str(",\"stages\":[");
    for (index, stage) in report.stages.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"stage\":\"{}\",\"features\":{},\"classified\":{:.4}}}",
            stage.stage, stage.features, stage.classified_fraction
        ));
    }
    out.push(']');
    out.push_str(",\"parameters\":[");
    for (index, parameter) in report.parameters.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_escaped(&mut out, parameter);
    }
    out.push(']');
    if let Some(plan) = &report.plan {
        out.push_str(",\"reconstruction\":");
        out.push_str(&plan_to_history_json(plan));
    }
    out.push('}');
    out
}

pub fn report_summary(report: &ReverseReport) -> String {
    let mut out = String::new();
    if let Some(alignment) = &report.datum {
        out.push_str("datum frame:\n");
        for note in &alignment.notes {
            out.push_str(&format!("  - {note}\n"));
        }
    }
    if !report.demotions.is_empty() {
        out.push_str("fits that did not describe their own material:\n");
        for note in &report.demotions {
            out.push_str(&format!("  - {note}\n"));
        }
    }
    if !report.stages.is_empty() {
        out.push_str("stage progress (features / classified / seconds):\n");
        for stage in &report.stages {
            out.push_str(&format!(
                "  {:<20} {:>6}   {:>5.1}%   {:>7.1}s\n",
                stage.stage,
                stage.features,
                stage.classified_fraction * 100.0,
                stage.seconds
            ));
        }
    }
    if !report.parameters.is_empty() {
        out.push_str("shared parameters:\n");
        for parameter in &report.parameters {
            out.push_str(&format!("  - {parameter}\n"));
        }
    }
    out.push_str(&format!(
        "{} region(s), {:.1}% of surface area classified\n",
        report.features.len(),
        if report.total_area > 0.0 {
            100.0 * report.classified_area / report.total_area
        } else {
            0.0
        }
    ));
    // Fragments recovered from the residue are listed as a group, not
    // one by one. On the test pump 2,000 of them hold under five percent
    // of the area between them and bury the hundred features anyone
    // actually wants to read. They stay in the model exactly as they
    // are — this is how the list is printed, not what is in it, and the
    // two must not be confused: demoting them for real cost the gear a
    // phantom 120-fold ring and seven times its triangle count.
    let (mut fragments, mut fragment_area) = (0usize, 0.0);
    for feature in &report.features {
        if feature.is_recovered() && feature.area < FRAGMENT_AREA {
            fragments += 1;
            fragment_area += feature.area;
            continue;
        }
        let description = match &feature.surface {
            SurfaceClass::Plane(fit) => format!(
                "plane    normal ({:+.3} {:+.3} {:+.3}) offset {:+.3}",
                fit.normal.x,
                fit.normal.y,
                fit.normal.z,
                (fit.origin - Point3::default()).dot(fit.normal)
            ),
            SurfaceClass::Cylinder(fit) => format!(
                "cylinder diameter {:.3} axis ({:+.3} {:+.3} {:+.3}) through ({:+.2} {:+.2} {:+.2})",
                fit.radius * 2.0,
                fit.axis.x,
                fit.axis.y,
                fit.axis.z,
                fit.axis_point.x,
                fit.axis_point.y,
                fit.axis_point.z
            ),
            SurfaceClass::Sphere(fit) => format!(
                "sphere   diameter {:.3} center ({:+.2} {:+.2} {:+.2})",
                fit.radius * 2.0,
                fit.center.x,
                fit.center.y,
                fit.center.z
            ),
            SurfaceClass::Cone(fit) => format!(
                "cone     half-angle {:.2} deg apex ({:+.2} {:+.2} {:+.2})",
                fit.half_angle.to_degrees(),
                fit.apex.x,
                fit.apex.y,
                fit.apex.z
            ),
            SurfaceClass::Torus(fit) => format!(
                "torus    R {:.3} r {:.3} axis ({:+.3} {:+.3} {:+.3})",
                fit.major_radius, fit.minor_radius, fit.axis.x, fit.axis.y, fit.axis.z
            ),
            SurfaceClass::Blend(fit) => format!(
                "fillet   r {:.3} ring d {:.3} at z {:+.3}",
                fit.minor_radius,
                fit.major_radius * 2.0,
                fit.axis_point.z
            ),
            SurfaceClass::Pattern(fit) => format!(
                "pattern  {} instances about Z, fold rms {:.3} (worst instance {:.3})",
                fit.count, fit.deviation.rms, fit.worst_instance_rms
            ),
            SurfaceClass::EdgeRound(fit) => {
                format!("edge round, span {:.2} mm", fit.span)
            }
            SurfaceClass::Freeform => "freeform".to_owned(),
        };
        let rms = feature
            .surface
            .rms()
            .map_or(String::new(), |rms| format!("  rms {rms:.4}"));
        out.push_str(&format!(
            "  #{:<3} {description}  [{} faces, area {:.1}]{rms}\n",
            feature.id, feature.face_count, feature.area
        ));
        for note in &feature.notes {
            out.push_str(&format!("       - {note}\n"));
        }
    }
    if fragments > 0 {
        out.push_str(&format!(
            "  ... and {fragments} recovered fragment(s) under {FRAGMENT_AREA:.0} mm^2,              {fragment_area:.0} mm^2 in total (in the model, folded here)\n"
        ));
    }
    if let Some(plan) = &report.plan {
        out.push_str(&plan_summary(plan));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;
    use crate::transform::RigidTransform;
    use artificer_geometry::Vector3;

    #[test]
    fn tilted_scan_lands_in_its_datum_frame() {
        // A scan never arrives axis-aligned; the pipeline must recover the
        // part's own frame and snap against it.
        let pose = RigidTransform::from_axis_angle(Vector3::new(1.0, 0.5, 0.0), 0.4)
            .unwrap()
            .then(&RigidTransform::from_translation(Vector3::new(
                30.0, -10.0, 5.0,
            )));
        let tilted = synth::plate_with_boss().transformed(&pose);
        let report = reverse_engineer(&tilted, &ReverseOptions::default());
        assert!(report.datum.is_some());
        // No stage may lose surface area: features must tile the mesh.
        assert!(
            (report.total_area - tilted.surface_area()).abs() < 1e-6,
            "area not conserved: {} vs {}",
            report.total_area,
            tilted.surface_area()
        );
        let cylinders: Vec<_> = report
            .features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cylinder(fit) => Some(fit),
                _ => None,
            })
            .collect();
        assert_eq!(cylinders.len(), 1);
        // In the datum frame the boss axis snaps exactly onto Z, the axis
        // line onto the origin, and the diameter onto the grid.
        assert!((cylinders[0].axis.z.abs() - 1.0).abs() < 1e-12);
        assert!((cylinders[0].radius - 12.0).abs() < 1e-9);
        let level_planes = report
            .features
            .iter()
            .filter(|f| match &f.surface {
                SurfaceClass::Plane(fit) => fit.normal.z.abs() > 1.0 - 1e-9,
                _ => false,
            })
            .count();
        // Plate top and bottom plus the boss cap all face along Z.
        assert!(level_planes >= 3, "{level_planes} level planes");
    }

    #[test]
    fn plate_report_classifies_and_snaps() {
        let mesh = synth::plate_with_boss();
        let report = reverse_engineer(&mesh, &ReverseOptions::default());
        let cylinders: Vec<_> = report
            .features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cylinder(fit) => Some(fit),
                _ => None,
            })
            .collect();
        assert_eq!(cylinders.len(), 1);
        // Faceted at 96 segments, the shell's fitted radius sits just under
        // 12; snapping pulls the diameter onto the 0.5 mm grid at 24.0.
        assert!((cylinders[0].radius - 12.0).abs() < 0.05);
        assert!(cylinders[0].axis.z.abs() > 1.0 - 1e-9);
        assert!(report.classified_area / report.total_area > 0.95);
        let json = report_to_json(&report);
        assert!(json.contains("\"kind\":\"cylinder\""));
        assert!(json.contains("\"features\""));
    }
}

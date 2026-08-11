//! The end-to-end reverse-engineering pipeline and its report formats.

use artificer_geometry::Point3;

use artificer_geometry::Vector3;

use crate::datum::{DatumAlignment, auto_datum_alignment};
use crate::finalize::finalize_features;
use crate::merge::{absorb_into_anchors, merge_fragments};
use crate::mesh::TriangleMesh;
use crate::ransac::{RansacParams, extract_primitives};
use crate::reconstruct::{
    MasterProfile, PatternProposal, ReconstructionPlan, axis_lock_refit,
    detect_circular_pattern, extract_revolved_bands, plan_summary, plan_to_history_json,
    recognize_blends, recognize_pattern_feature, reconstruct,
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
    /// Canonicalization policy; `None` reports raw fitted values.
    pub snap: Option<SnapPolicy>,
    /// Complete the decomposition: claim on-surface faces, recognize edge
    /// rounds between features, collapse the rest into one residue record.
    pub finalize: bool,
}

impl Default for ReverseOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.05,
            segmentation: SegmentationParams::default(),
            ransac: Some(RansacParams::default()),
            merge_fragments: true,
            min_feature_area: 25.0,
            auto_datum: true,
            snap: Some(SnapPolicy::default()),
            finalize: true,
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

#[derive(Clone, Debug)]
pub struct ReverseReport {
    pub features: Vec<FeatureRecord>,
    pub total_area: f64,
    pub classified_area: f64,
    /// When auto-datum ran: the transform from scan coordinates into the
    /// datum frame that all reported features are expressed in.
    pub datum: Option<DatumAlignment>,
    /// When auto-datum ran: the revolved-profile reconstruction plan.
    pub plan: Option<ReconstructionPlan>,
}

/// Segments the mesh, fits analytic surfaces, and canonicalizes the result.
pub fn reverse_engineer(mesh: &TriangleMesh, options: &ReverseOptions) -> ReverseReport {
    let regions = segment(mesh, &options.segmentation);
    let mut features: Vec<FeatureRecord> = regions
        .into_iter()
        .enumerate()
        .map(|(id, region)| {
            let surface =
                classify_region(mesh, &region, options.tolerance, &options.segmentation);
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
        if r.epsilon > 0.0 { r.epsilon } else { options.tolerance }
    });
    if options.merge_fragments {
        features = merge_fragments(mesh, features, merge_epsilon);
        features = absorb_into_anchors(mesh, features, merge_epsilon);
    }
    let datum = if options.auto_datum {
        auto_datum_alignment(&features)
    } else {
        None
    };
    let mut detected_pattern: Option<PatternProposal> = None;
    let mut master_profile: Option<MasterProfile> = None;
    if let Some(alignment) = &datum {
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
        detected_pattern = detect_circular_pattern(mesh, &features, alignment);
        if let Some(pattern) = &detected_pattern {
            master_profile =
                recognize_pattern_feature(mesh, &mut features, alignment, pattern, options.tolerance);
        }
    }
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
        let mut surfaces: Vec<SurfaceClass> =
            features.iter().map(|f| f.surface.clone()).collect();
        for (feature, surface) in features.iter_mut().zip(surfaces.iter_mut()) {
            feature.notes.extend(snap_surface(surface, policy));
        }
        let harmonize_notes = harmonize_surfaces(&mut surfaces, policy);
        for ((feature, surface), notes) in
            features.iter_mut().zip(surfaces).zip(harmonize_notes)
        {
            feature.surface = surface;
            feature.notes.extend(notes);
        }
    }
    if options.finalize {
        finalize_features(mesh, &mut features, datum.as_ref(), options.tolerance);
        features.sort_by(|a, b| b.area.total_cmp(&a.area));
        for (id, feature) in features.iter_mut().enumerate() {
            feature.id = id;
        }
    }
    let total_area: f64 = features.iter().map(|f| f.area).sum();
    let classified_area: f64 = features
        .iter()
        .filter(|f| !matches!(f.surface, SurfaceClass::Freeform))
        .map(|f| f.area)
        .sum();
    let plan = datum
        .as_ref()
        .map(|alignment| {
            reconstruct(
                mesh,
                &features,
                alignment,
                options.tolerance,
                detected_pattern,
                master_profile.clone(),
            )
        });
    ReverseReport {
        features,
        total_area,
        classified_area,
        datum,
        plan,
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
        "],\"total_area\":{:.6},\"classified_area\":{:.6}",
        report.total_area, report.classified_area
    ));
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
    out.push_str(&format!(
        "{} region(s), {:.1}% of surface area classified\n",
        report.features.len(),
        if report.total_area > 0.0 {
            100.0 * report.classified_area / report.total_area
        } else {
            0.0
        }
    ));
    for feature in &report.features {
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
            .then(&RigidTransform::from_translation(Vector3::new(30.0, -10.0, 5.0)));
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

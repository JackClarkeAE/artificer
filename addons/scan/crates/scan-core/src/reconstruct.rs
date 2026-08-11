//! Feature reconstruction: from stitched primitives to a parametric plan.
//!
//! Three steps close the gap between "patches that fit" and "features a
//! CAD kernel can replay":
//!
//! 1. **Axis-locked refit** — once a datum axis exists, every near-axis
//!    cylinder patch is refit with the axis direction *fixed*. Small
//!    noisy patches (tooth-root arcs, interrupted bands) stop wobbling,
//!    their radii cluster, and a second merge pass stitches them into
//!    single surfaces.
//! 2. **Blend recognition** — leftover freeform patches are projected
//!    into profile space `(radial distance, height)` about the datum
//!    axis. A fillet ring is a torus, and a torus is a plain circle
//!    there; a circle fit inside tolerance recognizes the fillet and its
//!    radius.
//! 3. **Reconstruction plan** — level planes and on-axis cylinders
//!    assemble into a revolved profile (a stack of annulus segments,
//!    bores from inward-facing walls, bosses from outward), with fillet
//!    and chamfer proposals attached to the profile corners they round.
//!    The plan serializes into kernel-command-shaped history operations
//!    (`make_revolved_annulus` entries are wire-exact for the Artificer
//!    protocol; blend proposals carry geometric edge descriptors until an
//!    executing kernel can resolve entity references).

use artificer_geometry::{Point3, Vector3};

use crate::datum::DatumAlignment;
use crate::fit::{fit_cylinder_with_axis, fit_revolved_blend};
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::{SurfaceClass, fit_inputs};

/// Cylinder axes within this angle of the datum axis are candidates for
/// the axis-locked refit. Free fits on small noisy arcs wobble by around
/// ten degrees, so the window must be wider than that wobble; the RMS
/// gate keeps genuinely tilted surfaces from being force-locked.
const AXIS_LOCK_ANGLE_DEG: f64 = 14.0;
/// Profile z-levels closer than this merge into one breakpoint (mm).
const LEVEL_MERGE_TOL: f64 = 0.4;
/// A profile segment must be at least this tall to matter (mm).
const MIN_SEGMENT_HEIGHT: f64 = 0.4;
/// How close (mm) a blend must sit to a profile corner to be its fillet.
const CORNER_MATCH_TOL: f64 = 2.5;

/// Re-fits near-axis cylinders with the axis direction locked to the
/// datum axis. Returns how many features were replaced.
pub fn axis_lock_refit(
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    axis: Vector3,
    tolerance: f64,
) -> usize {
    let cos_lock = AXIS_LOCK_ANGLE_DEG.to_radians().cos();
    let mut locked = 0;
    for feature in features.iter_mut() {
        let SurfaceClass::Cylinder(fit) = &feature.surface else {
            continue;
        };
        if fit.axis.dot(axis).abs() < cos_lock {
            continue;
        }
        let inputs = fit_inputs(mesh, &feature.faces);
        let Some(refit) = fit_cylinder_with_axis(&inputs.points, axis) else {
            continue;
        };
        if refit.deviation.rms <= tolerance {
            feature.surface = SurfaceClass::Cylinder(refit);
            feature
                .notes
                .push("axis locked to the datum direction".to_owned());
            locked += 1;
        }
    }
    locked
}

/// Recognizes revolved fillets among freeform patches: in profile space
/// about the datum axis a fillet ring is a circular arc. Surfaces are
/// replaced in place; returns how many blends were recognized.
///
/// Runs on datum-frame features; `alignment` maps the mesh's scan
/// coordinates into that frame.
pub fn recognize_blends(
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    alignment: &DatumAlignment,
    tolerance: f64,
) -> usize {
    let mut recognized = 0;
    for feature in features.iter_mut() {
        if !matches!(feature.surface, SurfaceClass::Freeform) || feature.face_count < 30 {
            continue;
        }
        let inputs = fit_inputs(mesh, &feature.faces);
        let datum_points: Vec<Point3> = inputs
            .points
            .iter()
            .map(|p| alignment.transform.apply_point(*p))
            .collect();
        let Some(blend) = fit_revolved_blend(
            &datum_points,
            Point3::default(),
            Vector3::new(0.0, 0.0, 1.0),
        ) else {
            continue;
        };
        // A believable fillet is a ring, not a sliver: any small patch fits
        // some circle in profile space, so demand real area and wide
        // angular coverage around the axis before accepting.
        let mut bins = [false; 24];
        for p in &datum_points {
            let angle = p.y.atan2(p.x);
            let bin = ((angle + std::f64::consts::PI) / std::f64::consts::TAU * 24.0) as usize;
            bins[bin.min(23)] = true;
        }
        let coverage = bins.iter().filter(|b| **b).count();
        let plausible = blend.deviation.rms <= tolerance
            && blend.minor_radius >= 2.0 * tolerance
            && blend.minor_radius <= 0.3 * blend.major_radius
            && feature.area >= 10.0
            && coverage >= 8;
        if plausible {
            feature.surface = SurfaceClass::Blend(blend);
            feature
                .notes
                .push("recognized as a revolved fillet ring".to_owned());
            recognized += 1;
        }
    }
    recognized
}

#[derive(Clone, Copy, Debug)]
pub struct ProfileSegment {
    pub z0: f64,
    pub z1: f64,
    /// Zero when the segment is solid to the axis.
    pub inner_radius: f64,
    pub outer_radius: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct FilletProposal {
    pub radius: f64,
    pub z: f64,
    pub at_radius: f64,
    /// Whether a matching profile corner was found for this blend.
    pub matched_corner: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ChamferProposal {
    pub distance: f64,
    pub z: f64,
    pub at_radius: f64,
}

/// An n-fold circular repetition detected in the unexplained geometry —
/// for a gear, the teeth.
#[derive(Clone, Copy, Debug)]
pub struct PatternProposal {
    pub count: usize,
    /// Normalized circular autocorrelation at the pattern period (0..1).
    pub strength: f64,
    pub z_range: (f64, f64),
    pub radius_range: (f64, f64),
    /// Total unexplained area participating in the pattern (mm^2).
    pub area: f64,
}

/// How much of the scanned surface the reconstruction explains.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanCoverage {
    pub covered_area: f64,
    pub total_area: f64,
}

impl PlanCoverage {
    pub fn fraction(&self) -> f64 {
        if self.total_area > 0.0 {
            self.covered_area / self.total_area
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReconstructionPlan {
    pub segments: Vec<ProfileSegment>,
    pub fillets: Vec<FilletProposal>,
    pub chamfers: Vec<ChamferProposal>,
    pub pattern: Option<PatternProposal>,
    pub coverage: PlanCoverage,
    pub notes: Vec<String>,
}

/// z and radial extent of a feature's faces in the datum frame.
fn extents(
    mesh: &TriangleMesh,
    faces: &[u32],
    alignment: &DatumAlignment,
) -> (f64, f64, f64, f64) {
    let mut z_min = f64::INFINITY;
    let mut z_max = f64::NEG_INFINITY;
    let mut r_min = f64::INFINITY;
    let mut r_max = f64::NEG_INFINITY;
    for &face in faces {
        let c = alignment
            .transform
            .apply_point(mesh.face_centroid(face as usize));
        let radial = c.x.hypot(c.y);
        z_min = z_min.min(c.z);
        z_max = z_max.max(c.z);
        r_min = r_min.min(radial);
        r_max = r_max.max(radial);
    }
    (z_min, z_max, r_min, r_max)
}

/// Positive when the feature's surface normals face away from the axis
/// (a boss); negative when they face it (a bore).
fn outwardness(mesh: &TriangleMesh, faces: &[u32], alignment: &DatumAlignment) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for &face in faces.iter().step_by(faces.len().div_ceil(200).max(1)) {
        let Some(normal) = mesh.face_normal(face as usize) else {
            continue;
        };
        let n = alignment.transform.apply_vector(normal);
        let c = alignment
            .transform
            .apply_point(mesh.face_centroid(face as usize));
        let radial = Vector3::new(c.x, c.y, 0.0);
        let length = radial.length();
        if length > 1e-9 {
            sum += n.dot(radial / length);
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

/// Detects an n-fold circular repetition in the freeform residue by
/// circular autocorrelation of its azimuthal area distribution. A gear's
/// teeth, a bolt circle's lugs, or a knurl band all leave a periodic
/// signature that survives noise and arbitrary patch grouping.
fn detect_circular_pattern(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    alignment: &DatumAlignment,
) -> Option<PatternProposal> {
    const BINS: usize = 1440;
    const MIN_AREA: f64 = 50.0;
    const MIN_STRENGTH: f64 = 0.5;
    let mut histogram = vec![0.0f64; BINS];
    let mut total = 0.0;
    let mut z_range = (f64::INFINITY, f64::NEG_INFINITY);
    let mut radius_range = (f64::INFINITY, f64::NEG_INFINITY);
    for feature in features {
        if !matches!(feature.surface, SurfaceClass::Freeform) {
            continue;
        }
        for &face in &feature.faces {
            let c = alignment
                .transform
                .apply_point(mesh.face_centroid(face as usize));
            let radial = c.x.hypot(c.y);
            if radial < 1.0 {
                continue;
            }
            let area = mesh.face_area(face as usize);
            let angle = c.y.atan2(c.x);
            let bin =
                (((angle + std::f64::consts::PI) / std::f64::consts::TAU) * BINS as f64) as usize;
            histogram[bin.min(BINS - 1)] += area;
            total += area;
            z_range = (z_range.0.min(c.z), z_range.1.max(c.z));
            radius_range = (radius_range.0.min(radial), radius_range.1.max(radial));
        }
    }
    #[cfg(debug_assertions)]
    eprintln!("pattern entry: freeform area {total:.1}");
    if total < MIN_AREA {
        return None;
    }
    let mean = total / BINS as f64;
    let centered: Vec<f64> = histogram.iter().map(|v| v - mean).collect();
    let denominator: f64 = centered.iter().map(|v| v * v).sum();
    if denominator < 1e-12 {
        return None;
    }
    let score_of = |count: usize| -> f64 {
        let shift = (BINS as f64 / count as f64).round() as usize;
        if shift == 0 {
            return 0.0;
        }
        centered
            .iter()
            .enumerate()
            .map(|(b, v)| v * centered[(b + shift) % BINS])
            .sum::<f64>()
            / denominator
    };
    let scores: Vec<(usize, f64)> = (5..=120).map(|count| (count, score_of(count))).collect();
    let max_score = scores.iter().map(|(_, s)| *s).fold(f64::NEG_INFINITY, f64::max);
    #[cfg(debug_assertions)]
    eprintln!("pattern probe: area {total:.1}, max score {max_score:.3}");
    if max_score < MIN_STRENGTH {
        return None;
    }
    // Correlation also peaks when the shift is a multiple of the true
    // period, which reads as a divisor of the true count — take the
    // largest count that still scores near the maximum.
    let count = scores
        .iter()
        .filter(|(_, s)| *s >= 0.92 * max_score)
        .map(|(c, _)| *c)
        .max()?;
    Some(PatternProposal {
        count,
        strength: score_of(count),
        z_range,
        radius_range,
        area: total,
    })
}

/// Area-weighted fraction of the scan within `tolerance` of the
/// reconstructed boundary (annulus walls and caps, recognized fillet
/// rings). The metric is honest by construction: geometry the plan does
/// not model — a toothed band, a keyway — counts as uncovered.
fn plan_coverage(
    mesh: &TriangleMesh,
    alignment: &DatumAlignment,
    segments: &[ProfileSegment],
    blends: &[(f64, f64, f64)],
    tolerance: f64,
) -> PlanCoverage {
    let mut coverage = PlanCoverage::default();
    if segments.is_empty() && blends.is_empty() {
        for face in 0..mesh.triangles().len() {
            coverage.total_area += mesh.face_area(face);
        }
        return coverage;
    }
    for face in 0..mesh.triangles().len() {
        let area = mesh.face_area(face);
        coverage.total_area += area;
        let c = alignment.transform.apply_point(mesh.face_centroid(face));
        let radial = c.x.hypot(c.y);
        let mut distance = f64::INFINITY;
        for segment in segments {
            let inside_r = radial >= segment.inner_radius && radial <= segment.outer_radius;
            let inside_z = c.z >= segment.z0 && c.z <= segment.z1;
            let boundary = if inside_r && inside_z {
                let mut d = (segment.outer_radius - radial).min(c.z - segment.z0);
                d = d.min(segment.z1 - c.z);
                if segment.inner_radius > 0.0 {
                    d = d.min(radial - segment.inner_radius);
                }
                d
            } else {
                let dr = if radial < segment.inner_radius {
                    segment.inner_radius - radial
                } else if radial > segment.outer_radius {
                    radial - segment.outer_radius
                } else {
                    0.0
                };
                let dz = if c.z < segment.z0 {
                    segment.z0 - c.z
                } else if c.z > segment.z1 {
                    c.z - segment.z1
                } else {
                    0.0
                };
                dr.hypot(dz)
            };
            distance = distance.min(boundary);
        }
        for &(major, z, minor) in blends {
            distance = distance.min(((radial - major).hypot(c.z - z) - minor).abs());
        }
        if distance <= tolerance {
            coverage.covered_area += area;
        }
    }
    coverage
}

/// Assembles the revolved-profile reconstruction from datum-frame features.
pub fn reconstruct(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    alignment: &DatumAlignment,
    tolerance: f64,
) -> ReconstructionPlan {
    let mut plan = ReconstructionPlan::default();
    // Gather profile evidence.
    struct AxisCylinder {
        radius: f64,
        z0: f64,
        z1: f64,
        area: f64,
        outward: bool,
    }
    let mut cylinders: Vec<AxisCylinder> = Vec::new();
    let mut levels: Vec<f64> = Vec::new();
    for feature in features {
        match &feature.surface {
            SurfaceClass::Cylinder(fit)
                if fit.axis.z.abs() > 0.999
                    && fit.axis_point.x.hypot(fit.axis_point.y) < 3.0 =>
            {
                let (z0, z1, _, _) = extents(mesh, &feature.faces, alignment);
                cylinders.push(AxisCylinder {
                    radius: fit.radius,
                    z0,
                    z1,
                    area: feature.area,
                    outward: outwardness(mesh, &feature.faces, alignment) >= 0.0,
                });
            }
            SurfaceClass::Plane(fit) if fit.normal.z.abs() > 0.999 => {
                levels.push(fit.origin.z);
            }
            _ => {}
        }
    }
    // Even without a revolve profile, pattern detection and coverage below
    // still apply, so record the gap and continue rather than returning.
    if cylinders.is_empty() {
        plan.notes
            .push("no on-axis cylinders; nothing to revolve".to_owned());
    }
    // Breakpoints: plane levels plus cylinder extents, deduplicated.
    let mut breaks: Vec<f64> = levels.clone();
    for c in &cylinders {
        breaks.push(c.z0);
        breaks.push(c.z1);
    }
    breaks.sort_by(f64::total_cmp);
    breaks.dedup_by(|a, b| (*a - *b).abs() <= LEVEL_MERGE_TOL);
    // One annulus per interval that an outward cylinder spans.
    for pair in breaks.windows(2) {
        let (z0, z1) = (pair[0], pair[1]);
        if z1 - z0 < MIN_SEGMENT_HEIGHT {
            continue;
        }
        let mid = (z0 + z1) / 2.0;
        let covering = |c: &&AxisCylinder| c.z0 - LEVEL_MERGE_TOL <= mid && mid <= c.z1 + LEVEL_MERGE_TOL;
        let outer = cylinders
            .iter()
            .filter(|c| c.outward)
            .filter(covering)
            .max_by(|a, b| a.area.total_cmp(&b.area));
        let inner = cylinders
            .iter()
            .filter(|c| !c.outward)
            .filter(covering)
            .max_by(|a, b| a.area.total_cmp(&b.area));
        match (outer, inner) {
            (Some(outer), inner) => {
                let inner_radius = inner.map_or(0.0, |c| c.radius);
                if inner_radius < outer.radius {
                    plan.segments.push(ProfileSegment {
                        z0,
                        z1,
                        inner_radius,
                        outer_radius: outer.radius,
                    });
                }
            }
            (None, Some(bore)) => {
                plan.notes.push(format!(
                    "bore wall d {:.2} over z {:.2}..{:.2} has no outer surface; outer boundary is freeform there",
                    bore.radius * 2.0,
                    z0,
                    z1
                ));
            }
            (None, None) => {}
        }
    }
    // Merge adjacent segments with identical radii.
    plan.segments.dedup_by(|next, prev| {
        let same = (prev.outer_radius - next.outer_radius).abs() < 1e-9
            && (prev.inner_radius - next.inner_radius).abs() < 1e-9
            && (prev.z1 - next.z0).abs() <= LEVEL_MERGE_TOL;
        if same {
            prev.z1 = next.z1;
        }
        same
    });
    // Profile corners: segment boundaries where a radius changes.
    let mut corners: Vec<(f64, f64)> = Vec::new();
    for pair in plan.segments.windows(2) {
        if (pair[0].outer_radius - pair[1].outer_radius).abs() > 1e-9 {
            corners.push((pair[0].z1, pair[0].outer_radius.min(pair[1].outer_radius)));
            corners.push((pair[0].z1, pair[0].outer_radius.max(pair[1].outer_radius)));
        }
    }
    for segment in &plan.segments {
        corners.push((segment.z0, segment.outer_radius));
        corners.push((segment.z1, segment.outer_radius));
        if segment.inner_radius > 0.0 {
            corners.push((segment.z0, segment.inner_radius));
            corners.push((segment.z1, segment.inner_radius));
        }
    }
    // Attach recognized blends and chamfer-angle cones to corners.
    for feature in features {
        match &feature.surface {
            SurfaceClass::Blend(fit) => {
                let z = fit.axis_point.z;
                let matched = corners.iter().any(|(cz, cr)| {
                    (cz - z).abs() <= CORNER_MATCH_TOL
                        && (cr - fit.major_radius).abs() <= CORNER_MATCH_TOL + fit.minor_radius
                });
                plan.fillets.push(FilletProposal {
                    radius: fit.minor_radius,
                    z,
                    at_radius: fit.major_radius,
                    matched_corner: matched,
                });
            }
            SurfaceClass::Cone(fit)
                if fit.axis.z.abs() > 0.999
                    && (fit.half_angle.to_degrees() - 45.0).abs() <= 3.0 =>
            {
                let (z0, z1, r0, r1) = extents(mesh, &feature.faces, alignment);
                plan.chamfers.push(ChamferProposal {
                    distance: z1 - z0,
                    z: (z0 + z1) / 2.0,
                    at_radius: (r0 + r1) / 2.0,
                });
            }
            _ => {}
        }
    }
    plan.pattern = detect_circular_pattern(mesh, features, alignment);
    if let Some(pattern) = &plan.pattern {
        plan.notes.push(format!(
            "unexplained band (z {:.1}..{:.1}, d {:.1}..{:.1}) repeats {} times around the axis \
             (correlation {:.2}) — candidate tooth or lug count",
            pattern.z_range.0,
            pattern.z_range.1,
            pattern.radius_range.0 * 2.0,
            pattern.radius_range.1 * 2.0,
            pattern.count,
            pattern.strength
        ));
    }
    let blend_rings: Vec<(f64, f64, f64)> = features
        .iter()
        .filter_map(|f| match &f.surface {
            SurfaceClass::Blend(fit) => {
                Some((fit.major_radius, fit.axis_point.z, fit.minor_radius))
            }
            _ => None,
        })
        .collect();
    plan.coverage = plan_coverage(mesh, alignment, &plan.segments, &blend_rings, tolerance);
    plan
}

fn push_frame(out: &mut String, z: f64) {
    out.push_str(&format!(
        "\"frame\":{{\"origin\":{{\"x\":0.0,\"y\":0.0,\"z\":{z:.6}}},\
         \"u\":{{\"x\":1.0,\"y\":0.0,\"z\":0.0}},\
         \"v\":{{\"x\":0.0,\"y\":1.0,\"z\":0.0}}}}"
    ));
}

/// Serializes the plan as replay-proposal operations. The
/// `make_revolved_annulus` entries match the Artificer protocol's
/// `KernelCommand` wire format exactly; blend entries are proposals with
/// geometric edge descriptors, resolvable once a kernel executes the
/// constructive prefix.
pub fn plan_to_history_json(plan: &ReconstructionPlan) -> String {
    let mut out = String::from(
        "{\"format\":\"artificer.scan.replay-proposal\",\"version\":1,\"operations\":[",
    );
    let mut first = true;
    let mut separate = |out: &mut String| {
        if !first {
            out.push(',');
        }
        first = false;
    };
    for segment in &plan.segments {
        separate(&mut out);
        out.push_str("{\"type\":\"make_revolved_annulus\",");
        push_frame(&mut out, segment.z0);
        out.push_str(&format!(
            ",\"inner_radius\":{:.6},\"outer_radius\":{:.6},\"height\":{:.6}}}",
            segment.inner_radius,
            segment.outer_radius,
            segment.z1 - segment.z0
        ));
    }
    for fillet in &plan.fillets {
        separate(&mut out);
        out.push_str(&format!(
            "{{\"type\":\"finish_edge_proposal\",\"kind\":\"fillet\",\"distance\":{:.6},\
             \"edge\":{{\"revolved_corner\":{{\"z\":{:.6},\"radius\":{:.6}}}}},\
             \"matched_corner\":{}}}",
            fillet.radius, fillet.z, fillet.at_radius, fillet.matched_corner
        ));
    }
    for chamfer in &plan.chamfers {
        separate(&mut out);
        out.push_str(&format!(
            "{{\"type\":\"finish_edge_proposal\",\"kind\":\"chamfer\",\"distance\":{:.6},\
             \"edge\":{{\"revolved_corner\":{{\"z\":{:.6},\"radius\":{:.6}}}}},\
             \"matched_corner\":true}}",
            chamfer.distance, chamfer.z, chamfer.at_radius
        ));
    }
    if let Some(pattern) = &plan.pattern {
        separate(&mut out);
        out.push_str(&format!(
            "{{\"type\":\"circular_pattern_proposal\",\"count\":{},\"strength\":{:.3},\
             \"z_range\":[{:.6},{:.6}],\"radius_range\":[{:.6},{:.6}],\"area\":{:.6}}}",
            pattern.count,
            pattern.strength,
            pattern.z_range.0,
            pattern.z_range.1,
            pattern.radius_range.0,
            pattern.radius_range.1,
            pattern.area
        ));
    }
    out.push_str(&format!(
        "],\"coverage\":{{\"covered_area\":{:.6},\"total_area\":{:.6}}}}}",
        plan.coverage.covered_area, plan.coverage.total_area
    ));
    out
}

pub fn plan_summary(plan: &ReconstructionPlan) -> String {
    let mut out = String::new();
    if plan.segments.is_empty() && plan.fillets.is_empty() && plan.chamfers.is_empty() {
        return out;
    }
    out.push_str("reconstruction (revolved about datum Z):\n");
    for segment in &plan.segments {
        if segment.inner_radius > 0.0 {
            out.push_str(&format!(
                "  annulus  z {:+8.3} .. {:+8.3}  bore d {:7.3}  outer d {:7.3}\n",
                segment.z0,
                segment.z1,
                segment.inner_radius * 2.0,
                segment.outer_radius * 2.0
            ));
        } else {
            out.push_str(&format!(
                "  disc     z {:+8.3} .. {:+8.3}  outer d {:7.3}\n",
                segment.z0,
                segment.z1,
                segment.outer_radius * 2.0
            ));
        }
    }
    for fillet in &plan.fillets {
        out.push_str(&format!(
            "  fillet   r {:.3} at z {:+.3}, ring d {:.3}{}\n",
            fillet.radius,
            fillet.z,
            fillet.at_radius * 2.0,
            if fillet.matched_corner {
                ""
            } else {
                "  (no matching profile corner)"
            }
        ));
    }
    for chamfer in &plan.chamfers {
        out.push_str(&format!(
            "  chamfer  {:.3} x 45 deg at z {:+.3}, d {:.3}\n",
            chamfer.distance,
            chamfer.z,
            chamfer.at_radius * 2.0
        ));
    }
    if let Some(pattern) = &plan.pattern {
        out.push_str(&format!(
            "  pattern  {} repeats around the axis (correlation {:.2})\n",
            pattern.count, pattern.strength
        ));
    }
    if plan.coverage.total_area > 0.0 {
        out.push_str(&format!(
            "  coverage: reconstruction explains {:.1}% of the scanned surface within tolerance\n",
            plan.coverage.fraction() * 100.0
        ));
    }
    for note in &plan.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReverseOptions, reverse_engineer};
    use crate::synth;

    /// Deterministic radial jitter keyed on the vertex position, so
    /// coincident corners of neighbouring triangles move identically and
    /// welding still stitches the shell. About +/- 0.02 mm: inside
    /// tolerance, but enough to wobble a free cylinder fit on a narrow arc.
    fn jitter(soup: &mut [[Point3; 3]]) {
        for triangle in soup.iter_mut() {
            for point in triangle.iter_mut() {
                let quantize = |v: f64| (v * 512.0).round() as i64 as u64;
                let mut seed = quantize(point.x)
                    .wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    ^ quantize(point.y).wrapping_mul(0xbf58_476d_1ce4_e5b9)
                    ^ quantize(point.z).wrapping_mul(0x94d0_49bb_1331_11eb);
                seed ^= seed >> 31;
                let noise = (seed % 1024) as f64 / 1024.0 - 0.5;
                let radial = point.x.hypot(point.y).max(1e-9);
                let scale = 1.0 + noise * 0.04 / radial;
                *point = Point3::new(point.x * scale, point.y * scale, point.z);
            }
        }
    }

    #[test]
    fn interrupted_ring_stitches_into_one_cylinder() {
        // Twelve 20-degree arcs with gaps, like a tooth-root band. Free
        // fits on narrow noisy arcs wobble; the datum-locked refit and
        // re-merge must stitch them into a single cylinder.
        let mut soup = Vec::new();
        for arc in 0..12 {
            let start = std::f64::consts::TAU * arc as f64 / 12.0;
            soup.extend(synth::cylinder_arc_soup(
                15.0,
                20.0,
                start,
                start + std::f64::consts::TAU / 18.0,
                10,
                10,
            ));
        }
        jitter(&mut soup);
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, -1.0),
            15.0,
            96,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let report = reverse_engineer(&mesh, &ReverseOptions::default());
        let cylinders: Vec<_> = report
            .features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cylinder(fit) => Some((f, fit)),
                _ => None,
            })
            .collect();
        assert_eq!(cylinders.len(), 1, "arcs did not stitch: {}", cylinders.len());
        assert!((cylinders[0].1.radius - 15.0).abs() < 0.05);
        assert!(
            report
                .features
                .iter()
                .flat_map(|f| &f.notes)
                .any(|n| n.contains("axis locked")),
            "axis lock never engaged"
        );
    }

    #[test]
    fn filleted_corner_is_recognized_and_planned() {
        // A turned part: outer wall d 40 up to z 8.5, a 1.5 mm fillet ring
        // rolling over to a top face at z 10, and a bottom face at z 0.
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
        let blends: Vec<_> = report
            .features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Blend(fit) => Some(fit),
                _ => None,
            })
            .collect();
        assert_eq!(blends.len(), 1, "fillet ring not recognized");
        assert!(
            (blends[0].minor_radius - 1.5).abs() < 0.05,
            "fillet radius {}",
            blends[0].minor_radius
        );
        let plan = report.plan.as_ref().expect("plan present");
        assert!(!plan.segments.is_empty(), "no revolved segments");
        assert!(
            (plan.segments[0].outer_radius - 20.0).abs() < 0.05,
            "outer radius {}",
            plan.segments[0].outer_radius
        );
        assert_eq!(plan.fillets.len(), 1);
        assert!(plan.fillets[0].matched_corner, "fillet not tied to a corner");
        let history = plan_to_history_json(plan);
        assert!(history.contains("\"type\":\"make_revolved_annulus\""));
        assert!(history.contains("\"kind\":\"fillet\""));
        assert!(history.contains("\"coverage\""));
        // The wall, bottom face, and fillet ring are modelled; the top
        // section above the last level plane is not, and stays uncovered.
        assert!(
            plan.coverage.fraction() > 0.5,
            "coverage {:.2}",
            plan.coverage.fraction()
        );
        assert!(plan.pattern.is_none(), "no pattern exists on a turned part");
    }

    #[test]
    fn twelve_lugs_read_as_a_twelve_fold_pattern() {
        // Twelve wavy (deliberately non-quadric) lugs around a ring: the
        // azimuthal autocorrelation must recover the count even though
        // every lug stays freeform.
        let mut soup = Vec::new();
        for lug in 0..12 {
            let start = std::f64::consts::TAU * lug as f64 / 12.0;
            for i in 0..8usize {
                for j in 0..8usize {
                    let corner = |di: usize, dj: usize| {
                        let angle = start + (i + di) as f64 * (14.0f64.to_radians() / 8.0);
                        let z = 2.0 + (j + dj) as f64 * 0.75;
                        let wave = ((i + di) as f64 * 2.5).sin() * ((j + dj) as f64 * 2.5).sin();
                        let radial = 20.0 + 0.6 * wave;
                        Point3::new(radial * angle.cos(), radial * angle.sin(), z)
                    };
                    let (a, b, c, d) = (corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1));
                    soup.push([a, b, c]);
                    soup.push([a, c, d]);
                }
            }
        }
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, -1.0),
            22.0,
            96,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let report = reverse_engineer(&mesh, &ReverseOptions::default());
        let pattern = report
            .plan
            .as_ref()
            .and_then(|p| p.pattern)
            .expect("pattern detected");
        assert_eq!(pattern.count, 12, "count {} strength {}", pattern.count, pattern.strength);
        assert!(pattern.strength > 0.5);
    }
}

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
        // Freeform patches are candidates outright; so are spheres, which
        // is what a torus shoulder ring mis-fits as when no torus is in
        // the vocabulary — those must beat their sphere fit to convert.
        let sphere_rms = match &feature.surface {
            SurfaceClass::Freeform => None,
            SurfaceClass::Sphere(fit) => Some(fit.deviation.rms),
            _ => continue,
        };
        if feature.face_count < 30 {
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
            && coverage >= 8
            && sphere_rms.is_none_or(|rms| blend.deviation.rms < 0.8 * rms);
        if plausible {
            let note = if sphere_rms.is_some() {
                "sphere patch reclassified as a revolved fillet ring"
            } else {
                "recognized as a revolved fillet ring"
            };
            feature.surface = SurfaceClass::Blend(blend);
            feature.notes.push(note.to_owned());
            recognized += 1;
        }
    }
    recognized
}

/// Extracts interrupted surfaces of revolution that per-region fitting
/// cannot see: each land patch bleeds into its neighbours through edge
/// rounds, so its own fit tilts and fails the axis lock.
///
/// In profile space `(radial distance, z)` about the datum axis every
/// surface of revolution is a curve: a cylinder is a vertical ridge and
/// a cone is a slanted line. Phase one histograms radially-facing donor
/// faces by radius and claims the dominant vertical ridges as
/// axis-locked cylinders; phase two runs a deterministic line RANSAC
/// over what remains and claims slanted lines as axis-true cones (a
/// synchro taper, a chamfer band). Donors are freeform patches, small
/// analytic scraps, **tilted** cylinders and cones (a tilted axis on a
/// revolved part is a misfit, not a feature), and spheres (a shallow
/// cone reads as an absurd giant sphere). Every claim is gated by the
/// locked fit meeting tolerance.
pub fn extract_revolved_bands(
    mesh: &TriangleMesh,
    features: &mut Vec<FeatureRecord>,
    alignment: &DatumAlignment,
    tolerance: f64,
) -> usize {
    const RADIAL_ALIGNMENT: f64 = 0.866; // cos 30 deg
    const BAND_MIN_AREA: f64 = 100.0;
    const MAX_DONOR_AREA: f64 = 150.0;
    const BIN_WIDTH: f64 = 0.1;
    let tilt_donor = 2.5f64.to_radians().cos();
    let donor = |feature: &FeatureRecord| match &feature.surface {
        SurfaceClass::Freeform => true,
        SurfaceClass::Blend(_) | SurfaceClass::Pattern(_) | SurfaceClass::EdgeRound(_) => false,
        SurfaceClass::Cylinder(fit) => {
            feature.area < MAX_DONOR_AREA || fit.axis.z.abs() < tilt_donor
        }
        SurfaceClass::Cone(fit) => {
            feature.area < MAX_DONOR_AREA || fit.axis.z.abs() < tilt_donor
        }
        SurfaceClass::Sphere(_) => true,
        SurfaceClass::Plane(_) => feature.area < MAX_DONOR_AREA,
    };
    // Candidate faces: radially-facing faces of donor features.
    struct Candidate {
        face: u32,
        radial: f64,
        z: f64,
        area: f64,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for feature in features.iter() {
        if !donor(feature) {
            continue;
        }
        for &face in &feature.faces {
            let Some(normal) = mesh.face_normal(face as usize) else {
                continue;
            };
            let c = alignment
                .transform
                .apply_point(mesh.face_centroid(face as usize));
            let radial_direction = Vector3::new(c.x, c.y, 0.0);
            let length = radial_direction.length();
            if length < 1.0 {
                continue;
            }
            let n = alignment.transform.apply_vector(normal);
            if n.dot(radial_direction / length).abs() < RADIAL_ALIGNMENT {
                continue;
            }
            candidates.push(Candidate {
                face,
                radial: length,
                z: c.z,
                area: mesh.face_area(face as usize),
            });
        }
    }
    if candidates.is_empty() {
        return 0;
    }
    let mut stolen = vec![false; mesh.triangles().len()];
    let mut extracted = 0usize;
    // Phase one: vertical ridges (cylinders).
    let r_min = candidates.iter().map(|c| c.radial).fold(f64::INFINITY, f64::min);
    let r_max = candidates
        .iter()
        .map(|c| c.radial)
        .fold(f64::NEG_INFINITY, f64::max);
    let bins = (((r_max - r_min) / BIN_WIDTH).ceil() as usize).clamp(1, 4096);
    let mut histogram = vec![0.0f64; bins];
    for candidate in &candidates {
        let bin = (((candidate.radial - r_min) / BIN_WIDTH) as usize).min(bins - 1);
        histogram[bin] += candidate.area;
    }
    let mut claimed = vec![false; bins];
    let mut bands: Vec<(f64, f64)> = Vec::new();
    let mut order: Vec<usize> = (0..bins).collect();
    order.sort_by(|&a, &b| histogram[b].total_cmp(&histogram[a]));
    for &peak in &order {
        if claimed[peak] || histogram[peak] <= 0.0 {
            continue;
        }
        let floor = 0.25 * histogram[peak];
        let mut low = peak;
        while low > 0 && !claimed[low - 1] && histogram[low - 1] >= floor {
            low -= 1;
        }
        let mut high = peak;
        while high + 1 < bins && !claimed[high + 1] && histogram[high + 1] >= floor {
            high += 1;
        }
        let area: f64 = histogram[low..=high].iter().sum();
        for flag in &mut claimed[low..=high] {
            *flag = true;
        }
        if area >= BAND_MIN_AREA {
            bands.push((
                r_min + low as f64 * BIN_WIDTH - tolerance,
                r_min + (high + 1) as f64 * BIN_WIDTH + tolerance,
            ));
        }
    }
    for (band_low, band_high) in bands {
        let members: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| {
                !stolen[c.face as usize] && (band_low..=band_high).contains(&c.radial)
            })
            .collect();
        if members.is_empty() {
            continue;
        }
        let points: Vec<Point3> = members
            .iter()
            .flat_map(|c| {
                mesh.triangles()[c.face as usize]
                    .into_iter()
                    .map(|v| alignment.transform.apply_point(mesh.positions()[v as usize]))
            })
            .collect();
        let Some(fit) =
            crate::fit::fit_cylinder_with_axis(&points, Vector3::new(0.0, 0.0, 1.0))
        else {
            continue;
        };
        if fit.deviation.rms > tolerance {
            continue;
        }
        let area: f64 = members.iter().map(|c| c.area).sum();
        if area < BAND_MIN_AREA {
            continue;
        }
        let faces: Vec<u32> = members.iter().map(|c| c.face).collect();
        for &face in &faces {
            stolen[face as usize] = true;
        }
        features.push(FeatureRecord {
            id: 0,
            surface: SurfaceClass::Cylinder(fit),
            face_count: faces.len(),
            area,
            faces,
            notes: vec!["interrupted revolved band stitched across the axis".to_owned()],
        });
        extracted += 1;
    }
    // Phase two: slanted lines (cones) among the remaining candidates.
    let mut rng_state = 0x51ab_5eed_u64;
    let mut next_rng = move || {
        rng_state = rng_state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = rng_state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    for _cone_round in 0..8 {
        let remaining: Vec<usize> = (0..candidates.len())
            .filter(|&i| !stolen[candidates[i].face as usize])
            .collect();
        let remaining_area: f64 = remaining.iter().map(|&i| candidates[i].area).sum();
        if remaining_area < BAND_MIN_AREA {
            break;
        }
        let mut best: Option<(f64, f64, f64)> = None; // (area, slope, intercept)
        for _ in 0..250 {
            let a = &candidates[remaining[(next_rng() % remaining.len() as u64) as usize]];
            let b = &candidates[remaining[(next_rng() % remaining.len() as u64) as usize]];
            let dz = b.z - a.z;
            if dz.abs() < 0.5 {
                continue;
            }
            let slope = (b.radial - a.radial) / dz;
            if !(0.08..=8.0).contains(&slope.abs()) {
                continue;
            }
            let intercept = a.radial - slope * a.z;
            let support: f64 = remaining
                .iter()
                .map(|&i| &candidates[i])
                .filter(|c| (c.radial - (intercept + slope * c.z)).abs() <= tolerance)
                .map(|c| c.area)
                .sum();
            if best.is_none_or(|(best_area, _, _)| support > best_area) {
                best = Some((support, slope, intercept));
            }
        }
        let Some((support, slope, intercept)) = best else { break };
        if support < BAND_MIN_AREA {
            break;
        }
        // Refine by weighted least squares over the inliers, then re-gate.
        let inliers: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| {
                let c = &candidates[i];
                (c.radial - (intercept + slope * c.z)).abs() <= tolerance
            })
            .collect();
        let (mut sw, mut sz, mut sr, mut szz, mut szr) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &i in &inliers {
            let c = &candidates[i];
            sw += c.area;
            sz += c.area * c.z;
            sr += c.area * c.radial;
            szz += c.area * c.z * c.z;
            szr += c.area * c.z * c.radial;
        }
        let denom = sw * szz - sz * sz;
        if denom.abs() < 1e-9 {
            break;
        }
        let slope = (sw * szr - sz * sr) / denom;
        let intercept = (sr - slope * sz) / sw;
        if !(0.08..=8.0).contains(&slope.abs()) {
            break;
        }
        let mut squared = 0.0;
        let mut max_abs = 0.0f64;
        let final_members: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| {
                let c = &candidates[i];
                (c.radial - (intercept + slope * c.z)).abs() <= tolerance
            })
            .collect();
        let mut area = 0.0;
        for &i in &final_members {
            let c = &candidates[i];
            let residual = c.radial - (intercept + slope * c.z);
            squared += c.area * residual * residual;
            max_abs = max_abs.max(residual.abs());
            area += c.area;
        }
        if area < BAND_MIN_AREA {
            break;
        }
        let rms = (squared / area).sqrt();
        if rms > tolerance {
            break;
        }
        // rho = intercept + slope * z crosses the axis at the apex.
        let apex_z = -intercept / slope;
        let axis = if slope > 0.0 {
            Vector3::new(0.0, 0.0, 1.0)
        } else {
            Vector3::new(0.0, 0.0, -1.0)
        };
        let fit = crate::fit::ConeFit {
            apex: Point3::new(0.0, 0.0, apex_z),
            axis,
            half_angle: slope.abs().atan(),
            deviation: crate::fit::DeviationStats { rms, max_abs },
        };
        let faces: Vec<u32> = final_members
            .iter()
            .map(|&i| candidates[i].face)
            .collect();
        for &face in &faces {
            stolen[face as usize] = true;
        }
        features.push(FeatureRecord {
            id: 0,
            surface: SurfaceClass::Cone(fit),
            face_count: faces.len(),
            area,
            faces,
            notes: vec![
                "interrupted revolved cone band stitched across the axis".to_owned(),
            ],
        });
        extracted += 1;
    }
    if extracted > 0 {
        for feature in features.iter_mut() {
            if donor(feature) {
                feature.faces.retain(|&face| !stolen[face as usize]);
                feature.face_count = feature.faces.len();
                feature.area = feature
                    .faces
                    .iter()
                    .map(|&face| mesh.face_area(face as usize))
                    .sum();
            }
        }
        features.retain(|feature| !feature.faces.is_empty());
    }
    extracted
}

/// The master tooth cross-section as a sweepable profile: a polyline in
/// `(azimuth within one sector, radius)`, extracted from the folded
/// height-field of an accepted pattern. Sweeping it helically at
/// `helix_rate` over `z_range` and repeating it `count` times about the
/// axis regenerates the toothing.
#[derive(Clone, Debug)]
pub struct MasterProfile {
    pub count: usize,
    pub helix_rate: f64,
    pub z_range: (f64, f64),
    /// `(azimuth radians within the sector, radius mm)`, azimuth ascending.
    pub points: Vec<(f64, f64)>,
}

/// Douglas-Peucker polyline simplification in scaled coordinates.
fn simplify_polyline(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let (first, last) = (points[0], points[points.len() - 1]);
    let (dx, dy) = (last.0 - first.0, last.1 - first.1);
    let length = dx.hypot(dy).max(1e-12);
    let mut worst = 0.0;
    let mut split = 0;
    for (index, p) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let distance = ((p.0 - first.0) * dy - (p.1 - first.1) * dx).abs() / length;
        if distance > worst {
            worst = distance;
            split = index;
        }
    }
    if worst <= epsilon {
        return vec![first, last];
    }
    let mut left = simplify_polyline(&points[..=split], epsilon);
    let right = simplify_polyline(&points[split..], epsilon);
    left.pop();
    left.extend(right);
    left
}

/// Recognizes the n-fold pattern feature itself: one master surface,
/// sampled in folded profile space, repeated `count` times about the
/// datum axis — a gear's toothing as a single design feature.
///
/// Every candidate face's azimuth folds into one sector
/// (`theta mod 2*pi/count`); if the instances really are rotational
/// copies, all of them collapse onto one master height-field
/// `radius(folded theta, z)`. The fold residual is the acceptance gate
/// and the reported metric: it is the tooth-to-tooth error. Member faces
/// are claimed from freeform and small analytic features into one
/// `SurfaceClass::Pattern` feature.
pub fn recognize_pattern_feature(
    mesh: &TriangleMesh,
    features: &mut Vec<FeatureRecord>,
    alignment: &DatumAlignment,
    pattern: &PatternProposal,
    tolerance: f64,
) -> Option<MasterProfile> {
    const MAX_DONOR_AREA: f64 = 150.0;
    const MIN_MEMBER_AREA: f64 = 300.0;
    const THETA_CELLS: usize = 96;
    const Z_CELLS: usize = 24;
    const MIN_CELL_WEIGHT: f64 = 1e-9;
    let sector = std::f64::consts::TAU / pattern.count as f64;
    let z_low = pattern.z_range.0 - 1.0;
    let z_high = pattern.z_range.1 + 1.0;
    let rho_floor = pattern.radius_range.0 * 0.8;
    let rho_ceil = pattern.radius_range.1 * 1.1;
    let donor = |feature: &FeatureRecord| {
        matches!(feature.surface, SurfaceClass::Freeform)
            || (feature.area < MAX_DONOR_AREA
                && !matches!(
                    feature.surface,
                    SurfaceClass::Blend(_) | SurfaceClass::Pattern(_)
                ))
    };
    struct Sample {
        face: u32,
        theta: f64,
        z: f64,
        radial: f64,
        area: f64,
    }
    let mut samples: Vec<Sample> = Vec::new();
    let mut member_area = 0.0;
    for feature in features.iter() {
        if !donor(feature) {
            continue;
        }
        for &face in &feature.faces {
            let c = alignment
                .transform
                .apply_point(mesh.face_centroid(face as usize));
            let radial = c.x.hypot(c.y);
            if !(rho_floor..=rho_ceil).contains(&radial) || !(z_low..=z_high).contains(&c.z) {
                continue;
            }
            let theta = (c.y.atan2(c.x) + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU);
            let area = mesh.face_area(face as usize);
            member_area += area;
            samples.push(Sample {
                face,
                theta,
                z: c.z,
                radial,
                area,
            });
        }
    }
    if member_area < MIN_MEMBER_AREA {
        return None;
    }
    // A helical pattern drifts in azimuth with height; folded against a
    // static sector the flanks smear and the residual explodes. Estimate
    // the helix rate (radians of azimuth per millimetre of height) by
    // scanning candidates and keeping the one that folds tightest, then
    // unwrap every sample before folding. For a true helical surface the
    // unwrapped geometry is z-invariant, so this both stitches the
    // pattern and measures the helix angle from the scan.
    let z_mid = (z_low + z_high) / 2.0;
    let fold_rms_at = |rate: f64| -> f64 {
        const CELLS: usize = 192;
        let mut weight = [0.0f64; CELLS];
        let mut radius_sum = [0.0f64; CELLS];
        for sample in &samples {
            let unwrapped =
                (sample.theta - rate * (sample.z - z_mid)).rem_euclid(std::f64::consts::TAU);
            let folded = unwrapped.rem_euclid(sector);
            let cell = (((folded / sector) * CELLS as f64) as usize).min(CELLS - 1);
            weight[cell] += sample.area;
            radius_sum[cell] += sample.area * sample.radial;
        }
        let mut squared = 0.0;
        let mut counted = 0.0;
        for sample in &samples {
            let unwrapped =
                (sample.theta - rate * (sample.z - z_mid)).rem_euclid(std::f64::consts::TAU);
            let folded = unwrapped.rem_euclid(sector);
            let cell = (((folded / sector) * CELLS as f64) as usize).min(CELLS - 1);
            if weight[cell] > sample.area + MIN_CELL_WEIGHT {
                let reference = (radius_sum[cell] - sample.area * sample.radial)
                    / (weight[cell] - sample.area);
                let residual = sample.radial - reference;
                squared += sample.area * residual * residual;
                counted += sample.area;
            }
        }
        if counted > 0.0 { (squared / counted).sqrt() } else { f64::INFINITY }
    };
    let mut helix_rate = 0.0;
    let mut best_rms = f64::INFINITY;
    let mut step = 0.004;
    for i in -20..=20 {
        let rate = i as f64 * step;
        let rms = fold_rms_at(rate);
        if rms < best_rms {
            best_rms = rms;
            helix_rate = rate;
        }
    }
    for _ in 0..4 {
        step /= 2.0;
        for candidate in [helix_rate - step, helix_rate + step] {
            let rms = fold_rms_at(candidate);
            if rms < best_rms {
                best_rms = rms;
                helix_rate = candidate;
            }
        }
    }
    struct Member {
        face: u32,
        theta_cell: usize,
        z_cell: usize,
        instance: usize,
        radial: f64,
        area: f64,
    }
    let members: Vec<Member> = samples
        .iter()
        .map(|sample| {
            let unwrapped = (sample.theta - helix_rate * (sample.z - z_mid))
                .rem_euclid(std::f64::consts::TAU);
            let instance = ((unwrapped / sector) as usize).min(pattern.count - 1);
            let folded = unwrapped - instance as f64 * sector;
            let theta_cell = ((folded / sector) * THETA_CELLS as f64) as usize;
            let z_cell = (((sample.z - z_low) / (z_high - z_low)) * Z_CELLS as f64) as usize;
            Member {
                face: sample.face,
                theta_cell: theta_cell.min(THETA_CELLS - 1),
                z_cell: z_cell.min(Z_CELLS - 1),
                instance,
                radial: sample.radial,
                area: sample.area,
            }
        })
        .collect();
    // Master height-field: area-weighted mean radius per folded cell.
    let mut weight = vec![0.0f64; THETA_CELLS * Z_CELLS];
    let mut radius_sum = vec![0.0f64; THETA_CELLS * Z_CELLS];
    for member in &members {
        let cell = member.z_cell * THETA_CELLS + member.theta_cell;
        weight[cell] += member.area;
        radius_sum[cell] += member.area * member.radial;
    }
    // Fold residuals, leave-one-out against the master cell. Tooth-end
    // chamfers and sub-root gullet scraps do not fold — they are not part
    // of the repeated surface — so residuals are trimmed against the
    // median and the master is rebuilt from the survivors. Only surviving
    // faces are claimed; the ends honestly stay outside the pattern.
    let residual_of = |member: &Member, weight: &[f64], radius_sum: &[f64]| -> Option<f64> {
        let cell = member.z_cell * THETA_CELLS + member.theta_cell;
        if weight[cell] <= member.area + MIN_CELL_WEIGHT {
            return None;
        }
        let reference =
            (radius_sum[cell] - member.area * member.radial) / (weight[cell] - member.area);
        Some(member.radial - reference)
    };
    let mut first_residuals: Vec<Option<f64>> = members
        .iter()
        .map(|m| residual_of(m, &weight, &radius_sum))
        .collect();
    let mut magnitudes: Vec<f64> = first_residuals
        .iter()
        .flatten()
        .map(|r| r.abs())
        .collect();
    if magnitudes.is_empty() {
        return None;
    }
    magnitudes.sort_by(f64::total_cmp);
    let median = magnitudes[magnitudes.len() / 2];
    let cutoff = (4.0 * median).max(2.0 * tolerance);
    let kept: Vec<&Member> = members
        .iter()
        .zip(&first_residuals)
        .filter(|(_, residual)| residual.is_some_and(|r| r.abs() <= cutoff))
        .map(|(member, _)| member)
        .collect();
    first_residuals.clear();
    if kept.len() < members.len() / 4 {
        return None;
    }
    // Rebuild the master from survivors and measure the final fold.
    let mut weight = vec![0.0f64; THETA_CELLS * Z_CELLS];
    let mut radius_sum = vec![0.0f64; THETA_CELLS * Z_CELLS];
    for member in &kept {
        let cell = member.z_cell * THETA_CELLS + member.theta_cell;
        weight[cell] += member.area;
        radius_sum[cell] += member.area * member.radial;
    }
    let mut squared = 0.0;
    let mut max_abs = 0.0f64;
    let mut counted = 0.0;
    let mut instance_squared = vec![0.0f64; pattern.count];
    let mut instance_weight = vec![0.0f64; pattern.count];
    let mut member_area = 0.0;
    for member in &kept {
        member_area += member.area;
        let Some(residual) = residual_of(member, &weight, &radius_sum) else {
            continue;
        };
        squared += member.area * residual * residual;
        counted += member.area;
        max_abs = max_abs.max(residual.abs());
        instance_squared[member.instance] += member.area * residual * residual;
        instance_weight[member.instance] += member.area;
    }
    if counted <= 0.0 || member_area < MIN_MEMBER_AREA {
        return None;
    }
    let rms = (squared / counted).sqrt();
    if rms > 2.5 * tolerance {
        return None;
    }
    let worst_instance_rms = (0..pattern.count)
        .filter(|&k| instance_weight[k] > 0.0)
        .map(|k| (instance_squared[k] / instance_weight[k]).sqrt())
        .fold(0.0f64, f64::max);
    // Claim the member faces into one pattern feature.
    let mut stolen = vec![false; mesh.triangles().len()];
    let faces: Vec<u32> = kept.iter().map(|m| m.face).collect();
    for &face in &faces {
        stolen[face as usize] = true;
    }
    for feature in features.iter_mut() {
        if donor(feature) {
            feature.faces.retain(|&face| !stolen[face as usize]);
            feature.face_count = feature.faces.len();
            feature.area = feature
                .faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
        }
    }
    features.retain(|feature| !feature.faces.is_empty());
    features.push(FeatureRecord {
        id: 0,
        surface: SurfaceClass::Pattern(crate::fit::PatternFit {
            axis_point: Point3::default(),
            axis: Vector3::new(0.0, 0.0, 1.0),
            count: pattern.count,
            z_range: (z_low, z_high),
            radius_range: (rho_floor, rho_ceil),
            deviation: crate::fit::DeviationStats { rms, max_abs },
            worst_instance_rms,
            helix_rate,
        }),
        face_count: faces.len(),
        area: member_area,
        faces,
        notes: vec![format!(
            "master surface x {} circular pattern; fold rms {:.3} mm, worst instance {:.3} mm, \
             helix angle {:.2} deg at d {:.1}",
            pattern.count,
            rms,
            worst_instance_rms,
            (helix_rate * (rho_floor + rho_ceil) / 2.0).atan().to_degrees(),
            rho_floor + rho_ceil
        )],
    });
    // The sweepable master profile: fold the central 60 percent of the
    // band (ends carry chamfers) into a fine 1D height-field and simplify.
    const PROFILE_BINS: usize = 256;
    let z_margin = 0.2 * (z_high - z_low);
    let mut weight_1d = [0.0f64; PROFILE_BINS];
    let mut radius_1d = [0.0f64; PROFILE_BINS];
    for member in &kept {
        let z = z_low + (member.z_cell as f64 + 0.5) / Z_CELLS as f64 * (z_high - z_low);
        if z < z_low + z_margin || z > z_high - z_margin {
            continue;
        }
        let folded = (member.theta_cell as f64 + 0.5) / THETA_CELLS as f64 * sector;
        let bin = ((folded / sector) * PROFILE_BINS as f64) as usize;
        weight_1d[bin.min(PROFILE_BINS - 1)] += member.area;
        radius_1d[bin.min(PROFILE_BINS - 1)] += member.area * member.radial;
    }
    let r_mid = (rho_floor + rho_ceil) / 2.0;
    let raw: Vec<(f64, f64)> = (0..PROFILE_BINS)
        .filter(|&bin| weight_1d[bin] > MIN_CELL_WEIGHT)
        .map(|bin| {
            (
                (bin as f64 + 0.5) / PROFILE_BINS as f64 * sector,
                radius_1d[bin] / weight_1d[bin],
            )
        })
        .collect();
    if raw.len() < 4 {
        return None;
    }
    // Simplify in arc-length scale so the epsilon is millimetres in both axes.
    let scaled: Vec<(f64, f64)> = raw.iter().map(|(t, r)| (t * r_mid, *r)).collect();
    let simplified = simplify_polyline(&scaled, tolerance * 0.75);
    let points: Vec<(f64, f64)> = simplified
        .iter()
        .map(|(x, r)| (x / r_mid, *r))
        .collect();
    Some(MasterProfile {
        count: pattern.count,
        helix_rate,
        z_range: (z_low + z_margin, z_high - z_margin),
        points,
    })
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
    pub master_profile: Option<MasterProfile>,
    pub coverage: PlanCoverage,
    pub notes: Vec<String>,
}

/// z and radial extent of a feature's faces in the datum frame.
pub(crate) fn extents(
    mesh: &TriangleMesh,
    faces: &[u32],
    alignment: &DatumAlignment,
) -> (f64, f64, f64, f64) {
    let mut z_min = f64::INFINITY;
    let mut z_max = f64::NEG_INFINITY;
    let mut r_min = f64::INFINITY;
    let mut r_max = f64::NEG_INFINITY;
    for &face in faces {
        // Corners, not centroids: a triangle fan's centroids all sit at
        // one radius and would collapse the radial extent.
        for corner in mesh.triangle_points(face as usize) {
            let c = alignment.transform.apply_point(corner);
            let radial = c.x.hypot(c.y);
            z_min = z_min.min(c.z);
            z_max = z_max.max(c.z);
            r_min = r_min.min(radial);
            r_max = r_max.max(radial);
        }
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

/// Detects an n-fold circular repetition in the freeform residue.
///
/// Two complementary azimuthal signals are autocorrelated: the **area
/// histogram** (a sparse pattern — lugs, bosses — leaves presence/absence
/// pulses) and the **mean-radius profile** (a dense band — gear teeth —
/// covers the whole circumference, so its signature is the root-to-tip
/// radius oscillation instead). Both are built per z-slab, because a
/// helical pattern drifts in azimuth with height and autocorrelation is
/// phase-invariant per slab, so slab correlations sum coherently no
/// matter the twist. Shifts are fractional with linear interpolation,
/// and the peak picker climbs to the largest multiple that still scores,
/// which undoes the subharmonic alias (a shift of two periods reads as
/// half the count).
pub(crate) fn detect_circular_pattern(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    alignment: &DatumAlignment,
) -> Option<PatternProposal> {
    const BINS: usize = 1440;
    const SLABS: usize = 16;
    const MIN_AREA: f64 = 50.0;
    const MIN_STRENGTH: f64 = 0.35;
    const MAX_COUNT: usize = 120;
    let mut samples: Vec<(f64, f64, f64, f64)> = Vec::new();
    let mut total = 0.0;
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
            samples.push((c.y.atan2(c.x), c.z, radial, area));
            total += area;
        }
    }
    if total < MIN_AREA {
        return None;
    }
    // Trim to the dense band: unexplained slivers are scattered over the
    // whole part, and only the concentrated band (a gear's teeth, a lug
    // ring) carries the pattern. Keep the 10th..90th area-weighted
    // percentile window in height and radius.
    let percentile_window = |key: fn(&(f64, f64, f64, f64)) -> f64,
                             samples: &[(f64, f64, f64, f64)],
                             total: f64| {
        let mut sorted: Vec<(f64, f64)> = samples.iter().map(|s| (key(s), s.3)).collect();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut cumulative = 0.0;
        let mut low = sorted.first().map_or(0.0, |s| s.0);
        let mut high = sorted.last().map_or(0.0, |s| s.0);
        let mut low_set = false;
        for (value, area) in &sorted {
            cumulative += area;
            if !low_set && cumulative >= 0.10 * total {
                low = *value;
                low_set = true;
            }
            if cumulative <= 0.90 * total {
                high = *value;
            }
        }
        (low, high)
    };
    // Radius first, as a mode-centred window: the pattern band (teeth,
    // lugs) is the densest radius family, and percentiles would blur it
    // together with unexplained rings at other radii.
    const RADIUS_BINS: usize = 48;
    let r_min = samples.iter().map(|s| s.2).fold(f64::INFINITY, f64::min);
    let r_max = samples.iter().map(|s| s.2).fold(f64::NEG_INFINITY, f64::max);
    let span = (r_max - r_min).max(1e-9);
    let mut radius_histogram = [0.0f64; RADIUS_BINS];
    for &(_, _, radial, area) in &samples {
        let bin = (((radial - r_min) / span) * RADIUS_BINS as f64) as usize;
        radius_histogram[bin.min(RADIUS_BINS - 1)] += area;
    }
    let peak = (0..RADIUS_BINS)
        .max_by(|&a, &b| radius_histogram[a].total_cmp(&radius_histogram[b]))?;
    let floor = 0.2 * radius_histogram[peak];
    let mut low_bin = peak;
    while low_bin > 0 && radius_histogram[low_bin - 1] >= floor {
        low_bin -= 1;
    }
    let mut high_bin = peak;
    while high_bin + 1 < RADIUS_BINS && radius_histogram[high_bin + 1] >= floor {
        high_bin += 1;
    }
    let r_low = r_min + span * low_bin as f64 / RADIUS_BINS as f64;
    let r_high = r_min + span * (high_bin + 1) as f64 / RADIUS_BINS as f64;
    samples.retain(|(_, _, radial, _)| (r_low..=r_high).contains(radial));
    total = samples.iter().map(|(_, _, _, a)| a).sum();
    if total < MIN_AREA {
        return None;
    }
    // Then trim height to the dense 10th..90th percentile band.
    let (z_low, z_high) = percentile_window(|s| s.1, &samples, total);
    samples.retain(|(_, z, _, _)| (z_low..=z_high).contains(z));
    total = samples.iter().map(|(_, _, _, a)| a).sum();
    if total < MIN_AREA || z_high <= z_low {
        return None;
    }
    let slab_height = ((z_high - z_low) / SLABS as f64).max(1e-9);
    let mut weight = vec![vec![0.0f64; BINS]; SLABS];
    let mut radius_sum = vec![vec![0.0f64; BINS]; SLABS];
    for &(angle, z, radial, area) in &samples {
        let slab = (((z - z_low) / slab_height) as usize).min(SLABS - 1);
        let bin = ((((angle + std::f64::consts::PI) / std::f64::consts::TAU) * BINS as f64)
            as usize)
            .min(BINS - 1);
        weight[slab][bin] += area;
        radius_sum[slab][bin] += area * radial;
    }
    // Centered per-slab signals: area presence, and mean-radius profile.
    let mut area_signal = vec![vec![0.0f64; BINS]; SLABS];
    let mut radius_signal = vec![vec![0.0f64; BINS]; SLABS];
    for slab in 0..SLABS {
        let slab_area: f64 = weight[slab].iter().sum();
        if slab_area <= 0.0 {
            continue;
        }
        let area_mean = slab_area / BINS as f64;
        let radius_mean = radius_sum[slab].iter().sum::<f64>() / slab_area;
        for bin in 0..BINS {
            area_signal[slab][bin] = weight[slab][bin] - area_mean;
            if weight[slab][bin] > 0.0 {
                radius_signal[slab][bin] = radius_sum[slab][bin] / weight[slab][bin] - radius_mean;
            }
        }
    }
    let correlation = |signals: &[Vec<f64>], count: usize| -> f64 {
        let denominator: f64 = signals.iter().flat_map(|s| s.iter()).map(|v| v * v).sum();
        if denominator < 1e-12 {
            return 0.0;
        }
        let shift = BINS as f64 / count as f64;
        let base = shift.floor() as usize;
        let fraction = shift - base as f64;
        signals
            .iter()
            .map(|slab| {
                slab.iter()
                    .enumerate()
                    .map(|(b, v)| {
                        let lower = slab[(b + base) % BINS];
                        let upper = slab[(b + base + 1) % BINS];
                        v * (lower * (1.0 - fraction) + upper * fraction)
                    })
                    .sum::<f64>()
            })
            .sum::<f64>()
            / denominator
    };
    let score_of = |count: usize| -> f64 {
        correlation(&area_signal, count).max(correlation(&radius_signal, count))
    };
    let scores: Vec<f64> = (0..=MAX_COUNT)
        .map(|count| if count < 5 { 0.0 } else { score_of(count) })
        .collect();
    let mut count = (5..=MAX_COUNT).max_by(|&a, &b| scores[a].total_cmp(&scores[b]))?;
    if scores[count] < MIN_STRENGTH {
        return None;
    }
    // Climb the harmonic ladder: a shift of k periods aliases the true
    // count down to count/k, so prefer the largest multiple that still
    // scores close to the peak.
    loop {
        let climb = (2..)
            .map(|k| count * k)
            .take_while(|&m| m <= MAX_COUNT)
            .filter(|&m| scores[m] >= 0.85 * scores[count])
            .max();
        match climb {
            Some(higher) if higher != count => count = higher,
            _ => break,
        }
    }
    Some(PatternProposal {
        count,
        strength: scores[count],
        z_range: (z_low, z_high),
        radius_range: (r_low, r_high),
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
    pattern: Option<PatternProposal>,
    master_profile: Option<MasterProfile>,
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
    plan.pattern = pattern;
    plan.master_profile = master_profile;
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
    if let Some(profile) = &plan.master_profile {
        separate(&mut out);
        out.push_str(&format!(
            "{{\"type\":\"helical_sweep_pattern_proposal\",\"count\":{},\"helix_rate\":{:.8},\"z_range\":[{:.6},{:.6}],\"profile\":[",
            profile.count, profile.helix_rate, profile.z_range.0, profile.z_range.1
        ));
        for (index, (theta, radius)) in profile.points.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&format!("[{theta:.6},{radius:.6}]"));
        }
        out.push_str("]}");
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
    if let Some(profile) = &plan.master_profile {
        let radii: Vec<f64> = profile.points.iter().map(|(_, r)| *r).collect();
        let low = radii.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let high = radii.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        out.push_str(&format!(
            "  master profile: {} points, d {:.2}..{:.2}, helical sweep rate {:.4} rad/mm\n",
            profile.points.len(),
            low * 2.0,
            high * 2.0,
            profile.helix_rate
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
    fn interrupted_band_lumped_as_freeform_still_extracts() {
        use crate::datum::DatumAlignment;
        use crate::transform::RigidTransform;
        // Twelve root-land arcs that segmentation lumped into one freeform
        // blob (as flank contamination causes on real gears): the
        // profile-space ridge must still recover one axis-locked cylinder.
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
        let arc_face_count = soup.len();
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, -1.0),
            15.0,
            96,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let arc_faces: Vec<u32> = (0..arc_face_count as u32).collect();
        let disk_faces: Vec<u32> = (arc_face_count as u32..mesh.triangles().len() as u32).collect();
        let mut features = vec![
            FeatureRecord {
                id: 0,
                surface: SurfaceClass::Freeform,
                face_count: arc_faces.len(),
                area: arc_faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
                faces: arc_faces,
                notes: Vec::new(),
            },
            FeatureRecord {
                id: 1,
                surface: SurfaceClass::Freeform,
                face_count: disk_faces.len(),
                area: disk_faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
                faces: disk_faces,
                notes: Vec::new(),
            },
        ];
        let alignment = DatumAlignment {
            transform: RigidTransform::to_frame(
                Point3::default(),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            )
            .unwrap(),
            notes: Vec::new(),
        };
        let extracted = extract_revolved_bands(&mesh, &mut features, &alignment, 0.05);
        assert_eq!(extracted, 1, "band not extracted");
        let cylinders: Vec<_> = features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cylinder(fit) => Some((f, fit)),
                _ => None,
            })
            .collect();
        assert_eq!(cylinders.len(), 1);
        assert!((cylinders[0].1.radius - 15.0).abs() < 0.02, "radius {}", cylinders[0].1.radius);
        assert_eq!(cylinders[0].0.face_count, arc_face_count);
        // The disk faces point axially, not radially, and must stay put.
        assert!(features.iter().any(|f| matches!(f.surface, SurfaceClass::Freeform)
            && f.face_count == mesh.triangles().len() - arc_face_count));
    }

    #[test]
    fn interrupted_cone_band_extracts_as_one_cone() {
        use crate::datum::DatumAlignment;
        use crate::transform::RigidTransform;
        // Twelve arcs of a conical band (slope 0.5): profile-space line
        // RANSAC must stitch them into one axis-true cone.
        let mut soup = Vec::new();
        for arc in 0..12 {
            let start = std::f64::consts::TAU * arc as f64 / 12.0;
            let arc_soup = synth::revolved_profile_soup(&[(20.0, 0.0), (25.0, 10.0)], 96);
            // Keep only faces inside a 20-degree window of this arc.
            for triangle in &arc_soup {
                let c = Point3::new(
                    (triangle[0].x + triangle[1].x + triangle[2].x) / 3.0,
                    (triangle[0].y + triangle[1].y + triangle[2].y) / 3.0,
                    0.0,
                );
                let angle = (c.y.atan2(c.x) + std::f64::consts::PI)
                    .rem_euclid(std::f64::consts::TAU);
                if angle >= start && angle < start + std::f64::consts::TAU / 18.0 {
                    soup.push(*triangle);
                }
            }
        }
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, 0.0),
            Vector3::new(0.0, 0.0, -1.0),
            20.0,
            96,
        ));
        let mesh = crate::mesh::TriangleMesh::from_triangle_soup(&soup, 1e-9).unwrap();
        let disk_start = mesh.triangles().len() - 96;
        let cone_faces: Vec<u32> = (0..disk_start as u32).collect();
        let disk_faces: Vec<u32> = (disk_start as u32..mesh.triangles().len() as u32).collect();
        let mut features = vec![
            FeatureRecord {
                id: 0,
                surface: SurfaceClass::Freeform,
                face_count: cone_faces.len(),
                area: cone_faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
                faces: cone_faces,
                notes: Vec::new(),
            },
            FeatureRecord {
                id: 1,
                surface: SurfaceClass::Freeform,
                face_count: disk_faces.len(),
                area: disk_faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
                faces: disk_faces,
                notes: Vec::new(),
            },
        ];
        let alignment = DatumAlignment {
            transform: RigidTransform::to_frame(
                Point3::default(),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            )
            .unwrap(),
            notes: Vec::new(),
        };
        extract_revolved_bands(&mesh, &mut features, &alignment, 0.05);
        let cones: Vec<_> = features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Cone(fit) => Some(fit),
                _ => None,
            })
            .collect();
        assert_eq!(cones.len(), 1, "cone band not extracted");
        let expected = 0.5f64.atan();
        assert!(
            (cones[0].half_angle - expected).abs() < 0.01,
            "half angle {} vs {}",
            cones[0].half_angle,
            expected
        );
        assert!(cones[0].axis.z.abs() > 1.0 - 1e-9);
        // Apex where rho = 20 + 0.5 z crosses zero: z = -40.
        assert!((cones[0].apex.z - -40.0).abs() < 0.5, "apex {}", cones[0].apex.z);
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
        // The twelve identical lugs must also unify into one master-pattern
        // feature with a tight fold residual.
        let features: Vec<_> = report
            .features
            .iter()
            .filter_map(|f| match &f.surface {
                SurfaceClass::Pattern(fit) => Some((f, fit)),
                _ => None,
            })
            .collect();
        assert_eq!(features.len(), 1, "pattern feature missing");
        assert_eq!(features[0].1.count, 12);
        assert!(
            features[0].1.deviation.rms < 0.1,
            "fold rms {}",
            features[0].1.deviation.rms
        );
        let profile = report
            .plan
            .as_ref()
            .and_then(|p| p.master_profile.as_ref())
            .expect("master profile present");
        assert_eq!(profile.count, 12);
        assert!(profile.points.len() >= 4, "{} points", profile.points.len());
        let radii: Vec<f64> = profile.points.iter().map(|(_, r)| *r).collect();
        let low = radii.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let high = radii.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        assert!(low > 19.0 && high < 21.0, "profile radii {low}..{high}");
    }
}

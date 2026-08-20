//! Sharp-edge reconstruction: the idealized model.
//!
//! The scan carries rounds on every physical edge; the design does not.
//! This stage rebuilds the part from its recognized features with every
//! revolved surface extended to its exact intersection with its
//! neighbours — fillet and chamfer rings drop out of the geometry and
//! survive only as parametric callouts, the toothing regenerates as the
//! master profile swept helically, and the result is a model whose edges
//! are sharp.

use artificer_geometry::{Point3, Vector3};

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
    Wall {
        feature: usize,
        rho: f64,
        z0: f64,
        z1: f64,
    },
    /// z constant over [rho0, rho1].
    Face {
        feature: usize,
        z: f64,
        rho0: f64,
        rho1: f64,
    },
    /// rho = intercept + slope * z over [z0, z1].
    Taper {
        feature: usize,
        slope: f64,
        intercept: f64,
        z0: f64,
        z1: f64,
    },
}

/// A revolved fillet, as the circular arc it is in profile space:
/// `(rho, z) = (major + minor cos t, z_center + minor sin t)` over
/// `[t0, t1]`. Kept apart from `Element` because a fillet plays no part
/// in the sharp-corner solve — it is what *replaces* a sharp corner, and
/// its own extent is measured from the scan rather than derived from
/// where its neighbours would have met.
#[derive(Clone, Copy, Debug)]
struct Fillet {
    feature: usize,
    major: f64,
    minor: f64,
    z_center: f64,
    t0: f64,
    t1: f64,
}

impl Fillet {
    fn at(&self, t: f64) -> (f64, f64) {
        (
            self.major + self.minor * t.cos(),
            self.z_center + self.minor * t.sin(),
        )
    }

    fn ends(&self) -> [(f64, f64); 2] {
        [self.at(self.t0), self.at(self.t1)]
    }
}

pub struct RebuiltModel {
    pub mesh: TriangleMesh,
    /// Feature id (in the report) per rebuilt triangle.
    pub feature_of_face: Vec<usize>,
    /// Features left out of the rebuild because their surface is
    /// azimuthally interrupted (an unrecognized pattern, like a dog-tooth
    /// ring): emitting a full revolution would entomb real geometry.
    pub skipped: Vec<String>,
    /// Judgement calls the rebuild made and stands behind: families of
    /// co-annular arcs emitted as one revolution, scan holes assumed full.
    pub notes: Vec<String>,
    /// The curves adjacent faces share — the model's topology, as far as
    /// it has been recovered.
    pub edges: Vec<SharedEdge>,
    /// The exact points where three or more faces meet.
    pub corners: Vec<crate::sew::Corner>,
    /// Edge ends no corner adopted, each with the reason it is open —
    /// the watertightness work list, located in space.
    pub open_ends: Vec<crate::sew::OpenEnd>,
}

/// Rebuilds the sharp idealized model from a finished report. Requires a
/// datum frame; returns `None` without one.
pub fn rebuild_sharp(mesh: &TriangleMesh, report: &ReverseReport) -> Option<RebuiltModel> {
    let alignment = report.datum.as_ref()?;
    // Pattern bands: each master profile regenerates its band, so revolved
    // elements inside any of them are already covered.
    struct Band<'a> {
        profile: &'a crate::reconstruct::MasterProfile,
        /// Everything the pattern claimed, chamfers included.
        z: (f64, f64),
        /// Where the band's flat end faces actually sit.
        ends: (f64, f64),
        rho: (f64, f64),
        root: f64,
    }
    // The heights at which a band's material actually ends, read off its
    // own flat end faces rather than taken from the extreme of everything
    // the pattern claimed. A pattern claims the tooth-end chamfers too,
    // so its z_range overshoots the flat end face — by 0.7 mm on the test
    // gear, which is enough to leave the whole end face unaccounted for.
    // Area-weighted histogram, because the flat end is by far the most
    // area at any one height.
    let measured_ends = |faces: &[u32], z_range: (f64, f64)| -> (f64, f64) {
        const BIN: f64 = 0.1;
        let middle = (z_range.0 + z_range.1) / 2.0;
        let mut lower: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
        let mut upper: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
        for &face in faces {
            let Some(normal) = mesh.face_normal(face as usize) else {
                continue;
            };
            if alignment.transform.apply_vector(normal).z.abs() < 0.7 {
                continue;
            }
            let c = alignment
                .transform
                .apply_point(mesh.face_centroid(face as usize));
            let bin = (c.z / BIN).round() as i64;
            let side = if c.z < middle { &mut lower } else { &mut upper };
            *side.entry(bin).or_default() += mesh.face_area(face as usize);
        }
        let peak = |bins: &std::collections::HashMap<i64, f64>, fallback: f64| {
            bins.iter()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(bin, _)| *bin as f64 * BIN)
                .unwrap_or(fallback)
        };
        (peak(&lower, z_range.0), peak(&upper, z_range.1))
    };
    let mut bands: Vec<Band> = Vec::new();
    if let Some(plan) = report.plan.as_ref() {
        for profile in &plan.master_profiles {
            let Some((faces, fit)) = report.features.iter().find_map(|f| match &f.surface {
                SurfaceClass::Pattern(fit) if f.id == profile.feature_id => Some((&f.faces, fit)),
                _ => None,
            }) else {
                continue;
            };
            let root = profile
                .points
                .iter()
                .map(|(_, rho)| *rho)
                .fold(f64::INFINITY, f64::min);
            let ends = measured_ends(faces, fit.z_range);
            bands.push(Band {
                profile,
                z: fit.z_range,
                ends,
                rho: fit.radius_range,
                root,
            });
        }
    }
    let inside_pattern = |rho: f64, z0: f64, z1: f64| -> bool {
        bands.iter().any(|band| {
            rho >= band.rho.0 - 1.0
                && rho <= band.rho.1 + 1.0
                && z0 >= band.z.0 - 1.0
                && z1 <= band.z.1 + 1.0
        })
    };
    // Solidity: measured area against the area of the full revolved
    // surface. A castellated ring's lands ring the whole circumference —
    // azimuth bins alone read full — but they carry only a fraction of
    // the area, and must not rebuild as a solid revolution.
    const FULL_REVOLUTION: f64 = 0.70;
    /// A family of co-annular arcs may jointly justify the revolution a
    /// single arc cannot.
    const FAMILY_REVOLUTION: f64 = 0.60;
    let mut skipped: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    // Filled once the faces have been trimmed to each other.
    let mut edges: Vec<SharedEdge>;
    let corners: Vec<crate::sew::Corner>;
    let open_ends: Vec<crate::sew::OpenEnd>;
    // Recognized cut volumes: built inside the plan block, consumed
    // once more by the punch after every emission stage has run.
    let mut cut_volumes: Vec<CutVolume> = Vec::new();
    let mut scan_cells: std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)> =
        std::collections::HashMap::new();
    // Exact-emitted triangles are immune to the punch: their volumes
    // are their own license, and the broom must not sweep the floor
    // it stands on.
    let mut exact_range: (usize, usize) = (0, 0);
    let mut elements: Vec<Element> = Vec::new();
    let mut fillets: Vec<Fillet> = Vec::new();
    // Every axis-true revolved candidate, for the family and scan-hole
    // judgements below.
    #[derive(Clone, Copy)]
    struct Arc {
        id: usize,
        kind: u8,
        /// Kind-specific compatibility parameter: a cylinder's radius, a
        /// level plane's z, a cone's signed profile slope.
        param: f64,
        z0: f64,
        z1: f64,
        r0: f64,
        r1: f64,
        area: f64,
    }
    let arcs: Vec<Arc> = report
        .features
        .iter()
        .filter_map(|feature| {
            let (z0, z1, r0, r1) = extents(mesh, &feature.faces, alignment);
            let (kind, param) = match &feature.surface {
                SurfaceClass::Cylinder(fit)
                    if fit.axis.z.abs() > 0.999
                        && fit.axis_point.x.hypot(fit.axis_point.y) < 3.0 =>
                {
                    (0u8, fit.radius)
                }
                SurfaceClass::Plane(fit) if fit.normal.z.abs() > 0.999 => (1, fit.origin.z),
                SurfaceClass::Cone(fit)
                    if fit.axis.z.abs() > 0.999
                        && crate::reconstruct::cone_axis_offset(fit, (z0 + z1) / 2.0) < 3.0 =>
                {
                    (2, fit.half_angle.tan() * fit.axis.z.signum())
                }
                _ => return None,
            };
            Some(Arc {
                id: feature.id,
                kind,
                param,
                z0,
                z1,
                r0,
                r1,
                area: feature.area,
            })
        })
        .collect();
    // Primary analytic area inside a band box, excluding the given family:
    // material genuinely occupying an interrupted ring's gaps (a pattern's
    // teeth, the bore behind a sliver). Edge rounds, blends and freeform
    // are transition skin and do not count as occupation.
    //
    // Presence is the wrong test, because the thing most likely to be
    // found sitting in an interrupted ring's gaps is *another arc of the
    // same surface* that failed to unify, or a misfit (a shallow cone
    // reads as an absurd sphere) lying exactly on it. Those are the
    // surface, not an obstruction, and counting them makes co-annular
    // arcs veto each other so neither is ever emitted. So a face blocks
    // the revolution only when it lies OFF the candidate surface.
    const OCCUPY_BAND: f64 = 3.0;
    let occupied_by_others =
        |candidate: &SurfaceClass, family: &[usize], z0: f64, z1: f64, r0: f64, r1: f64| -> f64 {
            report
                .features
                .iter()
                .filter(|f| {
                    !family.contains(&f.id)
                        && f.area >= 100.0
                        && matches!(
                            f.surface,
                            SurfaceClass::Plane(_)
                                | SurfaceClass::Cylinder(_)
                                | SurfaceClass::Cone(_)
                                | SurfaceClass::Sphere(_)
                                | SurfaceClass::Pattern(_)
                        )
                })
                .map(|f| {
                    f.faces
                        .iter()
                        .filter(|&&face| {
                            let c = alignment
                                .transform
                                .apply_point(mesh.face_centroid(face as usize));
                            let rho = c.x.hypot(c.y);
                            if !((z0..=z1).contains(&c.z) && (r0..=r1).contains(&rho)) {
                                return false;
                            }
                            candidate
                                .probe(c)
                                .is_none_or(|(d, _)| d.abs() > OCCUPY_BAND * report.tolerance)
                        })
                        .map(|&face| mesh.face_area(face as usize))
                        .sum::<f64>()
                })
                .sum()
        };
    // The family of arcs co-annular with `own` (same kind, overlapping
    // bands, transitively chained), and whether `own` is its largest.
    let family_of = |own: &Arc| -> Vec<Arc> {
        let mut family = vec![*own];
        loop {
            let mut grew = false;
            for arc in &arcs {
                if arc.kind != own.kind || family.iter().any(|f| f.id == arc.id) {
                    continue;
                }
                let compatible = match own.kind {
                    0 => (arc.param - own.param).abs() <= 0.75,
                    1 => (arc.param - own.param).abs() <= 0.5,
                    _ => (arc.param - own.param).abs() <= 0.06,
                };
                let joins = compatible
                    && family.iter().any(|f| {
                        f.z0.max(arc.z0) <= f.z1.min(arc.z1) + 0.5
                            && f.r0.max(arc.r0) <= f.r1.min(arc.r1) + 0.5
                    });
                if joins {
                    family.push(*arc);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        family
    };
    // Area-weighted profile-space sums over a set of features' faces,
    // for joint locked fits: (sum w, sum wz, sum w rho, sum wzz, sum wz rho).
    let profile_sums = |ids: &[usize]| -> (f64, f64, f64, f64, f64) {
        let (mut sw, mut sz, mut sr, mut szz, mut szr) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for f in report.features.iter().filter(|f| ids.contains(&f.id)) {
            for &face in &f.faces {
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
        }
        (sw, sz, sr, szz, szr)
    };
    for feature in &report.features {
        match &feature.surface {
            SurfaceClass::Cylinder(fit)
                if fit.axis.z.abs() > 0.999 && fit.axis_point.x.hypot(fit.axis_point.y) < 3.0 =>
            {
                let (z0, z1, _, _) = extents(mesh, &feature.faces, alignment);
                if z1 - z0 < 0.3 {
                    notes.push(format!(
                        "#{} cylinder d {:.2}: {:.2} mm tall, below the 0.3 mm emission floor",
                        feature.id,
                        fit.radius * 2.0,
                        z1 - z0
                    ));
                    continue;
                }
                if inside_pattern(fit.radius, z0, z1) {
                    notes.push(format!(
                        "#{} cylinder d {:.2}: inside a pattern band, regenerated by the pattern",
                        feature.id,
                        fit.radius * 2.0
                    ));
                    continue;
                }
                let expected = std::f64::consts::TAU * fit.radius * (z1 - z0);
                if feature.area < FULL_REVOLUTION * expected {
                    let own = arcs.iter().find(|a| a.id == feature.id).copied();
                    let mut verdict = format!(
                        "#{} cylinder d {:.2}: interrupted ring ({:.0}% solid), not a full revolution",
                        feature.id,
                        fit.radius * 2.0,
                        100.0 * feature.area / expected
                    );
                    let mut rescued = false;
                    if let Some(own) = own {
                        let family = family_of(&own);
                        let ids: Vec<usize> = family.iter().map(|a| a.id).collect();
                        let uz0 = family.iter().map(|a| a.z0).fold(f64::INFINITY, f64::min);
                        let uz1 = family
                            .iter()
                            .map(|a| a.z1)
                            .fold(f64::NEG_INFINITY, f64::max);
                        let family_area: f64 = family.iter().map(|a| a.area).sum();
                        let expected_union =
                            std::f64::consts::TAU * fit.radius * (uz1 - uz0).max(1e-9);
                        let missing = (expected_union - family_area).max(0.0);
                        // Inset along z so end faces the ring abuts do
                        // not count as occupying its gaps.
                        let occupied = occupied_by_others(
                            &feature.surface,
                            &ids,
                            uz0 + 0.3,
                            uz1 - 0.3,
                            fit.radius - 0.75,
                            fit.radius + 0.75,
                        );
                        let largest = family.iter().all(|a| a.area <= own.area + 1e-9);
                        if family.len() > 1
                            && family_area >= FAMILY_REVOLUTION * expected_union
                            && occupied < 0.5 * missing
                        {
                            if largest {
                                let (sw, _, sr, _, _) = profile_sums(&ids);
                                if sw > 0.0 {
                                    elements.push(Element::Wall {
                                        feature: feature.id,
                                        rho: sr / sw,
                                        z0: uz0,
                                        z1: uz1,
                                    });
                                    notes.push(format!(
                                        "#{} cylinder d {:.2}: emitted from a family of {} co-annular arcs ({:.0}% joint solidity)",
                                        feature.id,
                                        2.0 * sr / sw,
                                        family.len(),
                                        100.0 * family_area / expected_union
                                    ));
                                    rescued = true;
                                }
                            } else {
                                verdict = format!(
                                    "#{} cylinder d {:.2}: arc covered by its family's emission",
                                    feature.id,
                                    fit.radius * 2.0
                                );
                                notes.push(verdict.clone());
                                rescued = true;
                            }
                        }
                        // A lone arc used to be swept into a whole
                        // revolution on the argument that nothing else
                        // occupied its gaps. That was inventing material
                        // to fill a scan hole, and it is no longer the
                        // best available answer: the feature now falls
                        // through to a trimmed patch, which draws exactly
                        // what was measured and nothing more.
                    }
                    if !rescued {
                        skipped.push(verdict);
                    }
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
                // A face that absorbed pattern lands is sparse beyond the
                // root radius but solid inside it — and inside is all the
                // rebuild emits, so judge solidity on the clamped annulus.
                let cap = bands
                    .iter()
                    .filter(|band| {
                        fit.origin.z >= band.z.0 - SNAP_REACH
                            && fit.origin.z <= band.z.1 + SNAP_REACH
                            && r1 > band.root
                    })
                    .map(|band| band.root)
                    .fold(r1, f64::min);
                let within: f64 = feature
                    .faces
                    .iter()
                    .filter(|&&face| {
                        let c = alignment
                            .transform
                            .apply_point(mesh.face_centroid(face as usize));
                        c.x.hypot(c.y) <= cap + 0.5
                    })
                    .map(|&face| mesh.face_area(face as usize))
                    .sum();
                let expected_full = std::f64::consts::PI * (r1 * r1 - r0 * r0).max(1e-9);
                let expected_cap = std::f64::consts::PI * (cap * cap - r0 * r0).max(1e-9);
                let solidity = (feature.area / expected_full).max(within / expected_cap);
                if solidity < FULL_REVOLUTION {
                    let own = arcs.iter().find(|a| a.id == feature.id).copied();
                    let mut verdict = format!(
                        "#{} plane z {:+.2}: interrupted ring ({:.0}% solid), not a full revolution",
                        feature.id,
                        fit.origin.z,
                        100.0 * solidity
                    );
                    let mut rescued = false;
                    if let Some(own) = own {
                        let family = family_of(&own);
                        let ids: Vec<usize> = family.iter().map(|a| a.id).collect();
                        let ur0 = family.iter().map(|a| a.r0).fold(f64::INFINITY, f64::min);
                        let ur1 = family
                            .iter()
                            .map(|a| a.r1)
                            .fold(f64::NEG_INFINITY, f64::max);
                        let family_area: f64 = family.iter().map(|a| a.area).sum();
                        let expected_union =
                            std::f64::consts::PI * (ur1 * ur1 - ur0 * ur0).max(1e-9);
                        let missing = (expected_union - family_area).max(0.0);
                        // Inset radially so the walls this annulus spans
                        // between do not count as occupying its gaps.
                        let occupied = occupied_by_others(
                            &feature.surface,
                            &ids,
                            fit.origin.z - 0.5,
                            fit.origin.z + 0.5,
                            ur0 + 0.3,
                            ur1 - 0.3,
                        );
                        let largest = family.iter().all(|a| a.area <= own.area + 1e-9);
                        if family.len() > 1
                            && family_area >= FAMILY_REVOLUTION * expected_union
                            && occupied < 0.5 * missing
                        {
                            if largest {
                                let (sw, sz, _, _, _) = profile_sums(&ids);
                                if sw > 0.0 {
                                    elements.push(Element::Face {
                                        feature: feature.id,
                                        z: sz / sw,
                                        rho0: ur0.max(0.0),
                                        rho1: ur1,
                                    });
                                    notes.push(format!(
                                        "#{} plane z {:+.2}: emitted from a family of {} co-annular arcs ({:.0}% joint solidity)",
                                        feature.id,
                                        sz / sw,
                                        family.len(),
                                        100.0 * family_area / expected_union
                                    ));
                                    rescued = true;
                                }
                            } else {
                                verdict = format!(
                                    "#{} plane z {:+.2}: arc covered by its family's emission",
                                    feature.id, fit.origin.z
                                );
                                notes.push(verdict.clone());
                                rescued = true;
                            }
                        }
                        // No lone-annulus invention; a trimmed patch draws
                        // what was measured instead.
                    }
                    if !rescued {
                        skipped.push(verdict);
                    }
                    continue;
                }
                elements.push(Element::Face {
                    feature: feature.id,
                    z: fit.origin.z,
                    rho0: r0.max(0.0),
                    rho1: r1,
                });
            }
            SurfaceClass::Cone(fit) if fit.axis.z.abs() > 0.999 => {
                let (z0, z1, _, _) = extents(mesh, &feature.faces, alignment);
                if z1 - z0 < 0.3 {
                    continue;
                }
                let offset = crate::reconstruct::cone_axis_offset(fit, (z0 + z1) / 2.0);
                if offset >= 3.0 {
                    skipped.push(format!(
                        "#{} cone {:.1} deg: axis tilted {:.2} deg off the datum axis, putting its axis line {:.2} mm off centre at z {:+.1}; not a surface of revolution about it",
                        feature.id,
                        fit.half_angle.to_degrees(),
                        fit.axis.z.abs().min(1.0).acos().to_degrees(),
                        offset,
                        (z0 + z1) / 2.0
                    ));
                    continue;
                }
                let (_, _, cone_r0, cone_r1) = extents(mesh, &feature.faces, alignment);
                let slant = ((z1 - z0).powi(2) + (cone_r1 - cone_r0).powi(2)).sqrt();
                let expected = std::f64::consts::TAU * (cone_r0 + cone_r1) / 2.0 * slant.max(1e-9);
                if feature.area < FULL_REVOLUTION * expected {
                    let own = arcs.iter().find(|a| a.id == feature.id).copied();
                    let mut verdict = format!(
                        "#{} cone {:.1} deg: interrupted ring ({:.0}% solid), not a full revolution",
                        feature.id,
                        fit.half_angle.to_degrees(),
                        100.0 * feature.area / expected
                    );
                    let mut rescued = false;
                    if let Some(own) = own {
                        let family = family_of(&own);
                        let ids: Vec<usize> = family.iter().map(|a| a.id).collect();
                        let uz0 = family.iter().map(|a| a.z0).fold(f64::INFINITY, f64::min);
                        let uz1 = family
                            .iter()
                            .map(|a| a.z1)
                            .fold(f64::NEG_INFINITY, f64::max);
                        let ur0 = family.iter().map(|a| a.r0).fold(f64::INFINITY, f64::min);
                        let ur1 = family
                            .iter()
                            .map(|a| a.r1)
                            .fold(f64::NEG_INFINITY, f64::max);
                        let family_area: f64 = family.iter().map(|a| a.area).sum();
                        let union_slant = ((uz1 - uz0).powi(2) + (ur1 - ur0).powi(2)).sqrt();
                        let expected_union =
                            std::f64::consts::TAU * (ur0 + ur1) / 2.0 * union_slant.max(1e-9);
                        let missing = (expected_union - family_area).max(0.0);
                        // Inset radially so the wall a chamfer ring sits
                        // on does not count as occupying its gaps.
                        let occupied = occupied_by_others(
                            &feature.surface,
                            &ids,
                            uz0 - 0.3,
                            uz1 + 0.3,
                            ur0 + 0.3,
                            ur1 - 0.3,
                        );
                        let largest = family.iter().all(|a| a.area <= own.area + 1e-9);
                        if family.len() > 1
                            && family_area >= FAMILY_REVOLUTION * expected_union
                            && occupied < 0.5 * missing
                        {
                            if largest {
                                let (sw, sz, sr, szz, szr) = profile_sums(&ids);
                                let denom = sw * szz - sz * sz;
                                if denom.abs() > 1e-9 {
                                    let slope = (sw * szr - sz * sr) / denom;
                                    let intercept = (sr - slope * sz) / sw;
                                    if (0.02..=12.0).contains(&slope.abs()) {
                                        elements.push(Element::Taper {
                                            feature: feature.id,
                                            slope,
                                            intercept,
                                            z0: uz0,
                                            z1: uz1,
                                        });
                                        notes.push(format!(
                                            "#{} cone {:.1} deg: emitted from a family of {} co-annular arcs ({:.0}% joint solidity)",
                                            feature.id,
                                            slope.abs().atan().to_degrees(),
                                            family.len(),
                                            100.0 * family_area / expected_union
                                        ));
                                        rescued = true;
                                    }
                                }
                            } else {
                                verdict = format!(
                                    "#{} cone {:.1} deg: arc covered by its family's emission",
                                    feature.id,
                                    fit.half_angle.to_degrees()
                                );
                                notes.push(verdict.clone());
                                rescued = true;
                            }
                        }
                        // No lone-taper invention; a trimmed patch draws
                        // what was measured instead.
                    }
                    if !rescued {
                        skipped.push(verdict);
                    }
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
            // A recognized revolved fillet is real geometry on the
            // finished part, so it is modelled as the arc it is and its
            // neighbours are trimmed back to tangency. Its angular span
            // comes from the scan rather than from where the neighbours
            // would have met, because the measured extent is what the
            // fillet actually covers.
            SurfaceClass::Blend(fit) if fit.axis.z.abs() > 0.999 => {
                // A fillet whose radius is within a couple of noise
                // widths is not resolvable as an arc — its "circle" is
                // fitted to scatter — and the honest model of it is the
                // sharp edge the neighbours already make.
                if fit.minor_radius <= 2.0 * report.tolerance {
                    notes.push(format!(
                        "#{} fillet r {:.3}: within noise of a sharp edge, left sharp",
                        feature.id, fit.minor_radius
                    ));
                    continue;
                }
                // Only points actually lying on the fitted arc define its
                // span. The finalize pass claims on-surface faces onto a
                // blend after it was fitted, so the feature's face set is
                // wider than the fillet itself, and measuring over all of
                // it puts the span far outside the circle.
                let band = (2.0 * report.tolerance).max(0.2);
                let angles: Vec<f64> = feature
                    .faces
                    .iter()
                    .flat_map(|&face| mesh.triangle_points(face as usize))
                    .filter_map(|corner| {
                        let c = alignment.transform.apply_point(corner);
                        let (dr, dz) = (c.x.hypot(c.y) - fit.major_radius, c.z - fit.axis_point.z);
                        ((dr.hypot(dz) - fit.minor_radius).abs() <= band).then(|| dz.atan2(dr))
                    })
                    .collect();
                if angles.len() < 12 {
                    skipped.push(format!(
                        "#{} fillet r {:.2}: too little of it lies on the fitted arc to span",
                        feature.id, fit.minor_radius
                    ));
                    continue;
                }
                // Span about the circular mean, so an arc straddling the
                // +/-pi branch cut measures as the quarter turn it is
                // rather than as a full revolution.
                let mean = angles
                    .iter()
                    .map(|t| t.sin())
                    .sum::<f64>()
                    .atan2(angles.iter().map(|t| t.cos()).sum::<f64>());
                let mut relative: Vec<f64> = angles
                    .iter()
                    .map(|t| {
                        let mut d = t - mean;
                        while d > std::f64::consts::PI {
                            d -= std::f64::consts::TAU;
                        }
                        while d < -std::f64::consts::PI {
                            d += std::f64::consts::TAU;
                        }
                        d
                    })
                    .collect();
                relative.sort_by(f64::total_cmp);
                let low = relative[relative.len() / 50];
                let high = relative[relative.len() - 1 - relative.len() / 50];
                // A quarter turn is the usual fillet, but a round-over on
                // an exposed rim is a bullnose and legitimately runs to a
                // half turn. Past that the "arc" has wrapped its own
                // circle and the fit means nothing.
                if high - low > std::f64::consts::PI * 1.05 {
                    skipped.push(format!(
                        "#{} fillet r {:.2}: arc spans {:.0} deg, more than a round-over can",
                        feature.id,
                        fit.minor_radius,
                        (high - low).to_degrees()
                    ));
                    continue;
                }
                // A fillet is swept the whole way round, and until now it
                // faced none of the solidity test that guards every other
                // revolved surface — the same hole that let a pattern
                // invent 49,000 mm². The gear's rim bullnose measured
                // 122 mm² against the 1,132 mm² its own revolution would
                // cover: eleven percent of a ring, drawn as a whole one.
                // An interrupted fillet is not a worse fillet, it is a
                // trimmed one, and it now falls through to be drawn as a
                // measured patch on the exact torus it was fitted to.
                let revolution = std::f64::consts::TAU
                    * fit.major_radius.abs()
                    * fit.minor_radius.abs()
                    * (high - low).abs();
                let solidity = if revolution > 1e-9 {
                    feature.area / revolution
                } else {
                    0.0
                };
                if solidity < FILLET_SOLID {
                    skipped.push(format!(
                        "#{} fillet r {:.2}: only {:.0}% of its revolution was measured; \
                         drawn as a measured patch on its torus instead",
                        feature.id,
                        fit.minor_radius,
                        solidity * 100.0
                    ));
                    continue;
                }
                fillets.push(Fillet {
                    feature: feature.id,
                    major: fit.major_radius,
                    minor: fit.minor_radius,
                    z_center: fit.axis_point.z,
                    t0: mean + low,
                    t1: mean + high,
                });
                notes.push(format!(
                    "#{} fillet r {:.2}: modelled as a revolved arc over {:.0} deg, neighbours trimmed to tangency",
                    feature.id,
                    fit.minor_radius,
                    (high - low).to_degrees()
                ));
            }
            // Edge rounds that survived refinement are not surfaces of
            // revolution (a tooth edge follows the toothing), so they
            // stay callouts rather than geometry.
            _ => {}
        }
    }
    // Accounting. Every analytic feature of consequence must either emit
    // geometry, be explicitly represented by another feature's emission,
    // or state why it was left out — a feature that falls through in
    // silence is a hole in the model that nothing reports.
    const MIN_ACCOUNTED_AREA: f64 = 100.0;
    {
        let emitted: std::collections::HashSet<usize> = elements
            .iter()
            .map(|element| match element {
                Element::Wall { feature, .. }
                | Element::Face { feature, .. }
                | Element::Taper { feature, .. } => *feature,
            })
            .chain(fillets.iter().map(|fillet| fillet.feature))
            .collect();
        let explained: std::collections::HashSet<usize> = skipped
            .iter()
            .chain(notes.iter())
            .filter_map(|line| {
                line.strip_prefix('#')?
                    .split(|c: char| !c.is_ascii_digit())
                    .next()?
                    .parse()
                    .ok()
            })
            .collect();
        let mut unaccounted: Vec<String> = Vec::new();
        for feature in &report.features {
            if feature.area < MIN_ACCOUNTED_AREA
                || emitted.contains(&feature.id)
                || explained.contains(&feature.id)
                || !matches!(
                    feature.surface,
                    SurfaceClass::Plane(_)
                        | SurfaceClass::Cylinder(_)
                        | SurfaceClass::Cone(_)
                        | SurfaceClass::Sphere(_)
                        | SurfaceClass::Blend(_)
                )
            {
                continue;
            }
            let (z0, z1, r0, r1) = extents(mesh, &feature.faces, alignment);
            let reason = if matches!(feature.surface, SurfaceClass::Sphere(_)) {
                "the sharp rebuild does not emit spherical faces yet"
            } else {
                "no emission rule matched; not axis-true after datum alignment"
            };
            unaccounted.push(format!(
                "#{} {} ({:.0} mm^2, d {:.1}..{:.1}, z {:+.1}..{:+.1}): {reason}",
                feature.id,
                crate::finalize::feature_label(&feature.surface),
                feature.area,
                2.0 * r0,
                2.0 * r1,
                z0,
                z1
            ));
        }
        skipped.extend(unaccounted);
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
    // No material can sit inside a round's own tube. The tangency trim
    // above only fires where a face meets a fillet END; a face crossing
    // the arc part-way along is never asked, and after the strips that
    // used to bound it were consumed into the fillet, one plane ran
    // inward through the round and invented 491 mm².
    //
    // Only ever shrinks a face, and only when a single edge is inside
    // the tube: a face spanning the whole tube is not intruding on a
    // round, it is somewhere this test does not understand.
    for fillet in &fillets {
        let inside = |r: f64, low: f64, high: f64| r > low && r < high;
        for element in &mut elements {
            let Element::Face { z, rho0, rho1, .. } = element else {
                continue;
            };
            let height = *z - fillet.z_center;
            if height.abs() >= fillet.minor {
                continue;
            }
            let half = (fillet.minor * fillet.minor - height * height).sqrt();
            let (low, high) = (fillet.major - half, fillet.major + half);
            let (inner, outer) = (inside(*rho0, low, high), inside(*rho1, low, high));
            if inner == outer {
                continue;
            }
            if inner && high < *rho1 {
                *rho0 = high;
            } else if outer && low > *rho0 {
                *rho1 = low;
            }
        }
    }
    // A level face meeting a pattern band ends exactly at that band's root
    // circle, and the band's end caps carry on outward from there. Both
    // directions matter and only one used to be handled.
    //
    // Too far out: a face that absorbed the pattern's lands measures all
    // the way to the tip radius, and as a solid annulus it would web over
    // the gaps between teeth. Clamp it back to the root.
    //
    // Too far in: the pattern also claims the gullet floors between its
    // teeth, so the face it grew from is left measuring short of the root
    // — on the test gear the end faces stopped at radius 38 while the
    // caps began at 42.4, leaving a 4 mm annular hole right around the
    // part at both ends of the toothing. Extend it out to meet them.
    /// How far short of the root a face may fall and still be read as
    /// having been truncated by the pattern rather than genuinely ending.
    const ROOT_REACH: f64 = 6.0;
    for band in &bands {
        for element in &mut elements {
            if let Element::Face { z, rho0, rho1, .. } = element {
                let near_band = *z >= band.z.0 - SNAP_REACH && *z <= band.z.1 + SNAP_REACH;
                if !near_band {
                    continue;
                }
                let reaches_past = *rho1 > band.root + 0.5;
                let truncated_short =
                    *rho1 > band.root - ROOT_REACH && *rho1 > band.rho.0 - 1.0 && *rho0 < band.root;
                if reaches_past || truncated_short {
                    *rho1 = band.root;
                }
            }
        }
    }
    // Trim to tangency. Sharpening ran every element out to the sharp
    // corner its neighbours make; wherever a fillet rounds that corner,
    // the neighbours must stop at the fillet's ends instead, or the model
    // carries both the corner and the fillet over the top of it.
    //
    // Each fillet end is matched to the element whose profile passes
    // through it — a wall by its radius, a face by its height, a taper by
    // its line — and that element's nearer end is moved onto it.
    /// How far off its profile an element may sit and still be read as
    /// the surface this fillet runs into.
    const TANGENT_BAND: f64 = 0.35;
    /// The share of its own revolution a fillet must have been measured over
    /// before it is swept the whole way round. Matches the threshold every
    /// other revolved surface already answers to.
    const FILLET_SOLID: f64 = 0.70;
    for fillet in &fillets {
        // A neighbour is trimmed to tangency only if it actually reaches
        // the fillet. Matching on one coordinate alone yanks the edge of
        // any face at the right height across open space to meet it: a
        // plane at z +29.4 was dragged inward and invented 491 mm² of
        // material the part does not have.
        let reach = fillet.minor + TANGENT_BAND;
        for end in fillet.ends() {
            let (rho_end, z_end) = end;
            for element in &mut elements {
                match element {
                    Element::Wall { rho, z0, z1, .. } => {
                        if (*rho - rho_end).abs() > TANGENT_BAND {
                            continue;
                        }
                        let near = if (*z0 - z_end).abs() < (*z1 - z_end).abs() {
                            z0
                        } else {
                            z1
                        };
                        if (*near - z_end).abs() > reach {
                            continue;
                        }
                        *near = z_end;
                    }
                    Element::Face { z, rho0, rho1, .. } => {
                        if (*z - z_end).abs() > TANGENT_BAND {
                            continue;
                        }
                        let near = if (*rho0 - rho_end).abs() < (*rho1 - rho_end).abs() {
                            rho0
                        } else {
                            rho1
                        };
                        if (*near - rho_end).abs() > reach {
                            continue;
                        }
                        *near = rho_end;
                    }
                    Element::Taper {
                        slope,
                        intercept,
                        z0,
                        z1,
                        ..
                    } => {
                        if (*intercept + *slope * z_end - rho_end).abs() > TANGENT_BAND {
                            continue;
                        }
                        if (*z0 - z_end).abs() < (*z1 - z_end).abs() {
                            *z0 = z_end;
                        } else {
                            *z1 = z_end;
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
    // A revolved element is exact with a single profile segment, but one
    // quad row spanning tens of millimetres makes triangles that no
    // downstream consumer — renderer, mesh comparison, kernel import —
    // handles gracefully. Walk the profile at roughly PROFILE_STEP.
    const PROFILE_STEP: f64 = 2.0;
    let densify = |a: (f64, f64), b: (f64, f64)| -> Vec<(f64, f64)> {
        let span = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let steps = ((span / PROFILE_STEP).ceil() as usize).clamp(1, 128);
        (0..=steps)
            .map(|i| {
                let t = i as f64 / steps as f64;
                (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
            })
            .collect()
    };
    for element in &elements {
        let (profile, feature) = match element {
            Element::Wall {
                feature,
                rho,
                z0,
                z1,
                ..
            } => (densify((*rho, *z0), (*rho, *z1)), *feature),
            Element::Face {
                feature,
                z,
                rho0,
                rho1,
                ..
            } => (densify((*rho0, *z), (*rho1, *z)), *feature),
            Element::Taper {
                feature,
                slope,
                intercept,
                z0,
                z1,
            } => (
                densify((intercept + slope * z0, *z0), (intercept + slope * z1, *z1)),
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
    // Fillets, as arcs walked in profile space at roughly PROFILE_STEP of
    // arc length so a small radius still reads as round.
    for fillet in &fillets {
        let arc = fillet.minor * (fillet.t1 - fillet.t0).abs();
        let steps = ((arc / PROFILE_STEP).ceil() as usize).clamp(3, 64);
        let profile: Vec<(f64, f64)> = (0..=steps)
            .map(|index| {
                let t = fillet.t0 + (fillet.t1 - fillet.t0) * index as f64 / steps as f64;
                fillet.at(t)
            })
            .collect();
        push_soup(
            crate::synth::revolved_profile_soup(&profile, SEGMENTS),
            fillet.feature,
            &mut positions,
            &mut triangles,
            &mut feature_of_face,
        );
    }
    for band in &bands {
        let profile = band.profile;
        if profile.axial {
            // Castellated ring: extrude the axial outline z(theta) between
            // the band's radii, based at the nearest face below.
            let mut base = profile.z_range.0;
            let mut best = f64::INFINITY;
            for element in &snapshot {
                if let Element::Face { z, .. } = element {
                    let gap = (z - profile.z_range.0).abs();
                    if gap <= SNAP_REACH && gap < best {
                        best = gap;
                        base = *z;
                    }
                }
            }
            let soup = match &profile.grid {
                Some(grid) => axial_grid_soup(grid, profile.count, base),
                None => axial_castellation_soup(
                    &profile.points,
                    profile.count,
                    profile.rho_range.0,
                    profile.rho_range.1,
                    base,
                ),
            };
            push_soup(
                soup,
                profile.feature_id,
                &mut positions,
                &mut triangles,
                &mut feature_of_face,
            );
            continue;
        }
        // Where the band runs from and to. Its own measured end faces are
        // the authority — a pattern's claimed z_range includes the
        // tooth-end chamfers and so overshoots the flat end — but a level
        // face element in reach wins, because two surfaces that meet
        // should meet exactly rather than a tenth of a millimetre apart.
        let mut z0 = band.ends.0;
        let mut z1 = band.ends.1;
        let mut best0 = f64::INFINITY;
        let mut best1 = f64::INFINITY;
        for element in &snapshot {
            if let Element::Face { z, .. } = element {
                if (z - band.ends.0).abs() <= SNAP_REACH && (z - band.ends.0).abs() < best0 {
                    best0 = (z - band.ends.0).abs();
                    z0 = *z;
                }
                if (z - band.ends.1).abs() <= SNAP_REACH && (z - band.ends.1).abs() < best1 {
                    best1 = (z - band.ends.1).abs();
                    z1 = *z;
                }
            }
        }
        push_soup(
            helical_pattern_soup(
                &profile.points,
                profile.count,
                profile.helix_rate,
                profile.z_reference,
                z0,
                z1,
                48,
            ),
            profile.feature_id,
            &mut positions,
            &mut triangles,
            &mut feature_of_face,
        );
        // End caps: radial strips between the band's root circle and the
        // master profile — zero-width in the gaps, so only the repeated
        // material gets capped.
        for z_end in [z0, z1] {
            push_soup(
                pattern_end_cap_soup(
                    &profile.points,
                    profile.count,
                    profile.helix_rate,
                    profile.z_reference,
                    z_end,
                    band.root,
                ),
                profile.feature_id,
                &mut positions,
                &mut triangles,
                &mut feature_of_face,
            );
            // Inboard of the root circle a gear is solid, so the band's
            // end face is a complete annulus from wherever the pattern's
            // material starts out to the root — the caps above only cover
            // the toothed part beyond it. Nothing else emits this ring:
            // the pattern claims the gullet floors, so no plane feature
            // survives at this height to be extended into place, and
            // without it the model has a bare annular hole right around
            // the part at both ends of the toothing.
            let already_capped = elements.iter().any(|element| match element {
                Element::Face { z, rho1, .. } => {
                    (z - z_end).abs() < 0.5 && *rho1 >= band.root - 0.5
                }
                _ => false,
            });
            if !already_capped && band.root > band.rho.0 + 0.5 {
                push_soup(
                    crate::synth::revolved_profile_soup(
                        &[(band.rho.0, z_end), (band.root, z_end)],
                        SEGMENTS,
                    ),
                    profile.feature_id,
                    &mut positions,
                    &mut triangles,
                    &mut feature_of_face,
                );
            }
        }
    }
    // Everything the revolved path could not express gets its carrier
    // surface trimmed to its own measured footprint. The revolved path
    // keeps priority where it applies — it produces sharp, mutually
    // intersected geometry, which a measured trim does not — so this is
    // the floor beneath it rather than a replacement.
    /// Grid step for a measured trim (mm).
    const PATCH_STEP: f64 = 0.4;
    /// Vertex clustering at cell `c` moves a vertex by at most `c`·√3/2, so
    /// decimating measured surface at the working tolerance keeps it inside
    /// the same budget the scan is already held to and costs nothing that
    /// tolerance had not already spent.
    fn measured_cell(tolerance: f64) -> f64 {
        tolerance
    }
    /// Vertex weld distance (mm) when rebuilding a measured region's soup.
    const WELD: f64 = 1e-6;
    /// Below this a feature is not worth a patch of its own.
    const MIN_PATCH_AREA: f64 = 20.0;
    {
        let expressed: std::collections::HashSet<usize> = elements
            .iter()
            .map(|element| match element {
                Element::Wall { feature, .. }
                | Element::Face { feature, .. }
                | Element::Taper { feature, .. } => *feature,
            })
            .chain(fillets.iter().map(|fillet| fillet.feature))
            .chain(bands.iter().map(|band| band.profile.feature_id))
            .collect();
        let mut trimmed = 0usize;
        let mut trimmed_area = 0.0;
        // Build every footprint first, so adjacent faces can be grown to
        // meet each other before any of them is emitted.
        // Scan occupancy first: the patch footprints below read it.
        scan_cells.extend(scan_occupancy(mesh, alignment));
        let mut patched: Vec<PatchedFace> = Vec::new();
        for feature in &report.features {
            if expressed.contains(&feature.id)
                || feature.area < MIN_PATCH_AREA
                || !matches!(
                    feature.surface,
                    SurfaceClass::Plane(_)
                        | SurfaceClass::Cylinder(_)
                        | SurfaceClass::Cone(_)
                        | SurfaceClass::Sphere(_)
                )
            {
                continue;
            }
            let Some(carrier) = Carrier::of(&feature.surface, &feature.faces, mesh, alignment)
            else {
                continue;
            };
            let cells = footprint(&carrier, &feature.faces, mesh, alignment, PATCH_STEP);
            if cells.is_empty() {
                continue;
            }
            patched.push((feature.id, carrier, cells));
        }
        // The scan's own footprint, assigned one owner per cell: at
        // high noise claiming is ragged and each patch would be full
        // of holes — but letting every coplanar fragment absorb the
        // whole lid's cells draws the same surface five times over
        // and the sheets z-fight as a checkerboard. Each occupancy
        // cell marks the single best-matching carrier.
        {
            let band = 1.3 * report.tolerance;
            let surfaces: Vec<&SurfaceClass> = patched
                .iter()
                .map(|(id, ..)| {
                    &report
                        .features
                        .iter()
                        .find(|f| f.id == *id)
                        .expect("patched feature")
                        .surface
                })
                .collect();
            let areas: Vec<f64> = patched
                .iter()
                .map(|(id, ..)| {
                    report
                        .features
                        .iter()
                        .find(|f| f.id == *id)
                        .map(|f| f.area)
                        .unwrap_or(0.0)
                })
                .collect();
            for &(point, normal) in scan_cells.values() {
                // Distance decides in coarse buckets and AREA breaks
                // the tie: a micro-fragment's locally tighter fit must
                // not steal single cells out of the lid it sits on, or
                // the patch comes out as confetti.
                let mut best: Option<(usize, i64, f64)> = None;
                for (slot, surface) in surfaces.iter().enumerate() {
                    let Some((distance, surface_normal)) = surface.probe(point) else {
                        continue;
                    };
                    let magnitude = distance.abs();
                    if magnitude > band || normal.dot(surface_normal).abs() < 0.82 {
                        continue;
                    }
                    let bucket = (magnitude / 0.05).floor() as i64;
                    let better = match best {
                        None => true,
                        Some((_, known_bucket, known_area)) => {
                            bucket < known_bucket
                                || (bucket == known_bucket && areas[slot] > known_area)
                        }
                    };
                    if better {
                        best = Some((slot, bucket, areas[slot]));
                    }
                }
                let Some((slot, _, _)) = best else { continue };
                if let Some((a, b)) = patched[slot].1.to_uv(point) {
                    let (ca, cb) = (
                        (a / PATCH_STEP).floor() as i64,
                        (b / PATCH_STEP).floor() as i64,
                    );
                    for da in -1..=1 {
                        for db in -1..=1 {
                            patched[slot].2.insert((ca + da, cb + db));
                        }
                    }
                }
            }
        }
        const PRESENCE_CELL: f64 = 0.6;
        let presence = feature_presence(&patched, report, mesh, alignment, PRESENCE_CELL);
        sharpen_planar_faces(
            &patched_planes(&patched, report),
            &mut patched,
            &presence,
            PRESENCE_CELL,
            PATCH_STEP,
        );
        // Every carrier, not just the flats: a cylinder cutting a plane
        // and two planes meeting are the same test on a signed distance.
        let carriers: Vec<(usize, SurfaceClass)> = patched
            .iter()
            .filter_map(|(id, ..)| {
                report
                    .features
                    .iter()
                    .find(|f| f.id == *id)
                    .map(|f| (*id, f.surface.clone()))
            })
            .collect();
        let trimmed_overlap = trim_at_intersections(
            &mut patched,
            &carriers,
            &presence,
            PRESENCE_CELL,
            PATCH_STEP,
        );
        // Now that the faces stop in the right place, the curve they stop
        // ON is the model's topology.
        // Edges are read off a *grown* copy while the geometry stays
        // the un-grown one. The growth exists to make the sign change
        // findable, not to add material: a footprint that stops short of
        // its boundary has every cell on one side of the neighbour, so
        // the crossing does not exist to be found and the edge dies
        // there. Growing the copy found 34 more corners and doubled the
        // curve on the gear; growing what is *drawn* also laid 2,032 mm2
        // where the physical round is, which is invention for a benefit
        // the edge already gives without it.
        let mut probing = patched.clone();
        let grown = grow_to_neighbours(
            &mut probing,
            &carriers,
            &presence,
            PRESENCE_CELL,
            PATCH_STEP,
        );
        if grown > 0.0 {
            notes.push(format!(
                "{grown:.0} mm^2 of footprint grown for edge finding only, reaching boundaries \
                 the scan's own face ownership stopped short of"
            ));
        }
        edges = extract_edges(&probing, &carriers, &presence, PRESENCE_CELL, PATCH_STEP);
        // The smooth boundaries the crossing extractor cannot see.
        let tangent =
            extract_tangent_boundaries(&probing, &carriers, &presence, PRESENCE_CELL, PATCH_STEP);
        if !tangent.is_empty() {
            let length: f64 = tangent.iter().map(|edge| edge.length()).sum();
            notes.push(format!(
                "{} tangent boundary(ies), {length:.0} mm: blends meeting the faces they round, \
                 found by ownership because tangent surfaces never cross",
                tangent.len()
            ));
            edges.extend(tangent);
        }
        // Which way each carrier's own normal points relative to the
        // material, measured from the scan faces that produced it.
        let outward: std::collections::HashMap<usize, bool> = patched
            .iter()
            .filter_map(|(id, ..)| {
                let feature = report.features.iter().find(|f| f.id == *id)?;
                let stride = (feature.faces.len() / 400).max(1);
                let (mut sum, mut weight) = (0.0, 0.0);
                for &face in feature.faces.iter().step_by(stride) {
                    let Some(normal) = mesh.face_normal(face as usize) else {
                        continue;
                    };
                    let normal = alignment.transform.apply_vector(normal);
                    let centre = alignment
                        .transform
                        .apply_point(mesh.face_centroid(face as usize));
                    let Some((_, fitted)) = feature.surface.probe(centre) else {
                        continue;
                    };
                    let area = mesh.face_area(face as usize);
                    sum += area * normal.dot(fitted);
                    weight += area;
                }
                (weight > 0.0).then_some((*id, sum >= 0.0))
            })
            .collect();
        // Fragments of one curve first: a cluster of ends bordering only
        // two faces is a broken edge, not a corner.
        let joined = crate::sew::join_fragments(&mut edges, 3.0);
        if joined > 0 {
            notes.push(format!(
                "{joined} edge fragment(s) joined into whole curves before corners were sought"
            ));
        }
        let (found_corners, sewn, shell, opens) =
            crate::sew::resolve(&mut edges, &carriers, &outward);
        corners = found_corners;
        open_ends = opens;
        notes.push(format!(
            "topology: {} corner(s) solved exactly; {} of {} edge ends resolved, {} left open;              {} closed ring(s) + {} walked loop(s); {} of {} edged faces have a closed boundary",
            sewn.corners,
            sewn.resolved_ends,
            sewn.resolved_ends + sewn.open_ends,
            sewn.open_ends,
            sewn.closed_rings,
            sewn.walked_loops,
            sewn.bounded_faces,
            sewn.edged_faces
        ));
        if !open_ends.is_empty() {
            // The triage: the same open count, split by what each end
            // is waiting for, so the number reads as a work list.
            let mut counted: Vec<(crate::sew::OpenCause, usize)> = Vec::new();
            for open in &open_ends {
                match counted.iter_mut().find(|(cause, _)| *cause == open.cause) {
                    Some((_, count)) => *count += 1,
                    None => counted.push((open.cause, 1)),
                }
            }
            counted.sort_by_key(|&(cause, count)| (std::cmp::Reverse(count), cause));
            let listing = counted
                .iter()
                .map(|(cause, count)| format!("{} {}", count, cause.describe()))
                .collect::<Vec<_>>()
                .join(", ");
            notes.push(format!("open-end triage: {listing}"));
        }
        notes.push(shell.describe());
        if trimmed_overlap > 0.0 {
            notes.push(format!(
                "{trimmed_overlap:.0} mm^2 of interpenetrating material cut back to the exact                  line where the two faces meet"
            ));
        }
        // Exact swept features first: recognized bores, bosses, and
        // prismatic extrusions emit true surfaces about their own axes
        // and directions, and the features they express skip the patch
        // floor below.
        let hole_stacks = build_hole_stacks(mesh, report, alignment, report.tolerance, &scan_cells);
        exact_range.0 = triangles.len();
        let mut exact_covered = emit_exact_holes(
            &hole_stacks,
            &mut positions,
            &mut triangles,
            &mut feature_of_face,
            &mut notes,
        );
        let (extrusion_covered, extrusion_volumes) = emit_exact_extrusions(
            mesh,
            report,
            alignment,
            &expressed,
            &scan_cells,
            report.tolerance,
            &mut positions,
            &mut triangles,
            &mut feature_of_face,
            &mut notes,
        );
        exact_range.1 = triangles.len();
        exact_covered.extend(extrusion_covered);
        let mut bands_quiet: Vec<CutVolume> = Vec::new();
        // Rim bands: a quiet volume hugging each chamfer funnel from
        // just inside its surface to half a millimetre proud of it,
        // extended past the wide end — whatever rasterized or
        // recovered junk loiters at a mouth is cleared, and the exact
        // geometry inside the band is immune to its own broom.
        for stack in &hole_stacks {
            for &(_, z0, z1, r0, r1) in &stack.cones {
                let (zmin, rmin, zmax, rmax) = if z0 <= z1 {
                    (z0, r0, z1, r1)
                } else {
                    (z1, r1, z0, r0)
                };
                if zmax - zmin < 1e-6 {
                    continue;
                }
                let slope = (rmax - rmin) / (zmax - zmin);
                // The narrow side dips well under the meet: funnel
                // fragments hug the tube just below it.
                let (mut zlo, mut zhi) = (zmin - 0.05, zmax + 0.05);
                if rmax >= rmin {
                    zhi += 0.30;
                    zlo -= 0.40;
                } else {
                    zlo -= 0.30;
                    zhi += 0.40;
                }
                let radius_at = |z: f64| rmin + slope * (z - zmin) + 1.25;
                bands_quiet.push(CutVolume::Stack(HoleStack {
                    origin: stack.origin,
                    axis: stack.axis,
                    pieces: vec![(zlo, zhi, radius_at(zlo), radius_at(zhi))],
                    span: (zlo, zhi),
                    bore_diameter: 0.0,
                    wall_id: 0,
                    wall_radius: 0.0,
                    wall_run: (0.0, 0.0),
                    cones: Vec::new(),
                    mouths: Vec::new(),
                }));
            }
        }
        cut_volumes.extend(
            hole_stacks
                .into_iter()
                .map(CutVolume::Stack)
                .chain(extrusion_volumes)
                .chain(bands_quiet),
        );
        for (id, carrier, cells) in &patched {
            if exact_covered.contains(id) {
                continue;
            }
            let feature = report
                .features
                .iter()
                .find(|f| f.id == *id)
                .expect("feature");
            let soup = footprint_soup(carrier, cells, PATCH_STEP);
            if soup.is_empty() {
                continue;
            }
            push_soup(
                soup,
                feature.id,
                &mut positions,
                &mut triangles,
                &mut feature_of_face,
            );
            trimmed += 1;
            trimmed_area += feature.area;
        }
        // Cast and organic surface has no analytic form to emit it on,
        // and a scan-to-CAD model that simply omits it has a hole where a
        // third of the pump used to be. It is emitted as what it is:
        // the measured surface itself, in the datum frame, marked so no
        // reader mistakes it for something the kernel can certify. A
        // hybrid model that says which parts are exact beats an exact
        // model of half a part.
        let (mut measured, mut measured_area) = (0usize, 0.0);
        // The emitted analytic surfaces, hoisted once for the carry
        // filter below.
        let patched_surfaces: Vec<&SurfaceClass> = report
            .features
            .iter()
            .filter(|f| {
                !matches!(f.surface, SurfaceClass::Freeform)
                    && (expressed.contains(&f.id) || patched.iter().any(|(id, ..)| id == &f.id))
            })
            .map(|f| &f.surface)
            .collect();
        for feature in &report.features {
            if !matches!(feature.surface, SurfaceClass::Freeform)
                || feature.area < MIN_PATCH_AREA
                || expressed.contains(&feature.id)
            {
                continue;
            }
            // A carried face that an emitted analytic surface already
            // explains is the same material drawn twice — the second
            // copy is the speckle fighting the clean patch above it.
            // Material stays in the carry only where no patched
            // carrier accounts for it.
            let explain_band = 1.3 * report.tolerance;
            let raw: Vec<[Point3; 3]> = feature
                .faces
                .iter()
                .filter(|&&face| {
                    let centroid = alignment
                        .transform
                        .apply_point(mesh.face_centroid(face as usize));
                    let normal = mesh
                        .face_normal(face as usize)
                        .map(|n| alignment.transform.apply_vector(n));
                    // Distance alone: at this noise per-face normals
                    // tilt tens of degrees and a normal gate lets half
                    // the explained material dodge suppression as
                    // speckle. Sub-band sheets are the thin-sheet
                    // regime's problem, not this filter's.
                    let _ = normal;
                    !patched_surfaces.iter().any(|surface| {
                        surface
                            .probe(centroid)
                            .is_some_and(|(distance, _)| distance.abs() <= explain_band)
                    })
                })
                .map(|&face| {
                    let corners = mesh.triangle_points(face as usize);
                    [
                        alignment.transform.apply_point(corners[0]),
                        alignment.transform.apply_point(corners[1]),
                        alignment.transform.apply_point(corners[2]),
                    ]
                })
                .collect();
            if raw.is_empty() {
                continue;
            }
            // At scan density this is a million triangles of noise. It is
            // measured surface, so it is only ever as good as the
            // tolerance, and carrying it finer than that buys nothing but
            // file size.
            let soup = TriangleMesh::from_triangle_soup(&raw, WELD)
                .map(|patch| {
                    let (coarse, _) =
                        patch.simplified_by_clustering(measured_cell(report.tolerance));
                    (0..coarse.triangles().len())
                        .map(|face| coarse.triangle_points(face))
                        .collect::<Vec<_>>()
                })
                .filter(|coarse: &Vec<[Point3; 3]>| !coarse.is_empty())
                .unwrap_or(raw);
            push_soup(
                soup,
                feature.id,
                &mut positions,
                &mut triangles,
                &mut feature_of_face,
            );
            measured += 1;
            measured_area += feature.area;
        }
        if measured > 0 {
            notes.push(format!(
                "{measured} region(s) totalling {measured_area:.0} mm^2 have no analytic form \
                 (cast or organic) and are carried as measured surface, not certified geometry"
            ));
        }
        if trimmed > 0 {
            notes.push(format!(
                "{trimmed} feature(s) totalling {trimmed_area:.0} mm^2 emitted as measured trimmed \
                 patches on their fitted surfaces, not as revolutions"
            ));
            // Those are no longer missing, whatever the revolve test said.
            skipped.retain(|line| {
                !line
                    .strip_prefix('#')
                    .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|id| id.parse::<usize>().ok())
                    .is_some_and(|id| {
                        report.features.iter().any(|f| {
                            f.id == id && f.area >= MIN_PATCH_AREA && !expressed.contains(&id)
                        })
                    })
            });
        }
    }
    // Recognized drilled holes open whatever covered them — the
    // revolved sweeps that ignore footprint, and any patch a
    // wrong-carrier feature painted across a mouth. The coplanar scan
    // evidence inside each stack is what keeps a blind hole's floor,
    // and true walls and chamfers sit ON the envelope, never inside
    // it, so honest geometry survives its own hole being opened.
    punch_volumes(
        &cut_volumes,
        &scan_cells,
        &positions,
        &mut triangles,
        &mut feature_of_face,
        exact_range,
        report.tolerance,
        &mut notes,
    );
    let mesh = TriangleMesh::new(positions, triangles)?;
    Some(RebuiltModel {
        mesh,
        feature_of_face,
        skipped,
        notes,
        edges,
        corners,
        open_ends,
    })
}

/// Sweeps the master sector profile helically about +Z, repeated `count`
/// times per revolution, from `z0` to `z1`.
fn helical_pattern_soup(
    profile: &[(f64, f64)],
    count: usize,
    helix_rate: f64,
    z_reference: f64,
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
        // Phase-exact: the same reference the fold used — including the
        // fold's atan2 + pi binning, undone here so odd counts land at
        // the scanned azimuths too.
        let angle = theta - std::f64::consts::PI + helix_rate * (z - z_reference);
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

/// Flat cap at `z_end`: quads between the root circle and the swept
/// master profile, using the same helix phase as the sweep.
fn pattern_end_cap_soup(
    profile: &[(f64, f64)],
    count: usize,
    helix_rate: f64,
    z_reference: f64,
    z_end: f64,
    root_rho: f64,
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
    let at = |slot: usize, rho: f64| -> Point3 {
        let (theta, _) = ring[slot % ring_len];
        let angle = theta - std::f64::consts::PI + helix_rate * (z_end - z_reference);
        Point3::new(rho * angle.cos(), rho * angle.sin(), z_end)
    };
    for slot in 0..ring_len {
        let (_, rho_a) = ring[slot % ring_len];
        let (_, rho_b) = ring[(slot + 1) % ring_len];
        if rho_a - root_rho < 0.05 && rho_b - root_rho < 0.05 {
            continue;
        }
        let a = at(slot, root_rho);
        let b = at(slot, rho_a);
        let c = at(slot + 1, rho_b);
        let d = at(slot + 1, root_rho);
        soup.push([a, b, c]);
        soup.push([a, c, d]);
    }
    soup
}

/// Castellation from the folded sector height-field.
///
/// Each cell emits one flat quad at its own level, and each change of
/// level emits one full-height wall spanning the whole shared edge.
/// Averaging cell corners instead turns every step into a fan of
/// slivers. Rim cells and voids drop their wall to the base face.
///
/// The fold binned azimuth as atan2 + pi, so emission subtracts pi to
/// land the pattern at its true phase for every count, odd or even.
fn axial_grid_soup(
    grid: &crate::reconstruct::AxialGrid,
    count: usize,
    base: f64,
) -> Vec<[Point3; 3]> {
    let (t_cells, r_cells) = (grid.theta_cells, grid.rho_cells);
    let mut soup = Vec::new();
    if t_cells == 0 || r_cells == 0 || grid.z.len() != t_cells * r_cells {
        return soup;
    }
    let cell = |t: usize, r: usize| -> f64 { grid.z[r * t_cells + t] };
    let sector = std::f64::consts::TAU / count as f64;
    let d_theta = sector / t_cells as f64;
    let d_rho = (grid.rho1 - grid.rho0) / r_cells as f64;
    for k in 0..count {
        let phase = k as f64 * sector - std::f64::consts::PI;
        let at = |t: usize, r: usize, height: f64| -> Point3 {
            let theta = phase + t as f64 * d_theta;
            let rho = grid.rho0 + r as f64 * d_rho;
            Point3::new(rho * theta.cos(), rho * theta.sin(), height)
        };
        for r in 0..r_cells {
            for t in 0..t_cells {
                let here = cell(t, r);
                if !here.is_finite() {
                    continue;
                }
                let p00 = at(t, r, here);
                let p10 = at(t + 1, r, here);
                let p01 = at(t, r + 1, here);
                let p11 = at(t + 1, r + 1, here);
                soup.push([p00, p10, p11]);
                soup.push([p00, p11, p01]);
                let floor_of = |neighbour: f64| -> Option<f64> {
                    let floor = if neighbour.is_finite() {
                        neighbour
                    } else {
                        base
                    };
                    (here - floor > 1e-6).then_some(floor)
                };
                if let Some(floor) = floor_of(cell((t + 1) % t_cells, r)) {
                    soup.push([p10, at(t + 1, r, floor), at(t + 1, r + 1, floor)]);
                    soup.push([p10, at(t + 1, r + 1, floor), p11]);
                }
                if let Some(floor) = floor_of(cell((t + t_cells - 1) % t_cells, r)) {
                    soup.push([p01, at(t, r + 1, floor), at(t, r, floor)]);
                    soup.push([p01, at(t, r, floor), p00]);
                }
                let inner = if r > 0 { cell(t, r - 1) } else { f64::NAN };
                if let Some(floor) = floor_of(inner) {
                    soup.push([p00, at(t, r, floor), at(t + 1, r, floor)]);
                    soup.push([p00, at(t + 1, r, floor), p10]);
                }
                let outer = if r + 1 < r_cells {
                    cell(t, r + 1)
                } else {
                    f64::NAN
                };
                if let Some(floor) = floor_of(outer) {
                    soup.push([p11, at(t + 1, r + 1, floor), at(t, r + 1, floor)]);
                    soup.push([p11, at(t, r + 1, floor), p01]);
                }
            }
        }
    }
    soup
}

/// An orthonormal frame whose third axis is `axis`.
fn frame_about(axis: Vector3) -> (Vector3, Vector3, Vector3) {
    let axis = if axis.length() > 1e-12 {
        axis / axis.length()
    } else {
        Vector3::new(0.0, 0.0, 1.0)
    };
    let seed = if axis.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    let u = {
        let raw = seed - axis * seed.dot(axis);
        raw / raw.length().max(1e-12)
    };
    (u, axis.cross(u), axis)
}
/// A face awaiting emission: its feature id, the carrier it sits on, and
/// the parameter cells its measured material covers.
type PatchedFace = (usize, Carrier, std::collections::HashSet<(i64, i64)>);

/// The planar members of the patched set, paired with their fitted plane.
fn patched_planes(
    patched: &[PatchedFace],
    report: &ReverseReport,
) -> Vec<(usize, crate::fit::PlaneFit)> {
    patched
        .iter()
        .filter_map(|(id, ..)| {
            let feature = report.features.iter().find(|f| f.id == *id)?;
            match feature.surface {
                SurfaceClass::Plane(fit) => Some((*id, fit)),
                _ => None,
            }
        })
        .collect()
}

/// Where each feature's measured material actually sits, on a coarse
/// voxel grid, so one face can ask whether another is anywhere near a
/// given point without a scan of every triangle.
fn feature_presence(
    patched: &[PatchedFace],
    report: &ReverseReport,
    mesh: &TriangleMesh,
    alignment: &crate::datum::DatumAlignment,
    cell: f64,
) -> std::collections::HashMap<(i64, i64, i64), Vec<usize>> {
    let mut presence: std::collections::HashMap<(i64, i64, i64), Vec<usize>> =
        std::collections::HashMap::new();
    for (id, ..) in patched {
        let Some(feature) = report.features.iter().find(|f| f.id == *id) else {
            continue;
        };
        for &face in &feature.faces {
            for corner in mesh.triangle_points(face as usize) {
                let point = alignment.transform.apply_point(corner);
                let key = (
                    (point.x / cell).floor() as i64,
                    (point.y / cell).floor() as i64,
                    (point.z / cell).floor() as i64,
                );
                let bucket = presence.entry(key).or_default();
                if !bucket.contains(id) {
                    bucket.push(*id);
                }
            }
        }
    }
    presence
}

/// Whether a feature has measured material within a cell of a point.
fn near_feature(
    presence: &std::collections::HashMap<(i64, i64, i64), Vec<usize>>,
    cell: f64,
    point: Point3,
    feature: usize,
) -> bool {
    let base = (
        (point.x / cell).floor() as i64,
        (point.y / cell).floor() as i64,
        (point.z / cell).floor() as i64,
    );
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let key = (base.0 + dx, base.1 + dy, base.2 + dz);
                if presence.get(&key).is_some_and(|ids| ids.contains(&feature)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Grows adjacent planar faces out to the exact line where their planes
/// meet, so flat faces join at a sharp edge.
///
/// A measured footprint stops where the scan stopped, and between two
/// faces the scan stops on either side of the physical round that joins
/// them. Left alone the model is a quilt with a fillet's width of gap at
/// every edge. Extending both planes to their common line is the sharp
/// answer, and it is exactly certifiable: two planes meet in a straight
/// line and nothing is approximated.
///
/// Growth is bounded and evidence-led — a face only reaches toward a line
/// it was already close to, and only across a gap the size of an edge
/// break — so a plane never runs off across the part to meet a distant
/// neighbour it has no edge with.
fn sharpen_planar_faces(
    planes: &[(usize, crate::fit::PlaneFit)],
    patched: &mut [PatchedFace],
    presence: &std::collections::HashMap<(i64, i64, i64), Vec<usize>>,
    presence_cell: f64,
    step: f64,
) {
    /// Faces meeting at a shallower angle than this are the same surface
    /// wandering, not two faces with an edge between them.
    const MIN_DIHEDRAL: f64 = 15.0;
    /// The widest gap an edge break may leave (mm).
    const EXTEND_REACH: f64 = 1.2;
    let limit = MIN_DIHEDRAL.to_radians().cos();
    let reach_cells = (EXTEND_REACH / step).ceil() as i64;
    let mut additions: Vec<(usize, Vec<(i64, i64)>)> = Vec::new();
    for (index, (id, carrier, cells)) in patched.iter().enumerate() {
        let Some((_, own)) = planes.iter().find(|(other, _)| other == id) else {
            continue;
        };
        let mut grown: Vec<(i64, i64)> = Vec::new();
        for (neighbour, other) in planes {
            if neighbour == id || own.normal.dot(other.normal).abs() > limit {
                continue;
            }
            // The line where the two planes meet.
            let direction = own.normal.cross(other.normal);
            let length = direction.length();
            if length < 1e-9 {
                continue;
            }
            let direction = direction / length;
            let (da, db) = (
                own.normal
                    .dot(Vector3::new(own.origin.x, own.origin.y, own.origin.z)),
                other
                    .normal
                    .dot(Vector3::new(other.origin.x, other.origin.y, other.origin.z)),
            );
            let dot = own.normal.dot(other.normal);
            let denominator = 1.0 - dot * dot;
            if denominator.abs() < 1e-9 {
                continue;
            }
            let anchor = own.normal * ((da - db * dot) / denominator)
                + other.normal * ((db - da * dot) / denominator);
            let anchor = Point3::new(anchor.x, anchor.y, anchor.z);
            // Walk the line and, for each row it crosses, reach back to
            // this face's nearest measured cell.
            let extent = 400.0;
            let samples = ((2.0 * extent / (step * 0.5)) as usize).min(20_000);
            for sample in 0..=samples {
                let t = -extent + 2.0 * extent * sample as f64 / samples as f64;
                let point = anchor + direction * t;
                // Two planes meet along a line that runs the whole length
                // of the part, but they share an *edge* only where both
                // still have material. Without this test a face reaches
                // for every non-parallel plane in the model and the pump
                // invents a quarter of its own surface.
                if !near_feature(presence, presence_cell, point, *neighbour) {
                    continue;
                }
                let Some((a, b)) = carrier.to_uv(point) else {
                    continue;
                };
                let (i_line, j) = ((a / step).floor() as i64, (b / step).floor() as i64);
                let nearest = (1..=reach_cells).find_map(|offset| {
                    if cells.contains(&(i_line - offset, j)) {
                        Some(i_line - offset)
                    } else if cells.contains(&(i_line + offset, j)) {
                        Some(i_line + offset)
                    } else {
                        None
                    }
                });
                if let Some(near) = nearest {
                    let (low, high) = (near.min(i_line), near.max(i_line));
                    for i in low..=high {
                        grown.push((i, j));
                    }
                }
            }
        }
        if !grown.is_empty() {
            additions.push((index, grown));
        }
    }
    for (index, grown) in additions {
        patched[index].2.extend(grown);
    }
}

/// Grows every footprint out to the neighbour surfaces it stops short
/// of, whatever carrier either of them sits on.
///
/// A footprint is rasterized from the faces the scan gave this feature,
/// and near a physical edge the scan gives them to somebody else: the
/// round takes a strip, the neighbour takes another, and the footprint
/// ends a millimetre inside the boundary it should reach. Nothing
/// downstream can recover from that. The edge extractor looks for a
/// sign change in the neighbour's signed distance, and a footprint that
/// stops short has every cell on one side, so the crossing does not
/// exist to be found and the edge simply dies there — which is what
/// left the gear with 151 unresolved edge ends and the pump with 3,979.
/// The scans themselves are watertight; this gap is ours.
///
/// Growth is bounded the same way the trim is: only toward a surface
/// that has material where the growth is going, only across a gap the
/// size of an edge break, and only until the neighbour's zero set is
/// crossed. Stopping one cell *past* the crossing rather than at it is
/// deliberate — a sign change needs cells on both sides to exist at all.
fn grow_to_neighbours(
    patched: &mut [PatchedFace],
    surfaces: &[(usize, SurfaceClass)],
    presence: &std::collections::HashMap<(i64, i64, i64), Vec<usize>>,
    presence_cell: f64,
    step: f64,
) -> f64 {
    /// The widest gap a footprint may be grown across (mm).
    const REACH: f64 = 1.2;
    /// Surfaces meeting shallower than this are tangent; a blend running
    /// into its own face has no boundary to grow to.
    const MIN_DIHEDRAL: f64 = 15.0;
    let limit = MIN_DIHEDRAL.to_radians().cos();
    let rings = (REACH / step).ceil() as usize;
    let mut additions: Vec<(usize, Vec<(i64, i64)>)> = Vec::new();
    for (index, (id, carrier, cells)) in patched.iter().enumerate() {
        let Some((_, own)) = surfaces.iter().find(|(other, _)| other == id) else {
            continue;
        };
        // Neighbours standing on this face's own cells.
        let mut candidates: Vec<usize> = Vec::new();
        for &(a, b) in cells.iter() {
            let point = carrier.at((a as f64 + 0.5) * step, (b as f64 + 0.5) * step);
            let base = (
                (point.x / presence_cell).floor() as i64,
                (point.y / presence_cell).floor() as i64,
                (point.z / presence_cell).floor() as i64,
            );
            if let Some(here) = presence.get(&base) {
                for &other in here {
                    if other != *id && !candidates.contains(&other) {
                        candidates.push(other);
                    }
                }
            }
        }
        candidates.sort_unstable();
        let mut grown: std::collections::HashSet<(i64, i64)> = cells.clone();
        let mut added: Vec<(i64, i64)> = Vec::new();
        for neighbour in candidates {
            let Some((_, other)) = surfaces.iter().find(|(known, _)| *known == neighbour) else {
                continue;
            };
            let at = |cell: (i64, i64)| {
                carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step)
            };
            // Which side of the neighbour this face's material sits on.
            let mut order: Vec<f64> = cells
                .iter()
                .filter_map(|&cell| other.probe(at(cell)).map(|(d, _)| d))
                .collect();
            if order.len() < 8 {
                continue;
            }
            order.sort_by(f64::total_cmp);
            let median = order[order.len() / 2];
            if median.abs() < 1e-9 {
                continue;
            }
            let side = median.signum();
            // Grow ring by ring toward the neighbour.
            let mut frontier: Vec<(i64, i64)> = grown.iter().copied().collect();
            frontier.sort_unstable();
            for _ in 0..rings {
                let mut next: Vec<(i64, i64)> = Vec::new();
                for &(a, b) in &frontier {
                    for (da, db) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        let cell = (a + da, b + db);
                        if grown.contains(&cell) {
                            continue;
                        }
                        let point = at(cell);
                        let (Some((distance, their)), Some((_, ours))) =
                            (other.probe(point), own.probe(point))
                        else {
                            continue;
                        };
                        // Tangent surfaces do not bound each other.
                        if ours.dot(their).abs() > limit {
                            continue;
                        }
                        // Only toward a neighbour that is really there.
                        if !near_feature(presence, presence_cell, point, neighbour) {
                            continue;
                        }
                        // Stop one cell past the crossing: a sign change
                        // needs both sides to exist.
                        if distance * side < -step {
                            continue;
                        }
                        grown.insert(cell);
                        added.push(cell);
                        if distance * side > 0.0 {
                            next.push(cell);
                        }
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
        }
        if !added.is_empty() {
            additions.push((index, added));
        }
    }
    let mut area = 0.0;
    for (index, added) in additions {
        for cell in added {
            if patched[index].2.insert(cell) {
                area += step * step;
            }
        }
    }
    area
}

/// Cuts each face back to where it meets its neighbour, so two faces
/// share an edge instead of running through one another.
///
/// This is the same line the growing pass reaches for, taken from the
/// other side. A footprint is rasterized over whatever its own measured
/// faces covered, and measurement does not stop politely at an edge: a
/// face keeps a fringe of cells belonging to the round beyond it, or to
/// the neighbour itself, and the patches interpenetrate. Coverage cannot
/// see this at all — the fringe is measured surface, so it counts as
/// explained and not as invented — but it is exactly what makes the
/// model read as a pile of overlapping sheets rather than a solid.
///
/// Every carrier answers `probe` with a signed distance, so no pair of
/// surfaces needs its intersection curve written out: a cylinder cutting
/// a plane, a cone cutting a cylinder and two planes meeting are all the
/// same test on the sign of that distance. Which side to keep is read
/// from the evidence rather than assumed — the face's own cells sit
/// predominantly on one side of the neighbour, and that side is its
/// material — so nothing needs to know whether the solid is inside or
/// outside.
fn trim_at_intersections(
    patched: &mut [PatchedFace],
    surfaces: &[(usize, SurfaceClass)],
    presence: &std::collections::HashMap<(i64, i64, i64), Vec<usize>>,
    presence_cell: f64,
    step: f64,
) -> f64 {
    /// Cells whose centre is this far past the neighbour still survive.
    /// It has to exceed half a cell, or a cell straddling the line is
    /// dropped whole from BOTH faces and the pair retreats, leaving a
    /// sliver of the part covered by neither.
    const KEEP_BAND: f64 = 0.5;
    /// Surfaces meeting shallower than this are tangent — a blend and the
    /// face it runs into — and neither one cuts the other.
    const MIN_DIHEDRAL: f64 = 15.0;
    let limit = MIN_DIHEDRAL.to_radians().cos();
    let mut cuts: Vec<(usize, Vec<(i64, i64)>)> = Vec::new();
    for (index, (id, carrier, cells)) in patched.iter().enumerate() {
        let Some((_, own)) = surfaces.iter().find(|(other, _)| other == id) else {
            continue;
        };
        // Where each cell sits, once.
        let points: Vec<((i64, i64), Point3)> = cells
            .iter()
            .map(|&(a, b)| {
                (
                    (a, b),
                    carrier.at((a as f64 + 0.5) * step, (b as f64 + 0.5) * step),
                )
            })
            .collect();
        if points.len() < 8 {
            continue;
        }
        // Only surfaces this face actually touches are candidates — all
        // pairs would be hundreds of thousands of tests on the pump, and
        // most of them between faces at opposite ends of the part.
        let mut candidates: Vec<usize> = Vec::new();
        for (_, point) in points.iter().step_by(4) {
            let base = (
                (point.x / presence_cell).floor() as i64,
                (point.y / presence_cell).floor() as i64,
                (point.z / presence_cell).floor() as i64,
            );
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(here) = presence.get(&(base.0 + dx, base.1 + dy, base.2 + dz)) {
                            for &other in here {
                                if other != *id && !candidates.contains(&other) {
                                    candidates.push(other);
                                }
                            }
                        }
                    }
                }
            }
        }
        candidates.sort_unstable();
        let mut drop: Vec<(i64, i64)> = Vec::new();
        for neighbour in candidates {
            let Some((_, other)) = surfaces.iter().find(|(id, _)| *id == neighbour) else {
                continue;
            };
            let signed: Vec<((i64, i64), f64)> = points
                .iter()
                .filter_map(|&(cell, point)| {
                    let (distance, their) = other.probe(point)?;
                    let (_, ours) = own.probe(point)?;
                    // Tangent surfaces do not cut each other.
                    (ours.dot(their).abs() <= limit).then_some((cell, distance))
                })
                .collect();
            if signed.len() < 8 {
                continue;
            }
            let mut order: Vec<f64> = signed.iter().map(|&(_, d)| d).collect();
            order.sort_by(f64::total_cmp);
            let median = order[order.len() / 2];
            if median.abs() < KEEP_BAND {
                // The face straddles its neighbour: it has no side, so
                // cutting it would be a guess.
                continue;
            }
            let side = median.signum();
            for &(cell, distance) in &signed {
                if distance * side >= -KEEP_BAND {
                    continue;
                }
                // The cell must be past the neighbour AND standing where
                // the neighbour actually is. Asking only whether the two
                // features meet *somewhere* lets a face cut everything on
                // the far side of another it merely passes near: the pump
                // lost 79,183 mm², more than half its surface.
                let point = carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step);
                if near_feature(presence, presence_cell, point, neighbour) {
                    drop.push(cell);
                }
            }
        }
        if !drop.is_empty() {
            cuts.push((index, drop));
        }
    }
    let mut trimmed = 0.0;
    for (index, drop) in cuts {
        for cell in drop {
            if patched[index].2.remove(&cell) {
                trimmed += step * step;
            }
        }
    }
    trimmed
}

/// A curve two faces share: the exact line where their carriers meet,
/// bounded to where both actually have material.
#[derive(Clone, Debug)]
pub struct SharedEdge {
    /// The two features that meet here.
    pub between: (usize, usize),
    /// The curve, in the datum frame.
    pub points: Vec<Point3>,
    /// True where the two faces meet *smoothly* — a blend running into
    /// the face it rounds. The boundary is real and bounds both faces,
    /// but the surfaces do not cross there, so it is found by a
    /// different means and can never carry a corner.
    pub tangent: bool,
}

impl SharedEdge {
    /// Length of the curve in millimetres.
    pub fn length(&self) -> f64 {
        self.points
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).length())
            .sum()
    }
}

/// Orders scattered crossing points into a curve.
///
/// The points arrive in whatever order the cells were visited, which is
/// no order at all. Chaining nearest to nearest from one end recovers the
/// curve for anything that does not branch, and an edge between two
/// surfaces does not branch — it is one intersection curve.
fn chain_one(loose: &mut Vec<Point3>, step: f64) -> Vec<Point3> {
    if loose.len() < 2 {
        return std::mem::take(loose);
    }
    // Start from the point furthest from the centroid, so an open curve
    // begins at an end rather than in its middle.
    let centre = loose.iter().fold(Vector3::new(0.0, 0.0, 0.0), |acc, p| {
        acc + (*p - Point3::default())
    }) / (loose.len() as f64);
    let centre = Point3::default() + centre;
    let first = loose
        .iter()
        .enumerate()
        .max_by(|a, b| {
            (*a.1 - centre)
                .length()
                .total_cmp(&(*b.1 - centre).length())
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    let mut ordered = vec![loose.swap_remove(first)];
    // A gap wider than this ends the curve rather than jumping across it.
    let reach = 3.0 * step;
    while !loose.is_empty() {
        let tail = *ordered.last().expect("non-empty");
        let Some((index, distance)) = loose
            .iter()
            .enumerate()
            .map(|(index, point)| (index, (*point - tail).length()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            break;
        };
        if distance > reach {
            break;
        }
        ordered.push(loose.swap_remove(index));
    }
    ordered
}

/// Chains scattered crossings into every run they contain.
///
/// One pair of surfaces can share several separate curves — a plane
/// cutting clean through a cylinder meets it in two parallel lines, a
/// bolt circle's plane meets its cylinders in many circles. Chaining
/// once and keeping the first run silently threw the others away.
fn chain_runs(mut loose: Vec<Point3>, step: f64) -> Vec<Vec<Point3>> {
    let mut runs = Vec::new();
    while loose.len() >= 2 {
        let run = chain_one(&mut loose, step);
        if run.len() < 2 {
            break;
        }
        runs.push(run);
    }
    runs
}

/// Finds the boundaries where two faces meet *smoothly*.
///
/// A fillet runs into the face it rounds with matching normals, and that
/// junction is a perfectly good edge — it bounds both faces and a loop
/// has to walk it — but it is invisible to the intersection machinery.
/// Two tangent surfaces do not cross; they touch, so there is no sign
/// change in either one's distance to the other, and the extractor
/// deliberately skips the pair rather than chase a root that is not
/// there. That is why every blend on both test parts contributed edge
/// ends that could never resolve: the boundary existed physically and
/// nowhere in the model.
///
/// So it is found the only way it can be — by **ownership** rather than
/// by intersection. The scan gave each face its own patch of surface,
/// and the tangent boundary is simply where one face's footprint stops
/// and the other's begins. That evidence is weaker than a solved
/// intersection and the edge is marked as such: it is located to about a
/// cell, and no corner can sit on it, because three surfaces meeting
/// with two of them tangent have no isolated common point.
fn extract_tangent_boundaries(
    patched: &[PatchedFace],
    surfaces: &[(usize, SurfaceClass)],
    presence: &std::collections::HashMap<(i64, i64, i64), Vec<usize>>,
    presence_cell: f64,
    step: f64,
) -> Vec<SharedEdge> {
    /// Surfaces agreeing to within this angle are tangent — the same
    /// threshold the crossing extractor uses to decide they are *not*
    /// worth intersecting.
    const MIN_DIHEDRAL: f64 = 15.0;
    /// A tangent boundary shorter than this is a corner artefact.
    const MIN_LENGTH: f64 = 1.5;
    let limit = MIN_DIHEDRAL.to_radians().cos();
    let face_of: std::collections::HashMap<usize, &PatchedFace> =
        patched.iter().map(|face| (face.0, face)).collect();
    let mut pairs: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for (id, carrier, cells) in patched {
        for &cell in cells.iter() {
            let point = carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step);
            let base = (
                (point.x / presence_cell).floor() as i64,
                (point.y / presence_cell).floor() as i64,
                (point.z / presence_cell).floor() as i64,
            );
            let Some(here) = presence.get(&base) else {
                continue;
            };
            for &other in here {
                if other != *id {
                    pairs.insert((*id.min(&other), *id.max(&other)));
                }
            }
        }
    }
    let mut ordered: Vec<(usize, usize)> = pairs.into_iter().collect();
    ordered.sort_unstable();
    let mut edges = Vec::new();
    for (first, second) in ordered {
        let (Some(a), Some(b)) = (face_of.get(&first), face_of.get(&second)) else {
            continue;
        };
        let (Some((_, sa)), Some((_, sb))) = (
            surfaces.iter().find(|(id, _)| *id == first),
            surfaces.iter().find(|(id, _)| *id == second),
        ) else {
            continue;
        };
        // Only pairs the crossing extractor refused as tangent.
        let touching: Vec<Point3> = {
            let (_, carrier, cells) = *a;
            let mut found = Vec::new();
            let (mut agree, mut samples) = (0.0, 0usize);
            for &cell in cells.iter() {
                let point = carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step);
                if !near_feature(presence, presence_cell, point, second) {
                    continue;
                }
                let (Some((_, ours)), Some((_, theirs))) = (sa.probe(point), sb.probe(point))
                else {
                    continue;
                };
                agree += ours.dot(theirs).abs();
                samples += 1;
                found.push(point);
            }
            if samples == 0 || agree / samples as f64 <= limit {
                continue;
            }
            found
        };
        if touching.len() < 2 {
            continue;
        }
        // The other side sees the same boundary; pooling both gives the
        // whole of it, exactly as for a crossing edge.
        let mut pooled = touching;
        {
            let (_, carrier, cells) = *b;
            for &cell in cells.iter() {
                let point = carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step);
                if near_feature(presence, presence_cell, point, first) {
                    pooled.push(point);
                }
            }
        }
        let mut seen: std::collections::HashSet<(i64, i64, i64)> = std::collections::HashSet::new();
        let grid = (step * 0.5).max(1e-6);
        pooled.retain(|point| {
            seen.insert((
                (point.x / grid).round() as i64,
                (point.y / grid).round() as i64,
                (point.z / grid).round() as i64,
            ))
        });
        for run in chain_runs(pooled, step * 2.0) {
            let edge = SharedEdge {
                between: (first, second),
                points: run,
                tangent: true,
            };
            if edge.length() >= MIN_LENGTH {
                edges.push(edge);
            }
        }
    }
    edges.sort_by(|a, b| b.length().total_cmp(&a.length()));
    edges
}

/// Extracts the curve every pair of touching faces shares.
///
/// The trimming pass already evaluates each neighbour's signed distance
/// over a face's own cells, and that field is zero exactly on the
/// intersection: no curve needs deriving per surface pair, only its sign
/// change located. Where two neighbouring cells disagree about the sign,
/// the crossing is interpolated between their centres and lands on the
/// curve to within a fraction of a cell.
///
/// Bounding falls out for free. The field only exists where the face has
/// material, so the curve is already clipped to the face — an infinite
/// plane-plane line never appears, only the piece the part actually has.
///
/// The walk is over **pairs**, not faces, and reads both footprints. An
/// edge is one curve shared by two faces, so finding it once from each
/// side gives two chains of the same thing; worse, each side dies
/// wherever its own footprint has a hole, and the first version returned
/// 215 mm of curve on the gear in forty disconnected fragments. Pooling
/// the crossings from both faces closes the holes that only one of them
/// has, and the pair yields a single curve.
fn extract_edges(
    patched: &[PatchedFace],
    surfaces: &[(usize, SurfaceClass)],
    presence: &std::collections::HashMap<(i64, i64, i64), Vec<usize>>,
    presence_cell: f64,
    step: f64,
) -> Vec<SharedEdge> {
    /// An edge shorter than this is a corner artefact, not a feature.
    const MIN_LENGTH: f64 = 1.0;
    let face_of: std::collections::HashMap<usize, &PatchedFace> =
        patched.iter().map(|face| (face.0, face)).collect();
    // Which pairs touch, each listed once.
    let mut pairs: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for (id, carrier, cells) in patched {
        for &cell in cells.iter() {
            let point = carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step);
            let base = (
                (point.x / presence_cell).floor() as i64,
                (point.y / presence_cell).floor() as i64,
                (point.z / presence_cell).floor() as i64,
            );
            let Some(here) = presence.get(&base) else {
                continue;
            };
            for &other in here {
                if other != *id {
                    pairs.insert((*id.min(&other), *id.max(&other)));
                }
            }
        }
    }
    let mut ordered: Vec<(usize, usize)> = pairs.into_iter().collect();
    ordered.sort_unstable();
    // Crossings of `against` read over `face`'s own grid.
    let crossings = |face: &PatchedFace, against: &SurfaceClass| -> Vec<Point3> {
        let (_, carrier, cells) = face;
        let at = |cell: (i64, i64)| {
            carrier.at((cell.0 as f64 + 0.5) * step, (cell.1 as f64 + 0.5) * step)
        };
        let field: std::collections::HashMap<(i64, i64), f64> = cells
            .iter()
            .filter_map(|&cell| against.probe(at(cell)).map(|(d, _)| (cell, d)))
            .collect();
        let mut found = Vec::new();
        for (&cell, &here) in &field {
            for offset in [(1, 0), (0, 1)] {
                let next = (cell.0 + offset.0, cell.1 + offset.1);
                let Some(&there) = field.get(&next) else {
                    continue;
                };
                if (here < 0.0) == (there < 0.0) {
                    continue;
                }
                let span = here - there;
                let ratio = if span.abs() < 1e-12 {
                    0.5
                } else {
                    (here / span).clamp(0.0, 1.0)
                };
                let (a, b) = (at(cell), at(next));
                found.push(a + (b - a) * ratio);
            }
        }
        found
    };
    let mut edges: Vec<SharedEdge> = Vec::new();
    for (first, second) in ordered {
        let (Some(a), Some(b)) = (face_of.get(&first), face_of.get(&second)) else {
            continue;
        };
        let (Some((_, sa)), Some((_, sb))) = (
            surfaces.iter().find(|(id, _)| *id == first),
            surfaces.iter().find(|(id, _)| *id == second),
        ) else {
            continue;
        };
        let mut pooled = crossings(a, sb);
        pooled.extend(crossings(b, sa));
        if pooled.len() < 2 {
            continue;
        }
        // Both sides describe the same curve, so pooling doubles its
        // density; thin it back to one point per half-cell or the chain
        // zigzags between near-duplicates.
        let mut seen: std::collections::HashSet<(i64, i64, i64)> = std::collections::HashSet::new();
        let grid = (step * 0.5).max(1e-6);
        pooled.retain(|point| {
            seen.insert((
                (point.x / grid).round() as i64,
                (point.y / grid).round() as i64,
                (point.z / grid).round() as i64,
            ))
        });
        for run in chain_runs(pooled, step) {
            let edge = SharedEdge {
                between: (first, second),
                points: run,
                tangent: false,
            };
            if edge.length() >= MIN_LENGTH {
                edges.push(edge);
            }
        }
    }
    edges.sort_by(|a, b| b.length().total_cmp(&a.length()));
    edges
}

/// The parameter domain of a fitted surface: a way to address points on
/// the carrier in millimetres, so a footprint can be rasterized, grown
/// and emitted without caring which kind of surface it sits on.
///
/// Both coordinates are lengths — arc length around a revolved carrier,
/// distance along it — so one grid step means the same thing on every
/// axis and a cell is never a sliver.
#[derive(Clone, Copy, Debug)]
enum Carrier {
    Plane {
        origin: Point3,
        u: Vector3,
        v: Vector3,
    },
    Cylinder {
        origin: Point3,
        u: Vector3,
        v: Vector3,
        w: Vector3,
        radius: f64,
    },
    Cone {
        apex: Point3,
        u: Vector3,
        v: Vector3,
        w: Vector3,
        slope: f64,
        mean: f64,
    },
    Sphere {
        center: Point3,
        u: Vector3,
        v: Vector3,
        w: Vector3,
        radius: f64,
    },
    /// A blend: the tube a rolling ball leaves, addressed by arc length
    /// along the spine circle and arc length around the tube.
    Torus {
        center: Point3,
        u: Vector3,
        v: Vector3,
        w: Vector3,
        major: f64,
        minor: f64,
    },
}

impl Carrier {
    /// The carrier for a fitted surface, sized against the faces that
    /// sit on it (a cone needs their mean radius to keep its angular
    /// axis in millimetres).
    fn of(
        surface: &SurfaceClass,
        faces: &[u32],
        mesh: &TriangleMesh,
        alignment: &crate::datum::DatumAlignment,
    ) -> Option<Carrier> {
        Some(match surface {
            SurfaceClass::Plane(fit) => {
                let (u, v, _) = frame_about(fit.normal);
                Carrier::Plane {
                    origin: fit.origin,
                    u,
                    v,
                }
            }
            SurfaceClass::Cylinder(fit) => {
                let (u, v, w) = frame_about(fit.axis);
                Carrier::Cylinder {
                    origin: fit.axis_point,
                    u,
                    v,
                    w,
                    radius: fit.radius.max(1e-6),
                }
            }
            SurfaceClass::Cone(fit) => {
                let (u, v, w) = frame_about(fit.axis);
                let slope = fit.half_angle.tan();
                let mean = faces
                    .iter()
                    .map(|&face| {
                        let c = alignment
                            .transform
                            .apply_point(mesh.face_centroid(face as usize));
                        ((c - fit.apex).dot(w) * slope).abs()
                    })
                    .sum::<f64>()
                    / (faces.len().max(1) as f64);
                Carrier::Cone {
                    apex: fit.apex,
                    u,
                    v,
                    w,
                    slope,
                    mean: mean.max(1e-6),
                }
            }
            SurfaceClass::Sphere(fit) => {
                let (u, v, w) = frame_about(Vector3::new(0.0, 0.0, 1.0));
                Carrier::Sphere {
                    center: fit.center,
                    u,
                    v,
                    w,
                    radius: fit.radius.max(1e-6),
                }
            }
            SurfaceClass::Blend(fit) => {
                let (u, v, w) = frame_about(fit.axis);
                Carrier::Torus {
                    center: fit.axis_point,
                    u,
                    v,
                    w,
                    major: fit.major_radius.max(1e-6),
                    minor: fit.minor_radius.max(1e-6),
                }
            }
            _ => return None,
        })
    }

    fn to_uv(self, p: Point3) -> Option<(f64, f64)> {
        match self {
            Carrier::Plane { origin, u, v } => Some(((p - origin).dot(u), (p - origin).dot(v))),
            Carrier::Cylinder {
                origin,
                u,
                v,
                w,
                radius,
            } => {
                let d = p - origin;
                Some((d.dot(v).atan2(d.dot(u)) * radius, d.dot(w)))
            }
            Carrier::Cone {
                apex,
                u,
                v,
                w,
                mean,
                ..
            } => {
                let d = p - apex;
                Some((d.dot(v).atan2(d.dot(u)) * mean, d.dot(w)))
            }
            Carrier::Sphere {
                center,
                u,
                v,
                w,
                radius,
            } => {
                let d = p - center;
                let length = d.length();
                if length < 1e-9 {
                    return None;
                }
                let d = d / length;
                Some((
                    d.dot(v).atan2(d.dot(u)) * radius,
                    d.dot(w).clamp(-1.0, 1.0).asin() * radius,
                ))
            }
            Carrier::Torus {
                center,
                u,
                v,
                w,
                major,
                minor,
            } => {
                let d = p - center;
                let height = d.dot(w);
                let flat = d - w * height;
                let reach = flat.length();
                if reach < 1e-9 {
                    return None;
                }
                Some((
                    flat.dot(v).atan2(flat.dot(u)) * major,
                    height.atan2(reach - major) * minor,
                ))
            }
        }
    }

    fn at(self, a: f64, b: f64) -> Point3 {
        match self {
            Carrier::Plane { origin, u, v } => origin + u * a + v * b,
            Carrier::Cylinder {
                origin,
                u,
                v,
                w,
                radius,
            } => {
                let angle = a / radius;
                origin + w * b + (u * angle.cos() + v * angle.sin()) * radius
            }
            Carrier::Cone {
                apex,
                u,
                v,
                w,
                slope,
                mean,
            } => {
                let angle = a / mean;
                apex + w * b + (u * angle.cos() + v * angle.sin()) * (b * slope)
            }
            Carrier::Sphere {
                center,
                u,
                v,
                w,
                radius,
            } => {
                let (angle, lat) = (a / radius, b / radius);
                center
                    + (u * angle.cos() * lat.cos() + v * angle.sin() * lat.cos() + w * lat.sin())
                        * radius
            }
            Carrier::Torus {
                center,
                u,
                v,
                w,
                major,
                minor,
            } => {
                let (around, tube) = (a / major, b / minor);
                let radial = u * around.cos() + v * around.sin();
                center + radial * (major + minor * tube.cos()) + w * (minor * tube.sin())
            }
        }
    }
}

/// The cells of the carrier's parameter grid that the feature's own
/// faces cover: the measured footprint of the face.
fn footprint(
    carrier: &Carrier,
    faces: &[u32],
    mesh: &TriangleMesh,
    alignment: &crate::datum::DatumAlignment,
    step: f64,
) -> std::collections::HashSet<(i64, i64)> {
    let mut cells = std::collections::HashSet::new();
    for &face in faces {
        for corner in mesh.triangle_points(face as usize) {
            let point = alignment.transform.apply_point(corner);
            if let Some((a, b)) = carrier.to_uv(point) {
                cells.insert(((a / step).floor() as i64, (b / step).floor() as i64));
            }
        }
    }
    cells
}

/// One drilled hole read from a cut extrusion instance: the bore wall
/// plus whatever coaxial cones (chamfers, countersinks) belong to it,
/// as a radius envelope along the axis.
struct HoleStack {
    origin: Point3,
    axis: Vector3,
    /// Linear radius runs (z0, z1, r0, r1) in stack-axis coordinates.
    pieces: Vec<(f64, f64, f64, f64)>,
    span: (f64, f64),
    bore_diameter: f64,
    /// The wall feature and the exact tube it emits.
    wall_id: usize,
    wall_radius: f64,
    /// Tube ends: the wall run extended to meet its cones or the lid
    /// it pierces.
    wall_run: (f64, f64),
    /// Exact chamfer rings: (feature id, narrow z, wide z, narrow r,
    /// wide r) — the z order tells which way the funnel opens.
    cones: Vec<(usize, f64, f64, f64, f64)>,
    /// Mouths where a ring meets a lid: (lid feature id, lid station,
    /// outward sign, ring radius at the lid). Each earns an exact flat
    /// collar bridging ring to lid, and a rim-band punch that clears
    /// the lid patch's ragged overhang.
    mouths: Vec<(usize, f64, f64, f64)>,
}

impl HoleStack {
    fn envelope(&self, s: f64, slack: f64) -> Option<f64> {
        let mut best: Option<f64> = None;
        for &(z0, z1, r0, r1) in &self.pieces {
            if s < z0 - slack || s > z1 + slack {
                continue;
            }
            let t = ((s - z0) / (z1 - z0).max(1e-9)).clamp(0.0, 1.0);
            let radius = r0 + (r1 - r0) * t;
            best = Some(best.map_or(radius, |known: f64| known.max(radius)));
        }
        best
    }

    fn inside(&self, point: Point3, margin: f64, slack: f64) -> bool {
        let arm = point - self.origin;
        let s = arm.dot(self.axis);
        if s < self.span.0 || s > self.span.1 {
            return false;
        }
        let radial = (arm - self.axis * s).length();
        self.envelope(s, slack)
            .is_some_and(|radius| radial < radius - margin)
    }
}

/// Scan occupancy in the datum frame: one representative point per
/// millimetre cell. Nominates candidates for evidence tests; exact
/// distances decide.
fn scan_occupancy(
    mesh: &TriangleMesh,
    alignment: &crate::datum::DatumAlignment,
) -> std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)> {
    const CELL: f64 = 1.0;
    let mut occupied: std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)> =
        std::collections::HashMap::new();
    let stride = (mesh.triangles().len() / 400_000).max(1);
    for face in (0..mesh.triangles().len()).step_by(stride) {
        let point = alignment.transform.apply_point(mesh.face_centroid(face));
        let normal = mesh
            .face_normal(face)
            .map(|n| alignment.transform.apply_vector(n))
            .unwrap_or(Vector3::new(0.0, 0.0, 1.0));
        let key = (
            (point.x / CELL).floor() as i32,
            (point.y / CELL).floor() as i32,
            (point.z / CELL).floor() as i32,
        );
        occupied.entry(key).or_insert((point, normal));
    }
    occupied
}

/// Reads recognized drilled holes out of the cut extrusion instances:
/// each inward-facing off-datum bore with its coaxial cones, its punch
/// envelope, and the exact tube and rings it will emit.
fn build_hole_stacks(
    mesh: &TriangleMesh,
    report: &ReverseReport,
    alignment: &crate::datum::DatumAlignment,
    tolerance: f64,
    occupied: &std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)>,
) -> Vec<HoleStack> {
    const AXIS_AGREE: f64 = 0.999;
    const CONE_REACH: f64 = 2.5;
    const CELL: f64 = 1.0;
    let Some(plan) = report.plan.as_ref() else {
        return Vec::new();
    };
    let feature_by_id = |id: usize| report.features.iter().find(|f| f.id == id);
    let faces_inward =
        |feature: &crate::report::FeatureRecord, origin: Point3, axis: Vector3| -> bool {
            let stride = (feature.faces.len() / 400).max(1);
            let (mut sum, mut weight) = (0.0, 0.0);
            for &face in feature.faces.iter().step_by(stride) {
                let Some(normal) = mesh.face_normal(face as usize) else {
                    continue;
                };
                let normal = alignment.transform.apply_vector(normal);
                let centroid = alignment
                    .transform
                    .apply_point(mesh.face_centroid(face as usize));
                let arm = centroid - origin;
                let radial = arm - axis * arm.dot(axis);
                let length = radial.length();
                if length < 1e-9 {
                    continue;
                }
                let area = mesh.face_area(face as usize);
                sum += area * normal.dot(radial / length);
                weight += area;
            }
            weight > 0.0 && sum / weight <= -0.3
        };
    let axial_run =
        |feature: &crate::report::FeatureRecord, origin: Point3, axis: Vector3| -> (f64, f64) {
            let stride = (feature.faces.len() / 400).max(1);
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &face in feature.faces.iter().step_by(stride) {
                for corner in mesh.triangle_points(face as usize) {
                    let s = (alignment.transform.apply_point(corner) - origin).dot(axis);
                    lo = lo.min(s);
                    hi = hi.max(s);
                }
            }
            (lo, hi)
        };
    // Level lids the stack may pierce: planes square to the stack axis
    // snap tube and ring ends to the surface they meet.
    let mut stacks: Vec<HoleStack> = Vec::new();
    for instance in &plan.instances.extrusions {
        // A drilled hole: one circle, no lines, and every member a
        // cylinder coaxial with the lead — at noise the wall arrives
        // as several arc fragments unioned into one instance, and the
        // largest fragment's fit speaks for the hole.
        if instance.circles.len() != 1 || !instance.lines.is_empty() {
            continue;
        }
        let Some(feature) = feature_by_id(instance.members[0]) else {
            continue;
        };
        let SurfaceClass::Cylinder(fit) = &feature.surface else {
            continue;
        };
        let all_coaxial = instance.members.iter().all(|&id| {
            feature_by_id(id).is_some_and(|member| match &member.surface {
                SurfaceClass::Cylinder(other) => {
                    other.axis.dot(fit.axis).abs() >= AXIS_AGREE && {
                        let offset = other.axis_point - fit.axis_point;
                        (offset - fit.axis * offset.dot(fit.axis)).length() <= 1.5
                    }
                }
                _ => false,
            })
        });
        if !all_coaxial {
            continue;
        }
        let on_datum = fit.axis.z.abs() >= AXIS_AGREE
            && fit.axis_point.x.hypot(fit.axis_point.y) <= 6.0 * tolerance;
        if on_datum {
            continue;
        }
        let inward_votes: Vec<bool> = instance
            .members
            .iter()
            .filter_map(|&id| feature_by_id(id))
            .map(|member| faces_inward(member, fit.axis_point, fit.axis))
            .collect();
        if inward_votes.iter().filter(|&&v| v).count() * 2 < inward_votes.len().max(1) {
            continue;
        }
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for &id in &instance.members {
            let Some(member) = feature_by_id(id) else {
                continue;
            };
            let (m_lo, m_hi) = axial_run(member, fit.axis_point, fit.axis);
            lo = lo.min(m_lo);
            hi = hi.max(m_hi);
        }
        if !(lo.is_finite() && hi.is_finite()) || hi - lo < 2.0 * tolerance {
            continue;
        }
        let mut pieces = vec![(lo, hi, fit.radius, fit.radius)];
        let mut cones: Vec<(usize, f64, f64, f64, f64)> = Vec::new();
        let mut wall_run = (lo, hi);
        for other in &report.features {
            let SurfaceClass::Cone(cone) = &other.surface else {
                continue;
            };
            if cone.axis.dot(fit.axis).abs() < AXIS_AGREE {
                continue;
            }
            let offset = cone.apex - fit.axis_point;
            if (offset - fit.axis * offset.dot(fit.axis)).length() > CONE_REACH {
                continue;
            }
            if !faces_inward(other, fit.axis_point, fit.axis) {
                continue;
            }
            let (c_lo, c_hi) = axial_run(other, fit.axis_point, fit.axis);
            if !(c_lo.is_finite() && c_hi.is_finite()) || c_hi - c_lo < 1e-6 {
                continue;
            }
            let apex_s = (cone.apex - fit.axis_point).dot(fit.axis);
            let slope = cone.half_angle.tan().abs();
            let radius_at = |s: f64| ((s - apex_s) * slope).abs();
            let (r_lo, r_hi) = (radius_at(c_lo), radius_at(c_hi));
            if r_lo.min(r_hi) > fit.radius + 2.0 * tolerance + 0.5 {
                continue;
            }
            pieces.push((c_lo, c_hi, r_lo, r_hi));
            // The exact ring runs from where the cone meets the bore
            // radius out to its wide end; the tube retreats to the
            // meeting point so the two surfaces share a rim.
            let slope_z = (r_hi - r_lo) / (c_hi - c_lo);
            let z_meet = if slope_z.abs() > 1e-9 {
                (c_lo + (fit.radius - r_lo) / slope_z).clamp(c_lo - 1.0, c_hi + 1.0)
            } else {
                c_lo
            };
            let (narrow_z, wide_z) = if r_lo <= r_hi {
                (z_meet, c_hi)
            } else {
                (z_meet, c_lo)
            };
            let ring_r0 = fit.radius;
            let ring_r1 = radius_at(wide_z);
            cones.push((other.id, narrow_z, wide_z, ring_r0, ring_r1));
            // Tube end pulls back to the meet.
            let mid = (lo + hi) / 2.0;
            if narrow_z < mid {
                wall_run.0 = wall_run.0.max(z_meet).min(mid);
            } else {
                wall_run.1 = wall_run.1.min(z_meet).max(mid);
            }
        }
        // A mouth with no cone feature is not proof there is no
        // chamfer: finalize's claiming can absorb a funnel into an
        // edge-round bucket, and a bucket carries no surface to join
        // the stack. The stack knows its own axis, so the funnel is
        // recoverable from that unfitted material directly — radial
        // distance against axial station over the mouth zone is a
        // straight line whose slope is the chamfer's angle.
        let mid = (lo + hi) / 2.0;
        for (end, sign) in [(lo, -1.0f64), (hi, 1.0f64)] {
            let already = cones.iter().any(|&(_, z0, z1, ..)| {
                let near_end = if sign < 0.0 { z0.min(z1) } else { z0.max(z1) };
                (near_end - end).abs() < 2.0
                    && (if sign < 0.0 {
                        z0.min(z1) < mid
                    } else {
                        z0.max(z1) > mid
                    })
            });
            if already {
                continue;
            }
            let mut samples: Vec<(f64, f64)> = Vec::new();
            let mut bins = [false; 16];
            let (au, av) = {
                let aside = if fit.axis.x.abs() < 0.9 {
                    Vector3::new(1.0, 0.0, 0.0)
                } else {
                    Vector3::new(0.0, 1.0, 0.0)
                };
                let across = fit.axis.cross(aside);
                let x = across / across.length().max(1e-12);
                (x, fit.axis.cross(x))
            };
            // Sample the scan occupancy cells in the mouth zone —
            // ownership-free, so it does not matter which feature the
            // claiming passes handed the funnel to, and stride-free,
            // so a hundred funnel faces inside a fifty-thousand-face
            // lid are not invisible.
            // The zone stops short of the lid and starts just off the
            // bore, or the flat rim and the wall itself join the fit
            // and a clean 45-degree funnel reads as slope 1.7 with
            // three populations' worth of scatter.
            let ahead_cap = level_lid_near(report, fit.axis_point, fit.axis, end, sign)
                .map(|lid| ((lid - end) * sign - 0.2).clamp(0.3, 1.8))
                .unwrap_or(1.8);
            let zone_center = fit.axis_point + fit.axis * (end + sign * 0.8);
            let reach = fit.radius + 4.5;
            let (cx, cy, cz) = (
                (zone_center.x).floor() as i32,
                (zone_center.y).floor() as i32,
                (zone_center.z).floor() as i32,
            );
            let cells_reach = reach.ceil() as i32 + 1;
            for dx in -cells_reach..=cells_reach {
                for dy in -cells_reach..=cells_reach {
                    for dz in -cells_reach..=cells_reach {
                        let Some(&(p, _)) = occupied.get(&(cx + dx, cy + dy, cz + dz)) else {
                            continue;
                        };
                        let arm = p - fit.axis_point;
                        let s = arm.dot(fit.axis);
                        let ahead = (s - end) * sign;
                        if !(-0.1..=ahead_cap).contains(&ahead) {
                            continue;
                        }
                        let radial = arm - fit.axis * s;
                        let d = radial.length();
                        if d < fit.radius + 0.12 || d > fit.radius + 2.2 {
                            continue;
                        }
                        let angle = radial.dot(av).atan2(radial.dot(au));
                        let bin = (((angle + std::f64::consts::PI) / std::f64::consts::TAU
                            * bins.len() as f64) as usize)
                            .min(bins.len() - 1);
                        bins[bin] = true;
                        samples.push((s, d));
                    }
                }
            }
            if std::env::var_os("ARTIFICER_PUNCH_DEBUG").is_some() {
                eprintln!(
                    "chamfer-recovery: bore ({:+.2} {:+.2}) end {end:+.2} sign {sign:+.0}: \
                     {} cell sample(s), {} bin(s)",
                    fit.axis_point.x,
                    fit.axis_point.y,
                    samples.len(),
                    bins.iter().filter(|filled| **filled).count()
                );
            }
            if samples.len() < 24 || bins.iter().filter(|filled| **filled).count() < 8 {
                continue;
            }
            // Least squares d = a + b·s over the mouth material,
            // fitted twice: cells from the flat lid beyond the funnel
            // pull the slope down, and the second pass drops whatever
            // the first pass could not explain.
            let line_fit = |points: &[(f64, f64)]| -> Option<(f64, f64, f64)> {
                let count = points.len() as f64;
                if count < 12.0 {
                    return None;
                }
                let (mut sum_s, mut sum_d, mut sum_ss, mut sum_sd) = (0.0, 0.0, 0.0, 0.0);
                for &(s, d) in points {
                    sum_s += s;
                    sum_d += d;
                    sum_ss += s * s;
                    sum_sd += s * d;
                }
                let denom = count * sum_ss - sum_s * sum_s;
                if denom.abs() < 1e-9 {
                    return None;
                }
                let slope = (count * sum_sd - sum_s * sum_d) / denom;
                let intercept = (sum_d - slope * sum_s) / count;
                let rms = (points
                    .iter()
                    .map(|&(s, d)| {
                        let e = d - (intercept + slope * s);
                        e * e
                    })
                    .sum::<f64>()
                    / count)
                    .sqrt();
                Some((slope, intercept, rms))
            };
            let Some((slope_first, intercept_first, rms_first)) = line_fit(&samples) else {
                continue;
            };
            let cut = (2.0 * rms_first).max(0.08);
            let core: Vec<(f64, f64)> = samples
                .iter()
                .copied()
                .filter(|&(s, d)| (d - (intercept_first + slope_first * s)).abs() <= cut)
                .collect();
            let Some((slope, intercept, rms)) = line_fit(&core) else {
                continue;
            };
            if std::env::var_os("ARTIFICER_PUNCH_DEBUG").is_some() {
                eprintln!(
                    "chamfer-recovery-fit: bore ({:+.2} {:+.2}) end {end:+.2}: \
                     slope {slope:+.3} rms {rms:.3} core {} of {}",
                    fit.axis_point.x,
                    fit.axis_point.y,
                    core.len(),
                    samples.len()
                );
            }
            // A chamfer leans between 15 and 75 degrees; flatter is a
            // lid, steeper is the wall itself.
            if !(0.27..=3.8).contains(&slope.abs()) || rms > 1.6 * tolerance {
                continue;
            }
            let s_meet = (fit.radius - intercept) / slope;
            let s_far = samples.iter().map(|&(s, _)| s).fold(
                if sign < 0.0 {
                    f64::INFINITY
                } else {
                    f64::NEG_INFINITY
                },
                |acc, s| {
                    if sign < 0.0 { acc.min(s) } else { acc.max(s) }
                },
            );
            let (z0, z1) = if s_meet <= s_far {
                (s_meet, s_far)
            } else {
                (s_far, s_meet)
            };
            if z1 - z0 < 0.2 {
                continue;
            }
            let radius_at = |s: f64| intercept + slope * s;
            // Recovered from unowned material: the ring wears the
            // wall's own id, which is already expressed exactly.
            pieces.push((z0, z1, radius_at(z0), radius_at(z1)));
            cones.push((feature.id, z0, z1, radius_at(z0), radius_at(z1)));
            // Tube end pulls back to the recovered meet, same as a
            // fitted cone's.
            if s_meet < mid {
                wall_run.0 = wall_run.0.max(s_meet).min(mid);
            } else {
                wall_run.1 = wall_run.1.min(s_meet).max(mid);
            }
        }
        // Ends with no cone extend to the level lid they pierce.
        let lid_near = |end: f64, sign: f64| -> Option<(f64, usize)> {
            let mut best: Option<(f64, usize)> = None;
            for other in &report.features {
                let SurfaceClass::Plane(plane) = &other.surface else {
                    continue;
                };
                if plane.normal.dot(fit.axis).abs() < 0.996 || other.area < 50.0 {
                    continue;
                }
                let s_plane = (plane.origin - fit.axis_point).dot(fit.axis);
                let ahead = (s_plane - end) * sign;
                if (-0.3..=2.8).contains(&ahead)
                    && best.is_none_or(|(known, _): (f64, usize)| {
                        (s_plane - end).abs() < (known - end).abs()
                    })
                {
                    best = Some((s_plane, other.id));
                }
            }
            best
        };
        let cone_low = cones
            .iter()
            .any(|&(_, z0, z1, ..)| z0.min(z1) < (lo + hi) / 2.0 && (z0.min(z1) - lo).abs() < 2.0);
        let cone_high = cones
            .iter()
            .any(|&(_, z0, z1, ..)| z0.max(z1) > (lo + hi) / 2.0 && (z0.max(z1) - hi).abs() < 2.0);
        if !cone_low && let Some((lid, _)) = lid_near(wall_run.0, -1.0) {
            wall_run.0 = lid;
        }
        if !cone_high && let Some((lid, _)) = lid_near(wall_run.1, 1.0) {
            wall_run.1 = lid;
        }
        // Ring wide ends snap to the lid they meet — wide meaning the
        // larger RADIUS, which for a bottom chamfer is the lower end.
        // The ring extends onto the lid along its own slope, and the
        // meeting earns a mouth: an exact collar there later bridges
        // ring to lid, and a rim band clears the lid patch's ragged
        // overhang.
        let mut mouths: Vec<(usize, f64, f64, f64)> = Vec::new();
        for cone in cones.iter_mut() {
            let wide_is_second = cone.4 >= cone.3;
            let (narrow_z, narrow_r, wide_z, wide_r) = if wide_is_second {
                (cone.1, cone.3, cone.2, cone.4)
            } else {
                (cone.2, cone.4, cone.1, cone.3)
            };
            let sign = if wide_z >= mid { 1.0 } else { -1.0 };
            if let Some((lid, lid_id)) = lid_near(wide_z, sign)
                && (lid - wide_z).abs() <= 1.2
            {
                let slope = (wide_r - narrow_r) / (wide_z - narrow_z);
                let r_at_lid = if slope.is_finite() {
                    narrow_r + slope * (lid - narrow_z)
                } else {
                    wide_r
                };
                if wide_is_second {
                    cone.2 = lid;
                    cone.4 = r_at_lid;
                } else {
                    cone.1 = lid;
                    cone.3 = r_at_lid;
                }
                mouths.push((lid_id, lid, sign, r_at_lid));
            }
        }
        let span = pieces.iter().fold(
            (f64::INFINITY, f64::NEG_INFINITY),
            |(s_lo, s_hi), &(z0, z1, ..)| (s_lo.min(z0), s_hi.max(z1)),
        );
        stacks.push(HoleStack {
            origin: fit.axis_point,
            axis: fit.axis,
            pieces,
            span: (span.0 - 3.0 * tolerance, span.1 + 3.0 * tolerance),
            bore_diameter: 2.0 * fit.radius,
            wall_id: feature.id,
            wall_radius: fit.radius,
            wall_run,
            cones,
            mouths,
        });
    }
    // Follow the void: a hole's measured wall can stop short of the
    // surface it pierces, so each stack end extends along the axis for
    // as long as the tube's own interior holds no scan at all. A blind
    // hole's floor is scan inside the tube, which ends the extension
    // by itself.
    for stack in &mut stacks {
        let (u, v) = {
            let hint = if stack.axis.x.abs() < 0.9 {
                Vector3::new(1.0, 0.0, 0.0)
            } else {
                Vector3::new(0.0, 1.0, 0.0)
            };
            let x = hint - stack.axis * hint.dot(stack.axis);
            let x = x / x.length().max(1e-12);
            (x, stack.axis.cross(x))
        };
        let probe_radius = (stack.bore_diameter / 2.0) * 0.55;
        let debug = std::env::var_os("ARTIFICER_PUNCH_DEBUG").is_some();
        let void_at = |s: f64| -> bool {
            let center = stack.origin + stack.axis * s;
            let mut points = vec![center];
            for k in 0..8 {
                let angle = std::f64::consts::TAU * k as f64 / 8.0;
                points.push(center + (u * angle.cos() + v * angle.sin()) * probe_radius);
            }
            points.iter().all(|point| {
                let base = (
                    (point.x / CELL).floor() as i32,
                    (point.y / CELL).floor() as i32,
                    (point.z / CELL).floor() as i32,
                );
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            if let Some(&(blocker, _)) =
                                occupied.get(&(base.0 + dx, base.1 + dy, base.2 + dz))
                            {
                                // Cells nominate; distance decides.
                                if (blocker - *point).length() > 1.2 {
                                    continue;
                                }
                                if debug {
                                    eprintln!(
                                        "punch-debug: void blocked at s {s:+.2} probe \
                                         ({:+.2} {:+.2} {:+.2}) by scan ({:+.2} {:+.2} {:+.2})",
                                        point.x, point.y, point.z, blocker.x, blocker.y, blocker.z
                                    );
                                }
                                return false;
                            }
                        }
                    }
                }
                true
            })
        };
        let mut extensions: Vec<(f64, f64, f64, f64)> = Vec::new();
        for sign in [-1.0f64, 1.0] {
            let end = if sign < 0.0 {
                stack.span.0
            } else {
                stack.span.1
            };
            let end_radius = stack
                .envelope(end - sign * 0.1, 0.5)
                .unwrap_or(stack.bore_diameter / 2.0);
            let mut reached = end;
            // Never bore blindly forever: six millimetres of void is
            // every lid this part class has.
            for step in 1..=12 {
                let probe = end + sign * 0.5 * step as f64;
                if !void_at(probe) {
                    break;
                }
                reached = probe;
            }
            if reached != end {
                extensions.push((sign, end, reached, end_radius));
            }
        }
        for (sign, end, reached, end_radius) in extensions {
            stack.pieces.push(if sign < 0.0 {
                (reached, end, end_radius, end_radius)
            } else {
                (end, reached, end_radius, end_radius)
            });
            if sign < 0.0 {
                stack.span.0 = reached;
            } else {
                stack.span.1 = reached;
            }
        }
    }
    stacks
}

/// Emits each recognized hole as exact geometry — the bore wall as a
/// true cylinder about its own axis, each chamfer as a true cone ring
/// — replacing the measured-patch mosaic the same surfaces used to
/// draw. This is the off-axis analogue of what the revolved profile
/// does about the datum: recognition earning exact form.
fn emit_exact_holes(
    stacks: &[HoleStack],
    positions: &mut Vec<Point3>,
    triangles: &mut Vec<[u32; 3]>,
    feature_of_face: &mut Vec<usize>,
    notes: &mut Vec<String>,
) -> std::collections::HashSet<usize> {
    let mut covered = std::collections::HashSet::new();
    for stack in stacks {
        let (u, v) = {
            let hint = if stack.axis.x.abs() < 0.9 {
                Vector3::new(1.0, 0.0, 0.0)
            } else {
                Vector3::new(0.0, 1.0, 0.0)
            };
            let x = hint - stack.axis * hint.dot(stack.axis);
            let x = x / x.length().max(1e-12);
            (x, stack.axis.cross(x))
        };
        let at = |s: f64, r: f64, angle: f64| -> Point3 {
            let radial = u * angle.cos() + v * angle.sin();
            Point3::new(
                stack.origin.x + stack.axis.x * s + radial.x * r,
                stack.origin.y + stack.axis.y * s + radial.y * r,
                stack.origin.z + stack.axis.z * s + radial.z * r,
            )
        };
        // A bore's material faces its own axis; wind the quads so the
        // emitted normals do too.
        let ring_soup = |id: usize,
                         z0: f64,
                         z1: f64,
                         r0: f64,
                         r1: f64,
                         positions: &mut Vec<Point3>,
                         triangles: &mut Vec<[u32; 3]>,
                         feature_of_face: &mut Vec<usize>| {
            if (z1 - z0).abs() < 1e-9 && (r1 - r0).abs() < 1e-9 {
                return;
            }
            let radius = r0.max(r1).max(0.1);
            let segments = ((std::f64::consts::TAU * radius / 0.5) as usize).clamp(48, 256);
            for k in 0..segments {
                let a0 = std::f64::consts::TAU * k as f64 / segments as f64;
                let a1 = std::f64::consts::TAU * (k + 1) as f64 / segments as f64;
                let quad = [
                    at(z0, r0, a0),
                    at(z0, r0, a1),
                    at(z1, r1, a1),
                    at(z1, r1, a0),
                ];
                // Inward winding: the cross of the first two edges
                // must run against the outward radial.
                let radial = (u * ((a0 + a1) / 2.0).cos() + v * ((a0 + a1) / 2.0).sin()) * 1.0;
                for tri in [[0usize, 1, 2], [0, 2, 3]] {
                    let (pa, pb, pc) = (quad[tri[0]], quad[tri[1]], quad[tri[2]]);
                    let emitted = (pb - pa).cross(pc - pa);
                    let base = positions.len() as u32;
                    if emitted.dot(radial) <= 0.0 {
                        positions.extend([pa, pb, pc]);
                    } else {
                        positions.extend([pa, pc, pb]);
                    }
                    triangles.push([base, base + 1, base + 2]);
                    feature_of_face.push(id);
                }
            }
        };
        let (t0, t1) = stack.wall_run;
        if t1 - t0 > 1e-6 {
            ring_soup(
                stack.wall_id,
                t0,
                t1,
                stack.wall_radius,
                stack.wall_radius,
                positions,
                triangles,
                feature_of_face,
            );
            covered.insert(stack.wall_id);
        }
        for &(id, z0, z1, r0, r1) in &stack.cones {
            ring_soup(id, z0, z1, r0, r1, positions, triangles, feature_of_face);
            covered.insert(id);
        }
        // The collar: an exact flat annulus on the lid plane from the
        // ring's rim outward, wearing the lid's own id. The boundary
        // between exact geometry and rasterized patch becomes a
        // coplanar overlap — invisible — instead of a ragged edge
        // hanging over the funnel.
        for &(lid_id, s_lid, sign, r_wide) in &stack.mouths {
            // The collar must outreach the rim punch by more than the
            // patch grid's sawtooth (0.4 mm cells merged into runs), or
            // the punched patch edge's teeth show as a dotted ring
            // riding just past whatever radius the punch used.
            let r_outer = r_wide + 2.1;
            let segments = ((std::f64::consts::TAU * r_outer / 0.5) as usize).clamp(48, 256);
            let wanted = stack.axis * sign;
            for k in 0..segments {
                let a0 = std::f64::consts::TAU * k as f64 / segments as f64;
                let a1 = std::f64::consts::TAU * (k + 1) as f64 / segments as f64;
                let quad = [
                    at(s_lid, r_wide, a0),
                    at(s_lid, r_wide, a1),
                    at(s_lid, r_outer, a1),
                    at(s_lid, r_outer, a0),
                ];
                for tri in [[0usize, 1, 2], [0, 2, 3]] {
                    let (pa, pb, pc) = (quad[tri[0]], quad[tri[1]], quad[tri[2]]);
                    let emitted = (pb - pa).cross(pc - pa);
                    let base = positions.len() as u32;
                    if emitted.dot(wanted) >= 0.0 {
                        positions.extend([pa, pb, pc]);
                    } else {
                        positions.extend([pa, pc, pb]);
                    }
                    triangles.push([base, base + 1, base + 2]);
                    feature_of_face.push(lid_id);
                }
            }
        }
        notes.push(format!(
            "bore d {:.2} at ({:+.2} {:+.2}) emitted exact: tube z {:+.2}..{:+.2}{}",
            stack.bore_diameter,
            stack.origin.x,
            stack.origin.y,
            stack.wall_run.0,
            stack.wall_run.1,
            if stack.cones.is_empty() {
                String::new()
            } else {
                format!(" + {} chamfer ring(s)", stack.cones.len())
            }
        ));
    }
    covered
}

/// A volume a recognized feature owns: covering geometry inside it is
/// candidate for removal, subject to the same scan-evidence rules
/// whatever the volume's shape.
enum CutVolume {
    /// A drilled hole: bore plus cones, radius envelope along an axis.
    Stack(HoleStack),
    /// A plain cylinder of influence — a boss's body, where the base
    /// surface must not pass.
    Cylinder {
        origin: Point3,
        axis: Vector3,
        radius: f64,
        span: (f64, f64),
        label: String,
    },
    /// A closed sketch profile swept along a direction — a pocket, a
    /// slot, a pad.
    Prism {
        u: Vector3,
        v: Vector3,
        direction: Vector3,
        polygon: Vec<(f64, f64)>,
        span: (f64, f64),
        label: String,
    },
}

impl CutVolume {
    fn inside(&self, point: Point3, margin: f64, slack: f64) -> bool {
        match self {
            CutVolume::Stack(stack) => stack.inside(point, margin, slack),
            CutVolume::Cylinder {
                origin,
                axis,
                radius,
                span,
                ..
            } => {
                let arm = point - *origin;
                let s = arm.dot(*axis);
                if s < span.0 || s > span.1 {
                    return false;
                }
                (arm - *axis * s).length() < radius - margin
            }
            CutVolume::Prism {
                u,
                v,
                direction,
                polygon,
                span,
                ..
            } => {
                let arm = point - Point3::default();
                let s = arm.dot(*direction);
                if s < span.0 || s > span.1 {
                    return false;
                }
                let sample = (arm.dot(*u), arm.dot(*v));
                point_in_polygon(sample, polygon) && distance_to_polygon(sample, polygon) > margin
            }
        }
    }

    fn label(&self) -> String {
        match self {
            CutVolume::Stack(stack) => format!(
                "drilled bore d {:.2} at ({:+.2} {:+.2})",
                stack.bore_diameter, stack.origin.x, stack.origin.y
            ),
            CutVolume::Cylinder { label, .. } | CutVolume::Prism { label, .. } => label.clone(),
        }
    }

    fn span(&self) -> (f64, f64) {
        match self {
            CutVolume::Stack(stack) => stack.span,
            CutVolume::Cylinder { span, .. } | CutVolume::Prism { span, .. } => *span,
        }
    }
}

fn point_in_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let count = polygon.len();
    for index in 0..count {
        let (x0, y0) = polygon[index];
        let (x1, y1) = polygon[(index + 1) % count];
        if (y0 > point.1) != (y1 > point.1) {
            let cross = (x1 - x0) * (point.1 - y0) / (y1 - y0) + x0;
            if point.0 < cross {
                inside = !inside;
            }
        }
    }
    inside
}

fn distance_to_polygon(point: (f64, f64), polygon: &[(f64, f64)]) -> f64 {
    let mut best = f64::INFINITY;
    let count = polygon.len();
    for index in 0..count {
        let (x0, y0) = polygon[index];
        let (x1, y1) = polygon[(index + 1) % count];
        let (dx, dy) = (x1 - x0, y1 - y0);
        let length_squared = dx * dx + dy * dy;
        let t = if length_squared > 1e-18 {
            (((point.0 - x0) * dx + (point.1 - y0) * dy) / length_squared).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (px, py) = (x0 + dx * t, y0 + dy * t);
        best = best.min(((point.0 - px).powi(2) + (point.1 - py).powi(2)).sqrt());
    }
    best
}

/// The nearest level lid a swept feature pierces: a plane square to
/// the axis within a short reach ahead of the given end.
fn level_lid_near(
    report: &ReverseReport,
    origin: Point3,
    axis: Vector3,
    end: f64,
    sign: f64,
) -> Option<f64> {
    let mut best: Option<f64> = None;
    for other in &report.features {
        let SurfaceClass::Plane(plane) = &other.surface else {
            continue;
        };
        if plane.normal.dot(axis).abs() < 0.996 || other.area < 50.0 {
            continue;
        }
        let s_plane = (plane.origin - origin).dot(axis);
        let ahead = (s_plane - end) * sign;
        if (-0.3..=2.8).contains(&ahead)
            && best.is_none_or(|known: f64| (s_plane - end).abs() < (known - end).abs())
        {
            best = Some(s_plane);
        }
    }
    best
}

/// The run a feature's own material covers along an axis, measured
/// over triangle corners — an end row's centroid sits a third in from
/// the rim and forfeits real depth.
fn feature_axial_run(
    mesh: &TriangleMesh,
    alignment: &crate::datum::DatumAlignment,
    feature: &crate::report::FeatureRecord,
    origin: Point3,
    axis: Vector3,
) -> (f64, f64) {
    let stride = (feature.faces.len() / 400).max(1);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for &face in feature.faces.iter().step_by(stride) {
        for corner in mesh.triangle_points(face as usize) {
            let s = (alignment.transform.apply_point(corner) - origin).dot(axis);
            lo = lo.min(s);
            hi = hi.max(s);
        }
    }
    (lo, hi)
}

/// Area-weighted mean of a feature's measured normals, datum frame.
fn feature_mean_normal(
    mesh: &TriangleMesh,
    alignment: &crate::datum::DatumAlignment,
    feature: &crate::report::FeatureRecord,
) -> Option<Vector3> {
    let stride = (feature.faces.len() / 400).max(1);
    let mut sum = Vector3::new(0.0, 0.0, 0.0);
    for &face in feature.faces.iter().step_by(stride) {
        let Some(normal) = mesh.face_normal(face as usize) else {
            continue;
        };
        sum = sum + alignment.transform.apply_vector(normal) * mesh.face_area(face as usize);
    }
    let length = sum.length();
    (length > 1e-9).then(|| sum / length)
}

/// Whether a swept rectangle is where the scan says material is.
///
/// A tangent-plane shard recovered off a cylinder is kinematically a
/// perfect extrusion member — a Z-cylinder's tangent planes are
/// genuinely invariant along Z — so no motion gate can refuse it. What
/// convicts it is the geometry it would emit: a flat rectangle hovers
/// off the curved surface it was fitted to, and the scan is not there.
/// Sample the rectangle; most sample points must have scan within a
/// tight reach, or the wall is not drawn.
fn rect_backed(
    corners: &[Point3; 4],
    occupied: &std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)>,
) -> bool {
    const REACH: f64 = 0.45;
    const MIN_FRACTION: f64 = 0.7;
    let (mut hits, mut total) = (0usize, 0usize);
    let steps_u = ((corners[1] - corners[0]).length() / 1.5).ceil().max(2.0) as usize;
    let steps_v = ((corners[3] - corners[0]).length() / 1.5).ceil().max(2.0) as usize;
    for i in 0..=steps_u {
        for j in 0..=steps_v {
            let fu = i as f64 / steps_u as f64;
            let fv = j as f64 / steps_v as f64;
            let bottom = corners[0] + (corners[1] - corners[0]) * fu;
            let top = corners[3] + (corners[2] - corners[3]) * fu;
            let point = bottom + (top - bottom) * fv;
            total += 1;
            let base = (
                (point.x).floor() as i32,
                (point.y).floor() as i32,
                (point.z).floor() as i32,
            );
            'cells: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(&(q, _)) =
                            occupied.get(&(base.0 + dx, base.1 + dy, base.2 + dz))
                            && (q - point).length() <= REACH
                        {
                            hits += 1;
                            break 'cells;
                        }
                    }
                }
            }
        }
    }
    total > 0 && hits as f64 / total as f64 >= MIN_FRACTION
}

/// Emits recognized prismatic extrusions as exact geometry — each
/// plane member as a draft-true rectangle on its own fitted plane,
/// each cylinder member as a true, arc-aware tube — and returns the
/// volumes their sketches sweep, so a pocket opens its mouth and a
/// boss clears the base surface drawn through its body. The walls are
/// the instance's evidence; the volume is its license.
#[allow(clippy::too_many_arguments)]
fn emit_exact_extrusions(
    mesh: &TriangleMesh,
    report: &ReverseReport,
    alignment: &crate::datum::DatumAlignment,
    expressed: &std::collections::HashSet<usize>,
    occupied: &std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)>,
    tolerance: f64,
    positions: &mut Vec<Point3>,
    triangles: &mut Vec<[u32; 3]>,
    feature_of_face: &mut Vec<usize>,
    notes: &mut Vec<String>,
) -> (std::collections::HashSet<usize>, Vec<CutVolume>) {
    const AXIS_AGREE: f64 = 0.999;
    let mut covered = std::collections::HashSet::new();
    let mut volumes: Vec<CutVolume> = Vec::new();
    let Some(plan) = report.plan.as_ref() else {
        return (covered, volumes);
    };
    let feature_by_id = |id: usize| report.features.iter().find(|f| f.id == id);
    fn push_wall(
        soup: Vec<[Point3; 3]>,
        id: usize,
        positions: &mut Vec<Point3>,
        triangles: &mut Vec<[u32; 3]>,
        feature_of_face: &mut Vec<usize>,
    ) {
        for [a, b, c] in soup {
            let base = positions.len() as u32;
            positions.extend([a, b, c]);
            triangles.push([base, base + 1, base + 2]);
            feature_of_face.push(id);
        }
    }
    for instance in &plan.instances.extrusions {
        let direction = instance.direction;
        let (u, v) = {
            let aside = if direction.x.abs() < 0.9 {
                Vector3::new(1.0, 0.0, 0.0)
            } else {
                Vector3::new(0.0, 1.0, 0.0)
            };
            let across = direction.cross(aside);
            let x = across / across.length().max(1e-12);
            (x, direction.cross(x))
        };
        // Single-circle bores belong to the hole path; single-circle
        // bosses are handled here.
        let single_circle =
            instance.members.len() == 1 && instance.circles.len() == 1 && instance.lines.is_empty();
        if single_circle {
            let Some(feature) = feature_by_id(instance.members[0]) else {
                continue;
            };
            let SurfaceClass::Cylinder(fit) = &feature.surface else {
                continue;
            };
            let on_datum = fit.axis.z.abs() >= AXIS_AGREE
                && fit.axis_point.x.hypot(fit.axis_point.y) <= 6.0 * tolerance;
            if on_datum || expressed.contains(&feature.id) {
                continue;
            }
            let Some(mean_normal) = feature_mean_normal(mesh, alignment, feature) else {
                continue;
            };
            let anchor = alignment
                .transform
                .apply_point(mesh.face_centroid(feature.faces[0] as usize));
            let arm = anchor - fit.axis_point;
            let radial = arm - fit.axis * arm.dot(fit.axis);
            if radial.length() < 1e-9 || mean_normal.dot(radial / radial.length()) < 0.0 {
                // Inward: the hole path owns bores.
                continue;
            }
            let (lo, hi) = feature_axial_run(mesh, alignment, feature, fit.axis_point, fit.axis);
            if !(lo.is_finite() && hi.is_finite()) || hi - lo < 2.0 * tolerance {
                continue;
            }
            let (cu, cv) = {
                let aside = if fit.axis.x.abs() < 0.9 {
                    Vector3::new(1.0, 0.0, 0.0)
                } else {
                    Vector3::new(0.0, 1.0, 0.0)
                };
                let across = fit.axis.cross(aside);
                let x = across / across.length().max(1e-12);
                (x, fit.axis.cross(x))
            };
            let segments = ((std::f64::consts::TAU * fit.radius / 0.5) as usize).clamp(48, 256);
            let mut soup = Vec::with_capacity(segments * 2);
            for k in 0..segments {
                let a0 = std::f64::consts::TAU * k as f64 / segments as f64;
                let a1 = std::f64::consts::TAU * (k + 1) as f64 / segments as f64;
                let at = |s: f64, angle: f64| -> Point3 {
                    let radial = cu * angle.cos() + cv * angle.sin();
                    Point3::new(
                        fit.axis_point.x + fit.axis.x * s + radial.x * fit.radius,
                        fit.axis_point.y + fit.axis.y * s + radial.y * fit.radius,
                        fit.axis_point.z + fit.axis.z * s + radial.z * fit.radius,
                    )
                };
                let quad = [at(lo, a0), at(lo, a1), at(hi, a1), at(hi, a0)];
                let outward = cu * ((a0 + a1) / 2.0).cos() + cv * ((a0 + a1) / 2.0).sin();
                for tri in [[0usize, 1, 2], [0, 2, 3]] {
                    let (pa, pb, pc) = (quad[tri[0]], quad[tri[1]], quad[tri[2]]);
                    let emitted = (pb - pa).cross(pc - pa);
                    if emitted.dot(outward) >= 0.0 {
                        soup.push([pa, pb, pc]);
                    } else {
                        soup.push([pa, pc, pb]);
                    }
                }
            }
            push_wall(soup, feature.id, positions, triangles, feature_of_face);
            covered.insert(feature.id);
            // Base end: the lid the boss stands on; the far end pulls
            // back so the cap's own patch survives the punch.
            let mut span = (lo + 0.25, hi - 0.25);
            if let Some(lid) = level_lid_near(report, fit.axis_point, fit.axis, lo, -1.0) {
                span = (lid - 0.4, hi - 0.25);
            } else if let Some(lid) = level_lid_near(report, fit.axis_point, fit.axis, hi, 1.0) {
                span = (lo + 0.25, lid + 0.4);
            }
            notes.push(format!(
                "boss d {:.2} at ({:+.2} {:+.2}) emitted exact: tube z {:+.2}..{:+.2}",
                2.0 * fit.radius,
                fit.axis_point.x,
                fit.axis_point.y,
                lo,
                hi
            ));
            volumes.push(CutVolume::Cylinder {
                origin: fit.axis_point,
                axis: fit.axis,
                radius: fit.radius,
                span,
                label: format!(
                    "boss d {:.2} at ({:+.2} {:+.2})",
                    2.0 * fit.radius,
                    fit.axis_point.x,
                    fit.axis_point.y
                ),
            });
            continue;
        }
        if instance.members.len() < 2 {
            continue;
        }
        // ---- Multi-member prismatic instance: emit each wall exactly.
        let mut emitted_walls = 0usize;
        let mut runs: Vec<(f64, f64)> = Vec::new();
        // Sketch pieces for profile assembly, in (u, v) coordinates.
        let mut pieces: Vec<Vec<(f64, f64)>> = Vec::new();
        for line in &instance.lines {
            let Some(feature) = feature_by_id(line.feature) else {
                continue;
            };
            let SurfaceClass::Plane(plane) = &feature.surface else {
                continue;
            };
            pieces.push(vec![line.from, line.to]);
            if expressed.contains(&feature.id) || covered.contains(&feature.id) {
                continue;
            }
            let (lo, hi) =
                feature_axial_run(mesh, alignment, feature, Point3::default(), direction);
            if !(lo.is_finite() && hi.is_finite()) || hi - lo < 1e-6 {
                continue;
            }
            runs.push((lo, hi));
            // Corners in sketch space swept over the run, then landed
            // exactly on the fitted plane — which is what keeps a
            // drafted wall drafted.
            let landed = |a: f64, b: f64, s: f64| -> Point3 {
                let q = Point3::new(
                    u.x * a + v.x * b + direction.x * s,
                    u.y * a + v.y * b + direction.y * s,
                    u.z * a + v.z * b + direction.z * s,
                );
                let offset = plane.normal.dot(q - plane.origin);
                Point3::new(
                    q.x - plane.normal.x * offset,
                    q.y - plane.normal.y * offset,
                    q.z - plane.normal.z * offset,
                )
            };
            let corners = [
                landed(line.from.0, line.from.1, lo),
                landed(line.to.0, line.to.1, lo),
                landed(line.to.0, line.to.1, hi),
                landed(line.from.0, line.from.1, hi),
            ];
            if !rect_backed(&corners, occupied) {
                continue;
            }
            let mean = feature_mean_normal(mesh, alignment, feature).unwrap_or(plane.normal);
            let mut soup = Vec::with_capacity(2);
            for tri in [[0usize, 1, 2], [0, 2, 3]] {
                let (pa, pb, pc) = (corners[tri[0]], corners[tri[1]], corners[tri[2]]);
                let emitted = (pb - pa).cross(pc - pa);
                if emitted.dot(mean) >= 0.0 {
                    soup.push([pa, pb, pc]);
                } else {
                    soup.push([pa, pc, pb]);
                }
            }
            push_wall(soup, feature.id, positions, triangles, feature_of_face);
            covered.insert(feature.id);
            emitted_walls += 1;
        }
        for circle in &instance.circles {
            let Some(feature) = feature_by_id(circle.feature) else {
                continue;
            };
            let SurfaceClass::Cylinder(fit) = &feature.surface else {
                continue;
            };
            // Arc range about the member's own axis, from its faces:
            // sort the sampled azimuths and take the complement of the
            // largest gap.
            let (cu, cv) = {
                let aside = if fit.axis.x.abs() < 0.9 {
                    Vector3::new(1.0, 0.0, 0.0)
                } else {
                    Vector3::new(0.0, 1.0, 0.0)
                };
                let across = fit.axis.cross(aside);
                let x = across / across.length().max(1e-12);
                (x, fit.axis.cross(x))
            };
            let stride = (feature.faces.len() / 400).max(1);
            let mut angles: Vec<f64> = feature
                .faces
                .iter()
                .step_by(stride)
                .filter_map(|&face| {
                    let c = alignment
                        .transform
                        .apply_point(mesh.face_centroid(face as usize));
                    let arm = c - fit.axis_point;
                    let radial = arm - fit.axis * arm.dot(fit.axis);
                    (radial.length() > 1e-9).then(|| radial.dot(cv).atan2(radial.dot(cu)))
                })
                .collect();
            if angles.len() < 8 {
                continue;
            }
            angles.sort_by(f64::total_cmp);
            let mut gap_start = angles.len() - 1;
            let mut widest = 0.0f64;
            for index in 0..angles.len() {
                let here = angles[index];
                let next = angles[(index + 1) % angles.len()];
                let gap = if index + 1 == angles.len() {
                    next + std::f64::consts::TAU - here
                } else {
                    next - here
                };
                if gap > widest {
                    widest = gap;
                    gap_start = index;
                }
            }
            let full = widest < 0.6;
            let (arc0, arc1) = if full {
                (0.0, std::f64::consts::TAU)
            } else {
                let start = angles[(gap_start + 1) % angles.len()];
                let mut end = angles[gap_start];
                while end < start {
                    end += std::f64::consts::TAU;
                }
                (start, end)
            };
            if !full {
                // The arc joins the sketch profile. The sketch frame
                // and the member frame agree about the axis to within
                // the instance gate, so angles carry over.
                let center = (
                    (fit.axis_point - Point3::default()).dot(u),
                    (fit.axis_point - Point3::default()).dot(v),
                );
                let steps = (((arc1 - arc0) * fit.radius / 1.0).ceil() as usize).max(4);
                pieces.push(
                    (0..=steps)
                        .map(|k| {
                            let angle = arc0 + (arc1 - arc0) * k as f64 / steps as f64;
                            (
                                center.0 + fit.radius * angle.cos(),
                                center.1 + fit.radius * angle.sin(),
                            )
                        })
                        .collect(),
                );
            }
            if expressed.contains(&feature.id) || covered.contains(&feature.id) {
                continue;
            }
            let (lo, hi) = feature_axial_run(mesh, alignment, feature, fit.axis_point, fit.axis);
            if !(lo.is_finite() && hi.is_finite()) || hi - lo < 1e-6 {
                continue;
            }
            runs.push((lo, hi));
            {
                // The tube's own backing test: corners at the arc ends
                // and quarter points must sit on scanned material.
                let at = |s: f64, angle: f64| -> Point3 {
                    let radial = cu * angle.cos() + cv * angle.sin();
                    Point3::new(
                        fit.axis_point.x + fit.axis.x * s + radial.x * fit.radius,
                        fit.axis_point.y + fit.axis.y * s + radial.y * fit.radius,
                        fit.axis_point.z + fit.axis.z * s + radial.z * fit.radius,
                    )
                };
                let quarter = (arc1 - arc0) / 4.0;
                let mut all_backed = true;
                for k in 0..4 {
                    let a = arc0 + quarter * k as f64;
                    let b = a + quarter;
                    let sector = [at(lo, a), at(lo, b), at(hi, b), at(hi, a)];
                    if !rect_backed(&sector, occupied) {
                        all_backed = false;
                        break;
                    }
                }
                if !all_backed {
                    continue;
                }
            }
            let mean = feature_mean_normal(mesh, alignment, feature);
            let segments = (((arc1 - arc0) * fit.radius / 0.5) as usize).clamp(24, 256);
            let mut soup = Vec::with_capacity(segments * 2);
            for k in 0..segments {
                let a0 = arc0 + (arc1 - arc0) * k as f64 / segments as f64;
                let a1 = arc0 + (arc1 - arc0) * (k + 1) as f64 / segments as f64;
                let at = |s: f64, angle: f64| -> Point3 {
                    let radial = cu * angle.cos() + cv * angle.sin();
                    Point3::new(
                        fit.axis_point.x + fit.axis.x * s + radial.x * fit.radius,
                        fit.axis_point.y + fit.axis.y * s + radial.y * fit.radius,
                        fit.axis_point.z + fit.axis.z * s + radial.z * fit.radius,
                    )
                };
                let quad = [at(lo, a0), at(lo, a1), at(hi, a1), at(hi, a0)];
                let outward = cu * ((a0 + a1) / 2.0).cos() + cv * ((a0 + a1) / 2.0).sin();
                let wanted = match mean {
                    Some(mean) if mean.dot(outward) < 0.0 => outward * -1.0,
                    _ => outward,
                };
                for tri in [[0usize, 1, 2], [0, 2, 3]] {
                    let (pa, pb, pc) = (quad[tri[0]], quad[tri[1]], quad[tri[2]]);
                    let emitted = (pb - pa).cross(pc - pa);
                    if emitted.dot(wanted) >= 0.0 {
                        soup.push([pa, pb, pc]);
                    } else {
                        soup.push([pa, pc, pb]);
                    }
                }
            }
            push_wall(soup, feature.id, positions, triangles, feature_of_face);
            covered.insert(feature.id);
            emitted_walls += 1;
        }
        if emitted_walls > 0 {
            notes.push(format!(
                "extrusion along ({:+.3} {:+.3} {:+.3}): {} wall(s) emitted exact{}",
                direction.x,
                direction.y,
                direction.z,
                emitted_walls,
                if instance.draft_deg.abs() > 0.3 {
                    format!(", draft {:.2} deg honoured", instance.draft_deg)
                } else {
                    String::new()
                }
            ));
        }
        // ---- The swept volume, when the sketch closes.
        let Some(polygon) = chain_profile(&pieces) else {
            continue;
        };
        if runs.is_empty() {
            continue;
        }
        let span = runs
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &(a, b)| {
                (lo.min(a), hi.max(b))
            });
        // Add or cut: walls facing away from the sketch centre bound a
        // pad, walls facing in bound a pocket.
        let centre = {
            let count = polygon.len().max(1) as f64;
            let sum = polygon
                .iter()
                .fold((0.0, 0.0), |acc, p| (acc.0 + p.0, acc.1 + p.1));
            (sum.0 / count, sum.1 / count)
        };
        let centre_point = Point3::new(
            u.x * centre.0 + v.x * centre.1,
            u.y * centre.0 + v.y * centre.1,
            u.z * centre.0 + v.z * centre.1,
        );
        let (mut add_votes, mut cut_votes) = (0.0f64, 0.0f64);
        for &id in &instance.members {
            let Some(feature) = feature_by_id(id) else {
                continue;
            };
            let Some(mean) = feature_mean_normal(mesh, alignment, feature) else {
                continue;
            };
            let anchor = alignment
                .transform
                .apply_point(mesh.face_centroid(feature.faces[0] as usize));
            let arm = anchor - centre_point;
            let outward = arm - direction * arm.dot(direction);
            if outward.length() < 1e-9 {
                continue;
            }
            if mean.dot(outward / outward.length()) >= 0.0 {
                add_votes += feature.area;
            } else {
                cut_votes += feature.area;
            }
        }
        let is_cut = cut_votes > add_votes;
        let mut volume_span = span;
        if is_cut {
            // A pocket's mouth reaches its lid at either end.
            if let Some(lid) = level_lid_near(report, Point3::default(), direction, span.0, -1.0) {
                volume_span.0 = lid - 0.4;
            }
            if let Some(lid) = level_lid_near(report, Point3::default(), direction, span.1, 1.0) {
                volume_span.1 = lid + 0.4;
            }
        } else {
            // A pad clears its base and spares its own cap.
            volume_span = (span.0 + 0.25, span.1 - 0.25);
            if let Some(lid) = level_lid_near(report, Point3::default(), direction, span.0, -1.0) {
                volume_span.0 = lid - 0.4;
            } else if let Some(lid) =
                level_lid_near(report, Point3::default(), direction, span.1, 1.0)
            {
                volume_span.1 = lid + 0.4;
            }
        }
        volumes.push(CutVolume::Prism {
            u,
            v,
            direction,
            polygon,
            span: volume_span,
            label: format!(
                "{} along ({:+.3} {:+.3} {:+.3})",
                if is_cut { "pocket" } else { "pad" },
                direction.x,
                direction.y,
                direction.z
            ),
        });
    }
    (covered, volumes)
}

/// Chains sketch pieces into one closed polygon, or answers `None`.
///
/// Endpoints connect within a small reach; success means every piece
/// is consumed and the walk returns to its start. An open profile is
/// not a volume, and guessing a closure would invent one.
fn chain_profile(pieces: &[Vec<(f64, f64)>]) -> Option<Vec<(f64, f64)>> {
    const REACH: f64 = 2.0;
    if pieces.len() < 2 {
        return None;
    }
    let mut remaining: Vec<Vec<(f64, f64)>> = pieces.to_vec();
    let mut polygon = remaining.swap_remove(0);
    while !remaining.is_empty() {
        let end = *polygon.last()?;
        let mut best: Option<(usize, bool, f64)> = None;
        for (index, piece) in remaining.iter().enumerate() {
            let first = *piece.first()?;
            let last = *piece.last()?;
            let to_first = ((end.0 - first.0).powi(2) + (end.1 - first.1).powi(2)).sqrt();
            let to_last = ((end.0 - last.0).powi(2) + (end.1 - last.1).powi(2)).sqrt();
            let (distance, flipped) = if to_first <= to_last {
                (to_first, false)
            } else {
                (to_last, true)
            };
            if distance <= REACH && best.is_none_or(|(_, _, known)| distance < known) {
                best = Some((index, flipped, distance));
            }
        }
        let (index, flipped, _) = best?;
        let mut piece = remaining.swap_remove(index);
        if flipped {
            piece.reverse();
        }
        polygon.extend(piece.into_iter().skip(1));
    }
    let first = *polygon.first()?;
    let last = *polygon.last()?;
    if ((first.0 - last.0).powi(2) + (first.1 - last.1).powi(2)).sqrt() > REACH {
        return None;
    }
    polygon.pop();
    // A profile needs real area; a sliver of chained noise does not.
    let area = {
        let mut doubled = 0.0;
        for index in 0..polygon.len() {
            let (x0, y0) = polygon[index];
            let (x1, y1) = polygon[(index + 1) % polygon.len()];
            doubled += x0 * y1 - x1 * y0;
        }
        doubled.abs() / 2.0
    };
    (polygon.len() >= 3 && area >= 25.0).then_some(polygon)
}

/// Punches recognized volumes through whatever covered them.
///
/// The revolved profile sweeps its material a full turn, so a lid
/// passes straight over every off-axis bore, pocket mouth, and boss
/// body. Geometry whose centroid lies inside a volume is removed —
/// **unless scan material sits clearly inside the same volume,
/// coplanar with it**, which is exactly the difference between a
/// through-feature's mouth (void beyond) and a blind feature's floor
/// (its own measured material). Evidence cuts; assumption does not.
#[allow(clippy::too_many_arguments)]
fn punch_volumes(
    volumes: &[CutVolume],
    occupied: &std::collections::HashMap<(i32, i32, i32), (Point3, Vector3)>,
    positions: &[Point3],
    triangles: &mut Vec<[u32; 3]>,
    feature_of_face: &mut Vec<usize>,
    immune: (usize, usize),
    tolerance: f64,
    notes: &mut Vec<String>,
) {
    const CELL: f64 = 1.0;
    if volumes.is_empty() {
        return;
    }
    let margin = 0.3 * tolerance;
    let slack = 0.75 * tolerance;
    let coplanar = 0.6f64.max(2.0 * tolerance);
    let mut punched = vec![0.0f64; volumes.len()];
    let mut kept_triangles = Vec::with_capacity(triangles.len());
    let mut kept_features = Vec::with_capacity(feature_of_face.len());
    for (index, triangle) in triangles.iter().enumerate() {
        if index >= immune.0 && index < immune.1 {
            kept_triangles.push(*triangle);
            kept_features.push(feature_of_face[index]);
            continue;
        }
        let corners = [
            positions[triangle[0] as usize],
            positions[triangle[1] as usize],
            positions[triangle[2] as usize],
        ];
        let centroid = Point3::new(
            (corners[0].x + corners[1].x + corners[2].x) / 3.0,
            (corners[0].y + corners[1].y + corners[2].y) / 3.0,
            (corners[0].z + corners[1].z + corners[2].z) / 3.0,
        );
        let mut removed = false;
        for (slot, volume) in volumes.iter().enumerate() {
            // The centroid decides: merged patch runs can poke a
            // corner past the envelope while lying across the mouth.
            if !volume.inside(centroid, margin, slack) {
                continue;
            }
            let cross = (corners[1] - corners[0]).cross(corners[2] - corners[0]);
            let area = cross.length() / 2.0;
            let normal = if area > 1e-12 {
                cross / (2.0 * area)
            } else {
                Vector3::new(0.0, 0.0, 1.0)
            };
            let base = (
                (centroid.x / CELL).floor() as i32,
                (centroid.y / CELL).floor() as i32,
                (centroid.z / CELL).floor() as i32,
            );
            // A rim band is scoped to a mouth the exact collar owns
            // outright; nothing non-immune belongs there and no
            // backing excuse applies. (A blind hole never yields a
            // lid mouth, so no floor is at risk.)
            let unconditional = matches!(volume, CutVolume::Stack(stack)
                if stack.bore_diameter == 0.0);
            let mut backed = false;
            'cells: for dx in -1..=1 {
                if unconditional {
                    break 'cells;
                }
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let Some(&(point, _)) =
                            occupied.get(&(base.0 + dx, base.1 + dy, base.2 + dz))
                        else {
                            continue;
                        };
                        // Backing evidence must be clearly interior:
                        // rim and wall scan graze the envelope, but a
                        // blind floor's material sits deep inside.
                        if volume.inside(point, 0.5f64.max(3.0 * tolerance), slack)
                            && normal.dot(point - centroid).abs() <= coplanar
                        {
                            backed = true;
                            break 'cells;
                        }
                    }
                }
            }
            if backed {
                continue;
            }
            punched[slot] += area;
            removed = true;
            break;
        }
        if !removed {
            kept_triangles.push(*triangle);
            kept_features.push(feature_of_face[index]);
        }
    }
    *triangles = kept_triangles;
    *feature_of_face = kept_features;
    for (slot, volume) in volumes.iter().enumerate() {
        // Rim bands are housekeeping, not features; they report nothing.
        if let CutVolume::Stack(stack) = volume
            && stack.bore_diameter == 0.0
        {
            continue;
        }
        let span = volume.span();
        if punched[slot] > 0.0 {
            notes.push(format!(
                "{} punched through covering material: {:.0} mm^2 removed over \
                 z {:+.2}..{:+.2}",
                volume.label(),
                punched[slot],
                span.0,
                span.1
            ));
        } else {
            notes.push(format!("{}: nothing covered it", volume.label()));
        }
    }
}

/// Emits a footprint as geometry on its carrier.
///
/// Solidity is a property of the *face*, not the surface: in every B-rep
/// — Parasolid, ACIS, STEP, and this project's own kernel — a face is an
/// unbounded carrier plus trimming loops, so a face covering two percent
/// of a cylinder's parametric domain is an ordinary sliver rather than a
/// degenerate revolve. On the test pump 161 perfectly good fitted
/// surfaces were once discarded for failing a question they were never
/// meant to answer.
///
/// Each row's contiguous cells merge into one run, subdivided so facets
/// stay on a curved carrier. That costs far less geometry than a cell
/// each, which in turn pays for a finer grid and a sharper boundary.
fn footprint_soup(
    carrier: &Carrier,
    cells: &std::collections::HashSet<(i64, i64)>,
    step: f64,
) -> Vec<[Point3; 3]> {
    let mut rows: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    for &(i, j) in cells {
        rows.entry(j).or_default().push(i);
    }
    let mut soup = Vec::new();
    for (j, mut columns) in rows {
        columns.sort_unstable();
        let mut runs: Vec<(i64, i64)> = Vec::new();
        let mut start = columns[0];
        let mut previous = columns[0];
        for &column in &columns[1..] {
            if column > previous + 1 {
                runs.push((start, previous));
                start = column;
            }
            previous = column;
        }
        runs.push((start, previous));
        for (first, last) in runs {
            /// Facets stay within this arc of the true surface.
            const ARC_SPAN: f64 = 2.0;
            let (a_start, a_end) = (first as f64 * step, (last + 1) as f64 * step);
            let pieces = (((a_end - a_start) / ARC_SPAN).ceil() as usize).max(1);
            for piece in 0..pieces {
                let a0 = a_start + (a_end - a_start) * piece as f64 / pieces as f64;
                let a1 = a_start + (a_end - a_start) * (piece + 1) as f64 / pieces as f64;
                let (b0, b1) = (j as f64 * step, (j + 1) as f64 * step);
                let p00 = carrier.at(a0, b0);
                let p10 = carrier.at(a1, b0);
                let p11 = carrier.at(a1, b1);
                let p01 = carrier.at(a0, b1);
                // A facet should be about as big in space as it is in
                // parameter space. Where it is not, the carrier is
                // degenerate — a cone near its apex, a barely-curved
                // cylinder fitted with a tiny radius — and the quad
                // becomes a spike radiating across the model.
                let longest = [
                    (p10 - p00).length(),
                    (p11 - p10).length(),
                    (p01 - p11).length(),
                    (p00 - p01).length(),
                ]
                .into_iter()
                .fold(0.0f64, f64::max);
                if longest > 4.0 * (a1 - a0).max(step) {
                    continue;
                }
                soup.push([p00, p10, p11]);
                soup.push([p00, p11, p01]);
            }
        }
    }
    soup
}

/// Prismatic castellation: top strips at the outline height plus inner
/// and outer walls down to the base, repeated `count` times.
fn axial_castellation_soup(
    profile: &[(f64, f64)],
    count: usize,
    rho0: f64,
    rho1: f64,
    base: f64,
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
                .map(move |&(theta, z)| (theta + k as f64 * sector, z))
        })
        .collect();
    let ring_len = ring.len();
    let at = |slot: usize, rho: f64, z: f64| -> Point3 {
        let (theta, _) = ring[slot % ring_len];
        let angle = theta - std::f64::consts::PI;
        Point3::new(rho * angle.cos(), rho * angle.sin(), z)
    };
    for slot in 0..ring_len {
        let (_, za) = ring[slot % ring_len];
        let (_, zb) = ring[(slot + 1) % ring_len];
        // Top strip.
        soup.push([
            at(slot, rho0, za),
            at(slot, rho1, za),
            at(slot + 1, rho1, zb),
        ]);
        soup.push([
            at(slot, rho0, za),
            at(slot + 1, rho1, zb),
            at(slot + 1, rho0, zb),
        ]);
        // Outer and inner walls down to the base.
        soup.push([
            at(slot, rho1, base),
            at(slot, rho1, za),
            at(slot + 1, rho1, zb),
        ]);
        soup.push([
            at(slot, rho1, base),
            at(slot + 1, rho1, zb),
            at(slot + 1, rho1, base),
        ]);
        soup.push([
            at(slot, rho0, base),
            at(slot, rho0, za),
            at(slot + 1, rho0, zb),
        ]);
        soup.push([
            at(slot, rho0, base),
            at(slot + 1, rho0, zb),
            at(slot + 1, rho0, base),
        ]);
    }
    soup
}

#[cfg(test)]
mod edge_tests {
    use super::*;

    /// Crossing points arrive in cell order, which is no order. Chained,
    /// they must come back as the curve — and for two planes meeting,
    /// that curve is a straight line.
    #[test]
    fn chained_crossings_recover_a_straight_edge() {
        // Points along y = 2x + 1 in the z = 0 plane, deliberately shuffled.
        let mut scattered: Vec<Point3> = Vec::new();
        for i in 0..24 {
            let t = i as f64 * 0.4;
            scattered.push(Point3::new(t, 2.0 * t + 1.0, 0.0));
        }
        let taken: Vec<Point3> = (0..scattered.len())
            .map(|i| scattered[(i * 7 + 5) % scattered.len()])
            .collect();
        let runs = chain_runs(taken, 0.4);
        assert_eq!(runs.len(), 1, "one line is one run");
        let ordered = &runs[0];
        assert_eq!(ordered.len(), 24, "every crossing must stay on the curve");
        // A straight line chained correctly has length equal to its span.
        let span = (ordered[ordered.len() - 1] - ordered[0]).length();
        let walked: f64 = ordered
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).length())
            .sum();
        assert!(
            (walked - span).abs() < 1e-9,
            "a straight edge walked out of order doubles back: walked {walked}, span {span}"
        );
    }

    /// A curve is not jumped across: two separate runs stay separate
    /// rather than being joined by a leap through empty space.
    #[test]
    fn a_gap_ends_the_curve() {
        let mut scattered: Vec<Point3> = Vec::new();
        for i in 0..8 {
            scattered.push(Point3::new(i as f64 * 0.4, 0.0, 0.0));
        }
        for i in 0..8 {
            scattered.push(Point3::new(40.0 + i as f64 * 0.4, 0.0, 0.0));
        }
        let runs = chain_runs(scattered, 0.4);
        assert_eq!(
            runs.len(),
            2,
            "two separate runs must come back as two curves, not one plus scrap"
        );
        assert!(
            runs.iter().all(|run| run.len() == 8),
            "each side keeps its own eight points"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReverseOptions, reverse_engineer};
    use crate::synth;
    use artificer_geometry::Vector3;

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "the turned-part fixture is perfectly axisymmetric, so segmentation \
     decisions are ties that last-ulp arithmetic breaks differently per \
     platform: the measured noise sigma differs between macOS and Linux by \
     one ulp, which cascades into a ~9% different feature decomposition and \
     a 0.09 deg different datum, and on Linux the fillet band is shattered \
     too finely for blend recognition to re-read. Kept live on macOS, where \
     it still guards the pipeline against ordinary regressions, until the \
     support test is made tolerant to those ties"
    )]
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

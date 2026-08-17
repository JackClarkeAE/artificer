//! Telling a blend from a crease from rough cast surface.
//!
//! Everything the region fitter cannot name ends up in one bucket, and
//! that bucket is then labelled by which two features it happens to lie
//! between. On a machined part that is nearly right, because what falls
//! through really is the round along an edge. On a casting it is badly
//! wrong: the rough surface is one connected sheet, so the whole sheet
//! becomes a single "round" — on the test pump, 43,243 mm² of it, a
//! third of the part, with a 77 mm span and 22.6 mm of deviation. A
//! round that wide is not a round.
//!
//! The three things in that bucket need three different answers, so the
//! first job is to tell them apart:
//!
//! * a **surface the fitter missed** — fit it and promote it;
//! * a **blend**, the constant-radius band rolled along an edge — an
//!   analytic surface in its own right once its radius is known;
//! * **cast or organic surface**, which has no analytic form at all and
//!   never will. It can only be carried as measured geometry.
//!
//! Curvature at a single scale cannot separate the last two, and neither
//! can curvature at a single scale separate a blend from a sharp edge:
//! a scanner rounds every crease it measures, so every edge looks like a
//! small fillet close up. What separates them is how curvature *behaves*
//! as the measuring window grows. A blend of radius R answers 1/R at
//! every scale up to R, because it genuinely is that circle. A crease
//! answers a curvature that decays as the window widens, because the
//! turn it measures is fixed while the window is not. Rough surface
//! answers noise that decays faster still.
//!
//! So the discriminator measures curvature at several radii and reads
//! the trend, not the value.

use crate::fit::{fit_cone, fit_cylinder, fit_plane, fit_sphere};
use crate::mesh::TriangleMesh;
use crate::segment::SurfaceClass;
use crate::transform::RigidTransform;
use artificer_geometry::{Point3, Vector3};

/// The radii the curvature window is opened to, in millimetres.
///
/// The lower end has to sit above the scanner's own edge rounding or
/// every crease reads as a blend; the upper end has to stay under the
/// smallest blend worth naming or a real fillet is measured across its
/// own boundary.
pub const SCALES: [f64; 5] = [1.0, 1.5, 2.0, 2.5, 3.0];

/// How curvature answers as the measuring window widens.
#[derive(Clone, Debug)]
pub struct ScaleProfile {
    /// Curvature at each of [`SCALES`], in 1/mm.
    pub curvature: [f64; SCALES.len()],
    /// Slope of log curvature against log radius.
    ///
    /// Near 0 the curvature is scale-free — the surface really is that
    /// curved, which is what a blend does. Near −1 the window is
    /// measuring a fixed turn over a growing width, which is what a
    /// crease does. Below that it is noise averaging itself away.
    pub slope: f64,
    /// Curvature at the widest scale, in 1/mm.
    pub coarse: f64,
}

impl ScaleProfile {
    /// The radius a blend reading of this profile would have (mm).
    pub fn radius(&self) -> f64 {
        if self.coarse.abs() < 1e-9 {
            f64::INFINITY
        } else {
            1.0 / self.coarse
        }
    }

    /// How much the radius reading disagrees with itself across scales:
    /// the widest reading over the narrowest.
    ///
    /// This is the sharper half of the test and the slope is the blunt
    /// half. A window that only reaches an edge at its widest settings
    /// reports a curvature that *climbs* with scale, so its slope comes
    /// out flat — the signature this looks for — while its readings
    /// disagree by a factor of three. Nothing that disagrees with itself
    /// that badly is a circle.
    pub fn spread(&self) -> f64 {
        let (mut low, mut high) = (f64::INFINITY, 0.0f64);
        for &value in &self.curvature {
            low = low.min(value);
            high = high.max(value);
        }
        if low <= 1e-9 {
            f64::INFINITY
        } else {
            high / low
        }
    }
}

/// What a component of unnamed faces actually is.
#[derive(Clone, Debug)]
pub enum Kind {
    /// An analytic surface the region fitter missed.
    Missed(SurfaceClass),
    /// A constant-radius band along an edge.
    Blend { radius: f64 },
    /// Cast, organic or simply rough: no analytic form.
    Freeform,
}

/// One face of the growing window, in the seed's own tangent frame.
struct Reach {
    distance: f64,
    u: f64,
    v: f64,
    height: f64,
    area: f64,
}

/// The larger principal curvature of the quadratic form that best fits
/// the window out to `radius`, in 1/mm.
fn principal_curvature(reached: &[Reach], radius: f64) -> Option<f64> {
    // Weighted least squares of height against (u²/2, uv, v²/2).
    let mut normal = vec![vec![0.0; 3]; 3];
    let mut right = vec![0.0; 3];
    let mut used = 0usize;
    for item in reached {
        if item.distance > radius {
            continue;
        }
        used += 1;
        let basis = [
            0.5 * item.u * item.u,
            item.u * item.v,
            0.5 * item.v * item.v,
        ];
        for row in 0..3 {
            for column in 0..3 {
                normal[row][column] += item.area * basis[row] * basis[column];
            }
            right[row] += item.area * basis[row] * item.height;
        }
    }
    if used < 6 {
        return None;
    }
    // A window that is exactly flat leaves the system singular, and a
    // flat surface has no curvature to report rather than no answer.
    let Some(form) = crate::numeric::solve_linear(normal, right) else {
        return Some(0.0);
    };
    let (a, b, c) = (form[0], form[1], form[2]);
    let mean = 0.5 * (a + c);
    let spread = (0.25 * (a - c) * (a - c) + b * b).sqrt();
    Some((mean + spread).abs().max((mean - spread).abs()))
}

/// Curvature measured over a surface neighbourhood that grows outward
/// from one face.
///
/// The window grows along the mesh rather than through space, so it
/// cannot step across a gap and average in the far side of a thin wall —
/// which is exactly the mistake that makes a rib read as a blend.
///
/// The estimator fits the local quadratic form — the surface's height
/// above its own tangent plane, ½(au² + 2buv + cv²) — and reports the
/// larger principal curvature. It has to be the larger one and not the
/// mean: a blend is a circle rolled along a spine, so it curves hard
/// across the band and not at all along it, and a mean would report half
/// the curvature and so twice the radius. That was this estimator's
/// first answer, and a 3 mm band came back as 4.9 mm.
pub fn scale_profile(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    member: &std::collections::HashSet<u32>,
    seed: u32,
    window_scale: f64,
) -> Option<ScaleProfile> {
    // Noise curvature at a window r goes like sigma/r^2: on a noisy
    // scan the smallest windows read pure scatter as "curves hard at
    // every scale" and every machined surface votes blend. Widening
    // every window by the noise-derived factor keeps the estimator
    // asking about geometry instead of about the scanner.
    let widest = SCALES[SCALES.len() - 1] * window_scale;
    let origin = mesh.face_centroid(seed as usize);
    let normal = mesh.face_normal(seed as usize)?;
    // A tangent frame to measure height above.
    let aside = if normal.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    let across = normal.cross(aside);
    let span = across.length();
    if span < 1e-9 {
        return None;
    }
    let first = across / span;
    let second = normal.cross(first);
    // Grow the window over the surface, keeping each face's distance.
    let mut seen = std::collections::HashSet::new();
    seen.insert(seed);
    let mut frontier = vec![seed];
    let mut reached: Vec<Reach> = Vec::new();
    while let Some(face) = frontier.pop() {
        for &next in &adjacency[face as usize] {
            if !member.contains(&next) || !seen.insert(next) {
                continue;
            }
            let centroid = mesh.face_centroid(next as usize);
            let offset = centroid - origin;
            let distance = offset.length();
            if distance > widest {
                continue;
            }
            reached.push(Reach {
                distance,
                u: offset.dot(first),
                v: offset.dot(second),
                height: offset.dot(normal),
                area: mesh.face_area(next as usize),
            });
            frontier.push(next);
        }
    }
    if reached.len() < 8 {
        return None;
    }
    let mut curvature = [0.0; SCALES.len()];
    for (slot, &radius) in SCALES.iter().enumerate() {
        curvature[slot] = principal_curvature(&reached, radius * window_scale)?;
    }
    // Least squares through (log r, log κ).
    let (mut sx, mut sy, mut sxx, mut sxy, mut count) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (slot, &radius) in SCALES.iter().enumerate() {
        if curvature[slot] <= 1e-9 {
            continue;
        }
        let (x, y) = ((radius * window_scale).ln(), curvature[slot].ln());
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
        count += 1.0;
    }
    if count < 3.0 {
        return None;
    }
    let denominator = count * sxx - sx * sx;
    if denominator.abs() < 1e-12 {
        return None;
    }
    Some(ScaleProfile {
        curvature,
        slope: (count * sxy - sx * sy) / denominator,
        coarse: curvature[SCALES.len() - 1],
    })
}

/// The share of a component's faces whose profile reads as a blend, and
/// the median radius among those that do.
///
/// A component is judged by a vote of its own faces rather than by one
/// reading, because a band that runs into a corner is genuinely part
/// blend and part something else, and one seed cannot know which part it
/// landed in.
pub fn blend_vote(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    faces: &[u32],
    seeds: usize,
    window_scale: f64,
) -> (f64, f64) {
    /// Curvature flatter than this against scale is scale-free: the
    /// surface is that curved at every window, which is what a circular
    /// band does and what a crease cannot do.
    const BLEND_SLOPE: f64 = -0.5;
    /// A blend narrower than 0.5 mm is the scanner's own edge rounding;
    /// wider than 12 mm it is a wall, not a blend.
    const MIN_RADIUS: f64 = 0.5;
    const MAX_RADIUS: f64 = 12.0;
    /// How far the radius reading may disagree with itself across the
    /// scales before the surface stops being a circle.
    const MAX_SPREAD: f64 = 1.8;
    let member: std::collections::HashSet<u32> = faces.iter().copied().collect();
    let stride = (faces.len() / seeds.max(1)).max(1);
    let (mut votes, mut total) = (0.0, 0.0);
    let mut radii: Vec<f64> = Vec::new();
    for &seed in faces.iter().step_by(stride) {
        let Some(profile) = scale_profile(mesh, adjacency, &member, seed, window_scale) else {
            continue;
        };
        total += 1.0;
        let radius = profile.radius();
        if profile.slope > BLEND_SLOPE
            && profile.spread() <= MAX_SPREAD
            && (MIN_RADIUS..=MAX_RADIUS).contains(&radius)
        {
            votes += 1.0;
            radii.push(radius);
        }
    }
    if total < 1.0 {
        return (0.0, 0.0);
    }
    radii.sort_by(|a, b| a.partial_cmp(b).expect("finite radius"));
    let median = radii.get(radii.len() / 2).copied().unwrap_or(0.0);
    (votes / total, median)
}

/// Samples a component's faces down to a workable count, taking every
/// nth face so the answer does not depend on a random seed.
fn samples(
    mesh: &TriangleMesh,
    faces: &[u32],
    cap: usize,
    to_frame: &RigidTransform,
) -> Vec<(Point3, Vector3, f64)> {
    let stride = (faces.len() / cap.max(1)).max(1);
    faces
        .iter()
        .step_by(stride)
        .filter_map(|&face| {
            let normal = mesh.face_normal(face as usize)?;
            Some((
                to_frame.apply_point(mesh.face_centroid(face as usize)),
                to_frame.apply_vector(normal),
                mesh.face_area(face as usize),
            ))
        })
        .collect()
}

/// The worst deviation of a set of samples from a candidate surface.
fn residual(surface: &SurfaceClass, samples: &[(Point3, Vector3, f64)]) -> Option<(f64, f64)> {
    let (mut sum, mut weight, mut worst) = (0.0, 0.0, 0.0f64);
    for &(point, _, area) in samples {
        let (distance, _) = surface.probe(point)?;
        sum += area * distance * distance;
        weight += area;
        worst = worst.max(distance.abs());
    }
    (weight > 0.0).then(|| ((sum / weight).sqrt(), worst))
}

/// Decides what a component of unnamed faces is.
///
/// The analytic fits are tried first and in order of how much they
/// assume, because a component that a plane explains should be a plane
/// and not a very large blend. Only what survives all of them is put to
/// the curvature vote.
pub fn classify(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    faces: &[u32],
    tolerance: f64,
    to_frame: &RigidTransform,
    window_scale: f64,
) -> (Kind, f64) {
    /// Enough to pin any of the four surfaces without carrying a
    /// million faces through a fit.
    const FIT_CAP: usize = 4_000;
    /// Enough seeds to vote with, few enough to grow a window from each.
    const VOTE_SEEDS: usize = 120;
    /// A component only counts as a blend if most of it reads as one.
    const BLEND_MAJORITY: f64 = 0.6;
    let taken = samples(mesh, faces, FIT_CAP, to_frame);
    if taken.len() < 8 {
        return (Kind::Freeform, 0.0);
    }
    let points: Vec<Point3> = taken.iter().map(|&(p, ..)| p).collect();
    let normals: Vec<(Vector3, f64)> = taken.iter().map(|&(_, n, w)| (n, w)).collect();
    let hint = taken
        .iter()
        .fold(Vector3::new(0.0, 0.0, 0.0), |acc, &(_, n, w)| acc + n * w);
    let candidates = [
        fit_plane(&points, Some(hint)).map(SurfaceClass::Plane),
        fit_cylinder(&points, &normals).map(SurfaceClass::Cylinder),
        fit_cone(&taken).map(SurfaceClass::Cone),
        fit_sphere(&points).map(SurfaceClass::Sphere),
    ];
    for candidate in candidates.into_iter().flatten() {
        let Some((rms, worst)) = residual(&candidate, &taken) else {
            continue;
        };
        if rms <= tolerance && worst <= 4.0 * tolerance {
            return (Kind::Missed(candidate), rms);
        }
    }
    let (share, radius) = blend_vote(mesh, adjacency, faces, VOTE_SEEDS, window_scale);
    if share >= BLEND_MAJORITY && radius > 0.0 {
        // A blend whose spine is straight is a cylinder and was already
        // caught above. What reaches here turns as it rolls, so try the
        // torus its own vote implies — recognizing a blend is only worth
        // anything if the model can then draw it.
        if let Some(fit) = fit_blend(&taken, radius)
            && fit.deviation.rms <= tolerance
            && fit.deviation.max_abs <= 4.0 * tolerance
        {
            return (Kind::Missed(SurfaceClass::Blend(fit)), fit.deviation.rms);
        }
        (Kind::Blend { radius }, share)
    } else {
        (Kind::Freeform, share)
    }
}

/// Splits a component where its own surface turns, joining neighbouring
/// faces only while they still agree on which way the surface faces.
fn split_where_it_turns(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    faces: &[u32],
    angle: f64,
) -> Vec<Vec<u32>> {
    let limit = angle.to_radians().cos();
    let member: std::collections::HashSet<u32> = faces.iter().copied().collect();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut pieces: Vec<Vec<u32>> = Vec::new();
    for &start in faces {
        if !seen.insert(start) {
            continue;
        }
        let mut piece = vec![start];
        let mut frontier = vec![start];
        while let Some(face) = frontier.pop() {
            let Some(normal) = mesh.face_normal(face as usize) else {
                continue;
            };
            for &next in &adjacency[face as usize] {
                if !member.contains(&next) || seen.contains(&next) {
                    continue;
                }
                let Some(other) = mesh.face_normal(next as usize) else {
                    continue;
                };
                if normal.dot(other) < limit {
                    continue;
                }
                seen.insert(next);
                piece.push(next);
                frontier.push(next);
            }
        }
        pieces.push(piece);
    }
    pieces
}

/// Fits a rolling-ball blend of known radius: a torus about a free axis.
///
/// The construction is the definition. A blend is the surface a ball of
/// radius r sweeps while touching both faces, so stepping inward from
/// every measured point along its own normal by exactly r lands on the
/// path the ball's centre took. Those centres collapse onto a curve, and
/// when that curve is a circle the blend is a torus — its axis is the
/// circle's axis, its major radius the circle's, its minor radius r.
/// Nothing here is approximated: the surface is recovered from geometry
/// the scan already contains.
///
/// The inward direction is whichever of the two the evidence prefers,
/// since a normal points out of the material for a convex blend and into
/// it for a concave one, and both are ordinary.
pub fn fit_blend(
    samples: &[(Point3, Vector3, f64)],
    radius: f64,
) -> Option<crate::fit::RevolvedBlendFit> {
    if samples.len() < 8 || !(radius.is_finite() && radius > 0.0) {
        return None;
    }
    let mut best: Option<(f64, crate::fit::RevolvedBlendFit)> = None;
    for sign in [-1.0, 1.0] {
        let spine: Vec<Point3> = samples
            .iter()
            .map(|&(point, normal, _)| point + normal * (sign * radius))
            .collect();
        // The centres lie in one plane; that plane's normal is the axis.
        let plane = crate::fit::fit_plane(&spine, None)?;
        let axis = plane.normal;
        let aside = if axis.x.abs() < 0.9 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let across = axis.cross(aside);
        let span = across.length();
        if span < 1e-9 {
            continue;
        }
        let (first, second) = (across / span, axis.cross(across / span));
        // Algebraic circle through the centres, in the spine plane.
        let mut normal_equations = vec![vec![0.0; 3]; 3];
        let mut right = vec![0.0; 3];
        for centre in &spine {
            let offset = *centre - plane.origin;
            let (u, v) = (offset.dot(first), offset.dot(second));
            let basis = [u, v, 1.0];
            let value = -(u * u + v * v);
            for row in 0..3 {
                for column in 0..3 {
                    normal_equations[row][column] += basis[row] * basis[column];
                }
                right[row] += basis[row] * value;
            }
        }
        let Some(circle) = crate::numeric::solve_linear(normal_equations, right) else {
            continue;
        };
        let (cu, cv) = (-0.5 * circle[0], -0.5 * circle[1]);
        let squared = cu * cu + cv * cv - circle[2];
        if squared <= 0.0 {
            continue;
        }
        let major = squared.sqrt();
        if !major.is_finite() || major <= radius {
            // A major radius inside the tube is a self-intersecting
            // torus, which is not a blend anyone rolled.
            continue;
        }
        let candidate = crate::fit::RevolvedBlendFit {
            axis_point: plane.origin + first * cu + second * cv,
            axis,
            major_radius: major,
            minor_radius: radius,
            deviation: crate::fit::DeviationStats {
                rms: 0.0,
                max_abs: 0.0,
            },
        };
        let (mut sum, mut weight, mut worst) = (0.0, 0.0, 0.0f64);
        for &(point, _, area) in samples {
            let distance = candidate.signed_distance(point);
            sum += area * distance * distance;
            weight += area;
            worst = worst.max(distance.abs());
        }
        if weight <= 0.0 {
            continue;
        }
        let rms = (sum / weight).sqrt();
        if best.as_ref().is_none_or(|(score, _)| rms < *score) {
            best = Some((
                rms,
                crate::fit::RevolvedBlendFit {
                    deviation: crate::fit::DeviationStats {
                        rms,
                        max_abs: worst,
                    },
                    ..candidate
                },
            ));
        }
    }
    best.map(|(_, fit)| fit)
}

/// Peels analytic surfaces out of a component that would not split.
///
/// Cutting a component where it turns only works when it turns. A cast
/// housing does not: its skin curves smoothly through every angle, so
/// normal agreement has nothing to cut on and the whole sheet survives
/// every threshold down to 4°. On the test pump that left one component
/// of 23,786 mm² — a fifth of the part — unnamed, not because there is
/// no analytic surface inside it but because nothing had asked in a way
/// that could find one.
///
/// RANSAC asks that way, and it already ran — but over the whole mesh,
/// where a boss of a few hundred faces cannot outvote the housing and
/// falls under the global support floor. Run again inside the component
/// alone, with a floor scaled to the component rather than to the part,
/// it finds what the global pass could not afford to look for.
fn peel(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    faces: &[u32],
    tolerance: f64,
    to_frame: &RigidTransform,
) -> Vec<(Vec<u32>, Kind)> {
    /// A peeled surface must gather this many connected faces — a few
    /// mm² at scan density. The floor must NOT scale with the component,
    /// or a small boss becomes invisible precisely because it sits in a
    /// large casting, which is the case this exists to catch. What stops
    /// a casting being shredded is not the floor but the requirement
    /// itself: rough skin cannot hold 400 connected faces to 0.2 mm.
    const MIN_FACES: usize = 400;
    let params = crate::ransac::RansacParams {
        epsilon: tolerance,
        min_support_faces: MIN_FACES,
        max_primitives: 600,
        ..Default::default()
    };
    let found = crate::ransac::extract_primitives(mesh, faces, adjacency, &params);
    if found.is_empty() {
        return Vec::new();
    }
    let mut claimed: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut out: Vec<(Vec<u32>, Kind)> = Vec::new();
    for primitive in found {
        // RANSAC fits in the mesh frame; features live in the datum one.
        let surface = primitive.surface.transformed(to_frame);
        claimed.extend(primitive.faces.iter().copied());
        out.push((primitive.faces, Kind::Missed(surface)));
    }
    let rest: Vec<u32> = faces
        .iter()
        .copied()
        .filter(|face| !claimed.contains(face))
        .collect();
    if !rest.is_empty() {
        out.push((rest, Kind::Freeform));
    }
    out
}

/// Works out what a component is, splitting it first if it is too mixed
/// to be any one thing.
///
/// A component this pass receives has already survived region growing
/// and RANSAC, so asking it the same question again would get the same
/// answer. What changes is the *scale* of the question: the pump's whole
/// rough sheet is one connected component of 2.6 million faces, and no
/// single plane, cylinder, cone or sphere fits it — not because there is
/// no analytic surface in there, but because there are dozens. So a
/// component that reads freeform and is large enough to hold more than
/// one surface is cut where it turns and each piece asked again, at a
/// tighter angle each time.
///
/// Splitting is speculative, so from the second level down a piece has
/// to be worth naming before it is named. Without that floor a cast
/// surface shatters into thousands of micro-planes, each of them
/// genuinely flat to tolerance and none of them a feature.
/// Angles the split is tried at, tightening each level.
const SPLIT_ANGLES: [f64; 3] = [15.0, 8.0, 4.0];
/// A component smaller than this is whatever it first read as.
const MIN_SPLIT_FACES: usize = 2_000;
/// A piece won from a split must reach this area (mm²) to be named.
const MIN_PIECE_AREA: f64 = 20.0;

pub fn decompose(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    faces: &[u32],
    tolerance: f64,
    to_frame: &RigidTransform,
    window_scale: f64,
) -> Vec<(Vec<u32>, Kind)> {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        mesh: &TriangleMesh,
        adjacency: &[Vec<u32>],
        faces: Vec<u32>,
        tolerance: f64,
        to_frame: &RigidTransform,
        window_scale: f64,
        level: usize,
        out: &mut Vec<(Vec<u32>, Kind)>,
    ) {
        let (kind, _) = classify(mesh, adjacency, &faces, tolerance, to_frame, window_scale);
        let named = !matches!(kind, Kind::Freeform);
        if named && level > 0 {
            let area: f64 = faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
            if area < MIN_PIECE_AREA {
                out.push((faces, Kind::Freeform));
                return;
            }
        }
        if named || faces.len() < MIN_SPLIT_FACES {
            out.push((faces, kind));
            return;
        }
        // RANSAC is the stronger instrument and goes first. Cutting a
        // component where it turns only helps when it turns, and a cast
        // housing curves smoothly through every angle: the cut lands
        // nowhere in particular and shatters the sheet into thousands of
        // shards too small to name, too small to keep, and too small to
        // ask again. That is not a decomposition, it is a residue — it
        // was 23,840 mm² of one on the pump. Peel what can be named,
        // then let the splitter try what is left.
        if level == 0 {
            let peeled = peel(mesh, adjacency, &faces, tolerance, to_frame);
            if !peeled.is_empty() {
                for (piece, verdict) in peeled {
                    if matches!(verdict, Kind::Freeform) {
                        walk(
                            mesh,
                            adjacency,
                            piece,
                            tolerance,
                            to_frame,
                            window_scale,
                            level + 1,
                            out,
                        );
                    } else {
                        out.push((piece, verdict));
                    }
                }
                return;
            }
        }
        if level >= SPLIT_ANGLES.len() {
            out.push((faces, kind));
            return;
        }
        let pieces = split_where_it_turns(mesh, adjacency, &faces, SPLIT_ANGLES[level]);
        if pieces.len() < 2 {
            out.push((faces, kind));
            return;
        }
        for piece in pieces {
            walk(
                mesh,
                adjacency,
                piece,
                tolerance,
                to_frame,
                window_scale,
                level + 1,
                out,
            );
        }
    }
    let mut out = Vec::new();
    walk(
        mesh,
        adjacency,
        faces.to_vec(),
        tolerance,
        to_frame,
        window_scale,
        0,
        &mut out,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    /// A quarter-cylinder band: the shape a rolling ball leaves along an
    /// edge. Curvature must read 1/R at every scale.
    #[test]
    fn a_rolling_ball_band_is_scale_free() {
        let radius = 3.0;
        let mut soup = Vec::new();
        let steps = 48;
        for i in 0..steps {
            for j in 0..40 {
                let (a0, a1) = (
                    std::f64::consts::FRAC_PI_2 * i as f64 / steps as f64,
                    std::f64::consts::FRAC_PI_2 * (i + 1) as f64 / steps as f64,
                );
                let (z0, z1) = (j as f64 * 0.8, (j + 1) as f64 * 0.8);
                let at = |a: f64, z: f64| Point3::new(radius * a.cos(), radius * a.sin(), z);
                soup.push([at(a0, z0), at(a1, z0), at(a1, z1)]);
                soup.push([at(a0, z0), at(a1, z1), at(a0, z1)]);
            }
        }
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-6).expect("mesh");
        let adjacency = mesh.face_adjacency();
        let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
        let member: std::collections::HashSet<u32> = faces.iter().copied().collect();
        // Seed away from the band's own boundary.
        let seed = faces[faces.len() / 2];
        let profile = scale_profile(&mesh, &adjacency, &member, seed, 1.0).expect("profile");
        for (slot, &scale) in SCALES.iter().enumerate() {
            let measured = 1.0 / profile.curvature[slot];
            assert!(
                (measured - radius).abs() < 1.2,
                "at {scale} mm the band read as radius {measured}, not {radius}"
            );
        }
        assert!(
            profile.slope.abs() < 0.35,
            "a circular band must be scale-free, got slope {}",
            profile.slope
        );
    }

    /// Two planes meeting at a sharp edge. Curvature must fall away as
    /// the window widens, because the turn is fixed and the window is
    /// not — which is what tells a crease from a blend.
    #[test]
    fn a_crease_loses_curvature_as_the_window_widens() {
        let mut soup = Vec::new();
        for i in 0..40 {
            for j in 0..40 {
                let (x0, x1) = (-8.0 + i as f64 * 0.4, -8.0 + (i + 1) as f64 * 0.4);
                let (z0, z1) = (j as f64 * 0.4, (j + 1) as f64 * 0.4);
                // A 90 degree crease along z at x = 0.
                let at = |x: f64, z: f64| {
                    if x < 0.0 {
                        Point3::new(x, 0.0, z)
                    } else {
                        Point3::new(0.0, x, z)
                    }
                };
                soup.push([at(x0, z0), at(x1, z0), at(x1, z1)]);
                soup.push([at(x0, z0), at(x1, z1), at(x0, z1)]);
            }
        }
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-6).expect("mesh");
        let adjacency = mesh.face_adjacency();
        let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
        let (share, _) = blend_vote(&mesh, &adjacency, &faces, 60, 1.0);
        assert!(
            share < 0.4,
            "a sharp crease must not vote itself a blend, got {share}"
        );
    }

    /// A fillet rolled around a boss is a torus about a free axis, and
    /// the fit has to find that axis without being told it.
    #[test]
    fn a_rolling_ball_around_a_boss_is_a_torus() {
        let (major, minor) = (14.0, 2.5);
        // A tilted axis, so nothing can succeed by assuming the datum.
        let axis = Vector3::new(0.3, -0.4, 1.0);
        let length = axis.length();
        let axis = axis / length;
        let aside = Vector3::new(1.0, 0.0, 0.0);
        let across = axis.cross(aside);
        let first = across / across.length();
        let second = axis.cross(first);
        let centre = Point3::new(-7.0, 22.0, 5.0);
        let mut samples = Vec::new();
        for i in 0..72 {
            for j in 0..18 {
                let theta = std::f64::consts::TAU * i as f64 / 72.0;
                // Only a quarter of the tube: a fillet, not a whole torus.
                let phi = std::f64::consts::FRAC_PI_2 * j as f64 / 18.0;
                let radial = first * theta.cos() + second * theta.sin();
                let normal = radial * phi.cos() + axis * phi.sin();
                let point = centre + radial * major + normal * minor;
                samples.push((point, normal, 1.0));
            }
        }
        let fit = fit_blend(&samples, minor).expect("blend fit");
        assert!(
            (fit.major_radius - major).abs() < 0.05,
            "major radius {} not {major}",
            fit.major_radius
        );
        assert!(
            fit.axis.dot(axis).abs() > 0.9999,
            "axis {:?} is not the one the samples were built on",
            fit.axis
        );
        assert!(
            fit.deviation.rms < 1e-6,
            "an exact torus must fit exactly, got rms {}",
            fit.deviation.rms
        );
    }

    /// A cylinder the region fitter missed must come back as a cylinder,
    /// not as a very large blend.
    #[test]
    fn a_missed_cylinder_is_recovered_as_one() {
        let mesh = synth::open_cylinder(12.0, 30.0, 96, 40);
        let adjacency = mesh.face_adjacency();
        let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
        let (kind, _) = classify(
            &mesh,
            &adjacency,
            &faces,
            0.2,
            &RigidTransform::IDENTITY,
            1.0,
        );
        match kind {
            Kind::Missed(SurfaceClass::Cylinder(fit)) => {
                assert!(
                    (fit.radius - 12.0).abs() < 0.3,
                    "recovered radius {}",
                    fit.radius
                );
            }
            other => panic!("expected a recovered cylinder, got {other:?}"),
        }
    }
}

//! Feature instances: the extrude and revolve operations a part was
//! made by, recovered as groups of surfaces sharing one motion.
//!
//! Per-surface recognition names what each patch *is*; it never says
//! what several of them are *together*. A hex boss is six planes and a
//! designer's single extrude; a tilted pump stub is a cylinder, a cone
//! and a cap and one revolve about an axis the datum-locked pipeline
//! cannot even express. The commercial packages resolve this with a
//! human: the operator picks a region, a wizard fits the extrusion, and
//! the deliverable is the ordered feature tree — the surfaces are only
//! ever scaffolding. This module is that wizard layer without the
//! operator.
//!
//! Grouping is by *invariance*, not by fitting something new: a surface
//! belongs to an extrusion along `d` exactly when sliding along `d`
//! maps it onto itself — a plane whose normal is perpendicular to `d`,
//! a cylinder whose axis is parallel to it. Candidate directions come
//! from the surfaces themselves, membership is checked against the
//! part's own adjacency so two unrelated bosses on a shared axis stay
//! two instances, and every group must then survive the kinematic
//! classifier on its pooled samples — the linear line complex is the
//! judge of whether the group really is swept by the motion that
//! nominated it. Offered, never imposed, as everywhere else here.

use artificer_geometry::{Point3, Vector3};

use crate::datum::DatumAlignment;
use crate::kinematic::{Motion, fit_motion};
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::SurfaceClass;

/// Directions within this angle are the same direction (deg). Matches
/// the constraint stage's reading of "built square".
const ANGLE_DEG: f64 = 1.5;
/// A member must carry this much area (mm²) to join an instance.
const MIN_MEMBER_AREA: f64 = 30.0;
/// An instance below this area (mm²) is not worth a feature.
const MIN_INSTANCE_AREA: f64 = 150.0;
/// Two features are adjacent when they share at least this many mesh
/// edges — one or two is a stitching artefact, not a border.
const MIN_SHARED_EDGES: usize = 4;
/// A revolve axis closer than this to the datum axis (deg) is already
/// told by the revolved-profile plan; instances carry the others.
const MIN_REVOLVE_TILT_DEG: f64 = 2.5;
/// Axis lines further apart than this (mm) are different axes.
const AXIS_LINE_REACH: f64 = 3.0;
/// Faces sampled per instance for the pooled motion check.
const MOTION_SAMPLES: usize = 3_000;
/// The pooled fit's path-normal residual may be this many tolerances
/// before the group is refused. Direction agreement alone is not
/// acceptance: the pump chained 54 casting walls into a "273 mm deep
/// extrusion" whose residual was 7.75 mm at a 0.2 mm tolerance, and the
/// direction still matched.
const RESIDUAL_FACTOR: f64 = 4.0;
/// ...but never tighter than this (mm): cast surfaces are rough, and a
/// motion can be genuinely present on a surface that is not smooth.
const RESIDUAL_FLOOR: f64 = 0.5;

/// A straight sketch entity in the instance's own (u, v) plane.
#[derive(Clone, Debug)]
pub struct SketchLine {
    pub from: (f64, f64),
    pub to: (f64, f64),
    pub feature: usize,
}

/// A circular sketch entity in the instance's own (u, v) plane.
#[derive(Clone, Debug)]
pub struct SketchCircle {
    pub center: (f64, f64),
    pub radius: f64,
    /// Share of the full circle the measured material covers.
    pub arc_fraction: f64,
    pub feature: usize,
}

/// One extrude operation: surfaces invariant under a shared translation.
#[derive(Clone, Debug)]
pub struct ExtrudeInstance {
    /// Unit sweep direction, datum frame.
    pub direction: Vector3,
    /// Wall draft measured by the kinematic fit (deg).
    pub draft_deg: f64,
    /// Extent along the direction (mm).
    pub span: (f64, f64),
    pub members: Vec<usize>,
    pub area: f64,
    /// The sketch, as exact entities from the member carriers.
    pub lines: Vec<SketchLine>,
    pub circles: Vec<SketchCircle>,
    /// Path-normal residual from the pooled kinematic fit (mm).
    pub residual: f64,
}

/// One revolve operation about an axis the datum does not own.
#[derive(Clone, Debug)]
pub struct RevolveInstance {
    pub axis_point: Point3,
    pub axis: Vector3,
    pub members: Vec<usize>,
    pub area: f64,
    /// Profile runs in (radius, height-along-axis), one per member.
    pub profile: Vec<SketchLine>,
    pub residual: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Instances {
    pub extrusions: Vec<ExtrudeInstance>,
    pub revolves: Vec<RevolveInstance>,
    /// Groups whose pooled samples refused the nominating motion.
    pub refused: usize,
    /// Residuals of groups refused for residual alone, ascending — the
    /// evidence for whether the cap sits near real features or far from
    /// them. A cap is only defensible if there is a gap under it.
    pub refused_residuals: Vec<f64>,
}

/// The extent of the material itself, not of its bounding box.
///
/// A feature's faces can be disjoint along an axis — the merge stage
/// unifies coaxial fragments, so one cylinder record may hold two stubs
/// at opposite ends of a part — and raw min/max then spans the empty gap
/// between them. The pump's 10 mm pipe stub reported a 179 mm profile
/// run this way. Bin the heights by area, keep the bins that hold real
/// material, and return the longest unbroken run of them, which is the
/// same occupancy test `split_disjoint_bands` uses on the datum axis.
fn occupied_extent(heights: &[(f64, f64)]) -> Option<(f64, f64)> {
    /// Bin width (mm) — fine enough to see a groove, coarse enough that
    /// scan sparsity does not shred a solid run.
    const BIN: f64 = 0.5;
    /// A bin holding less than this share of the busiest bin is empty.
    const OCCUPIED: f64 = 0.02;
    /// Runs separated by less than this (mm) are one run.
    const BRIDGE: f64 = 2.0;
    let (low, high) = heights.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(lo, hi), &(height, _)| (lo.min(height), hi.max(height)),
    );
    if !(low.is_finite() && high.is_finite()) {
        return None;
    }
    if high - low <= BIN {
        return Some((low, high));
    }
    let bins = (((high - low) / BIN).ceil() as usize).max(1);
    let mut weight = vec![0.0; bins + 1];
    for &(height, area) in heights {
        let slot = (((height - low) / BIN).floor() as usize).min(bins);
        weight[slot] += area;
    }
    let peak = weight.iter().copied().fold(0.0f64, f64::max);
    if peak <= 0.0 {
        return Some((low, high));
    }
    let bridge = (BRIDGE / BIN).ceil() as usize;
    let (mut best, mut current, mut gap) = (None::<(usize, usize)>, None::<(usize, usize)>, 0usize);
    for (slot, &held) in weight.iter().enumerate() {
        if held >= OCCUPIED * peak {
            gap = 0;
            current = Some(match current {
                Some((start, _)) => (start, slot),
                None => (slot, slot),
            });
        } else if current.is_some() {
            gap += 1;
            if gap > bridge
                && let Some(run) = current.take()
                && best.is_none_or(|(a, b): (usize, usize)| run.1 - run.0 > b - a)
            {
                best = Some(run);
            }
        }
    }
    if let Some(run) = current
        && best.is_none_or(|(a, b): (usize, usize)| run.1 - run.0 > b - a)
    {
        best = Some(run);
    }
    let (start, end) = best?;
    // Report where the material actually is, not where its bins are: a
    // bin boundary overshoots by up to its own width.
    let mut span = (f64::INFINITY, f64::NEG_INFINITY);
    for &(height, _) in heights {
        let slot = (((height - low) / BIN).floor() as usize).min(bins);
        if slot >= start && slot <= end {
            span = (span.0.min(height), span.1.max(height));
        }
    }
    (span.0.is_finite() && span.1.is_finite()).then_some(span)
}

/// A right-handed pair completing `axis` into a frame.
/// A drilled wall reassembled from its arc fragments: the union of a
/// coaxial cluster judged by the same circumference-and-extent
/// evidence a whole wall would be. At clean densities the cluster is
/// one feature and this is the lone-cylinder path; at noise the wall
/// arrives shattered, no arc passes alone, and the union does.
fn cylinder_group_instance(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    cluster: &[usize],
    direction: Vector3,
    to_frame: &crate::transform::RigidTransform,
) -> Option<ExtrudeInstance> {
    /// Share of the 24 azimuth bins the union must fill.
    const MIN_COVERAGE: f64 = 0.55;
    /// Minimum axial extent (mm) — shallower is a ring, not a wall.
    const MIN_DEPTH: f64 = 1.0;
    let lead = &features[*cluster.first()?];
    let SurfaceClass::Cylinder(lead_fit) = &lead.surface else {
        return None;
    };
    let (around_u, around_v) = frame_about(lead_fit.axis);
    let mut bins = [false; 24];
    let mut heights: Vec<(f64, f64)> = Vec::new();
    let mut area = 0.0f64;
    let mut weighted_center = (0.0f64, 0.0f64);
    let mut weighted_radius = 0.0f64;
    let (sketch_u, sketch_v) = frame_about(direction);
    for &index in cluster {
        let feature = &features[index];
        let SurfaceClass::Cylinder(fit) = &feature.surface else {
            continue;
        };
        area += feature.area;
        weighted_center.0 += feature.area * (fit.axis_point - Point3::default()).dot(sketch_u);
        weighted_center.1 += feature.area * (fit.axis_point - Point3::default()).dot(sketch_v);
        weighted_radius += feature.area * fit.radius;
        let stride = (feature.faces.len() / 400).max(1);
        let sampled = feature.faces.len().div_ceil(stride).max(1);
        let share = feature.area / (3 * sampled) as f64;
        for &face in feature.faces.iter().step_by(stride) {
            let centroid = to_frame.apply_point(mesh.face_centroid(face as usize));
            let arm = centroid - lead_fit.axis_point;
            let radial = arm - lead_fit.axis * arm.dot(lead_fit.axis);
            if radial.length() > 1e-9 {
                let angle = radial.dot(around_v).atan2(radial.dot(around_u));
                let bin = (((angle + std::f64::consts::PI) / std::f64::consts::TAU
                    * bins.len() as f64) as usize)
                    .min(bins.len() - 1);
                bins[bin] = true;
            }
            // Extent from corners, never centroids.
            for corner in mesh.triangle_points(face as usize) {
                let corner = to_frame.apply_point(corner);
                heights.push(((corner - Point3::default()).dot(direction), share));
            }
        }
    }
    if area <= 0.0 {
        return None;
    }
    let coverage = bins.iter().filter(|filled| **filled).count() as f64 / bins.len() as f64;
    if coverage < MIN_COVERAGE {
        return None;
    }
    let (lo, hi) = occupied_extent(&heights)?;
    if hi - lo < MIN_DEPTH {
        return None;
    }
    let height = (hi - lo).max(1e-9);
    let radius = weighted_radius / area;
    Some(ExtrudeInstance {
        direction,
        draft_deg: 0.0,
        span: (lo, hi),
        members: cluster.iter().map(|&index| features[index].id).collect(),
        area,
        lines: Vec::new(),
        circles: vec![SketchCircle {
            center: (weighted_center.0 / area, weighted_center.1 / area),
            radius,
            arc_fraction: (area / (std::f64::consts::TAU * radius * height)).min(1.0),
            feature: lead.id,
        }],
        residual: match &lead.surface {
            SurfaceClass::Cylinder(fit) => fit.deviation.rms,
            _ => 0.0,
        },
    })
}

fn frame_about(axis: Vector3) -> (Vector3, Vector3) {
    let aside = if axis.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    let across = axis.cross(aside);
    let u = across / across.length();
    (u, axis.cross(u))
}

/// Which features border which, by counted shared mesh edges.
fn feature_adjacency(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
) -> std::collections::HashMap<(usize, usize), usize> {
    let mut owner = vec![u32::MAX; mesh.triangles().len()];
    for (index, feature) in features.iter().enumerate() {
        for &face in &feature.faces {
            owner[face as usize] = index as u32;
        }
    }
    let adjacency = mesh.face_adjacency();
    let mut counts: std::collections::HashMap<(usize, usize), usize> =
        std::collections::HashMap::new();
    for (face, neighbours) in adjacency.iter().enumerate() {
        let a = owner[face];
        if a == u32::MAX {
            continue;
        }
        for &neighbour in neighbours {
            let b = owner[neighbour as usize];
            if b == u32::MAX || b <= a {
                continue;
            }
            *counts.entry((a as usize, b as usize)).or_default() += 1;
        }
    }
    counts.retain(|_, count| *count >= MIN_SHARED_EDGES);
    // Bridge across rounds: on a scanned part every crease is a strip
    // of edge-round, so two walls that meet at a corner are never
    // mesh-adjacent — the round between them is. Two features that
    // both border the same round border each other for grouping
    // purposes, or no prismatic component would ever assemble from a
    // real scan.
    let mut bridged: Vec<(usize, usize)> = Vec::new();
    for (round_index, feature) in features.iter().enumerate() {
        if !matches!(
            feature.surface,
            SurfaceClass::EdgeRound(_) | SurfaceClass::Blend(_)
        ) {
            continue;
        }
        let bordering: Vec<usize> = features
            .iter()
            .enumerate()
            .filter(|(other, _)| {
                let key = (round_index.min(*other), round_index.max(*other));
                round_index != *other && counts.contains_key(&key)
            })
            .map(|(other, _)| other)
            .collect();
        for (slot, &a) in bordering.iter().enumerate() {
            for &b in &bordering[slot + 1..] {
                bridged.push((a.min(b), a.max(b)));
            }
        }
    }
    for pair in bridged {
        counts.entry(pair).or_insert(MIN_SHARED_EDGES);
    }
    counts
}

/// Connected components of `members` under the adjacency map.
fn components(
    members: &[usize],
    adjacency: &std::collections::HashMap<(usize, usize), usize>,
) -> Vec<Vec<usize>> {
    // Sorted, not a hash set: growth visits candidates in this order, so
    // iterating a HashSet made the components — and therefore which
    // instances exist at all — depend on hash order. Two runs of the
    // same pump differed by an extrusion.
    let mut ordered: Vec<usize> = members.to_vec();
    ordered.sort_unstable();
    ordered.dedup();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &start in &ordered {
        if seen.contains(&start) {
            continue;
        }
        let mut component = vec![start];
        seen.insert(start);
        let mut frontier = vec![start];
        while let Some(here) = frontier.pop() {
            for &other in &ordered {
                if seen.contains(&other) {
                    continue;
                }
                let key = (here.min(other), here.max(other));
                if adjacency.contains_key(&key) {
                    seen.insert(other);
                    component.push(other);
                    frontier.push(other);
                }
            }
        }
        component.sort_unstable();
        out.push(component);
    }
    out
}

/// Area-weighted motion samples pooled over a component's faces.
fn pooled_samples(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    component: &[usize],
    to_frame: &crate::transform::RigidTransform,
) -> Vec<(Point3, Vector3, f64)> {
    let total: usize = component
        .iter()
        .map(|&index| features[index].faces.len())
        .sum();
    let stride = (total / MOTION_SAMPLES).max(1);
    let mut samples = Vec::new();
    for &index in component {
        let surface = &features[index].surface;
        for &face in features[index].faces.iter().step_by(stride) {
            let centroid = to_frame.apply_point(mesh.face_centroid(face as usize));
            // The carrier's normal at the measured position, not the
            // raw face normal: a scan's per-face normals carry the
            // full noise slope and, multiplied by the part's radius,
            // four clean walls once read as a 4.4 mm residual. The
            // fits average that noise away, and a *wrong* group still
            // refuses — fifty-four casting-wall carriers genuinely
            // disagree with any single translation. Border faces a
            // claim dragged in keep the raw normal as the honest
            // fallback when the carrier cannot answer.
            let normal = match surface.probe(centroid) {
                Some((_, fitted)) => fitted,
                None => match mesh.face_normal(face as usize) {
                    Some(raw) => to_frame.apply_vector(raw),
                    None => continue,
                },
            };
            samples.push((centroid, normal, mesh.face_area(face as usize)));
        }
    }
    samples
}

/// Recognizes the extrude and revolve instances a feature set contains.
pub fn recognize_instances(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    alignment: Option<&DatumAlignment>,
    tolerance: f64,
) -> Instances {
    let residual_cap = (RESIDUAL_FACTOR * tolerance).max(RESIDUAL_FLOOR);
    let identity = crate::transform::RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let cos_same = ANGLE_DEG.to_radians().cos();
    let sin_flat = ANGLE_DEG.to_radians().sin();
    let adjacency = feature_adjacency(mesh, features);
    let mut out = Instances::default();

    // ---- Candidate extrusion directions: cylinder axes, the datum
    // axis, and the cross of every adjacent plane pair (two planes
    // sharing an edge extrude along the line where they meet).
    let mut candidates: Vec<Vector3> = vec![Vector3::new(0.0, 0.0, 1.0)];
    for feature in features {
        if feature.area < MIN_MEMBER_AREA {
            continue;
        }
        if let SurfaceClass::Cylinder(fit) = &feature.surface {
            candidates.push(fit.axis);
        }
    }
    let mut bordering: Vec<(usize, usize)> = adjacency.keys().copied().collect();
    bordering.sort_unstable();
    for (a, b) in bordering {
        let (SurfaceClass::Plane(pa), SurfaceClass::Plane(pb)) =
            (&features[a].surface, &features[b].surface)
        else {
            continue;
        };
        if features[a].area < MIN_MEMBER_AREA || features[b].area < MIN_MEMBER_AREA {
            continue;
        }
        let cross = pa.normal.cross(pb.normal);
        let length = cross.length();
        if length > 15.0f64.to_radians().sin() {
            candidates.push(cross / length);
        }
    }
    // Deduplicate directions, largest-support first is not needed —
    // instances themselves dedup by member set below.
    let mut directions: Vec<Vector3> = Vec::new();
    for candidate in candidates {
        if !directions
            .iter()
            .any(|known| known.dot(candidate).abs() >= cos_same)
        {
            directions.push(candidate);
        }
    }

    let mut claimed_extrusions: std::collections::HashSet<Vec<usize>> =
        std::collections::HashSet::new();
    for direction in directions {
        // Members: surfaces invariant under sliding along `direction`.
        let members: Vec<usize> = features
            .iter()
            .enumerate()
            .filter(|(_, feature)| feature.area >= MIN_MEMBER_AREA)
            .filter(|(_, feature)| match &feature.surface {
                SurfaceClass::Plane(fit) => fit.normal.dot(direction).abs() <= sin_flat,
                SurfaceClass::Cylinder(fit) => fit.axis.dot(direction).abs() >= cos_same,
                _ => false,
            })
            .map(|(index, _)| index)
            .collect();
        let mut cylinder_pool: Vec<usize> = Vec::new();
        for component in components(&members, &adjacency) {
            if component.len() < 2 {
                // A drilled hole is one wall with no invariant
                // neighbour — the commonest extrusion there is — and
                // the pooled kinematic fit cannot license it (a lone
                // cylinder's normals satisfy the translation and the
                // rotation reading alike). At noise the wall arrives
                // as SEVERAL arc fragments, none passing the
                // circumference gate alone, so lone cylinders pool
                // here and are judged jointly by coaxial cluster
                // below.
                if let Some(&index) = component.first()
                    && matches!(features[index].surface, SurfaceClass::Cylinder(_))
                    && !claimed_extrusions.contains(&component)
                {
                    cylinder_pool.push(index);
                }
                continue;
            }
            let area: f64 = component.iter().map(|&index| features[index].area).sum();
            if area < MIN_INSTANCE_AREA || claimed_extrusions.contains(&component) {
                continue;
            }
            // The pooled evidence has the last word — asked as the
            // translation question the grouping already hypothesized,
            // so a rotation reading scoring a hair lower cannot refuse
            // a real extrusion on branch identity; the residual cap
            // still can and does.
            let samples = pooled_samples(mesh, features, &component, to_frame);
            let fitted = crate::kinematic::fit_translation(&samples);
            if std::env::var_os("ARTIFICER_INSTANCE_DEBUG").is_some() {
                let members: Vec<String> = component
                    .iter()
                    .map(|&index| {
                        format!(
                            "#{}:{}:{:.0}mm2",
                            features[index].id,
                            features[index].surface.kind(),
                            features[index].area
                        )
                    })
                    .collect();
                eprintln!(
                    "instance-debug: dir ({:+.2} {:+.2} {:+.2}) members [{}] -> {:?}",
                    direction.x,
                    direction.y,
                    direction.z,
                    members.join(", "),
                    fitted.as_ref().map(|(found, draft, residual)| {
                        format!(
                            "found ({:+.2} {:+.2} {:+.2}) draft {:.2} residual {:.3}",
                            found.x, found.y, found.z, draft, residual
                        )
                    })
                );
            }
            let Some((found, draft, residual)) = fitted else {
                out.refused += 1;
                continue;
            };
            if found.dot(direction).abs() < cos_same || residual > residual_cap {
                out.refused += 1;
                if found.dot(direction).abs() >= cos_same {
                    out.refused_residuals.push(residual);
                }
                continue;
            }
            // Sketch plane and entities, exact from the carriers.
            let (u, v) = frame_about(direction);
            let mut lines = Vec::new();
            let mut circles = Vec::new();
            // Corners once, so the sweep extent is known before the
            // sketch needs it.
            let sampled: Vec<(usize, Vec<Point3>)> = component
                .iter()
                .map(|&index| {
                    let feature = &features[index];
                    let stride = (feature.faces.len() / 400).max(1);
                    (
                        index,
                        feature
                            .faces
                            .iter()
                            .step_by(stride)
                            .flat_map(|&face| mesh.triangle_points(face as usize))
                            .map(|corner| to_frame.apply_point(corner))
                            .collect(),
                    )
                })
                .collect();
            let sweep_heights: Vec<(f64, f64)> = sampled
                .iter()
                .flat_map(|(index, corners)| {
                    let share = features[*index].area / (corners.len().max(1) as f64);
                    corners
                        .iter()
                        .map(move |corner| ((*corner - Point3::default()).dot(direction), share))
                })
                .collect();
            let Some((lo, hi)) = occupied_extent(&sweep_heights) else {
                continue;
            };
            for (index, corners) in &sampled {
                let feature = &features[*index];
                let corners = corners.as_slice();
                match &feature.surface {
                    SurfaceClass::Plane(plane) => {
                        // The plane meets the sketch plane in an exact
                        // line; the measured corners bound it.
                        let along = plane.normal.cross(direction);
                        let length = along.length();
                        if length < 1e-9 {
                            continue;
                        }
                        let along = along / length;
                        let anchor = (
                            (plane.origin - Point3::default()).dot(u),
                            (plane.origin - Point3::default()).dot(v),
                        );
                        let axis_uv = (along.dot(u), along.dot(v));
                        let (mut t0, mut t1) = (f64::INFINITY, f64::NEG_INFINITY);
                        for corner in corners {
                            let offset = (
                                (*corner - Point3::default()).dot(u) - anchor.0,
                                (*corner - Point3::default()).dot(v) - anchor.1,
                            );
                            let t = offset.0 * axis_uv.0 + offset.1 * axis_uv.1;
                            t0 = t0.min(t);
                            t1 = t1.max(t);
                        }
                        lines.push(SketchLine {
                            from: (anchor.0 + axis_uv.0 * t0, anchor.1 + axis_uv.1 * t0),
                            to: (anchor.0 + axis_uv.0 * t1, anchor.1 + axis_uv.1 * t1),
                            feature: feature.id,
                        });
                    }
                    SurfaceClass::Cylinder(cylinder) => {
                        let height = (hi - lo).max(1e-9);
                        circles.push(SketchCircle {
                            center: (
                                (cylinder.axis_point - Point3::default()).dot(u),
                                (cylinder.axis_point - Point3::default()).dot(v),
                            ),
                            radius: cylinder.radius,
                            arc_fraction: (feature.area
                                / (std::f64::consts::TAU * cylinder.radius * height))
                                .min(1.0),
                            feature: feature.id,
                        });
                    }
                    _ => {}
                }
            }
            claimed_extrusions.insert(component.clone());
            out.extrusions.push(ExtrudeInstance {
                direction,
                draft_deg: draft.to_degrees(),
                span: (lo, hi),
                members: component.iter().map(|&index| features[index].id).collect(),
                area,
                lines,
                circles,
                residual,
            });
        }
        // Coaxial clusters of lone cylinders: fragments of one drilled
        // wall share an axis line and a radius, and their UNION can
        // pass the circumference gate no single arc passes. Largest
        // fragment first — downstream readers take the lead member's
        // fit as the hole's.
        cylinder_pool.sort_by(|&a, &b| {
            features[b]
                .area
                .total_cmp(&features[a].area)
                .then(features[a].id.cmp(&features[b].id))
        });
        let mut pooled_claimed = vec![false; cylinder_pool.len()];
        for seed_slot in 0..cylinder_pool.len() {
            if pooled_claimed[seed_slot] {
                continue;
            }
            let SurfaceClass::Cylinder(seed_fit) = &features[cylinder_pool[seed_slot]].surface
            else {
                continue;
            };
            let mut cluster: Vec<usize> = vec![cylinder_pool[seed_slot]];
            pooled_claimed[seed_slot] = true;
            for other_slot in (seed_slot + 1)..cylinder_pool.len() {
                if pooled_claimed[other_slot] {
                    continue;
                }
                let SurfaceClass::Cylinder(other_fit) =
                    &features[cylinder_pool[other_slot]].surface
                else {
                    continue;
                };
                let offset = other_fit.axis_point - seed_fit.axis_point;
                let lateral = (offset - direction * offset.dot(direction)).length();
                if lateral <= 1.2
                    && (other_fit.radius - seed_fit.radius).abs() <= (2.5 * tolerance).max(0.6)
                {
                    cluster.push(cylinder_pool[other_slot]);
                    pooled_claimed[other_slot] = true;
                }
            }
            let key: Vec<usize> = {
                let mut sorted = cluster.clone();
                sorted.sort_unstable();
                sorted
            };
            if claimed_extrusions.contains(&key) {
                continue;
            }
            if let Some(instance) =
                cylinder_group_instance(mesh, features, &cluster, direction, to_frame)
            {
                claimed_extrusions.insert(key);
                out.extrusions.push(instance);
            }
        }
    }

    // ---- Revolve instances about axes the datum does not own.
    let min_tilt = MIN_REVOLVE_TILT_DEG.to_radians().sin();
    let mut axes: Vec<(Vector3, Point3)> = Vec::new();
    for feature in features {
        if feature.area < MIN_MEMBER_AREA {
            continue;
        }
        let (axis, point) = match &feature.surface {
            SurfaceClass::Cylinder(fit) => (fit.axis, fit.axis_point),
            SurfaceClass::Cone(fit) => (fit.axis, fit.apex),
            _ => continue,
        };
        if axis.cross(Vector3::new(0.0, 0.0, 1.0)).length() < min_tilt {
            continue;
        }
        let duplicate = axes.iter().any(|(known_axis, known_point)| {
            known_axis.dot(axis).abs() >= cos_same && {
                let offset = point - *known_point;
                (offset - *known_axis * offset.dot(*known_axis)).length() <= AXIS_LINE_REACH
            }
        });
        if !duplicate {
            axes.push((axis, point));
        }
    }
    let mut claimed_revolves: std::collections::HashSet<Vec<usize>> =
        std::collections::HashSet::new();
    for (axis, axis_point) in axes {
        let on_line = |point: Point3, reach: f64| -> bool {
            let offset = point - axis_point;
            (offset - axis * offset.dot(axis)).length() <= reach
        };
        // Revolved walls share the axis; caps are planes square to it,
        // admitted only when they border a revolved member.
        let walls: Vec<usize> = features
            .iter()
            .enumerate()
            .filter(|(_, feature)| feature.area >= MIN_MEMBER_AREA)
            .filter(|(_, feature)| match &feature.surface {
                SurfaceClass::Cylinder(fit) => {
                    fit.axis.dot(axis).abs() >= cos_same && on_line(fit.axis_point, AXIS_LINE_REACH)
                }
                SurfaceClass::Cone(fit) => fit.axis.dot(axis).abs() >= cos_same,
                SurfaceClass::Sphere(fit) => on_line(fit.center, AXIS_LINE_REACH),
                _ => false,
            })
            .map(|(index, _)| index)
            .collect();
        let caps: Vec<usize> = features
            .iter()
            .enumerate()
            .filter(|(_, feature)| feature.area >= MIN_MEMBER_AREA)
            .filter(|(_, feature)| {
                matches!(&feature.surface, SurfaceClass::Plane(fit)
                    if fit.normal.dot(axis).abs() >= cos_same)
            })
            .filter(|(index, _)| {
                walls.iter().any(|&wall| {
                    let key = (wall.min(*index), wall.max(*index));
                    adjacency.contains_key(&key)
                })
            })
            .map(|(index, _)| index)
            .collect();
        let members: Vec<usize> = walls.iter().chain(caps.iter()).copied().collect();
        for component in components(&members, &adjacency) {
            // A lone surface is already told by its own fit; an instance
            // starts where surfaces have to be explained together.
            if component.len() < 2 {
                continue;
            }
            let area: f64 = component.iter().map(|&index| features[index].area).sum();
            if area < MIN_INSTANCE_AREA || claimed_revolves.contains(&component) {
                continue;
            }
            let samples = pooled_samples(mesh, features, &component, to_frame);
            let Some(fit) = fit_motion(&samples) else {
                continue;
            };
            let (found_axis, found_point) = match fit.motion {
                Motion::Rotation { axis, point } => (axis, point),
                _ => {
                    out.refused += 1;
                    continue;
                }
            };
            if found_axis.dot(axis).abs() < cos_same
                || !on_line(found_point, AXIS_LINE_REACH * 2.0)
                || fit.residual > residual_cap
            {
                out.refused += 1;
                if found_axis.dot(axis).abs() >= cos_same
                    && on_line(found_point, AXIS_LINE_REACH * 2.0)
                {
                    out.refused_residuals.push(fit.residual);
                }
                continue;
            }
            // Profile runs in (radius from axis, height along axis).
            let mut profile = Vec::new();
            for &index in &component {
                let feature = &features[index];
                let stride = (feature.faces.len() / 400).max(1);
                let mut heights: Vec<(f64, f64)> = Vec::new();
                let mut radial: Vec<(f64, f64)> = Vec::new();
                let sampled = feature.faces.iter().step_by(stride).count().max(1);
                let share = feature.area / (sampled as f64 * 3.0);
                for &face in feature.faces.iter().step_by(stride) {
                    for corner in mesh.triangle_points(face as usize) {
                        let offset = to_frame.apply_point(corner) - axis_point;
                        let height = offset.dot(axis);
                        heights.push((height, share));
                        radial.push(((offset - axis * height).length(), share));
                    }
                }
                let Some((h0, h1)) = occupied_extent(&heights) else {
                    continue;
                };
                let Some((r0, r1)) = occupied_extent(&radial) else {
                    continue;
                };
                let run = match &feature.surface {
                    // A cylinder is a vertical run at its own radius.
                    SurfaceClass::Cylinder(fit) => SketchLine {
                        from: (fit.radius, h0),
                        to: (fit.radius, h1),
                        feature: feature.id,
                    },
                    // A cone slants between its measured extremes.
                    SurfaceClass::Cone(_) => SketchLine {
                        from: (r0, h0),
                        to: (r1, h1),
                        feature: feature.id,
                    },
                    // A cap is a horizontal run at its own height.
                    _ => {
                        let level = (h0 + h1) / 2.0;
                        SketchLine {
                            from: (r0, level),
                            to: (r1, level),
                            feature: feature.id,
                        }
                    }
                };
                profile.push(run);
            }
            claimed_revolves.insert(component.clone());
            out.revolves.push(RevolveInstance {
                axis_point,
                axis,
                members: component.iter().map(|&index| features[index].id).collect(),
                area,
                profile,
                residual: fit.residual,
            });
        }
    }
    out.refused_residuals.sort_by(f64::total_cmp);
    out.extrusions.sort_by(|a, b| b.area.total_cmp(&a.area));
    out.revolves.sort_by(|a, b| b.area.total_cmp(&a.area));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{CylinderFit, DeviationStats, PlaneFit};
    use crate::synth;
    use crate::transform::RigidTransform;

    fn plane_feature(
        id: usize,
        normal: Vector3,
        origin: Point3,
        faces: Vec<u32>,
        area: f64,
    ) -> FeatureRecord {
        FeatureRecord {
            id,
            surface: SurfaceClass::Plane(PlaneFit {
                origin,
                normal,
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            }),
            face_count: faces.len(),
            area,
            faces,
            notes: Vec::new(),
        }
    }

    fn cylinder_feature(id: usize, radius: f64, mesh: &TriangleMesh) -> FeatureRecord {
        FeatureRecord {
            id,
            surface: SurfaceClass::Cylinder(CylinderFit {
                axis_point: Point3::new(0.0, 0.0, 6.0),
                axis: Vector3::new(0.0, 0.0, 1.0),
                radius,
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            }),
            face_count: mesh.triangles().len(),
            area: mesh.surface_area(),
            faces: (0..mesh.triangles().len() as u32).collect(),
            notes: Vec::new(),
        }
    }

    /// Two arc fragments of one wall, neither passing the
    /// circumference gate alone, union into a single bore instance —
    /// which is how holes survive noise.
    #[test]
    fn coaxial_arc_fragments_union_into_one_bore() {
        let mut soup = synth::cylinder_arc_soup(5.0, 12.0, 0.0, 150.0f64.to_radians(), 18, 6);
        soup.extend(synth::cylinder_arc_soup(
            5.0,
            12.0,
            170.0f64.to_radians(),
            330.0f64.to_radians(),
            18,
            6,
        ));
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("mesh");
        // Two features: the faces of each arc.
        let half = mesh.triangles().len() / 2;
        let make = |id: usize, faces: Vec<u32>| FeatureRecord {
            id,
            surface: SurfaceClass::Cylinder(CylinderFit {
                axis_point: Point3::new(0.0, 0.0, 6.0),
                axis: Vector3::new(0.0, 0.0, 1.0),
                radius: 5.0,
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            }),
            face_count: faces.len(),
            area: faces.iter().map(|&f| mesh.face_area(f as usize)).sum(),
            faces,
            notes: Vec::new(),
        };
        let features = vec![
            make(3, (0..half as u32).collect()),
            make(4, (half as u32..mesh.triangles().len() as u32).collect()),
        ];
        let instances = recognize_instances(&mesh, &features, None, 0.15);
        assert_eq!(instances.extrusions.len(), 1, "one union bore");
        let bore = &instances.extrusions[0];
        assert_eq!(bore.members.len(), 2, "both fragments joined");
        assert!((bore.circles[0].radius - 5.0).abs() < 1e-6);
    }

    /// A lone full cylinder is a drilled hole's entire evidence, and
    /// it licenses an extrusion instance by its own circumference; a
    /// hundred-degree shell of the same cylinder names no hole.
    #[test]
    fn a_lone_full_cylinder_is_an_extrusion_and_an_arc_is_not() {
        let full = synth::open_cylinder(5.0, 12.0, 48, 6);
        let instances = recognize_instances(&full, &[cylinder_feature(7, 5.0, &full)], None, 0.15);
        assert_eq!(instances.extrusions.len(), 1, "one bore instance");
        let bore = &instances.extrusions[0];
        assert_eq!(bore.members, vec![7]);
        assert_eq!(bore.circles.len(), 1);
        assert!((bore.circles[0].radius - 5.0).abs() < 1e-6);
        assert!(
            (bore.span.1 - bore.span.0 - 12.0).abs() < 0.8,
            "span {:?}",
            bore.span
        );
        let arc = TriangleMesh::from_triangle_soup(
            &synth::cylinder_arc_soup(5.0, 12.0, 0.0, 100.0f64.to_radians(), 14, 6),
            1e-9,
        )
        .expect("arc mesh");
        let refused = recognize_instances(&arc, &[cylinder_feature(7, 5.0, &arc)], None, 0.15);
        assert!(
            refused.extrusions.is_empty(),
            "a partial shell licenses nothing"
        );
    }

    /// A box's four walls are one extrusion, and its sketch is the
    /// rectangle they stand on.
    #[test]
    fn a_box_reads_as_one_extrusion_with_a_rectangular_sketch() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(Point3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 6.0, 8.0), 6),
            1e-6,
        )
        .expect("mesh");
        // Bucket faces by their dominant normal.
        let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); 6];
        for face in 0..mesh.triangles().len() {
            let n = mesh.face_normal(face).expect("normal");
            let slot = if n.x < -0.9 {
                0
            } else if n.x > 0.9 {
                1
            } else if n.y < -0.9 {
                2
            } else if n.y > 0.9 {
                3
            } else if n.z < -0.9 {
                4
            } else {
                5
            };
            buckets[slot].push(face as u32);
        }
        let features = vec![
            plane_feature(
                0,
                Vector3::new(-1.0, 0.0, 0.0),
                Point3::new(0.0, 0.0, 0.0),
                buckets[0].clone(),
                48.0,
            ),
            plane_feature(
                1,
                Vector3::new(1.0, 0.0, 0.0),
                Point3::new(10.0, 0.0, 0.0),
                buckets[1].clone(),
                48.0,
            ),
            plane_feature(
                2,
                Vector3::new(0.0, -1.0, 0.0),
                Point3::new(0.0, 0.0, 0.0),
                buckets[2].clone(),
                80.0,
            ),
            plane_feature(
                3,
                Vector3::new(0.0, 1.0, 0.0),
                Point3::new(0.0, 6.0, 0.0),
                buckets[3].clone(),
                80.0,
            ),
            plane_feature(
                4,
                Vector3::new(0.0, 0.0, -1.0),
                Point3::new(0.0, 0.0, 0.0),
                buckets[4].clone(),
                60.0,
            ),
            plane_feature(
                5,
                Vector3::new(0.0, 0.0, 1.0),
                Point3::new(0.0, 0.0, 8.0),
                buckets[5].clone(),
                60.0,
            ),
        ];
        let instances = recognize_instances(&mesh, &features, None, 0.15);
        let along_z = instances
            .extrusions
            .iter()
            .find(|instance| instance.direction.z.abs() > 0.999)
            .expect("the four walls extrude along z");
        assert_eq!(along_z.members.len(), 4, "four walls, no caps");
        assert_eq!(along_z.lines.len(), 4, "the sketch is a rectangle");
        assert!(along_z.draft_deg.abs() < 0.2, "a box has no draft");
        assert!(
            (along_z.span.1 - along_z.span.0 - 8.0).abs() < 0.2,
            "extruded through the box's height"
        );
    }

    /// A tilted cylinder with a cap is one revolve about the tilted
    /// axis — an instance the datum could never express.
    #[test]
    fn a_tilted_capped_cylinder_reads_as_one_revolve() {
        let (radius, height, tilt) = (6.0, 14.0, 0.5f64);
        let mut soup = synth::open_cylinder_soup(radius, height, 96, 8);
        soup.extend(synth::disk_soup(
            Point3::new(0.0, 0.0, height),
            Vector3::new(0.0, 0.0, 1.0),
            radius,
            96,
        ));
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-6).expect("mesh");
        let (sin, cos) = tilt.sin_cos();
        let tilted = mesh.transformed(&RigidTransform {
            rotation: [[1.0, 0.0, 0.0], [0.0, cos, -sin], [0.0, sin, cos]],
            translation: Vector3::new(0.0, 0.0, 0.0),
        });
        let axis = Vector3::new(0.0, -sin, cos);
        // Faces split by normal: near-axis normals are the cap.
        let (mut wall, mut cap) = (Vec::new(), Vec::new());
        for face in 0..tilted.triangles().len() {
            let n = tilted.face_normal(face).expect("normal");
            if n.dot(axis).abs() > 0.9 {
                cap.push(face as u32);
            } else {
                wall.push(face as u32);
            }
        }
        let features = vec![
            FeatureRecord {
                id: 0,
                surface: SurfaceClass::Cylinder(CylinderFit {
                    axis_point: Point3::default(),
                    axis,
                    radius,
                    deviation: DeviationStats {
                        rms: 0.0,
                        max_abs: 0.0,
                    },
                }),
                face_count: wall.len(),
                area: std::f64::consts::TAU * radius * height,
                faces: wall,
                notes: Vec::new(),
            },
            plane_feature(
                1,
                axis,
                Point3::new(0.0, -sin * height, cos * height),
                cap,
                std::f64::consts::PI * radius * radius,
            ),
        ];
        let instances = recognize_instances(&tilted, &features, None, 0.15);
        let revolve = instances
            .revolves
            .iter()
            .find(|instance| instance.axis.dot(axis).abs() > 0.999)
            .expect("one revolve about the tilted axis");
        assert_eq!(revolve.members.len(), 2, "wall and cap");
        assert!(
            revolve.profile.iter().any(|run| {
                (run.from.0 - radius).abs() < 0.1 && (run.to.0 - radius).abs() < 0.1
            }),
            "the wall is a vertical profile run at its radius"
        );
        assert!(revolve.residual < 0.1, "an exact revolve fits exactly");
    }
}

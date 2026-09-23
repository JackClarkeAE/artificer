//! B-spline patches for the surface the analytic vocabulary cannot carry.
//!
//! Runs last among the recognition stages, over what they left, and
//! answers one question per region: is there a single smooth patch here
//! that describes the material better than anything else the model has?
//!
//! **Which regions.** Freeform features, first: organic sheets and the
//! residue. But measuring the freeform test block showed that is not
//! where a designed freeform surface ends up. RANSAC carves it into
//! plane, sphere and cone facets, each within tolerance — a gently
//! curved surface is flat to seven noise sigmas over a hand's width —
//! so almost none of it stays freeform, and the facets together miss
//! the true surface by a millimetre. So an analytic feature is also a
//! candidate when its fit is **strained**: its robust residual scale
//! (1.4826 times the median distance of its own faces from its surface)
//! stands well above the scan's noise floor. A plane that describes a
//! flat face leaves noise behind; one laid across a curved surface
//! leaves a bowl, and the bowl is the evidence. The median makes the
//! test blind to the few strays every feature collects at its border,
//! so an honest plane that claimed a rounded edge stays an honest plane.
//!
//! Candidate faces group into regions over mesh adjacency, crossing from
//! one feature into another only where the two meet without a crease —
//! a facet boundary on a smooth surface, not the edge between a top and
//! a wall.
//!
//! **Analytic first, always.** Each region is offered to the pipeline's
//! own classifier before any spline is fitted; if a plane, cylinder,
//! sphere, cone or torus describes the whole region within tolerance,
//! the region is left to the analytic stages and no patch is made. A
//! patch that would absorb facets must also fit the material markedly
//! better than the facets did. Only then does the region become one
//! freeform feature carrying the patch.
//!
//! **Bounded by its material.** A patch spans its chart's rectangle; the
//! face is where the scan's region stops. The region's boundary loops
//! are projected onto the patch and simplified in its parameters. Inner
//! loops that are scanner dropout — rims with no mesh on the far side —
//! are bridged, since the part has material there that the scanner did
//! not see; inner loops that border another feature stay holes, since
//! that feature carries the surface there.

use artificer_geometry::{Point3, Vector3};

use crate::bspline::{SplineFit, SplineFitOptions, fit_surface};
use crate::datum::DatumAlignment;
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::{Region, SegmentationParams, SurfaceClass, classify_region};
use crate::transform::RigidTransform;

/// Knobs for the spline stage.
#[derive(Clone, Copy, Debug)]
pub struct SplineOptions {
    pub fit: SplineFitOptions,
    /// Regions smaller than this (mm^2) get no patch: the same
    /// significance line the analytic features are held to.
    pub min_area: f64,
    /// Whether analytic facets with a strained fit may be reclaimed into
    /// a patch. Off, only freeform features are considered.
    pub reclaim_facets: bool,
}

impl Default for SplineOptions {
    fn default() -> Self {
        Self {
            fit: SplineFitOptions::default(),
            min_area: 25.0,
            reclaim_facets: true,
        }
    }
}

/// A facet is strained when its robust residual scale reaches this many
/// noise sigmas...
///
/// The scale is read at face centroids, where a vertex's noise has been
/// averaged three ways, so an honest plane reads well under one sigma;
/// on the freeform block the walls read 0.8 to 1.0 sigma and the facets
/// of the top 1.4 to 9. Between them the line is drawn at 1.5, and the
/// tolerance term below usually sits above it anyway.
const STRAIN_NOISE: f64 = 1.5;
/// ...and this share of the tolerance, so a quiet synthetic, whose noise
/// reads as zero, does not call every tessellation chord a strain.
const STRAIN_TOLERANCE: f64 = 0.25;
/// Neighbouring faces of two different features belong to one smooth
/// surface when their normals agree this closely (degrees).
const SMOOTH_LINK_DEG: f64 = 25.0;
/// A patch that absorbs facets must fit their material at least this
/// many times better than they did.
const RECLAIM_MARGIN: f64 = 1.5;
/// Faces probed per feature for its residual scale.
const STRAIN_SAMPLES: usize = 600;
/// An inner loop whose edges are this share open mesh boundary is
/// scanner dropout, not another feature.
const DROPOUT_SHARE: f64 = 0.9;
/// Parameter-space step (mm) of the tessellation the rebuild draws.
pub const TESSELLATION_STEP: f64 = 1.2;

/// A trim rasterized over its patch's parameter rectangle, so asking
/// whether a parameter pair lies on the face is a lookup rather than a
/// walk round every loop — the rebuild asks once per occupancy cell, and
/// a cast sheet's trim can run to thousands of vertices.
#[derive(Clone, Debug)]
pub struct TrimMask {
    origin: (f64, f64),
    cell: f64,
    columns: usize,
    rows: usize,
    inside: Vec<bool>,
}

impl TrimMask {
    /// At most this many cells along the longer side.
    const RESOLUTION: f64 = 1024.0;

    /// Even-odd scanline fill of the loops over `domain`.
    pub fn of(loops: &[Vec<(f64, f64)>], domain: ((f64, f64), (f64, f64))) -> Self {
        let ((u0, u1), (v0, v1)) = domain;
        let cell = ((u1 - u0).max(v1 - v0) / Self::RESOLUTION).max(1e-3);
        let columns = ((u1 - u0) / cell).ceil().max(1.0) as usize;
        let rows = ((v1 - v0) / cell).ceil().max(1.0) as usize;
        let mut inside = vec![false; columns * rows];
        let mut crossings: Vec<f64> = Vec::new();
        for row in 0..rows {
            let v = v0 + (row as f64 + 0.5) * cell;
            crossings.clear();
            for ring in loops {
                for index in 0..ring.len() {
                    let (a, b) = (ring[index], ring[(index + 1) % ring.len()]);
                    if (a.1 > v) != (b.1 > v) {
                        crossings.push(a.0 + (v - a.1) / (b.1 - a.1) * (b.0 - a.0));
                    }
                }
            }
            crossings.sort_by(f64::total_cmp);
            for pair in crossings.chunks_exact(2) {
                let first = ((pair[0] - u0) / cell - 0.5).ceil().max(0.0) as usize;
                let last = ((pair[1] - u0) / cell - 0.5).floor();
                if last < 0.0 {
                    continue;
                }
                let last = (last as usize).min(columns - 1);
                for column in first..=last {
                    inside[row * columns + column] = true;
                }
            }
        }
        Self {
            origin: (u0, v0),
            cell,
            columns,
            rows,
            inside,
        }
    }

    pub fn contains(&self, (u, v): (f64, f64)) -> bool {
        let column = ((u - self.origin.0) / self.cell).floor();
        let row = ((v - self.origin.1) / self.cell).floor();
        if column < 0.0 || row < 0.0 {
            return false;
        }
        let (column, row) = (column as usize, row as usize);
        column < self.columns && row < self.rows && self.inside[row * self.columns + column]
    }
}

/// One B-spline patch, bounded by where its region's material stops.
#[derive(Clone, Debug)]
pub struct SplinePatch {
    /// The freeform feature (report id) whose faces the patch describes.
    pub feature: usize,
    pub area: f64,
    pub fit: SplineFit,
    /// Trim loops in the surface's parameters: the outer boundary first,
    /// anticlockwise, then any holes, clockwise.
    pub loops: Vec<Vec<(f64, f64)>>,
    /// Whether `S_u x S_v` points out of the material, the side the
    /// scan's own normals face.
    pub outward: bool,
    /// Analytic facets the patch absorbed: how many, their area, and
    /// their area-weighted RMS against their own faces.
    pub reclaimed: usize,
    pub reclaimed_area: f64,
    pub reclaimed_rms: f64,
    /// Inner loops that were scanner dropout, bridged by the patch.
    pub bridged_holes: usize,
    /// Inner loops too small to trim — a face or two of another feature
    /// inside the patch — closed over.
    pub pinholes: usize,
    /// The trim, rasterized for fast inside tests.
    pub mask: TrimMask,
}

impl SplinePatch {
    /// A patch trimmed by `loops` (outer first, anticlockwise; holes
    /// clockwise), with nothing reclaimed or bridged yet.
    pub fn new(
        feature: usize,
        area: f64,
        fit: SplineFit,
        loops: Vec<Vec<(f64, f64)>>,
        outward: bool,
    ) -> Self {
        let mask = TrimMask::of(&loops, fit.surface.domain());
        Self {
            feature,
            area,
            fit,
            loops,
            outward,
            reclaimed: 0,
            reclaimed_area: 0.0,
            reclaimed_rms: 0.0,
            bridged_holes: 0,
            pinholes: 0,
            mask,
        }
    }

    pub fn describe(&self) -> String {
        let (nu, nv) = self.fit.surface.net();
        let mut line = format!(
            "B-spline {}x{} (degree {}) on a {}, rms {:.4} max {:.3} over {} samples",
            nu,
            nv,
            self.fit.surface.degree_u,
            self.fit.chart.describe(),
            self.fit.deviation.rms,
            self.fit.deviation.max_abs,
            self.fit.samples
        );
        if self.fit.outliers > 0 {
            line.push_str(&format!(
                " ({} trimmed as outliers, inliers rms {:.4})",
                self.fit.outliers, self.fit.inlier_rms
            ));
        }
        line.push_str(&format!(
            "; {} refinement round(s), {} parameter correction(s)",
            self.fit.rounds, self.fit.corrections
        ));
        if self.reclaimed > 0 {
            line.push_str(&format!(
                "; replaces {} strained facet(s) over {:.0} mm^2 that fitted at rms {:.4}",
                self.reclaimed, self.reclaimed_area, self.reclaimed_rms
            ));
        }
        let holes = self.loops.len().saturating_sub(1);
        if holes > 0 {
            line.push_str(&format!("; {holes} hole(s) trimmed round other features"));
        }
        if self.bridged_holes > 0 {
            line.push_str(&format!(
                "; {} scanner dropout(s) bridged",
                self.bridged_holes
            ));
        }
        if self.pinholes > 0 {
            line.push_str(&format!("; {} pinhole(s) closed", self.pinholes));
        }
        line
    }

    /// A box holding the whole patch: a B-spline surface lies inside the
    /// convex hull of its control net.
    pub fn bounds(&self) -> (Point3, Point3) {
        let mut low = Point3::new(f64::MAX, f64::MAX, f64::MAX);
        let mut high = Point3::new(f64::MIN, f64::MIN, f64::MIN);
        for p in &self.fit.surface.control {
            low = Point3::new(low.x.min(p.x), low.y.min(p.y), low.z.min(p.z));
            high = Point3::new(high.x.max(p.x), high.y.max(p.y), high.z.max(p.z));
        }
        (low, high)
    }

    /// The signed distance from the trimmed patch and its unit normal
    /// there, when the point projects inside the trim; `None` when it
    /// lands outside, where the face does not exist.
    pub fn trimmed_distance(&self, point: Point3) -> Option<(f64, Vector3)> {
        let landed = self.fit.surface.project(point, self.fit.chart.map(point));
        if !self.mask.contains((landed.u, landed.v)) {
            return None;
        }
        let normal = self.fit.surface.normal(landed.u, landed.v)?;
        Some((landed.distance, normal))
    }

    /// The trimmed patch as triangles in the datum frame, facing out of
    /// the material: the trim polygon clipped into triangles in the
    /// parameter plane, split until no edge is longer than `step`, and
    /// every vertex evaluated on the surface.
    pub fn tessellate(&self, step: f64) -> Vec<[Point3; 3]> {
        let (vertices, triangles) =
            trim_triangulation(&self.loops).unwrap_or_else(|| self.cell_triangulation(step));
        let (vertices, triangles) = refine_parameter_mesh(vertices, triangles, step);
        let surface = &self.fit.surface;
        let points: Vec<Point3> = vertices
            .iter()
            .map(|&(u, v)| surface.evaluate(u, v))
            .collect();
        triangles
            .into_iter()
            .filter_map(|[a, b, c]| {
                let (pa, pb, pc) = (points[a], points[b], points[c]);
                if (pb - pa).cross(pc - pa).length() < 1e-12 {
                    return None;
                }
                // Anticlockwise in (u, v) faces along S_u x S_v.
                Some(if self.outward {
                    [pa, pb, pc]
                } else {
                    [pa, pc, pb]
                })
            })
            .collect()
    }

    /// Fallback when the trim polygon will not clip cleanly: square cells
    /// of the parameter plane whose centres fall inside the trim.
    fn cell_triangulation(&self, step: f64) -> ParameterMesh {
        let ((u0, u1), (v0, v1)) = self.fit.surface.domain();
        let (columns, rows) = (
            ((u1 - u0) / step).ceil().max(1.0) as usize,
            ((v1 - v0) / step).ceil().max(1.0) as usize,
        );
        let (du, dv) = ((u1 - u0) / columns as f64, (v1 - v0) / rows as f64);
        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        for i in 0..columns {
            for j in 0..rows {
                let centre = (u0 + (i as f64 + 0.5) * du, v0 + (j as f64 + 0.5) * dv);
                if !self.mask.contains(centre) {
                    continue;
                }
                let base = vertices.len();
                let (a, b) = (u0 + i as f64 * du, v0 + j as f64 * dv);
                vertices.extend([(a, b), (a + du, b), (a + du, b + dv), (a, b + dv)]);
                triangles.push([base, base + 1, base + 2]);
                triangles.push([base, base + 2, base + 3]);
            }
        }
        (vertices, triangles)
    }
}

/// A triangulation of part of the parameter plane: vertices `(u, v)`
/// and triangles indexing them, anticlockwise.
type ParameterMesh = (Vec<(f64, f64)>, Vec<[usize; 3]>);

/// Clips the trim polygon — outer loop anticlockwise, holes clockwise,
/// bridged into one ring — into triangles. `None` when the clip does not
/// cover the polygon's own area, which is how a self-touching trim shows.
fn trim_triangulation(loops: &[Vec<(f64, f64)>]) -> Option<ParameterMesh> {
    /// Ear clipping and hole bridging are cubic and quadratic in the
    /// ring; past this a trim goes to the raster instead.
    const MAX_VERTICES: usize = 1500;
    const MAX_LOOPS: usize = 64;
    let outer = loops.first()?;
    let vertices: usize = loops.iter().map(Vec::len).sum();
    if outer.len() < 3 || vertices > MAX_VERTICES || loops.len() > MAX_LOOPS {
        return None;
    }
    let mut vertices: Vec<(f64, f64)> = Vec::new();
    let mut ring_of = |ring: &[(f64, f64)]| -> Vec<(f64, f64, usize)> {
        ring.iter()
            .map(|&(u, v)| {
                vertices.push((u, v));
                (u, v, vertices.len() - 1)
            })
            .collect()
    };
    let outer_ring = ring_of(outer);
    let holes: Vec<Vec<(f64, f64, usize)>> = loops[1..].iter().map(|ring| ring_of(ring)).collect();
    let expected: f64 = crate::step::signed_area(outer)
        + loops[1..]
            .iter()
            .map(|ring| crate::step::signed_area(ring))
            .sum::<f64>();
    let merged = crate::step::bridge_holes(outer_ring, holes);
    let polygon: Vec<(f64, f64)> = merged.iter().map(|&(u, v, _)| (u, v)).collect();
    let clipped = crate::step::ear_clip(&polygon);
    let covered: f64 = clipped
        .iter()
        .map(|&[a, b, c]| crate::step::signed_area(&[polygon[a], polygon[b], polygon[c]]))
        .sum();
    if expected <= 0.0 || (covered - expected).abs() > 0.01 * expected {
        return None;
    }
    let triangles = clipped
        .into_iter()
        .map(|[a, b, c]| [merged[a].2, merged[b].2, merged[c].2])
        .collect();
    Some((vertices, triangles))
}

/// Splits parameter-plane triangles until no edge is longer than `step`,
/// sharing each split midpoint between the two triangles on its edge so
/// the mesh stays conforming.
fn refine_parameter_mesh(
    mut vertices: Vec<(f64, f64)>,
    mut triangles: Vec<[usize; 3]>,
    step: f64,
) -> ParameterMesh {
    const PASSES: usize = 16;
    let limit = step * step;
    for _ in 0..PASSES {
        let mut midpoint: std::collections::BTreeMap<(usize, usize), usize> =
            std::collections::BTreeMap::new();
        let mut split = |a: usize, b: usize, vertices: &mut Vec<(f64, f64)>| -> Option<usize> {
            let (p, q) = (vertices[a], vertices[b]);
            if (q.0 - p.0).powi(2) + (q.1 - p.1).powi(2) <= limit {
                return None;
            }
            Some(*midpoint.entry((a.min(b), a.max(b))).or_insert_with(|| {
                vertices.push(((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0));
                vertices.len() - 1
            }))
        };
        let mut next = Vec::with_capacity(triangles.len() * 2);
        let mut any = false;
        for &[a, b, c] in &triangles {
            let (ab, bc, ca) = (
                split(a, b, &mut vertices),
                split(b, c, &mut vertices),
                split(c, a, &mut vertices),
            );
            any |= ab.is_some() || bc.is_some() || ca.is_some();
            match (ab, bc, ca) {
                (None, None, None) => next.push([a, b, c]),
                (Some(m), None, None) => next.extend([[a, m, c], [m, b, c]]),
                (None, Some(m), None) => next.extend([[b, m, a], [m, c, a]]),
                (None, None, Some(m)) => next.extend([[c, m, b], [m, a, b]]),
                (Some(x), Some(y), None) => next.extend([[x, b, y], [a, x, y], [a, y, c]]),
                (None, Some(x), Some(y)) => next.extend([[x, c, y], [b, x, y], [b, y, a]]),
                (Some(x), None, Some(y)) => next.extend([[a, x, y], [x, b, c], [y, x, c]]),
                (Some(x), Some(y), Some(z)) => {
                    next.extend([[a, x, z], [x, b, y], [z, y, c], [x, y, z]]);
                }
            }
        }
        triangles = next;
        if !any {
            break;
        }
    }
    (vertices, triangles)
}

/// Douglas–Peucker over a closed ring, keeping its two most distant
/// vertices as anchors.
fn simplify_ring(ring: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if ring.len() <= 4 {
        return ring.to_vec();
    }
    let first = 0usize;
    let far = (1..ring.len())
        .max_by(|&a, &b| {
            let da = (ring[a].0 - ring[first].0).powi(2) + (ring[a].1 - ring[first].1).powi(2);
            let db = (ring[b].0 - ring[first].0).powi(2) + (ring[b].1 - ring[first].1).powi(2);
            da.total_cmp(&db)
        })
        .unwrap_or(1);
    let mut closed: Vec<(f64, f64)> = ring.to_vec();
    closed.push(ring[0]);
    let mut keep = vec![false; closed.len()];
    keep[first] = true;
    keep[far] = true;
    // Spans still to examine, as index pairs into the closed ring; a
    // stack rather than recursion, since a long ragged rim can nest
    // deeply.
    let mut spans = vec![(first, far), (far, ring.len())];
    while let Some((start, end)) = spans.pop() {
        if end <= start + 1 {
            continue;
        }
        let (a, b) = (closed[start], closed[end]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy).max(1e-300);
        let (index, distance) = (start + 1..end)
            .map(|i| {
                let p = closed[i];
                (i, ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / length)
            })
            .max_by(|x, y| x.1.total_cmp(&y.1).then(y.0.cmp(&x.0)))
            .unwrap_or((start, 0.0));
        if distance > epsilon {
            keep[index] = true;
            spans.push((start, index));
            spans.push((index, end));
        }
    }
    ring.iter()
        .enumerate()
        .filter(|(index, _)| keep[*index])
        .map(|(_, &p)| p)
        .collect()
}

/// The robust residual scale of an analytic feature against its own
/// faces: the RMS of the nearest 85% of them, scaled so Gaussian noise
/// reads as its own sigma.
///
/// Trimmed, so the strays every feature collects at its border — a
/// claimed sliver of rounded edge — cannot make an honest plane look
/// strained. Not a median: a facet laid across a saddle leaves residuals
/// crowded near zero along the saddle's asymptotes, and a median reads
/// that crowd while the bowl either side of it is the evidence.
fn strain_of(
    mesh: &TriangleMesh,
    feature: &FeatureRecord,
    to_frame: &RigidTransform,
) -> Option<f64> {
    /// The share of faces kept, and the factor that makes the trimmed
    /// RMS of a Gaussian equal its sigma: `E[z^2 | |z| < q]` for the
    /// two-sided 85% quantile `q = 1.4395` is 0.5207 of the variance.
    const KEPT: f64 = 0.85;
    const CONSISTENCY: f64 = 0.7216;
    if feature.faces.is_empty() {
        return None;
    }
    let stride = (feature.faces.len() / STRAIN_SAMPLES).max(1);
    let mut distances: Vec<f64> = feature
        .faces
        .iter()
        .step_by(stride)
        .filter_map(|&face| {
            let centroid = to_frame.apply_point(mesh.face_centroid(face as usize));
            feature.surface.probe(centroid).map(|(d, _)| d.abs())
        })
        .collect();
    if distances.is_empty() {
        return None;
    }
    distances.sort_by(f64::total_cmp);
    let kept = ((distances.len() as f64 * KEPT).ceil() as usize).clamp(1, distances.len());
    let squared: f64 = distances[..kept].iter().map(|d| d * d).sum();
    Some((squared / kept as f64).sqrt() / CONSISTENCY)
}

/// How a candidate feature came to be one.
#[derive(Clone, Copy)]
enum Candidate {
    Freeform,
    /// A strained analytic fit, with its reported RMS.
    Strained {
        rms: f64,
    },
}

/// The boundary loops of a face set, as vertex rings in the faces' own
/// winding, each edge flagged when no mesh face lies on its far side.
fn boundary_loops(
    mesh: &TriangleMesh,
    faces: &[u32],
    adjacency: &[Vec<u32>],
) -> Vec<(Vec<u32>, Vec<bool>)> {
    let mut count: std::collections::BTreeMap<(u32, u32), u32> = std::collections::BTreeMap::new();
    for &face in faces {
        let [a, b, c] = mesh.triangles()[face as usize];
        for (u, v) in [(a, b), (b, c), (c, a)] {
            *count.entry((u.min(v), u.max(v))).or_default() += 1;
        }
    }
    // Directed boundary half-edges, keyed by their start vertex.
    let mut outgoing: std::collections::BTreeMap<u32, Vec<(u32, bool)>> =
        std::collections::BTreeMap::new();
    for &face in faces {
        let [a, b, c] = mesh.triangles()[face as usize];
        for (u, v) in [(a, b), (b, c), (c, a)] {
            if count[&(u.min(v), u.max(v))] != 1 {
                continue;
            }
            let open = !adjacency[face as usize].iter().any(|&other| {
                let tri = mesh.triangles()[other as usize];
                tri.contains(&u) && tri.contains(&v)
            });
            outgoing.entry(u).or_default().push((v, open));
        }
    }
    for edges in outgoing.values_mut() {
        edges.sort_unstable_by_key(|&(to, _)| to);
    }
    let mut loops = Vec::new();
    while let Some((&start, _)) = outgoing.iter().find(|(_, edges)| !edges.is_empty()) {
        let mut ring = vec![start];
        let mut flags = Vec::new();
        let mut at = start;
        while let Some(edges) = outgoing.get_mut(&at) {
            if edges.is_empty() {
                break;
            }
            let (to, open) = edges.remove(0);
            flags.push(open);
            if to == start {
                break;
            }
            ring.push(to);
            at = to;
        }
        if ring.len() >= 3 {
            loops.push((ring, flags));
        }
    }
    loops
}

/// What the stage did, for the report.
#[derive(Default)]
pub struct SplineOutcome {
    pub patches: Vec<SplinePatch>,
    /// One line per region that was offered a patch and did not get one.
    pub refusals: Vec<String>,
    /// Regions under the significance line, left measured.
    pub small_regions: usize,
    pub small_area: f64,
}

/// Fits B-spline patches to the freeform regions of a finished feature
/// list, reclaiming strained analytic facets where one smooth patch
/// describes them better, and rewrites the list so each patch owns one
/// freeform feature. Features are re-sorted by area and renumbered, the
/// way every other stage that rewrites the list leaves it.
pub fn fit_freeform_patches(
    mesh: &TriangleMesh,
    features: &mut Vec<FeatureRecord>,
    alignment: Option<&DatumAlignment>,
    tolerance: f64,
    noise_sigma: f64,
    options: &SplineOptions,
) -> SplineOutcome {
    let identity = RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let mut outcome = SplineOutcome::default();
    let face_count = mesh.triangles().len();
    let mut owner = vec![u32::MAX; face_count];
    for (index, feature) in features.iter().enumerate() {
        for &face in &feature.faces {
            owner[face as usize] = index as u32;
        }
    }
    let strain_floor = (STRAIN_NOISE * noise_sigma).max(STRAIN_TOLERANCE * tolerance);
    let candidates: Vec<Option<Candidate>> = features
        .iter()
        .map(|feature| match &feature.surface {
            SurfaceClass::Freeform => Some(Candidate::Freeform),
            SurfaceClass::Plane(_)
            | SurfaceClass::Cylinder(_)
            | SurfaceClass::Sphere(_)
            | SurfaceClass::Cone(_)
            | SurfaceClass::Torus(_)
                if options.reclaim_facets =>
            {
                let scale = strain_of(mesh, feature, to_frame)?;
                if std::env::var_os("ARTIFICER_SPLINE_DEBUG").is_some() {
                    eprintln!(
                        "spline-debug: #{} {} area {:.1} rms {:.4} strain {scale:.4} floor {strain_floor:.4}",
                        feature.id,
                        feature.surface.kind(),
                        feature.area,
                        feature.surface.rms().unwrap_or(0.0)
                    );
                }
                (scale >= strain_floor).then(|| Candidate::Strained {
                    rms: feature.surface.rms().unwrap_or(scale),
                })
            }
            _ => None,
        })
        .collect();
    if candidates.iter().all(Option::is_none) {
        return outcome;
    }
    let adjacency = mesh.face_adjacency();
    let is_candidate =
        |face: usize| owner[face] != u32::MAX && candidates[owner[face] as usize].is_some();
    let smooth = SMOOTH_LINK_DEG.to_radians().cos();
    // Regions: candidate faces joined over adjacency, across a feature
    // boundary only where the surface runs on without a crease.
    let mut region_of = vec![usize::MAX; face_count];
    let mut regions: Vec<Vec<u32>> = Vec::new();
    for seed in 0..face_count {
        if region_of[seed] != usize::MAX || !is_candidate(seed) {
            continue;
        }
        let index = regions.len();
        region_of[seed] = index;
        let mut members = vec![seed as u32];
        let mut queue = std::collections::VecDeque::from([seed]);
        while let Some(face) = queue.pop_front() {
            let normal = mesh.face_normal(face);
            for &next in &adjacency[face] {
                let next = next as usize;
                if region_of[next] != usize::MAX || !is_candidate(next) {
                    continue;
                }
                if owner[next] != owner[face] {
                    let agree = match (normal, mesh.face_normal(next)) {
                        (Some(a), Some(b)) => a.dot(b) >= smooth,
                        _ => false,
                    };
                    if !agree {
                        continue;
                    }
                }
                region_of[next] = index;
                members.push(next as u32);
                queue.push_back(next);
            }
        }
        regions.push(members);
    }
    // Fit each significant region, analytic surfaces first.
    struct Accepted {
        faces: Vec<u32>,
        area: f64,
        fit: SplineFit,
        /// The candidate features the region drew on.
        members: Vec<u32>,
        reclaimed: usize,
        reclaimed_area: f64,
        reclaimed_rms: f64,
    }
    let mut accepted: Vec<Accepted> = Vec::new();
    for faces in regions {
        let area: f64 = faces.iter().map(|&f| mesh.face_area(f as usize)).sum();
        // Which features the region draws on, in id order.
        let mut drawn: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
        for &face in &faces {
            *drawn.entry(owner[face as usize]).or_default() += mesh.face_area(face as usize);
        }
        let strained: Vec<(u32, f64, f64)> = drawn
            .iter()
            .filter_map(|(&index, &share)| match candidates[index as usize] {
                Some(Candidate::Strained { rms }) => Some((index, share, rms)),
                _ => None,
            })
            .collect();
        if area < options.min_area {
            outcome.small_regions += 1;
            outcome.small_area += area;
            continue;
        }
        let label = {
            let ids: Vec<String> = drawn
                .keys()
                .take(6)
                .map(|&index| format!("#{}", features[index as usize].id))
                .collect();
            let more = drawn.len().saturating_sub(6);
            format!(
                "{area:.0} mm^2 over {}{}",
                ids.join(" "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            )
        };
        let region = Region {
            faces: faces.clone(),
            area,
        };
        let analytic = classify_region(mesh, &region, tolerance, &SegmentationParams::default());
        if let Some(rms) = analytic.rms() {
            outcome.refusals.push(format!(
                "{label}: a {} fits it at rms {rms:.4}, so it stays with the analytic surfaces",
                analytic.kind()
            ));
            continue;
        }
        // Samples in the datum frame, every vertex once, in face order.
        let mut seen = vec![false; mesh.positions().len()];
        let mut points: Vec<Point3> = Vec::new();
        let mut samples: Vec<(Point3, Vector3, f64)> = Vec::with_capacity(faces.len());
        for &face in &faces {
            for vertex in mesh.triangles()[face as usize] {
                if !seen[vertex as usize] {
                    seen[vertex as usize] = true;
                    points.push(to_frame.apply_point(mesh.positions()[vertex as usize]));
                }
            }
            if let Some(normal) = mesh.face_normal(face as usize) {
                samples.push((
                    to_frame.apply_point(mesh.face_centroid(face as usize)),
                    to_frame.apply_vector(normal),
                    mesh.face_area(face as usize),
                ));
            }
        }
        let fit = match fit_surface(&points, &samples, tolerance, noise_sigma, &options.fit) {
            Ok(fit) => fit,
            Err(refusal) => {
                outcome.refusals.push(format!("{label}: {refusal}"));
                continue;
            }
        };
        let (mut reclaimed_area, mut squared) = (0.0f64, 0.0f64);
        for &(_, share, rms) in &strained {
            reclaimed_area += share;
            squared += share * rms * rms;
        }
        let reclaimed_rms = (squared / reclaimed_area.max(1e-12)).sqrt();
        if !strained.is_empty() && fit.deviation.rms * RECLAIM_MARGIN > reclaimed_rms {
            outcome.refusals.push(format!(
                "{label}: its {} facet(s) fit at rms {reclaimed_rms:.4} and a patch only reaches \
                 {:.4}, not enough better to replace them",
                strained.len(),
                fit.deviation.rms
            ));
            continue;
        }
        accepted.push(Accepted {
            faces,
            area,
            fit,
            members: drawn.keys().copied().collect(),
            reclaimed: strained.len(),
            reclaimed_area,
            reclaimed_rms,
        });
    }
    if accepted.is_empty() {
        return outcome;
    }
    // What the members keep once a patch has taken its share is often a
    // scatter of faces the region's walk did not reach — a noisy face
    // whose normal disagreed, a sliver between two facets. A connected
    // piece of that remainder too small to stand as a feature, touching
    // a patch, goes with the patch it touches; a piece elsewhere on the
    // part (a residue record is scattered) stays where it is.
    //
    // A feature of any kind below the significance line that the patch
    // *encloses* goes with it too. Those are the micro-planes the residue
    // pass recovers after the significance filter has run — flat to
    // tolerance over a few square millimetres, and by the pipeline's own
    // rule transition geometry rather than design features. Left alone
    // each would punch a hole in the trim for nothing.
    const MEMBER: u8 = 1;
    const ENCLOSED_ONLY: u8 = 2;
    let mut patch_of = vec![usize::MAX; face_count];
    for (slot, patch) in accepted.iter().enumerate() {
        for &face in &patch.faces {
            patch_of[face as usize] = slot;
        }
    }
    let mut leftover = vec![0u8; face_count];
    for feature in features.iter() {
        if feature.area < options.min_area {
            for &face in &feature.faces {
                if patch_of[face as usize] == usize::MAX {
                    leftover[face as usize] = ENCLOSED_ONLY;
                }
            }
        }
    }
    for patch in &accepted {
        for &member in &patch.members {
            for &face in &features[member as usize].faces {
                if patch_of[face as usize] == usize::MAX {
                    leftover[face as usize] = MEMBER;
                }
            }
        }
    }
    let mut visited = vec![false; face_count];
    for seed in 0..face_count {
        if leftover[seed] == 0 || visited[seed] {
            continue;
        }
        visited[seed] = true;
        let mut piece = vec![seed as u32];
        let mut cursor = 0;
        let mut touches: Option<usize> = None;
        let (mut enclosed, mut members_only) = (true, true);
        while cursor < piece.len() {
            let face = piece[cursor] as usize;
            cursor += 1;
            members_only &= leftover[face] == MEMBER;
            for &next in &adjacency[face] {
                let next = next as usize;
                if patch_of[next] != usize::MAX {
                    let slot = touches.map_or(patch_of[next], |slot| slot.min(patch_of[next]));
                    enclosed &= touches.is_none_or(|known| known == patch_of[next]);
                    touches = Some(slot);
                } else if leftover[next] != 0 {
                    if !visited[next] {
                        visited[next] = true;
                        piece.push(next as u32);
                    }
                } else {
                    enclosed = false;
                }
            }
        }
        let area: f64 = piece.iter().map(|&f| mesh.face_area(f as usize)).sum();
        if let Some(slot) = touches
            && area < options.min_area
            && (members_only || enclosed)
        {
            for &face in &piece {
                patch_of[face as usize] = slot;
            }
            accepted[slot].area += area;
            accepted[slot].faces.extend(piece);
        }
    }
    // Rewrite the feature list: patch faces leave their old owners, each
    // patch becomes one freeform feature, emptied features go.
    let taken: Vec<bool> = patch_of.iter().map(|&slot| slot != usize::MAX).collect();
    for feature in features.iter_mut() {
        let before = feature.faces.len();
        feature.faces.retain(|&face| !taken[face as usize]);
        if feature.faces.len() != before && !feature.faces.is_empty() {
            feature.face_count = feature.faces.len();
            feature.area = feature
                .faces
                .iter()
                .map(|&face| mesh.face_area(face as usize))
                .sum();
            feature.notes.push(format!(
                "{} face(s) passed to a B-spline patch the surface runs on into",
                before - feature.faces.len()
            ));
        }
    }
    features.retain(|feature| !feature.faces.is_empty());
    let first_patch = features.len();
    for patch in &accepted {
        features.push(FeatureRecord {
            id: 0,
            surface: SurfaceClass::Freeform,
            face_count: patch.faces.len(),
            area: patch.area,
            faces: patch.faces.clone(),
            notes: Vec::new(),
        });
    }
    // Largest first, as everywhere else; the patches' places tracked.
    let mut order: Vec<usize> = (0..features.len()).collect();
    order.sort_by(|&a, &b| {
        features[b]
            .area
            .total_cmp(&features[a].area)
            .then(a.cmp(&b))
    });
    let mut position = vec![0usize; features.len()];
    for (new, &old) in order.iter().enumerate() {
        position[old] = new;
    }
    let mut slots: Vec<Option<FeatureRecord>> = features.drain(..).map(Some).collect();
    features.extend(
        order
            .iter()
            .map(|&old| slots[old].take().expect("each once")),
    );
    for (id, feature) in features.iter_mut().enumerate() {
        feature.id = id;
    }
    for (offset, patch) in accepted.into_iter().enumerate() {
        let id = position[first_patch + offset];
        let spline = bound_patch(mesh, &adjacency, to_frame, patch.faces, patch.fit, id);
        let spline = SplinePatch {
            area: patch.area,
            reclaimed: patch.reclaimed,
            reclaimed_area: patch.reclaimed_area,
            reclaimed_rms: patch.reclaimed_rms,
            ..spline
        };
        features[id].notes.push(spline.describe());
        outcome.patches.push(spline);
    }
    outcome.patches.sort_by_key(|patch| patch.feature);
    outcome
}

/// Trims a fitted patch to its region: boundary loops projected into
/// the surface's parameters and simplified, dropout holes bridged, and
/// the side the material faces read from the scan's own normals.
fn bound_patch(
    mesh: &TriangleMesh,
    adjacency: &[Vec<u32>],
    to_frame: &RigidTransform,
    faces: Vec<u32>,
    fit: SplineFit,
    feature: usize,
) -> SplinePatch {
    let surface = &fit.surface;
    let area: f64 = faces.iter().map(|&f| mesh.face_area(f as usize)).sum();
    let mut vertices = 0usize;
    {
        let mut seen = std::collections::BTreeSet::new();
        for &face in &faces {
            for vertex in mesh.triangles()[face as usize] {
                seen.insert(vertex);
            }
        }
        vertices += seen.len();
    }
    let spacing = (area / vertices.max(1) as f64).sqrt();
    let land = |vertex: u32| -> (f64, f64) {
        let point = to_frame.apply_point(mesh.positions()[vertex as usize]);
        let landed = surface.project(point, fit.chart.map(point));
        (landed.u, landed.v)
    };
    /// A boundary loop in the patch's parameters, with its signed area
    /// and the share of its edges that are open mesh boundary.
    struct Ring {
        points: Vec<(f64, f64)>,
        signed: f64,
        open: f64,
    }
    let mut rings: Vec<Ring> = Vec::new();
    for (ring, flags) in boundary_loops(mesh, &faces, adjacency) {
        let params: Vec<(f64, f64)> = ring.iter().map(|&vertex| land(vertex)).collect();
        let points = simplify_ring(&params, 0.5 * spacing);
        let signed = crate::step::signed_area(&points);
        let open = flags.iter().filter(|&&open| open).count() as f64 / flags.len().max(1) as f64;
        rings.push(Ring {
            points,
            signed,
            open,
        });
    }
    // The outer boundary encloses the most; the rest are holes.
    rings.sort_by(|a, b| b.signed.abs().total_cmp(&a.signed.abs()));
    let mut loops: Vec<Vec<(f64, f64)>> = Vec::new();
    let (mut bridged_holes, mut pinholes) = (0usize, 0usize);
    for (index, ring) in rings.into_iter().enumerate() {
        let Ring {
            points: mut ring,
            signed,
            open,
        } = ring;
        if index > 0 {
            // A rim the scanner never closed: the part has material
            // there, and so does the patch.
            if open >= DROPOUT_SHARE {
                bridged_holes += 1;
                continue;
            }
            // A face or two of something else, too small to trim around.
            if signed.abs() < (2.0 * spacing).powi(2) {
                pinholes += 1;
                continue;
            }
        }
        let anticlockwise = signed > 0.0;
        if (index == 0) != anticlockwise {
            ring.reverse();
        }
        loops.push(ring);
    }
    // Which way the material faces, by the scan's own normals.
    let stride = (faces.len() / 400).max(1);
    let mut agreement = 0.0f64;
    for &face in faces.iter().step_by(stride) {
        let Some(normal) = mesh.face_normal(face as usize) else {
            continue;
        };
        let centroid = to_frame.apply_point(mesh.face_centroid(face as usize));
        let landed = surface.project(centroid, fit.chart.map(centroid));
        if let Some(surface_normal) = surface.normal(landed.u, landed.v) {
            agreement +=
                mesh.face_area(face as usize) * to_frame.apply_vector(normal).dot(surface_normal);
        }
    }
    SplinePatch {
        bridged_holes,
        pinholes,
        ..SplinePatch::new(feature, area, fit, loops, agreement >= 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ReverseOptions, reverse_engineer};
    use crate::simulate::{SimulateOptions, simulate_scan};

    /// A plane laid over a height field, as the region pass would have
    /// fitted it, with deterministic noise of the given sigma.
    fn plane_over(height: impl Fn(f64, f64) -> f64, sigma: f64) -> (TriangleMesh, FeatureRecord) {
        let noise = |i: usize, j: usize| {
            let hash = ((i as f64 * 12.9898 + j as f64 * 78.233).sin() * 43758.5453).fract();
            // Uniform on [-1, 1] has sigma 1/sqrt(3).
            sigma * 3f64.sqrt() * (2.0 * hash.abs() - 1.0)
        };
        let at = |i: usize, j: usize| {
            let (x, y) = (-15.0 + 0.5 * i as f64, -15.0 + 0.5 * j as f64);
            Point3::new(x, y, height(x, y) + noise(i, j))
        };
        let mut soup = Vec::new();
        for i in 0..60 {
            for j in 0..60 {
                soup.push([at(i, j), at(i + 1, j), at(i + 1, j + 1)]);
                soup.push([at(i, j), at(i + 1, j + 1), at(i, j + 1)]);
            }
        }
        let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-9).expect("grid");
        let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
        // Fitted before the border was claimed, as the pipeline orders it.
        let interior: Vec<Point3> = mesh
            .positions()
            .iter()
            .copied()
            .filter(|p| p.x <= 14.0)
            .collect();
        let fit =
            crate::fit::fit_plane(&interior, Some(Vector3::new(0.0, 0.0, 1.0))).expect("plane");
        let feature = FeatureRecord {
            id: 0,
            surface: SurfaceClass::Plane(fit),
            face_count: faces.len(),
            area: mesh.surface_area(),
            faces,
            notes: Vec::new(),
        };
        (mesh, feature)
    }

    #[test]
    fn a_plane_across_a_curve_reads_strained_and_a_flat_one_does_not() {
        let identity = RigidTransform::IDENTITY;
        let sigma = 0.02;
        // Flat, with a rounded edge's worth of strays along one border:
        // an honest plane that claimed a sliver of the round.
        let (mesh, flat) = plane_over(|x, _| if x > 14.0 { 0.6 * (x - 14.0) } else { 0.0 }, sigma);
        let honest = strain_of(&mesh, &flat, &identity).expect("probes");
        // Across a gentle saddle, sagging no more than the noise allows
        // a plane to pass a 0.12 mm tolerance.
        let (mesh, facet) = plane_over(|x, y| (x * x - y * y) / 2500.0, sigma);
        let strained = strain_of(&mesh, &facet, &identity).expect("probes");
        let floor = STRAIN_NOISE * sigma;
        assert!(
            honest < floor,
            "flat plane read {honest:.4} against {floor:.4}"
        );
        assert!(
            strained > floor,
            "saddle facet read {strained:.4} against {floor:.4}"
        );
        assert!(
            facet.surface.rms().expect("rms") < 0.12,
            "the facet passes tolerance"
        );
    }

    #[test]
    fn a_ring_simplifies_to_its_corners_and_keeps_its_winding() {
        // A square walked with extra points along each side, jittered
        // below the tolerance.
        let mut ring = Vec::new();
        for side in 0..4 {
            for k in 0..10 {
                let t = k as f64 / 10.0;
                let jitter = if k % 2 == 0 { 0.01 } else { -0.01 };
                ring.push(match side {
                    0 => (t * 10.0, jitter),
                    1 => (10.0 + jitter, t * 10.0),
                    2 => (10.0 - t * 10.0, 10.0 + jitter),
                    _ => (jitter, 10.0 - t * 10.0),
                });
            }
        }
        let simplified = simplify_ring(&ring, 0.1);
        assert_eq!(simplified.len(), 4, "{simplified:?}");
        assert!(crate::step::signed_area(&simplified) > 99.0);
    }

    #[test]
    fn a_trimmed_annulus_clips_refines_and_evaluates() {
        let outer: Vec<(f64, f64)> = (0..48)
            .map(|k| {
                let t = std::f64::consts::TAU * k as f64 / 48.0;
                (20.0 * t.cos(), 20.0 * t.sin())
            })
            .collect();
        let hole: Vec<(f64, f64)> = (0..24)
            .rev()
            .map(|k| {
                let t = std::f64::consts::TAU * k as f64 / 24.0;
                (6.0 * t.cos(), 6.0 * t.sin())
            })
            .collect();
        let loops = vec![outer.clone(), hole.clone()];
        let (vertices, triangles) = trim_triangulation(&loops).expect("clips");
        let (vertices, triangles) = refine_parameter_mesh(vertices, triangles, 1.5);
        let area: f64 = triangles
            .iter()
            .map(|&[a, b, c]| crate::step::signed_area(&[vertices[a], vertices[b], vertices[c]]))
            .sum();
        let expected = crate::step::signed_area(&outer) + crate::step::signed_area(&hole);
        assert!(
            (area - expected).abs() < 1e-6 * expected,
            "{area} vs {expected}"
        );
        for &[a, b, c] in &triangles {
            for (p, q) in [(a, b), (b, c), (c, a)] {
                let (p, q) = (vertices[p], vertices[q]);
                assert!((q.0 - p.0).hypot(q.1 - p.1) <= 1.5 + 1e-9);
            }
        }
        let mask = TrimMask::of(&loops, ((-20.0, 20.0), (-20.0, 20.0)));
        assert!(!mask.contains((0.0, 0.0)), "the hole is a hole");
        assert!(mask.contains((10.0, 0.0)));
        assert!(mask.contains((-13.0, 13.0)));
        assert!(!mask.contains((19.5, 19.5)), "outside the outer loop");
        assert!(!mask.contains((25.0, 0.0)), "off the domain");
    }

    /// The freeform block, scanned. On the analytic pipeline alone its
    /// top ends as forty-odd strained facets; with the spline stage it
    /// must end as a patch within a stated tolerance of the true top,
    /// while every wall and the floor stay the planes they are.
    #[test]
    fn the_freeform_block_top_becomes_one_patch_on_its_true_surface() {
        let scan = simulate_scan(
            &crate::synth::freeform_block(),
            &SimulateOptions {
                density: 0.6,
                smooth: 0.35,
                noise: 0.02,
                seed: 7,
                ..SimulateOptions::default()
            },
        );
        let report = reverse_engineer(&scan.mesh, &ReverseOptions::default());
        let alignment = report.datum.as_ref().expect("the block has a datum");
        let top = report
            .splines
            .iter()
            .max_by(|a, b| a.area.total_cmp(&b.area))
            .expect("a patch on the top");
        let (hx, hy) = crate::synth::FREEFORM_HALF;
        assert!(
            top.area > 0.8 * 4.0 * hx * hy,
            "the patch covers {:.0} mm^2 of a {:.0} mm^2 top",
            top.area,
            4.0 * hx * hy
        );
        assert!(top.fit.deviation.rms <= report.tolerance);
        // Against the truth: sample the patch over its own trim and carry
        // each point back to the part's frame.
        let truth = crate::synth::ground_truth("freeform-block").expect("truth");
        let back = alignment.transform.inverse();
        let (mut squared, mut count, mut worst) = (0.0f64, 0usize, 0.0f64);
        for triangle in top.tessellate(TESSELLATION_STEP) {
            for point in triangle {
                if let Some(distance) = truth(back.apply_point(point)) {
                    squared += distance * distance;
                    count += 1;
                    worst = worst.max(distance.abs());
                }
            }
        }
        let rms = (squared / count.max(1) as f64).sqrt();
        println!(
            "{}; against the truth rms {rms:.4} max {worst:.4}",
            top.describe()
        );
        assert!(count > 1000, "the patch lies over the true top");
        // The stated tolerance: half the scanner's 0.02 mm noise sigma in
        // RMS, two and a half sigma at worst. The facets it replaces
        // missed the same surface by over a millimetre.
        assert!(rms < 0.01, "rms {rms:.4} from the true surface");
        assert!(worst < 0.05, "worst {worst:.4} from the true surface");
        // The walls and the floor were never candidates.
        let planes = report
            .features
            .iter()
            .filter(|f| matches!(f.surface, SurfaceClass::Plane(_)) && f.area > 500.0)
            .count();
        assert!(planes >= 5, "{planes} large planes survive");
    }

    #[test]
    fn an_all_analytic_part_gives_up_nothing_to_a_spline() {
        let scan = simulate_scan(
            &crate::synth::plate_with_boss(),
            &SimulateOptions {
                density: 0.6,
                smooth: 0.35,
                noise: 0.02,
                seed: 7,
                ..SimulateOptions::default()
            },
        );
        let with = reverse_engineer(&scan.mesh, &ReverseOptions::default());
        let without = reverse_engineer(
            &scan.mesh,
            &ReverseOptions {
                splines: None,
                ..ReverseOptions::default()
            },
        );
        let analytic = |report: &crate::report::ReverseReport| -> f64 {
            report
                .features
                .iter()
                .filter(|f| !matches!(f.surface, SurfaceClass::Freeform))
                .map(|f| f.area)
                .sum()
        };
        assert!(
            (analytic(&with) - analytic(&without)).abs() < 1e-6,
            "analytic area {:.1} with splines, {:.1} without",
            analytic(&with),
            analytic(&without)
        );
    }
}

//! How much of the scan the rebuilt model actually explains.
//!
//! The report's `classified` fraction answers a different question than
//! most readers assume. It says every face was assigned to *some*
//! feature — and it counts a catch-all `EdgeRound` bucket as a success.
//! On the test water pump it reads 99.4 percent while the rebuild
//! expresses 2.4 percent of the surface, because a third of the part sits
//! in one 43,000 mm^2 "edge round" that nothing can emit.
//!
//! Two numbers about different subjects, and only one of them is about
//! geometry. This module measures the geometric one: the share of the
//! scan whose surface lies within tolerance of geometry the rebuild
//! actually emitted.

use artificer_geometry::Vector3;

use crate::datum::DatumAlignment;
use crate::mesh::TriangleMesh;

/// Cells are one tolerance across and the query sweeps its 26 neighbours,
/// so a scan point counts as explained when rebuilt geometry lies within
/// roughly one tolerance of it.
type Cell = (i64, i64, i64);

/// Area-weighted fraction of `scan` that lies within `tolerance` of
/// `rebuilt`, together with the explained and total areas in mm^2.
///
/// `rebuilt` is in the datum frame and `scan` in its own, so `alignment`
/// carries the scan across.
///
/// The rebuilt surface is sampled rather than tested exactly: emitted
/// geometry includes single quads spanning tens of millimetres, and a
/// bounding-box index over those degenerates. Sampling at half a
/// tolerance keeps the answer within a tolerance of the exact one and
/// stays linear in the emitted area.
pub fn explained_area(
    scan: &TriangleMesh,
    rebuilt: &TriangleMesh,
    alignment: &DatumAlignment,
    tolerance: f64,
) -> (f64, f64) {
    let aligned = scan.transformed(&alignment.transform);
    near_fraction(&aligned, rebuilt, tolerance)
}

/// Area of `rebuilt` that lies nowhere near the scan — geometry the model
/// invented.
///
/// The companion to `explained_area`, and just as necessary: a rebuild
/// that sweeps a full annulus where the part has three arms scores well
/// on explained area while being badly wrong. Under-emission and
/// over-emission are both failures, so both get a number.
pub fn invented_area(
    scan: &TriangleMesh,
    rebuilt: &TriangleMesh,
    alignment: &DatumAlignment,
    tolerance: f64,
) -> (f64, f64) {
    let aligned = scan.transformed(&alignment.transform);
    let (near, total) = near_fraction(rebuilt, &aligned, tolerance);
    (total - near, total)
}

/// Which faces of the rebuilt model lie nowhere near the scan, so the
/// invented area can be attributed to the features that produced it.
/// A run of invented material, described where it sits.
pub struct InventedPatch {
    pub feature: usize,
    pub area: f64,
    /// Height range in the datum frame (mm).
    pub z: (f64, f64),
    /// Radius range from the datum axis (mm).
    pub rho: (f64, f64),
    /// Area-weighted mean normal: which way the invented material faces.
    pub facing: Vector3,
}

impl InventedPatch {
    /// A one-line description in the language of the part.
    pub fn describe(&self, label: &str) -> String {
        let axial = self.facing.z;
        let shape = if axial.abs() > 0.7 {
            if axial > 0.0 {
                "up-facing"
            } else {
                "down-facing"
            }
        } else if (self.rho.1 - self.rho.0) < 0.5 {
            "cylindrical wall"
        } else {
            "sloped band"
        };
        format!(
            "{label}: {:.0} mm^2 of {shape} material at radius {:.2}..{:.2}, z {:+.2}..{:+.2}",
            self.area, self.rho.0, self.rho.1, self.z.0, self.z.1
        )
    }
}

/// Says *where* invented material is, not just how much.
///
/// A total is a score; a location is a work list. Grouping invented
/// faces by the feature that emitted them and the height they sit at
/// turns "this model invents 5,617 mm²" into a sentence naming the
/// surface, and the same move on the missing side is what found the
/// gear's bare end-face ring in one run after days of renders.
///
/// The rebuilt mesh is already in the datum frame, so heights and radii
/// read directly against the part's own axis.
pub fn invented_patches(
    scan: &TriangleMesh,
    rebuilt: &TriangleMesh,
    alignment: &DatumAlignment,
    tolerance: f64,
    feature_of_face: &[usize],
) -> Vec<InventedPatch> {
    /// Height band (mm) invented faces are grouped into.
    const BAND: f64 = 1.0;
    let flags = invented_flags(scan, rebuilt, alignment, tolerance);
    let mut groups: std::collections::HashMap<(usize, i64), InventedPatch> =
        std::collections::HashMap::new();
    for (face, &bad) in flags.iter().enumerate() {
        if !bad {
            continue;
        }
        let Some(normal) = rebuilt.face_normal(face) else {
            continue;
        };
        let area = rebuilt.face_area(face);
        if area <= 0.0 {
            continue;
        }
        let centre = rebuilt.face_centroid(face);
        let rho = centre.x.hypot(centre.y);
        let feature = feature_of_face.get(face).copied().unwrap_or(usize::MAX);
        let entry = groups
            .entry((feature, (centre.z / BAND).floor() as i64))
            .or_insert(InventedPatch {
                feature,
                area: 0.0,
                z: (f64::INFINITY, f64::NEG_INFINITY),
                rho: (f64::INFINITY, f64::NEG_INFINITY),
                facing: Vector3::new(0.0, 0.0, 0.0),
            });
        entry.area += area;
        entry.z = (entry.z.0.min(centre.z), entry.z.1.max(centre.z));
        entry.rho = (entry.rho.0.min(rho), entry.rho.1.max(rho));
        entry.facing = entry.facing + normal * area;
    }
    let mut patches: Vec<InventedPatch> = groups
        .into_values()
        .map(|mut patch| {
            let length = patch.facing.length();
            if length > 1e-12 {
                patch.facing = patch.facing / length;
            }
            patch
        })
        .collect();
    patches.sort_by(|a, b| {
        b.area
            .total_cmp(&a.area)
            .then_with(|| a.feature.cmp(&b.feature))
    });
    patches
}

/// How far each face of `subject` sits from `reference`, in whole
/// multiples of the tolerance, capped at `bands`.
///
/// This is the deviation map the commercial packages show live while a
/// model is built, and the number a metrologist actually signs off on:
/// not "does the model explain the scan" but "by how much, and where".
///
/// The distance is quantised rather than exact, because the structure
/// that answers it is an occupancy grid one tolerance across — a face
/// reported in band 2 lies between one and two tolerances away. That is
/// the honest resolution of the evidence, and it is also the resolution
/// anyone reads off a colour map.
///
/// `None` means nothing was found within `bands` tolerances at all:
/// geometry with no counterpart, which a heat map must show as its own
/// colour rather than as merely "far".
pub fn deviation_bands(
    subject: &TriangleMesh,
    reference: &TriangleMesh,
    alignment: &DatumAlignment,
    tolerance: f64,
    bands: u8,
    subject_is_scan: bool,
) -> Vec<Option<u8>> {
    let aligned;
    let (subject, reference) = if subject_is_scan {
        aligned = subject.transformed(&alignment.transform);
        (&aligned, reference)
    } else {
        aligned = reference.transformed(&alignment.transform);
        (subject, &aligned)
    };
    let cell_size = tolerance.max(1e-6);
    let occupied = occupancy(reference, cell_size);
    (0..subject.triangles().len())
        .map(|face| {
            let centre = subject.face_centroid(face);
            let base = (
                (centre.x / cell_size).floor() as i64,
                (centre.y / cell_size).floor() as i64,
                (centre.z / cell_size).floor() as i64,
            );
            // Grow the search shell by shell; the first hit's shell is
            // the band.
            for band in 0..=bands {
                let reach = band as i64;
                for dx in -reach..=reach {
                    for dy in -reach..=reach {
                        for dz in -reach..=reach {
                            // Only the new shell, not the whole cube.
                            if band > 0 && dx.abs() < reach && dy.abs() < reach && dz.abs() < reach
                            {
                                continue;
                            }
                            if occupied.contains(&(base.0 + dx, base.1 + dy, base.2 + dz)) {
                                return Some(band);
                            }
                        }
                    }
                }
            }
            None
        })
        .collect()
}

pub fn invented_flags(
    scan: &TriangleMesh,
    rebuilt: &TriangleMesh,
    alignment: &DatumAlignment,
    tolerance: f64,
) -> Vec<bool> {
    let aligned = scan.transformed(&alignment.transform);
    near_flags(rebuilt, &aligned, tolerance)
        .into_iter()
        .map(|near| !near)
        .collect()
}

/// Area of `subject` lying within `tolerance` of `reference`, and the
/// subject's total area. Both meshes must share a frame.
fn near_fraction(subject: &TriangleMesh, reference: &TriangleMesh, tolerance: f64) -> (f64, f64) {
    let flags = near_flags(subject, reference, tolerance);
    let mut near = 0.0;
    let mut total = 0.0;
    for (face, &close) in flags.iter().enumerate() {
        let area = subject.face_area(face);
        total += area;
        if close {
            near += area;
        }
    }
    (near, total)
}

/// Per-face nearness of `subject` to `reference`; both share a frame.
/// Which cells a mesh's surface passes through.
///
/// Triangles are walked on a barycentric lattice fine enough that
/// consecutive samples are under half a cell apart: emitted geometry
/// includes single quads spanning tens of millimetres, and any index
/// built from their bounding boxes degenerates on exactly those.
fn occupancy(reference: &TriangleMesh, cell_size: f64) -> std::collections::HashSet<Cell> {
    let mut occupied = std::collections::HashSet::new();
    for face in 0..reference.triangles().len() {
        let [a, b, c] = reference.triangle_points(face);
        let longest = (b - a).length().max((c - a).length()).max((c - b).length());
        let steps = ((longest / (cell_size * 0.5)).ceil() as usize).clamp(1, 4096);
        for i in 0..=steps {
            for j in 0..=(steps - i) {
                let (u, v) = (i as f64 / steps as f64, j as f64 / steps as f64);
                let point = a + (b - a) * u + (c - a) * v;
                occupied.insert((
                    (point.x / cell_size).floor() as i64,
                    (point.y / cell_size).floor() as i64,
                    (point.z / cell_size).floor() as i64,
                ));
            }
        }
    }
    occupied
}

fn near_flags(subject: &TriangleMesh, reference: &TriangleMesh, tolerance: f64) -> Vec<bool> {
    let cell_size = tolerance.max(1e-6);
    let occupied = occupancy(reference, cell_size);
    let cell_of = |x: f64, y: f64, z: f64| -> Cell {
        (
            (x / cell_size).floor() as i64,
            (y / cell_size).floor() as i64,
            (z / cell_size).floor() as i64,
        )
    };
    (0..subject.triangles().len())
        .map(|face| {
            let centroid = subject.face_centroid(face);
            let (x, y, z) = cell_of(centroid.x, centroid.y, centroid.z);
            (-1..=1).any(|dx| {
                (-1..=1).any(|dy| (-1..=1).any(|dz| occupied.contains(&(x + dx, y + dy, z + dz))))
            })
        })
        .collect()
}

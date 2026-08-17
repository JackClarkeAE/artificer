//! Mesh health: what is wrong with a scan before anything is fitted to
//! it, and the repairs that are safe to make.
//!
//! Every commercial workflow opens here — heal, fill, denoise, defeature
//! — and this pipeline has managed without, which is worth being honest
//! about in both directions. It has managed because every stage was
//! built to be robust to bad input: fits are median-trimmed, RANSAC
//! peels rather than assumes, coverage is measured against the scan
//! rather than believed. What it cannot manage is a defect that removes
//! evidence, because no amount of robustness recovers a surface that was
//! never measured.
//!
//! So the first job is not repair, it is **measurement**. A scan with no
//! holes needs no hole filling, and a pipeline that fills holes anyway
//! is inventing material for no reason. The inspection runs first and
//! reports; the repair only acts on what the report found.
//!
//! Filling is deliberately conservative. A hole spanned by a fan from
//! its own rim is flat, which is right for the small occlusion gaps a
//! scanner leaves in a face and wrong for a large opening that is
//! genuinely part of the part — so only small holes are closed, and the
//! filled triangles are counted so their contribution to any later
//! measurement can be discounted.

use artificer_geometry::Point3;

use crate::mesh::TriangleMesh;

/// What is wrong with a mesh, in counts that can be acted on.
#[derive(Clone, Debug, Default)]
pub struct MeshHealth {
    pub triangles: usize,
    /// Triangles with no area — three collinear corners. `TriangleMesh`
    /// already refuses repeated *indices*, so this is the only shape a
    /// degenerate can take here, and it still carries no normal and
    /// poisons every average it enters.
    pub degenerate: usize,
    /// Triangles repeating another's three corners.
    pub duplicate: usize,
    /// Edges used by exactly one triangle — the rim of a hole, or of the
    /// scan itself.
    pub boundary_edges: usize,
    /// Closed rims those boundary edges form.
    pub holes: usize,
    /// Edges of the largest hole.
    pub largest_hole: usize,
    /// Edges used by three or more triangles: the surface branches, and
    /// "which side is outside" stops having an answer there.
    pub non_manifold_edges: usize,
    /// Adjacent triangles whose shared edge runs the same way in both —
    /// one of the pair is wound backwards, so its normal points into the
    /// material.
    pub inconsistent_pairs: usize,
}

impl MeshHealth {
    /// Whether anything found is worth acting on.
    pub fn is_clean(&self) -> bool {
        self.degenerate == 0
            && self.duplicate == 0
            && self.holes == 0
            && self.non_manifold_edges == 0
            && self.inconsistent_pairs == 0
    }

    pub fn describe(&self) -> String {
        if self.is_clean() {
            return format!(
                "mesh health: {} triangles, nothing to repair",
                self.triangles
            );
        }
        let mut parts = Vec::new();
        if self.degenerate > 0 {
            parts.push(format!("{} degenerate", self.degenerate));
        }
        if self.duplicate > 0 {
            parts.push(format!("{} duplicate", self.duplicate));
        }
        if self.holes > 0 {
            parts.push(format!(
                "{} hole(s) over {} boundary edge(s), largest {}",
                self.holes, self.boundary_edges, self.largest_hole
            ));
        }
        if self.non_manifold_edges > 0 {
            parts.push(format!("{} non-manifold edge(s)", self.non_manifold_edges));
        }
        if self.inconsistent_pairs > 0 {
            parts.push(format!(
                "{} back-to-front neighbour(s)",
                self.inconsistent_pairs
            ));
        }
        format!(
            "mesh health: {} triangles, {}",
            self.triangles,
            parts.join(", ")
        )
    }
}

/// Half-edges of a triangle, in winding order.
fn half_edges(triangle: [u32; 3]) -> [(u32, u32); 3] {
    [
        (triangle[0], triangle[1]),
        (triangle[1], triangle[2]),
        (triangle[2], triangle[0]),
    ]
}

/// Measures a mesh without changing it.
pub fn inspect(mesh: &TriangleMesh) -> MeshHealth {
    let triangles = mesh.triangles();
    let mut health = MeshHealth {
        triangles: triangles.len(),
        ..Default::default()
    };
    // Degenerate and duplicate, in one pass.
    let mut seen: std::collections::HashSet<[u32; 3]> = std::collections::HashSet::new();
    let mut live: Vec<bool> = Vec::with_capacity(triangles.len());
    for (index, triangle) in triangles.iter().enumerate() {
        let mut sorted = *triangle;
        sorted.sort_unstable();
        if mesh.face_area(index) <= 0.0 {
            health.degenerate += 1;
            live.push(false);
            continue;
        }
        if !seen.insert(sorted) {
            health.duplicate += 1;
            live.push(false);
            continue;
        }
        live.push(true);
    }
    // Edge use, over the triangles that count.
    let mut uses: std::collections::HashMap<(u32, u32), Vec<(u32, u32)>> =
        std::collections::HashMap::new();
    for (index, triangle) in triangles.iter().enumerate() {
        if !live[index] {
            continue;
        }
        for (a, b) in half_edges(*triangle) {
            uses.entry((a.min(b), a.max(b))).or_default().push((a, b));
        }
    }
    let mut boundary: Vec<(u32, u32)> = Vec::new();
    for (key, directed) in &uses {
        match directed.len() {
            1 => {
                health.boundary_edges += 1;
                boundary.push(directed[0]);
            }
            2 => {
                // Consistent winding means the two uses run opposite
                // ways; the same way means one neighbour is flipped.
                if directed[0] == directed[1] {
                    health.inconsistent_pairs += 1;
                }
            }
            _ => {
                health.non_manifold_edges += 1;
                let _ = key;
            }
        }
    }
    // Walk boundary edges into loops.
    let mut next: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for &(a, b) in &boundary {
        next.entry(a).or_default().push(b);
    }
    let mut used: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    // Deterministic: walk the boundary edges in the order they were
    // collected from a sorted scan of the triangles, not hash order.
    let mut ordered = boundary.clone();
    ordered.sort_unstable();
    for &(start, _) in &ordered {
        let mut here = start;
        let mut length = 0usize;
        while let Some(&onward) = next.get(&here).and_then(|candidates| {
            candidates
                .iter()
                .find(|&&onward| !used.contains(&(here, onward)))
        }) {
            used.insert((here, onward));
            length += 1;
            here = onward;
            if here == start {
                break;
            }
        }
        if length > 0 {
            health.holes += 1;
            health.largest_hole = health.largest_hole.max(length);
        }
    }
    health
}

/// What a repair pass changed.
#[derive(Clone, Debug, Default)]
pub struct RepairReport {
    pub dropped_degenerate: usize,
    pub dropped_duplicate: usize,
    pub holes_filled: usize,
    pub triangles_added: usize,
    /// Holes left open because they were larger than the limit — a scan
    /// of an open shape has a rim, and closing it would be inventing a
    /// face the part does not have.
    pub holes_left: usize,
}

impl RepairReport {
    pub fn describe(&self) -> String {
        format!(
            "mesh repair: dropped {} degenerate and {} duplicate triangle(s); \
             filled {} hole(s) with {} triangle(s); left {} hole(s) too large to close",
            self.dropped_degenerate,
            self.dropped_duplicate,
            self.holes_filled,
            self.triangles_added,
            self.holes_left
        )
    }
}

/// Drops the junk triangles and closes small holes.
///
/// The limit is in boundary edges rather than millimetres because it is
/// a statement about the *hole*, not the part: a gap a scanner left in a
/// face is a handful of edges however big the part is, while the open
/// end of a tube is hundreds however small.
pub fn repair(mesh: &TriangleMesh, max_hole_edges: usize) -> (TriangleMesh, RepairReport) {
    let mut report = RepairReport::default();
    let triangles = mesh.triangles();
    let mut kept: Vec<[u32; 3]> = Vec::with_capacity(triangles.len());
    let mut seen: std::collections::HashSet<[u32; 3]> = std::collections::HashSet::new();
    for (index, triangle) in triangles.iter().enumerate() {
        let mut sorted = *triangle;
        sorted.sort_unstable();
        if mesh.face_area(index) <= 0.0 {
            report.dropped_degenerate += 1;
            continue;
        }
        if !seen.insert(sorted) {
            report.dropped_duplicate += 1;
            continue;
        }
        kept.push(*triangle);
    }
    // Boundary rims of what survived.
    let mut uses: std::collections::HashMap<(u32, u32), usize> = std::collections::HashMap::new();
    for triangle in &kept {
        for (a, b) in half_edges(*triangle) {
            *uses.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut boundary: Vec<(u32, u32)> = Vec::new();
    for triangle in &kept {
        for (a, b) in half_edges(*triangle) {
            if uses.get(&(a.min(b), a.max(b))) == Some(&1) {
                boundary.push((a, b));
            }
        }
    }
    boundary.sort_unstable();
    let mut next: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for &(a, b) in &boundary {
        next.entry(a).or_default().push(b);
    }
    let mut used: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let mut positions = mesh.positions().to_vec();
    for &(start, _) in &boundary {
        let mut loop_vertices = vec![start];
        let mut here = start;
        while let Some(&onward) = next.get(&here).and_then(|candidates| {
            candidates
                .iter()
                .find(|&&onward| !used.contains(&(here, onward)))
        }) {
            used.insert((here, onward));
            here = onward;
            if here == start {
                break;
            }
            loop_vertices.push(here);
        }
        if loop_vertices.len() < 3 || here != start {
            continue;
        }
        if loop_vertices.len() > max_hole_edges {
            report.holes_left += 1;
            continue;
        }
        // Fan from the rim's own centroid. Flat, which is what a small
        // occlusion gap in a face actually is.
        let centre = loop_vertices
            .iter()
            .fold(Point3::default(), |acc, &vertex| {
                let p = positions[vertex as usize];
                Point3::new(
                    acc.x + p.x / loop_vertices.len() as f64,
                    acc.y + p.y / loop_vertices.len() as f64,
                    acc.z + p.z / loop_vertices.len() as f64,
                )
            });
        let hub = positions.len() as u32;
        positions.push(centre);
        for window in 0..loop_vertices.len() {
            let a = loop_vertices[window];
            let b = loop_vertices[(window + 1) % loop_vertices.len()];
            // The rim runs along the open side, so a fan wound against it
            // faces the same way as the surface it closes.
            kept.push([hub, b, a]);
            report.triangles_added += 1;
        }
        report.holes_filled += 1;
    }
    let repaired = TriangleMesh::new(positions, kept).unwrap_or_else(|| mesh.clone());
    (repaired, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;
    use artificer_geometry::Vector3;

    /// A closed box has nothing wrong with it, and the inspection must
    /// say so rather than finding work to do.
    #[test]
    fn a_closed_box_is_clean() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(Point3::default(), Vector3::new(10.0, 10.0, 10.0), 3),
            1e-6,
        )
        .expect("mesh");
        let health = inspect(&mesh);
        assert!(
            health.is_clean(),
            "a closed box should need no repair: {}",
            health.describe()
        );
    }

    /// Punch a hole in that box: the inspection must find exactly one,
    /// and the repair must close it and say how.
    #[test]
    fn a_punched_hole_is_found_and_filled() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(Point3::default(), Vector3::new(10.0, 10.0, 10.0), 3),
            1e-6,
        )
        .expect("mesh");
        // Drop one triangle to open a three-edge hole.
        let mut triangles = mesh.triangles().to_vec();
        triangles.remove(0);
        let punched = TriangleMesh::new(mesh.positions().to_vec(), triangles).expect("mesh");
        let health = inspect(&punched);
        assert_eq!(health.holes, 1, "one hole: {}", health.describe());
        assert_eq!(health.boundary_edges, 3);
        let (repaired, report) = repair(&punched, 64);
        assert_eq!(report.holes_filled, 1);
        assert!(report.triangles_added >= 3);
        assert_eq!(
            inspect(&repaired).holes,
            0,
            "the hole must be closed after repair"
        );
    }

    /// An open tube's rim is not a defect. Closing it would invent a
    /// face the part does not have, so a limit leaves it alone and the
    /// report says it was left.
    #[test]
    fn a_large_rim_is_left_open() {
        let mesh = synth::open_cylinder(8.0, 20.0, 64, 4);
        let health = inspect(&mesh);
        assert_eq!(
            health.holes,
            2,
            "a tube has two rims: {}",
            health.describe()
        );
        let (_, report) = repair(&mesh, 8);
        assert_eq!(report.holes_filled, 0);
        assert_eq!(report.holes_left, 2, "both rims are too large to close");
    }

    /// Degenerate and duplicate triangles are found and dropped.
    #[test]
    fn junk_triangles_are_dropped() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(Point3::default(), Vector3::new(4.0, 4.0, 4.0), 2),
            1e-6,
        )
        .expect("mesh");
        let mut positions = mesh.positions().to_vec();
        let mut triangles = mesh.triangles().to_vec();
        let first = triangles[0];
        // A duplicate face, and a collinear one: both survive
        // `TriangleMesh::new` (which only forbids repeated indices) and
        // both arrive in real scans, the second whenever welding pulls a
        // sliver's corners onto one line.
        triangles.push(first);
        let a = positions[first[0] as usize];
        let b = positions[first[1] as usize];
        let midpoint = Point3::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0, (a.z + b.z) / 2.0);
        positions.push(midpoint);
        let mid = positions.len() as u32 - 1;
        triangles.push([first[0], mid, first[1]]);
        let dirty = TriangleMesh::new(positions, triangles).expect("mesh");
        let health = inspect(&dirty);
        assert_eq!(health.duplicate, 1);
        assert_eq!(health.degenerate, 1);
        let (repaired, report) = repair(&dirty, 8);
        assert_eq!(report.dropped_duplicate, 1);
        assert_eq!(report.dropped_degenerate, 1);
        assert_eq!(repaired.triangles().len(), mesh.triangles().len());
    }
}

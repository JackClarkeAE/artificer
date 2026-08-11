//! Indexed triangle meshes as produced by structured-light and laser scanners.

use std::collections::HashMap;

use artificer_geometry::{Bounds3, Point3, Vector3};

use crate::transform::{RigidTransform, normalize};

#[derive(Clone, Debug)]
pub struct TriangleMesh {
    positions: Vec<Point3>,
    triangles: Vec<[u32; 3]>,
}

impl TriangleMesh {
    /// Builds a mesh, rejecting non-finite positions, out-of-range indices,
    /// and topologically degenerate (repeated-index) triangles.
    pub fn new(positions: Vec<Point3>, triangles: Vec<[u32; 3]>) -> Option<Self> {
        let count = u32::try_from(positions.len()).ok()?;
        if positions.iter().any(|p| !p.is_finite()) {
            return None;
        }
        for [a, b, c] in &triangles {
            if *a >= count || *b >= count || *c >= count || a == b || b == c || a == c {
                return None;
            }
        }
        Some(Self {
            positions,
            triangles,
        })
    }

    /// Welds a triangle soup into an indexed mesh. Vertices closer than
    /// `weld_tolerance` collapse to a single vertex; geometrically
    /// degenerate triangles are dropped.
    pub fn from_triangle_soup(soup: &[[Point3; 3]], weld_tolerance: f64) -> Option<Self> {
        let tolerance = weld_tolerance.max(1e-12);
        let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
        let mut positions: Vec<Point3> = Vec::new();
        let mut triangles = Vec::with_capacity(soup.len());
        let cell = |value: f64| (value / tolerance).floor() as i64;
        let mut intern = |p: Point3| -> Option<u32> {
            if !p.is_finite() {
                return None;
            }
            let key = (cell(p.x), cell(p.y), cell(p.z));
            for dx in -1..=1_i64 {
                for dy in -1..=1_i64 {
                    for dz in -1..=1_i64 {
                        let neighbor = (key.0 + dx, key.1 + dy, key.2 + dz);
                        if let Some(bucket) = grid.get(&neighbor) {
                            for &index in bucket {
                                if (positions[index as usize] - p).length() <= tolerance {
                                    return Some(index);
                                }
                            }
                        }
                    }
                }
            }
            let index = u32::try_from(positions.len()).ok()?;
            positions.push(p);
            grid.entry(key).or_default().push(index);
            Some(index)
        };
        for triangle in soup {
            let a = intern(triangle[0])?;
            let b = intern(triangle[1])?;
            let c = intern(triangle[2])?;
            if a != b && b != c && a != c {
                triangles.push([a, b, c]);
            }
        }
        Self::new(positions, triangles)
    }

    pub fn positions(&self) -> &[Point3] {
        &self.positions
    }

    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.triangles
    }

    pub fn triangle_points(&self, face: usize) -> [Point3; 3] {
        let [a, b, c] = self.triangles[face];
        [
            self.positions[a as usize],
            self.positions[b as usize],
            self.positions[c as usize],
        ]
    }

    /// Cross-product face vector: direction is the face normal, length is
    /// twice the triangle area.
    pub fn face_area_vector(&self, face: usize) -> Vector3 {
        let [a, b, c] = self.triangle_points(face);
        (b - a).cross(c - a)
    }

    pub fn face_normal(&self, face: usize) -> Option<Vector3> {
        normalize(self.face_area_vector(face))
    }

    pub fn face_area(&self, face: usize) -> f64 {
        self.face_area_vector(face).length() * 0.5
    }

    pub fn surface_area(&self) -> f64 {
        (0..self.triangles.len()).map(|f| self.face_area(f)).sum()
    }

    pub fn face_centroid(&self, face: usize) -> Point3 {
        let [a, b, c] = self.triangle_points(face);
        Point3::new(
            (a.x + b.x + c.x) / 3.0,
            (a.y + b.y + c.y) / 3.0,
            (a.z + b.z + c.z) / 3.0,
        )
    }

    pub fn bounds(&self) -> Option<Bounds3> {
        let first = *self.positions.first()?;
        let mut min = first;
        let mut max = first;
        for p in &self.positions {
            min = Point3::new(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
            max = Point3::new(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
        }
        Bounds3::new(min, max)
    }

    pub fn bounds_diagonal(&self) -> f64 {
        self.bounds().map_or(0.0, |b| (b.max - b.min).length())
    }

    /// Area-weighted vertex normals; zero vector where every incident
    /// triangle is degenerate.
    pub fn vertex_normals(&self) -> Vec<Vector3> {
        let mut sums = vec![Vector3::default(); self.positions.len()];
        for face in 0..self.triangles.len() {
            let weighted = self.face_area_vector(face);
            for index in self.triangles[face] {
                sums[index as usize] = sums[index as usize] + weighted;
            }
        }
        sums.into_iter()
            .map(|sum| normalize(sum).unwrap_or_default())
            .collect()
    }

    /// For every face, the faces sharing one of its edges.
    pub fn face_adjacency(&self) -> Vec<Vec<u32>> {
        let mut edge_faces: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        for (face, [a, b, c]) in self.triangles.iter().enumerate() {
            let face = face as u32;
            for (u, v) in [(a, b), (b, c), (c, a)] {
                let key = (*u.min(v), *u.max(v));
                edge_faces.entry(key).or_default().push(face);
            }
        }
        let mut adjacency = vec![Vec::new(); self.triangles.len()];
        for faces in edge_faces.values() {
            for &f in faces {
                for &g in faces {
                    if f != g {
                        adjacency[f as usize].push(g);
                    }
                }
            }
        }
        // HashMap iteration order is randomized per process; sorted lists
        // keep downstream traversals (and the seeded RANSAC) reproducible.
        for neighbors in &mut adjacency {
            neighbors.sort_unstable();
        }
        adjacency
    }

    /// Display-grade decimation by vertex clustering on a uniform grid.
    ///
    /// Every vertex inside a cell collapses to the cell average; triangles
    /// whose corners land in three distinct cells survive. Returns the
    /// simplified mesh and, per surviving triangle, the index of the
    /// original triangle it came from — this is for viewers only, the
    /// reconstruction pipeline always works on the full mesh.
    pub fn simplified_by_clustering(&self, cell_size: f64) -> (Self, Vec<u32>) {
        let cell_size = cell_size.max(1e-9);
        let cell = |value: f64| (value / cell_size).floor() as i64;
        let mut cluster_of_vertex = Vec::with_capacity(self.positions.len());
        let mut clusters: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let mut sums: Vec<(Vector3, f64)> = Vec::new();
        for p in &self.positions {
            let key = (cell(p.x), cell(p.y), cell(p.z));
            let next_id = sums.len() as u32;
            let id = *clusters.entry(key).or_insert(next_id);
            if id as usize == sums.len() {
                sums.push((Vector3::default(), 0.0));
            }
            sums[id as usize].0 = sums[id as usize].0 + (*p - Point3::default());
            sums[id as usize].1 += 1.0;
            cluster_of_vertex.push(id);
        }
        let positions: Vec<Point3> = sums
            .into_iter()
            .map(|(sum, count)| Point3::default() + sum / count)
            .collect();
        let mut triangles = Vec::new();
        let mut origins = Vec::new();
        for (face, [a, b, c]) in self.triangles.iter().enumerate() {
            let ca = cluster_of_vertex[*a as usize];
            let cb = cluster_of_vertex[*b as usize];
            let cc = cluster_of_vertex[*c as usize];
            if ca != cb && cb != cc && ca != cc {
                triangles.push([ca, cb, cc]);
                origins.push(face as u32);
            }
        }
        (
            Self {
                positions,
                triangles,
            },
            origins,
        )
    }

    pub fn transformed(&self, transform: &RigidTransform) -> Self {
        Self {
            positions: self
                .positions
                .iter()
                .map(|p| transform.apply_point(*p))
                .collect(),
            triangles: self.triangles.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad_soup() -> Vec<[Point3; 3]> {
        let a = Point3::new(0.0, 0.0, 0.0);
        let b = Point3::new(1.0, 0.0, 0.0);
        let c = Point3::new(1.0, 1.0, 0.0);
        let d = Point3::new(0.0, 1.0, 0.0);
        vec![[a, b, c], [a, c, d]]
    }

    #[test]
    fn welding_merges_shared_vertices() {
        let mesh = TriangleMesh::from_triangle_soup(&quad_soup(), 1e-6).unwrap();
        assert_eq!(mesh.positions().len(), 4);
        assert_eq!(mesh.triangles().len(), 2);
        assert!((mesh.surface_area() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn adjacency_links_across_the_shared_edge() {
        let mesh = TriangleMesh::from_triangle_soup(&quad_soup(), 1e-6).unwrap();
        let adjacency = mesh.face_adjacency();
        assert_eq!(adjacency[0], vec![1]);
        assert_eq!(adjacency[1], vec![0]);
    }

    #[test]
    fn vertex_normals_point_up_for_a_flat_quad() {
        let mesh = TriangleMesh::from_triangle_soup(&quad_soup(), 1e-6).unwrap();
        for normal in mesh.vertex_normals() {
            assert!((normal.z - 1.0).abs() < 1e-12);
        }
    }
}

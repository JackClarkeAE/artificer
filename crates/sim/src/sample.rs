//! Reading a nodal field at an arbitrary point, which is how a result on
//! the grid is carried back onto the part's own faces.
//!
//! The display tessellation's vertices lie on the exact surface, which is
//! at best on a grid plane and at worst half a cell from any node. A vertex
//! is therefore read at a point nudged a little inward along its normal,
//! in the element that contains that point, by trilinear interpolation of
//! the element's eight nodes; a point that lands in a void cell — a thin
//! feature the grid lost — reads from the nearest solid cell nearby, or
//! nothing at all.

use artificer_protocol::{Point3, Vector3};

use crate::element::NODE_OFFSETS;
use crate::mesh::VoxelMesh;

/// Reads nodal fields at points.
#[derive(Clone, Copy, Debug)]
pub struct FieldSampler<'a> {
    mesh: &'a VoxelMesh,
}

/// Where a point reads from: an element and its position inside it, each
/// coordinate in `0..=1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    pub element: usize,
    pub local: [f64; 3],
}

impl<'a> FieldSampler<'a> {
    #[must_use]
    pub const fn new(mesh: &'a VoxelMesh) -> Self {
        Self { mesh }
    }

    /// The point a surface vertex is read at: nudged inward along its
    /// outward normal by a fraction of a cell, so a vertex lying exactly
    /// on a grid plane reads from the element inside the part rather than
    /// the void outside it.
    #[must_use]
    pub fn inward(&self, vertex: Point3, normal: Vector3) -> Point3 {
        let nudge = 0.25 * self.mesh.grid().cell();
        Point3::new(
            normal.x.mul_add(-nudge, vertex.x),
            normal.y.mul_add(-nudge, vertex.y),
            normal.z.mul_add(-nudge, vertex.z),
        )
    }

    /// The element a point reads from, or `None` when nothing solid is
    /// within two cells of it.
    #[must_use]
    pub fn locate(&self, point: Point3) -> Option<Location> {
        let grid = self.mesh.grid();
        let dims = grid.dims();
        if dims.contains(&0) {
            return None;
        }
        let origin = grid.origin();
        let cell = grid.cell();
        let fractional = [
            (point.x - origin.x) / cell,
            (point.y - origin.y) / cell,
            (point.z - origin.z) / cell,
        ];
        if fractional.iter().any(|value| !value.is_finite()) {
            return None;
        }
        // Clamp to the grid: a vertex a hair outside the bounding box is
        // still the part's own surface.
        let clamped: [usize; 3] = std::array::from_fn(|axis| {
            fractional[axis].floor().clamp(0.0, dims[axis] as f64 - 1.0) as usize
        });
        let element = self.mesh.element_in_cell(clamped).or_else(|| {
            // The nearest solid cell within two steps, ties to the lowest
            // index so the answer is the same every time.
            let mut best: Option<(f64, usize, usize)> = None;
            for radius in 1..=2_isize {
                for dz in -radius..=radius {
                    for dy in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx.abs().max(dy.abs()).max(dz.abs()) != radius {
                                continue;
                            }
                            let candidate = [
                                clamped[0] as isize + dx,
                                clamped[1] as isize + dy,
                                clamped[2] as isize + dz,
                            ];
                            if candidate.iter().any(|value| *value < 0) {
                                continue;
                            }
                            let candidate = candidate.map(|value| value as usize);
                            let Some(element) = self.mesh.element_in_cell(candidate) else {
                                continue;
                            };
                            let centre = grid.cell_centre(candidate);
                            let distance = (centre.x - point.x)
                                .hypot(centre.y - point.y)
                                .hypot(centre.z - point.z);
                            let index = grid.index(candidate);
                            if best.is_none_or(|(held, held_index, _)| {
                                distance < held || (distance == held && index < held_index)
                            }) {
                                best = Some((distance, index, element));
                            }
                        }
                    }
                }
                if best.is_some() {
                    break;
                }
            }
            best.map(|(_, _, element)| element)
        })?;
        let home = self.mesh.cell_of_element(element);
        let local =
            std::array::from_fn(|axis| (fractional[axis] - home[axis] as f64).clamp(0.0, 1.0));
        Some(Location { element, local })
    }

    /// The trilinear weight of each of the element's nodes at a location.
    fn weights(local: [f64; 3]) -> [f64; 8] {
        std::array::from_fn(|node| {
            NODE_OFFSETS[node]
                .iter()
                .zip(local)
                .map(|(offset, t)| if *offset == 1 { t } else { 1.0 - t })
                .product()
        })
    }

    /// A per-node vector field read at a point.
    #[must_use]
    pub fn vector_at(&self, point: Point3, per_node: &[[f64; 3]]) -> Option<[f64; 3]> {
        let location = self.locate(point)?;
        let nodes = self.mesh.nodes_of(location.element);
        let weights = Self::weights(location.local);
        let mut value = [0.0; 3];
        for (node, weight) in nodes.iter().zip(weights) {
            let held = per_node.get(*node as usize)?;
            for axis in 0..3 {
                value[axis] = weight.mul_add(held[axis], value[axis]);
            }
        }
        Some(value)
    }

    /// A per-node scalar field read at a point.
    #[must_use]
    pub fn scalar_at(&self, point: Point3, per_node: &[f64]) -> Option<f64> {
        let location = self.locate(point)?;
        let nodes = self.mesh.nodes_of(location.element);
        let weights = Self::weights(location.local);
        let mut value = 0.0;
        for (node, weight) in nodes.iter().zip(weights) {
            value = weight.mul_add(*per_node.get(*node as usize)?, value);
        }
        Some(value)
    }

    /// A per-element scalar field read at a point: the value of the
    /// element the point lies in.
    #[must_use]
    pub fn element_scalar_at(&self, point: Point3, per_element: &[f64]) -> Option<f64> {
        let location = self.locate(point)?;
        per_element.get(location.element).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artificer_kernel::VoxelGrid;

    fn slab() -> VoxelMesh {
        // A 4 × 2 × 1 slab with its middle two cells missing on the far
        // row, so there is a void to read across.
        let mut solid = vec![true; 8];
        solid[1 + 4] = false;
        solid[2 + 4] = false;
        VoxelMesh::from_grid(VoxelGrid::new(
            Point3::new(0.0, 0.0, 0.0),
            2.0,
            [4, 2, 1],
            solid,
            Vec::new(),
        ))
    }

    #[test]
    fn a_linear_field_is_reproduced_exactly_inside_an_element() {
        let mesh = slab();
        let field = (0..mesh.node_count())
            .map(|node| {
                let position = mesh.node_position(node);
                3.0 * position.x - position.y + 0.5 * position.z + 1.0
            })
            .collect::<Vec<_>>();
        let sampler = FieldSampler::new(&mesh);
        let point = Point3::new(1.3, 0.7, 1.9);
        let value = sampler.scalar_at(point, &field).unwrap();
        assert!((value - (3.0 * 1.3 - 0.7 + 0.5 * 1.9 + 1.0)).abs() < 1.0e-12);
    }

    #[test]
    fn a_point_in_a_void_reads_from_the_nearest_solid_cell() {
        let mesh = slab();
        let sampler = FieldSampler::new(&mesh);
        // Cell [1, 1, 0] is void; the nearest solid cell is directly
        // below it at [1, 0, 0].
        let location = sampler.locate(Point3::new(3.0, 3.0, 1.0)).unwrap();
        assert_eq!(mesh.cell_of_element(location.element), [1, 0, 0]);
        assert_eq!(location.local[1], 1.0, "clamped to the element's far side");
        assert!(sampler.locate(Point3::new(f64::NAN, 0.0, 0.0)).is_none());
    }

    #[test]
    fn a_vertex_is_read_just_inside_its_normal() {
        let mesh = slab();
        let sampler = FieldSampler::new(&mesh);
        let inside = sampler.inward(Point3::new(8.0, 1.0, 1.0), Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(inside, Point3::new(7.5, 1.0, 1.0));
        assert_eq!(
            mesh.cell_of_element(sampler.locate(inside).unwrap().element),
            [3, 0, 0]
        );
    }
}

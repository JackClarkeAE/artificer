//! The finite-element mesh a voxel grid implies: one hexahedral element per
//! solid cell, nodes at the shared corners, and the sides that face out.

use std::collections::BTreeSet;

use artificer_kernel::VoxelGrid;
use artificer_protocol::{EntityRef, Point3};

use crate::element::{NODE_OFFSETS, SIDE_NODES, SIDE_STEPS, side_of_step};

/// One element per solid cell, with its nodes numbered compactly.
#[derive(Clone, Debug)]
pub struct VoxelMesh {
    grid: VoxelGrid,
    /// The eight node ids of each element, in [`NODE_OFFSETS`] order.
    elements: Vec<[u32; 8]>,
    /// The cell each element stands in, in element order.
    element_cells: Vec<[u32; 3]>,
    /// The element in each cell, or `u32::MAX` for a void cell.
    cell_to_element: Vec<u32>,
    /// The lattice corner each node sits on.
    node_corners: Vec<[u32; 3]>,
}

impl VoxelMesh {
    /// Meshes every solid cell of a grid.
    #[must_use]
    pub fn from_grid(grid: VoxelGrid) -> Self {
        let dims = grid.dims();
        let corner_dims = [dims[0] + 1, dims[1] + 1, dims[2] + 1];
        let corner_index = |corner: [usize; 3]| {
            corner[0] + corner_dims[0] * (corner[1] + corner_dims[1] * corner[2])
        };
        let mut corner_to_node = vec![u32::MAX; corner_dims[0] * corner_dims[1] * corner_dims[2]];
        let mut node_corners = Vec::new();
        let mut elements = Vec::new();
        let mut element_cells = Vec::new();
        let mut cell_to_element = vec![u32::MAX; grid.cell_count()];
        for cell in grid.solid_cells() {
            let mut nodes = [0_u32; 8];
            for (slot, offset) in NODE_OFFSETS.iter().enumerate() {
                let corner = [
                    cell[0] + offset[0],
                    cell[1] + offset[1],
                    cell[2] + offset[2],
                ];
                let index = corner_index(corner);
                if corner_to_node[index] == u32::MAX {
                    corner_to_node[index] = node_corners.len() as u32;
                    node_corners.push([corner[0] as u32, corner[1] as u32, corner[2] as u32]);
                }
                nodes[slot] = corner_to_node[index];
            }
            cell_to_element[grid.index(cell)] = elements.len() as u32;
            elements.push(nodes);
            element_cells.push([cell[0] as u32, cell[1] as u32, cell[2] as u32]);
        }
        Self {
            grid,
            elements,
            element_cells,
            cell_to_element,
            node_corners,
        }
    }

    #[must_use]
    pub const fn grid(&self) -> &VoxelGrid {
        &self.grid
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_corners.len()
    }

    #[must_use]
    pub fn element_count(&self) -> usize {
        self.elements.len()
    }

    /// The eight nodes of an element.
    #[must_use]
    pub fn nodes_of(&self, element: usize) -> [u32; 8] {
        self.elements[element]
    }

    /// Every element's nodes, in element order.
    #[must_use]
    pub fn elements(&self) -> &[[u32; 8]] {
        &self.elements
    }

    /// The cell an element stands in.
    #[must_use]
    pub fn cell_of_element(&self, element: usize) -> [usize; 3] {
        let cell = self.element_cells[element];
        [cell[0] as usize, cell[1] as usize, cell[2] as usize]
    }

    /// The element standing in a cell, or `None` for a void cell.
    #[must_use]
    pub fn element_in_cell(&self, cell: [usize; 3]) -> Option<usize> {
        let dims = self.grid.dims();
        if cell[0] >= dims[0] || cell[1] >= dims[1] || cell[2] >= dims[2] {
            return None;
        }
        let element = self.cell_to_element[self.grid.index(cell)];
        (element != u32::MAX).then_some(element as usize)
    }

    /// The lattice corner a node sits on.
    #[must_use]
    pub fn node_corner(&self, node: usize) -> [usize; 3] {
        let corner = self.node_corners[node];
        [corner[0] as usize, corner[1] as usize, corner[2] as usize]
    }

    /// Where a node is, in millimetres.
    #[must_use]
    pub fn node_position(&self, node: usize) -> Point3 {
        let corner = self.node_corner(node);
        let origin = self.grid.origin();
        let cell = self.grid.cell();
        Point3::new(
            (corner[0] as f64).mul_add(cell, origin.x),
            (corner[1] as f64).mul_add(cell, origin.y),
            (corner[2] as f64).mul_add(cell, origin.z),
        )
    }

    /// The centre of an element, in millimetres.
    #[must_use]
    pub fn element_centre(&self, element: usize) -> Point3 {
        self.grid.cell_centre(self.cell_of_element(element))
    }

    /// The sides of an element that face a void cell or the edge of the
    /// grid, as indices into [`SIDE_NODES`] and [`SIDE_STEPS`].
    pub fn exposed_sides(&self, element: usize) -> impl Iterator<Item = usize> + '_ {
        let cell = self.cell_of_element(element);
        (0..6).filter(move |side| !self.grid.neighbour_is_solid(cell, SIDE_STEPS[*side]))
    }

    /// The four nodes on one side of an element.
    #[must_use]
    pub fn side_nodes(&self, element: usize, side: usize) -> [u32; 4] {
        let nodes = self.elements[element];
        SIDE_NODES[side].map(|slot| nodes[slot])
    }

    /// Every (element, side) pair lying on one face of the body, from the
    /// grid's surface table.
    #[must_use]
    pub fn sides_on_face(&self, face: EntityRef) -> Vec<(usize, usize)> {
        let mut sides = self
            .grid
            .cells_on_face(face)
            .filter_map(|surface| {
                let element = self.element_in_cell(surface.cell)?;
                let side = side_of_step(surface.outward)?;
                Some((element, side))
            })
            .collect::<Vec<_>>();
        sides.sort_unstable();
        sides.dedup();
        sides
    }

    /// The nodes on one face of the body: those on the exposed sides of the
    /// surface cells lying on it, sorted.
    #[must_use]
    pub fn nodes_on_face(&self, face: EntityRef) -> Vec<u32> {
        let mut nodes = BTreeSet::new();
        for (element, side) in self.sides_on_face(face) {
            nodes.extend(self.side_nodes(element, side));
        }
        nodes.into_iter().collect()
    }

    /// The area of one side of a cell, in square millimetres.
    #[must_use]
    pub fn side_area(&self) -> f64 {
        self.grid.cell() * self.grid.cell()
    }

    /// The volume of one cell, in cubic millimetres.
    #[must_use]
    pub fn cell_volume(&self) -> f64 {
        self.grid.cell().powi(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(dims: [usize; 3]) -> VoxelMesh {
        let grid = VoxelGrid::new(
            Point3::new(0.0, 0.0, 0.0),
            1.0,
            dims,
            vec![true; dims[0] * dims[1] * dims[2]],
            Vec::new(),
        );
        VoxelMesh::from_grid(grid)
    }

    #[test]
    fn a_block_shares_its_nodes_and_exposes_only_its_skin() {
        let mesh = block([2, 3, 4]);
        assert_eq!(mesh.element_count(), 24);
        assert_eq!(mesh.node_count(), 3 * 4 * 5);
        // An interior corner is shared by eight elements.
        let shared = mesh
            .elements()
            .iter()
            .filter(|nodes| {
                nodes.contains(
                    &mesh
                        .element_in_cell([0, 1, 1])
                        .map(|e| mesh.nodes_of(e)[6])
                        .unwrap(),
                )
            })
            .count();
        assert_eq!(shared, 8);
        let exposed: usize = (0..mesh.element_count())
            .map(|element| mesh.exposed_sides(element).count())
            .sum();
        // The surface of a 2 × 3 × 4 block in unit faces.
        assert_eq!(exposed, 2 * (2 * 3 + 3 * 4 + 2 * 4));
        assert_eq!(mesh.node_position(0), Point3::new(0.0, 0.0, 0.0));
    }
}

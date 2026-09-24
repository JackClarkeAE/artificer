//! The two questions a simulation asks of a body (ADR 0058): whether a
//! point is inside it, and the body as a grid of cells.
//!
//! ## Voxelising
//!
//! A voxel grid is a bit per cell over the body's bounding box. It is
//! filled by parity: along each of the three grid axes, a ray through every
//! column of cell centres crosses the display tessellation, the crossings
//! are sorted, and a cell is inside where an odd number of crossings lie
//! below its centre. The three axes vote, so a crack between two faces'
//! facets — which would flip one ray's parity for the rest of its column —
//! is outvoted by the two rays that never saw it.
//!
//! Every crossing also says which face it crossed, which is how a surface
//! cell knows the face it lies on: the simulation's boundary conditions are
//! picked by face, and its results are painted back onto faces, and both
//! go through that table.
//!
//! The grid is an approximation by construction, and says so: a cell is
//! inside or out by its centre, so a boundary layer one cell thick is
//! uncertain either way. The simulation crate labels everything it derives
//! from it as approximate.

use std::collections::BTreeSet;

use artificer_protocol::{EntityRef, Point3};

use crate::topology::Point3 as TopologyPoint3;
use crate::{DebugScene, NativeKernel, Snapshot};

/// The most cells one grid may hold. A 200³ grid is eight million cells, a
/// few dozen megabytes with the fields a study lays over it, and past this
/// the request is a mistake rather than a resolution.
pub const MAX_VOXELS: usize = 8_000_000;

/// One cell of a [`VoxelGrid`] that the body's surface passes through, and
/// which face of the body it lies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SurfaceCell {
    pub cell: [usize; 3],
    pub face: EntityRef,
    /// Which side of the cell the surface faces out of: a unit step along
    /// one grid axis, pointing out of the solid.
    pub outward: [i8; 3],
}

/// A body as a uniform grid of cubic cells over its bounding box.
#[derive(Clone, Debug, PartialEq)]
pub struct VoxelGrid {
    origin: Point3,
    cell: f64,
    dims: [usize; 3],
    solid: Vec<bool>,
    surface: Vec<SurfaceCell>,
}

impl VoxelGrid {
    /// A grid from its parts, for callers that build one without a body: a
    /// design domain for an optimisation, or a test fixture.
    ///
    /// `solid` runs x fastest, then y, then z, and must hold exactly
    /// `dims[0] * dims[1] * dims[2]` cells; a mismatch gives an empty grid.
    #[must_use]
    pub fn new(
        origin: Point3,
        cell: f64,
        dims: [usize; 3],
        solid: Vec<bool>,
        mut surface: Vec<SurfaceCell>,
    ) -> Self {
        if solid.len() != dims[0] * dims[1] * dims[2] || !(cell.is_finite() && cell > 0.0) {
            return Self::empty(cell);
        }
        surface.sort_unstable();
        surface.dedup();
        Self {
            origin,
            cell,
            dims,
            solid,
            surface,
        }
    }

    /// A grid with no cells at all.
    #[must_use]
    pub fn empty(cell: f64) -> Self {
        Self {
            origin: Point3::new(0.0, 0.0, 0.0),
            cell: if cell.is_finite() && cell > 0.0 {
                cell
            } else {
                1.0
            },
            dims: [0, 0, 0],
            solid: Vec::new(),
            surface: Vec::new(),
        }
    }

    /// The corner of cell `[0, 0, 0]`: the bounding box's minimum.
    #[must_use]
    pub const fn origin(&self) -> Point3 {
        self.origin
    }

    /// The edge length of every cell, in millimetres.
    #[must_use]
    pub const fn cell(&self) -> f64 {
        self.cell
    }

    /// How many cells along x, y and z.
    #[must_use]
    pub const fn dims(&self) -> [usize; 3] {
        self.dims
    }

    /// Every cell, solid or not.
    #[must_use]
    pub const fn cell_count(&self) -> usize {
        self.dims[0] * self.dims[1] * self.dims[2]
    }

    /// The flat index of a cell: x fastest, then y, then z.
    #[must_use]
    pub const fn index(&self, cell: [usize; 3]) -> usize {
        cell[0] + self.dims[0] * (cell[1] + self.dims[1] * cell[2])
    }

    /// The cell a flat index names.
    #[must_use]
    pub const fn cell_at(&self, index: usize) -> [usize; 3] {
        let i = index % self.dims[0];
        let rest = index / self.dims[0];
        [i, rest % self.dims[1], rest / self.dims[1]]
    }

    /// Whether a cell is inside the body. Anything off the grid is not.
    #[must_use]
    pub fn is_solid(&self, cell: [usize; 3]) -> bool {
        cell[0] < self.dims[0]
            && cell[1] < self.dims[1]
            && cell[2] < self.dims[2]
            && self.solid[self.index(cell)]
    }

    /// Whether the cell one step from `cell` is inside the body; a step off
    /// the grid is outside.
    #[must_use]
    pub fn neighbour_is_solid(&self, cell: [usize; 3], step: [i8; 3]) -> bool {
        let mut next = [0_usize; 3];
        for axis in 0..3 {
            let moved = cell[axis] as isize + isize::from(step[axis]);
            if moved < 0 {
                return false;
            }
            next[axis] = moved as usize;
        }
        self.is_solid(next)
    }

    /// The solid cells, in flat-index order.
    pub fn solid_cells(&self) -> impl Iterator<Item = [usize; 3]> + '_ {
        self.solid
            .iter()
            .enumerate()
            .filter(|(_, solid)| **solid)
            .map(|(index, _)| self.cell_at(index))
    }

    /// How many cells are inside the body.
    #[must_use]
    pub fn solid_count(&self) -> usize {
        self.solid.iter().filter(|solid| **solid).count()
    }

    /// The volume the solid cells stand for, in cubic millimetres.
    #[must_use]
    pub fn solid_volume(&self) -> f64 {
        self.solid_count() as f64 * self.cell * self.cell * self.cell
    }

    /// Whether the grid holds no solid cell at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.solid.iter().any(|solid| *solid)
    }

    /// The corner of a cell nearest the origin.
    #[must_use]
    pub fn cell_min(&self, cell: [usize; 3]) -> Point3 {
        Point3::new(
            (cell[0] as f64).mul_add(self.cell, self.origin.x),
            (cell[1] as f64).mul_add(self.cell, self.origin.y),
            (cell[2] as f64).mul_add(self.cell, self.origin.z),
        )
    }

    /// The middle of a cell.
    #[must_use]
    pub fn cell_centre(&self, cell: [usize; 3]) -> Point3 {
        Point3::new(
            (cell[0] as f64 + 0.5).mul_add(self.cell, self.origin.x),
            (cell[1] as f64 + 0.5).mul_add(self.cell, self.origin.y),
            (cell[2] as f64 + 0.5).mul_add(self.cell, self.origin.z),
        )
    }

    /// The cell a point falls in, or `None` off the grid.
    #[must_use]
    pub fn cell_of(&self, point: Point3) -> Option<[usize; 3]> {
        let mut cell = [0_usize; 3];
        for (axis, value) in [
            point.x - self.origin.x,
            point.y - self.origin.y,
            point.z - self.origin.z,
        ]
        .into_iter()
        .enumerate()
        {
            let index = (value / self.cell).floor();
            if !index.is_finite() || index < 0.0 || index >= self.dims[axis] as f64 {
                return None;
            }
            cell[axis] = index as usize;
        }
        Some(cell)
    }

    /// Every surface cell with the face it lies on, sorted by cell.
    #[must_use]
    pub fn surface(&self) -> &[SurfaceCell] {
        &self.surface
    }

    /// The surface cells lying on one face.
    pub fn cells_on_face(&self, face: EntityRef) -> impl Iterator<Item = &SurfaceCell> + '_ {
        self.surface.iter().filter(move |cell| cell.face == face)
    }

    /// Every face the surface cells lie on, in topology order of first
    /// appearance.
    #[must_use]
    pub fn faces(&self) -> Vec<EntityRef> {
        let mut seen = BTreeSet::new();
        let mut faces = Vec::new();
        for cell in &self.surface {
            if seen.insert(cell.face) {
                faces.push(cell.face);
            }
        }
        faces.sort_by_key(|face| face.entity.0);
        faces
    }
}

/// One ray's crossing of the tessellation: where along the ray, which face,
/// and which way the solid faces there.
#[derive(Clone, Copy, Debug)]
struct Crossing {
    along: f64,
    face: EntityRef,
    /// `+1` where the ray leaves the solid, `-1` where it enters.
    outward: i8,
}

impl NativeKernel {
    /// Whether `point` lies inside the solid, by exact parity ray casting
    /// against the analytic faces.
    ///
    /// `None` where the answer cannot be given exactly: a face whose
    /// carrier the exact cast does not handle (cone, sphere, torus, ruled
    /// or B-spline), or every ray direction grazing a boundary. Points on
    /// the surface itself are among the latter.
    #[must_use]
    pub fn point_in_solid(snapshot: &Snapshot, point: Point3) -> Option<bool> {
        crate::analytic_boolean::point_in_solid(
            &snapshot.topology,
            TopologyPoint3::new(point.x, point.y, point.z),
        )
    }

    /// The body as a grid of cubic cells `cell` millimetres on a side over
    /// its bounding box, with the face each surface cell lies on.
    ///
    /// The grid is empty for a body with no bounds, a cell that is not a
    /// positive finite length, or a grid that would exceed [`MAX_VOXELS`].
    #[must_use]
    pub fn voxelise(snapshot: &Snapshot, cell: f64) -> VoxelGrid {
        let Some(bounds) = snapshot
            .measures()
            .bounds
            .filter(|bounds| bounds.is_finite())
        else {
            return VoxelGrid::empty(cell);
        };
        if !(cell.is_finite() && cell > 0.0) {
            return VoxelGrid::empty(cell);
        }
        let extent = [
            bounds.max.x - bounds.min.x,
            bounds.max.y - bounds.min.y,
            bounds.max.z - bounds.min.z,
        ];
        let mut dims = [0_usize; 3];
        for axis in 0..3 {
            // A body exactly n cells wide gets n cells, not n + 1 with the
            // last one empty: the count is rounded up from just under.
            let count = (extent[axis] / cell - 1.0e-9).ceil().max(1.0);
            if !count.is_finite() || count > MAX_VOXELS as f64 {
                return VoxelGrid::empty(cell);
            }
            dims[axis] = count as usize;
        }
        if dims[0].saturating_mul(dims[1]).saturating_mul(dims[2]) > MAX_VOXELS {
            return VoxelGrid::empty(cell);
        }
        let origin = bounds.min;
        let scene = Self::debug_scene(snapshot);
        voxelise_scene(&scene, origin, cell, dims)
    }
}

/// Fills a grid from a display scene by three-axis parity with a majority
/// vote, and lists the surface cells by the faces their crossings name.
fn voxelise_scene(scene: &DebugScene, origin: Point3, cell: f64, dims: [usize; 3]) -> VoxelGrid {
    let total = dims[0] * dims[1] * dims[2];
    let mut votes = vec![0_u8; total];
    let mut crossings_by_axis = Vec::with_capacity(3);
    for axis in 0..3 {
        let columns = scan_axis(scene, origin, cell, dims, axis);
        vote_axis(&columns, origin, cell, dims, axis, &mut votes);
        crossings_by_axis.push(columns);
    }
    let solid = votes.iter().map(|vote| *vote >= 2).collect::<Vec<_>>();

    let mut surface = BTreeSet::new();
    let index = |cell: [usize; 3]| cell[0] + dims[0] * (cell[1] + dims[1] * cell[2]);
    for (axis, columns) in crossings_by_axis.iter().enumerate() {
        let (b, c) = other_axes(axis);
        for (column, crossings) in columns.iter().enumerate() {
            let column_b = column % dims[b];
            let column_c = column / dims[b];
            for crossing in crossings {
                // The solid sits just inside the crossing, on the side the
                // surface does not face.
                let nudge = -f64::from(crossing.outward) * 1.0e-6 * cell;
                let along = (crossing.along + nudge - origin_component(origin, axis)) / cell;
                if !along.is_finite() {
                    continue;
                }
                let along = along.floor().clamp(0.0, dims[axis] as f64 - 1.0) as usize;
                let mut position = [0_usize; 3];
                position[axis] = along;
                position[b] = column_b;
                position[c] = column_c;
                if !solid[index(position)] {
                    continue;
                }
                let mut outward = [0_i8; 3];
                outward[axis] = crossing.outward;
                surface.insert(SurfaceCell {
                    cell: position,
                    face: crossing.face,
                    outward,
                });
            }
        }
    }
    VoxelGrid {
        origin,
        cell,
        dims,
        solid,
        surface: surface.into_iter().collect(),
    }
}

const fn other_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    }
}

const fn origin_component(origin: Point3, axis: usize) -> f64 {
    match axis {
        0 => origin.x,
        1 => origin.y,
        _ => origin.z,
    }
}

const fn component(point: Point3, axis: usize) -> f64 {
    match axis {
        0 => point.x,
        1 => point.y,
        _ => point.z,
    }
}

/// The crossings of every column of rays along one axis, sorted along the
/// ray. Columns run over the other two axes, the lower one fastest.
fn scan_axis(
    scene: &DebugScene,
    origin: Point3,
    cell: f64,
    dims: [usize; 3],
    axis: usize,
) -> Vec<Vec<Crossing>> {
    let (b, c) = other_axes(axis);
    let mut columns: Vec<Vec<Crossing>> = vec![Vec::new(); dims[b] * dims[c]];
    let origin_b = origin_component(origin, b);
    let origin_c = origin_component(origin, c);
    for triangle in &scene.triangles {
        let mut projected = triangle
            .vertices
            .map(|vertex| [component(vertex, b), component(vertex, c)]);
        let mut along = triangle.vertices.map(|vertex| component(vertex, axis));
        let area = signed_area(projected);
        if !area.is_finite() || area == 0.0 {
            // Parallel to the ray: it is crossed by the other two scans.
            continue;
        }
        if area < 0.0 {
            projected.swap(1, 2);
            along.swap(1, 2);
        }
        let outward_component = triangle
            .normals
            .iter()
            .map(|normal| match axis {
                0 => normal.x,
                1 => normal.y,
                _ => normal.z,
            })
            .sum::<f64>();
        let outward = if outward_component >= 0.0 { 1 } else { -1 };

        // The columns whose centres the projection can contain.
        let (min_b, max_b) = extent(projected.map(|p| p[0]));
        let (min_c, max_c) = extent(projected.map(|p| p[1]));
        let first_b = column_from(min_b, origin_b, cell);
        let first_c = column_from(min_c, origin_c, cell);
        let (Some(last_b), Some(last_c)) = (
            column_to(max_b, origin_b, cell, dims[b]),
            column_to(max_c, origin_c, cell, dims[c]),
        ) else {
            continue;
        };
        if first_b > last_b || first_c > last_c {
            continue;
        }
        for column_c in first_c..=last_c {
            let centre_c = (column_c as f64 + 0.5).mul_add(cell, origin_c);
            for column_b in first_b..=last_b {
                let centre_b = (column_b as f64 + 0.5).mul_add(cell, origin_b);
                let Some(weights) = barycentric_inside(projected, [centre_b, centre_c]) else {
                    continue;
                };
                let hit = weights[0] * along[0] + weights[1] * along[1] + weights[2] * along[2];
                columns[column_b + dims[b] * column_c].push(Crossing {
                    along: hit,
                    face: triangle.source_face,
                    outward,
                });
            }
        }
    }
    for crossings in &mut columns {
        crossings.sort_by(|left, right| {
            left.along
                .total_cmp(&right.along)
                .then(left.face.entity.0.cmp(&right.face.entity.0))
                .then(left.outward.cmp(&right.outward))
        });
    }
    columns
}

/// Adds one vote per cell whose centre one axis's rays find inside.
fn vote_axis(
    columns: &[Vec<Crossing>],
    origin: Point3,
    cell: f64,
    dims: [usize; 3],
    axis: usize,
    votes: &mut [u8],
) {
    let (b, c) = other_axes(axis);
    let origin_a = origin_component(origin, axis);
    for (column, crossings) in columns.iter().enumerate() {
        if crossings.is_empty() {
            continue;
        }
        let column_b = column % dims[b];
        let column_c = column / dims[b];
        let mut crossed = 0_usize;
        let mut next = 0_usize;
        for along_index in 0..dims[axis] {
            let centre = (along_index as f64 + 0.5).mul_add(cell, origin_a);
            while next < crossings.len() && crossings[next].along < centre {
                crossed += 1;
                next += 1;
            }
            if crossed % 2 == 1 {
                let mut position = [0_usize; 3];
                position[axis] = along_index;
                position[b] = column_b;
                position[c] = column_c;
                votes[position[0] + dims[0] * (position[1] + dims[1] * position[2])] += 1;
            }
        }
    }
}

fn extent(values: [f64; 3]) -> (f64, f64) {
    (
        values.iter().copied().fold(f64::INFINITY, f64::min),
        values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    )
}

/// The first column whose centre is at or past `value`.
fn column_from(value: f64, origin: f64, cell: f64) -> usize {
    let index = ((value - origin) / cell - 0.5).ceil();
    if index.is_finite() && index > 0.0 {
        index as usize
    } else {
        0
    }
}

/// The last column whose centre is at or before `value`, clamped to the
/// grid, or `None` when no column centre lies before it.
fn column_to(value: f64, origin: f64, cell: f64, dims: usize) -> Option<usize> {
    let index = ((value - origin) / cell - 0.5).floor();
    if !index.is_finite() || index < 0.0 || dims == 0 {
        return None;
    }
    Some((index as usize).min(dims - 1))
}

fn signed_area(points: [[f64; 2]; 3]) -> f64 {
    let [a, b, c] = points;
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// The barycentric weights of `point` inside a counter-clockwise triangle,
/// or `None` outside it.
///
/// A point on an edge belongs to exactly one of the two triangles that
/// share the edge — the one for which it is a top or left edge, the rule
/// every rasteriser uses — so a ray through a shared edge crosses the
/// surface once rather than twice or not at all.
fn barycentric_inside(triangle: [[f64; 2]; 3], point: [f64; 2]) -> Option<[f64; 3]> {
    let area = signed_area(triangle);
    let mut weights = [0.0; 3];
    for corner in 0..3 {
        let start = triangle[(corner + 1) % 3];
        let end = triangle[(corner + 2) % 3];
        let edge = (end[0] - start[0]) * (point[1] - start[1])
            - (end[1] - start[1]) * (point[0] - start[0]);
        if edge < 0.0 {
            return None;
        }
        if edge == 0.0 {
            let direction = [end[0] - start[0], end[1] - start[1]];
            let top_left = direction[1] < 0.0 || (direction[1] == 0.0 && direction[0] > 0.0);
            if !top_left {
                return None;
            }
        }
        weights[corner] = edge / area;
    }
    Some(weights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_on_a_shared_edge_belongs_to_exactly_one_triangle() {
        let left = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
        let right = [[0.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let on_diagonal = [0.5, 0.5];
        let hits = usize::from(barycentric_inside(left, on_diagonal).is_some())
            + usize::from(barycentric_inside(right, on_diagonal).is_some());
        assert_eq!(hits, 1);
        assert!(barycentric_inside(left, [0.75, 0.25]).is_some());
        assert!(barycentric_inside(right, [0.75, 0.25]).is_none());
    }

    #[test]
    fn a_hand_built_grid_answers_its_own_geometry() {
        let grid = VoxelGrid::new(
            Point3::new(1.0, 2.0, 3.0),
            0.5,
            [2, 3, 4],
            vec![true; 24],
            Vec::new(),
        );
        assert_eq!(grid.cell_count(), 24);
        assert_eq!(grid.solid_count(), 24);
        assert_eq!(grid.cell_at(grid.index([1, 2, 3])), [1, 2, 3]);
        assert_eq!(grid.cell_of(Point3::new(1.9, 3.4, 4.9)), Some([1, 2, 3]));
        assert_eq!(grid.cell_of(Point3::new(0.9, 3.4, 4.9)), None);
        assert!(!grid.neighbour_is_solid([0, 0, 0], [-1, 0, 0]));
        assert!(grid.neighbour_is_solid([0, 0, 0], [1, 0, 0]));
        let centre = grid.cell_centre([0, 0, 0]);
        assert_eq!((centre.x, centre.y, centre.z), (1.25, 2.25, 3.25));
    }

    #[test]
    fn a_grid_with_the_wrong_cell_count_is_empty() {
        let grid = VoxelGrid::new(
            Point3::new(0.0, 0.0, 0.0),
            1.0,
            [2, 2, 2],
            vec![true; 7],
            Vec::new(),
        );
        assert!(grid.is_empty());
        assert_eq!(grid.dims(), [0, 0, 0]);
    }
}

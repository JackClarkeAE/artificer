//! A spatial index over face extents, so the analytic Boolean's per-face
//! stages visit candidate faces rather than every face (ADR 0056 R2).
//!
//! The general analytic engine reduces a Boolean to a per-face 2D operation
//! between a face's region and the other solid's section on that face's
//! carrier. Both the section and the coincident-overlay pass ask, for one
//! face of one operand, which faces of the other could interact with it.
//! Asked by scanning every face, that is O(faces²): a plate with a
//! thousand drilled holes then spends a million carrier tests on a Boolean,
//! nearly all of them between a hole on one side of the plate and a hole on
//! the other that could not possibly meet.
//!
//! Each face carries a model-space [`FaceExtent`] it cannot leave (a
//! superset of the face, so a pair the extents separate is a pair the faces
//! separate). This index buckets those extent boxes into a uniform grid and
//! answers "which faces' extents come near this box" by looking only in the
//! cells the box touches.
//!
//! # Why pruning here is safe
//!
//! The pieces a face `G` of the other solid contributes to face `F`'s
//! section are `G`'s carrier curve **clipped to `G`'s own region** and then
//! re-expressed on `F`'s carrier. A piece clipped to `G`'s region lies
//! inside `G`'s extent; if `G`'s extent is disjoint from `F`'s extent, that
//! piece lies outside `F`, so `F`'s 2D Boolean discards it and the closure
//! of the section within `F` never depended on it. Dropping such a `G` from
//! the scan therefore cannot change which pieces `F` keeps — only how many
//! it examines. The index is a superset broad phase: it never rules out a
//! pair the exact [`super::analytic_boolean::faces_apart`] test would keep,
//! so the result is byte for byte what the exhaustive scan produced. Below
//! a threshold the index reports every face, so small bodies — every
//! existing fixture — run the exhaustive path unchanged.

use crate::analytic_boolean::FaceExtent;
use crate::topology::Point3;

/// The box that contains every one of these extents, or `None` when any face
/// lacks one (then the solid's box is not fully known and no face can be
/// ruled outside it).
#[must_use]
pub(crate) fn union_extent(extents: &[Option<FaceExtent>]) -> Option<FaceExtent> {
    let mut low = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut high = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut any = false;
    for extent in extents {
        let extent = (*extent)?;
        low = Point3::new(
            low.x.min(extent.min.x),
            low.y.min(extent.min.y),
            low.z.min(extent.min.z),
        );
        high = Point3::new(
            high.x.max(extent.max.x),
            high.y.max(extent.max.y),
            high.z.max(extent.max.z),
        );
        any = true;
    }
    any.then_some(FaceExtent {
        min: low,
        max: high,
    })
}

/// The face count below which the index reports every face rather than
/// building a grid. Small bodies pay nothing and behave exactly as the
/// exhaustive scan did; only bodies large enough for O(faces²) to bite
/// build the grid.
const GRID_THRESHOLD: usize = 128;

/// The most cells the grid spans along any axis. A body's faces bucket into
/// at most this cubed cells, so the grid's memory is bounded whatever the
/// face count.
const MAX_CELLS_PER_AXIS: usize = 48;

#[derive(Clone, Copy)]
struct Aabb {
    min: Point3,
    max: Point3,
}

/// A uniform grid over face extents. A face whose box spans a large fraction
/// of the grid is held in `large` and returned for every query rather than
/// bucketed, so one big face (a plate's whole top) does not land in every
/// cell.
pub(crate) struct FaceIndex {
    face_count: usize,
    grid: Option<Grid>,
    /// Faces with no extent are never separated from anything, so they are a
    /// candidate for every query.
    without_extent: Vec<usize>,
}

struct Grid {
    origin: Point3,
    cell: [f64; 3],
    dims: [usize; 3],
    cells: Vec<Vec<usize>>,
    large: Vec<usize>,
    boxes: Vec<Option<Aabb>>,
}

impl FaceIndex {
    /// Builds the index over one operand's face extents, in face order.
    #[must_use]
    pub(crate) fn new(extents: &[Option<FaceExtent>]) -> Self {
        let boxes: Vec<Option<Aabb>> = extents
            .iter()
            .map(|extent| {
                extent.map(|extent| Aabb {
                    min: extent.min,
                    max: extent.max,
                })
            })
            .collect();
        let without_extent = boxes
            .iter()
            .enumerate()
            .filter_map(|(index, aabb)| aabb.is_none().then_some(index))
            .collect();
        let present = boxes.iter().filter(|aabb| aabb.is_some()).count();
        let grid = (present >= GRID_THRESHOLD).then(|| Grid::new(&boxes));
        Self {
            face_count: boxes.len(),
            grid,
            without_extent,
        }
    }

    /// Every face whose extent may come near `query`, in ascending index
    /// order. `None` — a face with no extent — matches everything, and so
    /// does an unindexed body: both return every face, which is the
    /// exhaustive scan the pruning replaces.
    ///
    /// The order is the faces' own order, so a caller that iterates the
    /// result builds its pieces in the same sequence the exhaustive scan
    /// did, and the sewn topology — and its digest — are unchanged.
    #[must_use]
    pub(crate) fn candidates(&self, query: Option<FaceExtent>) -> Vec<usize> {
        let Some(grid) = &self.grid else {
            return (0..self.face_count).collect();
        };
        let Some(query) = query.map(|extent| Aabb {
            min: extent.min,
            max: extent.max,
        }) else {
            return (0..self.face_count).collect();
        };
        grid.candidates(&query, &self.without_extent)
    }
}

impl Grid {
    fn new(boxes: &[Option<Aabb>]) -> Self {
        let mut low = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut high = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut present = 0_usize;
        for aabb in boxes.iter().flatten() {
            low = Point3::new(
                low.x.min(aabb.min.x),
                low.y.min(aabb.min.y),
                low.z.min(aabb.min.z),
            );
            high = Point3::new(
                high.x.max(aabb.max.x),
                high.y.max(aabb.max.y),
                high.z.max(aabb.max.z),
            );
            present += 1;
        }
        // Aim for a handful of faces per cell: roughly `present` cells, split
        // evenly across the three axes.
        let target = (present as f64).cbrt().ceil().max(1.0) as usize;
        let per_axis = target.clamp(1, MAX_CELLS_PER_AXIS);
        let span = [high.x - low.x, high.y - low.y, high.z - low.z];
        let cell = span.map(|extent| {
            let size = extent / per_axis as f64;
            if size.is_finite() && size > 0.0 {
                size
            } else {
                1.0
            }
        });
        let dims = [per_axis, per_axis, per_axis];
        let mut cells = vec![Vec::new(); dims[0] * dims[1] * dims[2]];
        let mut large = Vec::new();
        // A box wider than half the grid along an axis is a big face; keep it
        // in `large` rather than in every cell it would otherwise fill.
        for (index, aabb) in boxes.iter().enumerate() {
            let Some(aabb) = aabb else { continue };
            let range = cell_range(aabb, low, cell, dims);
            let spans =
                (0..3).any(|axis| range[axis].1 - range[axis].0 + 1 > dims[axis].max(2) / 2);
            if spans {
                large.push(index);
                continue;
            }
            for x in range[0].0..=range[0].1 {
                for y in range[1].0..=range[1].1 {
                    for z in range[2].0..=range[2].1 {
                        cells[(z * dims[1] + y) * dims[0] + x].push(index);
                    }
                }
            }
        }
        Self {
            origin: low,
            cell,
            dims,
            cells,
            large,
            boxes: boxes.to_vec(),
        }
    }

    fn candidates(&self, query: &Aabb, without_extent: &[usize]) -> Vec<usize> {
        let mut found = self.large.clone();
        found.extend_from_slice(without_extent);
        let range = cell_range(query, self.origin, self.cell, self.dims);
        for x in range[0].0..=range[0].1 {
            for y in range[1].0..=range[1].1 {
                for z in range[2].0..=range[2].1 {
                    for &index in &self.cells[(z * self.dims[1] + y) * self.dims[0] + x] {
                        // The grown-box overlap the grid answers is coarse;
                        // confirm the boxes really meet before reporting the
                        // face, so a query gathers only genuine neighbours.
                        if self.boxes[index].is_some_and(|aabb| boxes_meet(&aabb, query)) {
                            found.push(index);
                        }
                    }
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }
}

/// The inclusive cell index range a box covers, grown by one cell on each
/// side so a box that lands on a boundary is found from either neighbour and
/// the coarse grid never misses a meeting pair.
fn cell_range(
    aabb: &Aabb,
    origin: Point3,
    cell: [f64; 3],
    dims: [usize; 3],
) -> [(usize, usize); 3] {
    let axis = |value: f64, minimum: f64, size: f64, count: usize| -> usize {
        let index = ((value - minimum) / size).floor();
        if index.is_finite() {
            (index as i64).clamp(0, count as i64 - 1) as usize
        } else {
            0
        }
    };
    [0, 1, 2].map(|dimension| {
        let (min, max, origin_axis) = match dimension {
            0 => (aabb.min.x, aabb.max.x, origin.x),
            1 => (aabb.min.y, aabb.max.y, origin.y),
            _ => (aabb.min.z, aabb.max.z, origin.z),
        };
        let count = dims[dimension];
        let low = axis(min, origin_axis, cell[dimension], count).saturating_sub(1);
        let high = (axis(max, origin_axis, cell[dimension], count) + 1).min(count - 1);
        (low, high)
    })
}

/// Whether two boxes overlap when each is grown by a relative margin. The
/// margin is far wider than [`super::analytic_boolean::faces_apart`]'s, so
/// every pair that test keeps this test keeps too: the index is a superset.
fn boxes_meet(left: &Aabb, right: &Aabb) -> bool {
    let scale = [left.min, left.max, right.min, right.max]
        .iter()
        .map(|point| point.x.abs().max(point.y.abs()).max(point.z.abs()))
        .fold(1.0_f64, f64::max);
    let margin = 1.0e-6 * scale;
    left.min.x - margin <= right.max.x
        && right.min.x - margin <= left.max.x
        && left.min.y - margin <= right.max.y
        && right.min.y - margin <= left.max.y
        && left.min.z - margin <= right.max.z
        && right.min.z - margin <= left.max.z
}

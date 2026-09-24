//! Stock models: the lathe's exact `(r, z)` section and the mill's
//! heightmap (ADR 0057 §2.3 and §2.4).

use artificer_kernel::NativeKernel;
use artificer_protocol::{BooleanOperation, PlanarLoop2, PlanarRegion2, Point2, PrecisionPolicy};

use crate::CamRefusal;
use crate::geom;

/// The bar as its section: one or more regions in `(r, z)`, each an outer
/// loop with holes, updated exactly by the kernel's planar Boolean as every
/// pass subtracts what its tool swept.
#[derive(Clone, Debug, PartialEq)]
pub struct LatheStock {
    pub regions: Vec<PlanarRegion2>,
    precision: PrecisionPolicy,
}

impl LatheStock {
    /// A solid bar of `radius` from `back` to `front` along `z`.
    #[must_use]
    pub fn bar(radius: f64, back: f64, front: f64) -> Self {
        Self {
            regions: vec![PlanarRegion2 {
                outer: geom::rectangle(Point2::new(0.0, back), Point2::new(radius, front)),
                holes: Vec::new(),
            }],
            precision: PrecisionPolicy::default(),
        }
    }

    /// Subtracts one swept region.
    pub fn cut(&mut self, swept: &PlanarLoop2) -> Result<(), CamRefusal> {
        if self.regions.is_empty() {
            return Ok(());
        }
        let tool = [PlanarRegion2 {
            outer: geom::counter_clockwise(swept),
            holes: Vec::new(),
        }];
        // A sweep clear of the stock changes nothing and need not trouble
        // the Boolean.
        let Some((tool_min, tool_max)) = geom::bounds(&tool[0].outer) else {
            return Ok(());
        };
        let touches = self.regions.iter().any(|region| {
            geom::region_bounds(region).is_some_and(|(min, max)| {
                tool_min.x <= max.x
                    && tool_max.x >= min.x
                    && tool_min.y <= max.y
                    && tool_max.y >= min.y
            })
        });
        if !touches {
            return Ok(());
        }
        let result = NativeKernel::profile_boolean(
            &self.regions,
            &tool,
            BooleanOperation::Difference,
            self.precision,
        )
        .map_err(|error| CamRefusal::StockModel {
            detail: format!("{error} (swept region bounds {tool_min} .. {tool_max})"),
        })?;
        self.regions = result;
        Ok(())
    }

    /// The material's area in the section.
    #[must_use]
    pub fn area(&self) -> f64 {
        self.regions.iter().map(geom::region_area).sum()
    }

    /// The volume the section sweeps, by Pappus: each loop's area times the
    /// circle its centroid's radius traces.
    #[must_use]
    pub fn volume(&self) -> f64 {
        self.regions
            .iter()
            .map(|region| {
                loop_swept_volume(&region.outer)
                    - region.holes.iter().map(loop_swept_volume).sum::<f64>()
            })
            .sum()
    }

    /// The region furthest towards the tailstock: the part once it is
    /// parted off, the whole bar before.
    #[must_use]
    pub fn part_region(&self) -> Option<&PlanarRegion2> {
        self.regions.iter().max_by(|a, b| {
            let za = geom::region_bounds(a).map_or(f64::NEG_INFINITY, |(_, max)| max.y);
            let zb = geom::region_bounds(b).map_or(f64::NEG_INFINITY, |(_, max)| max.y);
            za.total_cmp(&zb)
        })
    }

    /// Whether a point of the section lies in remaining material.
    #[must_use]
    pub fn contains(&self, p: Point2) -> bool {
        self.regions
            .iter()
            .any(|region| geom::point_in_region(region, p))
    }

    /// Whether a point lies in remaining material by more than `margin`:
    /// a tool retracting along the face it has just cut is on the boundary,
    /// not in the stock.
    #[must_use]
    pub fn contains_with_margin(&self, p: Point2, margin: f64) -> bool {
        self.regions.iter().any(|region| {
            geom::point_in_region(region, p)
                && geom::distance_to_loop(&region.outer, p) > margin
                && region
                    .holes
                    .iter()
                    .all(|hole| geom::distance_to_loop(hole, p) > margin)
        })
    }
}

/// The mill's stock as a heightmap over the stock top: each cell holds the
/// height of the material left in it, and each tool position lowers every
/// cell under the tool's footprint to the tip's height. Exact for flat end
/// mills on 2.5D geometry, which is what this slice makes, and the model
/// every hobby simulator uses.
#[derive(Clone, Debug, PartialEq)]
pub struct MillStock {
    pub min: Point2,
    pub cell: f64,
    pub columns: usize,
    pub rows: usize,
    pub bottom: f64,
    pub top: f64,
    pub heights: Vec<f32>,
}

impl MillStock {
    /// The largest grid on either axis.
    pub const MAX_CELLS: usize = 512;

    /// A box from `min` to `max` in work coordinates, gridded at about
    /// `cell` millimetres, capped at [`Self::MAX_CELLS`] a side.
    #[must_use]
    pub fn new(
        min: artificer_protocol::Point3,
        max: artificer_protocol::Point3,
        cell: f64,
    ) -> Self {
        let width = (max.x - min.x).max(1.0e-6);
        let depth = (max.y - min.y).max(1.0e-6);
        let cell = cell
            .max(width / Self::MAX_CELLS as f64)
            .max(depth / Self::MAX_CELLS as f64);
        let columns = ((width / cell).ceil() as usize).clamp(1, Self::MAX_CELLS);
        let rows = ((depth / cell).ceil() as usize).clamp(1, Self::MAX_CELLS);
        // Spread the cells over the box exactly.
        let cell = (width / columns as f64).max(depth / rows as f64);
        Self {
            min: Point2::new(min.x, min.y),
            cell,
            columns,
            rows,
            bottom: min.z,
            top: max.z,
            heights: vec![max.z as f32; columns * rows],
        }
    }

    /// The grid a plan's simulation uses: cells an eighth of the smallest
    /// tool, and never coarser than a hundred and twentieth of the stock's
    /// narrower side, so a small part is still resolved.
    #[must_use]
    pub fn for_tools(
        min: artificer_protocol::Point3,
        max: artificer_protocol::Point3,
        smallest_tool_diameter: f64,
    ) -> Self {
        let narrow = (max.x - min.x).min(max.y - min.y).max(1.0e-6);
        let cell = (smallest_tool_diameter / 8.0)
            .min(narrow / 120.0)
            .max(1.0e-3);
        Self::new(min, max, cell)
    }

    #[must_use]
    pub fn cell_center(&self, column: usize, row: usize) -> Point2 {
        Point2::new(
            self.cell.mul_add(column as f64 + 0.5, self.min.x),
            self.cell.mul_add(row as f64 + 0.5, self.min.y),
        )
    }

    /// The height of the material at a point, or `None` outside the stock.
    #[must_use]
    pub fn height_at(&self, x: f64, y: f64) -> Option<f64> {
        let column = ((x - self.min.x) / self.cell).floor();
        let row = ((y - self.min.y) / self.cell).floor();
        if column < 0.0 || row < 0.0 {
            return None;
        }
        let (column, row) = (column as usize, row as usize);
        if column >= self.columns || row >= self.rows {
            return None;
        }
        Some(f64::from(self.heights[row * self.columns + column]))
    }

    /// Lowers every cell whose centre lies within `radius` of `(x, y)` to
    /// `z`, never below the bottom.
    pub fn lower_disc(&mut self, x: f64, y: f64, radius: f64, z: f64) {
        let z = z.max(self.bottom) as f32;
        let column_low = (((x - radius) - self.min.x) / self.cell).floor().max(0.0) as usize;
        let column_high = (((x + radius) - self.min.x) / self.cell).ceil().max(0.0) as usize;
        let row_low = (((y - radius) - self.min.y) / self.cell).floor().max(0.0) as usize;
        let row_high = (((y + radius) - self.min.y) / self.cell).ceil().max(0.0) as usize;
        let radius_squared = radius * radius;
        for row in row_low..row_high.min(self.rows) {
            let cy = self.cell.mul_add(row as f64 + 0.5, self.min.y);
            for column in column_low..column_high.min(self.columns) {
                let cx = self.cell.mul_add(column as f64 + 0.5, self.min.x);
                let dx = cx - x;
                let dy = cy - y;
                if dx.mul_add(dx, dy * dy) <= radius_squared {
                    let index = row * self.columns + column;
                    if self.heights[index] > z {
                        self.heights[index] = z;
                    }
                }
            }
        }
    }

    /// The material left, in cubic millimetres.
    #[must_use]
    pub fn volume(&self) -> f64 {
        let area = self.cell * self.cell;
        self.heights
            .iter()
            .map(|height| (f64::from(*height) - self.bottom).max(0.0) * area)
            .sum()
    }

    /// The volume of one cell from bottom to top: the unit the volume gate
    /// is stated in.
    #[must_use]
    pub fn cell_volume(&self) -> f64 {
        self.cell * self.cell * (self.top - self.bottom)
    }

    /// How many cells have a neighbour at a different height, or lie on the
    /// stock's edge: where the model's error lives.
    #[must_use]
    pub fn boundary_cells(&self) -> usize {
        let mut count = 0;
        for row in 0..self.rows {
            for column in 0..self.columns {
                let here = self.heights[row * self.columns + column];
                let edge =
                    row == 0 || column == 0 || row + 1 == self.rows || column + 1 == self.columns;
                let differs =
                    |r: usize, c: usize| (self.heights[r * self.columns + c] - here).abs() > 1.0e-6;
                if edge
                    || differs(row - 1, column)
                    || differs(row + 1, column)
                    || differs(row, column - 1)
                    || differs(row, column + 1)
                {
                    count += 1;
                }
            }
        }
        count
    }
}

/// The volume a section loop sweeps about the `z` axis: `2π · ∫ r dA`, the
/// first moment about the axis, exact for lines and arcs by Green's theorem.
#[must_use]
pub fn loop_swept_volume(source: &PlanarLoop2) -> f64 {
    // ∫∫ x dA = ∮ (x²/2) dy over the boundary, counter-clockwise.
    let mut moment = 0.0;
    for curve in &geom::normalised(source).curves {
        match curve {
            artificer_protocol::PlanarCurve2::Line { start, end } => {
                // ∫ x²/2 dy along a line: parametrise by t.
                let (x0, y0, x1, y1) = (start.x, start.y, end.x, end.y);
                moment += (y1 - y0) * (x0 * x0 + x0 * x1 + x1 * x1) / 6.0;
            }
            artificer_protocol::PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let (radius, a0, sweep) = geom::arc_parameters(*center, *start, *end, *direction);
                // x = cx + R cos t, dy = R cos t dt.
                // ∫ (x²/2) R cos t dt from a0 to a0 + sweep.
                let a1 = a0 + sweep;
                let (cx, r) = (center.x, radius);
                let primitive = |t: f64| -> f64 {
                    // ∫ (cx + R cos t)² R cos t / 2 dt
                    // = R/2 ∫ (cx² cos t + 2 cx R cos² t + R² cos³ t) dt
                    let cos3 = t.sin() - t.sin().powi(3) / 3.0;
                    let cos2 = t / 2.0 + (2.0 * t).sin() / 4.0;
                    r / 2.0 * (cx * cx * t.sin() + 2.0 * cx * r * cos2 + r * r * cos3)
                };
                moment += primitive(a1) - primitive(a0);
            }
            _ => {}
        }
    }
    std::f64::consts::TAU * moment
}

/// Whether two closed loops describe the same region to `tolerance`: the
/// same area, and every vertex of one within `tolerance` of a vertex of the
/// other, once collinear vertices are dropped from both.
pub fn loops_agree(
    first: &PlanarLoop2,
    second: &PlanarLoop2,
    tolerance: f64,
) -> Result<(), String> {
    let area_first = geom::signed_area(first).abs();
    let area_second = geom::signed_area(second).abs();
    if (area_first - area_second).abs() > tolerance {
        return Err(format!(
            "areas differ: {area_first} vs {area_second} (by {})",
            (area_first - area_second).abs()
        ));
    }
    let first_vertices = essential_vertices(first, tolerance);
    let second_vertices = essential_vertices(second, tolerance);
    for (label, own, other) in [
        ("expected", &first_vertices, &second_vertices),
        ("actual", &second_vertices, &first_vertices),
    ] {
        for vertex in own {
            if !other
                .iter()
                .any(|candidate| geom::distance(*vertex, *candidate) <= tolerance)
            {
                return Err(format!(
                    "{label} vertex ({}, {}) has no counterpart",
                    vertex.x, vertex.y
                ));
            }
        }
    }
    Ok(())
}

/// The loop's vertices with collinear junctions between straight runs
/// removed, so a boundary split by a pass mid-line still names the same
/// corners.
#[must_use]
pub fn essential_vertices(source: &PlanarLoop2, tolerance: f64) -> Vec<Point2> {
    let curves = geom::normalised(source).curves;
    let count = curves.len();
    let mut kept = Vec::new();
    for index in 0..count {
        let previous = &curves[(index + count - 1) % count];
        let current = &curves[index];
        let vertex = geom::curve_start(current);
        let both_lines = matches!(previous, artificer_protocol::PlanarCurve2::Line { .. })
            && matches!(current, artificer_protocol::PlanarCurve2::Line { .. });
        if both_lines {
            let a = geom::curve_start(previous);
            let b = geom::curve_end(current);
            let cross = (vertex.x - a.x) * (b.y - vertex.y) - (vertex.y - a.y) * (b.x - vertex.x);
            let scale = geom::distance(a, vertex)
                .max(geom::distance(vertex, b))
                .max(1.0);
            if cross.abs() <= tolerance * scale {
                continue;
            }
        }
        kept.push(vertex);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bar_cut_by_a_facing_sweep_loses_its_front() {
        let mut bar = LatheStock::bar(10.0, -50.0, 1.0);
        let sweep = geom::rectangle(Point2::new(-0.5, 0.0), Point2::new(20.0, 8.0));
        bar.cut(&sweep).unwrap();
        assert!((bar.area() - 500.0).abs() < 1.0e-9, "{}", bar.area());
        let volume = bar.volume();
        assert!(
            (volume - std::f64::consts::PI * 100.0 * 50.0).abs() < 1.0e-6,
            "{volume}"
        );
    }

    #[test]
    fn a_drill_sweep_opens_the_axis() {
        // A symmetric drill polygon whose tip lands on the bar's axis edge.
        let mut bar = LatheStock::bar(10.0, -50.0, 0.0);
        let radius = 1.5;
        let cone = radius / 59.0_f64.to_radians().tan();
        let tip = -2.0;
        let sweep = geom::polygon(&[
            Point2::new(0.0, tip),
            Point2::new(radius, tip + cone),
            Point2::new(radius, 5.0),
            Point2::new(-radius, 5.0),
            Point2::new(-radius, tip + cone),
        ]);
        bar.cut(&sweep).unwrap();
        assert_eq!(bar.regions.len(), 1);
        assert!(!bar.contains(Point2::new(0.0, -1.0)), "{:?}", bar.regions);
        assert!(!bar.contains(Point2::new(1.0, -0.5)));
        assert!(bar.contains(Point2::new(0.0, -3.0)));
        assert!(bar.contains(Point2::new(2.0, -1.0)));
        let expected = 500.0 - (radius * (2.0 - cone) + 0.5 * radius * cone);
        assert!(
            (bar.area() - expected).abs() < 1.0e-9,
            "{} vs {expected}",
            bar.area()
        );
    }

    #[test]
    fn a_sweep_clear_of_the_bar_changes_nothing() {
        let mut bar = LatheStock::bar(10.0, -50.0, 1.0);
        let before = bar.clone();
        bar.cut(&geom::rectangle(
            Point2::new(12.0, -10.0),
            Point2::new(20.0, 0.0),
        ))
        .unwrap();
        assert_eq!(bar, before);
    }

    #[test]
    fn a_heightmap_loses_what_a_disc_lowers() {
        use artificer_protocol::Point3;
        let mut stock = MillStock::new(
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(20.0, 10.0, 5.0),
            0.5,
        );
        assert_eq!((stock.columns, stock.rows), (40, 20));
        assert!((stock.volume() - 1000.0).abs() < 1.0e-9);
        stock.lower_disc(10.0, 5.0, 3.0, 2.0);
        let removed = 1000.0 - stock.volume();
        // A Ø6 disc three deep, to within the cells on its rim.
        let expected = std::f64::consts::PI * 9.0 * 3.0;
        assert!(
            (removed - expected).abs() < 0.05 * expected,
            "{removed} vs {expected}"
        );
        assert_eq!(stock.height_at(10.0, 5.0), Some(2.0));
        assert_eq!(stock.height_at(1.0, 1.0), Some(5.0));
        assert_eq!(stock.height_at(-1.0, 1.0), None);
        assert!(stock.boundary_cells() > 0);
    }

    #[test]
    fn collinear_vertices_do_not_count() {
        let square = geom::rectangle(Point2::new(0.0, 0.0), Point2::new(4.0, 4.0));
        let split = geom::polygon(&[
            Point2::new(0.0, 0.0),
            Point2::new(2.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 4.0),
            Point2::new(0.0, 4.0),
        ]);
        assert!(loops_agree(&square, &split, 1.0e-9).is_ok());
        let dented = geom::polygon(&[
            Point2::new(0.0, 0.0),
            Point2::new(2.0, 0.1),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 4.0),
            Point2::new(0.0, 4.0),
        ]);
        assert!(loops_agree(&square, &dented, 1.0e-9).is_err());
    }
}

//! Closing a section on a face by the cells of its arrangement (ADR 0056,
//! Track B5).
//!
//! The section of the other solid on a face is bounded by the curves the
//! other solid's faces cut the carrier in. The older closure on a cylinder
//! reads those curves as graphs over the azimuth and counts crossings along
//! the window's edge generators, which needs every curve to be such a graph
//! and the section to be bounded by curves alone. Neither holds in general:
//! a bore through a torus leaves a closed loop that is no graph, and a single
//! ring across a sphere bounds nothing by itself — the face's own boundary
//! closes the section.
//!
//! So the face's region and every section piece — exact chords from the
//! matrix and B-spline arcs from the numerical rung alike — are cut at their
//! mutual crossings into a planar arrangement, whose half-edge cycles bound
//! its cells. Every cell lies wholly inside the other solid or wholly outside
//! it, since every section piece is a boundary of that solid; a probe of each
//! cell is classified against the other solid in space by the same ray cast
//! that classifies an untouched face, and the cells the operation keeps are
//! the face's pieces. No rule about the shape of a section is needed.

use artificer_protocol::PrecisionPolicy;

use crate::analytic_extrusion::{AnalyticLoop, Segment, point_inside_loop};
use crate::bspline::SplineCurve2;
use crate::profile_boolean::{point_in_loops, split_at_mutual_crossings, split_segment_at_points, weld_aligned, wrap_loops};
use crate::sew::NumericalPiece;
use crate::surface_marching::TracedCurve;
use crate::topology::{Point2, Surface, Topology};

/// One stretch of a traced curve on this face: its parameter trace on the
/// face's carrier, over `[from, to]` of the curve's own parameter.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NumericalArc {
    pub(crate) curve: usize,
    pub(crate) pcurve: SplineCurve2,
    pub(crate) from: f64,
    pub(crate) to: f64,
}

/// One boundary loop of a kept cell: exact segments, with a placeholder
/// line standing in for every numerical arc, and the arc itself beside it.
#[derive(Clone, Debug)]
pub(crate) struct CellLoop {
    pub(crate) segments: Vec<Segment>,
    pub(crate) numerical: Vec<Option<NumericalPiece>>,
}

/// A kept cell: its outer loop first, then its holes.
#[derive(Clone, Debug)]
pub(crate) struct Cell {
    pub(crate) loops: Vec<CellLoop>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CellError {
    /// A section piece ends inside the face: some face of the other solid
    /// did not report the piece that continues it.
    Unclosed,
    /// A contact the arrangement does not resolve, or a probe that no ray
    /// could classify.
    Unsupported,
}

/// An edge of the arrangement: an exact piece or a stretch of a traced curve.
#[derive(Clone, Copy, Debug)]
enum Arc {
    Exact(Segment),
    Numerical {
        curve: usize,
        pcurve: SplineCurve2,
        from: f64,
        to: f64,
        start: Point2,
        end: Point2,
    },
}

impl Arc {
    fn start(self) -> Point2 {
        match self {
            Self::Exact(segment) => segment.start(),
            Self::Numerical { start, .. } => start,
        }
    }

    fn end(self) -> Point2 {
        match self {
            Self::Exact(segment) => segment.end(),
            Self::Numerical { end, .. } => end,
        }
    }

    fn point_at(self, fraction: f64) -> Point2 {
        match self {
            Self::Exact(segment) => segment.point_at(fraction),
            Self::Numerical {
                pcurve,
                from,
                to,
                start,
                end,
                ..
            } => {
                if fraction <= 0.0 {
                    start
                } else if fraction >= 1.0 {
                    end
                } else {
                    pcurve.point((to - from).mul_add(fraction, from))
                }
            }
        }
    }

    fn reversed(self) -> Self {
        match self {
            Self::Exact(segment) => Self::Exact(segment.reversed()),
            Self::Numerical {
                curve,
                pcurve,
                from,
                to,
                start,
                end,
            } => Self::Numerical {
                curve,
                pcurve,
                from: to,
                to: from,
                start: end,
                end: start,
            },
        }
    }

    fn with_endpoints(self, start: Point2, end: Point2) -> Self {
        match self {
            Self::Exact(segment) => Self::Exact(segment.with_endpoints(start, end)),
            Self::Numerical {
                curve,
                pcurve,
                from,
                to,
                ..
            } => Self::Numerical {
                curve,
                pcurve,
                from,
                to,
                start,
                end,
            },
        }
    }

    /// The direction the arc sets off in from its start.
    fn leaving(self) -> Option<Point2> {
        let (dx, dy) = match self {
            Self::Exact(segment) => {
                let (from, to) = (segment.start(), segment.point_at(0.01));
                (to.x - from.x, to.y - from.y)
            }
            Self::Numerical {
                pcurve, from, to, ..
            } => {
                let rate = pcurve.tangent(from);
                let sign = if to >= from { 1.0 } else { -1.0 };
                (rate.x * sign, rate.y * sign)
            }
        };
        let length = dx.hypot(dy);
        (length > 0.0).then(|| Point2::new(dx / length, dy / length))
    }

    fn length(self) -> f64 {
        match self {
            Self::Exact(segment) => segment.length(),
            Self::Numerical { .. } => (0..32)
                .map(|step| {
                    let a = self.point_at(f64::from(step) / 32.0);
                    let b = self.point_at(f64::from(step + 1) / 32.0);
                    (b.x - a.x).hypot(b.y - a.y)
                })
                .sum(),
        }
    }

    fn signed_area_contribution(self) -> f64 {
        match self {
            Self::Exact(segment) => segment.signed_area_contribution(),
            // `½∮(x dy − y dx)`, a polynomial on every knot span.
            Self::Numerical {
                pcurve, from, to, ..
            } => pcurve.contour(from, to, Point2::new(0.0, 0.0))[0],
        }
    }

    /// The arc as chords, for containment tests away from its ends.
    fn polyline(self) -> Vec<Segment> {
        match self {
            Self::Exact(segment) => vec![segment],
            Self::Numerical { .. } => (0..64)
                .map(|step| Segment::Line {
                    start: self.point_at(f64::from(step) / 64.0),
                    end: self.point_at(f64::from(step + 1) / 64.0),
                })
                .collect(),
        }
    }
}

/// A window an arrangement is drawn in on a periodic face: the face's own
/// azimuth span, and its `v` span where that wraps too.
#[derive(Clone, Copy, Debug)]
struct Periodic {
    u: Option<(f64, f64)>,
    v: Option<(f64, f64)>,
}

fn periodicity(surface: Surface, region: &[Vec<Segment>]) -> Periodic {
    let revolved = crate::revolved::Revolved::of(surface);
    let (u, v) = region_window(region);
    Periodic {
        u: revolved.map(|_| u),
        v: revolved.filter(|revolved| revolved.v_periodic()).map(|_| v),
    }
}

/// The parameter box of a region's vertices.
fn region_window(region: &[Vec<Segment>]) -> ((f64, f64), (f64, f64)) {
    let mut u = (f64::INFINITY, f64::NEG_INFINITY);
    let mut v = (f64::INFINITY, f64::NEG_INFINITY);
    for point in region
        .iter()
        .flatten()
        .flat_map(|segment| [segment.start(), segment.end()])
    {
        u = (u.0.min(point.x), u.1.max(point.x));
        v = (v.0.min(point.y), v.1.max(point.y));
    }
    (u, v)
}

/// The parameters in `[from, to]` at which a coordinate of the pcurve
/// equals `level`, by sampling and bisection.
fn level_crossings(pcurve: SplineCurve2, from: f64, to: f64, axis: usize, level: f64) -> Vec<f64> {
    const SAMPLES: usize = 96;
    let coordinate = |t: f64| {
        let point = pcurve.point(t);
        if axis == 0 { point.x } else { point.y }
    };
    let mut roots = Vec::new();
    let at = |step: usize| (to - from).mul_add(step as f64 / SAMPLES as f64, from);
    let mut previous = (at(0), coordinate(at(0)) - level);
    for step in 1..=SAMPLES {
        let t = at(step);
        let value = coordinate(t) - level;
        if previous.1 == 0.0 {
            roots.push(previous.0);
        } else if (previous.1 < 0.0) != (value < 0.0) {
            let (mut low, mut high) = (previous.0, t);
            let low_negative = previous.1 < 0.0;
            for _ in 0..100 {
                let middle = 0.5 * (low + high);
                if (coordinate(middle) - level < 0.0) == low_negative {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            roots.push(0.5 * (low + high));
        }
        previous = (t, value);
    }
    roots
}

/// The sub-ranges of `[from, to]` whose coordinate lies strictly inside
/// `(low, high)`.
fn clip_range(
    pcurve: SplineCurve2,
    from: f64,
    to: f64,
    axis: usize,
    (low, high): (f64, f64),
) -> Vec<(f64, f64)> {
    let mut stops = vec![from];
    stops.extend(level_crossings(pcurve, from, to, axis, low));
    stops.extend(level_crossings(pcurve, from, to, axis, high));
    stops.push(to);
    stops.sort_by(f64::total_cmp);
    stops
        .windows(2)
        .filter(|pair| pair[1] - pair[0] > 1.0e-12)
        .filter(|pair| {
            let middle = pcurve.point(0.5 * (pair[0] + pair[1]));
            let value = if axis == 0 { middle.x } else { middle.y };
            value > low && value < high
        })
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

/// The range a coordinate of the pcurve covers over `[from, to]`, by sampling.
fn coordinate_range(pcurve: SplineCurve2, from: f64, to: f64, axis: usize) -> (f64, f64) {
    let mut range = (f64::INFINITY, f64::NEG_INFINITY);
    for step in 0..=64 {
        let point = pcurve.point((to - from).mul_add(f64::from(step) / 64.0, from));
        let value = if axis == 0 { point.x } else { point.y };
        range = (range.0.min(value), range.1.max(value));
    }
    range
}

/// Every copy of a numerical arc that reaches into a periodic face's window,
/// brought there by whole turns and cut to the window.
fn lift_numerical(arc: NumericalArc, periodic: Periodic, reach: f64) -> Vec<NumericalArc> {
    let tau = std::f64::consts::TAU;
    let mut copies = vec![arc];
    for (axis, window) in [(0, periodic.u), (1, periodic.v)] {
        let Some((low, high)) = window else {
            continue;
        };
        let (low, high) = (low - reach, high + reach);
        let mut lifted = Vec::new();
        for copy in copies {
            let (first, last) = coordinate_range(copy.pcurve, copy.from, copy.to, axis);
            let lowest = ((low - last) / tau).floor() as i64;
            let highest = ((high - first) / tau).ceil() as i64;
            for turns in lowest..=highest {
                let shift = turns as f64 * tau;
                let Some(shifted) = copy.pcurve.mapped(|point| {
                    if axis == 0 {
                        [point[0] + shift, point[1]]
                    } else {
                        [point[0], point[1] + shift]
                    }
                }) else {
                    continue;
                };
                for (from, to) in clip_range(shifted, copy.from, copy.to, axis, (low, high)) {
                    lifted.push(NumericalArc {
                        curve: copy.curve,
                        pcurve: shifted,
                        from,
                        to,
                    });
                }
            }
        }
        copies = lifted;
    }
    copies
}

/// The parameters at which a traced curve's trace on a carrier crosses a
/// face region's boundary, on any turn of a periodic carrier: where the
/// curve is cut so that both faces it separates cut it alike.
pub(crate) fn curve_region_crossings(
    pcurve: SplineCurve2,
    surface: Surface,
    region: &[Vec<Segment>],
) -> Vec<f64> {
    let periodic = periodicity(surface, region);
    let arc = NumericalArc {
        curve: 0,
        pcurve,
        from: 0.0,
        to: 1.0,
    };
    let mut cuts = Vec::new();
    for copy in lift_numerical(arc, periodic, 0.05) {
        for boundary in region.iter().flatten() {
            for (t, _) in arc_segment_crossings(copy, *boundary) {
                cuts.push(t);
            }
        }
    }
    cuts
}

/// A signed measure of which side of a segment's carrier a point lies on:
/// exact for a line and an arc, by the nearest chord otherwise.
fn side_of(segment: Segment, point: Point2) -> f64 {
    match segment {
        Segment::Line { start, end } => {
            let (dx, dy) = (end.x - start.x, end.y - start.y);
            let length = dx.hypot(dy);
            if length <= 0.0 {
                return 0.0;
            }
            ((point.x - start.x) * dy - (point.y - start.y) * dx) / length
        }
        Segment::Arc { center, radius, .. } => {
            (point.x - center.x).hypot(point.y - center.y) - radius
        }
        _ => {
            let mut best = (f64::INFINITY, 0.0);
            for step in 0..64 {
                let a = segment.point_at(f64::from(step) / 64.0);
                let b = segment.point_at(f64::from(step + 1) / 64.0);
                let chord = Segment::Line { start: a, end: b };
                let distance = chord_distance(chord, point);
                if distance < best.0 {
                    best = (distance, side_of(chord, point));
                }
            }
            best.1
        }
    }
}

fn chord_distance(chord: Segment, point: Point2) -> f64 {
    let Segment::Line { start, end } = chord else {
        return f64::INFINITY;
    };
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let square = dx.mul_add(dx, dy * dy);
    let along = if square > 0.0 {
        ((point.x - start.x).mul_add(dx, (point.y - start.y) * dy) / square).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let foot = Point2::new(dx.mul_add(along, start.x), dy.mul_add(along, start.y));
    (point.x - foot.x).hypot(point.y - foot.y)
}

/// Distance from a point to a segment, within its span.
fn segment_distance(segment: Segment, point: Point2) -> f64 {
    match segment {
        Segment::Line { .. } => chord_distance(segment, point),
        Segment::Arc {
            center,
            radius,
            start_angle,
            sweep,
            start,
            end,
        } => {
            let angle = (point.y - center.y).atan2(point.x - center.x);
            let progress = if sweep >= 0.0 {
                (angle - start_angle).rem_euclid(std::f64::consts::TAU) / sweep
            } else {
                (start_angle - angle).rem_euclid(std::f64::consts::TAU) / -sweep
            };
            if (0.0..=1.0).contains(&progress) {
                ((point.x - center.x).hypot(point.y - center.y) - radius).abs()
            } else {
                (point.x - start.x)
                    .hypot(point.y - start.y)
                    .min((point.x - end.x).hypot(point.y - end.y))
            }
        }
        _ => (0..64)
            .map(|step| {
                let a = segment.point_at(f64::from(step) / 64.0);
                let b = segment.point_at(f64::from(step + 1) / 64.0);
                chord_distance(Segment::Line { start: a, end: b }, point)
            })
            .fold(f64::INFINITY, f64::min),
    }
}

/// Where a numerical arc crosses a segment: the arc's parameters, with the
/// crossing points on the arc, found by sampling the arc against the
/// segment's carrier and bisecting, then held to the segment's own span.
fn arc_segment_crossings(arc: NumericalArc, segment: Segment) -> Vec<(f64, Point2)> {
    const SAMPLES: usize = 96;
    let at = |step: usize| (arc.to - arc.from).mul_add(step as f64 / SAMPLES as f64, arc.from);
    let value = |t: f64| side_of(segment, arc.pcurve.point(t));
    let scale = [segment.start(), segment.end()]
        .into_iter()
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let mut crossings = Vec::new();
    let mut previous = (at(0), value(at(0)));
    for step in 1..=SAMPLES {
        let t = at(step);
        let here = value(t);
        if (previous.1 < 0.0) != (here < 0.0) && previous.1 != 0.0 {
            let (mut low, mut high) = (previous.0, t);
            let low_negative = previous.1 < 0.0;
            for _ in 0..100 {
                let middle = 0.5 * (low + high);
                if (value(middle) < 0.0) == low_negative {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            let root = 0.5 * (low + high);
            let point = arc.pcurve.point(root);
            if segment_distance(segment, point) <= 1.0e-8 * scale {
                crossings.push((root, point));
            }
        }
        previous = (t, here);
    }
    crossings
}

/// The face-boundary walk over the arrangement: every halfedge in exactly
/// one cycle, each cycle keeping the cell it bounds on its left, by the
/// fixed successor rule of [`crate::analytic_boolean`]'s walk.
fn halfedge_cycles(arcs: &[Arc]) -> Vec<Vec<usize>> {
    let count = arcs.len();
    let oriented = |halfedge: usize| {
        if halfedge.is_multiple_of(2) {
            arcs[halfedge / 2]
        } else {
            arcs[halfedge / 2].reversed()
        }
    };
    let key = |point: Point2| (point.x.to_bits(), point.y.to_bits());
    let mut outgoing: std::collections::BTreeMap<(u64, u64), Vec<usize>> =
        std::collections::BTreeMap::new();
    for halfedge in 0..count * 2 {
        outgoing
            .entry(key(oriented(halfedge).start()))
            .or_default()
            .push(halfedge);
    }
    let successor = |halfedge: usize| -> Option<usize> {
        let back = halfedge ^ 1;
        let branches = outgoing.get(&key(oriented(back).start()))?;
        let reference = oriented(back).leaving()?;
        branches.iter().copied().min_by(|left, right| {
            let turn = |branch: &usize| {
                if *branch == back {
                    return std::f64::consts::TAU;
                }
                let Some(out) = oriented(*branch).leaving() else {
                    return f64::INFINITY;
                };
                let angle = (reference.x * out.y - reference.y * out.x)
                    .atan2(reference.x * out.x + reference.y * out.y);
                if angle >= -1.0e-12 {
                    std::f64::consts::TAU - angle
                } else {
                    -angle
                }
            };
            turn(left).total_cmp(&turn(right))
        })
    };
    let mut visited = vec![false; count * 2];
    let mut cycles = Vec::new();
    for start in 0..count * 2 {
        if visited[start] {
            continue;
        }
        let mut cycle = Vec::new();
        let mut cursor = start;
        for _ in 0..count * 2 {
            visited[cursor] = true;
            cycle.push(cursor);
            let Some(next) = successor(cursor) else { break };
            if next == start || visited[next] {
                break;
            }
            cursor = next;
        }
        cycles.push(cycle);
    }
    cycles
}

/// Closes the section on one face into the cells the operation keeps.
///
/// `region` is the face's own region (outer loop first, welded); `exact` the
/// section pieces the matrix gave, on any turn; `numerical` the stretches of
/// traced curves that lie inside the other solid's faces; `curves` the
/// traced curves they index. `keep_inside` keeps the cells inside the other
/// solid, else those outside it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn close_by_cells(
    surface: Surface,
    region: &[Vec<Segment>],
    exact: &[Segment],
    numerical: &[NumericalArc],
    curves: &[TracedCurve],
    other: &Topology,
    keep_inside: bool,
    precision: PrecisionPolicy,
) -> Result<Vec<Cell>, CellError> {
    let periodic = periodicity(surface, region);
    let reach = 0.05;
    let scale = region
        .iter()
        .flatten()
        .flat_map(|segment| [segment.start(), segment.end()])
        .map(|point| point.x.abs().max(point.y.abs()))
        .fold(1.0_f64, f64::max);
    let weld = precision.linear_agreement.max(1.0e-12) * scale * 32.0;

    // Exact pieces onto the window, cut among themselves. A piece running
    // along the face's own boundary is that boundary, which already parts
    // the cells; it is dropped, and what it means is left to the probe.
    let mut segments: Vec<Segment> = region.iter().flatten().copied().collect();
    let lifted = if periodic.u.is_some() {
        crate::analytic_boolean::lift_section_pieces(exact.to_vec(), region, precision)
            .map_err(|_| CellError::Unsupported)?
    } else {
        exact.to_vec()
    };
    for piece in lifted {
        segments.extend(off_boundary(piece, region, weld, precision)?);
    }

    // Numerical arcs onto the window; their ends that land on the boundary
    // cut it, as the crossings they were cut at.
    let mut arcs: Vec<NumericalArc> = Vec::new();
    for arc in numerical {
        arcs.extend(lift_numerical(*arc, periodic, reach));
    }
    let mut boundary_cuts: Vec<Vec<Point2>> = vec![Vec::new(); segments.len()];
    for arc in &arcs {
        for point in [arc.pcurve.point(arc.from), arc.pcurve.point(arc.to)] {
            for (index, segment) in segments.iter().enumerate() {
                if segment_distance(*segment, point) <= weld {
                    boundary_cuts[index].push(point);
                }
            }
        }
    }
    let mut cut: Vec<Segment> = Vec::with_capacity(segments.len());
    for (segment, cuts) in segments.iter().zip(&boundary_cuts) {
        if cuts.is_empty() {
            cut.push(*segment);
        } else {
            cut.extend(
                split_segment_at_points(*segment, cuts, precision)
                    .map_err(|_| CellError::Unsupported)?,
            );
        }
    }
    let split = split_at_mutual_crossings(&cut, precision).map_err(|_| CellError::Unsupported)?;
    let split = weld_aligned(split, weld);

    // Every arc of the arrangement, ends welded to one point per cluster.
    let mut all: Vec<Arc> = split.into_iter().map(Arc::Exact).collect();
    for arc in arcs {
        all.push(Arc::Numerical {
            curve: arc.curve,
            pcurve: arc.pcurve,
            from: arc.from,
            to: arc.to,
            start: arc.pcurve.point(arc.from),
            end: arc.pcurve.point(arc.to),
        });
    }
    let mut seeds: Vec<Point2> = Vec::new();
    let mut cluster = |point: Point2| -> Point2 {
        if let Some(found) = seeds
            .iter()
            .find(|seed| (seed.x - point.x).hypot(seed.y - point.y) <= weld)
        {
            return *found;
        }
        seeds.push(point);
        point
    };
    // Exact ends first, so a numerical end adopts the exact vertex it lands
    // on and straight pieces keep their own coordinates.
    for arc in &all {
        if let Arc::Exact(_) = arc {
            cluster(arc.start());
            cluster(arc.end());
        }
    }
    let all: Vec<Arc> = all
        .into_iter()
        .map(|arc| {
            let (start, end) = (cluster(arc.start()), cluster(arc.end()));
            arc.with_endpoints(start, end)
        })
        .collect();

    // Section pieces outside the region bound nothing of the face.
    let wrapped = wrap_loops(region);
    let kept: Vec<Arc> = all
        .into_iter()
        .filter(|arc| {
            let middle = arc.point_at(0.5);
            let on_boundary = region
                .iter()
                .flatten()
                .any(|segment| segment_distance(*segment, middle) <= weld);
            on_boundary || point_in_loops(middle, &wrapped)
        })
        .collect();
    if kept.is_empty() {
        return Err(CellError::Unsupported);
    }

    // The cells: every positive cycle inside the region, each classified in
    // space; negative cycles are holes of the cell that contains them.
    let mut outers: Vec<(Vec<Arc>, bool)> = Vec::new();
    let mut holes: Vec<(Vec<Arc>, Point2)> = Vec::new();
    for cycle in halfedge_cycles(&kept) {
        let length = cycle.len();
        let dangling =
            (0..length).any(|position| cycle[position] == cycle[(position + 1) % length] ^ 1);
        let arcs: Vec<Arc> = cycle
            .into_iter()
            .map(|halfedge| {
                if halfedge.is_multiple_of(2) {
                    kept[halfedge / 2]
                } else {
                    kept[halfedge / 2].reversed()
                }
            })
            .collect();
        let Some(probe) = probe_left(&arcs) else {
            continue;
        };
        if !point_in_loops(probe, &wrapped) {
            continue;
        }
        if dangling {
            return Err(CellError::Unclosed);
        }
        let area: f64 = arcs.iter().map(|arc| arc.signed_area_contribution()).sum();
        if area > 0.0 {
            let inside = crate::analytic_boolean::point_in_solid(other, surface.evaluate(probe))
                .ok_or(CellError::Unsupported)?;
            outers.push((arcs, inside));
        } else {
            holes.push((arcs, probe));
        }
    }
    let mut cells: Vec<(Vec<Arc>, Vec<Vec<Arc>>, bool)> = outers
        .into_iter()
        .map(|(outer, inside)| (outer, Vec::new(), inside))
        .collect();
    for (hole, probe) in holes {
        let owner = cells
            .iter_mut()
            .filter(|(outer, _, _)| inside_cycle(probe, outer))
            .min_by(|left, right| {
                let area = |outer: &[Arc]| -> f64 {
                    outer.iter().map(|arc| arc.signed_area_contribution()).sum()
                };
                area(&left.0).total_cmp(&area(&right.0))
            });
        let Some((_, holes, _)) = owner else {
            return Err(CellError::Unclosed);
        };
        holes.push(hole);
    }
    Ok(cells
        .into_iter()
        .filter(|(_, _, inside)| *inside == keep_inside)
        .map(|(outer, holes, _)| Cell {
            loops: std::iter::once(&outer)
                .chain(&holes)
                .map(|arcs| cell_loop(arcs, curves))
                .collect(),
        })
        .collect())
}

/// The parts of a section piece that do not run along the region's
/// boundary: the piece cut at the ends of every stretch it shares with a
/// boundary segment, with the shared stretches left out.
fn off_boundary(
    piece: Segment,
    region: &[Vec<Segment>],
    weld: f64,
    precision: PrecisionPolicy,
) -> Result<Vec<Segment>, CellError> {
    let mut cuts = Vec::new();
    for boundary in region.iter().flatten() {
        if let Some(ends) = crate::profile_boolean::overlap_ends(piece, *boundary, precision) {
            cuts.extend(ends);
        }
    }
    if cuts.is_empty() {
        return Ok(vec![piece]);
    }
    let pieces =
        split_segment_at_points(piece, &cuts, precision).map_err(|_| CellError::Unsupported)?;
    Ok(pieces
        .into_iter()
        .filter(|part| {
            let middle = part.point_at(0.5);
            !region
                .iter()
                .flatten()
                .any(|boundary| segment_distance(*boundary, middle) <= weld)
        })
        .collect())
}

/// A point just left of the middle of a cycle's longest arc: inside the
/// cell it bounds, far from every vertex.
fn probe_left(cycle: &[Arc]) -> Option<Point2> {
    let piece = cycle
        .iter()
        .copied()
        .max_by(|left, right| left.length().total_cmp(&right.length()))?;
    let middle = piece.point_at(0.5);
    let (ahead, behind) = (piece.point_at(0.5 + 1.0e-3), piece.point_at(0.5 - 1.0e-3));
    let (dx, dy) = (ahead.x - behind.x, ahead.y - behind.y);
    let length = dx.hypot(dy);
    if length <= 0.0 {
        return None;
    }
    let reach = 1.0e-7 * (1.0 + middle.x.abs() + middle.y.abs());
    Some(Point2::new(
        (-dy / length).mul_add(reach, middle.x),
        (dx / length).mul_add(reach, middle.y),
    ))
}

fn inside_cycle(point: Point2, cycle: &[Arc]) -> bool {
    let segments: Vec<Segment> = cycle.iter().flat_map(|arc| arc.polyline()).collect();
    point_inside_loop(
        point,
        &AnalyticLoop {
            segments,
            signed_area: 0.0,
        },
    )
}

fn cell_loop(arcs: &[Arc], curves: &[TracedCurve]) -> CellLoop {
    let mut segments = Vec::with_capacity(arcs.len());
    let mut numerical = Vec::with_capacity(arcs.len());
    for arc in arcs {
        match *arc {
            Arc::Exact(segment) => {
                segments.push(segment);
                numerical.push(None);
            }
            Arc::Numerical {
                curve,
                pcurve,
                from,
                to,
                start,
                end,
            } => {
                segments.push(Segment::Line { start, end });
                numerical.push(Some(NumericalPiece {
                    curve: curves[curve].curve,
                    pcurve,
                    from,
                    to,
                }));
            }
        }
    }
    CellLoop {
        segments,
        numerical,
    }
}

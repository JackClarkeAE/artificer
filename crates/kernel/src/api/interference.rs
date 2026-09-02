//! Clearance and interference between two bodies.
//!
//! Everything a fit check needs is a distance question: how close do two
//! solids come, where, and does either reach inside the other. None of it
//! is a Boolean. The overlap *volume* of two interfering parts is, and the
//! probe that answers it lives elsewhere; this module answers the rest, so
//! a clearance study runs on bodies the Boolean engine would refuse.
//!
//! The work happens over each body's display facets, gathered into a
//! bounding-volume hierarchy so a pair costs a descent rather than the
//! product of two facet counts. A representative part here tessellates to
//! several thousand facets, and the product of two such bodies is tens of
//! millions of triangle pairs: the hierarchy is what makes the answer
//! arrive rather than an optimisation on top of one that already did.
//!
//! ## What the answer is worth
//!
//! Facets are chords of the surfaces they stand for, and two surfaces that
//! bulge towards one another are closer than their chords are. A facet
//! clearance therefore *over-reports* the gap, which is the direction that
//! matters for a fit. So the report carries a bound: the chord budget the
//! tessellation was built to, once per curved body. Between two bodies of
//! planar faces there is no chord and no bound, and the answer is exact.

use artificer_protocol::{Aabb3, Point3, PrecisionPolicy, Tier, Vector3};
use serde::{Deserialize, Serialize};

use crate::{DebugScene, NativeKernel, Snapshot};

/// The rigid placement of a body in the world an interference study is run
/// in. Assembly occurrences carry one; two bodies of the same session share
/// the identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Column-major rotation: `columns[i]` is the image of basis vector `i`.
    pub columns: [[f64; 3]; 3],
    pub translation: [f64; 3],
}

impl Default for Placement {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Placement {
    pub const IDENTITY: Self = Self {
        columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translation: [0.0, 0.0, 0.0],
    };

    /// A placement from a unit quaternion and a translation, which is the
    /// shape an assembly occurrence stores.
    #[must_use]
    pub fn from_quaternion(rotation: [f64; 4], translation: [f64; 3]) -> Option<Self> {
        let [w, x, y, z] = rotation;
        let norm = (w * w + x * x + y * y + z * z).sqrt();
        if !norm.is_finite() || norm <= f64::EPSILON {
            return None;
        }
        let (w, x, y, z) = (w / norm, x / norm, y / norm, z / norm);
        Some(Self {
            columns: [
                [
                    1.0 - 2.0 * (y * y + z * z),
                    2.0 * (x * y + z * w),
                    2.0 * (x * z - y * w),
                ],
                [
                    2.0 * (x * y - z * w),
                    1.0 - 2.0 * (x * x + z * z),
                    2.0 * (y * z + x * w),
                ],
                [
                    2.0 * (x * z + y * w),
                    2.0 * (y * z - x * w),
                    1.0 - 2.0 * (x * x + y * y),
                ],
            ],
            translation,
        })
    }

    /// The same rigid motion as a protocol similarity, for the commands
    /// that take one. A rotation matrix goes back to a quaternion by
    /// Shepperd's method: the largest of the four components is recovered
    /// from the trace first, so the division is never by a small number.
    #[must_use]
    pub fn to_similarity(self) -> Option<artificer_protocol::SimilarityTransform3> {
        let m = self.columns;
        // `m[column][row]`, so the trace is the three diagonal entries.
        let (m00, m11, m22) = (m[0][0], m[1][1], m[2][2]);
        let trace = m00 + m11 + m22;
        let (w, x, y, z) = if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0;
            (
                0.25 * s,
                (m[1][2] - m[2][1]) / s,
                (m[2][0] - m[0][2]) / s,
                (m[0][1] - m[1][0]) / s,
            )
        } else if m00 > m11 && m00 > m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
            (
                (m[1][2] - m[2][1]) / s,
                0.25 * s,
                (m[1][0] + m[0][1]) / s,
                (m[2][0] + m[0][2]) / s,
            )
        } else if m11 > m22 {
            let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
            (
                (m[2][0] - m[0][2]) / s,
                (m[1][0] + m[0][1]) / s,
                0.25 * s,
                (m[2][1] + m[1][2]) / s,
            )
        } else {
            let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
            (
                (m[0][1] - m[1][0]) / s,
                (m[2][0] + m[0][2]) / s,
                (m[2][1] + m[1][2]) / s,
                0.25 * s,
            )
        };
        let quaternion = artificer_protocol::RotationQuaternion::new(w, x, y, z);
        quaternion
            .is_finite()
            .then_some(artificer_protocol::SimilarityTransform3 {
                translation: artificer_protocol::Vector3::new(
                    self.translation[0],
                    self.translation[1],
                    self.translation[2],
                ),
                rotation: quaternion,
                uniform_scale: 1.0,
            })
    }

    fn apply(self, point: Point3) -> Point3 {
        let [cx, cy, cz] = self.columns;
        Point3::new(
            point
                .x
                .mul_add(cx[0], point.y.mul_add(cy[0], point.z * cz[0]))
                + self.translation[0],
            point
                .x
                .mul_add(cx[1], point.y.mul_add(cy[1], point.z * cz[1]))
                + self.translation[1],
            point
                .x
                .mul_add(cx[2], point.y.mul_add(cy[2], point.z * cz[2]))
                + self.translation[2],
        )
    }
}

/// How two bodies stand relative to one another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearanceState {
    /// The bodies are apart. `distance` is the gap.
    Clear,
    /// The surfaces meet without either reaching inside the other.
    Touching,
    /// One body reaches inside the other.
    Interfering,
}

impl ClearanceState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Touching => "touching",
            Self::Interfering => "interfering",
        }
    }
}

/// What a pair of bodies came back with.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClearanceReport {
    pub state: ClearanceState,
    /// The closest approach of the two surfaces, in millimetres.
    ///
    /// Zero when the surfaces cross or meet. A body wholly inside another
    /// keeps a positive distance: the gap to the wall around it, which is
    /// the number a fit is judged by, with `state` saying it is inside.
    pub distance: f64,
    /// Where on each body the closest approach is.
    pub witness_a: Point3,
    pub witness_b: Point3,
    pub tier: Tier,
    /// How far the true clearance may sit below `distance`, from the chord
    /// budget each curved body was tessellated to. Zero when both bodies
    /// are planar and the answer is exact.
    pub bound: f64,
}

/// One body's facets in world coordinates, in a bounding-volume hierarchy.
///
/// The facets are placed at build time rather than transformed during the
/// descent: an axis-aligned box under rotation is no longer axis-aligned,
/// and a hierarchy that has to account for that is a much larger thing to
/// get right than a rebuild is to pay for.
#[derive(Clone, Debug)]
pub struct FacetIndex {
    facets: Vec<[Point3; 3]>,
    nodes: Vec<Node>,
    /// Whether every face of the body is planar, so its facets are the
    /// surface rather than a chord of it.
    exact: bool,
    /// The chord budget the facets were built to.
    chord_budget: f64,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    bounds: Aabb3,
    /// Facet range for a leaf; `count == 0` marks an interior node, whose
    /// two children are named outright. Deriving the second child from the
    /// first would mean walking its subtree, which turns every descent
    /// quadratic in the size of the tree it is descending.
    start: usize,
    count: usize,
    left: usize,
    right: usize,
}

const LEAF_FACETS: usize = 8;

impl FacetIndex {
    /// Builds the index for a snapshot at a placement.
    #[must_use]
    pub fn build(snapshot: &Snapshot, placement: Placement) -> Self {
        let scene = NativeKernel::debug_scene(snapshot);
        let precision = snapshot.precision_policy().unwrap_or_default();
        Self::from_scene(
            &scene,
            placement,
            NativeKernel::is_polyhedral(snapshot),
            chord_budget(precision),
        )
    }

    /// Builds the index from a scene the caller already has, saying whether
    /// that body is planar throughout and what chord budget its facets were
    /// built to.
    #[must_use]
    pub fn from_scene(
        scene: &DebugScene,
        placement: Placement,
        exact: bool,
        chord_budget: f64,
    ) -> Self {
        let facets = scene
            .triangles
            .iter()
            .map(|triangle| triangle.vertices.map(|point| placement.apply(point)))
            .filter(|facet| facet.iter().all(|point| point.is_finite()))
            .collect::<Vec<_>>();
        let mut index = Self {
            facets,
            nodes: Vec::new(),
            exact,
            chord_budget,
        };
        if !index.facets.is_empty() {
            let count = index.facets.len();
            index.split(0, count);
        }
        index
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.facets.is_empty()
    }

    #[must_use]
    pub fn facet_count(&self) -> usize {
        self.facets.len()
    }

    /// The world bounds of every facet.
    #[must_use]
    pub fn bounds(&self) -> Option<Aabb3> {
        self.nodes.first().map(|node| node.bounds)
    }

    /// Builds one node over `facets[start..start + count]`, splitting until
    /// a leaf is small enough, and returns its index.
    fn split(&mut self, start: usize, count: usize) -> usize {
        let bounds = bounds_of(&self.facets[start..start + count]);
        let node = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            start,
            count,
            left: 0,
            right: 0,
        });
        if count <= LEAF_FACETS {
            return node;
        }
        // The longest axis, split at the median centroid: cheap to build and
        // good enough for facet sets that come from a tessellator rather
        // than from an adversary.
        let extents = [
            bounds.max.x - bounds.min.x,
            bounds.max.y - bounds.min.y,
            bounds.max.z - bounds.min.z,
        ];
        let axis = extents
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map_or(0, |(axis, _)| axis);
        let slice = &mut self.facets[start..start + count];
        slice.sort_by(|left, right| {
            centroid_axis(left, axis).total_cmp(&centroid_axis(right, axis))
        });
        let half = count / 2;
        let left = self.split(start, half);
        let right = self.split(start + half, count - half);
        self.nodes[node].count = 0;
        self.nodes[node].left = left;
        self.nodes[node].right = right;
        node
    }

    /// The distance from a point to the nearest facet, by descent.
    ///
    /// A point on the boundary is what separates touching from
    /// interfering, and ray parity cannot tell: a ray leaving a surface
    /// point inward counts an odd number of crossings ahead of it and
    /// reports the point as inside. Measuring the surface first settles
    /// that case before parity is consulted at all.
    #[must_use]
    pub fn distance_to_surface(&self, point: Point3) -> f64 {
        if self.nodes.is_empty() {
            return f64::INFINITY;
        }
        let mut best = f64::INFINITY;
        let mut stack = vec![0_usize];
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            if point_box_distance(point, node.bounds) >= best {
                continue;
            }
            if node.count == 0 {
                stack.push(node.left);
                stack.push(node.right);
                continue;
            }
            for facet in &self.facets[node.start..node.start + node.count] {
                let candidate = squared_distance(point, closest_point_on_triangle(point, facet));
                if candidate < best {
                    best = candidate;
                }
            }
        }
        best.sqrt()
    }

    /// Whether a point is inside the body and clear of its surface by more
    /// than `tolerance`.
    #[must_use]
    pub fn strictly_contains(&self, point: Point3, tolerance: f64) -> bool {
        self.distance_to_surface(point) > tolerance && self.contains(point)
    }

    /// Whether a point lies inside the body, by ray parity through the
    /// hierarchy.
    #[must_use]
    pub fn contains(&self, point: Point3) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        // The same off-axis direction the containment probe uses: a ray no
        // facet of an axis-aligned body is parallel to.
        let direction = Vector3::new(0.507_3, 0.331_9, 0.795_4);
        let mut crossings = 0_u32;
        let mut stack = vec![0_usize];
        while let Some(node) = stack.pop() {
            let node = self.nodes[node];
            if !ray_hits_box(point, direction, node.bounds) {
                continue;
            }
            if node.count == 0 {
                stack.push(node.left);
                stack.push(node.right);
                continue;
            }
            for facet in &self.facets[node.start..node.start + node.count] {
                if ray_triangle(point, direction, facet).is_some_and(|hit| hit > 0.0) {
                    crossings += 1;
                }
            }
        }
        crossings % 2 == 1
    }
}

/// The chord budget a snapshot's authoritative tessellation was built to.
fn chord_budget(precision: PrecisionPolicy) -> f64 {
    precision
        .approximation_budget
        .max(precision.modeling_resolution)
}

/// The closest approach of two bodies, and what it means.
///
/// The descent prunes on box distance, so the pair that ends up compared
/// facet by facet is the one that could hold the minimum. When the surfaces
/// meet, the two bodies are separated further: touching is not interfering,
/// and a fit check has to tell them apart.
#[must_use]
pub fn clearance(a: &FacetIndex, b: &FacetIndex, precision: PrecisionPolicy) -> ClearanceReport {
    let bound =
        if a.exact { 0.0 } else { a.chord_budget } + if b.exact { 0.0 } else { b.chord_budget };
    let tier = if a.exact && b.exact {
        Tier::Exact
    } else {
        Tier::Approximate
    };
    let mut best = Best {
        distance: f64::INFINITY,
        witness_a: Point3::new(0.0, 0.0, 0.0),
        witness_b: Point3::new(0.0, 0.0, 0.0),
    };
    if !a.is_empty() && !b.is_empty() {
        descend(a, 0, b, 0, &mut best);
    }
    let touching = precision.linear_agreement.max(1.0e-9);
    // A body wholly inside another never brings its surfaces close to the
    // other's, so containment cannot wait on the surface distance. It can
    // wait on the bounds: bodies whose boxes are apart cannot contain one
    // another, and that is the case worth making free.
    let boxes_meet = match (a.bounds(), b.bounds()) {
        (Some(left), Some(right)) => box_distance(left, right) <= touching,
        _ => false,
    };
    let state = if boxes_meet && (reaches_inside(a, b, touching) || reaches_inside(b, a, touching))
    {
        ClearanceState::Interfering
    } else if best.distance <= touching {
        ClearanceState::Touching
    } else {
        ClearanceState::Clear
    };
    ClearanceReport {
        state,
        distance: if best.distance.is_finite() {
            best.distance.max(0.0)
        } else {
            f64::INFINITY
        },
        witness_a: best.witness_a,
        witness_b: best.witness_b,
        tier,
        bound,
    }
}

/// Whether any vertex of `inner`'s facets lies inside `outer`.
///
/// One vertex is enough: a solid that merely touches another has its whole
/// boundary on or outside it, so the first vertex found inside settles the
/// question and the walk stops there.
fn reaches_inside(inner: &FacetIndex, outer: &FacetIndex, agreement: f64) -> bool {
    let Some(bounds) = outer.bounds() else {
        return false;
    };
    // A point on the shared boundary of two touching bodies is not inside
    // either of them, so the surface clearance is checked before parity.
    // A curved body's facets sit a chord below its true surface, which is
    // why the outer body's chord budget joins the tolerance.
    let tolerance = agreement + outer.chord_budget;
    let inside =
        |point: Point3| inside_bounds(point, bounds) && outer.strictly_contains(point, tolerance);
    // Vertices alone are not enough. Two boxes that overlap over a slab can
    // have every vertex of each lying on a face of the other, so the facet
    // centres are tested too: they are interior to their own facet, and one
    // of them lands in the overlap whenever the boundaries genuinely cross.
    for facet in &inner.facets {
        if inside(facet_centre(facet)) || facet.iter().any(|point| inside(*point)) {
            return true;
        }
    }
    // A body wholly coincident with another has its whole boundary on that
    // boundary, and only a point off the surface settles it.
    inner.bounds().map(box_centre).is_some_and(inside)
}

fn facet_centre(facet: &[Point3; 3]) -> Point3 {
    Point3::new(
        (facet[0].x + facet[1].x + facet[2].x) / 3.0,
        (facet[0].y + facet[1].y + facet[2].y) / 3.0,
        (facet[0].z + facet[1].z + facet[2].z) / 3.0,
    )
}

fn box_centre(bounds: Aabb3) -> Point3 {
    Point3::new(
        f64::midpoint(bounds.min.x, bounds.max.x),
        f64::midpoint(bounds.min.y, bounds.max.y),
        f64::midpoint(bounds.min.z, bounds.max.z),
    )
}

fn inside_bounds(point: Point3, bounds: Aabb3) -> bool {
    point.x >= bounds.min.x
        && point.x <= bounds.max.x
        && point.y >= bounds.min.y
        && point.y <= bounds.max.y
        && point.z >= bounds.min.z
        && point.z <= bounds.max.z
}

struct Best {
    distance: f64,
    witness_a: Point3,
    witness_b: Point3,
}

fn descend(a: &FacetIndex, ai: usize, b: &FacetIndex, bi: usize, best: &mut Best) {
    let (left, right) = (a.nodes[ai], b.nodes[bi]);
    if box_distance(left.bounds, right.bounds) >= best.distance {
        return;
    }
    match (left.count == 0, right.count == 0) {
        (false, false) => {
            for first in &a.facets[left.start..left.start + left.count] {
                for second in &b.facets[right.start..right.start + right.count] {
                    let (point_a, point_b, distance) = closest_points_on_triangles(first, second);
                    if distance < best.distance {
                        best.distance = distance;
                        best.witness_a = point_a;
                        best.witness_b = point_b;
                    }
                }
            }
        }
        // Descend the side with the larger box, which is what keeps the
        // hierarchy balanced against a big body meeting a small one.
        (true, false) => {
            for child in children(&a.nodes, ai) {
                descend(a, child, b, bi, best);
            }
        }
        (false, true) => {
            for child in children(&b.nodes, bi) {
                descend(a, ai, b, child, best);
            }
        }
        (true, true) => {
            if box_extent(left.bounds) >= box_extent(right.bounds) {
                for child in children(&a.nodes, ai) {
                    descend(a, child, b, bi, best);
                }
            } else {
                for child in children(&b.nodes, bi) {
                    descend(a, ai, b, child, best);
                }
            }
        }
    }
}

const fn children(nodes: &[Node], node: usize) -> [usize; 2] {
    [nodes[node].left, nodes[node].right]
}

fn bounds_of(facets: &[[Point3; 3]]) -> Aabb3 {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for facet in facets {
        for point in facet {
            for (axis, value) in [point.x, point.y, point.z].into_iter().enumerate() {
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
        }
    }
    Aabb3::new(
        Point3::new(min[0], min[1], min[2]),
        Point3::new(max[0], max[1], max[2]),
    )
}

fn centroid_axis(facet: &[Point3; 3], axis: usize) -> f64 {
    facet
        .iter()
        .map(|point| [point.x, point.y, point.z][axis])
        .sum::<f64>()
        / 3.0
}

fn box_extent(bounds: Aabb3) -> f64 {
    (bounds.max.x - bounds.min.x)
        .max(bounds.max.y - bounds.min.y)
        .max(bounds.max.z - bounds.min.z)
}

fn point_box_distance(point: Point3, bounds: Aabb3) -> f64 {
    let gap = |value: f64, low: f64, high: f64| (low - value).max(value - high).max(0.0);
    let x = gap(point.x, bounds.min.x, bounds.max.x);
    let y = gap(point.y, bounds.min.y, bounds.max.y);
    let z = gap(point.z, bounds.min.z, bounds.max.z);
    x.mul_add(x, y.mul_add(y, z * z))
}

fn box_distance(a: Aabb3, b: Aabb3) -> f64 {
    let gap = |a_min: f64, a_max: f64, b_min: f64, b_max: f64| {
        (b_min - a_max).max(a_min - b_max).max(0.0)
    };
    let x = gap(a.min.x, a.max.x, b.min.x, b.max.x);
    let y = gap(a.min.y, a.max.y, b.min.y, b.max.y);
    let z = gap(a.min.z, a.max.z, b.min.z, b.max.z);
    x.hypot(y).hypot(z)
}

/// Whether a ray from `origin` along `direction` can reach the box at all.
fn ray_hits_box(origin: Point3, direction: Vector3, bounds: Aabb3) -> bool {
    let mut near = 0.0_f64;
    let mut far = f64::INFINITY;
    for (start, step, low, high) in [
        (origin.x, direction.x, bounds.min.x, bounds.max.x),
        (origin.y, direction.y, bounds.min.y, bounds.max.y),
        (origin.z, direction.z, bounds.min.z, bounds.max.z),
    ] {
        if step.abs() <= f64::EPSILON {
            if start < low || start > high {
                return false;
            }
            continue;
        }
        let first = (low - start) / step;
        let second = (high - start) / step;
        near = near.max(first.min(second));
        far = far.min(first.max(second));
        if near > far {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Closest points
// ---------------------------------------------------------------------------

/// The closest points of two triangles and the distance between them.
///
/// Crossing triangles meet, so the answer is the crossing point at zero.
/// Otherwise the minimum sits on a vertex-face or edge-edge pair, and both
/// families are searched.
fn closest_points_on_triangles(first: &[Point3; 3], second: &[Point3; 3]) -> (Point3, Point3, f64) {
    if let Some(point) = triangles_cross(first, second) {
        return (point, point, 0.0);
    }
    let mut best = (first[0], second[0], f64::INFINITY);
    let mut consider = |a: Point3, b: Point3| {
        let distance = squared_distance(a, b);
        if distance < best.2 {
            best = (a, b, distance);
        }
    };
    for vertex in first {
        consider(*vertex, closest_point_on_triangle(*vertex, second));
    }
    for vertex in second {
        consider(closest_point_on_triangle(*vertex, first), *vertex);
    }
    for edge in edges(first) {
        for other in edges(second) {
            let (a, b) = closest_points_on_segments(edge, other);
            consider(a, b);
        }
    }
    (best.0, best.1, best.2.sqrt())
}

/// A point common to two triangles, when one crosses the other.
fn triangles_cross(first: &[Point3; 3], second: &[Point3; 3]) -> Option<Point3> {
    for [start, end] in edges(first) {
        let direction = subtract(end, start);
        if let Some(hit) = ray_triangle(start, direction, second)
            && (0.0..=1.0).contains(&hit)
        {
            return Some(along(start, direction, hit));
        }
    }
    for [start, end] in edges(second) {
        let direction = subtract(end, start);
        if let Some(hit) = ray_triangle(start, direction, first)
            && (0.0..=1.0).contains(&hit)
        {
            return Some(along(start, direction, hit));
        }
    }
    None
}

const fn edges(triangle: &[Point3; 3]) -> [[Point3; 2]; 3] {
    [
        [triangle[0], triangle[1]],
        [triangle[1], triangle[2]],
        [triangle[2], triangle[0]],
    ]
}

fn closest_point_on_segment(point: Point3, segment: [Point3; 2]) -> Point3 {
    let direction = subtract(segment[1], segment[0]);
    let length = dot(direction, direction);
    if length <= f64::EPSILON {
        return segment[0];
    }
    let t = (dot(subtract(point, segment[0]), direction) / length).clamp(0.0, 1.0);
    along(segment[0], direction, t)
}

/// The closest point of a triangle to `point`, by the barycentric region
/// test: the projection when it lands inside, and the nearest edge or
/// vertex otherwise.
fn closest_point_on_triangle(point: Point3, triangle: &[Point3; 3]) -> Point3 {
    let ab = subtract(triangle[1], triangle[0]);
    let ac = subtract(triangle[2], triangle[0]);
    let ap = subtract(point, triangle[0]);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return triangle[0];
    }
    let bp = subtract(point, triangle[1]);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return triangle[1];
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let denominator = d1 - d3;
        if denominator.abs() > f64::EPSILON {
            return along(triangle[0], ab, d1 / denominator);
        }
        return triangle[0];
    }
    let cp = subtract(point, triangle[2]);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return triangle[2];
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let denominator = d2 - d6;
        if denominator.abs() > f64::EPSILON {
            return along(triangle[0], ac, d2 / denominator);
        }
        return triangle[0];
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let denominator = (d4 - d3) + (d5 - d6);
        if denominator.abs() > f64::EPSILON {
            return along(
                triangle[1],
                subtract(triangle[2], triangle[1]),
                (d4 - d3) / denominator,
            );
        }
        return triangle[1];
    }
    let denominator = va + vb + vc;
    if denominator.abs() <= f64::EPSILON {
        return triangle[0];
    }
    let v = vb / denominator;
    let w = vc / denominator;
    Point3::new(
        triangle[0].x + ab.x * v + ac.x * w,
        triangle[0].y + ab.y * v + ac.y * w,
        triangle[0].z + ab.z * v + ac.z * w,
    )
}

/// The closest pair of points on two segments, clamped to both extents.
fn closest_points_on_segments(first: [Point3; 2], second: [Point3; 2]) -> (Point3, Point3) {
    let d1 = subtract(first[1], first[0]);
    let d2 = subtract(second[1], second[0]);
    let r = subtract(first[0], second[0]);
    let a = dot(d1, d1);
    let e = dot(d2, d2);
    let f = dot(d2, r);
    if a <= f64::EPSILON && e <= f64::EPSILON {
        return (first[0], second[0]);
    }
    if a <= f64::EPSILON {
        return (first[0], closest_point_on_segment(first[0], second));
    }
    if e <= f64::EPSILON {
        return (closest_point_on_segment(second[0], first), second[0]);
    }
    let c = dot(d1, r);
    let b = dot(d1, d2);
    let denominator = a.mul_add(e, -(b * b));
    let s = if denominator.abs() > f64::EPSILON {
        ((b * f - c * e) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let t = (b.mul_add(s, f)) / e;
    let t_clamped = t.clamp(0.0, 1.0);
    // Re-solve the first parameter against the clamped second so a pair of
    // segments whose infinite lines meet outside both extents still reports
    // the closest points on the segments themselves.
    let s = if a > f64::EPSILON {
        ((b * t_clamped - c) / a).clamp(0.0, 1.0)
    } else {
        s
    };
    (along(first[0], d1, s), along(second[0], d2, t_clamped))
}

/// The ray parameter at which `origin + direction·t` crosses the triangle,
/// front or back.
fn ray_triangle(origin: Point3, direction: Vector3, triangle: &[Point3; 3]) -> Option<f64> {
    const EPSILON: f64 = 1.0e-12;
    let edge1 = subtract(triangle[1], triangle[0]);
    let edge2 = subtract(triangle[2], triangle[0]);
    let h = cross(direction, edge2);
    let a = dot(edge1, h);
    if a.abs() < EPSILON {
        return None;
    }
    let f = 1.0 / a;
    let s = subtract(origin, triangle[0]);
    let u = f * dot(s, h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, edge1);
    let v = f * dot(direction, q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * dot(edge2, q);
    (t >= 0.0).then_some(t)
}

fn subtract(a: Point3, b: Point3) -> Vector3 {
    Vector3::new(a.x - b.x, a.y - b.y, a.z - b.z)
}

fn along(origin: Point3, direction: Vector3, t: f64) -> Point3 {
    Point3::new(
        direction.x.mul_add(t, origin.x),
        direction.y.mul_add(t, origin.y),
        direction.z.mul_add(t, origin.z),
    )
}

fn dot(a: Vector3, b: Vector3) -> f64 {
    a.x.mul_add(b.x, a.y.mul_add(b.y, a.z * b.z))
}

fn cross(a: Vector3, b: Vector3) -> Vector3 {
    Vector3::new(
        a.y.mul_add(b.z, -(a.z * b.y)),
        a.z.mul_add(b.x, -(a.x * b.z)),
        a.x.mul_add(b.y, -(a.y * b.x)),
    )
}

fn squared_distance(a: Point3, b: Point3) -> f64 {
    let d = subtract(a, b);
    dot(d, d)
}

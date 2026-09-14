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
//! Facets are chords of the surfaces they stand for. The chords of a
//! convex face — the outside of a boss or a pin — lie inside the body, so
//! their gap to anything over-reads the true one; the chords of a concave
//! face — a bore — lie in the void and can under-read it. Either way a
//! chord is never further from its arc than the arc's sagitta, and the
//! kernel knows the sagitta of every chord the display tessellation spent,
//! face by face ([`NativeKernel::display_chord_deviations`]).
//!
//! So the report carries a `bound` alongside the measured `distance`, and
//! the bound is earned rather than assumed: a second descent through the
//! same hierarchies minimises, over every pair of facets, the facet gap
//! less the deviations of the two facets, which is the least the true
//! surfaces can be apart. The true gap is never below `distance - bound`,
//! and every judgement — apart, touching, inside, and the verdict a fit
//! profile gives — is made on that pessimistic figure. Between two bodies
//! of planar faces there is no chord, the bound is zero, and the answer is
//! exact. The bound is also zero when the closest approach was read between
//! planar faces and no chorded face comes within the same distance.
//!
//! The descent knows how far a chord can be from its arc but not which
//! way, so it is conservative where the direction would have helped: a
//! cylinder standing on a plate touches it cap to face, exactly, but the
//! wall's chords end on that same rim at no distance from the plate, and
//! the pair carries the wall's sagitta as its bound. A contact the facets
//! cannot vouch for is reported as one they cannot vouch for.

use std::collections::BTreeMap;

use artificer_protocol::{Aabb3, EntityRef, Point3, PrecisionPolicy, Tier, Vector3};
use serde::{Deserialize, Serialize};

use crate::{ChordDeviation, DebugScene, NativeKernel, Snapshot};

/// The rigid placement of a body in the world an interference study is run
/// in. Assembly occurrences carry one; two bodies of the same session share
/// the identity.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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
    /// The bodies are apart by more than the facets can be wrong: the
    /// pessimistic gap, `distance - bound`, is positive.
    Clear,
    /// The surfaces meet, or come within the bound of one another, without
    /// either body reaching inside the other further than the facets can
    /// account for. Between planar bodies this is contact; between curved
    /// ones it is a gap the facets cannot tell from contact.
    Touching,
    /// One body reaches inside the other by more than the facets can be
    /// wrong.
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
    /// How far below `distance` the true clearance may sit, in millimetres.
    ///
    /// The true gap is never less than `distance - bound`. It can also sit
    /// above `distance`, by no more than `bound`, where the closest facets
    /// belong to a concave face whose chords lie in the void. Zero when
    /// both bodies are planar, and zero when the closest approach was read
    /// between planar faces with no chorded face as near: then the answer
    /// is exact even though a body is curved elsewhere.
    pub bound: f64,
}

impl ClearanceReport {
    /// The least the true surfaces can be apart: `distance - bound`, which
    /// is negative when the facets cannot rule out an overlap.
    #[must_use]
    pub fn pessimistic_distance(&self) -> f64 {
        self.distance - self.bound
    }

    /// Whether the bodies may share space: one reaches inside the other,
    /// or their facets come nearer than the facets can be wrong, so contact
    /// cannot be told from overlap. Two planar bodies in contact do not,
    /// because their contact is exact.
    #[must_use]
    pub fn may_overlap(&self) -> bool {
        self.state == ClearanceState::Interfering || self.pessimistic_distance() < 0.0
    }
}

/// One facet in world coordinates, with how far the face it stands for can
/// sit from it (`deviation`, which the bound is built from) and how far it
/// can sit from that face (`overshoot`, which a point of it has to be inside
/// another body by before it counts as evidence of an overlap).
#[derive(Clone, Copy, Debug)]
struct Facet {
    points: [Point3; 3],
    deviation: f64,
    overshoot: f64,
}

/// One body's facets in world coordinates, in a bounding-volume hierarchy.
///
/// The facets are placed at build time rather than transformed during the
/// descent: an axis-aligned box under rotation is no longer axis-aligned,
/// and a hierarchy that has to account for that is a much larger thing to
/// get right than a rebuild is to pay for.
#[derive(Clone, Debug)]
pub struct FacetIndex {
    facets: Vec<Facet>,
    nodes: Vec<Node>,
    /// Whether every facet is its surface rather than a chord of it, which
    /// is what a body of planes and straight edges has.
    exact: bool,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    bounds: Aabb3,
    /// The largest deviation of any facet under this node, so a descent
    /// that reasons about the true surfaces can prune on it.
    deviation: f64,
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
    /// Builds the index for a snapshot at a placement, over the display
    /// scene and the deviation of each of its faces.
    #[must_use]
    pub fn build(snapshot: &Snapshot, placement: Placement) -> Self {
        let scene = NativeKernel::debug_scene(snapshot);
        let deviations = NativeKernel::display_chord_deviations(snapshot);
        Self::from_scene(&scene, placement, &deviations)
    }

    /// Builds the index from a scene the caller already has, with how far
    /// the facets of each face can sit from that face, keyed as the scene's
    /// triangles name their source face
    /// ([`NativeKernel::display_chord_deviations`]). A face the map does not
    /// name takes the worst deviation in it, which errs towards a wider
    /// bound rather than a narrower one.
    #[must_use]
    pub fn from_scene(
        scene: &DebugScene,
        placement: Placement,
        deviations: &BTreeMap<EntityRef, ChordDeviation>,
    ) -> Self {
        let worst = deviations
            .values()
            .fold(ChordDeviation::default(), |worst, deviation| {
                ChordDeviation {
                    of_surface: worst.of_surface.max(deviation.of_surface),
                    of_facets: worst.of_facets.max(deviation.of_facets),
                }
            });
        let facets = scene
            .triangles
            .iter()
            .map(|triangle| {
                let deviation = deviations
                    .get(&triangle.source_face)
                    .copied()
                    .unwrap_or(worst);
                Facet {
                    points: triangle.vertices.map(|point| placement.apply(point)),
                    deviation: deviation.of_surface,
                    overshoot: deviation.of_facets,
                }
            })
            .filter(|facet| {
                facet.points.iter().all(|point| point.is_finite())
                    && facet.deviation.is_finite()
                    && facet.overshoot.is_finite()
            })
            .collect::<Vec<_>>();
        let exact = facets
            .iter()
            .all(|facet| facet.deviation == 0.0 && facet.overshoot == 0.0);
        let mut index = Self {
            facets,
            nodes: Vec::new(),
            exact,
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
        let facets = &self.facets[start..start + count];
        let bounds = bounds_of(facets);
        let deviation = facets
            .iter()
            .map(|facet| facet.deviation)
            .fold(0.0, f64::max);
        let node = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            deviation,
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
                let candidate =
                    squared_distance(point, closest_point_on_triangle(point, &facet.points));
                if candidate < best {
                    best = candidate;
                }
            }
        }
        best.sqrt()
    }

    /// The least a point can be from the body's true surface: the distance
    /// to each facet less that facet's deviation, minimised over the
    /// facets. Negative when a chord of the surface may pass on the far
    /// side of the point.
    #[must_use]
    pub fn surface_margin(&self, point: Point3) -> f64 {
        if self.nodes.is_empty() {
            return f64::INFINITY;
        }
        let mut best = f64::INFINITY;
        let mut stack = vec![0_usize];
        while let Some(index) = stack.pop() {
            let node = self.nodes[index];
            if point_box_distance(point, node.bounds).sqrt() - node.deviation >= best {
                continue;
            }
            if node.count == 0 {
                stack.push(node.left);
                stack.push(node.right);
                continue;
            }
            for facet in &self.facets[node.start..node.start + node.count] {
                let candidate =
                    squared_distance(point, closest_point_on_triangle(point, &facet.points)).sqrt()
                        - facet.deviation;
                if candidate < best {
                    best = candidate;
                }
            }
        }
        best
    }

    /// Whether a point is inside the body and clear of its true surface by
    /// more than `tolerance`, allowing for how far the facets can sit from
    /// that surface.
    #[must_use]
    pub fn strictly_contains(&self, point: Point3, tolerance: f64) -> bool {
        self.surface_margin(point) > tolerance && self.contains(point)
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
                if ray_triangle(point, direction, &facet.points).is_some_and(|hit| hit > 0.0) {
                    crossings += 1;
                }
            }
        }
        crossings % 2 == 1
    }
}

/// The closest approach of two bodies, and what it means.
///
/// Two descents. The first minimises the facet gap and gives `distance`
/// and the witnesses. The second minimises the facet gap less the two
/// facets' deviations, which is the least the true surfaces can be apart,
/// and the difference between the two is the `bound` the pair publishes.
/// Both prune on box distance, so the pairs compared facet by facet are
/// the ones that could hold the minimum.
///
/// The state is judged on the pessimistic figure. When the surfaces meet
/// or come within the bound, the two bodies are separated further:
/// touching is not interfering, and a fit check has to tell them apart.
#[must_use]
pub fn clearance(a: &FacetIndex, b: &FacetIndex, precision: PrecisionPolicy) -> ClearanceReport {
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
    let mut pessimistic = Best {
        distance: f64::INFINITY,
        witness_a: Point3::new(0.0, 0.0, 0.0),
        witness_b: Point3::new(0.0, 0.0, 0.0),
    };
    if !a.is_empty() && !b.is_empty() {
        descend(a, 0, b, 0, &mut best, Objective::Measured);
        if a.exact && b.exact {
            pessimistic.distance = best.distance;
        } else {
            descend(a, 0, b, 0, &mut pessimistic, Objective::Pessimistic);
        }
    }
    let distance = if best.distance.is_finite() {
        best.distance.max(0.0)
    } else {
        f64::INFINITY
    };
    // The pessimistic minimum is never above the measured one: the pair
    // that held the measured minimum is in its running too, with something
    // subtracted. Rounding is the only way they could disagree, and the
    // bound is clamped so it never reads below zero.
    let bound = if distance.is_finite() {
        (distance - pessimistic.distance).max(0.0)
    } else {
        0.0
    };
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
    } else if distance - bound <= touching {
        ClearanceState::Touching
    } else {
        ClearanceState::Clear
    };
    ClearanceReport {
        state,
        distance,
        witness_a: best.witness_a,
        witness_b: best.witness_b,
        tier,
        bound,
    }
}

/// The signed clearance from every vertex of a scene's facets to the
/// nearest of the other bodies, in scene order: three values per facet, one
/// per corner.
///
/// Positive is a gap. Negative is penetration, and its magnitude is how far
/// inside the nearest other body that corner sits, which is what tells a
/// collision from a tight fit. A vertex with no other body to measure
/// against reads infinite.
///
/// Sampling at corners rather than at facet centres is what lets a renderer
/// interpolate the reading across a facet: a tessellated cylinder then
/// shows the clearance rather than its own tessellation.
///
/// Corners alone would miss a collision that falls wholly inside a facet — a
/// pin driven through the middle of a disc has every corner of that disc on
/// its rim, outside the pin. So each facet's centre is read too, and a
/// facet whose centre is inside another body is painted as collision
/// throughout. That over-states a collision by at most one facet and never
/// hides one, which is the direction a fit check has to err in.
#[must_use]
pub fn clearance_field(
    scene: &DebugScene,
    placement: Placement,
    others: &[&FacetIndex],
) -> Vec<f64> {
    let mut values = Vec::with_capacity(scene.triangles.len() * 3);
    for triangle in &scene.triangles {
        let placed = triangle.vertices.map(|point| placement.apply(point));
        let mut corners = placed.map(|point| signed_clearance(point, others));
        let centre = signed_clearance(facet_centre(&placed), others);
        if centre < 0.0 {
            for corner in &mut corners {
                *corner = corner.min(centre);
            }
        }
        values.extend_from_slice(&corners);
    }
    values
}

/// The nearest signed clearance from one point to a set of bodies.
fn signed_clearance(point: Point3, others: &[&FacetIndex]) -> f64 {
    let mut best = f64::INFINITY;
    for other in others {
        let Some(bounds) = other.bounds() else {
            continue;
        };
        // The surface distance is a descent that prunes; containment is a
        // ray cast that does not, so it is asked only of the points whose
        // answer could be yes at all. It cannot be skipped on the running
        // minimum: a point deep inside one body reads a large distance and
        // still belongs below a small gap to another.
        let distance = other.distance_to_surface(point);
        let signed = if inside_bounds(point, bounds) && other.contains(point) {
            -distance
        } else {
            distance
        };
        if signed < best {
            best = signed;
        }
    }
    best
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
    // A curved body's facets sit a chord off its true surface, which the
    // margin test allows for facet by facet: a point counts as inside only
    // when no chord of the outer body could have carried its surface past
    // the point. The inner body's own facets can overshoot their surface as
    // well — a polygon inscribed in a bore reaches into the bore — and a
    // point of them inside by less than that overshoot is not evidence:
    // the state then rests on the pessimistic distance, which reads such
    // a pair as touching rather than clear.
    let inside = |point: Point3, allowance: f64| {
        inside_bounds(point, bounds)
            && outer.surface_margin(point) > agreement + allowance
            && outer.contains(point)
    };
    // Vertices alone are not enough. Two boxes that overlap over a slab can
    // have every vertex of each lying on a face of the other, so the facet
    // centres are tested too: they are interior to their own facet, and one
    // of them lands in the overlap whenever the boundaries genuinely cross.
    for facet in &inner.facets {
        let deep = |point: Point3| inside(point, facet.overshoot);
        if deep(facet_centre(&facet.points)) || facet.points.iter().any(|point| deep(*point)) {
            return true;
        }
    }
    // A body wholly coincident with another has its whole boundary on that
    // boundary, and only a point off the surface settles it. The point has
    // to be inside the inner body too: the centre of a ring's box is in its
    // hole, and a shaft through that hole is not an interference.
    inner
        .bounds()
        .map(box_centre)
        .is_some_and(|centre| inner.contains(centre) && inside(centre, 0.0))
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

/// What a descent minimises over the facet pairs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Objective {
    /// The gap between the facets as they stand.
    Measured,
    /// The gap less both facets' deviations: the least the true surfaces
    /// those facets stand for can be apart.
    Pessimistic,
}

impl Objective {
    /// How much the two sides' deviations take off a gap.
    fn allowance(self, left: f64, right: f64) -> f64 {
        match self {
            Self::Measured => 0.0,
            Self::Pessimistic => left + right,
        }
    }
}

fn descend(
    a: &FacetIndex,
    ai: usize,
    b: &FacetIndex,
    bi: usize,
    best: &mut Best,
    objective: Objective,
) {
    let (left, right) = (a.nodes[ai], b.nodes[bi]);
    if box_distance(left.bounds, right.bounds)
        - objective.allowance(left.deviation, right.deviation)
        >= best.distance
    {
        return;
    }
    match (left.count == 0, right.count == 0) {
        (false, false) => {
            for first in &a.facets[left.start..left.start + left.count] {
                for second in &b.facets[right.start..right.start + right.count] {
                    let (point_a, point_b, distance) =
                        closest_points_on_triangles(&first.points, &second.points);
                    let distance =
                        distance - objective.allowance(first.deviation, second.deviation);
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
                descend(a, child, b, bi, best, objective);
            }
        }
        (false, true) => {
            for child in children(&b.nodes, bi) {
                descend(a, ai, b, child, best, objective);
            }
        }
        (true, true) => {
            if box_extent(left.bounds) >= box_extent(right.bounds) {
                for child in children(&a.nodes, ai) {
                    descend(a, child, b, bi, best, objective);
                }
            } else {
                for child in children(&b.nodes, bi) {
                    descend(a, ai, b, child, best, objective);
                }
            }
        }
    }
}

const fn children(nodes: &[Node], node: usize) -> [usize; 2] {
    [nodes[node].left, nodes[node].right]
}

fn bounds_of(facets: &[Facet]) -> Aabb3 {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for facet in facets {
        for point in &facet.points {
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

fn centroid_axis(facet: &Facet, axis: usize) -> f64 {
    facet
        .points
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

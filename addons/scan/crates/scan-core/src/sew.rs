//! Corners and loops: turning a set of labelled curves into topology.
//!
//! The edge extractor hands over one curve per pair of touching faces,
//! each knowing which two faces it separates and nothing else. That is a
//! drawing, not a boundary: nothing connects one curve to the next where
//! a third face joins, and no face knows its border as something that
//! can be walked. A corner supplies the connection — the point where
//! three surfaces meet is where their three pairwise edges end — and
//! once every edge end sits on a corner, walking corner to corner
//! around a face yields its loop.
//!
//! Corners are **solved, not estimated**. Every carrier answers `probe`
//! with a signed distance and its gradient, so the point where three
//! surfaces meet is the root of three equations and Newton's method
//! lands on it to machine precision in a handful of steps. The
//! commercial and research pipelines mesh their surfaces and intersect
//! triangles precisely because their freeform patches make the analytic
//! calculation unstable; a kernel that forbids freeform surfaces gets
//! the exact corner almost for free, and it would be a waste to
//! approximate what can be solved.
//!
//! Where the seed comes from is evidence, as everywhere else in this
//! pipeline: edge ends that finish near one another nominate a corner
//! between the faces they collectively border. Three surfaces in
//! general position always meet *somewhere* — often far outside the
//! part — so the solve is anchored to where the edges actually stopped,
//! and a root that runs away from its seed is rejected as the phantom
//! it is.

use artificer_geometry::Point3;

use crate::rebuild::SharedEdge;
use crate::segment::SurfaceClass;

/// How far from its seed a corner may be solved before it is judged to
/// be a different point than the evidence suggested (mm). Edge ends
/// stop short of the true corner — the scanner rounds it, and the
/// footprints thin out — so the reach has to clear a rounded corner's
/// worth of gap without letting the root wander to the far side of the
/// part.
const CORNER_REACH: f64 = 4.0;

/// Edge ends within this distance of one another nominate one corner
/// (mm).
const CLUSTER_REACH: f64 = 3.0;

/// An edge whose two ends land this close together is a closed ring —
/// a bore's rim — and needs no corner at all.
const RING_CLOSE: f64 = 1.5;

/// A point where three or more faces meet, exactly.
#[derive(Clone, Debug)]
pub struct Corner {
    pub at: Point3,
    /// The faces meeting here, sorted.
    pub faces: Vec<usize>,
    /// The edge ends resolved onto this corner: `(edge index, end)`,
    /// end 0 being the front of the polyline and 1 the back.
    pub ends: Vec<(usize, usize)>,
}

/// One face's boundary, as an ordered walk of shared edges.
#[derive(Clone, Debug)]
pub struct FaceLoop {
    pub face: usize,
    /// Edge index and whether the walk runs against the curve's own
    /// direction.
    pub edges: Vec<(usize, bool)>,
}

/// The sewn shell, and how far it is from being a solid.
#[derive(Clone, Debug, Default)]
pub struct Shell {
    pub loops: Vec<FaceLoop>,
    /// Edges used by exactly two loops, in opposite directions: sewn.
    pub sewn_edges: usize,
    /// Used by two loops running the SAME way — the faces disagree about
    /// which side is outside, so the shell is not orientable there.
    pub disagreeing_edges: usize,
    /// Used by one loop: a free boundary, a hole in the shell.
    pub free_edges: usize,
    /// Used by three or more: the shell branches.
    pub branching_edges: usize,
    /// Loops reversed to agree with their face's outward normal.
    pub reoriented: usize,
    /// Loops enclosing so little signed area that "which way round" has
    /// no answer — two edges between the same pair of corners, mostly.
    /// A loop that cannot be orientated cannot sew.
    pub unorientable: usize,
    /// Loop lengths, for reading what the walk is actually producing.
    pub two_edge_loops: usize,
}

impl Shell {
    /// The share of edges that are properly sewn. One is watertight;
    /// anything less is the fraction of the boundary that closed.
    pub fn watertight_fraction(&self) -> f64 {
        let total =
            self.sewn_edges + self.disagreeing_edges + self.free_edges + self.branching_edges;
        if total == 0 {
            return 0.0;
        }
        self.sewn_edges as f64 / total as f64
    }

    pub fn describe(&self) -> String {
        format!(
            "shell: {} face loop(s), {:.1}% of edges sewn ({} sewn, {} free, {} disagreeing, \
             {} branching); {} loop(s) reoriented",
            self.loops.len(),
            100.0 * self.watertight_fraction(),
            self.sewn_edges,
            self.free_edges,
            self.disagreeing_edges,
            self.branching_edges,
            self.reoriented
        ) + &format!(
            "; {} unorientable; {} two-edge loop(s)",
            self.unorientable, self.two_edge_loops
        )
    }
}

/// What the sewing pass achieved, in numbers that admit failure.
#[derive(Clone, Copy, Debug)]
pub struct SewSummary {
    pub corners: usize,
    /// Edge ends attached to a corner.
    pub resolved_ends: usize,
    /// Edge ends left hanging — boundary the model does not close.
    pub open_ends: usize,
    /// Edges that close on themselves (a bore rim), loops already.
    pub closed_rings: usize,
    /// Loops walked corner-to-corner around a face and closed.
    pub walked_loops: usize,
    /// Faces owning at least one closed loop.
    pub bounded_faces: usize,
    /// Faces that have edges at all.
    pub edged_faces: usize,
}

/// Joins edge fragments that are two pieces of one curve.
///
/// A cluster of ends bordering only *two* faces is not a corner: every
/// edge in it separates the same pair, so what has been found is a
/// single intersection curve broken where the footprint had a gap.
/// Treating it as a corner is impossible — a corner needs three
/// surfaces — so those ends stayed open forever, and they were the
/// largest share of the unresolved ones.
pub fn join_fragments(edges: &mut Vec<SharedEdge>, reach: f64) -> usize {
    let mut joined = 0usize;
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for first in 0..edges.len() {
            for second in (first + 1)..edges.len() {
                if edges[first].between != edges[second].between {
                    continue;
                }
                let (Some(&a0), Some(&a1)) =
                    (edges[first].points.first(), edges[first].points.last())
                else {
                    continue;
                };
                let (Some(&b0), Some(&b1)) =
                    (edges[second].points.first(), edges[second].points.last())
                else {
                    continue;
                };
                // Four ways two open curves can abut.
                let options = [
                    ((a1 - b0).length(), false, false),
                    ((a1 - b1).length(), false, true),
                    ((a0 - b0).length(), true, false),
                    ((a0 - b1).length(), true, true),
                ];
                let Some(&(gap, flip_first, flip_second)) = options
                    .iter()
                    .min_by(|left, right| left.0.total_cmp(&right.0))
                else {
                    continue;
                };
                if gap > reach {
                    continue;
                }
                let mut head = edges[first].points.clone();
                let mut tail = edges[second].points.clone();
                if flip_first {
                    head.reverse();
                }
                if flip_second {
                    tail.reverse();
                }
                head.extend(tail);
                edges[first].points = head;
                edges.remove(second);
                joined += 1;
                merged = true;
                break 'outer;
            }
        }
    }
    joined
}

/// The direction an edge leaves a corner, as a unit vector.
///
/// `end` 0 is the front of the polyline and 1 the back, so the tangent
/// always points *away* from the corner into the curve.
fn leaving(edge: &SharedEdge, end: usize) -> Option<artificer_geometry::Vector3> {
    let points = &edge.points;
    if points.len() < 2 {
        return None;
    }
    let (from, to) = if end == 0 {
        (points[0], points[1])
    } else {
        (points[points.len() - 1], points[points.len() - 2])
    };
    let along = to - from;
    let length = along.length();
    (length > 1e-12).then(|| along / length)
}

/// Why a corner solve produced no corner — the distinction is the
/// triage: a singular system is a *tangent or parallel* meeting where
/// no isolated point exists, while a runaway root is a real
/// intersection somewhere the evidence never stood.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerMiss {
    /// The 3×3 system is singular or never converges: at least two of
    /// the surfaces meet in a curve (tangent, parallel), not a point.
    Singular,
    /// A root exists but ran away from the seed past the reach.
    Runaway,
}

/// Newton-solves the point where three carriers meet.
///
/// Each iteration linearises the three signed distances about the
/// current point and solves the 3×3 system their gradients form; on
/// planes that is exact in one step, and on curved carriers it
/// converges quadratically. Degenerate triples — two parallel planes
/// and anything else — leave the system singular and answer
/// `Err(Singular)`, which is correct: such surfaces meet in no single
/// point.
pub fn corner_of(
    surfaces: [&SurfaceClass; 3],
    seed: Point3,
    reach: f64,
) -> Result<Point3, CornerMiss> {
    let mut point = seed;
    for _ in 0..16 {
        let mut rows = vec![vec![0.0; 3]; 3];
        let mut rhs = vec![0.0; 3];
        let mut worst = 0.0f64;
        for (slot, surface) in surfaces.iter().enumerate() {
            let (distance, normal) = surface.probe(point).ok_or(CornerMiss::Singular)?;
            rows[slot] = vec![normal.x, normal.y, normal.z];
            rhs[slot] = -distance;
            worst = worst.max(distance.abs());
        }
        if worst < 1e-10 {
            break;
        }
        let step = crate::numeric::solve_linear(rows, rhs).ok_or(CornerMiss::Singular)?;
        point = Point3::new(point.x + step[0], point.y + step[1], point.z + step[2]);
        if (point - seed).length() > reach {
            // The root exists but is not the corner the evidence stood
            // beside: three surfaces extended far enough always meet
            // somewhere, and somewhere is not good enough.
            return Err(CornerMiss::Runaway);
        }
    }
    for surface in surfaces {
        if surface.probe(point).ok_or(CornerMiss::Singular)?.0.abs() > 1e-6 {
            return Err(CornerMiss::Singular);
        }
    }
    Ok(point)
}

/// Why an edge end is still open after both resolution passes.
///
/// Each cause names a different fix: `TwoFaces` wants a third surface
/// (or is a genuine seam); `Tangent` can never resolve and wants the
/// blend layer; `Singular` is a tangent junction the solve met;
/// `Runaway` is bad evidence or a bad seed; `MissingSurface` is
/// bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpenCause {
    /// Only two faces border this end: the curve died mid-run with no
    /// third surface to pin a corner against.
    TwoFaces,
    /// The end belongs to a tangent boundary; two of any three
    /// surfaces there are tangent, so no isolated corner exists.
    Tangent,
    /// A triple was named but its system is singular — the surfaces
    /// meet in a curve, not a point.
    Singular,
    /// The Newton root fled the evidence; whatever it found elsewhere
    /// is a phantom.
    Runaway,
    /// A bordering face had no carrier to probe.
    MissingSurface,
}

impl OpenCause {
    pub fn describe(&self) -> &'static str {
        match self {
            OpenCause::TwoFaces => "two-face end (no third surface)",
            OpenCause::Tangent => "tangent boundary (no corner exists)",
            OpenCause::Singular => "singular triple (tangent junction)",
            OpenCause::Runaway => "runaway root (phantom corner)",
            OpenCause::MissingSurface => "missing carrier",
        }
    }
}

/// One unresolved edge end, located and diagnosed.
#[derive(Clone, Copy, Debug)]
pub struct OpenEnd {
    pub at: Point3,
    pub cause: OpenCause,
    /// Index into the edge list the end belongs to.
    pub edge: usize,
}

/// Resolves edge ends into corners and walks face loops.
///
/// Edges are mutated only at their ends: an end adopted by a corner has
/// the exact corner point appended, so the drawn curve reaches the
/// point the solve found rather than stopping a cell short of it.
/// `outward[id]` is `false` when a face's fitted normal points *into*
/// the material, so the probe's answer must be flipped to get the
/// direction the solid faces.
pub fn resolve(
    edges: &mut [SharedEdge],
    surfaces: &[(usize, SurfaceClass)],
    outward: &std::collections::HashMap<usize, bool>,
) -> (Vec<Corner>, SewSummary, Shell, Vec<OpenEnd>) {
    let surface_of = |id: usize| -> Option<&SurfaceClass> {
        surfaces
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, s)| s)
    };
    // Rings first: they are loops already and their ends stay put.
    let mut is_ring = vec![false; edges.len()];
    for (index, edge) in edges.iter().enumerate() {
        let (Some(first), Some(last)) = (edge.points.first(), edge.points.last()) else {
            continue;
        };
        if edge.points.len() > 3 && (*last - *first).length() <= RING_CLOSE {
            is_ring[index] = true;
        }
    }
    // Every open end, nominated for clustering.
    struct End {
        edge: usize,
        end: usize,
        at: Point3,
    }
    let mut ends: Vec<End> = Vec::new();
    for (index, edge) in edges.iter().enumerate() {
        if is_ring[index] || edge.points.len() < 2 {
            continue;
        }
        ends.push(End {
            edge: index,
            end: 0,
            at: edge.points[0],
        });
        ends.push(End {
            edge: index,
            end: 1,
            at: *edge.points.last().expect("non-empty"),
        });
    }
    // Greedy clustering; deterministic because ends arrive in edge order.
    let mut assigned = vec![false; ends.len()];
    // Why each still-open end is open; `TwoFaces` is the default the
    // solve paths overwrite when they got further and failed later.
    let mut causes: Vec<OpenCause> = vec![OpenCause::TwoFaces; ends.len()];
    let mut corners: Vec<Corner> = Vec::new();
    for seed_index in 0..ends.len() {
        if assigned[seed_index] {
            continue;
        }
        let mut members = vec![seed_index];
        for other in (seed_index + 1)..ends.len() {
            if !assigned[other] && (ends[other].at - ends[seed_index].at).length() <= CLUSTER_REACH
            {
                members.push(other);
            }
        }
        // The faces the clustered edges collectively border.
        let mut faces: Vec<usize> = Vec::new();
        for &member in &members {
            let (a, b) = edges[ends[member].edge].between;
            for face in [a, b] {
                if !faces.contains(&face) {
                    faces.push(face);
                }
            }
        }
        if faces.len() < 3 {
            // Two faces meet in a curve, not a point; these ends stay
            // open until a third surface claims them.
            continue;
        }
        faces.sort_unstable();
        // Solve on the three faces the evidence names most often; with
        // exactly three, that is simply the three.
        let mut votes: Vec<(usize, usize)> = faces
            .iter()
            .map(|&face| {
                let count = members
                    .iter()
                    .filter(|&&member| {
                        let (a, b) = edges[ends[member].edge].between;
                        a == face || b == face
                    })
                    .count();
                (face, count)
            })
            .collect();
        votes.sort_by_key(|&(face, count)| (std::cmp::Reverse(count), face));
        let triple: Vec<usize> = votes.iter().take(3).map(|&(face, _)| face).collect();
        let (Some(sa), Some(sb), Some(sc)) = (
            surface_of(triple[0]),
            surface_of(triple[1]),
            surface_of(triple[2]),
        ) else {
            for &member in &members {
                causes[member] = OpenCause::MissingSurface;
            }
            continue;
        };
        let centroid = members.iter().fold(Point3::default(), |acc, &member| {
            let p = ends[member].at;
            Point3::new(
                acc.x + p.x / members.len() as f64,
                acc.y + p.y / members.len() as f64,
                acc.z + p.z / members.len() as f64,
            )
        });
        let exact = match corner_of([sa, sb, sc], centroid, CORNER_REACH) {
            Ok(point) => point,
            Err(miss) => {
                let cause = match miss {
                    CornerMiss::Singular => OpenCause::Singular,
                    CornerMiss::Runaway => OpenCause::Runaway,
                };
                for &member in &members {
                    causes[member] = cause;
                }
                continue;
            }
        };
        let mut corner = Corner {
            at: exact,
            faces,
            ends: Vec::new(),
        };
        for &member in &members {
            assigned[member] = true;
            corner.ends.push((ends[member].edge, ends[member].end));
        }
        corners.push(corner);
    }
    // An edge can also stop in the *middle* of another one — a T
    // junction, where this pair's curve runs into a third face that the
    // other edge already borders. End-to-end clustering cannot see that,
    // because the other edge has no end there to cluster with.
    {
        let cell = CLUSTER_REACH.max(1e-6);
        let mut index: std::collections::HashMap<(i64, i64, i64), Vec<usize>> =
            std::collections::HashMap::new();
        for (edge_index, edge) in edges.iter().enumerate() {
            for point in &edge.points {
                let key = (
                    (point.x / cell).floor() as i64,
                    (point.y / cell).floor() as i64,
                    (point.z / cell).floor() as i64,
                );
                let bucket = index.entry(key).or_default();
                if !bucket.contains(&edge_index) {
                    bucket.push(edge_index);
                }
            }
        }
        let mut extra: Vec<Corner> = Vec::new();
        for slot in 0..ends.len() {
            if assigned[slot] {
                continue;
            }
            let at = ends[slot].at;
            let mine = edges[ends[slot].edge].between;
            let base = (
                (at.x / cell).floor() as i64,
                (at.y / cell).floor() as i64,
                (at.z / cell).floor() as i64,
            );
            let mut nearby: Vec<usize> = Vec::new();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(bucket) = index.get(&(base.0 + dx, base.1 + dy, base.2 + dz)) {
                            for &other in bucket {
                                if other != ends[slot].edge && !nearby.contains(&other) {
                                    nearby.push(other);
                                }
                            }
                        }
                    }
                }
            }
            nearby.sort_unstable();
            // The third face is whichever the other edge borders that
            // this one does not.
            let mut best: Option<(f64, usize)> = None;
            for other in nearby {
                let pair = edges[other].between;
                let third = if pair.0 != mine.0 && pair.0 != mine.1 {
                    pair.0
                } else if pair.1 != mine.0 && pair.1 != mine.1 {
                    pair.1
                } else {
                    continue;
                };
                let Some(distance) = edges[other]
                    .points
                    .iter()
                    .map(|point| (*point - at).length())
                    .min_by(f64::total_cmp)
                else {
                    continue;
                };
                if distance <= CLUSTER_REACH && best.is_none_or(|(known, _)| distance < known) {
                    best = Some((distance, third));
                }
            }
            let Some((_, third)) = best else { continue };
            let (Some(sa), Some(sb), Some(sc)) =
                (surface_of(mine.0), surface_of(mine.1), surface_of(third))
            else {
                causes[slot] = OpenCause::MissingSurface;
                continue;
            };
            let exact = match corner_of([sa, sb, sc], at, CORNER_REACH) {
                Ok(point) => point,
                Err(miss) => {
                    causes[slot] = match miss {
                        CornerMiss::Singular => OpenCause::Singular,
                        CornerMiss::Runaway => OpenCause::Runaway,
                    };
                    continue;
                }
            };
            assigned[slot] = true;
            let mut faces = vec![mine.0, mine.1, third];
            faces.sort_unstable();
            extra.push(Corner {
                at: exact,
                faces,
                ends: vec![(ends[slot].edge, ends[slot].end)],
            });
        }
        corners.extend(extra);
    }
    // Snap adopted ends to their exact corner.
    for (index, corner) in corners.iter().enumerate() {
        let _ = index;
        for &(edge, end) in &corner.ends {
            let points = &mut edges[edge].points;
            if end == 0 {
                points.insert(0, corner.at);
            } else {
                points.push(corner.at);
            }
        }
    }
    // Walk loops per face: nodes are corners, arcs are edges whose both
    // ends resolved.
    let mut corner_of_end: std::collections::HashMap<(usize, usize), usize> =
        std::collections::HashMap::new();
    for (corner_index, corner) in corners.iter().enumerate() {
        for &(edge, end) in &corner.ends {
            corner_of_end.insert((edge, end), corner_index);
        }
    }
    let mut shell = Shell::default();
    let mut edged: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut bounded: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut walked_loops = 0usize;
    let mut closed_rings = 0usize;
    for (index, edge) in edges.iter().enumerate() {
        edged.insert(edge.between.0);
        edged.insert(edge.between.1);
        if is_ring[index] {
            closed_rings += 1;
            bounded.insert(edge.between.0);
            bounded.insert(edge.between.1);
        }
    }
    let faces: Vec<usize> = edged.iter().copied().collect();
    for &face in &faces {
        // Arcs on this face with both ends on corners.
        let arcs: Vec<(usize, usize, usize)> = edges
            .iter()
            .enumerate()
            .filter(|(index, edge)| {
                !is_ring[*index] && (edge.between.0 == face || edge.between.1 == face)
            })
            .filter_map(|(index, _)| {
                let head = corner_of_end.get(&(index, 0))?;
                let tail = corner_of_end.get(&(index, 1))?;
                Some((index, *head, *tail))
            })
            .collect();
        let mut used: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for &(start_edge, start_corner, mut here) in &arcs {
            if used.contains(&start_edge) {
                continue;
            }
            used.insert(start_edge);
            // The walk leaves `start_corner` along this edge, so the edge
            // is traversed forwards; later steps record their own sense.
            let mut walk: Vec<(usize, bool)> = vec![(start_edge, false)];
            let mut previous_edge = start_edge;
            let mut previous_at_head = false;
            let mut steps = 0usize;
            loop {
                if here == start_corner {
                    walked_loops += 1;
                    bounded.insert(face);
                    shell.loops.push(FaceLoop { face, edges: walk });
                    break;
                }
                // Take the next edge by angular order around the face,
                // not the first one that happens to touch this corner.
                // A greedy walk closes the shortest cycle available, and
                // on the gear that made every single "loop" two edges
                // long — a pair between the same two corners, enclosing
                // nothing and orientable in no direction.
                let arriving = leaving(&edges[previous_edge], if previous_at_head { 0 } else { 1 })
                    .map(|direction| direction * -1.0);
                let reference = surface_of(face)
                    .and_then(|surface| surface.probe(corners[here].at).map(|(_, normal)| normal));
                let mut best: Option<(usize, usize, usize, f64)> = None;
                for &(edge, head, tail) in &arcs {
                    if used.contains(&edge) || (head != here && tail != here) {
                        continue;
                    }
                    let end = if head == here { 0 } else { 1 };
                    let turn = match (arriving, reference, leaving(&edges[edge], end)) {
                        (Some(incoming), Some(normal), Some(outgoing)) => {
                            // Angle from the incoming direction, measured
                            // consistently about the face's own normal so
                            // every corner turns the same way.
                            let across = normal.cross(incoming);
                            let angle = outgoing.dot(across).atan2(outgoing.dot(incoming));
                            if angle < 0.0 {
                                angle + std::f64::consts::TAU
                            } else {
                                angle
                            }
                        }
                        _ => std::f64::consts::TAU,
                    };
                    if best.is_none_or(|(_, _, _, known)| turn < known) {
                        best = Some((edge, head, tail, turn));
                    }
                }
                let Some((next_edge, head, tail, _)) = best else {
                    break;
                };
                used.insert(next_edge);
                // Entering at the tail means walking the curve backwards.
                walk.push((next_edge, tail == here));
                previous_edge = next_edge;
                previous_at_head = tail == here;
                here = if head == here { tail } else { head };
                steps += 1;
                if steps > arcs.len() {
                    break;
                }
            }
        }
        // A ring is already a loop, and belongs to both its faces.
        for (index, edge) in edges.iter().enumerate() {
            if is_ring[index] && (edge.between.0 == face || edge.between.1 == face) {
                shell.loops.push(FaceLoop {
                    face,
                    edges: vec![(index, false)],
                });
            }
        }
    }
    // Orient each loop so it runs anticlockwise seen from outside its
    // own face, then count how the shell holds together. An edge shared
    // by two faces must be walked once in each direction; anything else
    // is a hole, a branch, or two faces disagreeing about which side is
    // outside.
    // Where each face's material lies: the centroid of every point on
    // every one of its loops. A single loop cannot supply this — a
    // cylinder's top ring is centred on the axis, and the direction from
    // the ring to its own centre is radial, which is exactly the
    // direction the signed-area test is blind to. Averaging over both
    // rings puts the reference at mid-height, where the axial direction
    // the loop needs finally has a sign.
    let mut interior: std::collections::HashMap<usize, (Point3, usize)> =
        std::collections::HashMap::new();
    for face_loop in &shell.loops {
        for &(edge, _) in &face_loop.edges {
            for point in &edges[edge].points {
                let entry = interior
                    .entry(face_loop.face)
                    .or_insert((Point3::default(), 0));
                entry.0 = Point3::new(
                    entry.0.x + point.x,
                    entry.0.y + point.y,
                    entry.0.z + point.z,
                );
                entry.1 += 1;
            }
        }
    }
    for face_loop in &mut shell.loops {
        let Some(surface) = surface_of(face_loop.face) else {
            continue;
        };
        let towards = interior.get(&face_loop.face).and_then(|&(sum, count)| {
            (count > 0).then(|| {
                Point3::new(
                    sum.x / count as f64,
                    sum.y / count as f64,
                    sum.z / count as f64,
                )
            })
        });
        let mut turning = 0.0;
        let mut samples = 0usize;
        for &(edge, reversed) in face_loop.edges.iter() {
            let points = &edges[edge].points;
            if points.len() < 2 {
                continue;
            }
            let (from, to) = if reversed {
                (*points.last().expect("non-empty"), points[0])
            } else {
                (points[0], *points.last().expect("non-empty"))
            };
            let along = to - from;
            let middle = Point3::new(
                (from.x + to.x) / 2.0,
                (from.y + to.y) / 2.0,
                (from.z + to.z) / 2.0,
            );
            let Some((_, mut facing)) = surface.probe(middle) else {
                continue;
            };
            // A fit's normal sign comes from its orientation hint, not
            // from which side the material is on, so it has to be turned
            // the way the scan says before it can decide anything. Two
            // faces orientated by the raw fit disagreed about which side
            // was outside on 38 of the gear's edges.
            if outward.get(&face_loop.face) == Some(&false) {
                facing = facing * -1.0;
            }
            // Walking the loop must keep the face's material on the
            // left: the cross of the travel direction with the outward
            // normal has to point into the face.
            match towards {
                Some(centre) => {
                    turning += along.cross(facing).dot(centre - middle);
                }
                None => {
                    // No interior reference: fall back to the signed area
                    // about the face's own normal, which is right for a
                    // planar face and blind on a curved one.
                    let arm = middle - Point3::default();
                    turning += facing.dot(arm.cross(along));
                }
            }
            samples += 1;
        }
        if face_loop.edges.len() <= 2 {
            shell.two_edge_loops += 1;
        }
        if samples == 0 || turning.abs() < 1e-6 {
            shell.unorientable += 1;
        }
        if samples > 0 && turning < 0.0 {
            face_loop.edges.reverse();
            for entry in face_loop.edges.iter_mut() {
                entry.1 = !entry.1;
            }
            shell.reoriented += 1;
        }
    }
    let mut usage: std::collections::HashMap<usize, Vec<bool>> = std::collections::HashMap::new();
    for face_loop in &shell.loops {
        for &(edge, reversed) in &face_loop.edges {
            usage.entry(edge).or_default().push(reversed);
        }
    }
    for senses in usage.values() {
        match senses.len() {
            1 => shell.free_edges += 1,
            2 => {
                if senses[0] == senses[1] {
                    shell.disagreeing_edges += 1;
                } else {
                    shell.sewn_edges += 1;
                }
            }
            _ => shell.branching_edges += 1,
        }
    }
    let resolved = corners.iter().map(|corner| corner.ends.len()).sum();
    let summary = SewSummary {
        corners: corners.len(),
        resolved_ends: resolved,
        open_ends: ends.len().saturating_sub(resolved),
        closed_rings,
        walked_loops,
        bounded_faces: bounded.len(),
        edged_faces: faces.len(),
    };
    // Every end still unassigned, located and named. A tangent edge's
    // ends are tangent by construction whatever the solve recorded —
    // the boundary itself cannot carry a corner.
    let opens: Vec<OpenEnd> = (0..ends.len())
        .filter(|&slot| !assigned[slot])
        .map(|slot| OpenEnd {
            at: ends[slot].at,
            cause: if edges[ends[slot].edge].tangent {
                OpenCause::Tangent
            } else {
                causes[slot]
            },
            edge: ends[slot].edge,
        })
        .collect();
    (corners, summary, shell, opens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{CylinderFit, DeviationStats, PlaneFit};
    use artificer_geometry::Vector3;

    fn plane(normal: Vector3, offset: f64) -> SurfaceClass {
        SurfaceClass::Plane(PlaneFit {
            origin: Point3::new(normal.x * offset, normal.y * offset, normal.z * offset),
            normal,
            deviation: DeviationStats {
                rms: 0.0,
                max_abs: 0.0,
            },
        })
    }

    /// Three planes meet in one point and Newton lands on it exactly —
    /// in one step, because the equations are linear.
    #[test]
    fn three_planes_solve_to_their_common_point() {
        let a = plane(Vector3::new(1.0, 0.0, 0.0), 1.0);
        let b = plane(Vector3::new(0.0, 1.0, 0.0), 2.0);
        let c = plane(Vector3::new(0.0, 0.0, 1.0), 3.0);
        let corner = corner_of([&a, &b, &c], Point3::new(0.4, 1.6, 2.7), 4.0).expect("corner");
        assert!((corner.x - 1.0).abs() < 1e-9);
        assert!((corner.y - 2.0).abs() < 1e-9);
        assert!((corner.z - 3.0).abs() < 1e-9);
    }

    /// A bore rim against a slot: cylinder and two planes. Curved, so
    /// Newton has to iterate — and still lands to machine precision.
    #[test]
    fn a_cylinder_and_two_planes_solve_exactly() {
        let bore = SurfaceClass::Cylinder(CylinderFit {
            axis_point: Point3::default(),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radius: 5.0,
            deviation: DeviationStats {
                rms: 0.0,
                max_abs: 0.0,
            },
        });
        let wall = plane(Vector3::new(0.0, 1.0, 0.0), 0.0);
        let floor = plane(Vector3::new(0.0, 0.0, 1.0), 2.0);
        let corner =
            corner_of([&bore, &wall, &floor], Point3::new(4.6, 0.4, 1.7), 4.0).expect("corner");
        assert!((corner.x - 5.0).abs() < 1e-8, "x {}", corner.x);
        assert!(corner.y.abs() < 1e-8);
        assert!((corner.z - 2.0).abs() < 1e-8);
    }

    /// Two parallel planes and a third meet in no point, and the solver
    /// must say so rather than invent one.
    #[test]
    fn a_degenerate_triple_is_refused() {
        let a = plane(Vector3::new(0.0, 0.0, 1.0), 1.0);
        let b = plane(Vector3::new(0.0, 0.0, 1.0), 5.0);
        let c = plane(Vector3::new(1.0, 0.0, 0.0), 0.0);
        assert_eq!(
            corner_of([&a, &b, &c], Point3::default(), 10.0),
            Err(CornerMiss::Singular)
        );
    }

    /// A root that exists but sits far from the evidence is a phantom:
    /// the walls of a wide pocket meet somewhere, and not here.
    #[test]
    fn a_faraway_root_is_rejected() {
        let a = plane(Vector3::new(1.0, 0.0, 0.0), 40.0);
        let b = plane(Vector3::new(0.0, 1.0, 0.0), 40.0);
        let c = plane(Vector3::new(0.0, 0.0, 1.0), 40.0);
        assert_eq!(
            corner_of([&a, &b, &c], Point3::default(), 4.0),
            Err(CornerMiss::Runaway)
        );
    }

    /// Ends meeting at a shared corner close the square; the walk finds
    /// one loop on the face all four edges border.
    #[test]
    fn four_edges_walk_into_one_loop() {
        // A unit square on face 0, each side shared with a wall 1..4.
        let corners_at = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(10.0, 0.0, 0.0),
            Point3::new(10.0, 10.0, 0.0),
            Point3::new(0.0, 10.0, 0.0),
        ];
        let mut edges: Vec<SharedEdge> = (0..4)
            .map(|side| {
                let from = corners_at[side];
                let to = corners_at[(side + 1) % 4];
                SharedEdge {
                    between: (0, side + 1),
                    points: vec![
                        from,
                        Point3::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0, 0.0),
                        to,
                    ],
                    tangent: false,
                }
            })
            .collect();
        // Surfaces: the floor plus four walls whose triples solve at the
        // square's corners.
        let floor = plane(Vector3::new(0.0, 0.0, 1.0), 0.0);
        let west = plane(Vector3::new(1.0, 0.0, 0.0), 0.0);
        let east = plane(Vector3::new(1.0, 0.0, 0.0), 10.0);
        let south = plane(Vector3::new(0.0, 1.0, 0.0), 0.0);
        let north = plane(Vector3::new(0.0, 1.0, 0.0), 10.0);
        let surfaces = vec![
            (0usize, floor),
            (1, south),
            (2, east),
            (3, north),
            (4, west),
        ];
        let outward: std::collections::HashMap<usize, bool> = (0..5).map(|id| (id, true)).collect();
        let (found, summary, shell, opens) = resolve(&mut edges, &surfaces, &outward);
        assert_eq!(found.len(), 4, "four corners of the square");
        assert_eq!(summary.open_ends, 0, "every end resolves");
        assert!(opens.is_empty(), "no open ends to triage");
        assert!(
            summary.walked_loops >= 1,
            "the floor's boundary closes into a loop"
        );
        assert!(summary.bounded_faces >= 1);
        assert!(
            !shell.loops.is_empty(),
            "the walk records the loop it found"
        );
    }
}

//! Exact fillets and chamfers on convex edges between planar faces, closed by
//! a certified patch at every corner the selection meets.
//!
//! The rungs before this one rebuild a whole body from a recovered profile: a
//! prism's vertical edges through a 2D corner op and a re-extrusion, a cap rim
//! through an inward offset of that profile. Neither can answer for the twelve
//! edges of a box at once, because the result is no longer a prism in any
//! direction. This rung therefore rebuilds nothing it does not have to: it
//! replaces the boundary of each planar face the selection touches, emits one
//! blend patch per selected edge and one corner patch per blended vertex, and
//! copies every other face — holes, slots, bores, blends from earlier features
//! — through verbatim.
//!
//! The geometry is the classical rolling-ball construction, all of it in
//! closed form:
//!
//! * A convex edge between planes `A` and `B` with interior dihedral `θ`
//!   carries a ball of radius `r` whose centre runs along the line at distance
//!   `r` from both planes. The ball touches each face along a line set back
//!   `r·cot(θ/2)` from the edge, and sweeps the quarter (in general the
//!   `π − θ`) cylinder between them. A chamfer replaces that cylinder with the
//!   plane through the two lines set back by the chamfer distance instead.
//! * At a vertex where three such edges and three planes meet, the ball's
//!   centre is the single point at distance `r` from all three planes, and the
//!   patch that closes the corner is the piece of the sphere of radius `r`
//!   about it bounded by the three cylinders' end circles — each of which is a
//!   great circle, because the cylinder's axis passes through that centre. A
//!   chamfer closes the same corner with the triangle through the three points
//!   where the setback lines meet inside each face.
//!
//! A sphere patch is expressed with one face's normal as its pole, which asks
//! that face to be square to the other two: a box corner, or the corner of any
//! prism where a flat cap meets two walls. A corner that is not square that way
//! would need a general great circle in the sphere's own parameters, which is
//! outside the line-and-circle vocabulary, and is refused by name rather than
//! approximated. A chamfer's corner is planar and has no such condition.
//!
//! A band does not have to end in a corner patch. Where a selected edge ends at
//! a vertex whose other two edges are *not* selected, the band runs out into
//! the third face there — the one the selected edge does not border. The ball's
//! end circle lies in that face's own plane whenever the face is square to the
//! edge, which is what a box corner, a prism cap, or a pocket floor gives, so
//! the run-out costs one arc in a face that was already flat and shortens the
//! two edges beside it. That is how three edges meeting at one vertex blend
//! without dragging the whole body's edge graph in with them, and how a second
//! corner blends on a body an earlier feature already rounded. A chamfer runs
//! out the same way, with a straight end instead of an arc, and needs no
//! squareness at all. A vertex with two of its three edges selected has no
//! exact answer in this vocabulary and is refused by name: the band would have
//! to fade out along the third edge rather than end on a face.

use std::collections::{BTreeMap, BTreeSet};

use artificer_protocol::{EdgeFinishKind, EntityKind, EntityRef, PrecisionPolicy, SnapshotId};

use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Cylinder, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole,
    Loop, LoopKey, Orientation, ParameterRange, Plane, Point2, Point3, Record, Shell, Sphere,
    Surface, Topology, Vector2, Vector3, Vertex, VertexKey,
};

/// A refusal this rung owns, with the code and sentence it publishes.
#[derive(Clone, Debug)]
pub(crate) struct Refusal {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    /// Whether no later rung can publish an honest answer either. The faceted
    /// tier can spend ten seconds discovering that for itself; when this is
    /// set, the ladder stops here instead.
    pub(crate) certain: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum VertexBlendError {
    /// Not this rung's request at all; the ladder continues in silence.
    DomainUnsupported,
    /// This rung owns the request and will not publish a guess.
    Refused(Refusal),
}

fn refuse(code: &'static str, message: impl Into<String>, certain: bool) -> VertexBlendError {
    VertexBlendError::Refused(Refusal {
        code,
        message: message.into(),
        certain,
    })
}

/// Blends every selected edge and closes every corner the selection completes,
/// or says why it cannot.
pub(crate) fn build_vertex_blend(
    snapshot: SnapshotId,
    topology: &Topology,
    targets: &[EntityRef],
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<Topology, VertexBlendError> {
    let plan = Plan::resolve(snapshot, topology, targets, kind, distance, precision)?;
    let built = plan.build()?;
    // Certify before publishing: a rung that cannot prove its own answer hands
    // the request on rather than committing it.
    let report = crate::validator::validate(&built, precision.linear_agreement);
    if report.diagnostics.is_empty() {
        return Ok(built);
    }
    let first = report.diagnostics[0].code.as_str();
    Err(refuse(
        "VERTEX_BLEND_CONSTRUCTION_FAILED",
        format!(
            "The corner blend was built but did not certify ({first} at {}); no approximation of \
             it is published under this rung.",
            report.diagnostics[0].path
        ),
        false,
    ))
}

// ---------------------------------------------------------------------------
// Reading the body
// ---------------------------------------------------------------------------

/// Which face uses an edge each way round, and which edges meet at a vertex.
struct Incidence {
    /// The face whose coedge runs along the edge's own direction, and the one
    /// that runs against it.
    edge_faces: Vec<Option<[usize; 2]>>,
    vertex_edges: Vec<Vec<usize>>,
    /// The shell each face belongs to.
    face_shell: Vec<usize>,
}

fn incidence(topology: &Topology) -> Option<Incidence> {
    let mut forward = vec![None; topology.edges.len()];
    let mut reverse = vec![None; topology.edges.len()];
    for (face_index, face) in topology.faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            let loop_record = topology.loop_record(loop_key)?;
            for coedge_key in &loop_record.value.coedges {
                let coedge = topology.coedge(*coedge_key)?;
                let side = match coedge.value.orientation {
                    Orientation::Forward => &mut forward,
                    Orientation::Reverse => &mut reverse,
                };
                let slot = side.get_mut(coedge.value.edge.0)?;
                if slot.is_some() {
                    // An edge used twice the same way round is a seam this
                    // rung does not read.
                    return None;
                }
                *slot = Some(face_index);
            }
        }
    }
    let edge_faces = forward
        .into_iter()
        .zip(reverse)
        .map(|(forward, reverse)| Some([forward?, reverse?]))
        .collect::<Vec<_>>();
    let mut vertex_edges = vec![Vec::new(); topology.vertices.len()];
    for (edge_index, edge) in topology.edges.iter().enumerate() {
        for vertex in edge.value.vertices {
            let slot = vertex_edges.get_mut(vertex.0)?;
            if !slot.contains(&edge_index) {
                slot.push(edge_index);
            }
        }
    }
    let mut face_shell = vec![usize::MAX; topology.faces.len()];
    for (shell_index, shell) in topology.shells.iter().enumerate() {
        for face in &shell.value.faces {
            *face_shell.get_mut(face.0)? = shell_index;
        }
    }
    Some(Incidence {
        edge_faces,
        vertex_edges,
        face_shell,
    })
}

/// One selected edge, read as a rolling-ball blend.
struct EdgePlan {
    edge: usize,
    /// The face along the edge's own direction, then the one against it.
    faces: [usize; 2],
    normals: [Vector3; 2],
    vertices: [usize; 2],
    /// Unit direction from `vertices[0]` to `vertices[1]`.
    direction: Vector3,
    /// How far inside each face the blend meets it.
    setback: f64,
    /// The cylinder's angular extent, `π − θ`; unused by a chamfer.
    sweep: f64,
    /// The chamfer plane's outward normal; unused by a fillet.
    bevel_normal: Vector3,
}

/// One vertex a selected edge ends at, and what the blend leaves there.
struct EndPlan {
    vertex: usize,
    /// Where the blend meets each face it touches here: all three at a closed
    /// corner, the band's own two at a run-out.
    tangency: BTreeMap<usize, Point3>,
    /// The ball's centre at a fillet corner or fillet run-out — in both cases
    /// a point on every incident band's axis — and the vertex's own point
    /// under a chamfer, which needs no ball.
    centre: Point3,
    kind: EndKind,
}

/// The two ways a band can end.
enum EndKind {
    /// Every edge at this vertex is selected, so a patch closes the corner:
    /// a sphere octant under a fillet, a triangle under a chamfer.
    Corner {
        /// The three planar faces at the corner, in ascending face order.
        faces: [usize; 3],
        /// Fillet only: the face whose normal is the sphere's pole, then the
        /// two whose tangencies sit on its equator, in increasing azimuth.
        frame: [usize; 3],
        /// Fillet only: the equator's angular extent.
        sweep: f64,
    },
    /// One selected edge ends here against a face square to it, so its band
    /// runs out into that face and the two edges beside it shorten.
    Runout {
        /// The selected edge that ends here.
        edge: usize,
        /// The face the band runs out into: the one at this vertex that the
        /// selected edge does not border.
        cap: usize,
    },
}

impl EndPlan {
    fn tangency_on(&self, face: usize) -> Option<Point3> {
        self.tangency.get(&face).copied()
    }

    /// The three faces of a closed corner, or nothing at a run-out.
    const fn corner_faces(&self) -> Option<[usize; 3]> {
        match self.kind {
            EndKind::Corner { faces, .. } => Some(faces),
            EndKind::Runout { .. } => None,
        }
    }

    /// The face a run-out ends in, or nothing at a closed corner.
    const fn cap_face(&self) -> Option<usize> {
        match self.kind {
            EndKind::Corner { .. } => None,
            EndKind::Runout { cap, .. } => Some(cap),
        }
    }
}

struct Plan<'a> {
    topology: &'a Topology,
    incidence: Incidence,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
    edges: Vec<EdgePlan>,
    /// Selected edge index -> position in `edges`.
    selected: BTreeMap<usize, usize>,
    ends: Vec<EndPlan>,
    /// Vertex index -> position in `ends`.
    ended: BTreeMap<usize, usize>,
    /// An unselected edge's end that a run-out shortens: `(edge, slot)` ->
    /// the `(vertex, face)` whose foot point it moves to.
    moved: BTreeMap<(usize, usize), (usize, usize)>,
    /// Every face the selection rebuilds.
    touched: BTreeSet<usize>,
}

impl<'a> Plan<'a> {
    #[allow(clippy::too_many_lines)]
    fn resolve(
        snapshot: SnapshotId,
        topology: &'a Topology,
        targets: &[EntityRef],
        kind: EdgeFinishKind,
        distance: f64,
        precision: PrecisionPolicy,
    ) -> Result<Self, VertexBlendError> {
        if targets.is_empty()
            || targets
                .iter()
                .any(|target| target.snapshot != snapshot || target.kind != EntityKind::Edge)
        {
            return Err(VertexBlendError::DomainUnsupported);
        }
        if topology.solids.is_empty() {
            return Err(VertexBlendError::DomainUnsupported);
        }
        let incidence = incidence(topology).ok_or(VertexBlendError::DomainUnsupported)?;
        let mut selected_indices = Vec::with_capacity(targets.len());
        for target in targets {
            let index = topology
                .edges
                .iter()
                .position(|edge| edge.id.get() == target.entity.0)
                .ok_or(VertexBlendError::DomainUnsupported)?;
            if selected_indices.contains(&index) {
                return Err(VertexBlendError::DomainUnsupported);
            }
            selected_indices.push(index);
        }
        selected_indices.sort_unstable();

        if !distance.is_finite() || distance < precision.min_feature_size {
            return Err(VertexBlendError::DomainUnsupported);
        }

        // Which of the selected edges this rung can own at all.
        let mut edges = Vec::with_capacity(selected_indices.len());
        let mut foreign = Vec::new();
        let mut foreign_curved = false;
        for index in &selected_indices {
            match read_edge(topology, &incidence, *index, kind, distance, precision) {
                Some(plan) => edges.push(plan),
                None => {
                    foreign.push(*index);
                    foreign_curved |= incidence.edge_faces[*index].is_some_and(|faces| {
                        faces
                            .into_iter()
                            .any(|face| topology.faces[face].value.surface.as_plane().is_none())
                    });
                }
            }
        }
        if edges.is_empty() {
            return Err(VertexBlendError::DomainUnsupported);
        }
        // Advising a caller to split a selection in two is only sound when the
        // two halves never touch. Where a straight run and an arc are links of
        // one chain — a prism's cap rim, say — they are one feature that
        // belongs to an earlier rung, and that rung's own sentence is the one
        // to publish, so this one stays silent.
        let touching = foreign.iter().any(|index| {
            let ends = topology.edges[*index].value.vertices;
            edges.iter().any(|plan| {
                plan.vertices.contains(&ends[0].0) || plan.vertices.contains(&ends[1].0)
            })
        });
        if !foreign.is_empty() && touching {
            return Err(VertexBlendError::DomainUnsupported);
        }
        if !foreign.is_empty() {
            let what = if foreign_curved {
                "Some of the selected edges border a curved face — a hole rim, a slot arc, or an \
                 earlier blend — and some are straight edges between flat faces. This release \
                 blends the two kinds in separate features"
            } else {
                "Some of the selected edges are not convex edges between two flat faces, so one \
                 blend cannot own the whole selection"
            };
            return Err(refuse(
                "VERTEX_BLEND_MIXED_SELECTION",
                format!(
                    "{what}: put the rims in one fillet or chamfer step and the straight edges in \
                     another. Either order works."
                ),
                false,
            ));
        }

        let selected: BTreeMap<usize, usize> = edges
            .iter()
            .enumerate()
            .map(|(position, plan)| (plan.edge, position))
            .collect();

        // Every end of every selected edge has to become something this rung
        // can close: a corner patch, or a run-out into a face beside it.
        let mut ended_vertices = Vec::new();
        for plan in &edges {
            for vertex in plan.vertices {
                if !ended_vertices.contains(&vertex) {
                    ended_vertices.push(vertex);
                }
            }
        }
        ended_vertices.sort_unstable();

        let mut ends = Vec::with_capacity(ended_vertices.len());
        for vertex in ended_vertices {
            ends.push(read_end(
                topology, &incidence, &selected, &edges, vertex, kind, distance, precision,
            )?);
        }
        let ended: BTreeMap<usize, usize> = ends
            .iter()
            .enumerate()
            .map(|(position, end)| (end.vertex, position))
            .collect();

        // Which faces the selection rebuilds, and which untouched edges a
        // run-out shortens.
        let mut touched = BTreeSet::new();
        let mut moved = BTreeMap::new();
        for plan in &edges {
            touched.extend(plan.faces);
        }
        for end in &ends {
            if let Some(faces) = end.corner_faces() {
                touched.extend(faces);
                continue;
            }
            let Some(cap) = end.cap_face() else { continue };
            touched.insert(cap);
            for side in &incidence.vertex_edges[end.vertex] {
                if selected.contains_key(side) {
                    continue;
                }
                let pair =
                    incidence.edge_faces[*side].ok_or(VertexBlendError::DomainUnsupported)?;
                // The face this side edge shares with the band is the one of
                // its two that is not the cap; the foot point there is where
                // the shortened edge now ends.
                let face = *pair
                    .iter()
                    .find(|face| **face != cap)
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                let slot = topology.edges[*side]
                    .value
                    .vertices
                    .iter()
                    .position(|vertex| vertex.0 == end.vertex)
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                moved.insert((*side, slot), (end.vertex, face));
                touched.insert(face);
            }
        }

        let plan = Self {
            topology,
            incidence,
            kind,
            distance,
            precision,
            edges,
            selected,
            ends,
            ended,
            moved,
            touched,
        };
        plan.check_fit()?;
        Ok(plan)
    }

    /// The blend has to leave a usable face and a usable band behind: every
    /// tangency line long enough to be a line, every rebuilt boundary simple,
    /// and every hole in a rebuilt face still strictly inside it.
    fn check_fit(&self) -> Result<(), VertexBlendError> {
        let minimum = self.precision.min_feature_size;
        for plan in &self.edges {
            let start = self.end_of(plan.vertices[0]);
            let end = self.end_of(plan.vertices[1]);
            for face in plan.faces {
                let (Some(from), Some(to)) = (start.tangency_on(face), end.tangency_on(face))
                else {
                    return Err(VertexBlendError::DomainUnsupported);
                };
                let along = (to - from).dot(plan.direction);
                if along <= minimum {
                    return Err(self.does_not_fit(
                        "A blend of {distance} leaves no band along one selected edge: the two \
                         ends' set-backs meet or cross.",
                    ));
                }
            }
        }
        // An edge a run-out shortens has to survive the shortening, at both
        // ends where two run-outs share it.
        for (edge, slots) in self.shortened_edges() {
            let [from, to] = self.shortened_endpoints(edge, slots)?;
            if (to - from).length() <= minimum {
                return Err(self.does_not_fit(
                    "A blend of {distance} consumes one of the edges beside it, leaving the face \
                     there nothing to stand on.",
                ));
            }
        }
        for face in &self.touched {
            self.check_face_fit(*face)?;
        }
        Ok(())
    }

    /// The one sentence every fit refusal ends with, so a caller reads the
    /// same remedy whichever check found the fault.
    fn does_not_fit(&self, what: &str) -> VertexBlendError {
        refuse(
            "VERTEX_BLEND_DISTANCE_INVALID",
            format!(
                "{} Use a smaller radius or distance.",
                what.replace("{distance}", &format!("{:.6}", self.distance))
            ),
            true,
        )
    }

    /// Every untouched edge a run-out shortens, with the ends it moves.
    fn shortened_edges(&self) -> BTreeMap<usize, [bool; 2]> {
        let mut shortened: BTreeMap<usize, [bool; 2]> = BTreeMap::new();
        for (edge, slot) in self.moved.keys() {
            shortened.entry(*edge).or_default()[*slot] = true;
        }
        shortened
    }

    /// Where one shortened edge now starts and ends.
    fn shortened_endpoints(
        &self,
        edge: usize,
        slots: [bool; 2],
    ) -> Result<[Point3; 2], VertexBlendError> {
        let record = self.topology.edges[edge].value;
        let original = record.endpoints();
        let mut moved = original;
        for (slot, point) in moved.iter_mut().enumerate() {
            if !slots[slot] {
                continue;
            }
            let (vertex, face) = self.moved[&(edge, slot)];
            *point = self
                .end_of(vertex)
                .tangency_on(face)
                .ok_or(VertexBlendError::DomainUnsupported)?;
        }
        Ok(moved)
    }

    fn check_face_fit(&self, face: usize) -> Result<(), VertexBlendError> {
        let record = &self.topology.faces[face].value;
        let Some(plane) = record.surface.as_plane() else {
            return Err(VertexBlendError::DomainUnsupported);
        };
        let outer = self
            .rebuilt_polygon(face, record.outer_loop, plane)
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let minimum = self.precision.min_feature_size;
        if polygon_area(&outer) <= minimum * minimum || polygon_self_intersects(&outer) {
            return Err(self.does_not_fit(
                "A blend of {distance} eats through one of the faces it insets: its boundary \
                 would cross itself.",
            ));
        }
        for inner in &record.inner_loops {
            let hole = self
                .rebuilt_polygon(face, *inner, plane)
                .ok_or(VertexBlendError::DomainUnsupported)?;
            for point in &hole {
                if !point_inside(&outer, *point) || distance_to_polygon(&outer, *point) <= minimum {
                    return Err(self.does_not_fit(
                        "A blend of {distance} would reach a hole or slot through one of the \
                         faces it insets. Blend the rim of that opening as its own feature, or \
                         leave the edges beside it out.",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The loop as the blend leaves it, sampled in the face's own parameters.
    fn rebuilt_polygon(&self, face: usize, loop_key: LoopKey, plane: Plane) -> Option<Vec<Point2>> {
        let loop_record = self.topology.loop_record(loop_key)?;
        let mut polygon = Vec::new();
        for coedge_key in &loop_record.value.coedges {
            let coedge = self.topology.coedge(*coedge_key)?.value;
            if self.selected.contains_key(&coedge.edge.0) {
                let plan = &self.edges[self.selected[&coedge.edge.0]];
                let [from, _] = self.tangency_span(plan, face, coedge.orientation)?;
                polygon.push(plane.project(from));
            } else if let Some(point) = self.moved_walk_start(coedge) {
                polygon.push(plane.project(point));
            } else {
                polygon.extend(sample_pcurve(coedge));
            }
            // A run-out's own end sits between the two edges it shortened, and
            // starts at the foot the arriving edge just reached.
            if self.cap_after(face, coedge).is_some() {
                polygon.push(plane.project(self.moved_walk_end(coedge)?));
            }
        }
        (polygon.len() >= 3).then_some(polygon)
    }

    /// Where an untouched edge's walk now starts, when a run-out moved it.
    fn moved_walk_start(&self, coedge: Coedge) -> Option<Point3> {
        self.moved_foot(
            coedge,
            usize::from(coedge.orientation == Orientation::Reverse),
        )
    }

    /// Where an untouched edge's walk now ends, when a run-out moved it.
    fn moved_walk_end(&self, coedge: Coedge) -> Option<Point3> {
        self.moved_foot(
            coedge,
            usize::from(coedge.orientation == Orientation::Forward),
        )
    }

    fn moved_foot(&self, coedge: Coedge, slot: usize) -> Option<Point3> {
        let (vertex, face) = self.moved.get(&(coedge.edge.0, slot)).copied()?;
        self.end_of(vertex).tangency_on(face)
    }

    /// Which band face's foot an untouched edge's walk now ends at.
    fn moved_face(&self, coedge: Coedge) -> Option<usize> {
        let slot = usize::from(coedge.orientation == Orientation::Forward);
        self.moved
            .get(&(coedge.edge.0, slot))
            .map(|(_, face)| *face)
    }

    /// The run-out whose end curve follows this coedge in this face's loop,
    /// if any: the coedge has to arrive at the run-out's vertex, and the face
    /// has to be the one the band runs out into.
    fn cap_after(&self, face: usize, coedge: Coedge) -> Option<&EndPlan> {
        if self.selected.contains_key(&coedge.edge.0) {
            return None;
        }
        let slot = usize::from(coedge.orientation == Orientation::Forward);
        let vertex = self.topology.edges[coedge.edge.0].value.vertices[slot].0;
        let end = self.ends.get(*self.ended.get(&vertex)?)?;
        (end.cap_face() == Some(face)).then_some(end)
    }

    /// The tangency line of one selected edge on one of its faces, in the
    /// order a coedge of that orientation walks it.
    fn tangency_span(
        &self,
        plan: &EdgePlan,
        face: usize,
        orientation: Orientation,
    ) -> Option<[Point3; 2]> {
        let start = self.end_of(plan.vertices[0]).tangency_on(face)?;
        let end = self.end_of(plan.vertices[1]).tangency_on(face)?;
        Some(match orientation {
            Orientation::Forward => [start, end],
            Orientation::Reverse => [end, start],
        })
    }

    fn end_of(&self, vertex: usize) -> &EndPlan {
        &self.ends[self.ended[&vertex]]
    }
}

/// Reads one selected edge, or reports that it is not this rung's to blend.
fn read_edge(
    topology: &Topology,
    incidence: &Incidence,
    edge: usize,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Option<EdgePlan> {
    let record = topology.edges[edge].value;
    let Curve3::Line { .. } = record.curve else {
        return None;
    };
    let faces = incidence.edge_faces[edge]?;
    let planes = [
        topology.faces[faces[0]].value.surface.as_plane()?,
        topology.faces[faces[1]].value.surface.as_plane()?,
    ];
    let [start, end] = record.endpoints();
    let direction = unit(end - start)?;
    let normals = [unit(planes[0].normal)?, unit(planes[1].normal)?];
    // Where each face lies, seen from the edge: the left of the coedge that
    // walks it. The forward face walks the edge's own direction.
    let inward = [
        normals[0].cross(direction),
        normals[1].cross(direction * -1.0),
    ];
    // `sin θ` and `cos θ` of the interior dihedral, measured through the
    // material. A convex edge turns through less than a half turn.
    let sine = -normals[0].dot(inward[1]);
    let cosine = inward[0].dot(inward[1]);
    let angle_tolerance = precision.angular_agreement_radians.max(1.0e-9);
    let interior = sine.atan2(cosine);
    if !(interior > angle_tolerance && interior < std::f64::consts::PI - angle_tolerance) {
        // Concave, tangent or coplanar: a different rung's business.
        return None;
    }
    let half = interior / 2.0;
    let setback = match kind {
        EdgeFinishKind::Fillet => distance * half.cos() / half.sin(),
        EdgeFinishKind::Chamfer => distance,
    };
    if !setback.is_finite() || setback < precision.min_feature_size {
        return None;
    }
    // The cylinder measures its sweep from the forward face's normal; the
    // chamfer's plane faces along the outward bisector.
    let sweep = std::f64::consts::PI - interior;
    let bevel_normal = (inward[0] * half.cos() + normals[0] * -half.sin()) * -1.0;
    Some(EdgePlan {
        edge,
        faces,
        normals,
        vertices: [record.vertices[0].0, record.vertices[1].0],
        direction,
        setback,
        sweep,
        bevel_normal,
    })
}

/// Reads one end of a selected edge — a corner the selection closes, or a
/// run-out into the face beside it — or refuses it by name.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn read_end(
    topology: &Topology,
    incidence: &Incidence,
    selected: &BTreeMap<usize, usize>,
    edges: &[EdgePlan],
    vertex: usize,
    kind: EdgeFinishKind,
    distance: f64,
    precision: PrecisionPolicy,
) -> Result<EndPlan, VertexBlendError> {
    let incident = &incidence.vertex_edges[vertex];
    let mut faces = BTreeSet::new();
    let mut curved = false;
    for edge in incident {
        if let Some(pair) = incidence.edge_faces[*edge] {
            for face in pair {
                faces.insert(face);
                curved |= topology.faces[face].value.surface.as_plane().is_none();
            }
        }
    }
    let unselected = incident
        .iter()
        .filter(|edge| !selected.contains_key(*edge))
        .count();
    if curved {
        // The corner carries an earlier blend or a curved wall. When that
        // blend is a cylinder of a different size, say so precisely: a corner
        // takes one blend size, not two.
        let mismatch = incident
            .iter()
            .filter(|edge| !selected.contains_key(*edge))
            .filter_map(|edge| incidence.edge_faces[*edge])
            .flatten()
            .filter_map(|face| match topology.faces[face].value.surface {
                Surface::Cylinder(cylinder) => Some(cylinder.radius),
                _ => None,
            })
            .find(|radius| (radius - distance).abs() > precision.linear_agreement);
        if let Some(radius) = mismatch {
            return Err(refuse(
                "VERTEX_BLEND_RADIUS_MISMATCH",
                format!(
                    "One corner of this selection already carries a blend of {radius:.6}, and this \
                     one is {distance:.6}. A corner patch needs a single size across the three \
                     edges that meet there: blend them together at one size, or leave that corner \
                     out of the selection."
                ),
                false,
            ));
        }
        return Err(refuse(
            "VERTEX_BLEND_CORNER_CURVED",
            "One corner of this selection meets a curved face — a hole wall, a slot arc, or a \
             blend an earlier feature left. This release closes a corner only where three flat \
             faces meet: blend those edges in the same feature as the ones that round this \
             corner, or leave this corner's edges for a later feature."
                .to_owned(),
            false,
        ));
    }
    // Three of three is a corner patch and one of three is a run-out. Two of
    // three is a seam: the bands meet along one curve — an ellipse under a
    // fillet, where two equal crossing cylinders meet, and a line under a
    // chamfer — running from the shared face's new corner to the new end of
    // the edge left sharp, which trims back by exactly the blend size. The
    // vocabulary carries both curves; what it does not yet carry is a third
    // ending in `EndKind` to build them from. ADR 0043 derives the seam and
    // pins the volume it must produce.
    if unselected > 0 && incident.len() - unselected > 1 {
        return Err(refuse(
            "VERTEX_BLEND_CORNER_INCOMPLETE",
            format!(
                "A corner of this selection has {unselected} of its {} edges left out, and this \
                 release closes a corner only with all of them. Select the other edge or edges at \
                 that corner as well, or blend it in a later feature.",
                incident.len()
            ),
            false,
        ));
    }
    if incident.len() != 3 || faces.len() != 3 {
        return Err(refuse(
            "VERTEX_BLEND_CORNER_UNSUPPORTED",
            format!(
                "A corner of this selection joins {} edges and {} faces. This release closes a \
                 corner where exactly three edges and three flat faces meet.",
                incident.len(),
                faces.len()
            ),
            false,
        ));
    }
    let faces: Vec<usize> = faces.into_iter().collect();
    let faces = [faces[0], faces[1], faces[2]];
    let mut normals = [Vector3::default(); 3];
    let mut offsets = [0.0; 3];
    for (slot, face) in faces.into_iter().enumerate() {
        let plane = topology.faces[face]
            .value
            .surface
            .as_plane()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let normal = unit(plane.normal).ok_or(VertexBlendError::DomainUnsupported)?;
        normals[slot] = normal;
        offsets[slot] = plane.origin.as_vector().dot(normal);
    }
    let angle_tolerance = precision.angular_agreement_radians.max(1.0e-9);
    let degenerate = || {
        refuse(
            "VERTEX_BLEND_CORNER_UNSUPPORTED",
            "Three faces of one corner of this selection are too nearly parallel for a blend to \
             meet all of them."
                .to_owned(),
            false,
        )
    };
    if unselected > 0 {
        return read_runout(
            topology,
            incidence,
            selected,
            edges,
            vertex,
            kind,
            distance,
            RunoutFrame {
                faces,
                normals,
                offsets,
                angle_tolerance,
            },
        );
    }
    match kind {
        EdgeFinishKind::Fillet => {
            // The ball's centre is the one point at the blend radius inside
            // all three planes.
            let inset = [
                offsets[0] - distance,
                offsets[1] - distance,
                offsets[2] - distance,
            ];
            let centre =
                intersect_three_planes(normals, inset, angle_tolerance).ok_or_else(degenerate)?;
            let (frame, sweep) =
                sphere_frame(normals, angle_tolerance).ok_or_else(square_refusal)?;
            Ok(EndPlan {
                vertex,
                tangency: faces
                    .into_iter()
                    .enumerate()
                    .map(|(slot, face)| (face, centre + normals[slot] * distance))
                    .collect(),
                centre,
                kind: EndKind::Corner {
                    faces,
                    frame,
                    sweep,
                },
            })
        }
        EdgeFinishKind::Chamfer => {
            // Each face keeps the point where the two set-back lines on it
            // meet: the intersection of that face with the two bevel planes.
            let mut bevels = BTreeMap::new();
            for edge in incident {
                let plan = &edges[selected[edge]];
                let anchor = corner_point(topology, plan, vertex)
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                let origin = anchor + plan.normals[0].cross(plan.direction) * plan.setback;
                bevels.insert(*edge, (plan.bevel_normal, origin));
            }
            let mut tangency = BTreeMap::new();
            for (slot, face) in faces.into_iter().enumerate() {
                let mut cutters = incident
                    .iter()
                    .filter(|edge| {
                        incidence.edge_faces[**edge].is_some_and(|pair| pair.contains(&face))
                    })
                    .filter_map(|edge| bevels.get(edge).copied());
                let (first_normal, first_origin) = cutters.next().ok_or_else(degenerate)?;
                let (second_normal, second_origin) = cutters.next().ok_or_else(degenerate)?;
                if cutters.next().is_some() {
                    return Err(degenerate());
                }
                tangency.insert(
                    face,
                    intersect_three_planes(
                        [normals[slot], first_normal, second_normal],
                        [
                            offsets[slot],
                            first_origin.as_vector().dot(first_normal),
                            second_origin.as_vector().dot(second_normal),
                        ],
                        angle_tolerance,
                    )
                    .ok_or_else(degenerate)?,
                );
            }
            Ok(EndPlan {
                vertex,
                tangency,
                centre: topology.vertices[vertex].value.point,
                kind: EndKind::Corner {
                    faces,
                    frame: [0, 1, 2],
                    sweep: 0.0,
                },
            })
        }
    }
}

/// What `read_end` already worked out about the three flat faces at a vertex,
/// handed on to the run-out reader.
struct RunoutFrame {
    faces: [usize; 3],
    normals: [Vector3; 3],
    offsets: [f64; 3],
    angle_tolerance: f64,
}

/// Reads the one end of a band that stops against the face beside it, rather
/// than turning a corner: the ball's end circle lies in that face's own plane,
/// and the two edges it meets there shorten to the circle's feet.
#[allow(clippy::too_many_arguments)]
fn read_runout(
    topology: &Topology,
    incidence: &Incidence,
    selected: &BTreeMap<usize, usize>,
    edges: &[EdgePlan],
    vertex: usize,
    kind: EdgeFinishKind,
    distance: f64,
    frame: RunoutFrame,
) -> Result<EndPlan, VertexBlendError> {
    let RunoutFrame {
        faces,
        normals,
        offsets,
        angle_tolerance,
    } = frame;
    let edge = *incidence.vertex_edges[vertex]
        .iter()
        .find(|edge| selected.contains_key(*edge))
        .ok_or(VertexBlendError::DomainUnsupported)?;
    let plan = &edges[selected[&edge]];
    let cap_slot = faces
        .iter()
        .position(|face| !plan.faces.contains(face))
        .ok_or(VertexBlendError::DomainUnsupported)?;
    let cap = faces[cap_slot];
    // The band's end curve has to lie in the cap face. Under a chamfer it is
    // the straight line where the bevel plane cuts that face, whatever the
    // angle; under a fillet it is the ball's end circle, which is planar only
    // where the cap is square to the edge.
    let cap_plane = topology.faces[cap]
        .value
        .surface
        .as_plane()
        .ok_or(VertexBlendError::DomainUnsupported)?;
    if matches!(kind, EdgeFinishKind::Fillet)
        && ((1.0 - normals[cap_slot].dot(plan.direction).abs()) > angle_tolerance
            || !orthonormal(cap_plane, 1.0e-9))
    {
        return Err(refuse(
            "VERTEX_BLEND_RUNOUT_NOT_SQUARE",
            "A fillet that stops part way along a body ends on the face across its edge, and this \
             kernel can carry that end as a circle only where that face is square to the edge — a \
             box corner, a prism cap, a pocket floor. One end of this selection is not: chamfer it \
             instead, which ends in a straight line and needs no squareness, or select the other \
             edges at that end as well so the blend turns a corner there."
                .to_owned(),
            false,
        ));
    }
    // Every edge beside the run-out shortens, so each has to be a straight one
    // this rung can move an end of.
    for side in &incidence.vertex_edges[vertex] {
        if *side == edge {
            continue;
        }
        if !matches!(topology.edges[*side].value.curve, Curve3::Line { .. }) {
            return Err(refuse(
                "VERTEX_BLEND_RUNOUT_UNSUPPORTED",
                "One end of this selection stops against a curved edge, which this release cannot \
                 shorten to meet the blend. Select the edges at that end as well, or blend it in \
                 a later feature."
                    .to_owned(),
                false,
            ));
        }
    }
    let degenerate = || {
        refuse(
            "VERTEX_BLEND_CORNER_UNSUPPORTED",
            "Three faces at one end of this selection are too nearly parallel for a blend to end \
             between them."
                .to_owned(),
            false,
        )
    };
    let blend_slots: [usize; 2] = std::array::from_fn(|slot| {
        faces
            .iter()
            .position(|face| *face == plan.faces[slot])
            .unwrap_or(cap_slot)
    });
    let (centre, tangency) = match kind {
        EdgeFinishKind::Fillet => {
            // The ball's centre where its axis meets the cap: at the blend
            // radius inside both band faces, and on the cap itself.
            let centre = intersect_three_planes(
                [
                    normals[blend_slots[0]],
                    normals[blend_slots[1]],
                    normals[cap_slot],
                ],
                [
                    offsets[blend_slots[0]] - distance,
                    offsets[blend_slots[1]] - distance,
                    offsets[cap_slot],
                ],
                angle_tolerance,
            )
            .ok_or_else(degenerate)?;
            (
                centre,
                (0..2)
                    .map(|slot| {
                        (
                            plan.faces[slot],
                            centre + normals[blend_slots[slot]] * distance,
                        )
                    })
                    .collect(),
            )
        }
        EdgeFinishKind::Chamfer => {
            // Each foot is where the face, the bevel plane and the cap meet.
            let anchor =
                corner_point(topology, plan, vertex).ok_or(VertexBlendError::DomainUnsupported)?;
            let bevel_origin = anchor + plan.normals[0].cross(plan.direction) * plan.setback;
            let bevel_offset = bevel_origin.as_vector().dot(plan.bevel_normal);
            let mut tangency = BTreeMap::new();
            for slot in 0..2 {
                tangency.insert(
                    plan.faces[slot],
                    intersect_three_planes(
                        [
                            normals[blend_slots[slot]],
                            plan.bevel_normal,
                            normals[cap_slot],
                        ],
                        [offsets[blend_slots[slot]], bevel_offset, offsets[cap_slot]],
                        angle_tolerance,
                    )
                    .ok_or_else(degenerate)?,
                );
            }
            (topology.vertices[vertex].value.point, tangency)
        }
    };
    Ok(EndPlan {
        vertex,
        tangency,
        centre,
        kind: EndKind::Runout { edge, cap },
    })
}

fn square_refusal() -> VertexBlendError {
    refuse(
        "VERTEX_BLEND_CORNER_NOT_SQUARE",
        "A fillet closes a corner with a piece of a sphere, which this kernel can carry only when \
         one of the three faces there is square to the other two — a box corner, or a flat cap \
         meeting two walls. One corner of this selection is not: chamfer it instead, which needs \
         no such squareness, or leave those edges out."
            .to_owned(),
        false,
    )
}

/// The pole face and equator pair of a corner's sphere patch: the face whose
/// normal is square to the other two, and the other two in increasing azimuth
/// about it.
fn sphere_frame(normals: [Vector3; 3], angle_tolerance: f64) -> Option<([usize; 3], f64)> {
    for pole in 0..3 {
        let others = [(pole + 1) % 3, (pole + 2) % 3];
        if normals[pole].dot(normals[others[0]]).abs() > angle_tolerance
            || normals[pole].dot(normals[others[1]]).abs() > angle_tolerance
        {
            continue;
        }
        for pair in [others, [others[1], others[0]]] {
            let radial = normals[pole].cross(normals[pair[0]]);
            let sweep = normals[pair[1]]
                .dot(radial)
                .atan2(normals[pair[1]].dot(normals[pair[0]]));
            if sweep > angle_tolerance && sweep < std::f64::consts::PI - angle_tolerance {
                return Some(([pole, pair[0], pair[1]], sweep));
            }
        }
    }
    None
}

/// The end of a selected edge at one of its vertices.
fn corner_point(topology: &Topology, plan: &EdgePlan, vertex: usize) -> Option<Point3> {
    let record = topology.edges[plan.edge].value;
    let [start, end] = record.endpoints();
    if plan.vertices[0] == vertex {
        Some(start)
    } else if plan.vertices[1] == vertex {
        Some(end)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// The keys one rebuilt body needs to find again while it is being written.
#[derive(Default)]
struct Keys {
    /// An untouched vertex, once.
    vertex: BTreeMap<usize, VertexKey>,
    /// A blended corner's tangency point on one face.
    corner_vertex: BTreeMap<(usize, usize), VertexKey>,
    /// An untouched edge, once.
    edge: BTreeMap<usize, EdgeKey>,
    /// A selected edge's tangency line on one of its faces.
    tangency: BTreeMap<(usize, usize), EdgeKey>,
    /// The arc or segment where one selected edge's patch meets one corner
    /// patch, with the vertices it is stored between.
    corner_edge: BTreeMap<(usize, usize), (EdgeKey, VertexKey, VertexKey)>,
    /// Old face index -> new face key.
    face: BTreeMap<usize, FaceKey>,
}

struct Builder<'a> {
    plan: &'a Plan<'a>,
    topology: Topology,
    next_id: u64,
    keys: Keys,
}

impl Builder<'_> {
    fn allocate(&mut self) -> EntityId {
        let id = EntityId::from_raw(self.next_id);
        self.next_id += 1;
        id
    }

    fn vertex(&mut self, point: Point3) -> VertexKey {
        let key = VertexKey(self.topology.vertices.len());
        let id = self.allocate();
        self.topology.vertices.push(Record {
            id,
            value: Vertex { point },
        });
        key
    }

    fn line_edge(&mut self, from: VertexKey, to: VertexKey) -> EdgeKey {
        let start = self.topology.vertices[from.0].value.point;
        let end = self.topology.vertices[to.0].value.point;
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record {
            id,
            value: Edge::line([from, to], [start, end]),
        });
        key
    }

    #[allow(clippy::too_many_arguments)]
    fn arc_edge(
        &mut self,
        vertices: [VertexKey; 2],
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
        sweep: f64,
    ) -> EdgeKey {
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record {
            id,
            value: Edge {
                vertices,
                curve: Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                },
                parameter_range: ParameterRange::new(0.0, sweep),
            },
        });
        key
    }

    fn push_loop(&mut self, uses: Vec<(EdgeKey, Orientation, Curve2, ParameterRange)>) -> LoopKey {
        let mut coedges = Vec::with_capacity(uses.len());
        for (edge, orientation, pcurve, parameter_range) in uses {
            let key = CoedgeKey(self.topology.coedges.len());
            let id = self.allocate();
            self.topology.coedges.push(Record {
                id,
                value: Coedge {
                    edge,
                    orientation,
                    pcurve,
                    parameter_range,
                },
            });
            coedges.push(key);
        }
        let key = LoopKey(self.topology.loops.len());
        let id = self.allocate();
        self.topology.loops.push(Record {
            id,
            value: Loop { coedges },
        });
        key
    }

    fn push_face(
        &mut self,
        surface: Surface,
        outer_loop: LoopKey,
        inner_loops: Vec<LoopKey>,
        role: FaceRole,
    ) -> FaceKey {
        let key = FaceKey(self.topology.faces.len());
        let id = self.allocate();
        self.topology.faces.push(Record {
            id,
            value: Face {
                surface,
                outer_loop,
                inner_loops,
                role,
            },
        });
        key
    }
}

impl Plan<'_> {
    #[allow(clippy::too_many_lines)]
    fn build(&self) -> Result<Topology, VertexBlendError> {
        let mut builder = Builder {
            plan: self,
            topology: Topology::default(),
            next_id: 1,
            keys: Keys::default(),
        };

        // Vertices. Every vertex a band ends at becomes one point per face the
        // blend meets there; every other vertex is copied where it stands.
        for (index, vertex) in self.topology.vertices.iter().enumerate() {
            if self.ended.contains_key(&index) {
                continue;
            }
            let key = builder.vertex(vertex.value.point);
            builder.keys.vertex.insert(index, key);
        }
        for end in &self.ends {
            for (face, point) in &end.tangency {
                let key = builder.vertex(*point);
                builder.keys.corner_vertex.insert((end.vertex, *face), key);
            }
        }

        // Edges. Untouched edges keep their curve, except where a run-out
        // shortens one; a selected edge becomes one tangency line per face,
        // and each of its ends one boundary of the patch or run-out there.
        let shortened = self.shortened_edges();
        for index in 0..self.topology.edges.len() {
            if self.selected.contains_key(&index) {
                continue;
            }
            let record = self.topology.edges[index].value;
            let moved = shortened.get(&index).copied().unwrap_or([false; 2]);
            let mut vertices = [VertexKey(0); 2];
            for (slot, key) in vertices.iter_mut().enumerate() {
                *key = if moved[slot] {
                    let (vertex, face) = self.moved[&(index, slot)];
                    builder.keys.corner_vertex[&(vertex, face)]
                } else {
                    *builder
                        .keys
                        .vertex
                        .get(&record.vertices[slot].0)
                        .ok_or(VertexBlendError::DomainUnsupported)?
                };
            }
            let key = EdgeKey(builder.topology.edges.len());
            let id = builder.allocate();
            let value = if moved == [false; 2] {
                Edge { vertices, ..record }
            } else {
                Edge::line(vertices, self.shortened_endpoints(index, moved)?)
            };
            builder.topology.edges.push(Record { id, value });
            builder.keys.edge.insert(index, key);
        }
        for plan in &self.edges {
            for face in plan.faces {
                let from = builder.keys.corner_vertex[&(plan.vertices[0], face)];
                let to = builder.keys.corner_vertex[&(plan.vertices[1], face)];
                let key = builder.line_edge(from, to);
                builder.keys.tangency.insert((plan.edge, face), key);
            }
        }
        // Both a corner patch and a run-out close a band with the same curve:
        // the arc a fillet's ball leaves between the two faces, or the segment
        // a chamfer's bevel leaves there.
        for end in &self.ends {
            for edge in &self.incidence.vertex_edges[end.vertex] {
                let Some(position) = self.selected.get(edge) else {
                    continue;
                };
                let plan = &self.edges[*position];
                let from = builder.keys.corner_vertex[&(end.vertex, plan.faces[0])];
                let to = builder.keys.corner_vertex[&(end.vertex, plan.faces[1])];
                let key = match self.kind {
                    EdgeFinishKind::Fillet => builder.arc_edge(
                        [from, to],
                        end.centre,
                        plan.normals[0],
                        plan.direction.cross(plan.normals[0]),
                        self.distance,
                        plan.sweep,
                    ),
                    EdgeFinishKind::Chamfer => builder.line_edge(from, to),
                };
                builder
                    .keys
                    .corner_edge
                    .insert((end.vertex, *edge), (key, from, to));
            }
        }

        // Faces: every face of the body, then one patch per selected edge and
        // one per corner.
        for index in 0..self.topology.faces.len() {
            let key = if self.touched.contains(&index) {
                builder.rebuild_planar_face(index)?
            } else {
                builder.copy_face(index)?
            };
            builder.keys.face.insert(index, key);
        }
        let mut extra = Vec::new();
        for (ordinal, plan) in self.edges.iter().enumerate() {
            extra.push((
                self.incidence.face_shell[plan.faces[0]],
                builder.build_blend_face(plan, ordinal)?,
            ));
        }
        for end in &self.ends {
            let Some(faces) = end.corner_faces() else {
                // A run-out adds no face of its own: its end curve joins the
                // band to a face the body already had.
                continue;
            };
            extra.push((
                self.incidence.face_shell[faces[0]],
                builder.build_corner_face(end, faces)?,
            ));
        }

        // Shells and solids keep the body's own structure, cavities included.
        for (shell_index, shell) in self.topology.shells.iter().enumerate() {
            let mut faces = shell
                .value
                .faces
                .iter()
                .map(|face| {
                    builder
                        .keys
                        .face
                        .get(&face.0)
                        .copied()
                        .ok_or(VertexBlendError::DomainUnsupported)
                })
                .collect::<Result<Vec<_>, _>>()?;
            faces.extend(
                extra
                    .iter()
                    .filter(|(owner, _)| *owner == shell_index)
                    .map(|(_, key)| *key),
            );
            let id = builder.allocate();
            builder.topology.shells.push(Record {
                id,
                value: Shell { faces },
            });
        }
        for solid in &self.topology.solids {
            let id = builder.allocate();
            builder.topology.solids.push(Record {
                id,
                value: solid.value.clone(),
            });
        }

        Ok(builder.topology)
    }
}

impl Builder<'_> {
    /// Copies one face the selection does not touch, keeping its carrier, its
    /// loops and their pcurves exactly as they stand.
    fn copy_face(&mut self, index: usize) -> Result<FaceKey, VertexBlendError> {
        let record = self.plan.topology.faces[index].value.clone();
        let outer = self.copy_loop(record.outer_loop)?;
        let inner = record
            .inner_loops
            .iter()
            .map(|loop_key| self.copy_loop(*loop_key))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.push_face(record.surface, outer, inner, record.role))
    }

    fn copy_loop(&mut self, loop_key: LoopKey) -> Result<LoopKey, VertexBlendError> {
        let source = self
            .plan
            .topology
            .loop_record(loop_key)
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let mut uses = Vec::with_capacity(source.value.coedges.len());
        for coedge_key in &source.value.coedges {
            let coedge = self
                .plan
                .topology
                .coedge(*coedge_key)
                .ok_or(VertexBlendError::DomainUnsupported)?
                .value;
            let edge = self
                .keys
                .edge
                .get(&coedge.edge.0)
                .copied()
                .ok_or(VertexBlendError::DomainUnsupported)?;
            uses.push((
                edge,
                coedge.orientation,
                coedge.pcurve,
                coedge.parameter_range,
            ));
        }
        Ok(self.push_loop(uses))
    }

    /// Rebuilds one planar face the selection insets: every selected edge on
    /// it becomes that edge's tangency line, and every other boundary stands.
    fn rebuild_planar_face(&mut self, index: usize) -> Result<FaceKey, VertexBlendError> {
        let record = self.plan.topology.faces[index].value.clone();
        let plane = record
            .surface
            .as_plane()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let outer = self.rebuild_loop(index, record.outer_loop, plane)?;
        let inner = record
            .inner_loops
            .iter()
            .map(|loop_key| self.rebuild_loop(index, *loop_key, plane))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.push_face(record.surface, outer, inner, record.role))
    }

    fn rebuild_loop(
        &mut self,
        face: usize,
        loop_key: LoopKey,
        plane: Plane,
    ) -> Result<LoopKey, VertexBlendError> {
        let source = self
            .plan
            .topology
            .loop_record(loop_key)
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let mut uses = Vec::with_capacity(source.value.coedges.len());
        for coedge_key in &source.value.coedges {
            let coedge = self
                .plan
                .topology
                .coedge(*coedge_key)
                .ok_or(VertexBlendError::DomainUnsupported)?
                .value;
            if let Some(position) = self.plan.selected.get(&coedge.edge.0) {
                let plan = &self.plan.edges[*position];
                let edge = self
                    .keys
                    .tangency
                    .get(&(plan.edge, face))
                    .copied()
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                let [from, to] = self
                    .plan
                    .tangency_span(plan, face, coedge.orientation)
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                let (pcurve, range) =
                    Curve2::line_segment([plane.project(from), plane.project(to)]);
                uses.push((edge, coedge.orientation, pcurve, range));
            } else {
                let edge = self
                    .keys
                    .edge
                    .get(&coedge.edge.0)
                    .copied()
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                // A run-out shortened this edge, so its trace on the face is a
                // shorter segment of the same line.
                let moved = [
                    self.plan.moved_walk_start(coedge),
                    self.plan.moved_walk_end(coedge),
                ];
                if moved == [None, None] {
                    uses.push((
                        edge,
                        coedge.orientation,
                        coedge.pcurve,
                        coedge.parameter_range,
                    ));
                } else {
                    let standing = self.walk_endpoints(coedge)?;
                    let (pcurve, range) = Curve2::line_segment([
                        plane.project(moved[0].unwrap_or(standing[0])),
                        plane.project(moved[1].unwrap_or(standing[1])),
                    ]);
                    uses.push((edge, coedge.orientation, pcurve, range));
                }
            }
            // The end curve of a run-out follows the edge that arrives at its
            // foot, closing this face's loop around the corner it lost.
            if let Some(end) = self.plan.cap_after(face, coedge) {
                uses.push(self.cap_use(end, coedge, plane)?);
            }
        }
        Ok(self.push_loop(uses))
    }

    /// One edge's own endpoints in the order a coedge walks it.
    fn walk_endpoints(&self, coedge: Coedge) -> Result<[Point3; 2], VertexBlendError> {
        let [start, end] = self.plan.topology.edges[coedge.edge.0].value.endpoints();
        Ok(match coedge.orientation {
            Orientation::Forward => [start, end],
            Orientation::Reverse => [end, start],
        })
    }

    /// The run-out's end curve as the cap face walks it: the arc a fillet's
    /// ball leaves in that plane, or the chamfer's straight end.
    fn cap_use(
        &self,
        end: &EndPlan,
        arriving: Coedge,
        plane: Plane,
    ) -> Result<(EdgeKey, Orientation, Curve2, ParameterRange), VertexBlendError> {
        let edge = match end.kind {
            EndKind::Corner { .. } => return Err(VertexBlendError::DomainUnsupported),
            EndKind::Runout { edge, .. } => edge,
        };
        let plan = &self.plan.edges[self.plan.selected[&edge]];
        let (key, stored_from, _) = self
            .keys
            .corner_edge
            .get(&(end.vertex, edge))
            .copied()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let arrived = self
            .plan
            .moved_face(arriving)
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let from = end
            .tangency_on(plan.faces[0])
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let to = end
            .tangency_on(plan.faces[1])
            .ok_or(VertexBlendError::DomainUnsupported)?;
        // The end curve is stored from the first band face's foot to the
        // second's, so the walk runs with it only when it arrived at the
        // first.
        let forward = arrived == plan.faces[0];
        debug_assert_eq!(
            stored_from,
            self.keys.corner_vertex[&(end.vertex, plan.faces[0])]
        );
        let orientation = if forward {
            Orientation::Forward
        } else {
            Orientation::Reverse
        };
        let (pcurve, range) = match self.plan.kind {
            EdgeFinishKind::Fillet => {
                let u = plan.normals[0];
                let v = plan.direction.cross(u);
                (
                    Curve2::Circle {
                        center: plane.project(end.centre),
                        u: plane_direction(plane, u),
                        v: plane_direction(plane, v),
                        radius: self.plan.distance,
                    },
                    if forward {
                        ParameterRange::new(0.0, plan.sweep)
                    } else {
                        ParameterRange::new(plan.sweep, 0.0)
                    },
                )
            }
            EdgeFinishKind::Chamfer => Curve2::line_segment(if forward {
                [plane.project(from), plane.project(to)]
            } else {
                [plane.project(to), plane.project(from)]
            }),
        };
        Ok((key, orientation, pcurve, range))
    }

    /// The band one selected edge carries: a cylinder for a fillet, the plane
    /// through the two set-back lines for a chamfer.
    fn build_blend_face(
        &mut self,
        plan: &EdgePlan,
        ordinal: usize,
    ) -> Result<FaceKey, VertexBlendError> {
        let start = self.plan.end_of(plan.vertices[0]);
        let end = self.plan.end_of(plan.vertices[1]);
        let forward = self.keys.tangency[&(plan.edge, plan.faces[0])];
        let reverse = self.keys.tangency[&(plan.edge, plan.faces[1])];
        let (surface, corners): (Surface, [Point2; 4]) = match self.plan.kind {
            EdgeFinishKind::Fillet => {
                // The cylinder measures its sweep from the forward face's
                // normal and its length along the edge from the first corner.
                let axis = plan.direction;
                let origin = start.centre;
                let length = (end.centre - origin).dot(axis);
                (
                    Surface::Cylinder(Cylinder {
                        origin,
                        axis,
                        radial_u: plan.normals[0],
                        radial_v: axis.cross(plan.normals[0]),
                        radius: self.plan.distance,
                        angular_sign: 1.0,
                    }),
                    [
                        Point2::new(0.0, length),
                        Point2::new(0.0, 0.0),
                        Point2::new(plan.sweep, 0.0),
                        Point2::new(plan.sweep, length),
                    ],
                )
            }
            EdgeFinishKind::Chamfer => {
                let origin = start
                    .tangency_on(plan.faces[0])
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                let plane = Plane::new(
                    origin,
                    plan.direction,
                    plan.bevel_normal.cross(plan.direction),
                );
                let at = |end: &EndPlan, face: usize| {
                    end.tangency_on(face)
                        .map(|point| plane.project(point))
                        .ok_or(VertexBlendError::DomainUnsupported)
                };
                (
                    Surface::Plane(plane),
                    [
                        at(end, plan.faces[0])?,
                        at(start, plan.faces[0])?,
                        at(start, plan.faces[1])?,
                        at(end, plan.faces[1])?,
                    ],
                )
            }
        };
        // The band walks the forward face's line against the edge, crosses the
        // first corner from that face to the other, walks the reverse face's
        // line back, and crosses the second corner the other way: the winding
        // an outward normal asks for.
        let entering = self.corner_crossing(plan, 0, plan.faces[0])?;
        let leaving = self.corner_crossing(plan, 1, plan.faces[1])?;
        let uses = vec![
            {
                let (pcurve, range) = Curve2::line_segment([corners[0], corners[1]]);
                (forward, Orientation::Reverse, pcurve, range)
            },
            {
                let (pcurve, range) = Curve2::line_segment([corners[1], corners[2]]);
                (entering.0, entering.1, pcurve, range)
            },
            {
                let (pcurve, range) = Curve2::line_segment([corners[2], corners[3]]);
                (reverse, Orientation::Forward, pcurve, range)
            },
            {
                let (pcurve, range) = Curve2::line_segment([corners[3], corners[0]]);
                (leaving.0, leaving.1, pcurve, range)
            },
        ];
        let loop_key = self.push_loop(uses);
        Ok(self.push_face(
            surface,
            loop_key,
            Vec::new(),
            FaceRole::FeatureSide(u32::try_from(ordinal).unwrap_or(u32::MAX)),
        ))
    }

    /// The patch that closes one corner: the sphere octant a fillet's ball
    /// leaves, or the triangle a chamfer's three set-back lines bound.
    fn build_corner_face(
        &mut self,
        corner: &EndPlan,
        faces: [usize; 3],
    ) -> Result<FaceKey, VertexBlendError> {
        match self.plan.kind {
            EdgeFinishKind::Fillet => self.build_sphere_patch(corner, faces),
            EdgeFinishKind::Chamfer => self.build_corner_triangle(corner, faces),
        }
    }

    fn build_sphere_patch(
        &mut self,
        corner: &EndPlan,
        faces: [usize; 3],
    ) -> Result<FaceKey, VertexBlendError> {
        let quarter = std::f64::consts::FRAC_PI_2;
        let EndKind::Corner { frame, sweep, .. } = corner.kind else {
            return Err(VertexBlendError::DomainUnsupported);
        };
        let pole = faces[frame[0]];
        let first = faces[frame[1]];
        let second = faces[frame[2]];
        let pole_normal = unit(
            self.plan.topology.faces[pole]
                .value
                .surface
                .as_plane()
                .ok_or(VertexBlendError::DomainUnsupported)?
                .normal,
        )
        .ok_or(VertexBlendError::DomainUnsupported)?;
        let first_normal = unit(
            self.plan.topology.faces[first]
                .value
                .surface
                .as_plane()
                .ok_or(VertexBlendError::DomainUnsupported)?
                .normal,
        )
        .ok_or(VertexBlendError::DomainUnsupported)?;
        let surface = Surface::Sphere(Sphere {
            origin: corner.centre,
            axis: pole_normal,
            radial_u: first_normal,
            radial_v: pole_normal.cross(first_normal),
            radius: self.plan.distance,
            angular_sign: 1.0,
        });
        // The equator runs between the two faces the pole is square to, and a
        // meridian rises from each of them to the pole, where both meet at one
        // vertex and the loop closes with three sides.
        let equator = self.corner_side(corner, first, second)?;
        let rising = self.corner_side(corner, second, pole)?;
        let falling = self.corner_side(corner, pole, first)?;
        let uses = vec![
            {
                let (pcurve, range) =
                    Curve2::line_segment([Point2::new(0.0, 0.0), Point2::new(sweep, 0.0)]);
                (equator.0, equator.1, pcurve, range)
            },
            {
                let (pcurve, range) =
                    Curve2::line_segment([Point2::new(sweep, 0.0), Point2::new(sweep, quarter)]);
                (rising.0, rising.1, pcurve, range)
            },
            {
                let (pcurve, range) =
                    Curve2::line_segment([Point2::new(0.0, quarter), Point2::new(0.0, 0.0)]);
                (falling.0, falling.1, pcurve, range)
            },
        ];
        let loop_key = self.push_loop(uses);
        Ok(self.push_face(surface, loop_key, Vec::new(), FaceRole::FeatureEnd))
    }

    fn build_corner_triangle(
        &mut self,
        corner: &EndPlan,
        faces: [usize; 3],
    ) -> Result<FaceKey, VertexBlendError> {
        let mut points = [Point3::default(); 3];
        for (slot, face) in faces.into_iter().enumerate() {
            points[slot] = corner
                .tangency_on(face)
                .ok_or(VertexBlendError::DomainUnsupported)?;
        }
        let mut normal = unit((points[1] - points[0]).cross(points[2] - points[0]))
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let outward = faces.into_iter().try_fold(
            Vector3::default(),
            |sum, face| -> Result<Vector3, VertexBlendError> {
                let plane = self.plan.topology.faces[face]
                    .value
                    .surface
                    .as_plane()
                    .ok_or(VertexBlendError::DomainUnsupported)?;
                Ok(sum + unit(plane.normal).ok_or(VertexBlendError::DomainUnsupported)?)
            },
        )?;
        if normal.dot(outward) < 0.0 {
            normal = normal * -1.0;
        }
        // The triangle's three sides are the three bands' ends; walking them
        // so the loop winds about the outward normal fixes the order.
        let order = if (points[1] - points[0])
            .cross(points[2] - points[0])
            .dot(normal)
            > 0.0
        {
            [0, 1, 2]
        } else {
            [0, 2, 1]
        };
        let anchor = points[order[0]];
        let u = unit(points[order[1]] - anchor).ok_or(VertexBlendError::DomainUnsupported)?;
        let plane = Plane::new(anchor, u, normal.cross(u));
        let mut uses = Vec::with_capacity(3);
        for step in 0..3 {
            let from = faces[order[step]];
            let to = faces[order[(step + 1) % 3]];
            let side = self.corner_side(corner, from, to)?;
            let (pcurve, range) = Curve2::line_segment([
                plane.project(points[order[step]]),
                plane.project(points[order[(step + 1) % 3]]),
            ]);
            uses.push((side.0, side.1, pcurve, range));
        }
        let loop_key = self.push_loop(uses);
        Ok(self.push_face(
            Surface::Plane(plane),
            loop_key,
            Vec::new(),
            FaceRole::FeatureEnd,
        ))
    }

    /// The end of one band at one of its corners, walked away from the
    /// tangency on `from`.
    fn corner_crossing(
        &self,
        plan: &EdgePlan,
        end: usize,
        from: usize,
    ) -> Result<(EdgeKey, Orientation), VertexBlendError> {
        let vertex = plan.vertices[end];
        let (key, stored_from, _) = self
            .keys
            .corner_edge
            .get(&(vertex, plan.edge))
            .copied()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let walked_from = self
            .keys
            .corner_vertex
            .get(&(vertex, from))
            .copied()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        Ok((
            key,
            if stored_from == walked_from {
                Orientation::Forward
            } else {
                Orientation::Reverse
            },
        ))
    }

    /// The corner patch's boundary running from the tangency on `from` to the
    /// one on `to`, with the orientation that walk needs.
    fn corner_side(
        &self,
        corner: &EndPlan,
        from: usize,
        to: usize,
    ) -> Result<(EdgeKey, Orientation), VertexBlendError> {
        let edge = *self.plan.incidence.vertex_edges[corner.vertex]
            .iter()
            .find(|edge| {
                self.plan.incidence.edge_faces[**edge]
                    .is_some_and(|pair| pair.contains(&from) && pair.contains(&to))
            })
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let (key, stored_from, _) = self
            .keys
            .corner_edge
            .get(&(corner.vertex, edge))
            .copied()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        let start = self
            .keys
            .corner_vertex
            .get(&(corner.vertex, from))
            .copied()
            .ok_or(VertexBlendError::DomainUnsupported)?;
        Ok((
            key,
            if stored_from == start {
                Orientation::Forward
            } else {
                Orientation::Reverse
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Small exact geometry
// ---------------------------------------------------------------------------

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > f64::EPSILON).then(|| vector / length)
}

/// A direction in space written in one plane's own parameters. Only sound for
/// an orthonormal basis, which [`orthonormal`] certifies before this rung
/// writes a circle in any plane.
fn plane_direction(plane: Plane, direction: Vector3) -> Vector2 {
    Vector2::new(direction.dot(plane.u), direction.dot(plane.v))
}

/// Whether a plane's parameters measure length: a circle written in them keeps
/// its radius only where the basis is unit and square.
fn orthonormal(plane: Plane, tolerance: f64) -> bool {
    (plane.u.length() - 1.0).abs() <= tolerance
        && (plane.v.length() - 1.0).abs() <= tolerance
        && plane.u.dot(plane.v).abs() <= tolerance
}

/// The point on all three planes `p·nᵢ = dᵢ`, by Cramer's rule. The three
/// normals are unit, so the determinant is the sine of how far the corner is
/// from flat and the angular agreement bounds it directly.
fn intersect_three_planes(
    normals: [Vector3; 3],
    offsets: [f64; 3],
    angle_tolerance: f64,
) -> Option<Point3> {
    let determinant = normals[0].dot(normals[1].cross(normals[2]));
    if !determinant.is_finite() || determinant.abs() <= angle_tolerance {
        return None;
    }
    let point = (normals[1].cross(normals[2]) * offsets[0]
        + normals[2].cross(normals[0]) * offsets[1]
        + normals[0].cross(normals[1]) * offsets[2])
        / determinant;
    point
        .is_finite()
        .then(|| Point3::new(point.x, point.y, point.z))
}

/// A boundary curve as a polyline in the face's own parameters, without its
/// last point, so the pieces of a loop concatenate.
fn sample_pcurve(coedge: Coedge) -> Vec<Point2> {
    const STEPS: usize = 24;
    match coedge.pcurve {
        Curve2::Line { endpoints } => vec![endpoints[0]],
        _ => {
            let range = coedge.parameter_range;
            (0..STEPS)
                .map(|step| {
                    let t = step as f64 / STEPS as f64;
                    coedge
                        .pcurve
                        .evaluate((range.end - range.start).mul_add(t, range.start))
                })
                .collect()
        }
    }
}

fn polygon_area(polygon: &[Point2]) -> f64 {
    let mut total = 0.0;
    for index in 0..polygon.len() {
        let current = polygon[index];
        let next = polygon[(index + 1) % polygon.len()];
        total += current.x.mul_add(next.y, -(current.y * next.x));
    }
    (total / 2.0).abs()
}

fn polygon_self_intersects(polygon: &[Point2]) -> bool {
    let count = polygon.len();
    for first in 0..count {
        for second in (first + 2)..count {
            // The first and last sides share a point; they are neighbours,
            // not a crossing.
            if first == 0 && second == count - 1 {
                continue;
            }
            if segments_cross(
                polygon[first],
                polygon[(first + 1) % count],
                polygon[second],
                polygon[(second + 1) % count],
            ) {
                return true;
            }
        }
    }
    false
}

fn cross_2d(origin: Point2, first: Point2, second: Point2) -> f64 {
    (first.x - origin.x).mul_add(
        second.y - origin.y,
        -((first.y - origin.y) * (second.x - origin.x)),
    )
}

/// Whether two open segments cross properly. Shared endpoints and grazing
/// contacts are left to the fit checks around this one.
fn segments_cross(a: Point2, b: Point2, c: Point2, d: Point2) -> bool {
    let first = cross_2d(a, b, c);
    let second = cross_2d(a, b, d);
    let third = cross_2d(c, d, a);
    let fourth = cross_2d(c, d, b);
    (first > 0.0) != (second > 0.0) && (third > 0.0) != (fourth > 0.0)
}

fn point_inside(polygon: &[Point2], point: Point2) -> bool {
    let mut inside = false;
    let count = polygon.len();
    for index in 0..count {
        let current = polygon[index];
        let next = polygon[(index + 1) % count];
        if (current.y > point.y) != (next.y > point.y) {
            let crossing =
                (next.x - current.x) * (point.y - current.y) / (next.y - current.y) + current.x;
            if point.x < crossing {
                inside = !inside;
            }
        }
    }
    inside
}

fn distance_to_polygon(polygon: &[Point2], point: Point2) -> f64 {
    let count = polygon.len();
    (0..count)
        .map(|index| point_segment_distance(point, polygon[index], polygon[(index + 1) % count]))
        .fold(f64::INFINITY, f64::min)
}

fn point_segment_distance(point: Point2, start: Point2, end: Point2) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx.mul_add(dx, dy * dy);
    if length_squared <= 0.0 {
        return (point.x - start.x).hypot(point.y - start.y);
    }
    let t = (((point.x - start.x) * dx) + ((point.y - start.y) * dy)) / length_squared;
    let t = t.clamp(0.0, 1.0);
    (point.x - dx.mul_add(t, start.x)).hypot(point.y - dy.mul_add(t, start.y))
}

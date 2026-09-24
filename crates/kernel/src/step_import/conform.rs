//! The conforming stage: STEP faces into the kernel's conventions.
//!
//! A STEP face is a carrier, a `same_sense` flag and loops of oriented
//! edges with exact 3D curves. The kernel wants the same face with its
//! surface turned to face out of the material, every edge use carrying a
//! pcurve in the surface's own parameters, periodic carriers split at the
//! canonical seams (azimuth `0` and `π`, ADR 0016) with a straight or
//! circular seam edge between the halves, a degenerate pole edge wherever a
//! loop passes through a pole, vertices welded at the file's accuracy, and
//! loops ordered outer-first. This module does each of those, one pass at
//! a time:
//!
//! 1. every face's surface and loops are read, and every edge's curve, with
//!    vertices welded by position;
//! 2. each face's loops are laid out in the surface's parameter space, one
//!    piece per edge use, continuous across the periodic branch and shifted
//!    into the canonical window;
//! 3. where a piece crosses a seam line, the edge parameter at the crossing
//!    is recorded, and every edge is cut into kernel sub-edges at the union
//!    of its crossings, so the two faces either side of it agree on them;
//! 4. each face's region is cut along its interior seam lines and each part
//!    becomes one kernel face.

use std::collections::{BTreeMap, HashMap};

use crate::bspline::plane_pcurve;
use crate::step_import::read::{
    self, CurveGeometry, ENTITY_UNSUPPORTED, FACE_UNSUPPORTED, GAP_EXCEEDS_TOLERANCE, Reader,
    Refusal, SHELL_OPEN, ShapeKind, ShapeSource, Weights, bounded_curve, unit,
};
use crate::topology::{
    Coedge, CoedgeKey, Curve2, Curve3, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop,
    LoopKey, Orientation, ParameterRange, Point2, Point3, Record, Shell, ShellKey, Solid, Surface,
    Topology, Vector2, Vector3, Vertex, VertexKey,
};

const TAU: f64 = std::f64::consts::TAU;
const PI: f64 = std::f64::consts::PI;
/// Two parameter values within this of one another are one.
const PARAMETER_SNAP: f64 = 1.0e-7;
/// A junction whose parameter jump is wider than this is a gap in the
/// face, not rounding.
const PARAMETER_GAP: f64 = 1.0e-5;

/// One edge use of a face's loop as the file lists it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EdgeUse {
    pub(crate) edge: u64,
    pub(crate) forward: bool,
}

/// One loop of a face: its edge uses in traversal order (an outer loop
/// counter-clockwise about the face normal), or the single vertex of a
/// vertex loop.
#[derive(Clone, Debug)]
pub(crate) struct LoopRead {
    pub(crate) outer: bool,
    pub(crate) uses: Vec<EdgeUse>,
    pub(crate) vertex_only: Option<u64>,
}

/// One STEP face as read: its oriented carrier, or why it has none, and
/// its loops.
#[derive(Clone, Debug)]
pub(crate) struct FaceRead {
    pub(crate) id: u64,
    pub(crate) ordinal: u32,
    pub(crate) surface: Result<Surface, Refusal>,
    pub(crate) loops: Vec<LoopRead>,
}

/// One STEP edge as read: its welded vertices and the kernel curve from the
/// first to the second.
#[derive(Clone, Debug)]
pub(crate) struct EdgeRead {
    pub(crate) id: u64,
    pub(crate) start: VertexKey,
    pub(crate) end: VertexKey,
    pub(crate) curve: Curve3,
    /// From `start` to `end`; decreasing when the curve runs the other way.
    pub(crate) range: ParameterRange,
    /// A zero-length edge standing at a pole.
    pub(crate) pole: bool,
    /// Edge parameters, strictly inside the range, where a face crosses a
    /// seam line along this edge.
    splits: Vec<f64>,
    /// The kernel edges from `start` to `end` once the edge is cut.
    pieces: Vec<SubEdge>,
}

/// One kernel edge a STEP edge was cut into. `vertices` and `range` run in
/// the STEP edge's own direction, whichever way the kernel edge's do.
#[derive(Clone, Copy, Debug)]
struct SubEdge {
    key: EdgeKey,
    vertices: [VertexKey; 2],
    range: ParameterRange,
}

/// One shell of a shape: which STEP faces it holds, and its role.
#[derive(Clone, Debug)]
pub(crate) struct ShellRead {
    pub(crate) id: u64,
    pub(crate) shape: usize,
    pub(crate) cavity: bool,
    pub(crate) faces: Vec<usize>,
}

/// The kernel topology under construction, with everything welded.
pub(crate) struct Importer<'r> {
    pub(crate) reader: &'r Reader<'r>,
    pub(crate) topology: Topology,
    next_id: u64,
    weld: f64,
    cells: HashMap<[i64; 3], Vec<VertexKey>>,
    step_vertices: HashMap<u64, VertexKey>,
    pub(crate) edges: BTreeMap<u64, Result<EdgeRead, Refusal>>,
    /// The degenerate edge at each pole vertex and how often it is used.
    pole_edges: HashMap<VertexKey, (EdgeKey, usize)>,
    /// How many faces pass through each pole vertex with a step in azimuth
    /// the file wrote no edge for. Two share a pole edge; any other number
    /// closes each loop through the vertex itself, as the kernel's own
    /// corner patches do.
    pole_reach: HashMap<VertexKey, usize>,
    /// Kernel edges by their vertices and a midpoint cell, so a curve two
    /// STEP edges spell twice is one edge.
    edge_cells: HashMap<(VertexKey, VertexKey, [i64; 3]), EdgeKey>,
    pub(crate) faces: Vec<FaceRead>,
    pub(crate) shells: Vec<ShellRead>,
    pub(crate) shapes: Vec<ShapeSource>,
    pub(crate) face_sources: Vec<u64>,
    pub(crate) edge_sources: Vec<Option<u64>>,
    /// Approximations the reading made, to be reported as warnings.
    pub(crate) approximations: Vec<Refusal>,
}

impl<'r> Importer<'r> {
    pub(crate) fn new(reader: &'r Reader<'r>, shapes: Vec<ShapeSource>) -> Self {
        Self {
            reader,
            topology: Topology::default(),
            next_id: 1,
            weld: reader.weld,
            cells: HashMap::new(),
            step_vertices: HashMap::new(),
            edges: BTreeMap::new(),
            pole_edges: HashMap::new(),
            pole_reach: HashMap::new(),
            edge_cells: HashMap::new(),
            faces: Vec::new(),
            shells: Vec::new(),
            shapes,
            face_sources: Vec::new(),
            edge_sources: Vec::new(),
            approximations: Vec::new(),
        }
    }

    fn allocate(&mut self) -> EntityId {
        let id = EntityId::from_raw(self.next_id);
        self.next_id += 1;
        id
    }

    fn cell_of(&self, point: Point3) -> [i64; 3] {
        let size = (self.weld * 4.0).max(1.0e-9);
        [
            (point.x / size).floor() as i64,
            (point.y / size).floor() as i64,
            (point.z / size).floor() as i64,
        ]
    }

    /// The kernel vertex at `point`: an existing one within the weld
    /// distance, or a new one.
    pub(crate) fn weld_point(&mut self, point: Point3) -> VertexKey {
        let cell = self.cell_of(point);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let key = [cell[0] + dx, cell[1] + dy, cell[2] + dz];
                    if let Some(candidates) = self.cells.get(&key) {
                        for candidate in candidates {
                            if self.topology.vertices[candidate.0]
                                .value
                                .point
                                .distance(point)
                                <= self.weld
                            {
                                return *candidate;
                            }
                        }
                    }
                }
            }
        }
        let key = VertexKey(self.topology.vertices.len());
        let id = self.allocate();
        self.topology.vertices.push(Record {
            id,
            value: Vertex { point },
        });
        self.cells.entry(cell).or_default().push(key);
        key
    }

    pub(crate) fn point_of(&self, vertex: VertexKey) -> Point3 {
        self.topology.vertices[vertex.0].value.point
    }

    /// The kernel vertex of a `VERTEX_POINT`, welded.
    fn step_vertex(&mut self, id: u64) -> Result<VertexKey, Refusal> {
        if let Some(key) = self.step_vertices.get(&id) {
            return Ok(*key);
        }
        let point = self.reader.vertex_point(id)?;
        let key = self.weld_point(point);
        self.step_vertices.insert(id, key);
        Ok(key)
    }

    /// A kernel edge between two vertices along a curve: an existing one
    /// with the same vertices and midpoint, or a new one. Returns the edge
    /// and whether `vertices` runs it forwards.
    fn edge_between(
        &mut self,
        vertices: [VertexKey; 2],
        curve: Curve3,
        range: ParameterRange,
        source: Option<u64>,
    ) -> (EdgeKey, bool) {
        let middle = curve.evaluate((range.start + range.end) / 2.0);
        let cell = self.cell_of(middle);
        let ordered = if vertices[0].0 <= vertices[1].0 {
            (vertices[0], vertices[1])
        } else {
            (vertices[1], vertices[0])
        };
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let key = (
                        ordered.0,
                        ordered.1,
                        [cell[0] + dx, cell[1] + dy, cell[2] + dz],
                    );
                    if let Some(existing) = self.edge_cells.get(&key) {
                        let edge = self.topology.edges[existing.0].value;
                        let range = edge.parameter_range;
                        let existing_middle = edge.curve.evaluate((range.start + range.end) / 2.0);
                        if existing_middle.distance(middle) <= self.weld {
                            return (*existing, edge.vertices == vertices);
                        }
                    }
                }
            }
        }
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record {
            id,
            value: Edge {
                vertices,
                curve,
                parameter_range: range,
            },
        });
        self.edge_sources.push(source);
        self.edge_cells.insert((ordered.0, ordered.1, cell), key);
        (key, true)
    }

    /// The one degenerate edge at a pole vertex, and the orientation this
    /// use of it takes: the halves either side traverse it opposite ways.
    fn pole_edge(&mut self, vertex: VertexKey) -> (EdgeKey, Orientation) {
        if let Some((key, uses)) = self.pole_edges.get_mut(&vertex) {
            *uses += 1;
            let orientation = if *uses % 2 == 1 {
                Orientation::Forward
            } else {
                Orientation::Reverse
            };
            return (*key, orientation);
        }
        let point = self.point_of(vertex);
        let key = EdgeKey(self.topology.edges.len());
        let id = self.allocate();
        self.topology.edges.push(Record {
            id,
            value: Edge {
                vertices: [vertex, vertex],
                curve: Curve3::Line {
                    endpoints: [point, point],
                },
                parameter_range: ParameterRange::new(0.0, 1.0),
            },
        });
        self.edge_sources.push(None);
        self.pole_edges.insert(vertex, (key, 1));
        (key, Orientation::Forward)
    }

    // -----------------------------------------------------------------------
    // Pass 1: reading faces and edges
    // -----------------------------------------------------------------------

    /// Reads every shell of every shape into faces. `flip` turns the whole
    /// body inside out, for a file whose shells face into their material.
    pub(crate) fn read_faces(&mut self, flip: bool) -> Result<(), Refusal> {
        self.faces.clear();
        self.shells.clear();
        let mut ordinal = 0_u32;
        for (shape_index, shape) in self.shapes.clone().iter().enumerate() {
            let shells: Vec<(u64, bool, bool)> = shape
                .outer
                .iter()
                .map(|id| (*id, false, true))
                .chain(
                    shape
                        .voids
                        .iter()
                        .map(|(id, oriented)| (*id, true, *oriented)),
                )
                .collect();
            for (shell_id, cavity, oriented) in shells {
                let shell_entity = self.reader.entity(shell_id)?;
                if !(shell_entity.is("CLOSED_SHELL") || shell_entity.is("OPEN_SHELL")) {
                    return Err(Refusal::new(
                        ENTITY_UNSUPPORTED,
                        Some(shell_id),
                        format!("a {} where a shell was expected", shell_entity.kind()),
                    ));
                }
                let instance = shell_entity
                    .instance("CLOSED_SHELL")
                    .or_else(|| shell_entity.instance("OPEN_SHELL"))
                    .expect("checked above");
                let face_ids = instance.arg(1).as_refs().ok_or_else(|| {
                    Refusal::new(ENTITY_UNSUPPORTED, Some(shell_id), "a shell without faces")
                })?;
                let mut faces = Vec::with_capacity(face_ids.len());
                for face_id in face_ids {
                    // A shell that faces into its material is read turned
                    // inside out, unless the whole body is being flipped.
                    let face = self.read_face(face_id, ordinal, flip == oriented)?;
                    ordinal += 1;
                    faces.push(self.faces.len());
                    self.faces.push(face);
                }
                self.shells.push(ShellRead {
                    id: shell_id,
                    shape: shape_index,
                    cavity,
                    faces,
                });
            }
        }
        Ok(())
    }

    fn read_face(&mut self, id: u64, ordinal: u32, flip: bool) -> Result<FaceRead, Refusal> {
        let entity = self.reader.entity(id)?;
        let (bounds, surface_id, same_sense) = if let Some(instance) = entity
            .instance("ADVANCED_FACE")
            .or_else(|| entity.instance("FACE_SURFACE"))
        {
            (
                instance.arg(1).as_refs().unwrap_or_default(),
                instance.arg(2).as_ref(),
                instance.arg(3).as_bool().unwrap_or(true),
            )
        } else if let Some(instance) = entity.instance("FACE") {
            (instance.arg(1).as_refs().unwrap_or_default(), None, true)
        } else {
            return Err(Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                format!("a {} where a face was expected", entity.kind()),
            ));
        };
        let same_sense = same_sense != flip;
        let mut loops = Vec::new();
        let mut polygon_plane: Option<Surface> = None;
        for bound_id in bounds {
            let bound = self.reader.entity(bound_id)?;
            let outer = bound.is("FACE_OUTER_BOUND");
            let instance = bound
                .instance("FACE_OUTER_BOUND")
                .or_else(|| bound.instance("FACE_BOUND"))
                .ok_or_else(|| {
                    Refusal::new(
                        ENTITY_UNSUPPORTED,
                        Some(bound_id),
                        format!("a {} where a face bound was expected", bound.kind()),
                    )
                })?;
            let loop_id = instance.arg(1).as_ref().ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(bound_id),
                    "a face bound without a loop",
                )
            })?;
            let orientation = instance.arg(2).as_bool().unwrap_or(true) != flip;
            let loop_entity = self.reader.entity(loop_id)?;
            if loop_entity.is("VERTEX_LOOP") {
                let vertex = loop_entity
                    .instance("VERTEX_LOOP")
                    .and_then(|instance| instance.arg(1).as_ref());
                loops.push(LoopRead {
                    outer,
                    uses: Vec::new(),
                    vertex_only: vertex,
                });
                continue;
            }
            if loop_entity.is("POLY_LOOP") {
                let points = loop_entity
                    .instance("POLY_LOOP")
                    .and_then(|instance| instance.arg(1).as_refs())
                    .unwrap_or_default();
                let (uses, plane) = self.polygon_loop(loop_id, &points, orientation)?;
                polygon_plane.get_or_insert(plane);
                loops.push(LoopRead {
                    outer,
                    uses,
                    vertex_only: None,
                });
                continue;
            }
            let edge_loop = loop_entity.instance("EDGE_LOOP").ok_or_else(|| {
                Refusal::new(
                    ENTITY_UNSUPPORTED,
                    Some(loop_id),
                    format!("a {} where an edge loop was expected", loop_entity.kind()),
                )
            })?;
            let mut uses = Vec::new();
            for oriented_id in edge_loop.arg(1).as_refs().unwrap_or_default() {
                let oriented = self.reader.expect(oriented_id, "ORIENTED_EDGE")?;
                let instance = oriented.instance("ORIENTED_EDGE").expect("checked");
                let edge = instance.arg(3).as_ref().ok_or_else(|| {
                    Refusal::new(
                        ENTITY_UNSUPPORTED,
                        Some(oriented_id),
                        "an oriented edge without its edge",
                    )
                })?;
                uses.push(EdgeUse {
                    edge,
                    forward: instance.arg(4).as_bool().unwrap_or(true),
                });
            }
            if !orientation {
                uses.reverse();
                for edge_use in &mut uses {
                    edge_use.forward = !edge_use.forward;
                }
            }
            loops.push(LoopRead {
                outer,
                uses,
                vertex_only: None,
            });
        }
        // Outer first: the file's word for it, else the loop of largest
        // extent, which `layout` re-checks by area.
        loops.sort_by_key(|loop_read| !loop_read.outer);
        let surface = match surface_id {
            Some(surface_id) => match self.reader.surface(surface_id, same_sense) {
                Ok((surface, Weights::Approximated(spread))) => {
                    self.approximations.push(
                        Refusal::new(
                            read::RATIONAL_APPROXIMATED,
                            Some(surface_id),
                            format!(
                                "a rational B-spline surface read as non-rational: its weights differ by \
                                 {spread:.3e} relative"
                            ),
                        )
                        .with_measure(spread, self.reader.rational_tolerance),
                    );
                    Ok(surface)
                }
                Ok((surface, Weights::Exact)) => Ok(surface),
                Err(refusal) => Err(refusal),
            },
            None => match polygon_plane {
                Some(plane) if same_sense => Ok(plane),
                Some(Surface::Plane(plane)) => Ok(Surface::Plane(crate::topology::Plane::new(
                    plane.origin,
                    plane.v,
                    plane.u,
                ))),
                Some(other) => Ok(other),
                None => Err(Refusal::new(
                    FACE_UNSUPPORTED,
                    Some(id),
                    "a face with no surface and no polygon",
                )),
            },
        };
        Ok(FaceRead {
            id,
            ordinal,
            surface,
            loops,
        })
    }

    /// A `POLY_LOOP` as line edges between its points, and the plane of its
    /// polygon (by Newell's method, normal along the loop's winding).
    fn polygon_loop(
        &mut self,
        loop_id: u64,
        point_ids: &[u64],
        orientation: bool,
    ) -> Result<(Vec<EdgeUse>, Surface), Refusal> {
        let mut points = Vec::with_capacity(point_ids.len());
        for id in point_ids {
            points.push(self.reader.point(*id)?);
        }
        if !orientation {
            points.reverse();
        }
        points.dedup_by(|a, b| a.distance(*b) <= self.weld);
        if points.len() > 1 && points[0].distance(points[points.len() - 1]) <= self.weld {
            points.pop();
        }
        if points.len() < 3 {
            return Err(Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(loop_id),
                "a polygon loop of fewer than three points",
            ));
        }
        let mut normal = Vector3::new(0.0, 0.0, 0.0);
        for index in 0..points.len() {
            let a = points[index];
            let b = points[(index + 1) % points.len()];
            normal = normal
                + Vector3::new(
                    (a.y - b.y) * (a.z + b.z),
                    (a.z - b.z) * (a.x + b.x),
                    (a.x - b.x) * (a.y + b.y),
                );
        }
        let normal = unit(normal).ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(loop_id),
                "a polygon loop with no area",
            )
        })?;
        let u = unit((points[1] - points[0]) - normal * (points[1] - points[0]).dot(normal))
            .unwrap_or_else(|| read::any_perpendicular(normal));
        let plane = crate::topology::Plane::new(points[0], u, normal.cross(u));
        // Each side becomes an edge of its own, numbered after the loop so
        // the ids stay distinct from the file's.
        let mut uses = Vec::with_capacity(points.len());
        for index in 0..points.len() {
            let start = self.weld_point(points[index]);
            let end = self.weld_point(points[(index + 1) % points.len()]);
            let synthetic = loop_id * 1_000_000 + index as u64 + 1;
            let (curve, range) = Curve3::line_segment([self.point_of(start), self.point_of(end)]);
            self.edges.insert(
                synthetic,
                Ok(EdgeRead {
                    id: synthetic,
                    start,
                    end,
                    curve,
                    range,
                    pole: start == end,
                    splits: Vec::new(),
                    pieces: Vec::new(),
                }),
            );
            uses.push(EdgeUse {
                edge: synthetic,
                forward: true,
            });
        }
        Ok((uses, Surface::Plane(plane)))
    }

    /// Reads every edge the faces use. A B-spline edge whose faces are all
    /// analytic is recognised as the line or circle it really is when it
    /// lies on one within the file's accuracy.
    pub(crate) fn read_edges(&mut self) {
        let mut analytic_context: HashMap<u64, bool> = HashMap::new();
        for face in &self.faces {
            let analytic = matches!(
                face.surface,
                Ok(Surface::Plane(_)
                    | Surface::Cylinder(_)
                    | Surface::Cone(_)
                    | Surface::Sphere(_)
                    | Surface::Torus(_))
            );
            for loop_read in &face.loops {
                for edge_use in &loop_read.uses {
                    analytic_context
                        .entry(edge_use.edge)
                        .and_modify(|all| *all &= analytic)
                        .or_insert(analytic);
                }
            }
        }
        let mut ids: Vec<u64> = analytic_context.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            if self.edges.contains_key(&id) {
                continue;
            }
            let analytic = analytic_context[&id];
            let read = self.read_edge(id, analytic);
            self.edges.insert(id, read);
        }
    }

    fn read_edge(&mut self, id: u64, analytic_context: bool) -> Result<EdgeRead, Refusal> {
        let entity = self.reader.expect(id, "EDGE_CURVE")?;
        let instance = entity.instance("EDGE_CURVE").expect("checked");
        let start_id = instance.arg(1).as_ref().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                "an edge without a start vertex",
            )
        })?;
        let end_id = instance.arg(2).as_ref().ok_or_else(|| {
            Refusal::new(
                ENTITY_UNSUPPORTED,
                Some(id),
                "an edge without an end vertex",
            )
        })?;
        let curve_id = instance
            .arg(3)
            .as_ref()
            .ok_or_else(|| Refusal::new(ENTITY_UNSUPPORTED, Some(id), "an edge without a curve"))?;
        let same_sense = instance.arg(4).as_bool().unwrap_or(true);
        let start = self.step_vertex(start_id)?;
        let end = self.step_vertex(end_id)?;
        let (mut geometry, weights) = self.reader.curve(curve_id)?;
        if let Weights::Approximated(spread) = weights {
            self.approximations.push(
                Refusal::new(
                    read::RATIONAL_APPROXIMATED,
                    Some(curve_id),
                    format!(
                        "a rational B-spline curve read as non-rational: its weights differ by \
                         {spread:.3e} relative"
                    ),
                )
                .with_measure(spread, self.reader.rational_tolerance),
            );
        }
        let closed = start == end;
        if analytic_context
            && matches!(
                geometry,
                CurveGeometry::Bspline { .. } | CurveGeometry::Rational { .. }
            )
            && let Some(samples) = read::sample_curve(&geometry, 48)
            && let Some(conic) = read::recognise_conic(&samples, self.weld.max(1.0e-9) * 4.0)
        {
            geometry = conic;
        }
        let start_point = self.point_of(start);
        let end_point = self.point_of(end);
        // A zero-length edge is a pole: the loop closes through its vertex.
        let degenerate = closed
            && match &geometry {
                CurveGeometry::Circle { radius, .. } => *radius <= self.weld,
                CurveGeometry::Ellipse { major, .. } => *major <= self.weld,
                CurveGeometry::Line { .. } => true,
                CurveGeometry::Bspline { curve } => curve.size() <= self.weld * 4.0,
                CurveGeometry::Rational { .. } => false,
            };
        if degenerate {
            return Ok(EdgeRead {
                id,
                start,
                end,
                curve: Curve3::Line {
                    endpoints: [start_point, start_point],
                },
                range: ParameterRange::new(0.0, 1.0),
                pole: true,
                splits: Vec::new(),
                pieces: Vec::new(),
            });
        }
        let (curve, range) = bounded_curve(
            geometry,
            start_point,
            end_point,
            same_sense,
            closed,
            self.weld,
            id,
        )?;
        Ok(EdgeRead {
            id,
            start,
            end,
            curve,
            range,
            pole: false,
            splits: Vec::new(),
            pieces: Vec::new(),
        })
    }

    // -----------------------------------------------------------------------
    // Pass 2: laying loops out in parameter space
    // -----------------------------------------------------------------------

    /// The pieces of every loop of a face, laid out in its surface's
    /// parameter space, or why the face cannot be laid out.
    fn layout(&self, face: &FaceRead) -> Result<Vec<Vec<Piece>>, Refusal> {
        let surface = match &face.surface {
            Ok(surface) => *surface,
            Err(refusal) => return Err(refusal.clone()),
        };
        let mut loops = Vec::with_capacity(face.loops.len());
        for loop_read in &face.loops {
            if loop_read.uses.is_empty() {
                if loop_read.vertex_only.is_some() && face.loops.len() > 1 {
                    // A vertex loop marks a pole a face reaches; the pole
                    // edge the face's other loop needs is inserted there.
                    continue;
                }
                return Err(Refusal::new(
                    FACE_UNSUPPORTED,
                    Some(face.id),
                    "a face bounded by a vertex loop alone",
                ));
            }
            let mut pieces = Vec::with_capacity(loop_read.uses.len());
            for edge_use in &loop_read.uses {
                let edge = match self.edges.get(&edge_use.edge) {
                    Some(Ok(edge)) => edge,
                    Some(Err(refusal)) => return Err(refusal.clone()),
                    None => {
                        return Err(Refusal::new(
                            ENTITY_UNSUPPORTED,
                            Some(edge_use.edge),
                            "an edge the face uses was never read",
                        ));
                    }
                };
                pieces.push(self.piece(face, surface, edge, edge_use.forward)?);
            }
            loops.push(pieces);
        }
        if loops.is_empty() {
            return Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(face.id),
                "a face without an edge loop",
            ));
        }
        let any_marked_outer = face.loops.iter().any(|loop_read| loop_read.outer);
        let laid: Vec<&LoopRead> = face
            .loops
            .iter()
            .filter(|loop_read| !loop_read.uses.is_empty())
            .collect();
        for (index, pieces) in loops.iter_mut().enumerate() {
            let outer = laid.get(index).is_some_and(|loop_read| loop_read.outer)
                || (!any_marked_outer && index == 0);
            self.make_continuous(face, surface, pieces, outer)?;
        }
        // The canonical window: the lowest azimuth of the face in [0, 2π).
        if is_periodic(surface) {
            let min_u = loops
                .iter()
                .flatten()
                .flat_map(|piece| [piece.start_uv.x, piece.end_uv.x])
                .fold(f64::INFINITY, f64::min);
            let shift = -TAU * ((min_u + PARAMETER_SNAP) / TAU).floor();
            if shift != 0.0 {
                for piece in loops.iter_mut().flatten() {
                    piece.shift(shift, 0.0);
                }
            }
        }
        if let Surface::Torus(_) = surface {
            let min_v = loops
                .iter()
                .flatten()
                .flat_map(|piece| [piece.start_uv.y, piece.end_uv.y])
                .fold(f64::INFINITY, f64::min);
            let shift = -TAU * ((min_v + PARAMETER_SNAP) / TAU).floor();
            if shift != 0.0 {
                for piece in loops.iter_mut().flatten() {
                    piece.shift(0.0, shift);
                }
            }
        }
        // Outer first, by area: the outer loop is the one of positive area,
        // and there is one.
        let areas: Vec<f64> = loops.iter().map(|pieces| loop_area(pieces)).collect();
        let outer = areas
            .iter()
            .enumerate()
            .filter(|(_, area)| **area > 0.0)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index);
        let Some(outer) = outer else {
            return Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(face.id),
                "the face's loops enclose no area the way they run; its bounds and its normal disagree",
            )
            .with_measure(areas.iter().copied().fold(0.0, f64::max), 0.0));
        };
        if outer != 0 {
            loops.swap(0, outer);
        }
        for (index, area) in areas.iter().enumerate().skip(1) {
            if index != outer && *area >= 0.0 {
                return Err(Refusal::new(
                    FACE_UNSUPPORTED,
                    Some(face.id),
                    "an inner loop that runs the way an outer loop does",
                ));
            }
        }
        Ok(loops)
    }

    /// One edge use as a piece in the face's parameter space.
    fn piece(
        &self,
        face: &FaceRead,
        surface: Surface,
        edge: &EdgeRead,
        forward: bool,
    ) -> Result<Piece, Refusal> {
        let (vertices, range) = if forward {
            ([edge.start, edge.end], edge.range)
        } else {
            ([edge.end, edge.start], edge.range.reversed())
        };
        let ends = [self.point_of(vertices[0]), self.point_of(vertices[1])];
        if edge.pole {
            let v = pole_latitude(surface, ends[0]).ok_or_else(|| {
                Refusal::new(
                    FACE_UNSUPPORTED,
                    Some(face.id),
                    format!(
                        "edge #{} has no length and sits at no pole of the face's surface",
                        edge.id
                    ),
                )
            })?;
            return Ok(Piece {
                edge: Some(edge.id),
                forward,
                vertices,
                range,
                pcurve: Curve2::line_segment([Point2::new(0.0, v), Point2::new(0.0, v)]).0,
                prange: ParameterRange::new(0.0, 1.0),
                start_uv: Point2::new(f64::NAN, v),
                end_uv: Point2::new(f64::NAN, v),
                kind: PieceKind::Pole,
            });
        }
        let (pcurve, prange, start_uv, end_uv) =
            pcurve_for(surface, edge.curve, range, ends, face.id, edge.id)?;
        Ok(Piece {
            edge: Some(edge.id),
            forward,
            vertices,
            range,
            pcurve,
            prange,
            start_uv,
            end_uv,
            kind: PieceKind::Curve,
        })
    }

    /// Chooses each piece's periodic branch so the loop is continuous, fills
    /// in the azimuth of pole pieces, and inserts pole edges where a loop
    /// jumps in azimuth at a pole.
    ///
    /// A loop that uses one seam edge twice — a full sphere's, or a full
    /// cylinder's whose rims are poles — is ambiguous about which side of
    /// the seam it lies on; the placement that encloses area the way the
    /// loop should (`outer`) is the one taken.
    fn make_continuous(
        &self,
        face: &FaceRead,
        surface: Surface,
        pieces: &mut Vec<Piece>,
        outer: bool,
    ) -> Result<(), Refusal> {
        let periodic_u = is_periodic(surface);
        let periodic_v = matches!(surface, Surface::Torus(_));
        // Start from a piece with an azimuth of its own.
        if let Some(first) = pieces
            .iter()
            .position(|piece| piece.kind == PieceKind::Curve)
        {
            pieces.rotate_left(first);
        }
        let repeated: Vec<usize> = (1..pieces.len())
            .filter(|index| {
                pieces[*index].kind == PieceKind::Curve
                    && pieces[..*index].iter().any(|earlier| {
                        earlier.kind == PieceKind::Curve && earlier.edge == pieces[*index].edge
                    })
            })
            .collect();
        let turns: &[Option<f64>] = if periodic_u && !repeated.is_empty() {
            &[None, Some(-TAU), Some(TAU)]
        } else {
            &[None]
        };
        let base = pieces.clone();
        // A junction at a pole fixes no azimuth: the loop may step to any
        // branch there.
        let free: Vec<bool> = base
            .iter()
            .map(|piece| pole_latitude(surface, self.point_of(piece.vertices[1])).is_some())
            .collect();
        let mut fallback: Option<Vec<Piece>> = None;
        let mut first_error: Option<Refusal> = None;
        for turn in turns {
            for mut candidate in place_branches(
                &base,
                periodic_u,
                periodic_v,
                turn.map(|turn| (&repeated[..], turn)),
                &free,
            ) {
                match self.bridge(face, surface, &mut candidate) {
                    Ok(()) => {
                        let area = loop_area(&candidate);
                        if (outer && area > 0.0) || (!outer && area < 0.0) {
                            *pieces = candidate;
                            return Ok(());
                        }
                        fallback.get_or_insert(candidate);
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        match (fallback, first_error) {
            (Some(candidate), _) => {
                *pieces = candidate;
                Ok(())
            }
            (None, Some(error)) => Err(error),
            (None, None) => Err(Refusal::new(
                FACE_UNSUPPORTED,
                Some(face.id),
                "a loop with no pieces",
            )),
        }
    }

    /// Snaps junctions that agree, bridges the ones that jump at a pole
    /// with the pole edge, and refuses the ones that gap.
    fn bridge(
        &self,
        face: &FaceRead,
        surface: Surface,
        pieces: &mut Vec<Piece>,
    ) -> Result<(), Refusal> {
        let mut index = 0;
        while index < pieces.len() {
            let count = pieces.len();
            let next = (index + 1) % count;
            let end = pieces[index].end_uv;
            let start = pieces[next].start_uv;
            let du = if end.x.is_finite() && start.x.is_finite() {
                (end.x - start.x).abs()
            } else {
                0.0
            };
            let dv = (end.y - start.y).abs();
            if du <= PARAMETER_SNAP && dv <= PARAMETER_SNAP {
                if next != index {
                    let snapped = pieces[index].end_uv;
                    pieces[next].set_start(snapped);
                }
                index += 1;
                continue;
            }
            // A jump in azimuth at a pole: the loop passes through the pole
            // and needs the degenerate edge between the two azimuths.
            let at_pole = pole_latitude(surface, self.point_of(pieces[index].vertices[1]));
            if let Some(v) = at_pole
                && dv <= PARAMETER_SNAP
                && (end.y - v).abs() <= PARAMETER_SNAP
                && pieces[index].kind == PieceKind::Curve
                && pieces[next].kind == PieceKind::Curve
                && pieces[index].vertices[1] == pieces[next].vertices[0]
            {
                let vertex = pieces[index].vertices[1];
                let pole = Piece {
                    edge: None,
                    forward: true,
                    vertices: [vertex, vertex],
                    range: ParameterRange::new(0.0, 1.0),
                    pcurve: Curve2::line_segment([Point2::new(end.x, v), Point2::new(start.x, v)])
                        .0,
                    prange: ParameterRange::new(0.0, 1.0),
                    start_uv: Point2::new(end.x, v),
                    end_uv: Point2::new(start.x, v),
                    kind: PieceKind::Pole,
                };
                pieces.insert(index + 1, pole);
                index += 2;
                continue;
            }
            let (measured, allowed) = (du.max(dv), PARAMETER_SNAP);
            if measured > PARAMETER_GAP {
                return Err(Refusal::new(
                    GAP_EXCEEDS_TOLERANCE,
                    Some(face.id),
                    format!(
                        "the loop jumps by {measured:.3e} in the surface's parameters between edge #{} and edge #{}",
                        pieces[index].edge.unwrap_or(0),
                        pieces[next].edge.unwrap_or(0)
                    ),
                )
                .with_measure(measured, allowed));
            }
            let snapped = pieces[index].end_uv;
            pieces[next].set_start(snapped);
            index += 1;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pass 3: cutting edges at the seams every face crosses
    // -----------------------------------------------------------------------

    /// Records, on every edge, the parameters at which a face's piece along
    /// it crosses a seam line of that face's surface.
    fn record_crossings(&mut self, surface: Surface, loops: &[Vec<Piece>]) {
        for piece in loops.iter().flatten() {
            let Some(edge_id) = piece.edge else { continue };
            if piece.kind != PieceKind::Curve {
                continue;
            }
            let mut fractions = Vec::new();
            if is_periodic(surface) {
                fractions.extend(seam_fractions(piece.start_uv.x, piece.end_uv.x));
            }
            if matches!(surface, Surface::Torus(_)) {
                fractions.extend(seam_fractions(piece.start_uv.y, piece.end_uv.y));
            }
            if fractions.is_empty() {
                continue;
            }
            let Some(Ok(edge)) = self.edges.get_mut(&edge_id) else {
                continue;
            };
            for fraction in fractions {
                // The piece runs the edge forwards or backwards; the split is
                // recorded in the edge's own parameter.
                let parameter =
                    piece.range.start + (piece.range.end - piece.range.start) * fraction;
                edge.splits.push(parameter);
            }
        }
    }

    /// Cuts every edge at its recorded crossings into kernel edges.
    fn cut_edges(&mut self) {
        let ids: Vec<u64> = self.edges.keys().copied().collect();
        for id in ids {
            let Some(Ok(edge)) = self.edges.get(&id) else {
                continue;
            };
            let edge = edge.clone();
            let mut pieces = Vec::new();
            if edge.pole {
                self.edges.insert(id, Ok(EdgeRead { pieces, ..edge }));
                continue;
            }
            let ascending = edge.range.end >= edge.range.start;
            let span = (edge.range.end - edge.range.start)
                .abs()
                .max(f64::MIN_POSITIVE);
            let mut splits: Vec<f64> = edge
                .splits
                .iter()
                .copied()
                .filter(|t| {
                    let low = edge.range.start.min(edge.range.end);
                    let high = edge.range.start.max(edge.range.end);
                    *t > low + 1.0e-9 * span && *t < high - 1.0e-9 * span
                })
                .collect();
            splits.sort_by(f64::total_cmp);
            splits.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-9 * span);
            if !ascending {
                splits.reverse();
            }
            let mut previous_vertex = edge.start;
            let mut previous_parameter = edge.range.start;
            for parameter in splits
                .iter()
                .copied()
                .chain(std::iter::once(edge.range.end))
            {
                let vertex = if parameter == edge.range.end {
                    edge.end
                } else {
                    let point = edge.curve.evaluate(parameter);
                    self.weld_point(point)
                };
                let range = ParameterRange::new(previous_parameter, parameter);
                let (key, _) =
                    self.edge_between([previous_vertex, vertex], edge.curve, range, Some(id));
                pieces.push(SubEdge {
                    key,
                    vertices: [previous_vertex, vertex],
                    range,
                });
                previous_vertex = vertex;
                previous_parameter = parameter;
            }
            self.edges.insert(id, Ok(EdgeRead { pieces, ..edge }));
        }
    }

    /// A face's loops with every piece expanded into the kernel edges its
    /// STEP edge was cut into.
    fn expand(&self, loops: Vec<Vec<Piece>>) -> Vec<Vec<Piece>> {
        loops
            .into_iter()
            .map(|pieces| {
                let mut expanded = Vec::with_capacity(pieces.len());
                for piece in pieces {
                    let Some(edge_id) = piece.edge else {
                        expanded.push(piece);
                        continue;
                    };
                    let Some(Ok(edge)) = self.edges.get(&edge_id) else {
                        expanded.push(piece);
                        continue;
                    };
                    if edge.pieces.len() <= 1 || piece.kind == PieceKind::Pole {
                        expanded.push(piece);
                        continue;
                    }
                    let subs: Vec<&SubEdge> = if piece.forward {
                        edge.pieces.iter().collect()
                    } else {
                        edge.pieces.iter().rev().collect()
                    };
                    let span = piece.range.end - piece.range.start;
                    for sub in subs {
                        // The sub-edge's range and vertices in the piece's
                        // direction.
                        let ((from, to), vertices) = if piece.forward {
                            ((sub.range.start, sub.range.end), sub.vertices)
                        } else {
                            (
                                (sub.range.end, sub.range.start),
                                [sub.vertices[1], sub.vertices[0]],
                            )
                        };
                        let f0 = if span == 0.0 {
                            0.0
                        } else {
                            (from - piece.range.start) / span
                        };
                        let f1 = if span == 0.0 {
                            1.0
                        } else {
                            (to - piece.range.start) / span
                        };
                        expanded.push(piece.restricted(
                            f0,
                            f1,
                            ParameterRange::new(from, to),
                            vertices,
                        ));
                    }
                }
                expanded
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // Pass 4: cutting faces at their seams and emitting kernel faces
    // -----------------------------------------------------------------------

    /// Builds every face into the topology. Faces that cannot be conformed
    /// come back as refusals; the rest are built. Returns the kernel faces
    /// of each STEP face, by STEP face index.
    pub(crate) fn build_faces(&mut self) -> (Vec<Vec<FaceKey>>, Vec<Refusal>) {
        let mut refusals = Vec::new();
        // Pass 2 for every face, then the crossings, then the cuts.
        let mut layouts: Vec<Option<Vec<Vec<Piece>>>> = Vec::with_capacity(self.faces.len());
        for index in 0..self.faces.len() {
            let face = self.faces[index].clone();
            match self.layout(&face) {
                Ok(loops) => layouts.push(Some(loops)),
                Err(refusal) => {
                    refusals.push(refusal);
                    layouts.push(None);
                }
            }
        }
        for (index, layout) in layouts.iter().enumerate() {
            if let (Some(loops), Ok(surface)) = (layout, &self.faces[index].surface) {
                let surface = *surface;
                self.record_crossings(surface, loops);
            }
        }
        self.cut_edges();
        let mut pending: Vec<(usize, FaceRead, Surface, Vec<Region>)> =
            Vec::with_capacity(self.faces.len());
        for (index, layout) in layouts.into_iter().enumerate() {
            let Some(loops) = layout else { continue };
            let face = self.faces[index].clone();
            let Ok(surface) = face.surface else { continue };
            let loops = self.expand(loops);
            match self.cut_regions(&face, surface, loops) {
                Ok(regions) => pending.push((index, face, surface, regions)),
                Err(refusal) => refusals.push(refusal),
            }
        }
        // A pole edge is shared by the two faces either side of it; a pole
        // one face alone reaches, as a corner patch's does, gets none.
        self.pole_reach.clear();
        for piece in pending
            .iter()
            .flat_map(|(_, _, _, regions)| regions.iter())
            .flat_map(|region| region.loops.iter().flatten())
        {
            if piece.kind == PieceKind::Pole && piece.edge.is_none() {
                *self.pole_reach.entry(piece.vertices[0]).or_default() += 1;
            }
        }
        let mut built = vec![Vec::new(); self.faces.len()];
        for (index, face, surface, regions) in pending {
            for region in regions {
                let key = self.emit_face(&face, surface, region);
                built[index].push(key);
            }
        }
        (built, refusals)
    }

    /// Cuts a face's region along every seam line strictly inside it.
    fn cut_regions(
        &mut self,
        face: &FaceRead,
        surface: Surface,
        loops: Vec<Vec<Piece>>,
    ) -> Result<Vec<Region>, Refusal> {
        let mut regions = vec![Region { loops }];
        let cut_u = is_periodic(surface);
        let cut_v = matches!(surface, Surface::Torus(_));
        for axis in [Axis::U, Axis::V] {
            if (axis == Axis::U && !cut_u) || (axis == Axis::V && !cut_v) {
                continue;
            }
            let mut next = Vec::new();
            for region in regions {
                let (low, high) = region.extent(axis);
                let first = (low / PI).floor() as i64 + 1;
                let last = (high / PI).ceil() as i64 - 1;
                let mut parts = vec![region];
                for k in first..=last {
                    let line = k as f64 * PI;
                    if line <= low + PARAMETER_SNAP || line >= high - PARAMETER_SNAP {
                        continue;
                    }
                    let mut cut = Vec::new();
                    for part in parts {
                        let (part_low, part_high) = part.extent(axis);
                        if line <= part_low + PARAMETER_SNAP || line >= part_high - PARAMETER_SNAP {
                            cut.push(part);
                        } else {
                            cut.extend(self.cut_region(face, surface, part, axis, line)?);
                        }
                    }
                    parts = cut;
                }
                next.extend(parts);
            }
            regions = next;
        }
        Ok(regions)
    }

    /// Cuts one region along `axis = line`. Every piece already ends on the
    /// line or lies wholly on one side of it.
    fn cut_region(
        &mut self,
        face: &FaceRead,
        surface: Surface,
        region: Region,
        axis: Axis,
        line: f64,
    ) -> Result<Vec<Region>, Refusal> {
        let coordinate = |point: Point2| match axis {
            Axis::U => point.x,
            Axis::V => point.y,
        };
        let other = |point: Point2| match axis {
            Axis::U => point.y,
            Axis::V => point.x,
        };
        let mut left: Vec<Piece> = Vec::new();
        let mut right: Vec<Piece> = Vec::new();
        let mut on_line: Vec<(f64, VertexKey, Point2)> = Vec::new();
        for piece in region.loops.iter().flatten() {
            let (a, b) = (coordinate(piece.start_uv), coordinate(piece.end_uv));
            for (uv, vertex) in [
                (piece.start_uv, piece.vertices[0]),
                (piece.end_uv, piece.vertices[1]),
            ] {
                if (coordinate(uv) - line).abs() <= PARAMETER_SNAP {
                    on_line.push((other(uv), vertex, uv));
                }
            }
            let middle = (a + b) / 2.0;
            let side = if (a - line).abs() <= PARAMETER_SNAP && (b - line).abs() <= PARAMETER_SNAP {
                // Along the line: the region lies to the left of travel.
                let rising = other(piece.end_uv) > other(piece.start_uv);
                if rising == (axis == Axis::U) {
                    Side::Left
                } else {
                    Side::Right
                }
            } else if middle < line {
                Side::Left
            } else {
                Side::Right
            };
            let mut piece = piece.clone();
            piece.snap_to(axis, line);
            match side {
                Side::Left => left.push(piece),
                Side::Right => right.push(piece),
            }
        }
        // The crossing points along the line, and the intervals between them
        // that lie inside the region.
        on_line.sort_by(|a, b| a.0.total_cmp(&b.0));
        on_line.dedup_by(|a, b| (a.0 - b.0).abs() <= PARAMETER_SNAP && a.1 == b.1);
        let polygon = region.polylines();
        let mut cuts: Vec<(VertexKey, VertexKey, f64, f64)> = Vec::new();
        for pair in on_line.windows(2) {
            let (lo, hi) = (pair[0].0, pair[1].0);
            if hi - lo <= PARAMETER_SNAP || pair[0].1 == pair[1].1 {
                continue;
            }
            let middle = (lo + hi) / 2.0;
            let probe = match axis {
                Axis::U => Point2::new(line - 1.0e-6, middle),
                Axis::V => Point2::new(middle, line - 1.0e-6),
            };
            if point_in_polygons(probe, &polygon) {
                cuts.push((pair[0].1, pair[1].1, lo, hi));
            }
        }
        if cuts.is_empty() {
            return Ok(vec![region]);
        }
        for (start, end, lo, hi) in &cuts {
            let (start_uv, end_uv) = match axis {
                Axis::U => (Point2::new(line, *lo), Point2::new(line, *hi)),
                Axis::V => (Point2::new(*lo, line), Point2::new(*hi, line)),
            };
            let (curve, range) = seam_curve(
                surface,
                axis,
                line,
                *lo,
                *hi,
                self.point_of(*start),
                self.point_of(*end),
            );
            let (key, _) = self.edge_between([*start, *end], curve, range, None);
            let ascending = Piece {
                edge: None,
                forward: true,
                vertices: [*start, *end],
                range,
                pcurve: Curve2::line_segment([start_uv, end_uv]).0,
                prange: ParameterRange::new(0.0, 1.0),
                start_uv,
                end_uv,
                kind: PieceKind::Seam(key),
            };
            let descending = ascending.reversed();
            // The left region has the line on its right and walks it
            // upwards; the right region walks it downwards.
            match axis {
                Axis::U => {
                    left.push(ascending);
                    right.push(descending);
                }
                Axis::V => {
                    left.push(descending);
                    right.push(ascending);
                }
            }
        }
        let mut regions = Vec::new();
        for pieces in [left, right] {
            if pieces.is_empty() {
                continue;
            }
            let cycles = chain(pieces, face.id)?;
            regions.extend(group_regions(cycles));
        }
        Ok(regions)
    }

    /// One region as a kernel face.
    fn emit_face(&mut self, face: &FaceRead, surface: Surface, region: Region) -> FaceKey {
        let mut loop_keys = Vec::with_capacity(region.loops.len());
        for pieces in region.loops {
            let mut coedges = Vec::with_capacity(pieces.len());
            for piece in pieces {
                if piece.kind == PieceKind::Pole
                    && piece.edge.is_none()
                    && self.pole_reach.get(&piece.vertices[0]).copied() != Some(2)
                {
                    // The loop closes through the pole vertex itself.
                    continue;
                }
                let (edge, orientation) = match piece.kind {
                    PieceKind::Pole => self.pole_edge(piece.vertices[0]),
                    PieceKind::Seam(key) => {
                        let edge = self.topology.edges[key.0].value;
                        (
                            key,
                            if edge.vertices == piece.vertices
                                || edge.vertices[0] == edge.vertices[1]
                            {
                                Orientation::Forward
                            } else {
                                Orientation::Reverse
                            },
                        )
                    }
                    PieceKind::Curve => {
                        let edge_id = piece.edge.expect("a curve piece has an edge");
                        let (key, orientation) =
                            self.kernel_edge_for(edge_id, piece.range, piece.vertices);
                        (key, orientation)
                    }
                };
                let key = CoedgeKey(self.topology.coedges.len());
                let id = self.allocate();
                self.topology.coedges.push(Record {
                    id,
                    value: Coedge {
                        edge,
                        orientation,
                        pcurve: piece.pcurve,
                        parameter_range: piece.prange,
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
            loop_keys.push(key);
        }
        let key = FaceKey(self.topology.faces.len());
        let id = self.allocate();
        self.topology.faces.push(Record {
            id,
            value: Face {
                surface,
                outer_loop: loop_keys[0],
                inner_loops: loop_keys[1..].to_vec(),
                role: FaceRole::FeatureSide(face.ordinal),
            },
        });
        self.face_sources.push(face.id);
        key
    }

    /// The kernel edge a piece of a STEP edge runs along, and which way.
    fn kernel_edge_for(
        &mut self,
        edge_id: u64,
        range: ParameterRange,
        vertices: [VertexKey; 2],
    ) -> (EdgeKey, Orientation) {
        let Some(Ok(edge)) = self.edges.get(&edge_id) else {
            unreachable!("a laid-out piece refers to a read edge");
        };
        if edge.pole {
            return self.pole_edge(vertices[0]);
        }
        let middle = (range.start + range.end) / 2.0;
        let sub = edge
            .pieces
            .iter()
            .find(|sub| {
                let low = sub.range.start.min(sub.range.end);
                let high = sub.range.start.max(sub.range.end);
                middle >= low - 1.0e-12 && middle <= high + 1.0e-12
            })
            .or_else(|| edge.pieces.first())
            .copied();
        let Some(sub) = sub else {
            // An edge that was never cut (a polygon loop's line): make it now.
            let curve = edge.curve;
            let edge_range = edge.range;
            let (start, end) = (edge.start, edge.end);
            let (key, forward) = self.edge_between([start, end], curve, edge_range, Some(edge_id));
            let kernel = self.topology.edges[key.0].value;
            let _ = forward;
            let orientation = if kernel.vertices == vertices {
                Orientation::Forward
            } else {
                Orientation::Reverse
            };
            if let Some(Ok(edge)) = self.edges.get_mut(&edge_id) {
                edge.pieces.push(SubEdge {
                    key,
                    vertices: kernel.vertices,
                    range: edge_range,
                });
            }
            return (key, orientation);
        };
        let kernel = self.topology.edges[sub.key.0].value;
        let orientation = if kernel.vertices == vertices {
            Orientation::Forward
        } else {
            Orientation::Reverse
        };
        (sub.key, orientation)
    }

    // -----------------------------------------------------------------------
    // Pass 5: shells and solids
    // -----------------------------------------------------------------------

    /// Gathers the built faces into the shells and solids the shapes
    /// declare. A shell-based model's shells become a solid when every
    /// shell closes.
    pub(crate) fn assemble(&mut self, built: &[Vec<FaceKey>]) -> Vec<Refusal> {
        let mut refusals = Vec::new();
        let mut shell_keys: Vec<Option<ShellKey>> = vec![None; self.shells.len()];
        for (index, shell) in self.shells.clone().iter().enumerate() {
            let faces: Vec<FaceKey> = shell
                .faces
                .iter()
                .flat_map(|face| built[*face].iter().copied())
                .collect();
            if faces.is_empty() {
                continue;
            }
            if let Some(open) = self.open_edges(&faces) {
                refusals.push(
                    Refusal::new(
                        SHELL_OPEN,
                        Some(shell.id),
                        format!(
                            "the shell does not close: {open} of its edges bound one face only (a face it \
                             needs may have been refused above)"
                        ),
                    )
                    .with_measure(open as f64, 0.0),
                );
                continue;
            }
            let key = ShellKey(self.topology.shells.len());
            let id = self.allocate();
            self.topology.shells.push(Record {
                id,
                value: Shell { faces },
            });
            shell_keys[index] = Some(key);
        }
        for (shape_index, shape) in self.shapes.clone().iter().enumerate() {
            let shells: Vec<(usize, ShellRead)> = self
                .shells
                .iter()
                .cloned()
                .enumerate()
                .filter(|(_, shell)| shell.shape == shape_index)
                .collect();
            match shape.kind {
                ShapeKind::Solid => {
                    let outer = shells
                        .iter()
                        .find(|(_, shell)| !shell.cavity)
                        .and_then(|(index, _)| shell_keys[*index]);
                    let Some(outer) = outer else { continue };
                    let inner: Vec<ShellKey> = shells
                        .iter()
                        .filter(|(_, shell)| shell.cavity)
                        .filter_map(|(index, _)| shell_keys[*index])
                        .collect();
                    let id = self.allocate();
                    self.topology.solids.push(Record {
                        id,
                        value: Solid {
                            outer_shell: outer,
                            inner_shells: inner,
                        },
                    });
                }
                ShapeKind::ShellModel => {
                    // Every closed shell of the model is a solid of its own;
                    // which of them nest is not recorded in the file.
                    for (index, _) in &shells {
                        let Some(key) = shell_keys[*index] else {
                            continue;
                        };
                        let id = self.allocate();
                        self.topology.solids.push(Record {
                            id,
                            value: Solid {
                                outer_shell: key,
                                inner_shells: Vec::new(),
                            },
                        });
                    }
                }
            }
        }
        refusals
    }

    /// How many edges of these faces are used once, or `None` when every
    /// edge is used exactly twice.
    fn open_edges(&self, faces: &[FaceKey]) -> Option<usize> {
        let mut uses: HashMap<EdgeKey, usize> = HashMap::new();
        for face in faces {
            for loop_key in self.topology.faces[face.0].value.loops() {
                for coedge in &self.topology.loops[loop_key.0].value.coedges {
                    *uses
                        .entry(self.topology.coedges[coedge.0].value.edge)
                        .or_default() += 1;
                }
            }
        }
        let open = uses.values().filter(|count| **count != 2).count();
        (open > 0).then_some(open)
    }

    /// The bounding extent of every vertex, for scale-relative choices.
    pub(crate) fn extent(&self) -> f64 {
        let mut low = Point3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut high = Point3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for vertex in &self.topology.vertices {
            let point = vertex.value.point;
            low = Point3::new(low.x.min(point.x), low.y.min(point.y), low.z.min(point.z));
            high = Point3::new(
                high.x.max(point.x),
                high.y.max(point.y),
                high.z.max(point.z),
            );
        }
        let extent = (high - low).length();
        if extent.is_finite() && extent > 0.0 {
            extent
        } else {
            1.0
        }
    }
}

// ---------------------------------------------------------------------------
// Pieces and regions in parameter space
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PieceKind {
    /// A piece of a STEP edge.
    Curve,
    /// The degenerate edge at a pole.
    Pole,
    /// A seam edge the cut made, already in the topology.
    Seam(EdgeKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    U,
    V,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// One edge use laid out in a face's parameter space: the kernel curve it
/// runs along, oriented the way the loop runs, and its pcurve.
#[derive(Clone, Debug)]
struct Piece {
    edge: Option<u64>,
    forward: bool,
    vertices: [VertexKey; 2],
    range: ParameterRange,
    pcurve: Curve2,
    prange: ParameterRange,
    start_uv: Point2,
    end_uv: Point2,
    kind: PieceKind,
}

impl Piece {
    /// Moves the piece by whole turns in `u` and `v`.
    fn shift(&mut self, du: f64, dv: f64) {
        match &mut self.pcurve {
            Curve2::Line { endpoints } => {
                for point in endpoints {
                    point.x += du;
                    point.y += dv;
                }
            }
            Curve2::Harmonic { .. } => {
                // The parameter is `u` itself; the graph repeats every turn.
                self.prange = ParameterRange::new(self.prange.start + du, self.prange.end + du);
            }
            Curve2::Circle { center, .. } | Curve2::Ellipse { center, .. } => {
                center.x += du;
                center.y += dv;
            }
            Curve2::Bspline { .. } | Curve2::Trace { .. } => {}
        }
        self.start_uv = Point2::new(self.start_uv.x + du, self.start_uv.y + dv);
        self.end_uv = Point2::new(self.end_uv.x + du, self.end_uv.y + dv);
    }

    fn set_line(&mut self, start: Point2, end: Point2) {
        self.pcurve = Curve2::line_segment([start, end]).0;
        self.prange = ParameterRange::new(0.0, 1.0);
        self.start_uv = start;
        self.end_uv = end;
    }

    /// Moves the piece's start to `start`, keeping its end.
    fn set_start(&mut self, start: Point2) {
        match &mut self.pcurve {
            Curve2::Line { endpoints } => {
                endpoints[0] = start;
                self.start_uv = start;
            }
            Curve2::Harmonic { .. } => {
                self.prange = ParameterRange::new(start.x, self.prange.end);
                self.start_uv = Point2::new(start.x, self.start_uv.y);
            }
            _ => {}
        }
    }

    /// The same piece over the fractions `[f0, f1]` of its walk: the
    /// pcurve's parameter is affine in the edge's, so the restriction is
    /// exact.
    fn restricted(
        &self,
        f0: f64,
        f1: f64,
        range: ParameterRange,
        vertices: [VertexKey; 2],
    ) -> Self {
        let mut piece = self.clone();
        piece.range = range;
        piece.vertices = vertices;
        match self.pcurve {
            Curve2::Line { endpoints } => {
                let at = |f: f64| {
                    Point2::new(
                        endpoints[0].x + (endpoints[1].x - endpoints[0].x) * f,
                        endpoints[0].y + (endpoints[1].y - endpoints[0].y) * f,
                    )
                };
                let (start, end) = (at(f0), at(f1));
                piece.set_line(start, end);
            }
            Curve2::Harmonic { .. } => {
                let delta = self.prange.end - self.prange.start;
                piece.prange = ParameterRange::new(
                    self.prange.start + delta * f0,
                    self.prange.start + delta * f1,
                );
                piece.start_uv = piece.pcurve.evaluate(piece.prange.start);
                piece.end_uv = piece.pcurve.evaluate(piece.prange.end);
            }
            Curve2::Circle { .. } | Curve2::Ellipse { .. } | Curve2::Bspline { .. } => {
                // The pcurve's parameter is the edge's own.
                piece.prange = range;
                piece.start_uv = piece.pcurve.evaluate(range.start);
                piece.end_uv = piece.pcurve.evaluate(range.end);
            }
            Curve2::Trace { .. } => {}
        }
        piece
    }

    /// The piece walked the other way.
    fn reversed(&self) -> Self {
        let mut piece = self.clone();
        piece.vertices = [self.vertices[1], self.vertices[0]];
        piece.range = self.range.reversed();
        piece.forward = !self.forward;
        match self.pcurve {
            Curve2::Line { endpoints } => {
                piece.set_line(endpoints[1], endpoints[0]);
            }
            _ => {
                piece.prange = self.prange.reversed();
                piece.start_uv = self.end_uv;
                piece.end_uv = self.start_uv;
            }
        }
        piece
    }

    /// Snaps an endpoint that lies within rounding of `axis = line` onto
    /// it exactly.
    fn snap_to(&mut self, axis: Axis, line: f64) {
        let snap = |point: &mut Point2| match axis {
            Axis::U if (point.x - line).abs() <= PARAMETER_SNAP => point.x = line,
            Axis::V if (point.y - line).abs() <= PARAMETER_SNAP => point.y = line,
            _ => {}
        };
        if let Curve2::Line { endpoints } = &mut self.pcurve {
            snap(&mut endpoints[0]);
            snap(&mut endpoints[1]);
            self.start_uv = endpoints[0];
            self.end_uv = endpoints[1];
        } else if let Curve2::Harmonic { .. } = self.pcurve
            && axis == Axis::U
        {
            if (self.prange.start - line).abs() <= PARAMETER_SNAP {
                self.prange.start = line;
            }
            if (self.prange.end - line).abs() <= PARAMETER_SNAP {
                self.prange.end = line;
            }
            self.start_uv = self.pcurve.evaluate(self.prange.start);
            self.end_uv = self.pcurve.evaluate(self.prange.end);
        }
    }

    /// Points along the pcurve, for area and containment tests.
    fn polyline(&self) -> Vec<Point2> {
        match self.pcurve {
            Curve2::Line { .. } => vec![self.start_uv, self.end_uv],
            _ => (0..=8)
                .map(|step| {
                    let t = self.prange.start
                        + (self.prange.end - self.prange.start) * f64::from(step) / 8.0;
                    self.pcurve.evaluate(t)
                })
                .collect(),
        }
    }

    /// `½∮(x dy − y dx)` along the piece.
    fn area(&self) -> f64 {
        match self.pcurve {
            Curve2::Line { .. } => {
                0.5 * (self.start_uv.x * self.end_uv.y - self.start_uv.y * self.end_uv.x)
            }
            Curve2::Harmonic {
                mean,
                amplitude,
                phase,
            } => crate::validator::harmonic_area_contribution(
                mean,
                amplitude,
                phase,
                self.prange.start,
                self.prange.end,
            ),
            _ => {
                let points = self.polyline();
                points
                    .windows(2)
                    .map(|pair| 0.5 * (pair[0].x * pair[1].y - pair[0].y * pair[1].x))
                    .sum()
            }
        }
    }
}

fn loop_area(pieces: &[Piece]) -> f64 {
    pieces.iter().map(Piece::area).sum()
}

/// Lays the loop out in the surface's parameters: each piece after the
/// first is moved by whole turns so it starts where the piece before it
/// ends, a piece listed in `forced` a further turn on, and every pole
/// piece is laid between its neighbours' azimuths.
///
/// A junction marked `free` sits at a pole, where the azimuth means
/// nothing, so the run of pieces after it may sit on any branch: every
/// choice of branches at the free junctions is returned as a candidate,
/// the narrowest in `u` first. Where the loop closes at a junction that
/// is not free, the last run is placed so that it does.
fn place_branches(
    base: &[Piece],
    periodic_u: bool,
    periodic_v: bool,
    forced: Option<(&[usize], f64)>,
    free: &[bool],
) -> Vec<Vec<Piece>> {
    let count = base.len();
    let mut pieces = base.to_vec();
    // Pass 1: continuity at every junction that is not free.
    for index in 1..count {
        if pieces[index].kind == PieceKind::Pole || free[index - 1] {
            continue;
        }
        let previous = pieces[index - 1].end_uv;
        let current = pieces[index].start_uv;
        let mut du = 0.0;
        let mut dv = 0.0;
        if periodic_u && previous.x.is_finite() && current.x.is_finite() {
            du = TAU * ((previous.x - current.x) / TAU).round();
            if let Some((forced, turn)) = forced
                && forced.contains(&index)
            {
                du += turn;
            }
        }
        if periodic_v && previous.y.is_finite() && current.y.is_finite() {
            dv = TAU * ((previous.y - current.y) / TAU).round();
        }
        if du != 0.0 || dv != 0.0 {
            pieces[index].shift(du, dv);
        }
    }
    // The runs between free junctions, pole pieces aside.
    let mut runs: Vec<Vec<usize>> = Vec::new();
    for index in 0..count {
        if pieces[index].kind == PieceKind::Pole {
            continue;
        }
        if index == 0 || free[index - 1] || pieces[index - 1].kind == PieceKind::Pole {
            runs.push(Vec::new());
        }
        runs.last_mut().expect("a run was started").push(index);
    }
    if runs.is_empty() {
        return vec![pieces];
    }
    let closing_free = free[count - 1] || pieces[count - 1].kind == PieceKind::Pole;
    let determined_last = !closing_free && runs.len() >= 2;
    let free_runs: Vec<usize> = (1..runs.len() - usize::from(determined_last)).collect();
    let choices: &[f64] = if periodic_u {
        &[0.0, -TAU, TAU]
    } else {
        &[0.0]
    };
    // Up to four free runs are enumerated in full; any beyond stay put.
    let enumerated = free_runs.len().min(4);
    let combinations = choices.len().pow(enumerated as u32);
    let mut candidates: Vec<(f64, f64, Vec<Piece>)> = Vec::with_capacity(combinations);
    for combination in 0..combinations {
        let mut candidate = pieces.clone();
        let mut code = combination;
        let mut total_shift = 0.0;
        for run in free_runs.iter().take(enumerated) {
            let shift = choices[code % choices.len()];
            code /= choices.len();
            if shift != 0.0 {
                for &index in &runs[*run] {
                    candidate[index].shift(shift, 0.0);
                }
                total_shift += shift.abs();
            }
        }
        if determined_last && periodic_u {
            let last = runs.last().expect("at least two runs");
            let end = candidate[*last.last().expect("a run is never empty")].end_uv;
            let start = candidate[runs[0][0]].start_uv;
            if end.x.is_finite() && start.x.is_finite() {
                let shift = TAU * ((start.x - end.x) / TAU).round();
                if shift != 0.0 {
                    for &index in last {
                        candidate[index].shift(shift, 0.0);
                    }
                }
            }
        }
        // Every pole piece runs from the azimuth before it to the one after.
        for index in 0..count {
            if candidate[index].kind != PieceKind::Pole {
                continue;
            }
            let v = candidate[index].start_uv.y;
            let before = (1..count)
                .map(|back| (index + count - back) % count)
                .find(|&other| candidate[other].kind != PieceKind::Pole)
                .map_or(0.0, |other| candidate[other].end_uv.x);
            let after = (1..count)
                .map(|forward| (index + forward) % count)
                .find(|&other| candidate[other].kind != PieceKind::Pole)
                .map_or(before, |other| candidate[other].start_uv.x);
            candidate[index].set_line(Point2::new(before, v), Point2::new(after, v));
        }
        let (low, high) =
            candidate
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), piece| {
                    let points = [piece.start_uv.x, piece.end_uv.x];
                    (
                        points
                            .iter()
                            .copied()
                            .filter(|x| x.is_finite())
                            .fold(low, f64::min),
                        points
                            .iter()
                            .copied()
                            .filter(|x| x.is_finite())
                            .fold(high, f64::max),
                    )
                });
        candidates.push((high - low, total_shift, candidate));
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    candidates
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .collect()
}

/// A set of loops, outer first, that becomes one kernel face.
#[derive(Clone, Debug)]
struct Region {
    loops: Vec<Vec<Piece>>,
}

impl Region {
    fn extent(&self, axis: Axis) -> (f64, f64) {
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        for piece in self.loops.iter().flatten() {
            for point in [piece.start_uv, piece.end_uv] {
                let value = match axis {
                    Axis::U => point.x,
                    Axis::V => point.y,
                };
                if value.is_finite() {
                    low = low.min(value);
                    high = high.max(value);
                }
            }
        }
        (low, high)
    }

    fn polylines(&self) -> Vec<Vec<Point2>> {
        self.loops
            .iter()
            .map(|pieces| {
                let mut points = Vec::new();
                for piece in pieces {
                    let mut line = piece.polyline();
                    if !points.is_empty() {
                        line.remove(0);
                    }
                    points.extend(line);
                }
                points
            })
            .collect()
    }
}

/// The fractions along a piece from `a` to `b` at which it crosses a
/// multiple of `π` strictly inside.
fn seam_fractions(a: f64, b: f64) -> Vec<f64> {
    if !(a.is_finite() && b.is_finite()) {
        return Vec::new();
    }
    let (low, high) = (a.min(b), a.max(b));
    let first = (low / PI).floor() as i64 + 1;
    let last = (high / PI).ceil() as i64 - 1;
    let mut fractions = Vec::new();
    for k in first..=last {
        let line = k as f64 * PI;
        if line > low + PARAMETER_SNAP && line < high - PARAMETER_SNAP {
            fractions.push((line - a) / (b - a));
        }
    }
    fractions
}

/// Whether a point lies inside the region the polylines bound, by the
/// crossing count of a horizontal ray.
fn point_in_polygons(point: Point2, polylines: &[Vec<Point2>]) -> bool {
    let mut inside = false;
    for polyline in polylines {
        let count = polyline.len();
        if count < 2 {
            continue;
        }
        for index in 0..count {
            let a = polyline[index];
            let b = polyline[(index + 1) % count];
            if (a.y > point.y) != (b.y > point.y) {
                let x = a.x + (point.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if point.x < x {
                    inside = !inside;
                }
            }
        }
    }
    inside
}

/// Chains directed pieces into closed cycles by their parameter-space
/// endpoints.
fn chain(mut pieces: Vec<Piece>, face: u64) -> Result<Vec<Vec<Piece>>, Refusal> {
    let mut cycles = Vec::new();
    while !pieces.is_empty() {
        let mut cycle = vec![pieces.remove(0)];
        loop {
            let end = cycle.last().expect("non-empty").end_uv;
            let start = cycle[0].start_uv;
            if cycle.len() > 1
                && (end.x - start.x).abs() <= PARAMETER_SNAP * 10.0
                && (end.y - start.y).abs() <= PARAMETER_SNAP * 10.0
            {
                break;
            }
            let next = pieces.iter().position(|piece| {
                (piece.start_uv.x - end.x).abs() <= PARAMETER_SNAP * 10.0
                    && (piece.start_uv.y - end.y).abs() <= PARAMETER_SNAP * 10.0
            });
            match next {
                Some(index) => cycle.push(pieces.remove(index)),
                None => {
                    if cycle.len() == 1 {
                        break;
                    }
                    return Err(Refusal::new(
                        GAP_EXCEEDS_TOLERANCE,
                        Some(face),
                        "the face's loops do not chain once cut at the seam",
                    ));
                }
            }
        }
        if cycle.len() >= 2 {
            cycles.push(cycle);
        }
    }
    Ok(cycles)
}

/// Groups cycles into regions: each cycle of positive area is an outer
/// loop, and each of negative area a hole of the smallest outer loop that
/// contains it.
fn group_regions(cycles: Vec<Vec<Piece>>) -> Vec<Region> {
    let mut outers: Vec<(Vec<Piece>, f64, Vec<Point2>)> = Vec::new();
    let mut holes: Vec<Vec<Piece>> = Vec::new();
    for cycle in cycles {
        let area = loop_area(&cycle);
        if area > 0.0 {
            let polyline = Region {
                loops: vec![cycle.clone()],
            }
            .polylines()
            .remove(0);
            outers.push((cycle, area, polyline));
        } else {
            holes.push(cycle);
        }
    }
    let mut regions: Vec<Region> = outers
        .iter()
        .map(|(cycle, _, _)| Region {
            loops: vec![cycle.clone()],
        })
        .collect();
    for hole in holes {
        let probe = hole[0].start_uv;
        let owner = outers
            .iter()
            .enumerate()
            .filter(|(_, (_, _, polyline))| {
                point_in_polygons(probe, std::slice::from_ref(polyline))
            })
            .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
            .map(|(index, _)| index);
        match owner {
            Some(index) => regions[index].loops.push(hole),
            None => {
                if let Some(first) = regions.first_mut() {
                    first.loops.push(hole);
                }
            }
        }
    }
    regions
}

// ---------------------------------------------------------------------------
// Surface parameterisation
// ---------------------------------------------------------------------------

fn is_periodic(surface: Surface) -> bool {
    matches!(
        surface,
        Surface::Cylinder(_) | Surface::Cone(_) | Surface::Sphere(_) | Surface::Torus(_)
    )
}

/// The frame of a revolved carrier: origin, axis, radial u and v, and the
/// angular sign.
fn revolved_frame(surface: Surface) -> Option<(Point3, Vector3, Vector3, Vector3, f64)> {
    Some(match surface {
        Surface::Cylinder(c) => (c.origin, c.axis, c.radial_u, c.radial_v, c.angular_sign),
        Surface::Cone(c) => (c.origin, c.axis, c.radial_u, c.radial_v, c.angular_sign),
        Surface::Sphere(s) => (s.origin, s.axis, s.radial_u, s.radial_v, s.angular_sign),
        Surface::Torus(t) => (t.origin, t.axis, t.radial_u, t.radial_v, t.angular_sign),
        _ => return None,
    })
}

/// The azimuth of a point about a revolved carrier's axis, in `(−π, π]`,
/// or `None` on the axis.
fn azimuth(surface: Surface, point: Point3) -> Option<f64> {
    let (origin, axis, radial_u, radial_v, _) = revolved_frame(surface)?;
    let relative = point - origin;
    let radial = relative - axis * relative.dot(axis);
    if radial.length() <= 1.0e-12 {
        return None;
    }
    Some(radial.dot(radial_v).atan2(radial.dot(radial_u)))
}

/// A point's `(u, v)` on a revolved carrier, with `u` in the sign's own
/// units and NaN on the axis.
fn revolved_uv(surface: Surface, point: Point3) -> Option<Point2> {
    let (origin, axis, radial_u, _, sign) = revolved_frame(surface)?;
    let u = azimuth(surface, point).map_or(f64::NAN, |theta| sign * theta);
    let relative = point - origin;
    let v = match surface {
        Surface::Cylinder(_) | Surface::Cone(_) => relative.dot(axis),
        Surface::Sphere(sphere) => (relative.dot(axis) / sphere.radius).clamp(-1.0, 1.0).asin(),
        Surface::Torus(torus) => {
            let radial = relative - axis * relative.dot(axis);
            let outward = unit(radial).unwrap_or(radial_u);
            let from_ring = relative - outward * torus.major_radius;
            from_ring.dot(axis).atan2(from_ring.dot(outward))
        }
        _ => return None,
    };
    Some(Point2::new(u, v))
}

/// The `v` at which a revolved carrier's ring closes to a point, if the
/// given point sits at such a pole.
fn pole_latitude(surface: Surface, point: Point3) -> Option<f64> {
    match surface {
        Surface::Sphere(sphere) => {
            let relative = point - sphere.origin;
            let radial = relative - sphere.axis * relative.dot(sphere.axis);
            if radial.length() > 1.0e-6 * sphere.radius.max(1.0) {
                return None;
            }
            Some(if relative.dot(sphere.axis) >= 0.0 {
                std::f64::consts::FRAC_PI_2
            } else {
                -std::f64::consts::FRAC_PI_2
            })
        }
        Surface::Cone(cone) => {
            let relative = point - cone.origin;
            let radial = relative - cone.axis * relative.dot(cone.axis);
            if radial.length() > 1.0e-6 * cone.base_radius.max(1.0) {
                return None;
            }
            let apex = -cone.base_radius / cone.slope;
            ((relative.dot(cone.axis) - apex).abs() <= 1.0e-6 * apex.abs().max(1.0)).then_some(apex)
        }
        _ => None,
    }
}

/// The pcurve of an oriented edge on a surface: the curve, its parameter
/// range, and its two ends in `(u, v)`.
fn pcurve_for(
    surface: Surface,
    curve: Curve3,
    range: ParameterRange,
    ends: [Point3; 2],
    face: u64,
    edge: u64,
) -> Result<(Curve2, ParameterRange, Point2, Point2), Refusal> {
    let unsupported = |what: &str| {
        Refusal::new(
            FACE_UNSUPPORTED,
            Some(face),
            format!(
                "edge #{edge} is {what}, which the kernel has no pcurve for on this face's surface"
            ),
        )
    };
    match surface {
        Surface::Plane(plane) => {
            let (pcurve, prange) = match curve {
                Curve3::Line { .. } => {
                    Curve2::line_segment([plane.project(ends[0]), plane.project(ends[1])])
                }
                Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                } => (
                    Curve2::Circle {
                        center: plane.project(center),
                        u: project_direction(plane, u),
                        v: project_direction(plane, v),
                        radius,
                    },
                    range,
                ),
                Curve3::Ellipse {
                    center,
                    u,
                    v,
                    major_radius,
                    minor_radius,
                } => (
                    Curve2::Ellipse {
                        center: plane.project(center),
                        u: project_direction(plane, u),
                        v: project_direction(plane, v),
                        major_radius,
                        minor_radius,
                    },
                    range,
                ),
                Curve3::Bspline { curve } => (
                    Curve2::Bspline {
                        curve: plane_pcurve(curve, plane)
                            .ok_or_else(|| unsupported("a B-spline the plane cannot carry"))?,
                    },
                    range,
                ),
                Curve3::Trace { .. } => return Err(unsupported("a surface trace")),
            };
            let start = pcurve.evaluate(prange.start);
            let end = pcurve.evaluate(prange.end);
            Ok((pcurve, prange, start, end))
        }
        Surface::Cylinder(_) | Surface::Cone(_) | Surface::Sphere(_) | Surface::Torus(_) => {
            let (origin, axis, _, _, sign) = revolved_frame(surface).expect("revolved");
            match curve {
                Curve3::Line { endpoints }
                    if matches!(surface, Surface::Cylinder(_) | Surface::Cone(_)) =>
                {
                    let direction = unit(endpoints[1] - endpoints[0])
                        .ok_or_else(|| unsupported("a line of no length"))?;
                    let start = revolved_uv(surface, ends[0]).expect("revolved");
                    let end = revolved_uv(surface, ends[1]).expect("revolved");
                    // A generator: its azimuth is one value, taken where it
                    // is defined (a cone's apex has none).
                    let u = if start.x.is_finite() { start.x } else { end.x };
                    if !u.is_finite() {
                        return Err(unsupported("a line along the axis"));
                    }
                    if start.x.is_finite() && end.x.is_finite() {
                        let drift = angle_difference(start.x, end.x);
                        if drift > 1.0e-6 {
                            return Err(unsupported(
                                "a line that is not a generator of the surface",
                            ));
                        }
                    }
                    let radial = direction - axis * direction.dot(axis);
                    if let Surface::Cylinder(_) = surface
                        && radial.length() > 1.0e-6
                    {
                        return Err(unsupported(
                            "a line that is not a generator of the cylinder",
                        ));
                    }
                    let (a, b) = (Point2::new(u, start.y), Point2::new(u, end.y));
                    Ok((
                        Curve2::line_segment([a, b]).0,
                        ParameterRange::new(0.0, 1.0),
                        a,
                        b,
                    ))
                }
                Curve3::Circle {
                    center,
                    u,
                    v,
                    radius,
                } => {
                    let normal = u.cross(v);
                    let on_axis = {
                        let relative = center - origin;
                        (relative - axis * relative.dot(axis)).length() <= 1.0e-6 * radius.max(1.0)
                    };
                    if on_axis && normal.cross(axis).length() <= 1.0e-6 {
                        // A ring at fixed `v`: the azimuth runs affinely with
                        // the circle's parameter.
                        let start = revolved_uv(surface, ends[0]).expect("revolved");
                        let end = revolved_uv(surface, ends[1]).expect("revolved");
                        // A torus ring's minor angle is periodic: the inner
                        // equator reads as `π` at one end and `−π` at the
                        // other.
                        let v_ring = if matches!(surface, Surface::Torus(_)) {
                            if angle_difference(start.y, end.y) > 1.0e-6 {
                                return Err(unsupported(
                                    "a circle that is not a ring of the surface",
                                ));
                            }
                            read::canonical_angle(start.y)
                        } else {
                            if (start.y - end.y).abs() > 1.0e-6 * (1.0 + start.y.abs()) {
                                return Err(unsupported(
                                    "a circle that is not a ring of the surface",
                                ));
                            }
                            (start.y + end.y) / 2.0
                        };
                        let phase = azimuth(surface, center + u * radius).unwrap_or(0.0);
                        let handed = normal.dot(axis).signum();
                        // u(t) = sign·(phase + handed·(t − t₀)) from t₀ = the
                        // circle's own zero, anchored at the start end.
                        let u0 = if start.x.is_finite() {
                            start.x
                        } else {
                            sign * (phase + handed * range.start)
                        };
                        let u1 = u0 + sign * handed * (range.end - range.start);
                        let (a, b) = (Point2::new(u0, v_ring), Point2::new(u1, v_ring));
                        return Ok((
                            Curve2::line_segment([a, b]).0,
                            ParameterRange::new(0.0, 1.0),
                            a,
                            b,
                        ));
                    }
                    match surface {
                        Surface::Sphere(sphere) => {
                            // A meridian: a great circle through the poles.
                            if center.distance(sphere.origin) > 1.0e-6 * radius.max(1.0)
                                || normal.dot(axis).abs() > 1.0e-6
                            {
                                return Err(unsupported(
                                    "a circle that is neither a ring nor a meridian of the sphere",
                                ));
                            }
                            let middle = curve.evaluate((range.start + range.end) / 2.0);
                            let u_mid = revolved_uv(surface, middle).expect("revolved").x;
                            if !u_mid.is_finite() {
                                return Err(unsupported("a meridian with no azimuth"));
                            }
                            let start = revolved_uv(surface, ends[0]).expect("revolved");
                            let end = revolved_uv(surface, ends[1]).expect("revolved");
                            // The latitude runs affinely with the parameter:
                            // which way is read off the middle of the arc,
                            // where the latitude is changing even when the
                            // arc starts at a pole and its rate there is
                            // nothing.
                            let v_mid = revolved_uv(surface, middle).expect("revolved").y;
                            let towards_middle = (v_mid - start.y) * (range.end - range.start);
                            let sense = if towards_middle.abs() > 1.0e-12 {
                                towards_middle.signum()
                            } else if curve.derivative(range.start).dot(axis) >= 0.0 {
                                1.0
                            } else {
                                -1.0
                            };
                            let v0 = snap_latitude(start.y);
                            let v1 = snap_latitude(v0 + sense * (range.end - range.start));
                            if (v1 - end.y).abs() > 1.0e-6 {
                                return Err(unsupported(
                                    "a meridian whose parameter does not follow the latitude",
                                ));
                            }
                            let (a, b) = (Point2::new(u_mid, v0), Point2::new(u_mid, v1));
                            Ok((
                                Curve2::line_segment([a, b]).0,
                                ParameterRange::new(0.0, 1.0),
                                a,
                                b,
                            ))
                        }
                        Surface::Torus(torus) => {
                            // A minor circle at one azimuth.
                            let relative = center - torus.origin;
                            let ring = (relative - axis * relative.dot(axis)).length();
                            if (ring - torus.major_radius).abs()
                                > 1.0e-6 * torus.major_radius.max(1.0)
                                || relative.dot(axis).abs() > 1.0e-6 * torus.major_radius.max(1.0)
                                || normal.dot(axis).abs() > 1.0e-6
                                || (radius - torus.minor_radius).abs() > 1.0e-6 * radius.max(1.0)
                            {
                                return Err(unsupported(
                                    "a circle that is neither a ring nor a minor circle of the torus",
                                ));
                            }
                            let u_center = revolved_uv(surface, center).expect("revolved").x;
                            let start = revolved_uv(surface, ends[0]).expect("revolved");
                            let end = revolved_uv(surface, ends[1]).expect("revolved");
                            let outward =
                                unit(relative - axis * relative.dot(axis)).expect("off axis");
                            let tangent = curve.derivative(range.start);
                            // dv/dt at the start: the minor angle grows towards the axis.
                            let arm = ends[0] - center;
                            let rate = tangent.dot(axis) * arm.dot(outward)
                                - tangent.dot(outward) * arm.dot(axis);
                            let sense = if rate >= 0.0 { 1.0 } else { -1.0 };
                            let v0 = start.y;
                            let v1 = v0 + sense * (range.end - range.start);
                            if angle_difference(v1, end.y) > 1.0e-6 {
                                return Err(unsupported(
                                    "a minor circle whose parameter does not follow the minor angle",
                                ));
                            }
                            let (a, b) = (Point2::new(u_center, v0), Point2::new(u_center, v1));
                            Ok((
                                Curve2::line_segment([a, b]).0,
                                ParameterRange::new(0.0, 1.0),
                                a,
                                b,
                            ))
                        }
                        _ => Err(unsupported("a circle that is not a ring of the surface")),
                    }
                }
                Curve3::Ellipse {
                    center,
                    u,
                    v,
                    major_radius: _,
                    minor_radius,
                } if matches!(surface, Surface::Cylinder(_)) => {
                    let Surface::Cylinder(cylinder) = surface else {
                        unreachable!()
                    };
                    let normal = unit(u.cross(v))
                        .ok_or_else(|| unsupported("an ellipse with a degenerate frame"))?;
                    let along = normal.dot(axis);
                    if along.abs() <= 1.0e-9
                        || (minor_radius - cylinder.radius).abs()
                            > 1.0e-6 * cylinder.radius.max(1.0)
                    {
                        return Err(unsupported(
                            "an ellipse that is not a plane section of the cylinder",
                        ));
                    }
                    let nx = normal.dot(cylinder.radial_u);
                    let ny = normal.dot(cylinder.radial_v);
                    let mean = normal.dot(center - origin) / along;
                    let amplitude = -cylinder.radius * nx.hypot(ny) / along;
                    let phi = ny.atan2(nx);
                    let start = revolved_uv(surface, ends[0]).expect("revolved");
                    let sense = along.signum();
                    let u0 = start.x;
                    let u1 = u0 + sign * sense * (range.end - range.start);
                    let pcurve = Curve2::Harmonic {
                        mean,
                        amplitude,
                        phase: sign * phi,
                    };
                    let prange = ParameterRange::new(u0, u1);
                    let a = pcurve.evaluate(u0);
                    let b = pcurve.evaluate(u1);
                    let end = revolved_uv(surface, ends[1]).expect("revolved");
                    if (b.y - end.y).abs() > 1.0e-6 * (1.0 + end.y.abs())
                        || angle_difference(b.x, end.x) > 1.0e-6
                    {
                        return Err(unsupported(
                            "an ellipse whose parameter does not follow the azimuth",
                        ));
                    }
                    Ok((pcurve, prange, a, b))
                }
                Curve3::Line { .. } => Err(unsupported("a straight line")),
                Curve3::Ellipse { .. } => Err(unsupported("an ellipse")),
                Curve3::Bspline { .. } => Err(unsupported("a B-spline")),
                Curve3::Trace { .. } => Err(unsupported("a surface trace")),
            }
        }
        Surface::Bspline(spline) => {
            let (u_min, u_max, v_min, v_max) = spline.domain();
            let invert = |point: Point3| -> Result<Point2, Refusal> {
                let found = spline.invert(point, None).ok_or_else(|| {
                    unsupported("an edge whose ends the surface cannot be inverted at")
                })?;
                let gap = spline.evaluate(found).distance(point);
                if gap > 1.0e-6 * spline.scale().max(1.0) {
                    return Err(Refusal::new(
                        GAP_EXCEEDS_TOLERANCE,
                        Some(face),
                        format!("edge #{edge} stands {gap:.3e} off the B-spline face"),
                    )
                    .with_measure(gap, 1.0e-6));
                }
                Ok(found)
            };
            let mut start = invert(ends[0])?;
            let mut end = invert(ends[1])?;
            let snap = |value: f64, low: f64, high: f64| {
                let span = (high - low).max(1.0e-12);
                if (value - low).abs() <= 1.0e-6 * span {
                    low
                } else if (value - high).abs() <= 1.0e-6 * span {
                    high
                } else {
                    value
                }
            };
            start = Point2::new(snap(start.x, u_min, u_max), snap(start.y, v_min, v_max));
            end = Point2::new(snap(end.x, u_min, u_max), snap(end.y, v_min, v_max));
            // An iso-line: whichever coordinate agrees at both ends is held.
            let du = (start.x - end.x).abs();
            let dv = (start.y - end.y).abs();
            if du <= 1.0e-6 * (u_max - u_min).max(1.0e-12) && du <= dv {
                end.x = start.x;
            } else if dv <= 1.0e-6 * (v_max - v_min).max(1.0e-12) {
                end.y = start.y;
            } else {
                return Err(unsupported(
                    "a curve that is not an iso-parameter line of the B-spline surface (ADR 0050)",
                ));
            }
            if !matches!(curve, Curve3::Line { .. } | Curve3::Bspline { .. }) {
                return Err(unsupported(
                    "a curve of a kind a B-spline face cannot carry",
                ));
            }
            Ok((
                Curve2::line_segment([start, end]).0,
                ParameterRange::new(0.0, 1.0),
                start,
                end,
            ))
        }
        Surface::Ruled(_) => Err(unsupported(
            "an edge of a ruled face, which import never builds",
        )),
    }
}

fn project_direction(plane: crate::topology::Plane, direction: Vector3) -> Vector2 {
    let u = direction.dot(plane.u) / plane.u.dot(plane.u);
    let v = direction.dot(plane.v) / plane.v.dot(plane.v);
    Vector2::new(u, v)
}

fn snap_latitude(v: f64) -> f64 {
    let half = std::f64::consts::FRAC_PI_2;
    if (v - half).abs() <= 1.0e-9 {
        half
    } else if (v + half).abs() <= 1.0e-9 {
        -half
    } else {
        v
    }
}

/// The smallest difference between two angles, modulo a turn.
fn angle_difference(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(TAU);
    d.min(TAU - d)
}

/// The kernel edge along a seam line of a revolved carrier between two
/// parameter values: a generator, a meridian, or a minor circle.
fn seam_curve(
    surface: Surface,
    axis: Axis,
    line: f64,
    from: f64,
    to: f64,
    start: Point3,
    end: Point3,
) -> (Curve3, ParameterRange) {
    match (surface, axis) {
        (Surface::Cylinder(_) | Surface::Cone(_), Axis::U) => Curve3::line_segment([start, end]),
        (Surface::Sphere(sphere), Axis::U) => {
            let theta = sphere.angular_sign * line;
            let radial = sphere.radial_u * theta.cos() + sphere.radial_v * theta.sin();
            (
                Curve3::Circle {
                    center: sphere.origin,
                    u: radial,
                    v: sphere.axis,
                    radius: sphere.radius,
                },
                ParameterRange::new(from, to),
            )
        }
        (Surface::Torus(torus), Axis::U) => {
            let theta = torus.angular_sign * line;
            let radial = torus.radial_u * theta.cos() + torus.radial_v * theta.sin();
            (
                Curve3::Circle {
                    center: torus.origin + radial * torus.major_radius,
                    u: radial,
                    v: torus.axis,
                    radius: torus.minor_radius,
                },
                ParameterRange::new(from, to),
            )
        }
        (Surface::Torus(torus), Axis::V) => {
            // A ring at a fixed minor angle, from azimuth `from` to `to`.
            let ring = torus.major_radius + torus.minor_radius * line.cos();
            (
                Curve3::Circle {
                    center: torus.origin + torus.axis * (torus.minor_radius * line.sin()),
                    u: torus.radial_u,
                    v: torus.radial_v,
                    radius: ring,
                },
                ParameterRange::new(torus.angular_sign * from, torus.angular_sign * to),
            )
        }
        _ => Curve3::line_segment([start, end]),
    }
}

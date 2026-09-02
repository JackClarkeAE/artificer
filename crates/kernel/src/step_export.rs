//! Exact STEP export: the B-rep as AP214 `advanced_brep_shape_representation`.
//!
//! The mapping is direct because the topology is already STEP-shaped. The
//! five carrier surfaces become the five STEP elementary surfaces; lines,
//! circles and ellipses become `line`, `circle` and `ellipse`; every coedge
//! is an `oriented_edge` over an `edge_curve` with an exact 3D curve and no
//! curve-on-surface, which STEP permits when the 3D curves are exact. Two
//! half faces per revolved carrier and their seam edges are ordinary
//! topology. Cavities are `brep_with_voids`.
//!
//! Every surface is written so that its STEP normal is the direction the
//! kernel's own parameterisation calls outward; where the kernel's face is
//! the inward-facing half of a carrier, the face says `same_sense = .F.`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use artificer_protocol::{KernelError, KernelErrorCode, KernelStage};

use crate::topology::{
    Curve3, EdgeKey, Orientation, Point3, Surface, Topology, Vector3, VertexKey, frame_orientation,
};
use crate::{DebugTriangle, NativeKernel, Snapshot, error};

/// The AP214 schema identifier the file claims.
const SCHEMA: &str = "AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }";

/// Where a body sits in the exported file: a proper rigid placement, as a
/// rotation matrix by columns and a translation. Geometry is written in
/// the placed position; a rotation keeps every normal and every
/// orientation flag as the kernel has them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepPlacement {
    /// The images of the X, Y and Z axes.
    pub columns: [[f64; 3]; 3],
    pub translation: [f64; 3],
}

impl StepPlacement {
    pub const IDENTITY: Self = Self {
        columns: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translation: [0.0, 0.0, 0.0],
    };

    fn rotate(&self, vector: Vector3) -> Vector3 {
        let [x, y, z] = self.columns;
        Vector3::new(
            x[0] * vector.x + y[0] * vector.y + z[0] * vector.z,
            x[1] * vector.x + y[1] * vector.y + z[1] * vector.z,
            x[2] * vector.x + y[2] * vector.y + z[2] * vector.z,
        )
    }

    fn place(&self, point: Point3) -> Point3 {
        let rotated = self.rotate(point.as_vector());
        Point3::new(
            rotated.x + self.translation[0],
            rotated.y + self.translation[1],
            rotated.z + self.translation[2],
        )
    }

    /// Whether the placement is a proper rigid motion: orthonormal columns
    /// with a right-handed determinant, so normals transform as vectors.
    fn is_rigid(&self) -> bool {
        let [x, y, z] = self
            .columns
            .map(|column| Vector3::new(column[0], column[1], column[2]));
        let near = |value: f64, target: f64| (value - target).abs() < 1.0e-9;
        near(x.length(), 1.0)
            && near(y.length(), 1.0)
            && near(z.length(), 1.0)
            && near(x.dot(y), 0.0)
            && near(y.dot(z), 0.0)
            && near(x.dot(z), 0.0)
            && near(x.cross(y).dot(z), 1.0)
            && self.translation.iter().all(|value| value.is_finite())
    }
}

impl NativeKernel {
    /// Writes the snapshot's solids as exact AP214 B-rep, in millimetres.
    /// Every face, edge and vertex of the committed topology is written as
    /// itself; nothing is tessellated.
    pub fn export_step(snapshot: &Snapshot, name: &str) -> Result<String, KernelError> {
        Self::export_step_bodies(&[(snapshot, name)], name)
    }

    /// One STEP file holding several bodies, each a solid of its own under
    /// one product.
    pub fn export_step_bodies(
        bodies: &[(&Snapshot, &str)],
        product: &str,
    ) -> Result<String, KernelError> {
        let placed: Vec<(&Snapshot, &str, StepPlacement)> = bodies
            .iter()
            .map(|(snapshot, name)| (*snapshot, *name, StepPlacement::IDENTITY))
            .collect();
        Self::export_step_bodies_placed(&placed, product)
    }

    /// One STEP file holding several bodies, each placed by a rigid
    /// transform, as an assembly's occurrences are.
    pub fn export_step_bodies_placed(
        bodies: &[(&Snapshot, &str, StepPlacement)],
        product: &str,
    ) -> Result<String, KernelError> {
        let mut file = StepFile::new(product);
        let mut solids = Vec::new();
        for (snapshot, name, placement) in bodies {
            if !placement.is_rigid() {
                return Err(error(
                    KernelErrorCode::InvalidInput,
                    KernelStage::Preflight,
                    snapshot.id,
                    "a STEP placement must be a proper rigid motion",
                    Vec::new(),
                ));
            }
            if snapshot.topology.solids.is_empty() {
                return Err(error(
                    KernelErrorCode::InvalidInput,
                    KernelStage::Preflight,
                    snapshot.id,
                    "the snapshot has no solid to export",
                    Vec::new(),
                ));
            }
            let mut writer = BodyWriter {
                file: &mut file,
                topology: &snapshot.topology,
                placement: *placement,
                vertices: BTreeMap::new(),
                edges: BTreeMap::new(),
            };
            for (index, solid) in snapshot.topology.solids.iter().enumerate() {
                let label = if snapshot.topology.solids.len() == 1 {
                    (*name).to_owned()
                } else {
                    format!("{name} {}", index + 1)
                };
                let outer = writer
                    .shell(solid.value.outer_shell, &label)
                    .map_err(|why| {
                        error(
                            KernelErrorCode::Unsupported,
                            KernelStage::Construction,
                            snapshot.id,
                            why,
                            Vec::new(),
                        )
                    })?;
                let mut voids = Vec::new();
                for inner in &solid.value.inner_shells {
                    let shell = writer
                        .shell(*inner, &format!("{label} void"))
                        .map_err(|why| {
                            error(
                                KernelErrorCode::Unsupported,
                                KernelStage::Construction,
                                snapshot.id,
                                why,
                                Vec::new(),
                            )
                        })?;
                    // The kernel orients a cavity shell into the void, which
                    // is the orientation STEP asks of a void; the oriented
                    // shell keeps it.
                    voids.push(
                        writer
                            .file
                            .entity(format!("ORIENTED_CLOSED_SHELL('',*,#{shell},.T.)")),
                    );
                }
                let solid_id = if voids.is_empty() {
                    writer
                        .file
                        .entity(format!("MANIFOLD_SOLID_BREP({},#{outer})", quoted(&label)))
                } else {
                    writer.file.entity(format!(
                        "BREP_WITH_VOIDS({},#{outer},({}))",
                        quoted(&label),
                        ids(&voids)
                    ))
                };
                solids.push(solid_id);
            }
        }
        Ok(file.finish(&solids, "ADVANCED_BREP_SHAPE_REPRESENTATION"))
    }

    /// Writes the snapshot's display tessellation as an AP214 faceted
    /// surface model, for consumers that want triangles in a STEP wrapper.
    #[must_use]
    pub fn export_step_faceted(snapshot: &Snapshot, name: &str) -> String {
        let scene = Self::debug_scene(snapshot);
        let mut file = StepFile::new(name);
        let faces: Vec<u64> = scene
            .triangles
            .iter()
            .map(|triangle: &DebugTriangle| {
                let points: Vec<u64> = triangle
                    .vertices
                    .iter()
                    .map(|vertex| {
                        file.entity(format!(
                            "CARTESIAN_POINT('',({},{},{}))",
                            real(vertex.x),
                            real(vertex.y),
                            real(vertex.z)
                        ))
                    })
                    .collect();
                let poly = file.entity(format!("POLY_LOOP('',({}))", ids(&points)));
                let bound = file.entity(format!("FACE_OUTER_BOUND('',#{poly},.T.)"));
                file.entity(format!("FACE('',(#{bound}))"))
            })
            .collect();
        let shell = file.entity(format!("OPEN_SHELL({},({}))", quoted(name), ids(&faces)));
        let model = file.entity(format!(
            "SHELL_BASED_SURFACE_MODEL({},(#{shell}))",
            quoted(name)
        ));
        file.finish(&[model], "MANIFOLD_SURFACE_SHAPE_REPRESENTATION")
    }
}

/// A Part 21 file under construction: the header and product structure
/// are fixed; entities are numbered as they are added.
struct StepFile {
    data: String,
    next: u64,
    context: u64,
    shape: u64,
}

impl StepFile {
    fn new(product: &str) -> Self {
        let mut file = Self {
            data: String::new(),
            next: 1,
            context: 0,
            shape: 0,
        };
        let name = quoted(product);
        let application = file.entity("APPLICATION_CONTEXT('automotive design')".to_owned());
        let product_context =
            file.entity(format!("PRODUCT_CONTEXT('',#{application},'mechanical')"));
        let product_id = file.entity(format!("PRODUCT({name},{name},'',(#{product_context}))"));
        let formation = file.entity(format!("PRODUCT_DEFINITION_FORMATION('','',#{product_id})"));
        let definition_context = file.entity(format!(
            "PRODUCT_DEFINITION_CONTEXT('part definition',#{application},'design')"
        ));
        let definition = file.entity(format!(
            "PRODUCT_DEFINITION('design','',#{formation},#{definition_context})"
        ));
        file.shape = file.entity(format!("PRODUCT_DEFINITION_SHAPE('','',#{definition})"));
        let length = file.entity("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))".to_owned());
        let angle = file.entity("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))".to_owned());
        let solid_angle =
            file.entity("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())".to_owned());
        let uncertainty = file.entity(format!(
            "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-6),#{length},'distance_accuracy_value','confusion accuracy')"
        ));
        file.context = file.entity(format!(
            "(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncertainty}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{length},#{angle},#{solid_angle}))REPRESENTATION_CONTEXT('',''))"
        ));
        file
    }

    /// Appends one entity and returns its number.
    fn entity(&mut self, body: String) -> u64 {
        let id = self.next;
        self.next += 1;
        let _ = writeln!(self.data, "#{id}={body};");
        id
    }

    fn finish(mut self, items: &[u64], representation: &str) -> String {
        let origin = self.entity("CARTESIAN_POINT('',(0.,0.,0.))".to_owned());
        let z = self.entity("DIRECTION('',(0.,0.,1.))".to_owned());
        let x = self.entity("DIRECTION('',(1.,0.,0.))".to_owned());
        let placement = self.entity(format!("AXIS2_PLACEMENT_3D('',#{origin},#{z},#{x})"));
        let mut listed = vec![placement];
        listed.extend_from_slice(items);
        let representation = self.entity(format!(
            "{representation}('',({}),#{})",
            ids(&listed),
            self.context
        ));
        self.entity(format!(
            "SHAPE_DEFINITION_REPRESENTATION(#{},#{representation})",
            self.shape
        ));
        let mut file = String::with_capacity(self.data.len() + 512);
        file.push_str("ISO-10303-21;\nHEADER;\n");
        file.push_str("FILE_DESCRIPTION(('Artificer exact B-rep export'),'2;1');\n");
        file.push_str(
            "FILE_NAME('Artificer.step','',('Artificer'),(''),'Artificer','Artificer','');\n",
        );
        let _ = writeln!(file, "FILE_SCHEMA(('{SCHEMA}'));");
        file.push_str("ENDSEC;\nDATA;\n");
        file.push_str(&self.data);
        file.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        file
    }

    fn point(&mut self, point: Point3) -> u64 {
        self.entity(format!(
            "CARTESIAN_POINT('',({},{},{}))",
            real(point.x),
            real(point.y),
            real(point.z)
        ))
    }

    fn direction(&mut self, vector: Vector3) -> u64 {
        self.entity(format!(
            "DIRECTION('',({},{},{}))",
            real(vector.x),
            real(vector.y),
            real(vector.z)
        ))
    }

    /// An `axis2_placement_3d` at `origin` with `axis` as its Z and `reference`
    /// as its X; both are written as unit vectors.
    fn placement(&mut self, origin: Point3, axis: Vector3, reference: Vector3) -> u64 {
        let origin = self.point(origin);
        let axis = self.direction(axis);
        let reference = self.direction(reference);
        self.entity(format!(
            "AXIS2_PLACEMENT_3D('',#{origin},#{axis},#{reference})"
        ))
    }
}

/// Writes one body's shells, sharing vertices and edges across faces.
struct BodyWriter<'a> {
    file: &'a mut StepFile,
    topology: &'a Topology,
    placement: StepPlacement,
    vertices: BTreeMap<VertexKey, u64>,
    edges: BTreeMap<EdgeKey, u64>,
}

impl BodyWriter<'_> {
    fn point(&mut self, point: Point3) -> u64 {
        let placed = self.placement.place(point);
        self.file.point(placed)
    }

    fn direction(&mut self, vector: Vector3) -> u64 {
        let rotated = self.placement.rotate(vector);
        self.file.direction(rotated)
    }

    fn placement(&mut self, origin: Point3, axis: Vector3, reference: Vector3) -> u64 {
        let origin = self.placement.place(origin);
        let axis = self.placement.rotate(axis);
        let reference = self.placement.rotate(reference);
        self.file.placement(origin, axis, reference)
    }

    fn shell(&mut self, key: crate::topology::ShellKey, label: &str) -> Result<u64, String> {
        let shell = self
            .topology
            .shell(key)
            .ok_or_else(|| "a solid refers to a shell that does not exist".to_owned())?;
        let mut faces = Vec::new();
        for face_key in &shell.value.faces {
            faces.push(self.face(*face_key)?);
        }
        Ok(self
            .file
            .entity(format!("CLOSED_SHELL({},({}))", quoted(label), ids(&faces))))
    }

    fn face(&mut self, key: crate::topology::FaceKey) -> Result<u64, String> {
        let face = self
            .topology
            .face(key)
            .ok_or_else(|| "a shell refers to a face that does not exist".to_owned())?;
        let (surface, same_sense) = self.surface(face.value.surface)?;
        let mut bounds = Vec::new();
        for (index, loop_key) in face.value.loops().enumerate() {
            let edge_loop = self.edge_loop(loop_key)?;
            let kind = if index == 0 {
                "FACE_OUTER_BOUND"
            } else {
                "FACE_BOUND"
            };
            bounds.push(self.file.entity(format!("{kind}('',#{edge_loop},.T.)")));
        }
        Ok(self.file.entity(format!(
            "ADVANCED_FACE('',({}),#{surface},{})",
            ids(&bounds),
            flag(same_sense)
        )))
    }

    fn edge_loop(&mut self, key: crate::topology::LoopKey) -> Result<u64, String> {
        let loop_record = self
            .topology
            .loop_record(key)
            .ok_or_else(|| "a face refers to a loop that does not exist".to_owned())?;
        let mut oriented = Vec::new();
        let mut pole = None;
        for coedge_key in &loop_record.value.coedges {
            let coedge = self
                .topology
                .coedge(*coedge_key)
                .ok_or_else(|| "a loop refers to a coedge that does not exist".to_owned())?;
            // A revolved pole is a zero-length edge from a vertex to itself.
            // STEP has no such edge: the loop simply closes through the
            // vertex, and a loop of nothing else is a vertex loop.
            let record = self
                .topology
                .edge(coedge.value.edge)
                .ok_or_else(|| "a coedge refers to an edge that does not exist".to_owned())?;
            if is_pole(&record.value) {
                pole = Some(record.value.vertices[0]);
                continue;
            }
            let edge = self.edge(coedge.value.edge)?;
            let forward = coedge.value.orientation == Orientation::Forward;
            oriented.push(
                self.file
                    .entity(format!("ORIENTED_EDGE('',*,*,#{edge},{})", flag(forward))),
            );
        }
        if oriented.is_empty() {
            let vertex = pole.ok_or("a loop has no edges")?;
            let vertex = self.vertex(vertex)?;
            return Ok(self.file.entity(format!("VERTEX_LOOP('',#{vertex})")));
        }
        Ok(self
            .file
            .entity(format!("EDGE_LOOP('',({}))", ids(&oriented))))
    }

    fn edge(&mut self, key: EdgeKey) -> Result<u64, String> {
        if let Some(id) = self.edges.get(&key) {
            return Ok(*id);
        }
        let edge = self
            .topology
            .edge(key)
            .ok_or_else(|| "a coedge refers to an edge that does not exist".to_owned())?;
        let [start, end] = edge.value.vertices;
        let start = self.vertex(start)?;
        let end = self.vertex(end)?;
        let range = edge.value.parameter_range;
        let curve = match edge.value.curve {
            Curve3::Line { endpoints } => {
                let direction = endpoints[1] - endpoints[0];
                let length = direction.length();
                let unit = unit(direction).ok_or("a line edge has no length")?;
                let origin = self.point(endpoints[0]);
                let direction = self.direction(unit);
                let vector = self
                    .file
                    .entity(format!("VECTOR('',#{direction},{})", real(length)));
                self.file.entity(format!("LINE('',#{origin},#{vector})"))
            }
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            } => {
                let axis = unit(u.cross(v)).ok_or("a circle edge has a degenerate frame")?;
                let reference = unit(u).ok_or("a circle edge has a degenerate frame")?;
                let placement = self.placement(center, axis, reference);
                self.file
                    .entity(format!("CIRCLE('',#{placement},{})", real(radius.abs())))
            }
            Curve3::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            } => {
                let axis = unit(u.cross(v)).ok_or("an ellipse edge has a degenerate frame")?;
                let reference = unit(u).ok_or("an ellipse edge has a degenerate frame")?;
                let placement = self.placement(center, axis, reference);
                self.file.entity(format!(
                    "ELLIPSE('',#{placement},{},{})",
                    real(major_radius.abs()),
                    real(minor_radius.abs())
                ))
            }
        };
        // The edge runs from its first vertex to its second; the curve's
        // parameter agrees with that unless the range runs backwards.
        let id = self.file.entity(format!(
            "EDGE_CURVE('',#{start},#{end},#{curve},{})",
            flag(range.end >= range.start)
        ));
        self.edges.insert(key, id);
        Ok(id)
    }

    fn vertex(&mut self, key: VertexKey) -> Result<u64, String> {
        if let Some(id) = self.vertices.get(&key) {
            return Ok(*id);
        }
        let vertex = self
            .topology
            .vertex(key)
            .ok_or_else(|| "an edge refers to a vertex that does not exist".to_owned())?;
        let point = self.point(vertex.value.point);
        let id = self.file.entity(format!("VERTEX_POINT('',#{point})"));
        self.vertices.insert(key, id);
        Ok(id)
    }

    /// The STEP surface of a carrier, and whether the kernel's outward
    /// normal agrees with the surface's own.
    fn surface(&mut self, surface: Surface) -> Result<(u64, bool), String> {
        Ok(match surface {
            Surface::Plane(plane) => {
                let normal = unit(plane.normal).ok_or("a planar face has a degenerate frame")?;
                let reference = unit(plane.u).ok_or("a planar face has a degenerate frame")?;
                let placement = self.placement(plane.origin, normal, reference);
                (self.file.entity(format!("PLANE('',#{placement})")), true)
            }
            Surface::Cylinder(cylinder) => {
                let axis = unit(cylinder.axis).ok_or("a cylindrical face has a degenerate axis")?;
                let reference =
                    unit(cylinder.radial_u).ok_or("a cylindrical face has a degenerate frame")?;
                let sign = frame_orientation(
                    cylinder.radial_u,
                    cylinder.radial_v,
                    axis,
                    cylinder.angular_sign,
                )
                .ok_or("a cylindrical face has a degenerate frame")?;
                let placement = self.placement(cylinder.origin, axis, reference);
                (
                    self.file.entity(format!(
                        "CYLINDRICAL_SURFACE('',#{placement},{})",
                        real(cylinder.radius.abs())
                    )),
                    sign > 0.0,
                )
            }
            Surface::Cone(cone) => {
                let axis = unit(cone.axis).ok_or("a conical face has a degenerate axis")?;
                let reference =
                    unit(cone.radial_u).ok_or("a conical face has a degenerate frame")?;
                let sign = frame_orientation(cone.radial_u, cone.radial_v, axis, cone.angular_sign)
                    .ok_or("a conical face has a degenerate frame")?;
                // STEP's semi-angle is positive and opens along the placement
                // axis; a cone that narrows along the kernel axis is written
                // along the opposite axis, which leaves its surface normal
                // exactly where the kernel's parameterisation puts it.
                let slope = cone.slope / cone.axis.length();
                let (axis, reference) = if slope >= 0.0 {
                    (axis, reference)
                } else {
                    (axis * -1.0, reference * -1.0)
                };
                let placement = self.placement(cone.origin, axis, reference);
                (
                    self.file.entity(format!(
                        "CONICAL_SURFACE('',#{placement},{},{})",
                        real(cone.base_radius.abs()),
                        real(slope.abs().atan())
                    )),
                    sign > 0.0,
                )
            }
            Surface::Sphere(sphere) => {
                let axis = unit(sphere.axis).ok_or("a spherical face has a degenerate axis")?;
                let reference =
                    unit(sphere.radial_u).ok_or("a spherical face has a degenerate frame")?;
                let sign =
                    frame_orientation(sphere.radial_u, sphere.radial_v, axis, sphere.angular_sign)
                        .ok_or("a spherical face has a degenerate frame")?;
                let placement = self.placement(sphere.origin, axis, reference);
                (
                    self.file.entity(format!(
                        "SPHERICAL_SURFACE('',#{placement},{})",
                        real(sphere.radius.abs())
                    )),
                    sign > 0.0,
                )
            }
            Surface::Torus(torus) => {
                let axis = unit(torus.axis).ok_or("a toroidal face has a degenerate axis")?;
                let reference =
                    unit(torus.radial_u).ok_or("a toroidal face has a degenerate frame")?;
                let sign =
                    frame_orientation(torus.radial_u, torus.radial_v, axis, torus.angular_sign)
                        .ok_or("a toroidal face has a degenerate frame")?;
                let placement = self.placement(torus.origin, axis, reference);
                (
                    self.file.entity(format!(
                        "TOROIDAL_SURFACE('',#{placement},{},{})",
                        real(torus.major_radius.abs()),
                        real(torus.minor_radius.abs())
                    )),
                    sign > 0.0,
                )
            }
        })
    }
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > f64::EPSILON).then(|| vector / length)
}

/// A zero-length line from a vertex to itself: the pole of a revolved
/// carrier, real topology to the kernel and nothing to STEP.
pub(crate) fn is_pole(edge: &crate::topology::Edge) -> bool {
    edge.vertices[0] == edge.vertices[1]
        && matches!(edge.curve, Curve3::Line { endpoints } if (endpoints[1] - endpoints[0]).length() <= f64::EPSILON)
}

fn flag(value: bool) -> &'static str {
    if value { ".T." } else { ".F." }
}

fn ids(ids: &[u64]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A Part 21 string literal: apostrophes doubled, non-ASCII replaced.
fn quoted(text: &str) -> String {
    let body: String = text
        .chars()
        .map(|character| {
            if character == '\'' {
                "''".to_owned()
            } else if character.is_ascii_graphic() || character == ' ' {
                character.to_string()
            } else {
                "_".to_owned()
            }
        })
        .collect();
    format!("'{body}'")
}

/// A Part 21 real: the shortest digits that read back to the same float,
/// always with a decimal point, and an uppercase exponent when there is
/// one.
pub(crate) fn real(value: f64) -> String {
    if !value.is_finite() {
        return "0.".to_owned();
    }
    let text = format!("{value:?}");
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa.to_owned(), Some(exponent.to_owned())),
        None => (text, None),
    };
    let mantissa = if mantissa.contains('.') {
        mantissa
    } else {
        format!("{mantissa}.")
    };
    match exponent {
        Some(exponent) => format!("{mantissa}E{exponent}"),
        None => mantissa,
    }
}

#[cfg(test)]
mod tests {
    use super::real;

    #[test]
    fn reals_carry_a_point_and_an_uppercase_exponent() {
        assert_eq!(real(1.0), "1.0");
        assert_eq!(real(-0.5), "-0.5");
        assert_eq!(real(1.0e-7), "1.E-7");
        assert_eq!(real(12345678901234567890.0), "1.2345678901234567E19");
        assert_eq!(real(f64::NAN), "0.");
    }
}

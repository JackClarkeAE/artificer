//! General-purpose generational B-rep storage and analytic primitives.
//!
//! The production feature kernel still has specialized reconstruction paths,
//! while this module owns the M3-neutral topology contract they can target.
//! Handles carry generations, periodic seams are explicit coedge uses, and an
//! edit publishes only after the complete candidate validates.

use std::marker::PhantomData;

use artificer_geometry::{Point2, Point3, Vector3};
use sha2::{Digest, Sha256};

#[derive(Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Handle<T> {
    slot: u32,
    generation: u32,
    marker: PhantomData<fn() -> T>,
}

impl<T> Copy for Handle<T> {}
impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Handle<T> {
    pub const fn slot(self) -> u32 {
        self.slot
    }
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

impl<T> Handle<T> {
    const fn new(slot: usize, generation: u32) -> Self {
        Self {
            slot: slot as u32,
            generation,
            marker: PhantomData,
        }
    }
}

#[derive(Clone, Debug)]
struct ArenaSlot<T> {
    generation: u32,
    value: Option<T>,
}

#[derive(Clone, Debug)]
pub struct Arena<T> {
    slots: Vec<ArenaSlot<T>>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T> Arena<T> {
    pub fn insert(&mut self, value: T) -> Handle<T> {
        if let Some((index, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.value.is_none())
        {
            slot.value = Some(value);
            return Handle::new(index, slot.generation);
        }
        let handle = Handle::new(self.slots.len(), 0);
        self.slots.push(ArenaSlot {
            generation: 0,
            value: Some(value),
        });
        handle
    }
    pub fn get(&self, handle: Handle<T>) -> Option<&T> {
        let slot = self.slots.get(handle.slot as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }
    pub fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        (slot.generation == handle.generation)
            .then_some(slot.value.as_mut())
            .flatten()
    }
    pub fn remove(&mut self, handle: Handle<T>) -> Option<T> {
        let slot = self.slots.get_mut(handle.slot as usize)?;
        if slot.generation != handle.generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        Some(value)
    }
    pub fn contains(&self, handle: Handle<T>) -> bool {
        self.get(handle).is_some()
    }
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn iter(&self) -> impl Iterator<Item = (Handle<T>, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            Some((Handle::new(index, slot.generation), slot.value.as_ref()?))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sense {
    Forward,
    Reverse,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveGeometry {
    Line {
        start: Point3,
        end: Point3,
    },
    Circle {
        center: Point3,
        axis: Vector3,
        radial: Vector3,
        radius: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SurfaceGeometry {
    Plane {
        origin: Point3,
        u: Vector3,
        v: Vector3,
    },
    Cylinder {
        origin: Point3,
        axis: Vector3,
        radial: Vector3,
        radius: f64,
    },
    Cone {
        apex: Point3,
        axis: Vector3,
        radial: Vector3,
        half_angle: f64,
    },
    Sphere {
        center: Point3,
        axis: Vector3,
        radial: Vector3,
        radius: f64,
    },
    Torus {
        center: Point3,
        axis: Vector3,
        radial: Vector3,
        major_radius: f64,
        minor_radius: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PcurveGeometry {
    Line { start: Point2, end: Point2 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub point: Point3,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub vertices: [Handle<Vertex>; 2],
    pub curve: CurveGeometry,
    pub periodic: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coedge {
    pub edge: Handle<Edge>,
    pub sense: Sense,
    pub pcurve: PcurveGeometry,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Loop {
    pub coedges: Vec<Handle<Coedge>>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub surface: SurfaceGeometry,
    pub loops: Vec<Handle<Loop>>,
    pub sense: Sense,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Shell {
    pub faces: Vec<Handle<Face>>,
    pub closed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solid {
    pub outer: Handle<Shell>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BrepCounts {
    pub vertices: usize,
    pub edges: usize,
    pub coedges: usize,
    pub loops: usize,
    pub faces: usize,
    pub shells: usize,
    pub solids: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Brep {
    pub vertices: Arena<Vertex>,
    pub edges: Arena<Edge>,
    pub coedges: Arena<Coedge>,
    pub loops: Arena<Loop>,
    pub faces: Arena<Face>,
    pub shells: Arena<Shell>,
    pub solids: Arena<Solid>,
}

impl Brep {
    pub fn counts(&self) -> BrepCounts {
        BrepCounts {
            vertices: self.vertices.len(),
            edges: self.edges.len(),
            coedges: self.coedges.len(),
            loops: self.loops.len(),
            faces: self.faces.len(),
            shells: self.shells.len(),
            solids: self.solids.len(),
        }
    }
    pub fn validate(&self) -> Result<(), Vec<BrepDiagnostic>> {
        let mut errors = Vec::new();
        for (handle, vertex) in self.vertices.iter() {
            if !vertex.point.is_finite() {
                errors.push(BrepDiagnostic::new(
                    BrepCode::NonFinite,
                    "vertex",
                    handle.slot,
                ));
            }
        }
        let mut edge_uses = vec![0usize; self.edges.slots.len()];
        for (handle, edge) in self.edges.iter() {
            if edge
                .vertices
                .iter()
                .any(|vertex| !self.vertices.contains(*vertex))
            {
                errors.push(BrepDiagnostic::new(
                    BrepCode::DanglingReference,
                    "edge",
                    handle.slot,
                ));
            }
            if !curve_valid(edge.curve) {
                errors.push(BrepDiagnostic::new(
                    BrepCode::InvalidGeometry,
                    "edge",
                    handle.slot,
                ));
            }
        }
        for (handle, coedge) in self.coedges.iter() {
            if self.edges.contains(coedge.edge) {
                edge_uses[coedge.edge.slot as usize] += 1;
            } else {
                errors.push(BrepDiagnostic::new(
                    BrepCode::DanglingReference,
                    "coedge",
                    handle.slot,
                ));
            }
        }
        for (handle, loop_record) in self.loops.iter() {
            if loop_record.coedges.is_empty() {
                errors.push(BrepDiagnostic::new(
                    BrepCode::LoopEmpty,
                    "loop",
                    handle.slot,
                ));
            }
            if loop_record
                .coedges
                .iter()
                .any(|coedge| !self.coedges.contains(*coedge))
            {
                errors.push(BrepDiagnostic::new(
                    BrepCode::DanglingReference,
                    "loop",
                    handle.slot,
                ));
            }
        }
        for (handle, face) in self.faces.iter() {
            if face.loops.is_empty() {
                errors.push(BrepDiagnostic::new(
                    BrepCode::FaceWithoutLoop,
                    "face",
                    handle.slot,
                ));
            }
            if face
                .loops
                .iter()
                .any(|loop_handle| !self.loops.contains(*loop_handle))
            {
                errors.push(BrepDiagnostic::new(
                    BrepCode::DanglingReference,
                    "face",
                    handle.slot,
                ));
            }
            if !surface_valid(face.surface) {
                errors.push(BrepDiagnostic::new(
                    BrepCode::InvalidGeometry,
                    "face",
                    handle.slot,
                ));
            }
        }
        let mut face_uses = vec![0usize; self.faces.slots.len()];
        for (handle, shell) in self.shells.iter() {
            if shell.faces.is_empty() {
                errors.push(BrepDiagnostic::new(
                    BrepCode::ShellEmpty,
                    "shell",
                    handle.slot,
                ));
            }
            for face in &shell.faces {
                if self.faces.contains(*face) {
                    face_uses[face.slot as usize] += 1;
                } else {
                    errors.push(BrepDiagnostic::new(
                        BrepCode::DanglingReference,
                        "shell",
                        handle.slot,
                    ));
                }
            }
        }
        for (handle, solid) in self.solids.iter() {
            if !self.shells.contains(solid.outer) {
                errors.push(BrepDiagnostic::new(
                    BrepCode::DanglingReference,
                    "solid",
                    handle.slot,
                ));
            } else if !self
                .shells
                .get(solid.outer)
                .is_some_and(|shell| shell.closed)
            {
                errors.push(BrepDiagnostic::new(
                    BrepCode::OpenShell,
                    "solid",
                    handle.slot,
                ));
            }
        }
        for (edge, use_count) in edge_uses.into_iter().enumerate() {
            if self
                .edges
                .slots
                .get(edge)
                .is_some_and(|slot| slot.value.is_some())
                && use_count != 2
            {
                errors.push(BrepDiagnostic::new(
                    BrepCode::EdgeUseCount,
                    "edge",
                    edge as u32,
                ));
            }
        }
        for (face, use_count) in face_uses.into_iter().enumerate() {
            if self
                .faces
                .slots
                .get(face)
                .is_some_and(|slot| slot.value.is_some())
                && use_count != 1
            {
                errors.push(BrepDiagnostic::new(
                    BrepCode::FaceUseCount,
                    "face",
                    face as u32,
                ));
            }
        }
        errors.sort_by_key(|error| (error.code, error.entity_kind, error.slot));
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
    pub fn edit(&self, operation: impl FnOnce(&mut Brep)) -> Result<Self, Vec<BrepDiagnostic>> {
        let mut candidate = self.clone();
        operation(&mut candidate);
        candidate.validate()?;
        Ok(candidate)
    }
    pub fn transformed(&self, transform: RigidTransform) -> Result<Self, BrepCode> {
        transform.validate()?;
        let mut result = self.clone();
        for (_, vertex) in result
            .vertices
            .slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| Some((i, s.value.as_mut()?)))
        {
            vertex.point = transform.point(vertex.point);
        }
        for (_, edge) in result
            .edges
            .slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| Some((i, s.value.as_mut()?)))
        {
            transform_curve(&mut edge.curve, transform);
        }
        for (_, face) in result
            .faces
            .slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, s)| Some((i, s.value.as_mut()?)))
        {
            transform_surface(&mut face.surface, transform);
        }
        Ok(result)
    }
    pub fn semantic_digest(&self) -> [u8; 32] {
        let mut text = String::new();
        use std::fmt::Write as _;
        let counts = self.counts();
        let _ = write!(text, "v1:{counts:?};");
        for (_, v) in self.vertices.iter() {
            let _ = write!(
                text,
                "v:{:016x},{:016x},{:016x};",
                v.point.x.to_bits(),
                v.point.y.to_bits(),
                v.point.z.to_bits()
            );
        }
        for (_, e) in self.edges.iter() {
            let _ = write!(
                text,
                "e:{}:{}:{};",
                e.vertices[0].slot, e.vertices[1].slot, e.periodic
            );
        }
        for (_, c) in self.coedges.iter() {
            let _ = write!(text, "c:{}:{:?};", c.edge.slot, c.sense);
        }
        for (_, l) in self.loops.iter() {
            let _ = write!(
                text,
                "l:{:?};",
                l.coedges.iter().map(|c| c.slot).collect::<Vec<_>>()
            );
        }
        for (_, f) in self.faces.iter() {
            let _ = write!(text, "f:{:?}:{:?};", f.surface, f.sense);
        }
        Sha256::digest(text.as_bytes()).into()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BrepCode {
    DanglingReference,
    NonFinite,
    InvalidGeometry,
    LoopEmpty,
    FaceWithoutLoop,
    ShellEmpty,
    OpenShell,
    EdgeUseCount,
    FaceUseCount,
    InvalidTransform,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrepDiagnostic {
    pub code: BrepCode,
    pub entity_kind: &'static str,
    pub slot: u32,
}
impl BrepDiagnostic {
    fn new(code: BrepCode, entity_kind: &'static str, slot: u32) -> Self {
        Self {
            code,
            entity_kind,
            slot,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidTransform {
    pub rotation: [[f64; 3]; 3],
    pub translation: Vector3,
}
impl RigidTransform {
    pub const fn identity() -> Self {
        Self {
            rotation: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            translation: Vector3::new(0., 0., 0.),
        }
    }
    pub fn validate(self) -> Result<(), BrepCode> {
        if !self.translation.is_finite() || self.rotation.iter().flatten().any(|v| !v.is_finite()) {
            return Err(BrepCode::InvalidTransform);
        }
        let row = |i: usize| {
            Vector3::new(
                self.rotation[i][0],
                self.rotation[i][1],
                self.rotation[i][2],
            )
        };
        let tolerance = 1e-12;
        if (row(0).length() - 1.).abs() > tolerance
            || (row(1).length() - 1.).abs() > tolerance
            || (row(2).length() - 1.).abs() > tolerance
            || row(0).dot(row(1)).abs() > tolerance
            || row(1).dot(row(2)).abs() > tolerance
            || row(2).dot(row(0)).abs() > tolerance
            || row(0).cross(row(1)).dot(row(2)) < 1. - tolerance
        {
            return Err(BrepCode::InvalidTransform);
        }
        Ok(())
    }
    fn vector(self, v: Vector3) -> Vector3 {
        Vector3::new(
            self.rotation[0][0] * v.x + self.rotation[0][1] * v.y + self.rotation[0][2] * v.z,
            self.rotation[1][0] * v.x + self.rotation[1][1] * v.y + self.rotation[1][2] * v.z,
            self.rotation[2][0] * v.x + self.rotation[2][1] * v.y + self.rotation[2][2] * v.z,
        )
    }
    fn point(self, p: Point3) -> Point3 {
        let v = self.vector(Vector3::new(p.x, p.y, p.z)) + self.translation;
        Point3::new(v.x, v.y, v.z)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Primitive {
    Box {
        size: Vector3,
    },
    Cylinder {
        radius: f64,
        height: f64,
    },
    Cone {
        radius: f64,
        height: f64,
    },
    Sphere {
        radius: f64,
    },
    Torus {
        major_radius: f64,
        minor_radius: f64,
    },
}

pub fn make_primitive(primitive: Primitive) -> Result<Brep, BrepCode> {
    match primitive {
        Primitive::Box { size } => make_box(size),
        Primitive::Cylinder { radius, height } => make_cylinder(radius, height),
        Primitive::Cone { radius, height } => make_cone(radius, height),
        Primitive::Sphere { radius } => make_sphere(radius),
        Primitive::Torus {
            major_radius,
            minor_radius,
        } => make_torus(major_radius, minor_radius),
    }
}

fn make_box(size: Vector3) -> Result<Brep, BrepCode> {
    if !size.is_finite() || size.x <= 0. || size.y <= 0. || size.z <= 0. {
        return Err(BrepCode::InvalidGeometry);
    }
    let mut b = Brep::default();
    let points = [
        Point3::new(0., 0., 0.),
        Point3::new(size.x, 0., 0.),
        Point3::new(size.x, size.y, 0.),
        Point3::new(0., size.y, 0.),
        Point3::new(0., 0., size.z),
        Point3::new(size.x, 0., size.z),
        Point3::new(size.x, size.y, size.z),
        Point3::new(0., size.y, size.z),
    ];
    let v = points.map(|point| b.vertices.insert(Vertex { point }));
    let definitions = [
        ([0, 3, 2, 1], Vector3::new(0., 0., -1.)),
        ([4, 5, 6, 7], Vector3::new(0., 0., 1.)),
        ([0, 1, 5, 4], Vector3::new(0., -1., 0.)),
        ([1, 2, 6, 5], Vector3::new(1., 0., 0.)),
        ([2, 3, 7, 6], Vector3::new(0., 1., 0.)),
        ([3, 0, 4, 7], Vector3::new(-1., 0., 0.)),
    ];
    let mut edge_map = std::collections::BTreeMap::<(usize, usize), Handle<Edge>>::new();
    let mut faces = Vec::new();
    for (indices, normal) in definitions {
        let mut uses = Vec::new();
        for pair in indices
            .into_iter()
            .zip(indices.into_iter().cycle().skip(1))
            .take(4)
        {
            let key = (pair.0.min(pair.1), pair.0.max(pair.1));
            let edge = *edge_map.entry(key).or_insert_with(|| {
                b.edges.insert(Edge {
                    vertices: [v[key.0], v[key.1]],
                    curve: CurveGeometry::Line {
                        start: points[key.0],
                        end: points[key.1],
                    },
                    periodic: false,
                })
            });
            let sense = if pair == key {
                Sense::Forward
            } else {
                Sense::Reverse
            };
            uses.push(b.coedges.insert(Coedge {
                edge,
                sense,
                pcurve: PcurveGeometry::Line {
                    start: Point2::new(0., 0.),
                    end: Point2::new(1., 0.),
                },
            }));
        }
        let loop_handle = b.loops.insert(Loop { coedges: uses });
        let u = if normal.z.abs() > 0.5 {
            Vector3::new(1., 0., 0.)
        } else {
            Vector3::new(0., 0., 1.)
        };
        let vv = normal.cross(u);
        faces.push(b.faces.insert(Face {
            surface: SurfaceGeometry::Plane {
                origin: points[indices[0]],
                u,
                v: vv,
            },
            loops: vec![loop_handle],
            sense: Sense::Forward,
        }));
    }
    finish(b, faces)
}

fn make_cylinder(radius: f64, height: f64) -> Result<Brep, BrepCode> {
    positive(radius)?;
    positive(height)?;
    let mut b = Brep::default();
    let bottom = b.vertices.insert(Vertex {
        point: Point3::new(radius, 0., 0.),
    });
    let top = b.vertices.insert(Vertex {
        point: Point3::new(radius, 0., height),
    });
    let radial = Vector3::new(1., 0., 0.);
    let axis = Vector3::new(0., 0., 1.);
    let bottom_circle = b.edges.insert(Edge {
        vertices: [bottom, bottom],
        curve: CurveGeometry::Circle {
            center: Point3::new(0., 0., 0.),
            axis,
            radial,
            radius,
        },
        periodic: true,
    });
    let top_circle = b.edges.insert(Edge {
        vertices: [top, top],
        curve: CurveGeometry::Circle {
            center: Point3::new(0., 0., height),
            axis,
            radial,
            radius,
        },
        periodic: true,
    });
    let seam = b.edges.insert(Edge {
        vertices: [bottom, top],
        curve: CurveGeometry::Line {
            start: Point3::new(radius, 0., 0.),
            end: Point3::new(radius, 0., height),
        },
        periodic: false,
    });
    let bottom_face = single_edge_face(
        &mut b,
        bottom_circle,
        SurfaceGeometry::Plane {
            origin: Point3::new(0., 0., 0.),
            u: radial,
            v: Vector3::new(0., -1., 0.),
        },
    );
    let top_face = single_edge_face(
        &mut b,
        top_circle,
        SurfaceGeometry::Plane {
            origin: Point3::new(0., 0., height),
            u: radial,
            v: Vector3::new(0., 1., 0.),
        },
    );
    let side = periodic_side_face(
        &mut b,
        bottom_circle,
        top_circle,
        seam,
        SurfaceGeometry::Cylinder {
            origin: Point3::new(0., 0., 0.),
            axis,
            radial,
            radius,
        },
    );
    finish(b, vec![bottom_face, top_face, side])
}
fn make_cone(radius: f64, height: f64) -> Result<Brep, BrepCode> {
    positive(radius)?;
    positive(height)?;
    let mut b = Brep::default();
    let base = b.vertices.insert(Vertex {
        point: Point3::new(radius, 0., 0.),
    });
    let apex = b.vertices.insert(Vertex {
        point: Point3::new(0., 0., height),
    });
    let axis = Vector3::new(0., 0., 1.);
    let radial = Vector3::new(1., 0., 0.);
    let circle = b.edges.insert(Edge {
        vertices: [base, base],
        curve: CurveGeometry::Circle {
            center: Point3::new(0., 0., 0.),
            axis,
            radial,
            radius,
        },
        periodic: true,
    });
    let seam = b.edges.insert(Edge {
        vertices: [base, apex],
        curve: CurveGeometry::Line {
            start: Point3::new(radius, 0., 0.),
            end: Point3::new(0., 0., height),
        },
        periodic: false,
    });
    let cap = single_edge_face(
        &mut b,
        circle,
        SurfaceGeometry::Plane {
            origin: Point3::new(0., 0., 0.),
            u: radial,
            v: Vector3::new(0., -1., 0.),
        },
    );
    let c0 = coedge(&mut b, circle, Sense::Forward);
    let s0 = coedge(&mut b, seam, Sense::Forward);
    let s1 = coedge(&mut b, seam, Sense::Reverse);
    let loop_handle = b.loops.insert(Loop {
        coedges: vec![c0, s0, s1],
    });
    let side = b.faces.insert(Face {
        surface: SurfaceGeometry::Cone {
            apex: Point3::new(0., 0., height),
            axis: Vector3::new(0., 0., -1.),
            radial,
            half_angle: (radius / height).atan(),
        },
        loops: vec![loop_handle],
        sense: Sense::Forward,
    });
    finish(b, vec![cap, side])
}
fn make_sphere(radius: f64) -> Result<Brep, BrepCode> {
    positive(radius)?;
    let mut b = Brep::default();
    let south = b.vertices.insert(Vertex {
        point: Point3::new(0., 0., -radius),
    });
    let north = b.vertices.insert(Vertex {
        point: Point3::new(0., 0., radius),
    });
    let seam = b.edges.insert(Edge {
        vertices: [south, north],
        curve: CurveGeometry::Circle {
            center: Point3::new(0., 0., 0.),
            axis: Vector3::new(0., 1., 0.),
            radial: Vector3::new(0., 0., -1.),
            radius,
        },
        periodic: false,
    });
    let c0 = coedge(&mut b, seam, Sense::Forward);
    let c1 = coedge(&mut b, seam, Sense::Reverse);
    let loop_handle = b.loops.insert(Loop {
        coedges: vec![c0, c1],
    });
    let face = b.faces.insert(Face {
        surface: SurfaceGeometry::Sphere {
            center: Point3::new(0., 0., 0.),
            axis: Vector3::new(0., 0., 1.),
            radial: Vector3::new(1., 0., 0.),
            radius,
        },
        loops: vec![loop_handle],
        sense: Sense::Forward,
    });
    finish(b, vec![face])
}
fn make_torus(major_radius: f64, minor_radius: f64) -> Result<Brep, BrepCode> {
    positive(major_radius)?;
    positive(minor_radius)?;
    if minor_radius >= major_radius {
        return Err(BrepCode::InvalidGeometry);
    }
    let mut b = Brep::default();
    let point = Point3::new(major_radius + minor_radius, 0., 0.);
    let vertex = b.vertices.insert(Vertex { point });
    let axis = Vector3::new(0., 0., 1.);
    let radial = Vector3::new(1., 0., 0.);
    let meridian = b.edges.insert(Edge {
        vertices: [vertex, vertex],
        curve: CurveGeometry::Circle {
            center: Point3::new(major_radius, 0., 0.),
            axis: Vector3::new(0., 1., 0.),
            radial,
            radius: minor_radius,
        },
        periodic: true,
    });
    let parallel = b.edges.insert(Edge {
        vertices: [vertex, vertex],
        curve: CurveGeometry::Circle {
            center: Point3::new(0., 0., 0.),
            axis,
            radial,
            radius: major_radius + minor_radius,
        },
        periodic: true,
    });
    let uses = vec![
        coedge(&mut b, meridian, Sense::Forward),
        coedge(&mut b, parallel, Sense::Forward),
        coedge(&mut b, meridian, Sense::Reverse),
        coedge(&mut b, parallel, Sense::Reverse),
    ];
    let loop_handle = b.loops.insert(Loop { coedges: uses });
    let face = b.faces.insert(Face {
        surface: SurfaceGeometry::Torus {
            center: Point3::new(0., 0., 0.),
            axis,
            radial,
            major_radius,
            minor_radius,
        },
        loops: vec![loop_handle],
        sense: Sense::Forward,
    });
    finish(b, vec![face])
}

fn finish(mut b: Brep, faces: Vec<Handle<Face>>) -> Result<Brep, BrepCode> {
    let shell = b.shells.insert(Shell {
        faces,
        closed: true,
    });
    b.solids.insert(Solid { outer: shell });
    b.validate().map_err(|_| BrepCode::InvalidGeometry)?;
    Ok(b)
}
fn coedge(b: &mut Brep, edge: Handle<Edge>, sense: Sense) -> Handle<Coedge> {
    b.coedges.insert(Coedge {
        edge,
        sense,
        pcurve: PcurveGeometry::Line {
            start: Point2::new(0., 0.),
            end: Point2::new(1., 0.),
        },
    })
}
fn single_edge_face(b: &mut Brep, edge: Handle<Edge>, surface: SurfaceGeometry) -> Handle<Face> {
    let use_handle = coedge(b, edge, Sense::Forward);
    let loop_handle = b.loops.insert(Loop {
        coedges: vec![use_handle],
    });
    b.faces.insert(Face {
        surface,
        loops: vec![loop_handle],
        sense: Sense::Forward,
    })
}
fn periodic_side_face(
    b: &mut Brep,
    bottom: Handle<Edge>,
    top: Handle<Edge>,
    seam: Handle<Edge>,
    surface: SurfaceGeometry,
) -> Handle<Face> {
    let uses = vec![
        coedge(b, bottom, Sense::Reverse),
        coedge(b, seam, Sense::Forward),
        coedge(b, top, Sense::Forward),
        coedge(b, seam, Sense::Reverse),
    ];
    let loop_handle = b.loops.insert(Loop { coedges: uses });
    b.faces.insert(Face {
        surface,
        loops: vec![loop_handle],
        sense: Sense::Forward,
    })
}
fn positive(value: f64) -> Result<(), BrepCode> {
    if value.is_finite() && value > 0. {
        Ok(())
    } else {
        Err(BrepCode::InvalidGeometry)
    }
}
fn curve_valid(curve: CurveGeometry) -> bool {
    match curve {
        CurveGeometry::Line { start, end } => start.is_finite() && end.is_finite() && start != end,
        CurveGeometry::Circle {
            center,
            axis,
            radial,
            radius,
        } => {
            center.is_finite()
                && axis.is_finite()
                && radial.is_finite()
                && radius.is_finite()
                && radius > 0.
                && axis.cross(radial).length() > 0.
        }
    }
}
fn surface_valid(surface: SurfaceGeometry) -> bool {
    match surface {
        SurfaceGeometry::Plane { origin, u, v } => origin.is_finite() && u.cross(v).length() > 0.,
        SurfaceGeometry::Cylinder {
            origin,
            axis,
            radial,
            radius,
        } => origin.is_finite() && axis.cross(radial).length() > 0. && radius > 0.,
        SurfaceGeometry::Cone {
            apex,
            axis,
            radial,
            half_angle,
        } => {
            apex.is_finite()
                && axis.cross(radial).length() > 0.
                && half_angle > 0.
                && half_angle < std::f64::consts::FRAC_PI_2
        }
        SurfaceGeometry::Sphere {
            center,
            axis,
            radial,
            radius,
        } => center.is_finite() && axis.cross(radial).length() > 0. && radius > 0.,
        SurfaceGeometry::Torus {
            center,
            axis,
            radial,
            major_radius,
            minor_radius,
        } => {
            center.is_finite()
                && axis.cross(radial).length() > 0.
                && major_radius > minor_radius
                && minor_radius > 0.
        }
    }
}
fn transform_curve(curve: &mut CurveGeometry, t: RigidTransform) {
    match curve {
        CurveGeometry::Line { start, end } => {
            *start = t.point(*start);
            *end = t.point(*end);
        }
        CurveGeometry::Circle {
            center,
            axis,
            radial,
            ..
        } => {
            *center = t.point(*center);
            *axis = t.vector(*axis);
            *radial = t.vector(*radial);
        }
    }
}
fn transform_surface(surface: &mut SurfaceGeometry, t: RigidTransform) {
    match surface {
        SurfaceGeometry::Plane { origin, u, v } => {
            *origin = t.point(*origin);
            *u = t.vector(*u);
            *v = t.vector(*v);
        }
        SurfaceGeometry::Cylinder {
            origin,
            axis,
            radial,
            ..
        } => {
            *origin = t.point(*origin);
            *axis = t.vector(*axis);
            *radial = t.vector(*radial);
        }
        SurfaceGeometry::Cone {
            apex, axis, radial, ..
        } => {
            *apex = t.point(*apex);
            *axis = t.vector(*axis);
            *radial = t.vector(*radial);
        }
        SurfaceGeometry::Sphere {
            center,
            axis,
            radial,
            ..
        }
        | SurfaceGeometry::Torus {
            center,
            axis,
            radial,
            ..
        } => {
            *center = t.point(*center);
            *axis = t.vector(*axis);
            *radial = t.vector(*radial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generations_reject_stale_handles() {
        let mut arena = Arena::default();
        let old = arena.insert(7);
        assert_eq!(arena.remove(old), Some(7));
        let new = arena.insert(9);
        assert_eq!(old.slot(), new.slot());
        assert_ne!(old.generation(), new.generation());
        assert!(arena.get(old).is_none());
        assert_eq!(arena.get(new), Some(&9));
    }
    #[test]
    fn all_analytic_primitives_validate_and_digest_deterministically() {
        let cases = [
            Primitive::Box {
                size: Vector3::new(2., 3., 4.),
            },
            Primitive::Cylinder {
                radius: 2.,
                height: 4.,
            },
            Primitive::Cone {
                radius: 2.,
                height: 4.,
            },
            Primitive::Sphere { radius: 2. },
            Primitive::Torus {
                major_radius: 3.,
                minor_radius: 1.,
            },
        ];
        for primitive in cases {
            let first = make_primitive(primitive).unwrap();
            let second = make_primitive(primitive).unwrap();
            assert_eq!(first.validate(), Ok(()));
            assert_eq!(first.semantic_digest(), second.semantic_digest());
        }
    }
    #[test]
    fn rigid_transform_preserves_topology_and_validation() {
        let body = make_primitive(Primitive::Cylinder {
            radius: 2.,
            height: 5.,
        })
        .unwrap();
        let transform = RigidTransform {
            rotation: [[0., -1., 0.], [1., 0., 0.], [0., 0., 1.]],
            translation: Vector3::new(4., 5., 6.),
        };
        let moved = body.transformed(transform).unwrap();
        assert_eq!(body.counts(), moved.counts());
        assert_eq!(moved.validate(), Ok(()));
        assert_ne!(body.semantic_digest(), moved.semantic_digest());
    }
    #[test]
    fn corrupt_edits_fail_transactionally() {
        let body = make_primitive(Primitive::Box {
            size: Vector3::new(1., 1., 1.),
        })
        .unwrap();
        let digest = body.semantic_digest();
        let result = body.edit(|candidate| {
            let edge = candidate.edges.iter().next().unwrap().0;
            candidate.edges.remove(edge);
        });
        assert!(result.is_err());
        assert_eq!(body.semantic_digest(), digest);
    }
}

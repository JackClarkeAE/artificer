//! Descriptions of faces and edges read off the exact B-rep.
//!
//! A selector answers "which face"; a description answers "what is it":
//! the carrier surface with its defining numbers, an exact area, a centre,
//! and the outward normal there. Reports, probes, and consoles all speak
//! this vocabulary, so a person and an agent reading the same session see
//! the same words for the same face.

use std::collections::BTreeMap;

use artificer_protocol::{
    EntityKind, EntityRef, KernelError, KernelErrorCode, KernelStage, Point3 as ProtocolPoint3,
    Vector3 as ProtocolVector3,
};
use serde::{Deserialize, Serialize};

use crate::topology::{Curve3, Point2, Point3, Surface, Vector3};
use crate::{
    NativeKernel, Snapshot, entity_ref, error, protocol_point, protocol_vector, validator,
};

/// The carrier surface of a face with the numbers that define it. Axes and
/// normals are unit vectors; lengths are model millimetres.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum FaceGeometry {
    /// A plane; the description's `normal` is its outward normal.
    Plane {
        origin: ProtocolPoint3,
    },
    Cylinder {
        origin: ProtocolPoint3,
        axis: ProtocolVector3,
        radius: f64,
    },
    Cone {
        apex: ProtocolPoint3,
        axis: ProtocolVector3,
        half_angle_degrees: f64,
    },
    Sphere {
        center: ProtocolPoint3,
        radius: f64,
    },
    Torus {
        origin: ProtocolPoint3,
        axis: ProtocolVector3,
        major_radius: f64,
        minor_radius: f64,
    },
}

impl FaceGeometry {
    /// The surface kind as one lowercase word: `plane`, `cylinder`, `cone`,
    /// `sphere`, or `torus`.
    #[must_use]
    pub const fn surface_kind(&self) -> &'static str {
        match self {
            Self::Plane { .. } => "plane",
            Self::Cylinder { .. } => "cylinder",
            Self::Cone { .. } => "cone",
            Self::Sphere { .. } => "sphere",
            Self::Torus { .. } => "torus",
        }
    }
}

/// One face: its carrier, exact area, a centre, and its outward normal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceDescription {
    pub face: EntityRef,
    #[serde(flatten)]
    pub geometry: FaceGeometry,
    /// Exact area from the analytic carrier and the loops' parameter
    /// integrals; no facets are involved.
    pub area: f64,
    /// The face's area centroid for a planar face; for a curved face, the
    /// point on the surface at the centre of its parameter domain.
    pub centre: ProtocolPoint3,
    /// The outward unit normal at `centre`.
    pub normal: ProtocolVector3,
    /// One for a simply bounded face, one more for every hole through it.
    pub loops: u32,
    /// The face in words, for a console or a person: the kind, which way a
    /// planar face faces, and where its centre is.
    pub summary: String,
}

/// The carrier curve of an edge with the numbers that define it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "curve", rename_all = "snake_case")]
pub enum EdgeGeometry {
    Line {
        start: ProtocolPoint3,
        end: ProtocolPoint3,
    },
    CircularArc {
        center: ProtocolPoint3,
        /// Unit normal of the circle's plane.
        normal: ProtocolVector3,
        radius: f64,
        start: ProtocolPoint3,
        end: ProtocolPoint3,
        /// The turn the edge covers; 360 for a full circle.
        sweep_degrees: f64,
    },
    EllipticalArc {
        center: ProtocolPoint3,
        normal: ProtocolVector3,
        major_radius: f64,
        minor_radius: f64,
        start: ProtocolPoint3,
        end: ProtocolPoint3,
        sweep_degrees: f64,
    },
}

impl EdgeGeometry {
    /// The curve kind as one lowercase word: `line`, `circle`, or `ellipse`.
    #[must_use]
    pub const fn curve_kind(&self) -> &'static str {
        match self {
            Self::Line { .. } => "line",
            Self::CircularArc { .. } => "circle",
            Self::EllipticalArc { .. } => "ellipse",
        }
    }
}

/// One edge: its carrier, exact length, and midpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeDescription {
    pub edge: EntityRef,
    #[serde(flatten)]
    pub geometry: EdgeGeometry,
    /// Exact arc length; an elliptic integral for an elliptical edge.
    pub length: f64,
    /// The point halfway along the edge by parameter.
    pub midpoint: ProtocolPoint3,
    pub summary: String,
}

/// How many faces of each carrier kind a body has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCounts {
    pub planes: u64,
    pub cylinders: u64,
    pub cones: u64,
    pub spheres: u64,
    pub tori: u64,
}

impl SurfaceCounts {
    #[must_use]
    pub const fn total(self) -> u64 {
        self.planes + self.cylinders + self.cones + self.spheres + self.tori
    }
}

impl NativeKernel {
    /// Every face of the snapshot in topology order.
    #[must_use]
    pub fn faces(snapshot: &Snapshot) -> Vec<EntityRef> {
        snapshot
            .topology
            .faces
            .iter()
            .map(|face| entity_ref(snapshot.id, face.id.get(), EntityKind::Face))
            .collect()
    }

    /// Every edge of the snapshot in topology order.
    #[must_use]
    pub fn edges(snapshot: &Snapshot) -> Vec<EntityRef> {
        snapshot
            .topology
            .edges
            .iter()
            .map(|edge| entity_ref(snapshot.id, edge.id.get(), EntityKind::Edge))
            .collect()
    }

    /// How many faces of each carrier kind the snapshot has.
    #[must_use]
    pub fn surface_counts(snapshot: &Snapshot) -> SurfaceCounts {
        let mut counts = SurfaceCounts::default();
        for face in &snapshot.topology.faces {
            match face.value.surface {
                Surface::Plane(_) => counts.planes += 1,
                Surface::Cylinder(_) => counts.cylinders += 1,
                Surface::Cone(_) => counts.cones += 1,
                Surface::Sphere(_) => counts.spheres += 1,
                Surface::Torus(_) => counts.tori += 1,
            }
        }
        counts
    }

    /// Whether every face of the snapshot is planar and every edge straight,
    /// in which case display facets reproduce the body exactly.
    #[must_use]
    pub fn is_polyhedral(snapshot: &Snapshot) -> bool {
        snapshot
            .topology
            .faces
            .iter()
            .all(|face| matches!(face.value.surface, Surface::Plane(_)))
            && snapshot
                .topology
                .edges
                .iter()
                .all(|edge| matches!(edge.value.curve, Curve3::Line { .. }))
    }

    /// Whether one face is planar.
    pub fn face_is_planar(snapshot: &Snapshot, face: EntityRef) -> Result<bool, KernelError> {
        let record = crate::resolve_measure_entity(snapshot, face, EntityKind::Face, "face")?;
        Ok(matches!(
            snapshot.topology.faces[record].value.surface,
            Surface::Plane(_)
        ))
    }

    /// Describes one face of the snapshot from its exact carrier.
    pub fn describe_face(
        snapshot: &Snapshot,
        face: EntityRef,
    ) -> Result<FaceDescription, KernelError> {
        let record = crate::resolve_measure_entity(snapshot, face, EntityKind::Face, "face")?;
        let face_record = &snapshot.topology.faces[record].value;
        let surface = face_record.surface;
        let indeterminate = |what: &str| {
            error(
                KernelErrorCode::NumericallyIndeterminate,
                KernelStage::Preflight,
                snapshot.id,
                format!("the requested face's {what} could not be evaluated"),
                Vec::new(),
            )
        };
        let geometry = face_geometry(surface).ok_or_else(|| indeterminate("carrier"))?;
        let area = Self::face_area(snapshot, face)?;
        let (parameter_area, moment) =
            validator::face_parameter_area_and_moment(&snapshot.topology, face_record)
                .filter(|(area, _)| area.abs() > f64::EPSILON)
                .ok_or_else(|| indeterminate("centre"))?;
        let centre = surface.evaluate(Point2::new(
            moment.x / parameter_area,
            moment.y / parameter_area,
        ));
        let normal = surface
            .outward_normal_at(centre)
            .ok_or_else(|| indeterminate("normal"))?;
        let loops = 1 + u32::try_from(face_record.inner_loops.len()).unwrap_or(u32::MAX - 1);
        let summary = face_summary(&geometry, normal, centre, loops);
        Ok(FaceDescription {
            face,
            geometry,
            area,
            centre: protocol_point(centre),
            normal: protocol_vector(normal),
            loops,
            summary,
        })
    }

    /// Describes every face of the snapshot, keyed by entity id, skipping
    /// any whose carrier cannot be evaluated.
    #[must_use]
    pub fn describe_faces(snapshot: &Snapshot) -> BTreeMap<u64, FaceDescription> {
        Self::faces(snapshot)
            .into_iter()
            .filter_map(|face| {
                Self::describe_face(snapshot, face)
                    .ok()
                    .map(|description| (face.entity.0, description))
            })
            .collect()
    }

    /// Describes one edge of the snapshot from its exact carrier.
    pub fn describe_edge(
        snapshot: &Snapshot,
        edge: EntityRef,
    ) -> Result<EdgeDescription, KernelError> {
        let record = crate::resolve_measure_entity(snapshot, edge, EntityKind::Edge, "edge")?;
        let edge_record = snapshot.topology.edges[record].value;
        let range = edge_record.parameter_range;
        let [start, end] = edge_record.endpoints();
        let sweep_degrees = (range.end - range.start).abs().to_degrees();
        let geometry = match edge_record.curve {
            Curve3::Line { .. } => EdgeGeometry::Line {
                start: protocol_point(start),
                end: protocol_point(end),
            },
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            } => EdgeGeometry::CircularArc {
                center: protocol_point(center),
                normal: protocol_vector(unit(u.cross(v)).unwrap_or(Vector3::new(0.0, 0.0, 1.0))),
                radius,
                start: protocol_point(start),
                end: protocol_point(end),
                sweep_degrees,
            },
            Curve3::Ellipse {
                center,
                u,
                v,
                major_radius,
                minor_radius,
            } => EdgeGeometry::EllipticalArc {
                center: protocol_point(center),
                normal: protocol_vector(unit(u.cross(v)).unwrap_or(Vector3::new(0.0, 0.0, 1.0))),
                major_radius,
                minor_radius,
                start: protocol_point(start),
                end: protocol_point(end),
                sweep_degrees,
            },
        };
        let midpoint = edge_record.curve.evaluate((range.start + range.end) * 0.5);
        let length = edge_record.length();
        let summary = match geometry {
            EdgeGeometry::Line { .. } => format!(
                "straight edge, length {}, from {} to {}",
                number(length),
                point_text(start),
                point_text(end)
            ),
            EdgeGeometry::CircularArc { radius, .. } if sweep_degrees >= 359.999 => {
                format!(
                    "full circle, radius {}, centre {}",
                    number(radius),
                    point_text(midpoint)
                )
            }
            EdgeGeometry::CircularArc { radius, .. } => format!(
                "circular arc, radius {}, {} degrees, midpoint {}",
                number(radius),
                number(sweep_degrees),
                point_text(midpoint)
            ),
            EdgeGeometry::EllipticalArc {
                major_radius,
                minor_radius,
                ..
            } => format!(
                "elliptical arc, radii {} and {}, {} degrees, midpoint {}",
                number(major_radius),
                number(minor_radius),
                number(sweep_degrees),
                point_text(midpoint)
            ),
        };
        Ok(EdgeDescription {
            edge,
            geometry,
            length,
            midpoint: protocol_point(midpoint),
            summary,
        })
    }
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > f64::EPSILON).then(|| vector / length)
}

fn face_geometry(surface: Surface) -> Option<FaceGeometry> {
    Some(match surface {
        Surface::Plane(plane) => FaceGeometry::Plane {
            origin: protocol_point(plane.origin),
        },
        Surface::Cylinder(cylinder) => FaceGeometry::Cylinder {
            origin: protocol_point(cylinder.origin),
            axis: protocol_vector(unit(cylinder.axis)?),
            radius: cylinder.radius.abs(),
        },
        Surface::Cone(cone) => {
            let axis_length = cone.axis.length();
            let apex = if cone.slope.abs() > f64::EPSILON {
                cone.origin + cone.axis * (-cone.base_radius / cone.slope)
            } else {
                cone.origin
            };
            FaceGeometry::Cone {
                apex: protocol_point(apex),
                axis: protocol_vector(unit(cone.axis)?),
                half_angle_degrees: (cone.slope.abs() / axis_length).atan().to_degrees(),
            }
        }
        Surface::Sphere(sphere) => FaceGeometry::Sphere {
            center: protocol_point(sphere.origin),
            radius: sphere.radius.abs(),
        },
        Surface::Torus(torus) => FaceGeometry::Torus {
            origin: protocol_point(torus.origin),
            axis: protocol_vector(unit(torus.axis)?),
            major_radius: torus.major_radius.abs(),
            minor_radius: torus.minor_radius.abs(),
        },
    })
}

/// The face in words. Planar faces say which way they face in the words a
/// person uses at a viewport: up, down, +X, -X, +Y, -Y, or the normal.
fn face_summary(geometry: &FaceGeometry, normal: Vector3, centre: Point3, loops: u32) -> String {
    let holes = match loops {
        1 => String::new(),
        2 => ", one hole".to_owned(),
        n => format!(", {} holes", n - 1),
    };
    let kind = match geometry {
        FaceGeometry::Plane { .. } => {
            let axes = [
                ("+X", Vector3::new(1.0, 0.0, 0.0)),
                ("-X", Vector3::new(-1.0, 0.0, 0.0)),
                ("+Y", Vector3::new(0.0, 1.0, 0.0)),
                ("-Y", Vector3::new(0.0, -1.0, 0.0)),
                ("up", Vector3::new(0.0, 0.0, 1.0)),
                ("down", Vector3::new(0.0, 0.0, -1.0)),
            ];
            axes.iter()
                .find(|(_, axis)| normal.dot(*axis) > 0.999)
                .map_or_else(
                    || {
                        format!(
                            "planar, normal ({:.2}, {:.2}, {:.2})",
                            normal.x, normal.y, normal.z
                        )
                    },
                    |(word, _)| format!("planar, facing {word}"),
                )
        }
        FaceGeometry::Cylinder { radius, .. } => {
            format!("cylindrical, radius {}", number(*radius))
        }
        FaceGeometry::Cone {
            half_angle_degrees, ..
        } => format!(
            "conical, half angle {} degrees",
            number(*half_angle_degrees)
        ),
        FaceGeometry::Sphere { radius, .. } => format!("spherical, radius {}", number(*radius)),
        FaceGeometry::Torus {
            major_radius,
            minor_radius,
            ..
        } => format!(
            "toroidal, radii {} and {}",
            number(*major_radius),
            number(*minor_radius)
        ),
    };
    format!("{kind}{holes}, centre {}", point_text(centre))
}

fn point_text(point: Point3) -> String {
    format!(
        "({:.1}, {:.1}, {:.1})",
        tidy(point.x),
        tidy(point.y),
        tidy(point.z)
    )
}

/// Keeps `-0.0` from printing as `-0.0`.
fn tidy(value: f64) -> f64 {
    if value.abs() < 0.05 { 0.0 } else { value }
}

fn number(value: f64) -> String {
    if (value - value.round()).abs() < 1.0e-9 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.3}")
    }
}

//! Construction axes: what an axis is made from, and where that puts it.
//!
//! An axis is a line a later feature can turn about (ADR 0055), a feature in
//! the history as a construction plane is (ADR 0048). Its recipe names what
//! it is made from — an origin axis, a straight edge, the axis of a curved
//! face, or the line where two planes meet — and keeps the line it last
//! resolved to only as a cache for the moments that base cannot be found.
//! What a face or an edge *is* at the moment of replay is asked of the kernel
//! by the caller, through [`DatumAxisResolver`].

use artificer_protocol::{EntityKind, Point3, Vector3};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::datum::{
    DatumFaceGeometry, DatumFaceRef, DatumPlaneError, ORIGIN_DATUM_HALF_EXTENT, OriginPlane,
    ResolvedDatumPlane, check_reference, cross, dot, length, normal, scale, sub, translate, unit,
};
use crate::persistent::PersistentRef;
use crate::revolve::OriginAxis;
use crate::{BodyId, FeatureId};

/// Schema written for newly created construction-axis recipes.
pub const CURRENT_DATUM_AXIS_RECIPE_VERSION: u32 = 1;

const fn current_datum_axis_recipe_version() -> u32 {
    CURRENT_DATUM_AXIS_RECIPE_VERSION
}

/// How far an origin axis is drawn each way from the origin, in
/// millimetres: as far as an origin plane's card reaches.
pub const ORIGIN_AXIS_HALF_LENGTH: f64 = ORIGIN_DATUM_HALF_EXTENT;

/// The shortest an axis is drawn each way, so an axis on a tiny edge can
/// still be seen.
const MIN_HALF_LENGTH: f64 = 0.5;

/// One of the planes an axis can be the meeting of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DatumAxisPlane {
    /// One of the three origin planes.
    Origin { plane: OriginPlane },
    /// A planar face.
    Face(DatumFaceRef),
    /// A construction plane, named by its feature.
    Plane { plane: FeatureId },
}

impl DatumAxisPlane {
    fn describe(&self) -> String {
        match self {
            Self::Origin { plane } => format!("the {}", plane.label()),
            Self::Face(_) => "a face".to_owned(),
            Self::Plane { plane } => format!("construction plane {plane}"),
        }
    }
}

/// What a construction axis is made from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DatumAxisBase {
    /// One of the document's origin axes.
    Origin { axis: OriginAxis },
    /// Along a straight edge, from its start towards its end.
    Edge { body: BodyId, edge: PersistentRef },
    /// The axis of a cylindrical, conical, toroidal or spherical face.
    Face(DatumFaceRef),
    /// The line where two planes meet.
    Planes {
        first: DatumAxisPlane,
        second: DatumAxisPlane,
    },
    /// A line that names nothing.
    Fixed { origin: Point3, direction: Vector3 },
}

impl DatumAxisBase {
    /// The bodies whose faces or edges this base reads, in a stable order.
    #[must_use]
    pub fn bodies(&self) -> Vec<BodyId> {
        let mut bodies = match self {
            Self::Edge { body, .. } => vec![*body],
            Self::Face(face) => vec![face.body],
            Self::Planes { first, second } => [first, second]
                .into_iter()
                .filter_map(|plane| match plane {
                    DatumAxisPlane::Face(face) => Some(face.body),
                    DatumAxisPlane::Origin { .. } | DatumAxisPlane::Plane { .. } => None,
                })
                .collect(),
            Self::Origin { .. } | Self::Fixed { .. } => Vec::new(),
        };
        bodies.dedup();
        bodies
    }

    /// The construction planes this base reads.
    #[must_use]
    pub fn planes(&self) -> Vec<FeatureId> {
        match self {
            Self::Planes { first, second } => [first, second]
                .into_iter()
                .filter_map(|plane| match plane {
                    DatumAxisPlane::Plane { plane } => Some(*plane),
                    DatumAxisPlane::Origin { .. } | DatumAxisPlane::Face(_) => None,
                })
                .collect(),
            Self::Origin { .. } | Self::Edge { .. } | Self::Face(_) | Self::Fixed { .. } => {
                Vec::new()
            }
        }
    }

    /// A short phrase for what the axis is made from.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Origin { axis } => format!("along the origin {}", axis.label()),
            Self::Edge { .. } => "along an edge".to_owned(),
            Self::Face(_) => "through a curved face's axis".to_owned(),
            Self::Planes { first, second } => {
                format!("where {} meets {}", first.describe(), second.describe())
            }
            Self::Fixed { .. } => "at a fixed position".to_owned(),
        }
    }
}

/// A construction axis's replay recipe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatumAxisRecipe {
    #[serde(default = "current_datum_axis_recipe_version")]
    pub version: u32,
    pub base: DatumAxisBase,
    /// The axis runs the other way. Its line does not change.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip: bool,
    /// The line the recipe last resolved to. A cache, not a definition: it
    /// stands only while the base cannot be resolved.
    pub origin: Point3,
    pub direction: Vector3,
    /// How far the axis is drawn each way from `origin`.
    pub half_length: f64,
    #[serde(default = "default_visible", skip_serializing_if = "is_true")]
    pub visible: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_true(value: &bool) -> bool {
    *value
}

const fn default_visible() -> bool {
    true
}

/// An axis as resolved: a point on it, its unit direction, and how far it
/// is drawn each way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedDatumAxis {
    pub origin: Point3,
    pub direction: Vector3,
    pub half_length: f64,
}

/// What resolving an axis needs to be told about the model.
pub trait DatumAxisResolver {
    /// A straight edge's two ends.
    fn edge(&self, body: BodyId, edge: &PersistentRef) -> Result<[Point3; 2], DatumAxisError>;
    /// A curved face's axis, drawn along the face.
    fn face_axis(&self, face: &DatumFaceRef) -> Result<ResolvedDatumAxis, DatumAxisError>;
    /// A planar face.
    fn face_plane(&self, face: &DatumFaceRef) -> Result<DatumFaceGeometry, DatumAxisError>;
    /// A construction plane as it now stands.
    fn plane(&self, plane: FeatureId) -> Result<ResolvedDatumPlane, DatumAxisError>;
}

/// Why an axis recipe cannot be kept or placed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DatumAxisError {
    #[error(
        "unsupported construction-axis recipe version {found}; this build supports {CURRENT_DATUM_AXIS_RECIPE_VERSION}"
    )]
    UnsupportedVersion { found: u32 },
    #[error("the axis has no direction")]
    DegenerateLine,
    #[error("the axis's drawn length must be finite and positive")]
    InvalidExtent,
    #[error("an axis along an edge needs an edge reference")]
    EdgeReferenceRequired,
    #[error("an axis through a face needs a face reference")]
    FaceReferenceRequired,
    #[error("a persistent reference is malformed")]
    InvalidReference,
    #[error("the edge the axis runs along is no longer in the model")]
    EdgeMissing,
    #[error("the edge the axis runs along is not straight")]
    EdgeNotStraight,
    #[error("the face the axis is taken from is no longer in the model")]
    FaceMissing,
    #[error("the face the axis is taken from matches more than one face")]
    FaceAmbiguous,
    #[error("the face has no axis: it is not a cylinder, a cone, a torus or a sphere")]
    FaceHasNoAxis,
    #[error("a face the axis is the meeting of is not planar")]
    FaceNotPlanar,
    #[error("the two planes are parallel, so they do not meet in a line")]
    PlanesParallel,
    #[error("construction plane {0} is not in the history")]
    UnknownPlane(FeatureId),
    #[error("construction plane {0} cannot be placed")]
    PlaneUnavailable(FeatureId),
}

impl From<DatumPlaneError> for DatumAxisError {
    fn from(error: DatumPlaneError) -> Self {
        match error {
            DatumPlaneError::EdgeReferenceRequired => Self::EdgeReferenceRequired,
            DatumPlaneError::FaceReferenceRequired => Self::FaceReferenceRequired,
            _ => Self::InvalidReference,
        }
    }
}

impl DatumAxisRecipe {
    /// A recipe for `base`, standing where it was just resolved to.
    #[must_use]
    pub const fn new(base: DatumAxisBase, resolved: ResolvedDatumAxis) -> Self {
        Self {
            version: CURRENT_DATUM_AXIS_RECIPE_VERSION,
            base,
            flip: false,
            origin: resolved.origin,
            direction: resolved.direction,
            half_length: resolved.half_length,
            visible: true,
        }
    }

    /// Structural checks that need no model.
    pub fn validate(&self) -> Result<(), DatumAxisError> {
        if self.version != CURRENT_DATUM_AXIS_RECIPE_VERSION {
            return Err(DatumAxisError::UnsupportedVersion {
                found: self.version,
            });
        }
        if !self.origin.is_finite() || unit(self.direction).is_none() {
            return Err(DatumAxisError::DegenerateLine);
        }
        if !(self.half_length.is_finite() && self.half_length > 0.0) {
            return Err(DatumAxisError::InvalidExtent);
        }
        match &self.base {
            DatumAxisBase::Edge { edge, .. } => check_reference(edge, EntityKind::Edge)?,
            DatumAxisBase::Face(face) => check_reference(&face.face, EntityKind::Face)?,
            DatumAxisBase::Planes { first, second } => {
                for plane in [first, second] {
                    if let DatumAxisPlane::Face(face) = plane {
                        check_reference(&face.face, EntityKind::Face)?;
                    }
                }
            }
            DatumAxisBase::Fixed { origin, direction } => {
                if !origin.is_finite() || unit(*direction).is_none() {
                    return Err(DatumAxisError::DegenerateLine);
                }
            }
            DatumAxisBase::Origin { .. } => {}
        }
        Ok(())
    }

    /// Where the axis is, found from its base as the model now stands.
    pub fn resolve(
        &self,
        resolver: &dyn DatumAxisResolver,
    ) -> Result<ResolvedDatumAxis, DatumAxisError> {
        self.validate()?;
        let line = base_line(&self.base, self.half_length, resolver)?;
        Ok(if self.flip { flipped(line) } else { line })
    }

    /// The line the recipe last resolved to.
    #[must_use]
    pub const fn cached(&self) -> ResolvedDatumAxis {
        ResolvedDatumAxis {
            origin: self.origin,
            direction: self.direction,
            half_length: self.half_length,
        }
    }
}

/// The axis its base makes, before any flip.
fn base_line(
    base: &DatumAxisBase,
    cached_half_length: f64,
    resolver: &dyn DatumAxisResolver,
) -> Result<ResolvedDatumAxis, DatumAxisError> {
    match base {
        DatumAxisBase::Origin { axis } => Ok(ResolvedDatumAxis {
            origin: Point3::new(0.0, 0.0, 0.0),
            direction: axis.direction(),
            half_length: ORIGIN_AXIS_HALF_LENGTH,
        }),
        DatumAxisBase::Edge { body, edge } => {
            let [start, end] = resolver.edge(*body, edge)?;
            let along = sub(end, start);
            let direction = unit(along).ok_or(DatumAxisError::EdgeNotStraight)?;
            Ok(ResolvedDatumAxis {
                origin: translate(start, scale(along, 0.5)),
                direction,
                half_length: (length(along) * 0.5).max(MIN_HALF_LENGTH),
            })
        }
        DatumAxisBase::Face(face) => {
            let axis = resolver.face_axis(face)?;
            Ok(ResolvedDatumAxis {
                direction: unit(axis.direction).ok_or(DatumAxisError::FaceHasNoAxis)?,
                half_length: axis.half_length.max(MIN_HALF_LENGTH),
                ..axis
            })
        }
        DatumAxisBase::Planes { first, second } => {
            let place = |plane: &DatumAxisPlane| -> Result<(Point3, Vector3, f64), DatumAxisError> {
                let (frame, half_extent) = match plane {
                    DatumAxisPlane::Origin { plane } => {
                        (plane.frame(), [ORIGIN_DATUM_HALF_EXTENT; 2])
                    }
                    DatumAxisPlane::Face(face) => {
                        let geometry = resolver.face_plane(face)?;
                        (geometry.frame, geometry.half_extent)
                    }
                    DatumAxisPlane::Plane { plane } => {
                        let resolved = resolver.plane(*plane)?;
                        (resolved.frame, resolved.half_extent)
                    }
                };
                let normal = unit(normal(frame)).ok_or(DatumAxisError::FaceNotPlanar)?;
                Ok((frame.origin, normal, half_extent[0].max(half_extent[1])))
            };
            let (first_origin, first_normal, first_reach) = place(first)?;
            let (second_origin, second_normal, second_reach) = place(second)?;
            planes_meet(
                (first_origin, first_normal),
                (second_origin, second_normal),
                first_reach.max(second_reach),
            )
        }
        DatumAxisBase::Fixed { origin, direction } => Ok(ResolvedDatumAxis {
            origin: *origin,
            direction: unit(*direction).ok_or(DatumAxisError::DegenerateLine)?,
            half_length: cached_half_length,
        }),
    }
}

/// The line two planes meet in, centred between the planes' own origins and
/// drawn `reach` each way. Each plane is a point on it and its unit normal.
pub fn planes_meet(
    (first_origin, first_normal): (Point3, Vector3),
    (second_origin, second_normal): (Point3, Vector3),
    reach: f64,
) -> Result<ResolvedDatumAxis, DatumAxisError> {
    let along = cross(first_normal, second_normal);
    let squared = dot(along, along);
    // Planes within about a thousandth of a degree of each other meet so far
    // away, or so imprecisely, that the line means nothing.
    if !(squared.is_finite() && squared > 1.0e-10) {
        return Err(DatumAxisError::PlanesParallel);
    }
    let height =
        |origin: Point3, normal: Vector3| dot(Vector3::new(origin.x, origin.y, origin.z), normal);
    let (first_height, second_height) = (
        height(first_origin, first_normal),
        height(second_origin, second_normal),
    );
    // The point on both planes nearest the world origin.
    let on_both = scale(
        crate::datum::add(
            scale(cross(second_normal, along), first_height),
            scale(cross(along, first_normal), second_height),
        ),
        1.0 / squared,
    );
    let direction = scale(along, 1.0 / squared.sqrt());
    let base = Point3::new(on_both.x, on_both.y, on_both.z);
    // Centre it where the two planes are drawn.
    let middle = Point3::new(
        f64::midpoint(first_origin.x, second_origin.x),
        f64::midpoint(first_origin.y, second_origin.y),
        f64::midpoint(first_origin.z, second_origin.z),
    );
    let along_line = dot(sub(middle, base), direction);
    Ok(ResolvedDatumAxis {
        origin: translate(base, scale(direction, along_line)),
        direction,
        half_length: reach.max(MIN_HALF_LENGTH),
    })
}

const fn flipped(line: ResolvedDatumAxis) -> ResolvedDatumAxis {
    ResolvedDatumAxis {
        direction: scale(line.direction, -1.0),
        ..line
    }
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{OperationRole, PlanarFrame3};

    use super::*;

    /// Answers every question with one fixed model: an edge from (0, 0, 0)
    /// to (0, 0, 10), a cylinder about the line x = 5, y = 0, and the XY
    /// plane lifted to z = 3 as a planar face.
    struct Model;

    impl DatumAxisResolver for Model {
        fn edge(&self, _: BodyId, _: &PersistentRef) -> Result<[Point3; 2], DatumAxisError> {
            Ok([Point3::new(0.0, 0.0, 0.0), Point3::new(0.0, 0.0, 10.0)])
        }

        fn face_axis(&self, _: &DatumFaceRef) -> Result<ResolvedDatumAxis, DatumAxisError> {
            Ok(ResolvedDatumAxis {
                origin: Point3::new(5.0, 0.0, 2.0),
                direction: Vector3::new(0.0, 0.0, 2.0),
                half_length: 4.0,
            })
        }

        fn face_plane(&self, _: &DatumFaceRef) -> Result<DatumFaceGeometry, DatumAxisError> {
            Ok(DatumFaceGeometry {
                frame: PlanarFrame3::new(
                    Point3::new(1.0, 1.0, 3.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                half_extent: [6.0, 2.0],
            })
        }

        fn plane(&self, plane: FeatureId) -> Result<ResolvedDatumPlane, DatumAxisError> {
            Err(DatumAxisError::UnknownPlane(plane))
        }
    }

    fn face() -> DatumFaceRef {
        DatumFaceRef {
            body: BodyId::from_allocated(1),
            face: PersistentRef::new(
                FeatureId::from_allocated(1),
                OperationRole::new("extrusion.side", Some(0)),
                EntityKind::Face,
            ),
        }
    }

    fn resolve(base: DatumAxisBase) -> Result<ResolvedDatumAxis, DatumAxisError> {
        DatumAxisRecipe::new(
            base,
            ResolvedDatumAxis {
                origin: Point3::new(0.0, 0.0, 0.0),
                direction: Vector3::new(1.0, 0.0, 0.0),
                half_length: 1.0,
            },
        )
        .resolve(&Model)
    }

    #[test]
    fn an_axis_runs_along_an_edge_through_a_face_and_where_planes_meet() {
        let along_z = Vector3::new(0.0, 0.0, 1.0);
        let edge = resolve(DatumAxisBase::Edge {
            body: BodyId::from_allocated(1),
            edge: PersistentRef::new(
                FeatureId::from_allocated(1),
                OperationRole::new("extrusion.side", Some(0)),
                EntityKind::Edge,
            ),
        })
        .expect("an edge axis");
        assert_eq!(edge.origin, Point3::new(0.0, 0.0, 5.0));
        assert_eq!(edge.direction, along_z);
        assert_eq!(edge.half_length, 5.0);

        let through = resolve(DatumAxisBase::Face(face())).expect("a face axis");
        assert_eq!(through.origin, Point3::new(5.0, 0.0, 2.0));
        assert_eq!(through.direction, along_z, "the direction comes back unit");

        // The XZ origin plane (y = 0) meets the face lifted to z = 3 along
        // the line y = 0, z = 3, centred between their origins.
        let meet = resolve(DatumAxisBase::Planes {
            first: DatumAxisPlane::Origin {
                plane: OriginPlane::Xz,
            },
            second: DatumAxisPlane::Face(face()),
        })
        .expect("the planes meet");
        assert!(meet.direction.y.abs() < 1.0e-12 && meet.direction.z.abs() < 1.0e-12);
        assert!((meet.direction.x.abs() - 1.0).abs() < 1.0e-12);
        assert!(meet.origin.y.abs() < 1.0e-12 && (meet.origin.z - 3.0).abs() < 1.0e-12);
        assert!((meet.origin.x - 0.5).abs() < 1.0e-12);
        assert_eq!(meet.half_length, ORIGIN_DATUM_HALF_EXTENT);

        let origin = resolve(DatumAxisBase::Origin {
            axis: OriginAxis::Y,
        })
        .expect("origin");
        assert_eq!(origin.direction, Vector3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn a_flipped_axis_runs_the_other_way_along_the_same_line() {
        let mut recipe = DatumAxisRecipe::new(
            DatumAxisBase::Origin {
                axis: OriginAxis::Z,
            },
            ResolvedDatumAxis {
                origin: Point3::new(0.0, 0.0, 0.0),
                direction: Vector3::new(0.0, 0.0, 1.0),
                half_length: 25.0,
            },
        );
        recipe.flip = true;
        let flipped = recipe.resolve(&Model).expect("resolves");
        assert_eq!(flipped.direction, Vector3::new(0.0, 0.0, -1.0));
        assert_eq!(flipped.origin, Point3::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn parallel_planes_and_malformed_recipes_are_refused_by_name() {
        assert_eq!(
            resolve(DatumAxisBase::Planes {
                first: DatumAxisPlane::Origin {
                    plane: OriginPlane::Xy,
                },
                second: DatumAxisPlane::Face(face()),
            }),
            Err(DatumAxisError::PlanesParallel)
        );
        assert_eq!(
            resolve(DatumAxisBase::Planes {
                first: DatumAxisPlane::Plane {
                    plane: FeatureId::from_allocated(7),
                },
                second: DatumAxisPlane::Face(face()),
            }),
            Err(DatumAxisError::UnknownPlane(FeatureId::from_allocated(7)))
        );
        assert_eq!(
            resolve(DatumAxisBase::Fixed {
                origin: Point3::new(0.0, 0.0, 0.0),
                direction: Vector3::new(0.0, 0.0, 0.0),
            }),
            Err(DatumAxisError::DegenerateLine)
        );
        let mut edge_as_face = DatumAxisRecipe::new(
            DatumAxisBase::Edge {
                body: BodyId::from_allocated(1),
                edge: face().face,
            },
            ResolvedDatumAxis {
                origin: Point3::new(0.0, 0.0, 0.0),
                direction: Vector3::new(1.0, 0.0, 0.0),
                half_length: 1.0,
            },
        );
        assert_eq!(
            edge_as_face.validate(),
            Err(DatumAxisError::EdgeReferenceRequired)
        );
        edge_as_face.base = DatumAxisBase::Origin {
            axis: OriginAxis::X,
        };
        edge_as_face.half_length = 0.0;
        assert_eq!(edge_as_face.validate(), Err(DatumAxisError::InvalidExtent));
    }

    #[test]
    fn a_recipe_round_trips_through_json_and_omits_its_defaults() {
        let recipe = DatumAxisRecipe::new(
            DatumAxisBase::Planes {
                first: DatumAxisPlane::Origin {
                    plane: OriginPlane::Xz,
                },
                second: DatumAxisPlane::Plane {
                    plane: FeatureId::from_allocated(3),
                },
            },
            ResolvedDatumAxis {
                origin: Point3::new(0.0, 0.0, 0.0),
                direction: Vector3::new(1.0, 0.0, 0.0),
                half_length: 25.0,
            },
        );
        let json = serde_json::to_value(&recipe).expect("encodes");
        assert_eq!(json["base"]["kind"], "planes");
        assert_eq!(json["base"]["first"]["kind"], "origin");
        assert!(json.get("flip").is_none() && json.get("visible").is_none());
        let back: DatumAxisRecipe = serde_json::from_value(json).expect("decodes");
        assert_eq!(back, recipe);
        assert_eq!(recipe.base.planes(), vec![FeatureId::from_allocated(3)]);
        assert!(recipe.base.bodies().is_empty());
    }
}

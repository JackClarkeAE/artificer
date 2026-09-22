//! Construction planes: what a plane is made from, and where that puts it.
//!
//! A plane's recipe is its definition (ADR 0048). The frame it resolves to is
//! derived, and the recipe keeps the last one only as a cache for the moments
//! its base cannot be resolved. This module owns the arithmetic that turns a
//! base into a plane; what a face or an edge *is* at the moment of replay is
//! asked of the kernel by the caller, through [`DatumPlaneResolver`].

use artificer_protocol::{EntityKind, PlanarFrame3, Point3, Vector3};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::persistent::{
    CURRENT_PERSISTENT_REF_VERSION, MAX_PERSISTENT_LINEAGE_DEPTH, PersistentRef,
};
use crate::{BodyId, FeatureId};

/// Schema written for newly-created construction-plane recipes.
pub const CURRENT_DATUM_PLANE_RECIPE_VERSION: u32 = 1;

const fn current_datum_plane_recipe_version() -> u32 {
    CURRENT_DATUM_PLANE_RECIPE_VERSION
}

/// Half the side of the card an origin plane is drawn as, in millimetres.
pub const ORIGIN_DATUM_HALF_EXTENT: f64 = 25.0;

/// The largest offset a recipe accepts. Beyond it a plane is not near
/// anything a model could hold; a typo should fail at the recipe.
pub const MAX_DATUM_OFFSET: f64 = 1.0e6;

/// The smallest card a plane is drawn as, so a plane on a tiny face can still
/// be seen and picked.
const MIN_HALF_EXTENT: f64 = 0.5;

/// One of the three document origin planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginPlane {
    Xy,
    Yz,
    Xz,
}

impl OriginPlane {
    /// The origin plane's frame. The normal is `u × v`: +Z for XY, +X for YZ
    /// and −Y for XZ, matching the sketch planes of the same names.
    #[must_use]
    pub const fn frame(self) -> PlanarFrame3 {
        let (u, v) = match self {
            Self::Xy => (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0)),
            Self::Yz => (Vector3::new(0.0, 1.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
            Self::Xz => (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
        };
        PlanarFrame3::new(Point3::new(0.0, 0.0, 0.0), u, v)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Xy => "XY Plane",
            Self::Yz => "YZ Plane",
            Self::Xz => "XZ Plane",
        }
    }
}

/// A planar face on a stable body branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatumFaceRef {
    pub body: BodyId,
    pub face: PersistentRef,
}

/// What a construction plane is made from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DatumPlaneBase {
    /// One of the three origin planes.
    Origin { plane: OriginPlane },
    /// A planar face; the plane's normal is the face's outward normal.
    Face(DatumFaceRef),
    /// Another construction plane, named by its feature.
    Plane { plane: FeatureId },
    /// Halfway between two parallel planar faces, facing as the first does.
    Midplane {
        first: DatumFaceRef,
        second: DatumFaceRef,
    },
    /// Through a straight edge, turned about it from a planar face that
    /// holds it. At no angle the plane lies on the face.
    Edge {
        body: BodyId,
        edge: PersistentRef,
        face: PersistentRef,
    },
    /// A frame that names nothing: a plane whose origin could not be
    /// recorded, which is what planes from version 6 files are.
    Fixed { frame: PlanarFrame3 },
}

impl DatumPlaneBase {
    /// The bodies whose faces or edges this base reads, in a stable order.
    #[must_use]
    pub fn bodies(&self) -> Vec<BodyId> {
        let mut bodies = match self {
            Self::Face(face) => vec![face.body],
            Self::Midplane { first, second } => vec![first.body, second.body],
            Self::Edge { body, .. } => vec![*body],
            Self::Origin { .. } | Self::Plane { .. } | Self::Fixed { .. } => Vec::new(),
        };
        bodies.dedup();
        bodies
    }

    /// The persistent references this base resolves.
    pub fn references(&self) -> impl Iterator<Item = &PersistentRef> {
        let references: Vec<&PersistentRef> = match self {
            Self::Face(face) => vec![&face.face],
            Self::Midplane { first, second } => vec![&first.face, &second.face],
            Self::Edge { edge, face, .. } => vec![edge, face],
            Self::Origin { .. } | Self::Plane { .. } | Self::Fixed { .. } => Vec::new(),
        };
        references.into_iter()
    }

    /// Whether this base can be turned about an edge.
    #[must_use]
    pub const fn takes_an_angle(&self) -> bool {
        matches!(self, Self::Edge { .. })
    }

    /// A short phrase for what the plane is built from.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Origin { plane } => format!("on the {}", plane.label()),
            Self::Face(_) => "on a face".to_owned(),
            Self::Plane { plane } => format!("on construction plane {plane}"),
            Self::Midplane { .. } => "halfway between two faces".to_owned(),
            Self::Edge { .. } => "through an edge".to_owned(),
            Self::Fixed { .. } => "at a fixed position".to_owned(),
        }
    }
}

/// A construction plane's replay recipe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatumPlaneRecipe {
    #[serde(default = "current_datum_plane_recipe_version")]
    pub version: u32,
    pub base: DatumPlaneBase,
    /// Distance along the base's normal, before any flip.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: f64,
    /// Turn about the edge, in degrees; only an edge base takes one.
    /// Positive lifts the plane off the face it starts on.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub angle_degrees: f64,
    /// The plane faces the other way. Its position does not change.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip: bool,
    /// The frame the recipe last resolved to. A cache, not a definition: it
    /// stands only while the base cannot be resolved.
    pub frame: PlanarFrame3,
    /// Half the card's size along `frame.u` and `frame.v`.
    pub half_extent: [f64; 2],
    #[serde(default = "default_visible", skip_serializing_if = "is_true")]
    pub visible: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &f64) -> bool {
    *value == 0.0
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

/// A plane as resolved: where it is and how large to draw it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedDatumPlane {
    pub frame: PlanarFrame3,
    pub half_extent: [f64; 2],
}

/// A planar face as the kernel reports it: a frame whose `u × v` is the
/// outward normal, its origin at the face's middle, and the card's size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DatumFaceGeometry {
    pub frame: PlanarFrame3,
    pub half_extent: [f64; 2],
}

/// A straight edge and the face it is turned from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DatumEdgeGeometry {
    pub start: Point3,
    pub end: Point3,
    /// The face's outward unit normal.
    pub face_normal: Vector3,
    /// The unit direction in the face's plane, square to the edge, that
    /// points from the edge into the face.
    pub into_face: Vector3,
}

/// What resolving a plane needs to be told about the model.
pub trait DatumPlaneResolver {
    fn face(&self, face: &DatumFaceRef) -> Result<DatumFaceGeometry, DatumPlaneError>;
    fn edge(
        &self,
        body: BodyId,
        edge: &PersistentRef,
        face: &PersistentRef,
    ) -> Result<DatumEdgeGeometry, DatumPlaneError>;
    fn plane(&self, plane: FeatureId) -> Result<ResolvedDatumPlane, DatumPlaneError>;
}

/// Why a plane recipe is invalid, or could not be resolved.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DatumPlaneError {
    #[error("unsupported construction-plane recipe version {found}")]
    UnsupportedVersion { found: u32 },
    #[error("the plane's offset must be a finite length no larger than {MAX_DATUM_OFFSET}")]
    InvalidOffset,
    #[error("the plane's angle must be finite and between -180 and 180 degrees")]
    InvalidAngle,
    #[error("only a plane through an edge can be turned")]
    AngleWithoutEdge,
    #[error("the plane's cached frame is degenerate")]
    DegenerateFrame,
    #[error("the plane's size must be positive and finite")]
    InvalidExtent,
    #[error("a plane's face reference must name a face")]
    FaceReferenceRequired,
    #[error("a plane's edge reference must name an edge")]
    EdgeReferenceRequired,
    #[error("a plane's persistent reference is malformed")]
    InvalidReference,
    #[error("a plane cannot be built on itself")]
    SelfReference,
    #[error("the face this plane is built on is no longer in the model")]
    FaceMissing,
    #[error("the face this plane is built on is no longer a single face")]
    FaceAmbiguous,
    #[error("the face this plane is built on is no longer flat")]
    FaceNotPlanar,
    #[error("the edge this plane is built on is no longer in the model")]
    EdgeMissing,
    #[error("the edge this plane is built on is not straight")]
    EdgeNotStraight,
    #[error("the edge this plane turns about does not lie on its face")]
    EdgeNotOnFace,
    #[error("the two faces of a midplane must be parallel")]
    FacesNotParallel,
    #[error("construction plane {0} is not in this document")]
    UnknownPlane(FeatureId),
    #[error("construction plane {0} could not be placed")]
    PlaneUnavailable(FeatureId),
}

impl DatumPlaneRecipe {
    /// A recipe with no offset, angle or flip, resolved to `resolved`.
    #[must_use]
    pub fn new(base: DatumPlaneBase, resolved: ResolvedDatumPlane) -> Self {
        Self {
            version: CURRENT_DATUM_PLANE_RECIPE_VERSION,
            base,
            offset: 0.0,
            angle_degrees: 0.0,
            flip: false,
            frame: resolved.frame,
            half_extent: resolved.half_extent,
            visible: true,
        }
    }

    /// Validates the recipe's own values; whether its base still resolves is
    /// a question for replay.
    pub fn validate(&self) -> Result<(), DatumPlaneError> {
        if self.version != CURRENT_DATUM_PLANE_RECIPE_VERSION {
            return Err(DatumPlaneError::UnsupportedVersion {
                found: self.version,
            });
        }
        if !self.offset.is_finite() || self.offset.abs() > MAX_DATUM_OFFSET {
            return Err(DatumPlaneError::InvalidOffset);
        }
        if !self.angle_degrees.is_finite() || self.angle_degrees.abs() > 180.0 {
            return Err(DatumPlaneError::InvalidAngle);
        }
        if self.angle_degrees != 0.0 && !self.base.takes_an_angle() {
            return Err(DatumPlaneError::AngleWithoutEdge);
        }
        if orthonormal_frame(self.frame).is_none() {
            return Err(DatumPlaneError::DegenerateFrame);
        }
        if !self
            .half_extent
            .iter()
            .all(|half| half.is_finite() && *half > 0.0)
        {
            return Err(DatumPlaneError::InvalidExtent);
        }
        match &self.base {
            DatumPlaneBase::Face(face) => check_reference(&face.face, EntityKind::Face)?,
            DatumPlaneBase::Midplane { first, second } => {
                check_reference(&first.face, EntityKind::Face)?;
                check_reference(&second.face, EntityKind::Face)?;
            }
            DatumPlaneBase::Edge { edge, face, .. } => {
                check_reference(edge, EntityKind::Edge)?;
                check_reference(face, EntityKind::Face)?;
            }
            DatumPlaneBase::Fixed { frame } => {
                if orthonormal_frame(*frame).is_none() {
                    return Err(DatumPlaneError::DegenerateFrame);
                }
            }
            DatumPlaneBase::Origin { .. } | DatumPlaneBase::Plane { .. } => {}
        }
        Ok(())
    }

    /// The frame this recipe's base stands on, before the offset and flip.
    pub fn base_plane(
        &self,
        resolver: &dyn DatumPlaneResolver,
    ) -> Result<ResolvedDatumPlane, DatumPlaneError> {
        match &self.base {
            DatumPlaneBase::Origin { plane } => Ok(ResolvedDatumPlane {
                frame: plane.frame(),
                half_extent: [ORIGIN_DATUM_HALF_EXTENT; 2],
            }),
            DatumPlaneBase::Face(face) => {
                let geometry = resolver.face(face)?;
                let frame =
                    orthonormal_frame(geometry.frame).ok_or(DatumPlaneError::FaceNotPlanar)?;
                Ok(ResolvedDatumPlane {
                    frame,
                    half_extent: clamp_extent(geometry.half_extent),
                })
            }
            DatumPlaneBase::Plane { plane } => {
                let resolved = resolver.plane(*plane)?;
                let frame = orthonormal_frame(resolved.frame)
                    .ok_or(DatumPlaneError::PlaneUnavailable(*plane))?;
                Ok(ResolvedDatumPlane {
                    frame,
                    half_extent: clamp_extent(resolved.half_extent),
                })
            }
            DatumPlaneBase::Midplane { first, second } => {
                let first = resolver.face(first)?;
                let second = resolver.face(second)?;
                midplane(first, second)
            }
            DatumPlaneBase::Edge { body, edge, face } => {
                let geometry = resolver.edge(*body, edge, face)?;
                edge_plane(geometry, self.angle_degrees)
            }
            DatumPlaneBase::Fixed { frame } => Ok(ResolvedDatumPlane {
                frame: orthonormal_frame(*frame).ok_or(DatumPlaneError::DegenerateFrame)?,
                half_extent: clamp_extent(self.half_extent),
            }),
        }
    }

    /// Resolves the plane: its base, moved by the offset along the base's
    /// normal, then turned to face the other way if flipped.
    pub fn resolve(
        &self,
        resolver: &dyn DatumPlaneResolver,
    ) -> Result<ResolvedDatumPlane, DatumPlaneError> {
        self.validate()?;
        let base = self.base_plane(resolver)?;
        Ok(place_on_base(base, self.offset, self.flip))
    }

    /// The last resolved plane, as cached in the recipe.
    #[must_use]
    pub const fn cached(&self) -> ResolvedDatumPlane {
        ResolvedDatumPlane {
            frame: self.frame,
            half_extent: self.half_extent,
        }
    }
}

/// Moves a base plane by `offset` along its normal and flips it if asked.
#[must_use]
pub fn place_on_base(base: ResolvedDatumPlane, offset: f64, flip: bool) -> ResolvedDatumPlane {
    let frame = offset_along_normal(base.frame, offset);
    ResolvedDatumPlane {
        frame: if flip { flipped(frame) } else { frame },
        half_extent: base.half_extent,
    }
}

/// The plane halfway between two parallel faces, facing as the first does.
pub fn midplane(
    first: DatumFaceGeometry,
    second: DatumFaceGeometry,
) -> Result<ResolvedDatumPlane, DatumPlaneError> {
    let first_frame = orthonormal_frame(first.frame).ok_or(DatumPlaneError::FaceNotPlanar)?;
    let second_frame = orthonormal_frame(second.frame).ok_or(DatumPlaneError::FaceNotPlanar)?;
    let first_normal = normal(first_frame);
    let second_normal = normal(second_frame);
    if dot(first_normal, second_normal).abs() < 1.0 - 1.0e-8 {
        return Err(DatumPlaneError::FacesNotParallel);
    }
    // The midpoint of the two faces' middles lies on the midplane: its height
    // along the normal is the mean of the two faces' heights.
    let origin = Point3::new(
        0.5 * (first_frame.origin.x + second_frame.origin.x),
        0.5 * (first_frame.origin.y + second_frame.origin.y),
        0.5 * (first_frame.origin.z + second_frame.origin.z),
    );
    let first_extent = clamp_extent(first.half_extent);
    let second_extent = clamp_extent(second.half_extent);
    Ok(ResolvedDatumPlane {
        frame: PlanarFrame3::new(origin, first_frame.u, first_frame.v),
        half_extent: [
            first_extent[0].max(second_extent[0]),
            first_extent[1].max(second_extent[1]),
        ],
    })
}

/// The plane through a straight edge, turned `angle_degrees` from the face.
///
/// The frame's `u` runs along the edge and its `v` away from it, so the card
/// hangs off the edge like a door on its hinge. At no angle the plane is the
/// face's plane with the face's outward normal; a positive angle lifts the
/// card off the face.
pub fn edge_plane(
    edge: DatumEdgeGeometry,
    angle_degrees: f64,
) -> Result<ResolvedDatumPlane, DatumPlaneError> {
    let along = sub(edge.end, edge.start);
    let length = length(along);
    if !length.is_finite() || length <= 1.0e-9 {
        return Err(DatumPlaneError::EdgeMissing);
    }
    let direction = scale(along, 1.0 / length);
    let face_normal = unit(edge.face_normal).ok_or(DatumPlaneError::FaceNotPlanar)?;
    if dot(direction, face_normal).abs() > 1.0e-6 {
        return Err(DatumPlaneError::EdgeNotOnFace);
    }
    // Square the inward direction to the edge and the normal, keeping its side.
    let into = unit(sub_vector(
        edge.into_face,
        scale(face_normal, dot(edge.into_face, face_normal)),
    ))
    .and_then(|into| unit(sub_vector(into, scale(direction, dot(into, direction)))))
    .ok_or(DatumPlaneError::EdgeNotOnFace)?;
    // Orient the edge so that (along, into, normal) is right-handed; then the
    // frame (along, into) has the face's own normal at no angle.
    let along = if dot(cross(direction, into), face_normal) >= 0.0 {
        direction
    } else {
        scale(direction, -1.0)
    };
    let angle = angle_degrees.to_radians();
    let (sin, cos) = angle.sin_cos();
    let leaning = add(scale(into, cos), scale(face_normal, sin));
    let half = (0.5 * length * 1.15).max(MIN_HALF_EXTENT);
    let middle = Point3::new(
        0.5 * (edge.start.x + edge.end.x),
        0.5 * (edge.start.y + edge.end.y),
        0.5 * (edge.start.z + edge.end.z),
    );
    let origin = translate(middle, scale(leaning, half));
    Ok(ResolvedDatumPlane {
        frame: PlanarFrame3::new(origin, along, leaning),
        half_extent: [half, half],
    })
}

/// A frame moved by `distance` along its own unit normal.
#[must_use]
pub fn offset_along_normal(frame: PlanarFrame3, distance: f64) -> PlanarFrame3 {
    let Some(frame) = orthonormal_frame(frame) else {
        return frame;
    };
    PlanarFrame3::new(
        translate(frame.origin, scale(normal(frame), distance)),
        frame.u,
        frame.v,
    )
}

/// The same plane facing the other way: `v` reversed, so `u × v` is too.
#[must_use]
pub fn flipped(frame: PlanarFrame3) -> PlanarFrame3 {
    PlanarFrame3::new(frame.origin, frame.u, scale(frame.v, -1.0))
}

/// The unit normal `u × v` of a frame, or `None` when it is degenerate.
#[must_use]
pub fn frame_normal(frame: PlanarFrame3) -> Option<Vector3> {
    unit(cross(frame.u, frame.v))
}

/// The signed distance of `point` above `frame`'s plane along its normal.
#[must_use]
pub fn height_above(frame: PlanarFrame3, point: Point3) -> Option<f64> {
    let normal = frame_normal(frame)?;
    Some(dot(sub(point, frame.origin), normal))
}

/// A frame with unit, square axes spanning the same plane and keeping the
/// same normal, or `None` when the axes are degenerate or not finite.
#[must_use]
pub fn orthonormal_frame(frame: PlanarFrame3) -> Option<PlanarFrame3> {
    if !frame.is_finite() {
        return None;
    }
    let u = unit(frame.u)?;
    let normal = unit(cross(frame.u, frame.v))?;
    let v = unit(cross(normal, u))?;
    Some(PlanarFrame3::new(frame.origin, u, v))
}

fn clamp_extent(extent: [f64; 2]) -> [f64; 2] {
    extent.map(|half| {
        if half.is_finite() {
            half.max(MIN_HALF_EXTENT)
        } else {
            MIN_HALF_EXTENT
        }
    })
}

fn check_reference(reference: &PersistentRef, kind: EntityKind) -> Result<(), DatumPlaneError> {
    if reference.kind != kind {
        return Err(match kind {
            EntityKind::Edge => DatumPlaneError::EdgeReferenceRequired,
            _ => DatumPlaneError::FaceReferenceRequired,
        });
    }
    let mut current = Some(reference);
    let mut depth = 0;
    while let Some(reference) = current {
        if depth >= MAX_PERSISTENT_LINEAGE_DEPTH
            || reference.version != CURRENT_PERSISTENT_REF_VERSION
            || reference.producer.get() == 0
        {
            return Err(DatumPlaneError::InvalidReference);
        }
        depth += 1;
        current = reference.lineage.as_deref();
    }
    Ok(())
}

fn normal(frame: PlanarFrame3) -> Vector3 {
    cross(frame.u, frame.v)
}

const fn sub(end: Point3, start: Point3) -> Vector3 {
    Vector3::new(end.x - start.x, end.y - start.y, end.z - start.z)
}

const fn sub_vector(left: Vector3, right: Vector3) -> Vector3 {
    Vector3::new(left.x - right.x, left.y - right.y, left.z - right.z)
}

const fn add(left: Vector3, right: Vector3) -> Vector3 {
    Vector3::new(left.x + right.x, left.y + right.y, left.z + right.z)
}

const fn translate(point: Point3, by: Vector3) -> Point3 {
    Point3::new(point.x + by.x, point.y + by.y, point.z + by.z)
}

const fn scale(vector: Vector3, factor: f64) -> Vector3 {
    Vector3::new(vector.x * factor, vector.y * factor, vector.z * factor)
}

fn dot(left: Vector3, right: Vector3) -> f64 {
    left.x
        .mul_add(right.x, left.y.mul_add(right.y, left.z * right.z))
}

fn cross(left: Vector3, right: Vector3) -> Vector3 {
    Vector3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

fn length(vector: Vector3) -> f64 {
    dot(vector, vector).sqrt()
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = length(vector);
    (length.is_finite() && length > f64::EPSILON).then(|| scale(vector, 1.0 / length))
}

#[cfg(test)]
mod tests {
    use artificer_protocol::OperationRole;

    use super::*;

    struct Model {
        top: DatumFaceGeometry,
        bottom: DatumFaceGeometry,
        edge: DatumEdgeGeometry,
    }

    impl DatumPlaneResolver for Model {
        fn face(&self, face: &DatumFaceRef) -> Result<DatumFaceGeometry, DatumPlaneError> {
            match face.body.get() {
                1 => Ok(self.top),
                2 => Ok(self.bottom),
                _ => Err(DatumPlaneError::FaceMissing),
            }
        }

        fn edge(
            &self,
            _: BodyId,
            _: &PersistentRef,
            _: &PersistentRef,
        ) -> Result<DatumEdgeGeometry, DatumPlaneError> {
            Ok(self.edge)
        }

        fn plane(&self, plane: FeatureId) -> Result<ResolvedDatumPlane, DatumPlaneError> {
            Err(DatumPlaneError::UnknownPlane(plane))
        }
    }

    /// A 10 × 10 × 4 block from the origin: its top at z = 4 facing +Z, its
    /// bottom at z = 0 facing −Z, and the top's edge along y = 0.
    fn block() -> Model {
        Model {
            top: DatumFaceGeometry {
                frame: PlanarFrame3::new(
                    Point3::new(5.0, 5.0, 4.0),
                    Vector3::new(1.0, 0.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                ),
                half_extent: [5.75, 5.75],
            },
            bottom: DatumFaceGeometry {
                frame: PlanarFrame3::new(
                    Point3::new(5.0, 5.0, 0.0),
                    Vector3::new(0.0, 1.0, 0.0),
                    Vector3::new(1.0, 0.0, 0.0),
                ),
                half_extent: [5.75, 5.75],
            },
            edge: DatumEdgeGeometry {
                start: Point3::new(0.0, 0.0, 4.0),
                end: Point3::new(10.0, 0.0, 4.0),
                face_normal: Vector3::new(0.0, 0.0, 1.0),
                into_face: Vector3::new(0.0, 1.0, 0.0),
            },
        }
    }

    fn reference(kind: EntityKind) -> PersistentRef {
        PersistentRef::new(
            FeatureId::from_allocated(7),
            OperationRole::new("extrusion.top", None),
            kind,
        )
    }

    fn recipe(base: DatumPlaneBase) -> DatumPlaneRecipe {
        DatumPlaneRecipe::new(
            base,
            ResolvedDatumPlane {
                frame: OriginPlane::Xy.frame(),
                half_extent: [1.0, 1.0],
            },
        )
    }

    fn close(left: Point3, right: Point3) -> bool {
        length(sub(left, right)) < 1.0e-12
    }

    fn close_vector(left: Vector3, right: Vector3) -> bool {
        length(sub_vector(left, right)) < 1.0e-12
    }

    #[test]
    fn an_origin_plane_offsets_along_its_normal() {
        let mut plane = recipe(DatumPlaneBase::Origin {
            plane: OriginPlane::Xz,
        });
        plane.offset = 20.0;
        let resolved = plane.resolve(&block()).unwrap();
        // XZ faces −Y, so twenty along its normal is y = −20.
        assert!(close(resolved.frame.origin, Point3::new(0.0, -20.0, 0.0)));
        assert!(close_vector(
            frame_normal(resolved.frame).unwrap(),
            Vector3::new(0.0, -1.0, 0.0)
        ));
    }

    #[test]
    fn a_face_plane_offsets_outward_and_a_flip_keeps_its_place() {
        let mut plane = recipe(DatumPlaneBase::Face(DatumFaceRef {
            body: BodyId::from_allocated(1),
            face: reference(EntityKind::Face),
        }));
        plane.offset = 3.0;
        let resolved = plane.resolve(&block()).unwrap();
        assert!(close(resolved.frame.origin, Point3::new(5.0, 5.0, 7.0)));
        plane.flip = true;
        let flipped = plane.resolve(&block()).unwrap();
        assert!(close(flipped.frame.origin, Point3::new(5.0, 5.0, 7.0)));
        assert!(close_vector(
            frame_normal(flipped.frame).unwrap(),
            Vector3::new(0.0, 0.0, -1.0)
        ));
    }

    #[test]
    fn a_midplane_sits_halfway_and_faces_as_the_first_face() {
        let plane = recipe(DatumPlaneBase::Midplane {
            first: DatumFaceRef {
                body: BodyId::from_allocated(1),
                face: reference(EntityKind::Face),
            },
            second: DatumFaceRef {
                body: BodyId::from_allocated(2),
                face: reference(EntityKind::Face),
            },
        });
        let resolved = plane.resolve(&block()).unwrap();
        assert!((resolved.frame.origin.z - 2.0).abs() < 1.0e-12);
        assert!(close_vector(
            frame_normal(resolved.frame).unwrap(),
            Vector3::new(0.0, 0.0, 1.0)
        ));
    }

    #[test]
    fn an_edge_plane_lies_on_its_face_and_lifts_with_the_angle() {
        let mut plane = recipe(DatumPlaneBase::Edge {
            body: BodyId::from_allocated(1),
            edge: reference(EntityKind::Edge),
            face: reference(EntityKind::Face),
        });
        let flat = plane.resolve(&block()).unwrap();
        assert!(close_vector(
            frame_normal(flat.frame).unwrap(),
            Vector3::new(0.0, 0.0, 1.0)
        ));
        assert!((flat.frame.origin.z - 4.0).abs() < 1.0e-12);

        plane.angle_degrees = 90.0;
        let upright = plane.resolve(&block()).unwrap();
        // Standing up on the edge at y = 0, the card rises above the face and
        // its normal points out of the block, towards −Y.
        assert!(close_vector(
            frame_normal(upright.frame).unwrap(),
            Vector3::new(0.0, -1.0, 0.0)
        ));
        assert!(upright.frame.origin.y.abs() < 1.0e-12);
        assert!(upright.frame.origin.z > 4.0);
        // Both ends of the edge lie in the plane at every angle.
        for angle in [-120.0, -30.0, 0.0, 45.0, 135.0] {
            plane.angle_degrees = angle;
            let resolved = plane.resolve(&block()).unwrap();
            for point in [block().edge.start, block().edge.end] {
                assert!(height_above(resolved.frame, point).unwrap().abs() < 1.0e-9);
            }
        }
    }

    #[test]
    fn an_edge_off_its_face_is_refused_by_name() {
        let mut model = block();
        model.edge.end = Point3::new(10.0, 0.0, 9.0);
        let plane = recipe(DatumPlaneBase::Edge {
            body: BodyId::from_allocated(1),
            edge: reference(EntityKind::Edge),
            face: reference(EntityKind::Face),
        });
        assert_eq!(plane.resolve(&model), Err(DatumPlaneError::EdgeNotOnFace));
    }

    #[test]
    fn a_fixed_plane_offsets_from_where_it_was_fixed() {
        let mut plane = recipe(DatumPlaneBase::Fixed {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 10.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
        });
        assert!(close(
            plane.resolve(&block()).unwrap().frame.origin,
            Point3::new(0.0, 0.0, 10.0)
        ));
        // Caching a resolved frame does not move the base: resolving again
        // with a new offset starts from where the plane was fixed.
        plane.offset = 5.0;
        let moved = plane.resolve(&block()).unwrap();
        assert!(close(moved.frame.origin, Point3::new(0.0, 0.0, 15.0)));
        plane.frame = moved.frame;
        plane.offset = 2.0;
        assert!(close(
            plane.resolve(&block()).unwrap().frame.origin,
            Point3::new(0.0, 0.0, 12.0)
        ));
    }

    #[test]
    fn recipes_refuse_what_they_cannot_mean() {
        let mut plane = recipe(DatumPlaneBase::Origin {
            plane: OriginPlane::Xy,
        });
        plane.angle_degrees = 10.0;
        assert_eq!(plane.validate(), Err(DatumPlaneError::AngleWithoutEdge));
        plane.angle_degrees = 0.0;
        plane.offset = f64::NAN;
        assert_eq!(plane.validate(), Err(DatumPlaneError::InvalidOffset));
        let wrong_kind = recipe(DatumPlaneBase::Face(DatumFaceRef {
            body: BodyId::from_allocated(1),
            face: reference(EntityKind::Edge),
        }));
        assert_eq!(
            wrong_kind.validate(),
            Err(DatumPlaneError::FaceReferenceRequired)
        );
    }

    #[test]
    fn a_recipe_round_trips_through_json_and_omits_its_defaults() {
        let mut plane = recipe(DatumPlaneBase::Origin {
            plane: OriginPlane::Yz,
        });
        let json = serde_json::to_string(&plane).unwrap();
        assert!(!json.contains("offset"));
        assert!(!json.contains("visible"));
        plane.offset = -2.5;
        plane.visible = false;
        let json = serde_json::to_string(&plane).unwrap();
        let back: DatumPlaneRecipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, plane);
    }
}

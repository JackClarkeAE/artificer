//! Planes placed by the current body's faces and edges, for `sketch(on: …)`
//! (ADR 0048).
//!
//! These are the script forms of the workbench's construction planes and
//! follow the same conventions, so a plane written in a script and one placed
//! in the workbench on the same face or edge are the same plane:
//!
//! - **On a face**, the plane is the face's own frame — the frame a sketch on
//!   that face uses — moved `offset` along the face's outward normal.
//! - **Between two faces**, it is halfway between two parallel planar faces,
//!   through the midpoint of their origins, with the first face's axes and
//!   normal.
//! - **Through an edge**, it hangs off a straight edge like a door on its
//!   hinge: `u` runs along the edge, `v` leans away from it, turned `angle`
//!   degrees from the face it starts on. At no angle it lies on the face with
//!   the face's outward normal; a positive angle lifts it off the face.
//!
//! Every form then moves `offset` along its own normal and, with `flip`,
//! faces the other way.

use artificer_protocol::{PlanarFrame3, Point3, Vector3};

use crate::StraightEdgeOnFace;
use crate::api::debug::{ApiError, ApiErrorCode};

fn dot(first: Vector3, second: Vector3) -> f64 {
    first.x * second.x + first.y * second.y + first.z * second.z
}

fn cross(first: Vector3, second: Vector3) -> Vector3 {
    Vector3::new(
        first.y * second.z - first.z * second.y,
        first.z * second.x - first.x * second.z,
        first.x * second.y - first.y * second.x,
    )
}

fn scale(vector: Vector3, factor: f64) -> Vector3 {
    Vector3::new(vector.x * factor, vector.y * factor, vector.z * factor)
}

fn add(first: Vector3, second: Vector3) -> Vector3 {
    Vector3::new(first.x + second.x, first.y + second.y, first.z + second.z)
}

fn between(from: Point3, to: Point3) -> Vector3 {
    Vector3::new(to.x - from.x, to.y - from.y, to.z - from.z)
}

fn moved(point: Point3, by: Vector3) -> Point3 {
    Point3::new(point.x + by.x, point.y + by.y, point.z + by.z)
}

fn unit(vector: Vector3) -> Option<Vector3> {
    let length = dot(vector, vector).sqrt();
    (length.is_finite() && length > 1.0e-12).then(|| scale(vector, 1.0 / length))
}

fn invalid(message: &str) -> ApiError {
    ApiError::new(ApiErrorCode::InvalidInput, message)
}

/// A frame with unit, square axes spanning the same plane with the same
/// normal.
fn orthonormal(frame: PlanarFrame3) -> Result<PlanarFrame3, ApiError> {
    let degenerate = || invalid("plane(): the face has no usable frame");
    let u = unit(frame.u).ok_or_else(degenerate)?;
    let normal = unit(cross(frame.u, frame.v)).ok_or_else(degenerate)?;
    let v = unit(cross(normal, u)).ok_or_else(degenerate)?;
    Ok(PlanarFrame3::new(frame.origin, u, v))
}

/// Moves a frame `offset` along its own normal, then faces it the other way
/// when asked: `v` reversed, so `u × v` is too.
pub(crate) fn placed(
    frame: PlanarFrame3,
    offset: f64,
    flip: bool,
) -> Result<PlanarFrame3, ApiError> {
    if !offset.is_finite() {
        return Err(invalid("plane(): `offset` must be a finite length"));
    }
    let frame = orthonormal(frame)?;
    let normal = cross(frame.u, frame.v);
    let origin = moved(frame.origin, scale(normal, offset));
    Ok(PlanarFrame3::new(
        origin,
        frame.u,
        if flip { scale(frame.v, -1.0) } else { frame.v },
    ))
}

/// The plane halfway between two parallel faces, facing as the first does.
pub(crate) fn midplane(
    first: PlanarFrame3,
    second: PlanarFrame3,
) -> Result<PlanarFrame3, ApiError> {
    let first = orthonormal(first)?;
    let second = orthonormal(second)?;
    let first_normal = cross(first.u, first.v);
    let second_normal = cross(second.u, second.v);
    if dot(first_normal, second_normal).abs() < 1.0 - 1.0e-8 {
        return Err(invalid(
            "plane(): `between` needs two parallel planar faces; these two meet at an angle",
        ));
    }
    // The midpoint of the two faces' origins lies on the midplane: its height
    // along the normal is the mean of the two faces' heights.
    let origin = Point3::new(
        0.5 * (first.origin.x + second.origin.x),
        0.5 * (first.origin.y + second.origin.y),
        0.5 * (first.origin.z + second.origin.z),
    );
    Ok(PlanarFrame3::new(origin, first.u, first.v))
}

/// The plane through a straight edge, turned `angle_degrees` from its face,
/// with its origin half an edge-length off the edge's middle.
pub(crate) fn through_edge(
    edge: StraightEdgeOnFace,
    angle_degrees: f64,
) -> Result<PlanarFrame3, ApiError> {
    if !angle_degrees.is_finite() {
        return Err(invalid(
            "plane(): `angle` must be a finite number of degrees",
        ));
    }
    let not_on_face = || invalid("plane(): the edge is not on the face it is turned from");
    let along = between(edge.start, edge.end);
    let length = dot(along, along).sqrt();
    let direction = unit(along).ok_or_else(not_on_face)?;
    let face_normal = unit(edge.face_normal).ok_or_else(not_on_face)?;
    if dot(direction, face_normal).abs() > 1.0e-6 {
        return Err(not_on_face());
    }
    // Square the inward direction to the edge and the normal, keeping its side.
    let into = unit(add(
        edge.into_face,
        scale(face_normal, -dot(edge.into_face, face_normal)),
    ))
    .and_then(|into| unit(add(into, scale(direction, -dot(into, direction)))))
    .ok_or_else(not_on_face)?;
    // Orient the edge so that (along, into, normal) is right-handed; then the
    // frame (along, into) has the face's own normal at no angle.
    let along = if dot(cross(direction, into), face_normal) >= 0.0 {
        direction
    } else {
        scale(direction, -1.0)
    };
    let (sin, cos) = angle_degrees.to_radians().sin_cos();
    let leaning = add(scale(into, cos), scale(face_normal, sin));
    let half = (0.5 * length * 1.15).max(0.5);
    let middle = Point3::new(
        0.5 * (edge.start.x + edge.end.x),
        0.5 * (edge.start.y + edge.end.y),
        0.5 * (edge.start.z + edge.end.z),
    );
    Ok(PlanarFrame3::new(
        moved(middle, scale(leaning, half)),
        along,
        leaning,
    ))
}

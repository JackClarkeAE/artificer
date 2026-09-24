//! The little 3D arithmetic CAM needs over the protocol's points and vectors.

use artificer_protocol::{Point3, Vector3};

#[must_use]
pub const fn vector(x: f64, y: f64, z: f64) -> Vector3 {
    Vector3::new(x, y, z)
}

#[must_use]
pub fn dot(a: Vector3, b: Vector3) -> f64 {
    a.z.mul_add(b.z, a.x.mul_add(b.x, a.y * b.y))
}

#[must_use]
pub fn cross(a: Vector3, b: Vector3) -> Vector3 {
    vector(
        a.y.mul_add(b.z, -(a.z * b.y)),
        a.z.mul_add(b.x, -(a.x * b.z)),
        a.x.mul_add(b.y, -(a.y * b.x)),
    )
}

#[must_use]
pub fn length(a: Vector3) -> f64 {
    dot(a, a).sqrt()
}

#[must_use]
pub fn normalised(a: Vector3) -> Option<Vector3> {
    let len = length(a);
    (len.is_finite() && len > 1.0e-12).then(|| scale(a, 1.0 / len))
}

#[must_use]
pub fn scale(a: Vector3, factor: f64) -> Vector3 {
    vector(a.x * factor, a.y * factor, a.z * factor)
}

#[must_use]
pub fn add(a: Vector3, b: Vector3) -> Vector3 {
    vector(a.x + b.x, a.y + b.y, a.z + b.z)
}

#[must_use]
pub fn sub(a: Point3, b: Point3) -> Vector3 {
    vector(a.x - b.x, a.y - b.y, a.z - b.z)
}

#[must_use]
pub fn offset(p: Point3, v: Vector3) -> Point3 {
    Point3::new(p.x + v.x, p.y + v.y, p.z + v.z)
}

#[must_use]
pub fn as_vector(p: Point3) -> Vector3 {
    vector(p.x, p.y, p.z)
}

#[must_use]
pub fn as_point(v: Vector3) -> Point3 {
    Point3::new(v.x, v.y, v.z)
}

/// Whether two unit vectors point the same way (`1`), opposite ways (`-1`),
/// or neither (`0`), to a tight tolerance.
#[must_use]
pub fn parallel_sign(a: Vector3, b: Vector3) -> i8 {
    let (Some(a), Some(b)) = (normalised(a), normalised(b)) else {
        return 0;
    };
    let d = dot(a, b);
    if d > 1.0 - 1.0e-9 {
        1
    } else if d < -1.0 + 1.0e-9 {
        -1
    } else {
        0
    }
}

#[must_use]
pub fn perpendicular(a: Vector3, b: Vector3) -> bool {
    let (Some(a), Some(b)) = (normalised(a), normalised(b)) else {
        return false;
    };
    dot(a, b).abs() <= 1.0e-9
}

/// A right-handed frame: machine coordinates are `(p·u, p·v, p·w)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub origin: Point3,
    pub u: Vector3,
    pub v: Vector3,
    pub w: Vector3,
}

impl Frame {
    pub const IDENTITY: Self = Self {
        origin: Point3::new(0.0, 0.0, 0.0),
        u: vector(1.0, 0.0, 0.0),
        v: vector(0.0, 1.0, 0.0),
        w: vector(0.0, 0.0, 1.0),
    };

    /// World to machine.
    #[must_use]
    pub fn to_local(&self, p: Point3) -> Point3 {
        let d = sub(p, self.origin);
        Point3::new(dot(d, self.u), dot(d, self.v), dot(d, self.w))
    }

    /// Machine to world.
    #[must_use]
    pub fn to_world(&self, p: Point3) -> Point3 {
        offset(
            self.origin,
            add(
                add(scale(self.u, p.x), scale(self.v, p.y)),
                scale(self.w, p.z),
            ),
        )
    }

    #[must_use]
    pub fn vector_to_world(&self, v: Vector3) -> Vector3 {
        add(
            add(scale(self.u, v.x), scale(self.v, v.y)),
            scale(self.w, v.z),
        )
    }
}

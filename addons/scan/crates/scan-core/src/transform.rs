//! Rigid transforms used to align scans with each other and with datums.

use artificer_geometry::{Point3, Vector3};

/// Normalizes a vector, rejecting zero and non-finite input.
pub fn normalize(vector: Vector3) -> Option<Vector3> {
    let length = vector.length();
    (length.is_finite() && length > 0.0).then(|| vector / length)
}

/// A right-handed orthonormal basis completing `axis` (which must be unit).
pub fn orthonormal_basis(axis: Vector3) -> (Vector3, Vector3) {
    let helper = if axis.x.abs() < 0.9 {
        Vector3::new(1.0, 0.0, 0.0)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };
    let e1 = normalize(axis.cross(helper)).unwrap_or(Vector3::new(0.0, 0.0, 1.0));
    let e2 = axis.cross(e1);
    (e1, e2)
}

/// Proper rigid motion: rotation followed by translation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidTransform {
    pub rotation: [[f64; 3]; 3],
    pub translation: Vector3,
}

impl RigidTransform {
    pub const IDENTITY: Self = Self {
        rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translation: Vector3::new(0.0, 0.0, 0.0),
    };

    pub const fn from_translation(translation: Vector3) -> Self {
        Self {
            rotation: Self::IDENTITY.rotation,
            translation,
        }
    }

    /// Rodrigues rotation about a (not necessarily unit) axis.
    pub fn from_axis_angle(axis: Vector3, angle: f64) -> Option<Self> {
        let unit = normalize(axis)?;
        let (s, c) = angle.sin_cos();
        let t = 1.0 - c;
        let (x, y, z) = (unit.x, unit.y, unit.z);
        Some(Self {
            rotation: [
                [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
                [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
                [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
            ],
            translation: Vector3::new(0.0, 0.0, 0.0),
        })
    }

    /// The map taking world coordinates into the frame with the given origin,
    /// `z` normal, and `x` reference direction (projected orthogonal to `z`).
    ///
    /// This is the 3-2-1 datum alignment primitive: a fitted plane supplies
    /// `z`, a fitted axis or edge supplies `x`, and a fitted point supplies
    /// the origin.
    pub fn to_frame(origin: Point3, x_hint: Vector3, z: Vector3) -> Option<Self> {
        let z_axis = normalize(z)?;
        let x_axis = normalize(x_hint - z_axis * x_hint.dot(z_axis))?;
        let y_axis = z_axis.cross(x_axis);
        let rotation = [
            [x_axis.x, x_axis.y, x_axis.z],
            [y_axis.x, y_axis.y, y_axis.z],
            [z_axis.x, z_axis.y, z_axis.z],
        ];
        let rotated = apply_rotation(&rotation, origin - Point3::default());
        Some(Self {
            rotation,
            translation: rotated * -1.0,
        })
    }

    pub fn apply_vector(&self, vector: Vector3) -> Vector3 {
        apply_rotation(&self.rotation, vector)
    }

    pub fn apply_point(&self, point: Point3) -> Point3 {
        Point3::default() + self.apply_vector(point - Point3::default()) + self.translation
    }

    /// `self.then(next)` applies `self` first, then `next`.
    pub fn then(&self, next: &Self) -> Self {
        let mut rotation = [[0.0; 3]; 3];
        for (i, row) in rotation.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                for k in 0..3 {
                    *cell += next.rotation[i][k] * self.rotation[k][j];
                }
            }
        }
        Self {
            rotation,
            translation: next.apply_vector(self.translation) + next.translation,
        }
    }

    pub fn inverse(&self) -> Self {
        let mut transposed = [[0.0; 3]; 3];
        for (i, row) in transposed.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = self.rotation[j][i];
            }
        }
        let translation = apply_rotation(&transposed, self.translation) * -1.0;
        Self {
            rotation: transposed,
            translation,
        }
    }

    /// Re-orthonormalizes the rotation after accumulated composition drift.
    pub fn renormalized(&self) -> Self {
        let row = |i: usize| {
            Vector3::new(
                self.rotation[i][0],
                self.rotation[i][1],
                self.rotation[i][2],
            )
        };
        let x = normalize(row(0)).unwrap_or(Vector3::new(1.0, 0.0, 0.0));
        let mut y = row(1) - x * row(1).dot(x);
        y = normalize(y).unwrap_or(Vector3::new(0.0, 1.0, 0.0));
        let z = x.cross(y);
        Self {
            rotation: [[x.x, x.y, x.z], [y.x, y.y, y.z], [z.x, z.y, z.z]],
            translation: self.translation,
        }
    }
}

fn apply_rotation(rotation: &[[f64; 3]; 3], v: Vector3) -> Vector3 {
    Vector3::new(
        rotation[0][0] * v.x + rotation[0][1] * v.y + rotation[0][2] * v.z,
        rotation[1][0] * v.x + rotation[1][1] * v.y + rotation[1][2] * v.z,
        rotation[2][0] * v.x + rotation[2][1] * v.y + rotation[2][2] * v.z,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_angle_rotates_quarter_turn() {
        let t = RigidTransform::from_axis_angle(
            Vector3::new(0.0, 0.0, 1.0),
            std::f64::consts::FRAC_PI_2,
        )
        .unwrap();
        let p = t.apply_point(Point3::new(1.0, 0.0, 0.0));
        assert!((p.x).abs() < 1e-12 && (p.y - 1.0).abs() < 1e-12);
    }

    #[test]
    fn inverse_round_trips() {
        let t = RigidTransform::from_axis_angle(Vector3::new(1.0, 2.0, 3.0), 0.7)
            .unwrap()
            .then(&RigidTransform::from_translation(Vector3::new(
                4.0, -2.0, 9.0,
            )));
        let p = Point3::new(0.3, -1.2, 5.5);
        let back = t.inverse().apply_point(t.apply_point(p));
        assert!((back - p).length() < 1e-12);
    }

    #[test]
    fn frame_alignment_maps_datums_to_axes() {
        let t = RigidTransform::to_frame(
            Point3::new(5.0, 5.0, 5.0),
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(0.0, 0.0, 2.0),
        )
        .unwrap();
        let origin = t.apply_point(Point3::new(5.0, 5.0, 5.0));
        assert!((origin - Point3::default()).length() < 1e-12);
        let up = t.apply_vector(Vector3::new(0.0, 0.0, 1.0));
        assert!((up.z - 1.0).abs() < 1e-12);
    }
}

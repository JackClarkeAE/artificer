//! Proper similarity-transform construction for committed topology.
//!
//! This module owns only deterministic geometry mutation and quaternion math.
//! Transaction checks, diagnostics, validation, and publication remain in the
//! kernel facade.

use artificer_protocol::{RotationQuaternion, SimilarityTransform3};

use crate::topology::{Curve2, Curve3, Plane, Point3, Surface, Topology, Vector3};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransformInputError {
    NonFinite,
    NonPositiveScale,
    ZeroQuaternion,
}

/// Canonicalized internal similarity `p' = scale * rotation(p) + translation`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Similarity {
    translation: Vector3,
    rotation: Rotation,
    scale: f64,
}

impl Similarity {
    pub(crate) fn from_protocol(
        transform: SimilarityTransform3,
    ) -> Result<Self, TransformInputError> {
        if !transform.translation.is_finite()
            || !transform.rotation.is_finite()
            || !transform.uniform_scale.is_finite()
        {
            return Err(TransformInputError::NonFinite);
        }
        if transform.uniform_scale <= 0.0 {
            return Err(TransformInputError::NonPositiveScale);
        }
        let rotation =
            Rotation::normalize(transform.rotation).ok_or(TransformInputError::ZeroQuaternion)?;
        Ok(Self {
            translation: Vector3::new(
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
            ),
            rotation,
            scale: transform.uniform_scale,
        })
    }

    pub(crate) const fn scale(self) -> f64 {
        self.scale
    }

    pub(crate) fn transform_point(self, point: Point3) -> Point3 {
        let rotated = self.rotation.rotate(point.as_vector()) * self.scale;
        Point3::new(
            rotated.x + self.translation.x,
            rotated.y + self.translation.y,
            rotated.z + self.translation.z,
        )
    }

    pub(crate) fn transform_vector(self, vector: Vector3) -> Vector3 {
        self.rotation.rotate(vector)
    }
}

/// Clones and transforms all authoritative geometric representations while
/// retaining incidence, ordering, orientation, and snapshot-local numeric IDs.
pub(crate) fn transform_topology(input: &Topology, transform: Similarity) -> Topology {
    let mut output = input.clone();

    for vertex in &mut output.vertices {
        vertex.value.point = transform.transform_point(vertex.value.point);
    }
    for edge in &mut output.edges {
        match &mut edge.value.curve {
            Curve3::Line { endpoints } => {
                *endpoints = endpoints.map(|point| transform.transform_point(point));
            }
            Curve3::Circle {
                center,
                u,
                v,
                radius,
            } => {
                *center = transform.transform_point(*center);
                *u = transform.transform_vector(*u);
                *v = transform.transform_vector(*v);
                *radius *= transform.scale;
            }
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum PcurveOwner {
        Planar,
        Cylindrical,
        /// Both torus parameters are angles; a similarity leaves them fixed.
        Toroidal,
    }
    let mut pcurve_owner = vec![PcurveOwner::Planar; input.coedges.len()];
    for face in &input.faces {
        let owner_kind = match face.value.surface {
            Surface::Plane(_) => PcurveOwner::Planar,
            Surface::Cylinder(_) => PcurveOwner::Cylindrical,
            Surface::Torus(_) => PcurveOwner::Toroidal,
            Surface::Cone(_) => PcurveOwner::Cylindrical,
            // Both sphere parameters are angles, so a similarity leaves them
            // fixed, exactly as for a torus.
            Surface::Sphere(_) => PcurveOwner::Toroidal,
        };
        for loop_key in face.value.loops() {
            if let Some(loop_record) = input.loop_record(loop_key) {
                for coedge_key in &loop_record.value.coedges {
                    if let Some(owner) = pcurve_owner.get_mut(coedge_key.0) {
                        *owner = owner_kind;
                    }
                }
            }
        }
    }
    for (index, coedge) in output.coedges.iter_mut().enumerate() {
        match &mut coedge.value.pcurve {
            Curve2::Line { endpoints } => match pcurve_owner[index] {
                PcurveOwner::Cylindrical => {
                    for endpoint in endpoints {
                        endpoint.y *= transform.scale;
                    }
                }
                PcurveOwner::Planar => {
                    for endpoint in endpoints {
                        endpoint.x *= transform.scale;
                        endpoint.y *= transform.scale;
                    }
                }
                PcurveOwner::Toroidal => {}
            },
            Curve2::Circle { center, radius, .. } => {
                center.x *= transform.scale;
                center.y *= transform.scale;
                *radius *= transform.scale;
            }
        }
    }
    for face in &mut output.faces {
        face.value.surface = match face.value.surface {
            Surface::Plane(plane) => Surface::Plane(Plane::new(
                transform.transform_point(plane.origin),
                transform.transform_vector(plane.u),
                transform.transform_vector(plane.v),
            )),
            Surface::Cylinder(mut cylinder) => {
                cylinder.origin = transform.transform_point(cylinder.origin);
                cylinder.axis = transform.transform_vector(cylinder.axis);
                cylinder.radial_u = transform.transform_vector(cylinder.radial_u);
                cylinder.radial_v = transform.transform_vector(cylinder.radial_v);
                cylinder.radius *= transform.scale;
                Surface::Cylinder(cylinder)
            }
            Surface::Torus(mut torus) => {
                torus.origin = transform.transform_point(torus.origin);
                torus.axis = transform.transform_vector(torus.axis);
                torus.radial_u = transform.transform_vector(torus.radial_u);
                torus.radial_v = transform.transform_vector(torus.radial_v);
                torus.major_radius *= transform.scale;
                torus.minor_radius *= transform.scale;
                Surface::Torus(torus)
            }
            Surface::Sphere(mut sphere) => {
                sphere.origin = transform.transform_point(sphere.origin);
                sphere.axis = transform.transform_vector(sphere.axis);
                sphere.radial_u = transform.transform_vector(sphere.radial_u);
                sphere.radial_v = transform.transform_vector(sphere.radial_v);
                sphere.radius *= transform.scale;
                Surface::Sphere(sphere)
            }
            Surface::Cone(mut cone) => {
                cone.origin = transform.transform_point(cone.origin);
                cone.axis = transform.transform_vector(cone.axis);
                cone.radial_u = transform.transform_vector(cone.radial_u);
                cone.radial_v = transform.transform_vector(cone.radial_v);
                cone.base_radius *= transform.scale;
                // Both the ring radius and the axial parameter scale, so the
                // slope (their ratio) is invariant under a similarity.
                Surface::Cone(cone)
            }
        };
    }

    output
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rotation {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}

impl Rotation {
    /// Normalizes without overflowing by first scaling by the largest
    /// component, then selects one of the equivalent `q`/`-q` forms.
    fn normalize(quaternion: RotationQuaternion) -> Option<Self> {
        let maximum = [
            quaternion.w.abs(),
            quaternion.x.abs(),
            quaternion.y.abs(),
            quaternion.z.abs(),
        ]
        .into_iter()
        .fold(0.0_f64, f64::max);
        if !maximum.is_finite() || maximum == 0.0 {
            return None;
        }

        let mut values = [
            quaternion.w / maximum,
            quaternion.x / maximum,
            quaternion.y / maximum,
            quaternion.z / maximum,
        ];
        let norm = values.iter().map(|value| value * value).sum::<f64>().sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return None;
        }
        for value in &mut values {
            *value /= norm;
            if *value == 0.0 {
                *value = 0.0;
            }
        }

        let first_nonzero = values.iter().copied().find(|value| *value != 0.0)?;
        if first_nonzero.is_sign_negative() {
            for value in &mut values {
                *value = -*value;
            }
        }
        Some(Self {
            w: values[0],
            x: values[1],
            y: values[2],
            z: values[3],
        })
    }

    fn rotate(self, vector: Vector3) -> Vector3 {
        // Unit-quaternion vector rotation: v' = v + w*t + qv×t,
        // where t = 2(qv×v). This uses fewer operations than q*v*q^-1.
        let imaginary = Vector3::new(self.x, self.y, self.z);
        let twice_cross = imaginary.cross(vector) * 2.0;
        vector + twice_cross * self.w + imaginary.cross(twice_cross)
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_1_SQRT_2;

    use artificer_protocol::Vector3 as ProtocolVector3;

    use super::*;

    fn similarity(rotation: RotationQuaternion) -> Similarity {
        Similarity::from_protocol(SimilarityTransform3 {
            translation: ProtocolVector3::new(10.0, -2.0, 0.5),
            rotation,
            uniform_scale: 2.0,
        })
        .unwrap()
    }

    #[test]
    fn robust_normalization_handles_large_equivalent_quaternions() {
        let ordinary = similarity(RotationQuaternion::new(
            FRAC_1_SQRT_2,
            0.0,
            0.0,
            FRAC_1_SQRT_2,
        ));
        let huge = similarity(RotationQuaternion::new(1.0e300, 0.0, 0.0, 1.0e300));
        let point = Point3::new(3.0, 4.0, 5.0);
        assert_eq!(ordinary.transform_point(point), huge.transform_point(point));
    }

    #[test]
    fn quaternion_sign_is_geometrically_canonical() {
        let positive = similarity(RotationQuaternion::new(0.5, -0.5, 0.5, -0.5));
        let negative = similarity(RotationQuaternion::new(-0.5, 0.5, -0.5, 0.5));
        assert_eq!(positive, negative);
    }

    #[test]
    fn malformed_similarity_is_rejected() {
        let mut transform = SimilarityTransform3::identity();
        transform.uniform_scale = 0.0;
        assert_eq!(
            Similarity::from_protocol(transform),
            Err(TransformInputError::NonPositiveScale)
        );
        transform.uniform_scale = 1.0;
        transform.rotation = RotationQuaternion::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            Similarity::from_protocol(transform),
            Err(TransformInputError::ZeroQuaternion)
        );
    }
}

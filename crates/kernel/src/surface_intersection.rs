//! Closed-form intersections between the surfaces in the kernel's vocabulary.
//!
//! Booleans need the curve where two faces' carriers meet. For the general
//! surface pair that curve has no closed form and no representation in this
//! kernel's line/circle vocabulary — but for the pairs that actually arise in
//! modelled geometry it has both, and the answer is exact.
//!
//! The published domain is deliberately narrow and stated as a matrix rather
//! than discovered at runtime. Everything inside it returns lines and circles
//! derived algebraically; everything outside it returns
//! [`IntersectionError::Unsupported`] so the caller can refuse the operation
//! rather than approximate it. An ellipse from an oblique plane through a
//! cylinder is *not* an error in the geometry — it is a curve this kernel
//! cannot yet name, and saying so is the whole point.
//!
//! | | Plane | Cylinder | Cone | Sphere | Torus |
//! |---|---|---|---|---|---|
//! | **Plane** | line | circle ⟂, lines ∥ | circle ⟂ | circle | circles ⟂, circles through the axis |
//! | **Cylinder** | | coaxial, parallel axes | coaxial | centre on the axis | — |
//! | **Cone** | | | coaxial | — | — |
//! | **Sphere** | | | | any pair | — |
//! | **Torus** | | | | | coaxial |

use artificer_protocol::PrecisionPolicy;

use crate::topology::{Cone, Cylinder, Plane, Point3, Sphere, Surface, Torus, Vector3};

/// One exact curve where two surfaces meet.
///
/// Both variants are unbounded: a surface pair meets along a whole line or a
/// whole circle, and trimming that to the faces' own extents is the caller's
/// job, not the carrier's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum IntersectionCurve {
    Line {
        origin: Point3,
        direction: Vector3,
    },
    Circle {
        center: Point3,
        u: Vector3,
        v: Vector3,
        radius: f64,
    },
}

/// What two surface carriers share.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SurfaceIntersection {
    /// The carriers do not meet, or meet only at isolated points. A tangency
    /// point carries no curve to imprint, so it is reported as empty rather
    /// than as a degenerate curve.
    Empty,
    /// The carriers are the same surface. The caller must resolve the overlap
    /// by classification rather than by imprinting.
    Coincident,
    /// One or two exact curves.
    Curves(Vec<IntersectionCurve>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntersectionError {
    /// The pair lies outside the published matrix. The true intersection may
    /// well exist; it is simply not expressible as lines and circles.
    Unsupported,
    /// A degenerate carrier — a zero radius, a non-finite frame — that the
    /// validator should already have rejected upstream.
    Indeterminate,
}

type Intersection = Result<SurfaceIntersection, IntersectionError>;

/// Intersects two surface carriers, or reports why the pair leaves the domain.
pub(crate) fn intersect(left: Surface, right: Surface, precision: PrecisionPolicy) -> Intersection {
    let tolerances = Tolerances::from(precision);
    match (left, right) {
        (Surface::Plane(first), Surface::Plane(second)) => plane_plane(first, second, tolerances),
        (Surface::Plane(plane), Surface::Cylinder(cylinder))
        | (Surface::Cylinder(cylinder), Surface::Plane(plane)) => {
            plane_cylinder(plane, cylinder, tolerances)
        }
        (Surface::Plane(plane), Surface::Cone(cone))
        | (Surface::Cone(cone), Surface::Plane(plane)) => plane_cone(plane, cone, tolerances),
        (Surface::Plane(plane), Surface::Sphere(sphere))
        | (Surface::Sphere(sphere), Surface::Plane(plane)) => {
            plane_sphere(plane, sphere, tolerances)
        }
        (Surface::Plane(plane), Surface::Torus(torus))
        | (Surface::Torus(torus), Surface::Plane(plane)) => plane_torus(plane, torus, tolerances),
        (Surface::Cylinder(first), Surface::Cylinder(second)) => {
            cylinder_cylinder(first, second, tolerances)
        }
        (Surface::Cylinder(cylinder), Surface::Sphere(sphere))
        | (Surface::Sphere(sphere), Surface::Cylinder(cylinder)) => {
            cylinder_sphere(cylinder, sphere, tolerances)
        }
        (Surface::Cylinder(cylinder), Surface::Cone(cone))
        | (Surface::Cone(cone), Surface::Cylinder(cylinder)) => {
            cylinder_cone(cylinder, cone, tolerances)
        }
        (Surface::Sphere(first), Surface::Sphere(second)) => {
            sphere_sphere(first, second, tolerances)
        }
        (Surface::Cone(first), Surface::Cone(second)) => cone_cone(first, second, tolerances),
        (Surface::Torus(first), Surface::Torus(second)) => torus_torus(first, second, tolerances),
        _ => Err(IntersectionError::Unsupported),
    }
}

/// The two agreements every arm reads, resolved once.
#[derive(Clone, Copy, Debug)]
struct Tolerances {
    linear: f64,
    angular: f64,
}

impl From<PrecisionPolicy> for Tolerances {
    fn from(precision: PrecisionPolicy) -> Self {
        Self {
            linear: precision.linear_agreement.max(1.0e-12),
            angular: precision.angular_agreement_radians.max(1.0e-12),
        }
    }
}

impl Tolerances {
    /// Whether two unit directions name the same line, in either sense.
    fn parallel(self, first: Vector3, second: Vector3) -> bool {
        first.cross(second).length() <= self.angular
    }
}

// ---------------------------------------------------------------------------
// Plane pairs
// ---------------------------------------------------------------------------

fn plane_plane(first: Plane, second: Plane, tolerances: Tolerances) -> Intersection {
    let left = unit(first.normal)?;
    let right = unit(second.normal)?;
    let left_offset = first.origin.as_vector().dot(left);
    let right_offset = second.origin.as_vector().dot(right);
    if tolerances.parallel(left, right) {
        // Anti-parallel normals describe the same plane at the negated offset.
        let aligned = left.dot(right) > 0.0;
        let separation = if aligned {
            left_offset - right_offset
        } else {
            left_offset + right_offset
        };
        return Ok(if separation.abs() <= tolerances.linear {
            SurfaceIntersection::Coincident
        } else {
            SurfaceIntersection::Empty
        });
    }
    let direction = unit(left.cross(right))?;
    // The point of the intersection line closest to the origin, from the two
    // plane equations solved in the (left, right) span.
    let dot = left.dot(right);
    let denominator = dot.mul_add(-dot, 1.0);
    let left_scale = dot.mul_add(-right_offset, left_offset) / denominator;
    let right_scale = dot.mul_add(-left_offset, right_offset) / denominator;
    let base = left * left_scale + right * right_scale;
    Ok(SurfaceIntersection::Curves(vec![IntersectionCurve::Line {
        origin: Point3::new(base.x, base.y, base.z),
        direction,
    }]))
}

fn plane_cylinder(plane: Plane, cylinder: Cylinder, tolerances: Tolerances) -> Intersection {
    let normal = unit(plane.normal)?;
    let axis = unit(cylinder.axis)?;
    if cylinder.radius <= 0.0 || !cylinder.radius.is_finite() {
        return Err(IntersectionError::Indeterminate);
    }
    let offset = plane.origin - cylinder.origin;
    if tolerances.parallel(normal, axis) {
        // A cut perpendicular to the axis is the cylinder's own ring.
        let height = offset.dot(axis);
        let center = cylinder.origin + axis * height;
        return Ok(SurfaceIntersection::Curves(vec![
            IntersectionCurve::Circle {
                center,
                u: unit(cylinder.radial_u)?,
                v: unit(cylinder.radial_v)?,
                radius: cylinder.radius,
            },
        ]));
    }
    if normal.dot(axis).abs() > tolerances.angular {
        // An oblique cut is an ellipse, which this vocabulary cannot name.
        return Err(IntersectionError::Unsupported);
    }
    // The plane runs along the axis: every point of the axis is the same
    // signed distance from it, so the pair meets in generators. `offset` runs
    // from the axis to the plane, so its normal component reaches the foot.
    let distance = offset.dot(normal);
    let half = cylinder
        .radius
        .mul_add(cylinder.radius, -(distance * distance));
    if half < -tolerances.linear * cylinder.radius {
        return Ok(SurfaceIntersection::Empty);
    }
    let foot = cylinder.origin + normal * distance;
    if half <= tolerances.linear * cylinder.radius {
        // Grazing contact: one generator, counted once.
        return Ok(SurfaceIntersection::Curves(vec![IntersectionCurve::Line {
            origin: foot,
            direction: axis,
        }]));
    }
    let along = unit(axis.cross(normal))?;
    let reach = half.sqrt();
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Line {
            origin: foot + along * reach,
            direction: axis,
        },
        IntersectionCurve::Line {
            origin: foot + along * -reach,
            direction: axis,
        },
    ]))
}

fn plane_cone(plane: Plane, cone: Cone, tolerances: Tolerances) -> Intersection {
    let normal = unit(plane.normal)?;
    let axis = unit(cone.axis)?;
    if !tolerances.parallel(normal, axis) {
        // Every other cut is a conic section outside the vocabulary.
        return Err(IntersectionError::Unsupported);
    }
    let height = (plane.origin - cone.origin).dot(axis);
    let radius = cone.ring_radius(height);
    if !radius.is_finite() {
        return Err(IntersectionError::Indeterminate);
    }
    if radius <= tolerances.linear {
        // At or beyond the apex: a point, or nothing.
        return Ok(SurfaceIntersection::Empty);
    }
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center: cone.origin + axis * height,
            u: unit(cone.radial_u)?,
            v: unit(cone.radial_v)?,
            radius,
        },
    ]))
}

fn plane_sphere(plane: Plane, sphere: Sphere, tolerances: Tolerances) -> Intersection {
    let normal = unit(plane.normal)?;
    if sphere.radius <= 0.0 || !sphere.radius.is_finite() {
        return Err(IntersectionError::Indeterminate);
    }
    let height = (sphere.origin - plane.origin).dot(normal);
    let square = sphere.radius.mul_add(sphere.radius, -(height * height));
    // A tangent plane touches at one point, which carries no curve.
    if square <= tolerances.linear * sphere.radius {
        return Ok(SurfaceIntersection::Empty);
    }
    let (u, v) = orthonormal_to(normal)?;
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center: sphere.origin + normal * -height,
            u,
            v,
            radius: square.sqrt(),
        },
    ]))
}

fn plane_torus(plane: Plane, torus: Torus, tolerances: Tolerances) -> Intersection {
    let normal = unit(plane.normal)?;
    let axis = unit(torus.axis)?;
    let (major, minor) = (torus.major_radius, torus.minor_radius);
    if !major.is_finite() || !minor.is_finite() || minor <= 0.0 {
        return Err(IntersectionError::Indeterminate);
    }
    let offset = plane.origin - torus.origin;
    if tolerances.parallel(normal, axis) {
        // Perpendicular to the axis: the tube's own latitude circles, whose
        // ring radii straddle the major radius.
        let height = offset.dot(axis);
        let square = minor.mul_add(minor, -(height * height));
        if square < -tolerances.linear * minor {
            return Ok(SurfaceIntersection::Empty);
        }
        let center = torus.origin + axis * height;
        let (u, v) = (unit(torus.radial_u)?, unit(torus.radial_v)?);
        let reach = square.max(0.0).sqrt();
        let mut curves = Vec::with_capacity(2);
        for radius in [major + reach, major - reach] {
            if radius > tolerances.linear {
                curves.push(IntersectionCurve::Circle {
                    center,
                    u,
                    v,
                    radius,
                });
            }
        }
        // A grazing plane meets the tube in one circle, not two.
        if reach <= tolerances.linear * minor {
            curves.truncate(1);
        }
        return Ok(if curves.is_empty() {
            SurfaceIntersection::Empty
        } else {
            SurfaceIntersection::Curves(curves)
        });
    }
    // A plane containing the whole axis cuts the tube in its two generating
    // circles, one on each side.
    if normal.dot(axis).abs() <= tolerances.angular && offset.dot(normal).abs() <= tolerances.linear
    {
        let across = unit(axis.cross(normal))?;
        let (u, v) = (across, axis);
        return Ok(SurfaceIntersection::Curves(vec![
            IntersectionCurve::Circle {
                center: torus.origin + across * major,
                u,
                v,
                radius: minor,
            },
            IntersectionCurve::Circle {
                center: torus.origin + across * -major,
                u,
                v,
                radius: minor,
            },
        ]));
    }
    // Every other cut is a quartic, including the Villarceau case.
    Err(IntersectionError::Unsupported)
}

// ---------------------------------------------------------------------------
// Curved pairs
// ---------------------------------------------------------------------------

fn cylinder_cylinder(first: Cylinder, second: Cylinder, tolerances: Tolerances) -> Intersection {
    let axis = unit(first.axis)?;
    let other = unit(second.axis)?;
    if !tolerances.parallel(axis, other) {
        // Skew or crossing axes give a space quartic.
        return Err(IntersectionError::Unsupported);
    }
    if first.radius <= 0.0 || second.radius <= 0.0 {
        return Err(IntersectionError::Indeterminate);
    }
    // Work in the plane perpendicular to the shared axis direction, where both
    // cylinders are circles and the problem is two-dimensional.
    let offset = second.origin - first.origin;
    let across = offset - axis * offset.dot(axis);
    let separation = across.length();
    if separation <= tolerances.linear {
        return Ok(
            if (first.radius - second.radius).abs() <= tolerances.linear {
                SurfaceIntersection::Coincident
            } else {
                SurfaceIntersection::Empty
            },
        );
    }
    let (near, far) = (
        (first.radius - second.radius).abs(),
        first.radius + second.radius,
    );
    if separation > far + tolerances.linear || separation < near - tolerances.linear {
        return Ok(SurfaceIntersection::Empty);
    }
    let toward = across / separation;
    // The radical line's distance along the centre line.
    let reach = first
        .radius
        .mul_add(first.radius, -(second.radius * second.radius))
        .mul_add(1.0 / (2.0 * separation), separation / 2.0);
    let square = first.radius.mul_add(first.radius, -(reach * reach));
    let foot = first.origin + toward * reach;
    if square <= tolerances.linear * first.radius {
        return Ok(SurfaceIntersection::Curves(vec![IntersectionCurve::Line {
            origin: foot,
            direction: axis,
        }]));
    }
    let sideways = unit(axis.cross(toward))?;
    let half = square.sqrt();
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Line {
            origin: foot + sideways * half,
            direction: axis,
        },
        IntersectionCurve::Line {
            origin: foot + sideways * -half,
            direction: axis,
        },
    ]))
}

fn cylinder_sphere(cylinder: Cylinder, sphere: Sphere, tolerances: Tolerances) -> Intersection {
    let axis = unit(cylinder.axis)?;
    if cylinder.radius <= 0.0 || sphere.radius <= 0.0 {
        return Err(IntersectionError::Indeterminate);
    }
    let offset = sphere.origin - cylinder.origin;
    let across = offset - axis * offset.dot(axis);
    if across.length() > tolerances.linear {
        // Off the axis the pair meets in a space quartic.
        return Err(IntersectionError::Unsupported);
    }
    let square = sphere
        .radius
        .mul_add(sphere.radius, -(cylinder.radius * cylinder.radius));
    if square <= tolerances.linear * sphere.radius {
        // The sphere sits inside the tube, or touches it along one circle at
        // its own equator; a tangency carries no transversal curve.
        return Ok(SurfaceIntersection::Empty);
    }
    let reach = square.sqrt();
    let (u, v) = (unit(cylinder.radial_u)?, unit(cylinder.radial_v)?);
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center: sphere.origin + axis * reach,
            u,
            v,
            radius: cylinder.radius,
        },
        IntersectionCurve::Circle {
            center: sphere.origin + axis * -reach,
            u,
            v,
            radius: cylinder.radius,
        },
    ]))
}

fn cylinder_cone(cylinder: Cylinder, cone: Cone, tolerances: Tolerances) -> Intersection {
    let axis = unit(cylinder.axis)?;
    let cone_axis = unit(cone.axis)?;
    if !tolerances.parallel(axis, cone_axis) {
        return Err(IntersectionError::Unsupported);
    }
    let offset = cone.origin - cylinder.origin;
    if (offset - axis * offset.dot(axis)).length() > tolerances.linear {
        // Parallel but not coaxial: a space quartic.
        return Err(IntersectionError::Unsupported);
    }
    if cone.slope.abs() <= tolerances.angular {
        // A degenerate cone is a cylinder in disguise.
        return Ok(
            if (cone.base_radius - cylinder.radius).abs() <= tolerances.linear {
                SurfaceIntersection::Coincident
            } else {
                SurfaceIntersection::Empty
            },
        );
    }
    // Coaxial: the cone reaches the cylinder's radius at exactly one height.
    let height = (cylinder.radius - cone.base_radius) / cone.slope;
    if !height.is_finite() {
        return Err(IntersectionError::Indeterminate);
    }
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center: cone.origin + cone_axis * height,
            u: unit(cone.radial_u)?,
            v: unit(cone.radial_v)?,
            radius: cylinder.radius,
        },
    ]))
}

fn sphere_sphere(first: Sphere, second: Sphere, tolerances: Tolerances) -> Intersection {
    if first.radius <= 0.0 || second.radius <= 0.0 {
        return Err(IntersectionError::Indeterminate);
    }
    let offset = second.origin - first.origin;
    let separation = offset.length();
    if separation <= tolerances.linear {
        return Ok(
            if (first.radius - second.radius).abs() <= tolerances.linear {
                SurfaceIntersection::Coincident
            } else {
                SurfaceIntersection::Empty
            },
        );
    }
    let far = first.radius + second.radius;
    let near = (first.radius - second.radius).abs();
    if separation > far + tolerances.linear || separation < near - tolerances.linear {
        return Ok(SurfaceIntersection::Empty);
    }
    let toward = offset / separation;
    let reach = first
        .radius
        .mul_add(first.radius, -(second.radius * second.radius))
        .mul_add(1.0 / (2.0 * separation), separation / 2.0);
    let square = first.radius.mul_add(first.radius, -(reach * reach));
    if square <= tolerances.linear * first.radius {
        return Ok(SurfaceIntersection::Empty);
    }
    let (u, v) = orthonormal_to(toward)?;
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center: first.origin + toward * reach,
            u,
            v,
            radius: square.sqrt(),
        },
    ]))
}

fn cone_cone(first: Cone, second: Cone, tolerances: Tolerances) -> Intersection {
    let axis = unit(first.axis)?;
    let other = unit(second.axis)?;
    if !tolerances.parallel(axis, other) {
        return Err(IntersectionError::Unsupported);
    }
    let offset = second.origin - first.origin;
    if (offset - axis * offset.dot(axis)).length() > tolerances.linear {
        return Err(IntersectionError::Unsupported);
    }
    // Coaxial: both ring radii are affine in the shared height, so they agree
    // nowhere, at one height, or everywhere.
    let shift = offset.dot(axis) * if axis.dot(other) > 0.0 { 1.0 } else { -1.0 };
    let second_slope = second.slope * if axis.dot(other) > 0.0 { 1.0 } else { -1.0 };
    let slope_gap = first.slope - second_slope;
    let base_gap = second.base_radius - second_slope * shift - first.base_radius;
    if slope_gap.abs() <= tolerances.angular {
        return Ok(if base_gap.abs() <= tolerances.linear {
            SurfaceIntersection::Coincident
        } else {
            SurfaceIntersection::Empty
        });
    }
    let height = base_gap / slope_gap;
    let radius = first.ring_radius(height);
    if !radius.is_finite() {
        return Err(IntersectionError::Indeterminate);
    }
    if radius <= tolerances.linear {
        return Ok(SurfaceIntersection::Empty);
    }
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center: first.origin + axis * height,
            u: unit(first.radial_u)?,
            v: unit(first.radial_v)?,
            radius,
        },
    ]))
}

fn torus_torus(first: Torus, second: Torus, tolerances: Tolerances) -> Intersection {
    let axis = unit(first.axis)?;
    let other = unit(second.axis)?;
    if !tolerances.parallel(axis, other) {
        return Err(IntersectionError::Unsupported);
    }
    let offset = second.origin - first.origin;
    if (offset - axis * offset.dot(axis)).length() > tolerances.linear {
        return Err(IntersectionError::Unsupported);
    }
    // Coaxial tubes meet where their (r, z) section circles do, which is the
    // same two-dimensional problem as two coaxial spheres in the half-plane.
    let shift = offset.dot(axis);
    let separation = shift.abs();
    let (first_minor, second_minor) = (first.minor_radius, second.minor_radius);
    if (first.major_radius - second.major_radius).abs() > tolerances.linear {
        // Different tube centres in the radial direction as well as the axial
        // one; the section circles meet in points, not in a full latitude.
        return Err(IntersectionError::Unsupported);
    }
    if separation <= tolerances.linear {
        return Ok(if (first_minor - second_minor).abs() <= tolerances.linear {
            SurfaceIntersection::Coincident
        } else {
            SurfaceIntersection::Empty
        });
    }
    let far = first_minor + second_minor;
    let near = (first_minor - second_minor).abs();
    if separation > far + tolerances.linear || separation < near - tolerances.linear {
        return Ok(SurfaceIntersection::Empty);
    }
    let toward = if shift > 0.0 { 1.0 } else { -1.0 };
    let reach = first_minor
        .mul_add(first_minor, -(second_minor * second_minor))
        .mul_add(1.0 / (2.0 * separation), separation / 2.0);
    let square = first_minor.mul_add(first_minor, -(reach * reach));
    if square <= tolerances.linear * first_minor {
        return Ok(SurfaceIntersection::Empty);
    }
    // Both section intersections revolve into full latitude circles.
    let radial = square.sqrt();
    let center = first.origin + axis * (toward * reach);
    let (u, v) = (unit(first.radial_u)?, unit(first.radial_v)?);
    Ok(SurfaceIntersection::Curves(vec![
        IntersectionCurve::Circle {
            center,
            u,
            v,
            radius: first.major_radius + radial,
        },
        IntersectionCurve::Circle {
            center,
            u,
            v,
            radius: first.major_radius - radial,
        },
    ]))
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

fn unit(vector: Vector3) -> Result<Vector3, IntersectionError> {
    let length = vector.length();
    if !length.is_finite() || length <= f64::EPSILON {
        return Err(IntersectionError::Indeterminate);
    }
    Ok(vector / length)
}

/// Any right-handed pair spanning the plane perpendicular to `normal`.
///
/// The choice is arbitrary but must be stable and well conditioned, so the
/// seed axis is the one the normal leans on least.
fn orthonormal_to(normal: Vector3) -> Result<(Vector3, Vector3), IntersectionError> {
    let seed = if normal.x.abs() <= normal.y.abs() && normal.x.abs() <= normal.z.abs() {
        Vector3::new(1.0, 0.0, 0.0)
    } else if normal.y.abs() <= normal.z.abs() {
        Vector3::new(0.0, 1.0, 0.0)
    } else {
        Vector3::new(0.0, 0.0, 1.0)
    };
    let u = unit(seed - normal * seed.dot(normal))?;
    Ok((u, normal.cross(u)))
}

/// Whether every carrier pair between two shells can be intersected exactly.
///
/// Booleans need the imprint curves before anything else, so this is the
/// operation's domain oracle: it names the first pair that leaves the matrix
/// rather than letting reconstruction discover the problem halfway through a
/// rewrite. A pair that simply does not meet is inside the domain — an empty
/// intersection is an exact answer.
pub(crate) fn first_unsupported_pair(
    left: &[Surface],
    right: &[Surface],
    precision: PrecisionPolicy,
) -> Option<(Surface, Surface)> {
    left.iter().find_map(|first| {
        right
            .iter()
            .find(|second| intersect(*first, **second, precision).is_err())
            .map(|second| (*first, *second))
    })
}

/// The surface class's own name, for diagnostics that have to say which pair
/// left the domain.
pub(crate) const fn surface_name(surface: Surface) -> &'static str {
    match surface {
        Surface::Plane(_) => "plane",
        Surface::Cylinder(_) => "cylinder",
        Surface::Cone(_) => "cone",
        Surface::Sphere(_) => "sphere",
        Surface::Torus(_) => "torus",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn precision() -> PrecisionPolicy {
        PrecisionPolicy::default()
    }

    fn plane(origin: [f64; 3], u: [f64; 3], v: [f64; 3]) -> Surface {
        Surface::Plane(Plane::new(
            Point3::new(origin[0], origin[1], origin[2]),
            Vector3::new(u[0], u[1], u[2]),
            Vector3::new(v[0], v[1], v[2]),
        ))
    }

    fn upright_cylinder(origin: [f64; 3], radius: f64) -> Surface {
        Surface::Cylinder(Cylinder {
            origin: Point3::new(origin[0], origin[1], origin[2]),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            radius,
            angular_sign: 1.0,
        })
    }

    fn ball(origin: [f64; 3], radius: f64) -> Surface {
        Surface::Sphere(Sphere {
            origin: Point3::new(origin[0], origin[1], origin[2]),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            radius,
            angular_sign: 1.0,
        })
    }

    fn curves(result: Intersection) -> Vec<IntersectionCurve> {
        match result.expect("the pair is inside the published domain") {
            SurfaceIntersection::Curves(curves) => curves,
            other => panic!("expected curves, received {other:?}"),
        }
    }

    /// Every returned curve must actually lie on both carriers, sampled at
    /// enough parameters to catch a wrong centre, radius, or direction.
    fn assert_on(surface: Surface, curve: IntersectionCurve) {
        for step in 0..8 {
            let parameter = f64::from(step) * 0.7 - 2.0;
            let point = match curve {
                IntersectionCurve::Line { origin, direction } => origin + direction * parameter,
                IntersectionCurve::Circle {
                    center,
                    u,
                    v,
                    radius,
                } => center + (u * parameter.cos() + v * parameter.sin()) * radius,
            };
            let residual = match surface {
                Surface::Plane(plane) => (point - plane.origin).dot(plane.normal).abs(),
                Surface::Cylinder(cylinder) => {
                    let offset = point - cylinder.origin;
                    let across = offset - cylinder.axis * offset.dot(cylinder.axis);
                    (across.length() - cylinder.radius).abs()
                }
                Surface::Cone(cone) => {
                    let offset = point - cone.origin;
                    let height = offset.dot(cone.axis);
                    let across = offset - cone.axis * height;
                    (across.length() - cone.ring_radius(height)).abs()
                }
                Surface::Sphere(sphere) => ((point - sphere.origin).length() - sphere.radius).abs(),
                Surface::Torus(torus) => {
                    let offset = point - torus.origin;
                    let height = offset.dot(torus.axis);
                    let across = offset - torus.axis * height;
                    let ring = across.length() - torus.major_radius;
                    (ring.hypot(height) - torus.minor_radius).abs()
                }
            };
            assert!(
                residual < 1.0e-12,
                "sample at {parameter} is {residual} off the carrier"
            );
        }
    }

    #[test]
    fn two_crossing_planes_meet_in_a_line_on_both() {
        let first = plane([1.0, 2.0, 3.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let second = plane([-4.0, 5.0, 6.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        let found = curves(intersect(first, second, precision()));
        assert_eq!(found.len(), 1);
        assert_on(first, found[0]);
        assert_on(second, found[0]);
    }

    #[test]
    fn parallel_planes_are_empty_or_coincident_by_offset() {
        let base = plane([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let apart = plane([0.0, 0.0, 5.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        // The same plane, seen from the other side: u and v swapped.
        let flipped = plane([3.0, -2.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]);
        assert_eq!(
            intersect(base, apart, precision()),
            Ok(SurfaceIntersection::Empty)
        );
        assert_eq!(
            intersect(base, flipped, precision()),
            Ok(SurfaceIntersection::Coincident)
        );
    }

    #[test]
    fn a_plane_cuts_a_cylinder_in_a_ring_a_pair_of_generators_or_nothing() {
        let cylinder = upright_cylinder([0.0, 0.0, 0.0], 5.0);

        // Perpendicular: the cylinder's own ring, at the plane's height.
        let across = plane([0.0, 0.0, 4.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let ring = curves(intersect(across, cylinder, precision()));
        assert_eq!(ring.len(), 1);
        assert_on(cylinder, ring[0]);
        assert_on(across, ring[0]);

        // Along the axis, offset 3 from it: two generators at x = ±4.
        let along = plane([3.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        let generators = curves(intersect(along, cylinder, precision()));
        assert_eq!(generators.len(), 2);
        for curve in generators {
            assert_on(cylinder, curve);
            assert_on(along, curve);
        }

        // Clear of the cylinder entirely.
        let clear = plane([9.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        assert_eq!(
            intersect(clear, cylinder, precision()),
            Ok(SurfaceIntersection::Empty)
        );

        // Oblique: a genuine ellipse, and the vocabulary says so.
        let oblique = plane([0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 1.0, 0.0]);
        assert_eq!(
            intersect(oblique, cylinder, precision()),
            Err(IntersectionError::Unsupported)
        );
    }

    #[test]
    fn a_plane_cuts_a_sphere_in_a_circle_and_a_tangent_plane_in_nothing() {
        let sphere = ball([1.0, 1.0, 1.0], 5.0);
        let cut = plane([0.0, 0.0, 4.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let found = curves(intersect(cut, sphere, precision()));
        assert_eq!(found.len(), 1);
        assert_on(sphere, found[0]);
        assert_on(cut, found[0]);
        let IntersectionCurve::Circle { radius, .. } = found[0] else {
            panic!("a plane cuts a sphere in a circle");
        };
        // The cut sits 3 above the centre, so the chord radius is 4.
        assert!((radius - 4.0).abs() < 1.0e-12, "radius {radius}");

        let tangent = plane([0.0, 0.0, 6.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert_eq!(
            intersect(tangent, sphere, precision()),
            Ok(SurfaceIntersection::Empty)
        );
    }

    #[test]
    fn two_spheres_meet_in_the_circle_their_radical_plane_carries() {
        let first = ball([0.0, 0.0, 0.0], 5.0);
        let second = ball([6.0, 0.0, 0.0], 5.0);
        let found = curves(intersect(first, second, precision()));
        assert_eq!(found.len(), 1);
        assert_on(first, found[0]);
        assert_on(second, found[0]);

        let apart = ball([20.0, 0.0, 0.0], 5.0);
        assert_eq!(
            intersect(first, apart, precision()),
            Ok(SurfaceIntersection::Empty)
        );
        let swallowed = ball([0.0, 0.0, 0.0], 2.0);
        assert_eq!(
            intersect(first, swallowed, precision()),
            Ok(SurfaceIntersection::Empty)
        );
        assert_eq!(
            intersect(first, ball([0.0, 0.0, 0.0], 5.0), precision()),
            Ok(SurfaceIntersection::Coincident)
        );
    }

    #[test]
    fn parallel_cylinders_meet_in_generators_and_coaxial_ones_coincide() {
        let first = upright_cylinder([0.0, 0.0, 0.0], 5.0);
        let overlapping = upright_cylinder([6.0, 0.0, 0.0], 5.0);
        let found = curves(intersect(first, overlapping, precision()));
        assert_eq!(found.len(), 2);
        for curve in found {
            assert_on(first, curve);
            assert_on(overlapping, curve);
        }
        assert_eq!(
            intersect(first, upright_cylinder([0.0, 0.0, 9.0], 5.0), precision()),
            Ok(SurfaceIntersection::Coincident)
        );
        assert_eq!(
            intersect(first, upright_cylinder([20.0, 0.0, 0.0], 5.0), precision()),
            Ok(SurfaceIntersection::Empty)
        );
        // Crossing axes give a space quartic.
        let crossing = Surface::Cylinder(Cylinder {
            origin: Point3::new(0.0, 0.0, 0.0),
            axis: Vector3::new(1.0, 0.0, 0.0),
            radial_u: Vector3::new(0.0, 1.0, 0.0),
            radial_v: Vector3::new(0.0, 0.0, 1.0),
            radius: 3.0,
            angular_sign: 1.0,
        });
        assert_eq!(
            intersect(first, crossing, precision()),
            Err(IntersectionError::Unsupported)
        );
    }

    #[test]
    fn a_sphere_on_a_cylinder_axis_meets_it_in_two_latitude_circles() {
        let cylinder = upright_cylinder([0.0, 0.0, 0.0], 3.0);
        let sphere = ball([0.0, 0.0, 2.0], 5.0);
        let found = curves(intersect(cylinder, sphere, precision()));
        assert_eq!(found.len(), 2);
        for curve in found {
            assert_on(cylinder, curve);
            assert_on(sphere, curve);
        }
        // Off the axis the pair leaves the domain even though it does meet.
        assert_eq!(
            intersect(cylinder, ball([1.0, 0.0, 0.0], 5.0), precision()),
            Err(IntersectionError::Unsupported)
        );
    }

    #[test]
    fn a_perpendicular_plane_cuts_a_cone_at_its_own_ring_radius() {
        let cone = Surface::Cone(Cone {
            origin: Point3::new(0.0, 0.0, 0.0),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            base_radius: 4.0,
            slope: -0.5,
            angular_sign: 1.0,
        });
        let cut = plane([0.0, 0.0, 2.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let found = curves(intersect(cut, cone, precision()));
        assert_eq!(found.len(), 1);
        assert_on(cone, found[0]);
        assert_on(cut, found[0]);
        let IntersectionCurve::Circle { radius, .. } = found[0] else {
            panic!("a perpendicular cut is a circle");
        };
        assert!((radius - 3.0).abs() < 1.0e-12, "radius {radius}");

        // Beyond the apex the ring radius runs out.
        let past = plane([0.0, 0.0, 9.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert_eq!(
            intersect(past, cone, precision()),
            Ok(SurfaceIntersection::Empty)
        );

        // A coaxial cylinder meets the cone at the one height that fits.
        let cylinder = upright_cylinder([0.0, 0.0, 0.0], 3.0);
        let meeting = curves(intersect(cylinder, cone, precision()));
        assert_eq!(meeting.len(), 1);
        assert_on(cone, meeting[0]);
        assert_on(cylinder, meeting[0]);
    }

    #[test]
    fn a_plane_cuts_a_torus_across_or_through_its_axis() {
        let torus = Surface::Torus(Torus {
            origin: Point3::new(0.0, 0.0, 0.0),
            axis: Vector3::new(0.0, 0.0, 1.0),
            radial_u: Vector3::new(1.0, 0.0, 0.0),
            radial_v: Vector3::new(0.0, 1.0, 0.0),
            major_radius: 6.0,
            minor_radius: 2.0,
            angular_sign: 1.0,
        });

        // Perpendicular, above the tube centre: an inner and an outer ring.
        let across = plane([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let rings = curves(intersect(across, torus, precision()));
        assert_eq!(rings.len(), 2);
        for curve in rings {
            assert_on(torus, curve);
            assert_on(across, curve);
        }

        // Through the axis: the two generating circles.
        let through = plane([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let generators = curves(intersect(through, torus, precision()));
        assert_eq!(generators.len(), 2);
        for curve in generators {
            assert_on(torus, curve);
            assert_on(through, curve);
        }

        // Clear above the tube.
        let clear = plane([0.0, 0.0, 5.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert_eq!(
            intersect(clear, torus, precision()),
            Ok(SurfaceIntersection::Empty)
        );

        // Parallel to the axis but off it: a quartic.
        let offset = plane([3.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        assert_eq!(
            intersect(offset, torus, precision()),
            Err(IntersectionError::Unsupported)
        );
    }
}

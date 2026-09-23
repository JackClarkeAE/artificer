//! Exact mirror: an orientation-reversing isometry applied to committed
//! topology.
//!
//! A reflection maps every carrier to a carrier of the same kind, so the
//! geometry is reflected as itself: points, curve frames and surface frames
//! through the same linear map. What a reflection also does is flip
//! handedness: a reflected frame is left-handed, and every surface's
//! parametric normal ends up pointing into the material. A revolved
//! carrier therefore keeps its frame right-handed by negating the reflected
//! `radial_v`, which traces the same reflected surface with the same
//! parameters, and each face is then reversed by the kernel's own
//! convention, the one the Boolean engine uses: a plane swaps its axes, a
//! revolved carrier negates its angular sign, a ruled carrier walks its rails
//! the other way, a B-spline surface walks `u` the other way over its negated
//! domain, the pcurves go through the matching in-plane mirror, and every
//! loop walks the other way. Edges and vertices keep their identities, so
//! history maps one to one.

use crate::bspline::{array2, array3, point2, point3};
use crate::ruled::RailCurve;
use crate::topology::{
    Curve2, Curve3, Cylinder, ParameterRange, Plane, Point2, Point3, Surface, Topology, Vector2,
    Vector3,
};

/// Why a mirror was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorError {
    /// The plane normal has no direction.
    DegenerateNormal,
    /// A planar face carries a curve-on-surface the in-plane mirror cannot
    /// express, which no builder produces today.
    UnsupportedPcurve,
}

impl MirrorError {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::DegenerateNormal => "Mirror requires a non-zero plane normal.",
            Self::UnsupportedPcurve => {
                "A planar face carries a curve-on-surface the mirror cannot express."
            }
        }
    }
}

/// Reflects `input` across the plane through `origin` with normal `normal`
/// and returns a topology whose faces face outward again.
pub(crate) fn mirror_topology(
    input: &Topology,
    origin: Point3,
    normal: Vector3,
) -> Result<Topology, MirrorError> {
    let length = normal.length();
    if !length.is_finite() || length <= f64::EPSILON {
        return Err(MirrorError::DegenerateNormal);
    }
    let n = normal / length;
    let reflect_vector = |v: Vector3| v - n * (2.0 * v.dot(n));
    let reflect_point = |p: Point3| origin + reflect_vector(p - origin);

    let mut output = input.clone();
    for vertex in &mut output.vertices {
        vertex.value.point = reflect_point(vertex.value.point);
    }
    for edge in &mut output.edges {
        match &mut edge.value.curve {
            Curve3::Line { endpoints } => *endpoints = endpoints.map(reflect_point),
            Curve3::Trace { host, other, .. } => {
                // Every frame vector reflects and the angular sign stays, so
                // the same parameter names the reflected point and the edge's
                // own range needs no change.
                for cylinder in [host, other] {
                    cylinder.origin = reflect_point(cylinder.origin);
                    cylinder.axis = reflect_vector(cylinder.axis);
                    cylinder.radial_u = reflect_vector(cylinder.radial_u);
                    cylinder.radial_v = reflect_vector(cylinder.radial_v);
                }
            }
            Curve3::Circle { center, u, v, .. } | Curve3::Ellipse { center, u, v, .. } => {
                *center = reflect_point(*center);
                *u = reflect_vector(*u);
                *v = reflect_vector(*v);
            }
            // A reflection is affine, and an affine image of a B-spline is
            // the B-spline of the images of its control points, over the
            // same parameter.
            Curve3::Bspline { curve } => {
                *curve = curve
                    .mapped(|point| array3(reflect_point(point3(point))))
                    .ok_or(MirrorError::UnsupportedPcurve)?;
            }
        }
    }

    for face_index in 0..output.faces.len() {
        // Reflect the carrier and reverse its parameterisation in one step;
        // the in-plane mirror the pcurves need is the one the reversal
        // implies.
        let mirror: fn(Point2) -> Point2 = {
            let face = &mut output.faces[face_index].value;
            match &mut face.surface {
                Surface::Plane(plane) => {
                    *plane = Plane::new(
                        reflect_point(plane.origin),
                        reflect_vector(plane.v),
                        reflect_vector(plane.u),
                    );
                    |point: Point2| Point2::new(point.y, point.x)
                }
                // The reflected frame `(Ru, Rv, Ra)` is left-handed; the
                // right-handed `(Ru, −Rv, Ra)` with the angular sign
                // negated traces the same reflected surface at the same
                // parameters, and negating the sign again is the reversal
                // itself. The two cancel, so the sign stays.
                Surface::Cylinder(cylinder) => {
                    cylinder.origin = reflect_point(cylinder.origin);
                    cylinder.axis = reflect_vector(cylinder.axis);
                    cylinder.radial_u = reflect_vector(cylinder.radial_u);
                    cylinder.radial_v = reflect_vector(cylinder.radial_v) * -1.0;
                    |point: Point2| Point2::new(-point.x, point.y)
                }
                Surface::Cone(cone) => {
                    cone.origin = reflect_point(cone.origin);
                    cone.axis = reflect_vector(cone.axis);
                    cone.radial_u = reflect_vector(cone.radial_u);
                    cone.radial_v = reflect_vector(cone.radial_v) * -1.0;
                    |point: Point2| Point2::new(-point.x, point.y)
                }
                Surface::Torus(torus) => {
                    torus.origin = reflect_point(torus.origin);
                    torus.axis = reflect_vector(torus.axis);
                    torus.radial_u = reflect_vector(torus.radial_u);
                    torus.radial_v = reflect_vector(torus.radial_v) * -1.0;
                    |point: Point2| Point2::new(-point.x, point.y)
                }
                Surface::Sphere(sphere) => {
                    sphere.origin = reflect_point(sphere.origin);
                    sphere.axis = reflect_vector(sphere.axis);
                    sphere.radial_u = reflect_vector(sphere.radial_u);
                    sphere.radial_v = reflect_vector(sphere.radial_v) * -1.0;
                    |point: Point2| Point2::new(-point.x, point.y)
                }
                // Each rail reflects as the edges do, so the same `u` names
                // the reflected point; the normal `∂u × ∂v` then points into
                // the material, and walking `u` the other way along both
                // rails is the reversal that turns it out again.
                Surface::Ruled(ruled) => {
                    for rail in &mut ruled.rails {
                        rail.curve = match rail.curve {
                            RailCurve::Line { endpoints } => RailCurve::Line {
                                endpoints: endpoints.map(reflect_point),
                            },
                            RailCurve::Circle {
                                center,
                                u,
                                v,
                                radius,
                            } => RailCurve::Circle {
                                center: reflect_point(center),
                                u: reflect_vector(u),
                                v: reflect_vector(v),
                                radius,
                            },
                            RailCurve::Ellipse {
                                center,
                                u,
                                v,
                                major_radius,
                                minor_radius,
                            } => RailCurve::Ellipse {
                                center: reflect_point(center),
                                u: reflect_vector(u),
                                v: reflect_vector(v),
                                major_radius,
                                minor_radius,
                            },
                        };
                    }
                    *ruled = ruled.reversed_u();
                    |point: Point2| Point2::new(1.0 - point.x, point.y)
                }
                // The net reflects as the edges do, so the same parameters
                // name the reflected point; walking `u` the other way over
                // its negated domain turns the normal out again.
                Surface::Bspline(surface) => {
                    *surface = surface
                        .mapped(reflect_point)
                        .ok_or(MirrorError::UnsupportedPcurve)?
                        .reversed_u();
                    |point: Point2| Point2::new(-point.x, point.y)
                }
            }
        };
        let reflect_cylinder = |mut cylinder: Cylinder| {
            cylinder.origin = reflect_point(cylinder.origin);
            cylinder.axis = reflect_vector(cylinder.axis);
            cylinder.radial_u = reflect_vector(cylinder.radial_u);
            cylinder.radial_v = reflect_vector(cylinder.radial_v);
            cylinder
        };
        reverse_face_loops(&mut output, face_index, mirror, &reflect_cylinder)?;
    }
    Ok(output)
}

/// Reverses every loop of one face and carries its curves-on-surface
/// through `mirror`, the in-plane map the carrier's own reversal implies.
///
/// A face is reversed by flipping its carrier's parameterisation and then
/// walking its loops the other way, so the two halves belong together: the
/// mirror of a body and the void of a shell both need exactly this, and
/// both call it here.
///
/// A trace is written on two cylinders, so reversing it needs to know what
/// became of them: `carry` maps a cylinder as it was to the cylinder it is
/// now — reflected, for a mirror; unchanged, for a shell's void — exactly as
/// the caller has already carried every edge's curve, so a trace's own
/// parameter keeps naming the same point on both sides of its edge.
pub(crate) fn reverse_face_loops(
    topology: &mut Topology,
    face_index: usize,
    mirror: fn(Point2) -> Point2,
    carry: &dyn Fn(Cylinder) -> Cylinder,
) -> Result<(), MirrorError> {
    let planar = matches!(topology.faces[face_index].value.surface, Surface::Plane(_));
    let face_cylinder = match topology.faces[face_index].value.surface {
        Surface::Cylinder(cylinder) => Some(cylinder),
        _ => None,
    };
    let loops: Vec<_> = topology.faces[face_index].value.loops().collect();
    for loop_key in loops {
        let loop_record = &mut topology.loops[loop_key.0];
        loop_record.value.coedges.reverse();
        for coedge_key in loop_record.value.coedges.clone() {
            let coedge = &mut topology.coedges[coedge_key.0].value;
            coedge.orientation = coedge.orientation.reversed();
            let range = coedge.parameter_range;
            let map_vector = |vector: Vector2| {
                let mapped = mirror(Point2::new(vector.x, vector.y));
                Vector2::new(mapped.x, mapped.y)
            };
            match coedge.pcurve {
                Curve2::Line { .. } => {
                    let start = mirror(coedge.pcurve.evaluate(range.start));
                    let end = mirror(coedge.pcurve.evaluate(range.end));
                    let (pcurve, parameter_range) = Curve2::line_segment([end, start]);
                    coedge.pcurve = pcurve;
                    coedge.parameter_range = parameter_range;
                }
                Curve2::Circle {
                    center,
                    u,
                    v,
                    radius,
                } => {
                    coedge.pcurve = Curve2::Circle {
                        center: mirror(center),
                        u: map_vector(u),
                        v: map_vector(v),
                        radius,
                    };
                    coedge.parameter_range = ParameterRange::new(range.end, range.start);
                }
                Curve2::Ellipse {
                    center,
                    u,
                    v,
                    major_radius,
                    minor_radius,
                } => {
                    coedge.pcurve = Curve2::Ellipse {
                        center: mirror(center),
                        u: map_vector(u),
                        v: map_vector(v),
                        major_radius,
                        minor_radius,
                    };
                    coedge.parameter_range = ParameterRange::new(range.end, range.start);
                }
                // The face's azimuth now runs the other way, and the trace
                // goes with it. On the other cylinder's face the curve is
                // still walked over the host's azimuth, carried as the edge
                // was, and read into the face's new coordinates; only the
                // window it lies near turns round with the face. On the
                // host's own face the curve is a graph over the face's own
                // azimuth, which is now its old one negated: the same root
                // over the reversed record, walked from `−end` to `−start`,
                // which is still an affine match for the edge's parameter.
                Curve2::Trace {
                    host,
                    other,
                    branch,
                    on_other,
                    shift,
                } => {
                    let Some(face_cylinder) = face_cylinder else {
                        return Err(MirrorError::UnsupportedPcurve);
                    };
                    if on_other {
                        coedge.pcurve = Curve2::Trace {
                            host: carry(host),
                            other: face_cylinder,
                            branch,
                            on_other: true,
                            shift: Point2::new(-shift.x, shift.y),
                        };
                        coedge.parameter_range = ParameterRange::new(range.end, range.start);
                    } else {
                        // The reversed record has to be the carried host read
                        // backwards, or the graph over it is a different
                        // parameterization, not a reversed one; refusing by
                        // name beats publishing a mirror of something else.
                        let carried = carry(host);
                        let reversed = |x: f64| {
                            (face_cylinder.evaluate(Point2::new(x, 0.0))
                                - carried.evaluate(Point2::new(-x, 0.0)))
                            .length()
                        };
                        let scale = 1.0 + face_cylinder.radius.abs();
                        if reversed(1.0).max(reversed(-2.0)) > 1.0e-9 * scale
                            || (face_cylinder.axis - carried.axis).length() > 1.0e-12 * scale
                        {
                            return Err(MirrorError::UnsupportedPcurve);
                        }
                        coedge.pcurve = Curve2::Trace {
                            host: face_cylinder,
                            other: carry(other),
                            branch,
                            on_other: false,
                            shift: Point2::new(-shift.x, shift.y),
                        };
                        coedge.parameter_range = ParameterRange::new(-range.end, -range.start);
                    }
                }
                // The in-plane mirror is linear, and carries a B-spline by its
                // control points; the reversed walk runs the range backwards.
                Curve2::Bspline { curve } => {
                    coedge.pcurve = Curve2::Bspline {
                        curve: curve
                            .mapped(|point| array2(mirror(point2(point))))
                            .ok_or(MirrorError::UnsupportedPcurve)?,
                    };
                    coedge.parameter_range = ParameterRange::new(range.end, range.start);
                }
                Curve2::Harmonic {
                    mean,
                    amplitude,
                    phase,
                } => {
                    if planar {
                        return Err(MirrorError::UnsupportedPcurve);
                    }
                    // The azimuth mirror negates the parameter:
                    // `cos(-t - p) = cos(t + p)`, and the reversed walk runs
                    // from `-end` to `-start`.
                    coedge.pcurve = Curve2::Harmonic {
                        mean,
                        amplitude,
                        phase: -phase,
                    };
                    coedge.parameter_range = ParameterRange::new(-range.end, -range.start);
                }
            }
        }
    }
    Ok(())
}

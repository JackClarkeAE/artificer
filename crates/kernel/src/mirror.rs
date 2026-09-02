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
//! revolved carrier negates its angular sign, the pcurves go through the
//! matching in-plane mirror, and every loop walks the other way. Edges and
//! vertices keep their identities, so history maps one to one.

use crate::topology::{
    Curve2, Curve3, ParameterRange, Plane, Point2, Point3, Surface, Topology, Vector2, Vector3,
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
            Curve3::Circle { center, u, v, .. } | Curve3::Ellipse { center, u, v, .. } => {
                *center = reflect_point(*center);
                *u = reflect_vector(*u);
                *v = reflect_vector(*v);
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
            }
        };
        let planar = matches!(output.faces[face_index].value.surface, Surface::Plane(_));
        let loops: Vec<_> = output.faces[face_index].value.loops().collect();
        for loop_key in loops {
            let loop_record = &mut output.loops[loop_key.0];
            loop_record.value.coedges.reverse();
            for coedge_key in loop_record.value.coedges.clone() {
                let coedge = &mut output.coedges[coedge_key.0].value;
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
                    Curve2::Harmonic {
                        mean,
                        amplitude,
                        phase,
                    } => {
                        if planar {
                            return Err(MirrorError::UnsupportedPcurve);
                        }
                        // The azimuth mirror negates the parameter:
                        // `cos(−θ − φ) = cos(θ + φ)`, and the reversed walk
                        // runs from `−end` to `−start`.
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
    }
    Ok(output)
}
